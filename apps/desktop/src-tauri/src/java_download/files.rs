//! Managed runtimes publish as a complete directory with no replacement.
//! Every write and cleanup is relative to held private-directory descriptors;
//! links are leaf entries from a validated manifest, never traversal authority.
//! A durable owner record makes interrupted stages recognizable. Unknown or
//! modified contents are retained, and cleanup failure outranks cancellation.
use super::{
    catalog::{self, Entry, Manifest, Package},
    Result,
};
use crate::launcher_local::filesystem::{Dir, WriteLock};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
const OWNER: &str = ".pcl-java-owner.json";
const OWNER_LIMIT: u64 = 4 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Owner {
    schema: u32,
    stage: String,
    final_name: String,
    identity: (u64, u64),
    manifest: Manifest,
}
pub struct Store {
    pub path: PathBuf,
    pub dir: Dir,
}
pub struct Stage<'a> {
    store: &'a Store,
    pub dir: Dir,
    name: String,
    owner: Owner,
    published: bool,
    _lock: &'a WriteLock,
}
fn c(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| "Java 路径含有空字符".into())
}
fn io(label: &str) -> String {
    format!("{label}：{}", std::io::Error::last_os_error())
}

/// Unlike ordinary directory discovery, stage descent also refuses bind mounts.
/// The existing /data mount is above this captured root and remains supported.
fn child(parent: &Dir, name: &str) -> Result<Dir> {
    crate::launcher_local::filesystem::component(name)?;
    #[repr(C)]
    struct How {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = How {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x01 | 0x04 | 0x08,
    };
    let name = c(name)?;
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.0.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<How>(),
        )
    };
    if fd < 0 {
        return Err(io(
            "无法打开 Java 目录（需要 openat2，不允许链接或嵌套挂载）",
        ));
    }
    let dir = Dir(unsafe { File::from_raw_fd(fd as i32) });
    dir.require_private()?;
    Ok(dir)
}
fn descend(root: &Dir, path: &str) -> Result<(Dir, String)> {
    let mut parts = catalog::parts(path)?;
    let leaf = parts.pop().unwrap().to_string();
    let mut dir = Dir(root.0.try_clone().map_err(|e| e.to_string())?);
    for part in parts {
        dir = child(&dir, part)?;
    }
    Ok((dir, leaf))
}
fn link_target(parent: &Dir, name: &str) -> Result<String> {
    let mut bytes = vec![0u8; 4097];
    let name = c(name)?;
    let n = unsafe {
        libc::readlinkat(
            parent.0.as_raw_fd(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if n < 0 || n as usize == bytes.len() {
        return Err(io("Java 内部链接无效或过长"));
    }
    String::from_utf8(bytes[..n as usize].to_vec()).map_err(|_| "Java 内部链接不是 UTF-8".into())
}
fn unlink(parent: &Dir, name: &str, directory: bool) -> Result<()> {
    let name = c(name)?;
    if unsafe {
        libc::unlinkat(
            parent.0.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } != 0
    {
        return Err(io("无法清理 Java 暂存项"));
    }
    parent.sync()
}
fn verify_file(file: &mut File, size: u64, hash: &str, executable: bool) -> Result<()> {
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if meta.len() != size || meta.mode() & 0o777 != if executable { 0o700 } else { 0o600 } {
        return Err("Java 暂存文件大小或权限已变化，已保留文件".into());
    }
    let mut digest = Sha1::new();
    let mut total = 0u64;
    let mut bytes = [0u8; 65536];
    loop {
        let n = file.read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > size {
            return Err("Java 暂存文件已变化".into());
        }
        digest.update(&bytes[..n]);
    }
    if total != size || format!("{:x}", digest.finalize()) != hash {
        return Err("Java 暂存内容已被修改，已保留文件".into());
    }
    Ok(())
}
impl Store {
    pub fn capture(project: &Path) -> Result<Self> {
        super::StoreBinding::capture(project)?.open()
    }
    fn recheck(&self) -> Result<()> {
        if Dir::absolute(&self.path)?.identity()? != self.dir.identity()? {
            return Err("Java 托管目录已改变".into());
        }
        Ok(())
    }
    pub fn recover(&self, lock: &WriteLock) -> Result<()> {
        self.recheck()?;
        for name in self
            .dir
            .names()?
            .into_iter()
            .filter(|n| n.starts_with(".stage-"))
        {
            let dir = child(&self.dir, &name)?;
            let mut file = dir
                .file(OWNER)?
                .ok_or("Java 暂存目录缺少所有权记录，已保留目录")?;
            let mut raw = Vec::new();
            file.by_ref()
                .take(OWNER_LIMIT + 1)
                .read_to_end(&mut raw)
                .map_err(|e| e.to_string())?;
            if raw.len() as u64 > OWNER_LIMIT {
                return Err("Java 暂存记录超过限制，已保留目录".into());
            }
            let owner: Owner = serde_json::from_slice(&raw)
                .map_err(|_| "Java 暂存记录不支持或已修改，已保留目录")?;
            if owner.schema != 1 || owner.stage != name || owner.identity != dir.identity()? {
                return Err("Java 暂存记录身份不匹配，已保留目录".into());
            }
            owner.manifest.validate()?;
            Stage {
                store: self,
                dir,
                name,
                owner,
                published: false,
                _lock: lock,
            }
            .cleanup()?;
        }
        Ok(())
    }
    pub fn create<'a>(
        &'a self,
        package: &Package,
        manifest: &Manifest,
        task_id: &str,
        lock: &'a WriteLock,
    ) -> Result<Stage<'a>> {
        self.recheck()?;
        let final_name = format!("{}-{}", package.component, package.sha1);
        // Refuse an existing runtime before transferring any payload. It is
        // never overwritten, even if its executable later becomes unavailable.
        let final_c = c(&final_name)?;
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.dir.0.as_raw_fd(),
                final_c.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Err("该 Java 运行时已安装，原文件已保留".into());
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
            return Err(io("无法检查已有 Java"));
        }
        let name = format!(".stage-{task_id}");
        crate::launcher_local::filesystem::component(&name)?;
        let name_c = c(&name)?;
        if unsafe { libc::mkdirat(self.dir.0.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
            return Err(io("无法创建独立 Java 暂存目录"));
        }
        self.dir.sync()?;
        let dir = child(&self.dir, &name)?;
        let owner = Owner {
            schema: 1,
            stage: name.clone(),
            final_name,
            identity: dir.identity()?,
            manifest: manifest.clone(),
        };
        let stage = Stage {
            store: self,
            dir,
            name,
            owner,
            published: false,
            _lock: lock,
        };
        let raw = serde_json::to_vec(&stage.owner).map_err(|e| e.to_string())?;
        if raw.len() as u64 > OWNER_LIMIT {
            unlink(&self.dir, &stage.name, true)?;
            return Err("Java 暂存记录超过限制".into());
        }
        if let Err(e) = stage.dir.write_new(OWNER, &raw) {
            // A failed directory sync may mean the marker exists. Its content
            // is still compared with our exact in-memory owner before cleanup.
            if stage.dir.names()?.is_empty() {
                unlink(&self.dir, &stage.name, true)?;
            } else {
                stage.cleanup()?;
            }
            return Err(e);
        }
        Ok(stage)
    }
}
impl Stage<'_> {
    pub fn prepare_directories(&self) -> Result<()> {
        let mut directories: Vec<_> = self
            .owner
            .manifest
            .files
            .iter()
            .filter_map(|(p, e)| matches!(e, Entry::Directory).then_some(p))
            .collect();
        directories.sort_by_key(|p| p.matches('/').count());
        for path in directories {
            let (parent, name) = descend(&self.dir, path)?;
            parent.create(&name)?;
            child(&parent, &name)?;
        }
        Ok(())
    }
    pub fn anonymous(&self, path: &str) -> Result<File> {
        let (parent, _) = descend(&self.dir, path)?;
        let fd = unsafe {
            libc::openat(
                parent.0.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io("Java 下载需要支持匿名暂存的文件系统"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub fn publish_file(&self, path: &str, file: &File, executable: bool) -> Result<()> {
        file.set_permissions(std::fs::Permissions::from_mode(if executable {
            0o700
        } else {
            0o600
        }))
        .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let (parent, name) = descend(&self.dir, path)?;
        let name = c(&name)?;
        if unsafe {
            libc::linkat(
                file.as_raw_fd(),
                c"".as_ptr(),
                parent.0.as_raw_fd(),
                name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        } != 0
        {
            return Err(io("无法提交校验后的 Java 文件"));
        }
        parent.sync()
    }
    pub fn links(&self) -> Result<()> {
        for (path, entry) in &self.owner.manifest.files {
            if let Entry::Link { target } = entry {
                let (parent, name) = descend(&self.dir, path)?;
                let name = c(&name)?;
                let target = c(target)?;
                if unsafe { libc::symlinkat(target.as_ptr(), parent.0.as_raw_fd(), name.as_ptr()) }
                    != 0
                {
                    return Err(io("无法建立 Java 内部链接"));
                }
                parent.sync()?;
            }
        }
        Ok(())
    }
    fn validate(&self, complete: bool) -> Result<()> {
        let mut file = self.dir.file(OWNER)?.ok_or("Java 暂存所有权记录缺失")?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(OWNER_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > OWNER_LIMIT
            || serde_json::from_slice::<Owner>(&bytes).ok().as_ref() != Some(&self.owner)
        {
            return Err("Java 暂存记录已改变，已保留目录".into());
        }
        let count = self.walk(&self.dir, "", false)?;
        if complete && count != self.owner.manifest.files.len() {
            return Err("Java 运行时文件不完整".into());
        }
        Ok(())
    }
    fn walk(&self, dir: &Dir, prefix: &str, remove: bool) -> Result<usize> {
        let mut count = 0;
        for name in dir.names()? {
            if prefix.is_empty() && name == OWNER {
                continue;
            }
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let entry = self
                .owner
                .manifest
                .files
                .get(&path)
                .ok_or("Java 暂存目录出现额外文件，已保留目录")?;
            match entry {
                Entry::Directory => {
                    let next = child(dir, &name)?;
                    count += self.walk(&next, &path, remove)?;
                    if remove {
                        unlink(dir, &name, true)?;
                    }
                }
                Entry::File {
                    executable,
                    downloads,
                } => {
                    let raw = downloads.get("raw").unwrap();
                    let mut file = dir.file(&name)?.ok_or("Java 暂存文件丢失")?;
                    verify_file(&mut file, raw.size, &raw.sha1, *executable)?;
                    if remove {
                        unlink(dir, &name, false)?;
                    }
                }
                Entry::Link { target } => {
                    if link_target(dir, &name)? != *target {
                        return Err("Java 内部链接已修改，已保留目录".into());
                    }
                    if remove {
                        unlink(dir, &name, false)?;
                    }
                }
            }
            count += 1;
        }
        Ok(count)
    }
    pub fn java_path(&self) -> Result<PathBuf> {
        self.store.recheck()?;
        if child(&self.store.dir, &self.name)?.identity()? != self.dir.identity()? {
            return Err("Java 暂存目录已改变".into());
        }
        Ok(self.store.path.join(&self.name).join("bin/java"))
    }
    pub fn verify_complete(&self) -> Result<()> {
        self.validate(true)
    }
    pub fn publish(&mut self) -> Result<PathBuf> {
        self.validate(true)?;
        self.java_path()?;
        self.dir.sync()?;
        let old = c(&self.name)?;
        let new = c(&self.owner.final_name)?;
        if unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.store.dir.0.as_raw_fd(),
                old.as_ptr(),
                self.store.dir.0.as_raw_fd(),
                new.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(io("无法发布 Java 运行时（目标必须不存在）"));
        }
        self.published = true;
        // Once renamed, a later sync error cannot authorize deleting a complete
        // runtime. The caller reports the retained installation for recovery.
        self.store
            .dir
            .sync()
            .map_err(|e| format!("Java 已安装，但目录同步失败，请保留运行时：{e}"))?;
        Ok(self
            .store
            .path
            .join(&self.owner.final_name)
            .join("bin/java"))
    }
    pub fn cleanup(&self) -> Result<()> {
        if self.published {
            return Ok(());
        }
        if child(&self.store.dir, &self.name)?.identity()? != self.dir.identity()? {
            return Err("Java 暂存目录身份已改变，已保留目录".into());
        }
        self.validate(false)?;
        self.walk(&self.dir, "", true)?;
        unlink(&self.dir, OWNER, false)?;
        unlink(&self.store.dir, &self.name, true)
    }
}
