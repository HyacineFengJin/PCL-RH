//! Original Linux wire, snapshots, openat2 NO_XDEV and anonymous publication.
use super::{
    check, err, name_ok, operation_id, operation_ok, owned, slot, validate, write_journal, Journal,
    Result, MAX_DEPTH, MAX_FILES, MAX_JOURNAL, MAX_JSON,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
fn os_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}
fn c_name(name: &str) -> Result<CString> {
    name_ok(name)?;
    CString::new(name).map_err(err)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct DirKey {
    pub(super) dev: u64,
    pub(super) ino: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    pub(super) size: u64,
    pub(super) hash: String,
    pub(super) dev: u64,
    pub(super) ino: u64,
    pub(super) mtime: i64,
    pub(super) mtime_nsec: i64,
    pub(super) ctime: i64,
    pub(super) ctime_nsec: i64,
}
impl Snapshot {
    pub(super) fn stable_matches(&self, other: &Self) -> bool {
        self.size == other.size
            && self.hash == other.hash
            && self.dev == other.dev
            && self.ino == other.ino
    }
}

pub(super) struct Dir {
    pub(super) file: File,
}
impl Dir {
    pub(super) fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err("游戏目录必须是绝对路径".into());
        }
        let slash = CString::new("/").map_err(err)?;
        let fd = unsafe {
            libc::open(
                slash.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(os_error("无法打开实例重命名目录"));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        for component in path.components() {
            match component {
                std::path::Component::RootDir => continue,
                std::path::Component::Normal(name) => {
                    let name = CString::new(name.as_bytes()).map_err(err)?;
                    let fd = unsafe {
                        libc::openat(
                            file.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                        )
                    };
                    if fd < 0 {
                        return Err(os_error("游戏目录祖先已变化或包含符号链接"));
                    }
                    file = unsafe { File::from_raw_fd(fd) };
                }
                _ => return Err("游戏目录路径包含不安全的分量".into()),
            }
        }
        Ok(Self { file })
    }
    pub(super) fn child(&self, name: &str) -> Result<Self> {
        let name = c_name(name)?;
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let how = OpenHow {
            flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
            mode: 0,
            resolve: 0x01 | 0x02 | 0x04 | 0x08,
        };
        // NO_XDEV also rejects bind mounts, so temporary-tree cleanup cannot
        // descend into a mounted user directory even on the same filesystem.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.file.as_raw_fd(),
                name.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            ) as i32
        };
        if fd < 0 {
            return Err(os_error(
                "目录不存在、已变化、包含符号链接或跨挂载点（需要 Linux 5.6 以上）",
            ));
        }
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
        })
    }
    pub(super) fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(super) fn create_dir(&self, name: &str) -> Result<Self> {
        let name_c = c_name(name)?;
        if unsafe { libc::mkdirat(self.file.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
            return Err(os_error("无法创建实例重命名目录"));
        }
        self.sync()?;
        self.child(name)
    }
    pub(super) fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(dir) => Ok(dir),
            None => self.create_dir(name),
        }
    }
    pub(super) fn stat(&self, name: &str) -> Result<Option<libc::stat>> {
        let name = c_name(name)?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(unsafe { st.assume_init() }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(err(error))
        }
    }
    pub(super) fn regular(&self, name: &str) -> Result<File> {
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(os_error("无法读取核心文件"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(err)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err("核心文件必须是独立普通文件，不能是符号链接或硬链接".into());
        }
        Ok(file)
    }
    pub(super) fn create_file(&self, name: &str) -> Result<File> {
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(os_error("无法创建暂存文件"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(super) fn anonymous_file(&self) -> Result<File> {
        // O_TMPFILE takes a directory name. Use the pinned directory itself so
        // neither a partial nor an unregistered completed copy has a pathname.
        let path = CString::new(".").map_err(err)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                path.as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(os_error(
                "此文件系统不支持安全的匿名核心暂存文件，原实例未改动",
            ));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(super) fn names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(self.fd_path()).map_err(err)? {
            let name = entry
                .map_err(err)?
                .file_name()
                .into_string()
                .map_err(|_| "目录包含无效文件名")?;
            name_ok(&name)?;
            names.push(name);
            if names.len() > MAX_FILES {
                return Err("目录文件数量超过重命名安全上限".into());
            }
        }
        names.sort();
        Ok(names)
    }
    pub(super) fn fd_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
    }
    pub(super) fn key(&self) -> Result<DirKey> {
        let st = self.file.metadata().map_err(err)?;
        Ok(DirKey {
            dev: st.dev(),
            ino: st.ino(),
        })
    }
    pub(super) fn sync(&self) -> Result<()> {
        self.file.sync_all().map_err(err)
    }
    pub(super) fn unlink(&self, name: &str, directory: bool) -> Result<()> {
        let name = c_name(name)?;
        if unsafe {
            libc::unlinkat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } != 0
        {
            return Err(os_error("无法清理重命名暂存文件"));
        }
        self.sync()
    }
}

pub(super) fn snapshot(
    mut file: File,
    limit: u64,
    cancel: Option<&AtomicBool>,
) -> Result<Snapshot> {
    let st = file.metadata().map_err(err)?;
    if !st.is_file() || st.len() > limit {
        return Err("核心文件类型无效或大小超过安全上限".into());
    }
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        if let Some(cancel) = cancel {
            check(cancel)?;
        }
        let n = file.read(&mut buf).map_err(err)?;
        if n == 0 {
            break;
        }
        size = size.checked_add(n as u64).ok_or("核心文件过大")?;
        if size > limit {
            return Err("核心文件大小超过安全上限".into());
        }
        digest.update(&buf[..n]);
    }
    let after = file.metadata().map_err(err)?;
    if size != st.len()
        || st.len() != after.len()
        || st.mtime() != after.mtime()
        || st.mtime_nsec() != after.mtime_nsec()
        || st.ctime() != after.ctime()
        || st.ctime_nsec() != after.ctime_nsec()
    {
        return Err("读取期间文件发生变化，请刷新后重试".into());
    }
    Ok(Snapshot {
        size,
        hash: format!("{:x}", digest.finalize()),
        dev: st.dev(),
        ino: st.ino(),
        mtime: st.mtime(),
        mtime_nsec: st.mtime_nsec(),
        ctime: st.ctime(),
        ctime_nsec: st.ctime_nsec(),
    })
}
pub(super) fn content_revision(dir: &Dir, old: &str, new: &str) -> Result<String> {
    let mut digest = Sha256::new();
    let mut count = 0usize;
    inspect_contents(dir, "", old, new, &mut digest, &mut count, 0)?;
    Ok(format!("{:x}", digest.finalize()))
}
fn inspect_contents(
    dir: &Dir,
    relative: &str,
    old: &str,
    new: &str,
    digest: &mut Sha256,
    count: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("实例内容目录深度超过重命名安全上限".into());
    }
    let before = dir.file.metadata().map_err(err)?;
    digest.update(serde_json::to_vec(&(relative, dir.key()?, before.mode())).map_err(err)?);
    for name in dir.names()? {
        if relative.is_empty()
            && [
                format!("{old}.json"),
                format!("{old}.jar"),
                format!("{new}.json"),
                format!("{new}.jar"),
            ]
            .contains(&name)
        {
            continue;
        }
        *count += 1;
        if *count > MAX_FILES {
            return Err("实例内容数量超过重命名安全上限".into());
        }
        let path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        let stat = dir.stat(&name)?.ok_or("检查期间实例内容已变化")?;
        match stat.st_mode&libc::S_IFMT {
            libc::S_IFDIR=>{
                let child=dir.child(&name)?; let key=child.key()?;
                inspect_contents(&child,&path,old,new,digest,count,depth+1)?;
                let current=dir.child(&name)?;
                if current.key()?!=key { return Err("检查期间实例内容目录已变化".into()); }
            }
            libc::S_IFREG=>digest.update(serde_json::to_vec(&(&path,stat.st_dev,stat.st_ino,stat.st_mode,stat.st_nlink,
                stat.st_size,stat.st_mtime,stat.st_mtime_nsec,stat.st_ctime,stat.st_ctime_nsec)).map_err(err)?),
            libc::S_IFLNK=>return Err(format!("实例内容 {path} 含符号链接，改名后可能改变隔离目录或路径绑定；请先处理链接后再重命名")),
            _=>return Err(format!("实例内容 {path} 含特殊文件，无法安全重命名")),
        }
    }
    let after = dir.file.metadata().map_err(err)?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("检查期间实例内容已变化，请重新读取重命名计划".into());
    }
    Ok(())
}

pub(super) struct RenameLock(pub(super) File);
impl Drop for RenameLock {
    fn drop(&mut self) {
        // A concurrent fork can retain this open-file description until exec.
        // Closing our fd alone would let that child prolong a finished writer's
        // lock. Explicitly unlock at the owner's lifetime boundary.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
pub(super) fn lock(store: &Dir) -> Result<RenameLock> {
    let file = match store.stat(".lock")? {
        Some(_) => store.regular(".lock")?,
        None => store.create_file(".lock")?,
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("其他进程正在重命名或恢复实例，请稍后重试".into());
    }
    Ok(RenameLock(file))
}
pub(super) fn journal_bytes(operation: &Dir) -> Result<Option<Vec<u8>>> {
    if operation.stat("journal.json")?.is_none() {
        return Ok(None);
    }
    let mut file = operation.regular("journal.json")?;
    if file.metadata().map_err(err)?.len() > MAX_JOURNAL {
        return Err("重命名记录超过安全上限".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_JOURNAL + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err("重命名记录超过安全上限".into());
    }
    Ok(Some(bytes))
}
pub(super) fn publish_journal(operation: &Dir, bytes: &[u8]) -> Result<()> {
    let mut file = operation.anonymous_file()?;
    file.write_all(&bytes).map_err(err)?;
    file.sync_all().map_err(err)?;
    let pinned = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(err)?;
    if operation.stat("journal.json")?.is_none() {
        let name = c_name("journal.json")?;
        if unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                pinned.as_ptr(),
                operation.file.as_raw_fd(),
                name.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        } != 0
        {
            return Err(os_error("无法发布完整的初始重命名记录"));
        }
        return operation.sync();
    }
    operation.regular("journal.json")?;
    let name = format!("journal-{}.next", operation_id());
    let linked = c_name(&name)?;
    if unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            pinned.as_ptr(),
            operation.file.as_raw_fd(),
            linked.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    } != 0
    {
        return Err(os_error("无法暂存完整的重命名记录"));
    }
    let result = (|| {
        if operation.stat("journal.json")?.is_some() {
            operation.regular("journal.json")?;
        }
        let source = c_name(&name)?;
        let target = c_name("journal.json")?;
        if unsafe {
            libc::renameat(
                operation.file.as_raw_fd(),
                source.as_ptr(),
                operation.file.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            return Err(os_error("无法保存重命名记录"));
        }
        operation.sync()
    })();
    if let Err(error) = result {
        if operation.stat(&name)?.is_some() {
            operation
                .unlink(&name, false)
                .map_err(|cleanup| format!("取消清理失败：{cleanup}；原错误：{error}"))?;
        }
        return Err(error);
    }
    Ok(())
}
pub(super) fn cleanup_journal_temps(operation: &Dir, j: &Journal) -> Result<()> {
    for name in operation.names()? {
        let Some(id) = name
            .strip_prefix("journal-")
            .and_then(|name| name.strip_suffix(".next"))
        else {
            continue;
        };
        if !operation_ok(id) {
            return Err("重命名记录目录含未知暂存文件，已保留内容".into());
        }
        let mut file = operation.regular(&name)?;
        if file.metadata().map_err(err)?.len() > MAX_JOURNAL {
            return Err("重命名暂存记录过大，已保留内容".into());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_JOURNAL + 1)
            .read_to_end(&mut bytes)
            .map_err(err)?;
        if bytes.len() as u64 > MAX_JOURNAL {
            return Err("重命名暂存记录过大，已保留内容".into());
        }
        let next: Journal =
            serde_json::from_slice(&bytes).map_err(|_| "重命名暂存记录含未知内容，已保留文件")?;
        validate(&next, &j.operation_id)?;
        if next.root != j.root
            || next.project != j.project
            || next.id != j.id
            || next.new_name != j.new_name
            || next.operation_key != j.operation_key
            || next.root_key != j.root_key
            || next.storage_key != j.storage_key
            || next.refs.revision()? != j.refs.revision()?
        {
            return Err("重命名暂存记录范围不符，已保留文件".into());
        }
        operation.unlink(&name, false)?;
    }
    Ok(())
}
pub(super) fn rename_new(source: &Dir, old: &str, target: &Dir, new: &str) -> Result<()> {
    let old = c_name(old)?;
    let new = c_name(new)?;
    if unsafe {
        libc::renameat2(
            source.file.as_raw_fd(),
            old.as_ptr(),
            target.file.as_raw_fd(),
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(os_error("无法无覆盖重命名实例文件或目录"));
    }
    source.sync()?;
    target.sync()
}
pub(super) fn stage_file(
    operation: &Dir,
    j: &mut Journal,
    index: usize,
    bytes: &[u8],
    cancel: &AtomicBool,
) -> Result<()> {
    check(cancel)?;
    let incoming = owned(operation, "incoming", &j.incoming_key)?.ok_or("替换目录缺失")?;
    let mut file = incoming.anonymous_file()?;
    for chunk in bytes.chunks(65536) {
        check(cancel)?;
        file.write_all(chunk).map_err(err)?;
    }
    file.sync_all().map_err(err)?;
    file.seek(SeekFrom::Start(0)).map_err(err)?;
    j.files[index].replacement = Some(snapshot(
        file.try_clone().map_err(err)?,
        MAX_JSON,
        Some(cancel),
    )?);
    write_journal(operation, j)?;
    check(cancel)?;
    let source = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(err)?;
    let name = c_name(&slot(index))?;
    if unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            incoming.file.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    } != 0
    {
        return Err(os_error("无法发布已登记的重命名 JSON 暂存文件"));
    }
    incoming.sync()
}

pub(super) const SCHEMA: u32 = 1;
pub(super) const REVISION_PREFIX: &str = "rename-v1-";
pub(super) fn stat_is_key(stat: &libc::stat, key: &DirKey) -> bool {
    stat.st_dev == key.dev && stat.st_ino == key.ino
}
pub(super) fn remove_registered(
    dir: &Dir,
    name: &str,
    _expected: &Snapshot,
    _limit: u64,
) -> Result<()> {
    dir.unlink(name, false)
}
pub(super) fn remove_directory(parent: &Dir, name: &str, _expected: &Dir) -> Result<()> {
    parent.unlink(name, true)
}

pub(super) fn valid_snapshot(_snapshot: &Snapshot, _limit: u64) -> Result<()> {
    Ok(())
}
