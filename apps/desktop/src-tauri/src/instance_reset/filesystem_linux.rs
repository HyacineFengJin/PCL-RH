//! Original Linux reset inode wire, no-follow/openat2/O_TMPFILE and pinned-FD links.
use super::*;
use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
};
pub(super) const SCHEMA: u32 = 1;
pub(super) const REVISION_PREFIX: &str = "reset-v1-";
pub(super) const JOURNAL_LIMIT: u64 = 128 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct DirKey {
    dev: u64,
    ino: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    pub(super) size: u64,
    pub(super) hash: String,
    dev: u64,
    ino: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
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
fn os_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}
fn c_name(name: &str) -> Result<CString> {
    name_ok(name)?;
    CString::new(name).map_err(err)
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
            return Err(os_error("无法打开实例重置目录"));
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
            return Err(os_error("无法创建实例重置目录"));
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
    pub(super) fn cache_file(&self, name: &str) -> Result<File> {
        // Published cache files may have one additional staging link until cleanup.
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(os_error("无法读取支持库文件"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata().map_err(err)?.is_file() {
            return Err("支持库必须是普通文件".into());
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
                return Err("目录文件数量超过重置安全上限".into());
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
            return Err(os_error("无法清理重置暂存文件"));
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
pub(super) struct ResetLock {
    pub(super) file: File,
}
impl Drop for ResetLock {
    fn drop(&mut self) {
        // flock belongs to the open file description shared by fork/dup. Closing
        // our descriptor alone can leave it locked until a child reaches exec.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(super) fn lock(storage: &Dir) -> Result<ResetLock> {
    let file = match storage.stat(".lock")? {
        Some(_) => storage.regular(".lock")?,
        None => storage.create_file(".lock")?,
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("其他进程正在重置或恢复实例组件，请稍后重试".into());
    }
    Ok(ResetLock { file })
}
pub(super) fn rename_new(
    source: &Dir,
    source_name: &str,
    target: &Dir,
    target_name: &str,
) -> Result<()> {
    let source_name = c_name(source_name)?;
    let target_name = c_name(target_name)?;
    if unsafe {
        libc::renameat2(
            source.file.as_raw_fd(),
            source_name.as_ptr(),
            target.file.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(os_error("无法交换实例核心文件"));
    }
    source.sync()?;
    target.sync()
}
pub(super) fn remove_owned_tree(
    parent: &Dir,
    name: &str,
    key: &DirKey,
    count: &mut usize,
) -> Result<()> {
    remove_owned_tree_at(parent, name, key, count, 0)
}
fn remove_owned_tree_at(
    parent: &Dir,
    name: &str,
    key: &DirKey,
    count: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("重置暂存目录深度超过安全上限".into());
    }
    let dir = parent.child(name)?;
    if dir.key()? != *key {
        return Err("重置暂存目录已被替换".into());
    }
    for name in dir.names()? {
        *count += 1;
        if *count > MAX_FILES {
            return Err("暂存清理数量超过安全上限".into());
        }
        let st = dir.stat(&name)?.ok_or("暂存文件已变化")?;
        match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => {
                let child = dir.child(&name)?;
                remove_owned_tree_at(&dir, &name, &child.key()?, count, depth + 1)?;
            }
            libc::S_IFREG => {
                dir.cache_file(&name)?;
                dir.unlink(&name, false)?;
            }
            _ => return Err("重置暂存目录含符号链接或特殊文件，已保留待清理内容".into()),
        }
    }
    if parent.child(name)?.key()? != *key {
        return Err("重置暂存目录清理期间已变化".into());
    }
    parent.unlink(name, true)
}
pub(super) fn copy_core(
    source: File,
    incoming: &Dir,
    operation: &Dir,
    journal: &mut Journal,
    index: usize,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut source = source;
    let mut target = incoming.anonymous_file()?;
    let mut buf = [0u8; 65536];
    let mut size = 0u64;
    (|| {
        loop {
            check(cancel)?;
            let n = source.read(&mut buf).map_err(err)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > limit(index) {
                return Err("生成的核心文件过大".into());
            }
            target.write_all(&buf[..n]).map_err(err)?;
        }
        target.sync_all().map_err(err)?;
        target.seek(SeekFrom::Start(0)).map_err(err)?;
        journal.files[index].replacement = Some(snapshot(
            target.try_clone().map_err(err)?,
            limit(index),
            Some(cancel),
        )?);
        // Publish ownership and complete contents before giving this inode a
        // name. A crash at any earlier point automatically drops the anonymous
        // file; a crash here leaves a registered, optionally absent incoming file.
        write_journal(operation, journal)?;
        check(cancel)?;
        let pinned = CString::new(format!("/proc/self/fd/{}", target.as_raw_fd())).map_err(err)?;
        let name = c_name(&journal.files[index].name)?;
        if unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                pinned.as_ptr(),
                incoming.file.as_raw_fd(),
                name.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        } != 0
        {
            return Err(os_error("无法发布已登记的核心暂存文件"));
        }
        incoming.sync()
    })()
}
pub(super) fn merge_cache(
    source: &Dir,
    target: &Dir,
    cancel: &AtomicBool,
    count: &mut usize,
) -> Result<()> {
    merge_cache_at(source, target, cancel, count, 0)
}
fn merge_cache_at(
    source: &Dir,
    target: &Dir,
    cancel: &AtomicBool,
    count: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("支持库目录深度超过安全上限".into());
    }
    for name in source.names()? {
        check(cancel)?;
        *count += 1;
        if *count > MAX_FILES {
            return Err("安装支持文件超过重置安全上限".into());
        }
        let st = source.stat(&name)?.ok_or("安装支持文件已变化")?;
        match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => merge_cache_at(
                &source.child(&name)?,
                &target.ensure(&name)?,
                cancel,
                count,
                depth + 1,
            )?,
            libc::S_IFREG => {
                let file = source.cache_file(&name)?;
                let expected = snapshot(source.cache_file(&name)?, MAX_JAR, Some(cancel))?;
                if target.stat(&name)?.is_some() {
                    let existing = snapshot(target.cache_file(&name)?, MAX_JAR, Some(cancel))?;
                    if expected.size != existing.size || expected.hash != existing.hash {
                        return Err(format!(
                            "已有支持文件与新组件不一致，已保留现有文件：{name}"
                        ));
                    }
                    continue;
                }
                // Link the pinned ordinary file, rather than its mutable source
                // pathname. No existing shared cache file is replaced. A mount
                // boundary fails safely before any instance core is changed.
                let source_fd =
                    CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(err)?;
                let target_name = c_name(&name)?;
                if unsafe {
                    libc::linkat(
                        libc::AT_FDCWD,
                        source_fd.as_ptr(),
                        target.file.as_raw_fd(),
                        target_name.as_ptr(),
                        libc::AT_SYMLINK_FOLLOW,
                    )
                } != 0
                {
                    return Err(os_error(
                        "无法发布新支持文件（不覆盖已有文件，且须位于同一文件系统）",
                    ));
                }
                target.sync()?;
                let actual = snapshot(target.cache_file(&name)?, MAX_JAR, Some(cancel))?;
                if !expected.stable_matches(&actual) {
                    return Err("新支持文件发布校验失败".into());
                }
            }
            _ => return Err("生成的支持文件包含符号链接或特殊文件".into()),
        }
    }
    Ok(())
}
pub(super) fn publish_journal(operation: &Dir, bytes: &[u8]) -> Result<()> {
    let name = format!("journal-{}.next", operation_id());
    let mut next = operation.create_file(&name)?;
    let written = (|| {
        next.write_all(&bytes).map_err(err)?;
        next.sync_all().map_err(err)
    })();
    if let Err(error) = written {
        return match operation.unlink(&name, false) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
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
        let error = os_error("无法保存实例重置记录");
        return match operation.unlink(&name, false) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
    operation.sync()
}
pub(super) fn valid_snapshot(_snapshot: &Snapshot) -> Result<()> {
    Ok(())
}
pub(super) fn valid_key(_key: &DirKey) -> Result<()> {
    Ok(())
}
pub(super) fn journal_bytes(operation: &Dir) -> Result<Option<Vec<u8>>> {
    if operation.stat("journal.json")?.is_none() {
        return Ok(None);
    }
    let file = operation.regular("journal.json")?;
    if file.metadata().map_err(err)?.len() > MAX_JOURNAL {
        return Err("实例重置记录过大".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err("实例重置记录过大".into());
    }
    Ok(Some(bytes))
}
pub(super) fn operation_names(operation: &Dir) -> Result<Vec<String>> {
    operation.names()
}
pub(super) fn failed_initial_journal(
    store: &Dir,
    operation: &Dir,
    j: &Journal,
    error: String,
) -> String {
    let _ = operation;
    match remove_owned_tree(store, &j.operation_id, &j.operation_key, &mut 0) {
        Ok(()) => error,
        Err(cleanup) => format!("取消清理失败：{cleanup}；原错误：{error}"),
    }
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
impl Dir {
    pub(super) fn bound_file(&self) -> Result<File> {
        self.file.try_clone().map_err(err)
    }
}
pub(super) fn merge_cache_bound(
    source: &Dir,
    target: &Dir,
    _root: &Dir,
    cancel: &AtomicBool,
    count: &mut usize,
) -> Result<()> {
    merge_cache(source, target, cancel, count)
}
