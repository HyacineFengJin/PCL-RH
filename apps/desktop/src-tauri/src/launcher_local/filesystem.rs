//! Bounded FD-relative persistence for local statistics, diagnostics and entries.
//! Cooperative writers use separate flock names; optimistic snapshots catch
//! other editors, and changed publication bytes are retained for inspection.
#[cfg(not(target_os = "linux"))]
use pcl_core::retained_fs;
use pcl_core::retained_fs::stage::{PublishOutcome, Stage};
#[cfg(windows)]
use pcl_core::retained_fs::Kind;
use pcl_core::retained_fs::{Directory, Identity, Leaf};
#[cfg(not(target_os = "linux"))]
use pcl_core::retained_fs::{Links, Lock, Traversal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::{ffi::CStr, os::fd::FromRawFd};
#[cfg(unix)]
use std::{
    ffi::CString,
    fs::Metadata,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

#[cfg(windows)]
#[path = "windows_versions.rs"]
mod windows_versions;

pub(crate) const MAX_ENTRIES: usize = 4096;
pub(crate) fn component(name: &str) -> Result<(), String> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.len() > 255
        || name.contains(['/', '\0'])
        || name.chars().any(char::is_control)
    {
        return Err("启动器本地文件文件名无效".into());
    }
    Leaf::new(name)
        .map(|_| ())
        .map_err(|e| format!("启动器本地文件文件名无效：{e}"))
}
fn leaf(name: &str) -> Result<Leaf, String> {
    component(name)?;
    Leaf::new(name).map_err(|e| e.to_string())
}
#[cfg(unix)]
fn cstring(name: &str) -> Result<CString, String> {
    component(name)?;
    CString::new(name).map_err(|_| "启动器本地文件文件名无效".into())
}
#[cfg(unix)]
fn error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}
#[cfg(unix)]
pub(crate) type DirectoryIdentity = (u64, u64);
#[cfg(windows)]
pub(crate) type DirectoryIdentity = Identity;

#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum WindowsFormat {
    #[serde(rename = "windows-file-id-128-v1")]
    FileId128V1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stamp {
    #[cfg(unix)]
    pub device: u64,
    #[cfg(unix)]
    pub inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    owner: u32,
    #[cfg(windows)]
    format: WindowsFormat,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
    #[cfg(windows)]
    security: retained_fs::Security,
    links: u64,
    pub bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    // Kept for Unix consumers that already hold std metadata, and preserves
    // byte-for-byte legacy journal field order and timestamp representation.
    #[cfg(unix)]
    pub fn of(m: &Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            mode: m.mode(),
            owner: m.uid(),
            links: m.nlink(),
            bytes: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
    pub fn from_file(file: &File) -> Result<Self, String> {
        #[cfg(unix)]
        {
            Ok(Self::of(&file.metadata().map_err(|e| e.to_string())?))
        }
        #[cfg(windows)]
        {
            let m = retained_fs::metadata(file).map_err(|e| e.to_string())?;
            if m.kind != Kind::Regular || m.reparse_tag.is_some() {
                return Err("启动器本地文件不是字面普通文件".into());
            }
            let Identity::Windows { volume, file } = m.identity else {
                return Err("本地文件身份不属于当前平台".into());
            };
            Ok(Self {
                format: WindowsFormat::FileId128V1,
                volume,
                file_id: file,
                security: m.security,
                links: m.links,
                bytes: m.len,
                modified: (m.modified.seconds, i64::from(m.modified.nanos)),
                changed: (m.changed.seconds, i64::from(m.changed.nanos)),
            })
        }
    }
    #[cfg(target_os = "linux")]
    fn safe(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
            && self.owner == unsafe { libc::geteuid() }
            && self.links == 1
    }
    pub fn same_volume(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device
        }
        #[cfg(windows)]
        {
            self.volume == other.volume
        }
    }
    pub fn same_identity(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.same_volume(other) && self.inode == other.inode
        }
        #[cfg(windows)]
        {
            self.same_volume(other) && self.file_id == other.file_id
        }
    }
    pub fn moved_matches(&self, other: &Self) -> bool {
        #[cfg(unix)]
        let same = self.device == other.device
            && self.inode == other.inode
            && self.mode == other.mode
            && self.owner == other.owner;
        #[cfg(windows)]
        let same = self.format == other.format
            && self.volume == other.volume
            && self.file_id == other.file_id
            && self.security == other.security;
        same && self.links == other.links
            && self.bytes == other.bytes
            && self.modified == other.modified
    }
}

pub(crate) struct Dir(pub File);
impl Dir {
    fn native(&self) -> Result<Directory, String> {
        Directory::from_file(self.0.try_clone().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
    pub fn create_absolute(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("本地入口目录必须是绝对路径".into());
        }
        let mut root = PathBuf::new();
        let mut names = Vec::new();
        for part in path.components() {
            match part {
                Component::Prefix(prefix) if names.is_empty() => root.push(prefix.as_os_str()),
                Component::RootDir if names.is_empty() => root.push(part.as_os_str()),
                Component::Normal(name) => {
                    names.push(name.to_str().ok_or("本地入口目录必须是 UTF-8")?)
                }
                _ => return Err("本地入口目录不能含有相对路径跳转".into()),
            }
        }
        let mut dir = Self::absolute(&root)?;
        for name in names {
            dir = match dir.optional(name)? {
                Some(dir) => dir,
                None => dir.create(name)?,
            };
        }
        Ok(dir)
    }
    pub fn absolute(path: &Path) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            if !path.is_absolute() {
                return Err("启动器本地文件路径必须是绝对路径".into());
            }
            let fd = unsafe {
                libc::open(
                    c"/".as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(error("无法打开根目录"));
            }
            let mut dir = Self(unsafe { File::from_raw_fd(fd) });
            for part in path.components() {
                match part {
                    Component::RootDir => {}
                    Component::Normal(name) => {
                        dir = dir.child(name.to_str().ok_or("启动器本地文件路径必须是 UTF-8")?)?
                    }
                    _ => return Err("启动器本地文件路径不能包含相对路径跳转".into()),
                }
            }
            Ok(dir)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let dir = Directory::open_absolute(path)
                .map_err(|e| format!("无法打开启动器本地目录：{e}"))?;
            Ok(Self(dir.try_clone_file().map_err(|e| e.to_string())?))
        }
    }
    pub fn optional(&self, name: &str) -> Result<Option<Self>, String> {
        #[cfg(target_os = "linux")]
        {
            let c = cstring(name)?;
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd >= 0 {
                return Ok(Some(Self(unsafe { File::from_raw_fd(fd) })));
            }
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error("无法打开启动器本地文件目录（不允许符号链接）"))
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let dir = self.native()?;
            match dir.child(&leaf(name)?, Traversal::AllowMounts) {
                Ok(child) => Ok(Some(Self(
                    child.try_clone_file().map_err(|e| e.to_string())?,
                ))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("无法打开启动器本地文件目录（不允许链接）：{e}")),
            }
        }
    }
    pub fn child(&self, name: &str) -> Result<Self, String> {
        self.optional(name)?
            .ok_or_else(|| format!("启动器本地文件目录不存在：{name}"))
    }
    pub fn create(&self, name: &str) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            let c = cstring(name)?;
            if unsafe { libc::mkdirat(self.0.as_raw_fd(), c.as_ptr(), 0o700) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
            {
                return Err(error("无法创建启动器本地文件目录"));
            }
            let dir = self.child(name)?;
            if dir.0.metadata().map_err(|e| e.to_string())?.uid() != unsafe { libc::geteuid() } {
                return Err("启动器本地文件目录不属于当前用户".into());
            }
            self.sync()?;
            Ok(dir)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let native = self.native()?;
            let dir = match native.create_private(&leaf(name)?, Traversal::AllowMounts) {
                Ok(child) => Self(child.try_clone_file().map_err(|e| e.to_string())?),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => self.child(name)?,
                Err(e) => return Err(format!("无法创建启动器本地文件目录：{e}")),
            };
            dir.native()?
                .metadata()
                .and_then(|m| m.require_owned())
                .map_err(|e| e.to_string())?;
            self.sync()?;
            Ok(dir)
        }
    }
    pub fn identity(&self) -> Result<DirectoryIdentity, String> {
        let identity = self.native()?.identity().map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            match identity {
                Identity::Unix { device, inode } => Ok((device, inode)),
                _ => Err("目录身份不属于当前平台".into()),
            }
        }
        #[cfg(windows)]
        {
            Ok(identity)
        }
    }
    pub fn require_private(&self) -> Result<(), String> {
        self.native()?
            .metadata()
            .and_then(|m| m.require_private())
            .map_err(|e| format!("本地服务目录必须由当前用户独占，已保留目录权限：{e}"))
    }
    pub fn file(&self, name: &str) -> Result<Option<File>, String> {
        #[cfg(target_os = "linux")]
        {
            let c = cstring(name)?;
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                )
            };
            if fd < 0 {
                return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(error("无法读取启动器本地文件（不允许符号链接）"))
                };
            }
            let file = unsafe { File::from_raw_fd(fd) };
            if !Stamp::of(&file.metadata().map_err(|e| e.to_string())?).safe() {
                return Err("只允许当前用户独占的普通启动器本地文件文件".into());
            }
            Ok(Some(file))
        }
        #[cfg(not(target_os = "linux"))]
        {
            match self.native()?.open_regular(&leaf(name)?, Links::Single) {
                Ok(file) => {
                    retained_fs::metadata(&file)
                        .and_then(|m| m.require_owned())
                        .map_err(|e| e.to_string())?;
                    Ok(Some(file))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!(
                    "无法读取启动器本地文件（不允许链接或特殊文件）：{e}"
                )),
            }
        }
    }
    pub fn names(&self) -> Result<Vec<String>, String> {
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    c".".as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                return Err(error("无法扫描启动器本地文件目录"));
            }
            let stream = unsafe { libc::fdopendir(fd) };
            if stream.is_null() {
                unsafe {
                    libc::close(fd);
                }
                return Err(error("无法扫描启动器本地文件目录"));
            }
            struct Stream(*mut libc::DIR);
            impl Drop for Stream {
                fn drop(&mut self) {
                    unsafe {
                        libc::closedir(self.0);
                    }
                }
            }
            let stream = Stream(stream);
            let mut names = Vec::new();
            loop {
                unsafe {
                    *libc::__errno_location() = 0;
                }
                let item = unsafe { libc::readdir(stream.0) };
                if item.is_null() {
                    if std::io::Error::last_os_error().raw_os_error() != Some(0) {
                        return Err(error("启动器本地文件目录扫描失败"));
                    }
                    break;
                }
                let bytes = unsafe { CStr::from_ptr((*item).d_name.as_ptr()) }.to_bytes();
                if matches!(bytes, b"." | b"..") {
                    continue;
                }
                names.push(
                    std::str::from_utf8(bytes)
                        .map_err(|_| "启动器本地文件文件名必须是 UTF-8")?
                        .to_owned(),
                );
                if names.len() > MAX_ENTRIES {
                    return Err("启动器本地文件目录最多允许 4096 个文件".into());
                }
            }
            names.sort();
            Ok(names)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut names = self
                .native()?
                .names(MAX_ENTRIES)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|n| {
                    n.into_string()
                        .map_err(|_| "启动器本地文件文件名必须是 UTF-8".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            names.sort();
            Ok(names)
        }
    }
    pub fn sync(&self) -> Result<(), String> {
        self.native()?
            .sync()
            .map_err(|e| format!("启动器本地文件目录保存失败：{e}"))
    }
    pub fn lock(&self, name: &str) -> Result<WriteLock, String> {
        #[cfg(target_os = "linux")]
        {
            let c = cstring(name)?;
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    c.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(error("无法打开本地服务锁"));
            }
            let file = unsafe { File::from_raw_fd(fd) };
            if !Stamp::of(&file.metadata().map_err(|e| e.to_string())?).safe() {
                return Err("本地服务锁不是当前用户独占的普通文件".into());
            }
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err("另一个本地服务写入任务正在运行".into());
            }
            Ok(WriteLock(file))
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.native()?
                .lock(&leaf(name)?)
                .map(WriteLock)
                .map_err(|e| format!("无法获取本地服务独占写入锁：{e}"))
        }
    }
    pub fn move_new(&self, name: &str, destination: &Dir) -> Result<(), String> {
        component(name)?;
        pcl_core::file_ops::rename_noreplace_at(
            &self.0,
            name.as_ref(),
            &destination.0,
            name.as_ref(),
        )
        .map_err(|e| format!("无法移动本地入口（目标必须不存在，可能已移动时请保留复核）：{e}"))?;
        self.sync()?;
        destination.sync()
    }
    pub fn replace(
        &self,
        name: &str,
        expected: Option<&Snapshot>,
        bytes: &[u8],
        limit: u64,
    ) -> Result<(), String> {
        self.replace_checked(name, expected, bytes, limit, || {})
    }
    fn replace_checked(
        &self,
        name: &str,
        expected: Option<&Snapshot>,
        bytes: &[u8],
        limit: u64,
        _after_exchange: impl FnOnce(),
    ) -> Result<(), String> {
        if bytes.len() as u64 > limit {
            return Err("本地服务文件超过安全上限".into());
        }
        #[cfg(windows)]
        if windows_versions::managed(name) {
            return windows_versions::replace(self, name, expected, bytes, limit);
        }
        let token = |snapshot: &Snapshot| (snapshot.stamp.clone(), snapshot.digest.clone());
        let fresh = optional_snapshot(self, name, limit)?;
        if fresh.as_ref().map(token) != expected.map(token) {
            return Err("本地服务文件已被外部修改，已保留原文件".into());
        }
        if expected.is_none() {
            return self.write_new(name, bytes);
        }
        #[cfg(windows)]
        return Err("此平台尚未实现本地服务文件的可恢复原子替换；原文件已保留".into());
        #[cfg(unix)]
        {
            let temp = format!(
                ".local-stage-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            self.write_new(&temp, bytes)?;
            let staged = Snapshot::open(self, &temp, limit)?;
            if let Err(e) = self.exchange(&temp, name) {
                self.remove_owned(&temp, &staged, limit)?;
                return Err(e);
            }
            _after_exchange();
            self.sync()?;
            let published = Snapshot::open(self, name, limit)?;
            if !published.stamp.moved_matches(&staged.stamp) || published.digest != staged.digest {
                // A writer may still hold the newly published inode. Keep the
                // displaced original instead of silently discarding its backup.
                return Err(format!(
                    "本地服务保存期间新文件被修改，原暂存副本已保留：{temp}"
                ));
            }
            if let Some(expected) = expected {
                let displaced = Snapshot::open(self, &temp, limit)?;
                if !displaced.stamp.moved_matches(&expected.stamp)
                    || displaced.digest != expected.digest
                {
                    // Never destroy an editor's displaced bytes. Roll back only if
                    // the visible destination is still exactly our new file.
                    let published = Snapshot::open(self, name, limit)?;
                    if published.stamp.moved_matches(&staged.stamp)
                        && published.digest == staged.digest
                    {
                        if self.exchange(&temp, name).is_err() {
                            return Err(format!("本地服务保存发生冲突，两个文件均已保留：{temp}"));
                        }
                        self.sync()?;
                    }
                    return Err(format!("本地服务保存发生冲突，暂存文件已保留：{temp}"));
                }
                self.remove_owned(&temp, &displaced, limit)?;
            }
            Ok(())
        }
    }
    #[cfg(unix)]
    fn remove_owned(&self, name: &str, expected: &Snapshot, limit: u64) -> Result<(), String> {
        let current = Snapshot::open(self, name, limit)?;
        if current.stamp != expected.stamp || current.digest != expected.digest {
            return Err("本地服务暂存文件发生变化，已保留".into());
        }
        let name = cstring(name)?;
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(error("无法清理已验证的本地服务暂存文件"));
        }
        self.sync()
    }
    #[cfg(unix)]
    fn exchange(&self, source: &str, target: &str) -> Result<(), String> {
        let from = cstring(source)?;
        let to = cstring(target)?;
        #[cfg(target_os = "linux")]
        let code = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.0.as_raw_fd(),
                from.as_ptr(),
                self.0.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        #[cfg(target_os = "macos")]
        let code = unsafe {
            libc::renameatx_np(
                self.0.as_raw_fd(),
                from.as_ptr(),
                self.0.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_SWAP,
            )
        };
        if code != 0 {
            Err(error("无法原子交换本地服务文件"))
        } else {
            Ok(())
        }
    }
    pub fn write_new(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.write_new_mode(name, bytes, 0o600)
    }
    pub fn write_new_mode(&self, name: &str, bytes: &[u8], mode: u32) -> Result<(), String> {
        let name = leaf(name)?;
        if !matches!(mode, 0o600 | 0o700) {
            return Err("本地服务只允许私有文件权限".into());
        }
        #[cfg(windows)]
        if mode != 0o600 {
            return Err("Windows 本地入口不使用 Unix 可执行权限".into());
        }
        let parent = self.native()?;
        let mut stage =
            Stage::new(&parent).map_err(|e| format!("无法创建本地服务私有暂存：{e}"))?;
        stage
            .file_mut()
            .write_all(bytes)
            .map_err(|e| format!("本地暂存写入失败，命名租约（如有）已保留：{e}"))?;
        #[cfg(unix)]
        if unsafe { libc::fchmod(stage.file_mut().as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(error("无法设置本地入口文件权限，命名租约（如有）已保留"));
        }
        let sealed = stage
            .seal(bytes.len() as u64)
            .map_err(|e| format!("本地暂存封存失败，命名租约（如有）已保留：{e}"))?;
        match sealed
            .publish_new(&parent, &name)
            .map_err(|e| format!("本地暂存发布前检查失败，命名租约（如有）已保留：{e}"))?
        {
            PublishOutcome::Published { .. } => Ok(()),
            PublishOutcome::Exists { .. } => {
                Err("本地服务目标已存在，已有文件与命名暂存租约（如有）均已保留".into())
            }
            PublishOutcome::MayHavePublished { error, .. } => Err(format!(
                "本地服务文件可能已保存，但复核或持久化失败；现存文件均已保留，请重新检查：{error}"
            )),
        }
    }
}

// The native owner explicitly unlocks before closing, including when another
// handle duplicates its open file description on Unix.
#[cfg(target_os = "linux")]
pub(crate) struct WriteLock(File);
#[cfg(target_os = "linux")]
impl Drop for WriteLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(not(target_os = "linux"))]
pub(crate) struct WriteLock(#[allow(dead_code)] Lock);
pub(crate) fn optional_snapshot(
    dir: &Dir,
    name: &str,
    max: u64,
) -> Result<Option<Snapshot>, String> {
    #[cfg(windows)]
    if windows_versions::managed(name) {
        return windows_versions::snapshot(dir, name, max);
    }
    match dir.file(name)? {
        None => Ok(None),
        Some(file) => {
            drop(file);
            Snapshot::open(dir, name, max).map(Some)
        }
    }
}

pub(crate) struct Snapshot {
    pub file: File,
    pub stamp: Stamp,
    pub digest: String,
    pub header: Vec<u8>,
}
impl Snapshot {
    pub fn open(dir: &Dir, name: &str, max: u64) -> Result<Self, String> {
        #[cfg(windows)]
        if windows_versions::managed(name) {
            return windows_versions::snapshot(dir, name, max)?
                .ok_or_else(|| "启动器本地文件文件不存在".into());
        }
        Self::physical(dir, name, max)
    }
    fn physical(dir: &Dir, name: &str, max: u64) -> Result<Self, String> {
        let mut file = dir.file(name)?.ok_or("启动器本地文件文件不存在")?;
        let stamp = Stamp::from_file(&file)?;
        if stamp.bytes == 0 || stamp.bytes > max {
            return Err("启动器本地文件文件为空或超过大小上限".into());
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut count = 0u64;
        let mut header = Vec::new();
        loop {
            let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            count = count
                .checked_add(read as u64)
                .ok_or("启动器本地文件文件过大")?;
            if count > max {
                return Err("启动器本地文件读取期间超过大小上限".into());
            }
            hash.update(&buffer[..read]);
            let keep = read.min((128 * 1024usize).saturating_sub(header.len()));
            header.extend_from_slice(&buffer[..keep]);
        }
        let snapshot = Self {
            file,
            stamp,
            digest: format!("{:x}", hash.finalize()),
            header,
        };
        if count != snapshot.stamp.bytes {
            return Err("启动器本地文件读取期间发生变化".into());
        }
        check_physical_file(&snapshot.file, &snapshot.stamp, dir, name)?;
        Ok(snapshot)
    }
    pub fn recheck(&self, dir: &Dir, name: &str) -> Result<(), String> {
        check_file(&self.file, &self.stamp, dir, name)
    }
    pub fn bytes(&mut self, dir: &Dir, name: &str) -> Result<Vec<u8>, String> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(self.stamp.bytes as usize);
        (&mut self.file)
            .take(self.stamp.bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        self.recheck(dir, name)?;
        if bytes.len() as u64 != self.stamp.bytes {
            return Err("启动器本地文件读取期间发生变化".into());
        }
        Ok(bytes)
    }
}
pub(crate) fn check_file(file: &File, stamp: &Stamp, dir: &Dir, name: &str) -> Result<(), String> {
    #[cfg(windows)]
    if windows_versions::managed(name) {
        let current =
            windows_versions::snapshot(dir, name, windows_versions::document_limit(name))?
                .ok_or("启动器本地文件文件已消失")?;
        if current.stamp != *stamp || Stamp::from_file(file)? != *stamp {
            return Err("启动器本地文件文件已变化，请重新读取".into());
        }
        return Ok(());
    }
    check_physical_file(file, stamp, dir, name)
}
fn check_physical_file(file: &File, stamp: &Stamp, dir: &Dir, name: &str) -> Result<(), String> {
    let current = dir.file(name)?.ok_or("启动器本地文件文件已消失")?;
    if Stamp::from_file(file)? != *stamp || Stamp::from_file(&current)? != *stamp {
        Err("启动器本地文件文件已变化，请刷新启动器本地文件列表".into())
    } else {
        Ok(())
    }
}
pub(crate) fn revision(path: &Path, snapshot: &Snapshot) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(path, &snapshot.stamp, &snapshot.digest))
                .map_err(|e| e.to_string())?
        )
    ))
}
pub(crate) struct Scope {
    pub path: PathBuf,
    pub root: Dir,
    pub owner: Option<Dir>,
    pub folder: Option<Dir>,
    pub folder_name: &'static str,
}
impl Scope {
    pub fn open(project: &Path, folder_name: &'static str) -> Result<Self, String> {
        let root = Dir::absolute(project)?;
        let owner = root.optional(".pcl-rust")?;
        let folder = owner
            .as_ref()
            .map(|d| d.optional(folder_name))
            .transpose()?
            .flatten();
        if let Some(folder) = &folder {
            folder.require_private()?;
        }
        Ok(Self {
            path: project.to_owned(),
            root,
            owner,
            folder,
            folder_name,
        })
    }
    pub fn path(&self) -> PathBuf {
        self.path.join(".pcl-rust").join(self.folder_name)
    }
    pub fn recheck(&self) -> Result<(), String> {
        let fresh = Self::open(&self.path, self.folder_name)?;
        if fresh.root.identity()? != self.root.identity()?
            || fresh.owner.as_ref().map(Dir::identity).transpose()?
                != self.owner.as_ref().map(Dir::identity).transpose()?
            || fresh.folder.as_ref().map(Dir::identity).transpose()?
                != self.folder.as_ref().map(Dir::identity).transpose()?
        {
            Err("启动器本地文件目录发生变化，请重新读取".into())
        } else {
            Ok(())
        }
    }
    pub fn create(project: &Path, folder_name: &'static str) -> Result<Self, String> {
        let root = Dir::absolute(project)?;
        let owner = root.create(".pcl-rust")?;
        let folder = owner.create(folder_name)?;
        folder.require_private()?;
        let scope = Self {
            path: project.to_owned(),
            root,
            owner: Some(owner),
            folder: Some(folder),
            folder_name,
        };
        scope.recheck()?;
        Ok(scope)
    }
}

#[cfg(test)]
pub(crate) fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| {
            path.join("crates/core/Cargo.toml").is_file()
                && path.join("apps/desktop/src-tauri/Cargo.toml").is_file()
        })
        .expect("test workspace root")
        .join("work/launcher-service-fixtures")
        .join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use pcl_core::retained_fs::Traversal;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = fixture_path(&format!(
                "local-fs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let parent = path.parent().unwrap();
            std::fs::create_dir_all(parent).unwrap();
            let parent = std::fs::canonicalize(parent).unwrap();
            let root = Directory::open_absolute(&parent).unwrap();
            let name = Leaf::new(path.file_name().unwrap()).unwrap();
            root.create_private(&name, Traversal::SameFilesystem)
                .unwrap();
            Self(parent.join(name.as_os_str()))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[cfg(unix)]
    #[test]
    fn exchange_retains_live_edits_to_displaced_original_and_rolls_back() {
        let f = Fixture::new();
        let dir = Dir::absolute(&f.0).unwrap();
        dir.write_new("value.json", b"original").unwrap();
        let expected = Snapshot::open(&dir, "value.json", 1024).unwrap();
        let mut editor = std::fs::OpenOptions::new()
            .write(true)
            .open(f.0.join("value.json"))
            .unwrap();
        let result = dir.replace_checked("value.json", Some(&expected), b"published", 1024, || {
            editor.set_len(0).unwrap();
            editor.write_all(b"editor-owned change").unwrap();
            editor.sync_all().unwrap();
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(f.0.join("value.json")).unwrap(),
            b"editor-owned change"
        );
        let temp = dir
            .names()
            .unwrap()
            .into_iter()
            .find(|name| name.starts_with(".local-stage-"))
            .unwrap();
        assert_eq!(std::fs::read(f.0.join(temp)).unwrap(), b"published");
    }
    #[cfg(unix)]
    #[test]
    fn exchange_retains_original_when_new_publication_is_edited() {
        let f = Fixture::new();
        let dir = Dir::absolute(&f.0).unwrap();
        dir.write_new("value.json", b"original").unwrap();
        let expected = Snapshot::open(&dir, "value.json", 1024).unwrap();
        let result = dir.replace_checked("value.json", Some(&expected), b"published", 1024, || {
            std::fs::write(f.0.join("value.json"), b"keep published edit").unwrap();
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(f.0.join("value.json")).unwrap(),
            b"keep published edit"
        );
        let temp = dir
            .names()
            .unwrap()
            .into_iter()
            .find(|name| name.starts_with(".local-stage-"))
            .unwrap();
        assert_eq!(std::fs::read(f.0.join(temp)).unwrap(), b"original");
    }
    #[test]
    fn local_lock_unlocks_explicitly_even_with_an_open_duplicate() {
        let f = Fixture::new();
        let dir = Dir::absolute(&f.0).unwrap();
        let guard = dir.lock(".lock").unwrap();
        #[cfg(target_os = "linux")]
        let duplicate = guard.0.try_clone().unwrap();
        #[cfg(not(target_os = "linux"))]
        let duplicate = guard.0.file().try_clone().unwrap();
        assert!(dir.lock(".lock").is_err());
        drop(guard);
        let _fresh = dir.lock(".lock").unwrap();
        drop(duplicate);
    }
    #[test]
    fn retained_directory_writes_and_reads_original_parent_after_rename() {
        let fixture = Fixture::new();
        let parent = Dir::absolute(&fixture.0).unwrap();
        let child = parent.create("captured").unwrap();
        let identity = child.identity().unwrap();
        std::fs::rename(fixture.0.join("captured"), fixture.0.join("held")).unwrap();
        parent.create("captured").unwrap();
        child.write_new("value.json", b"held bytes").unwrap();
        assert_eq!(child.identity().unwrap(), identity);
        assert_eq!(
            std::fs::read(fixture.0.join("held/value.json")).unwrap(),
            b"held bytes"
        );
        assert!(!fixture.0.join("captured/value.json").exists());
        let mut snapshot = Snapshot::open(&child, "value.json", 1024).unwrap();
        assert_eq!(snapshot.bytes(&child, "value.json").unwrap(), b"held bytes");
        assert!(child
            .write_new("value.json", b"must not overwrite")
            .is_err());
        assert_eq!(
            std::fs::read(fixture.0.join("held/value.json")).unwrap(),
            b"held bytes"
        );
    }
    #[test]
    fn native_move_preserves_existing_destination_and_source() {
        let fixture = Fixture::new();
        let root = Dir::absolute(&fixture.0).unwrap();
        let source = root.create("source").unwrap();
        let target = root.create("target").unwrap();
        source.write_new("entry", b"source").unwrap();
        target.write_new("entry", b"target").unwrap();
        assert!(source.move_new("entry", &target).is_err());
        assert_eq!(
            std::fs::read(fixture.0.join("source/entry")).unwrap(),
            b"source"
        );
        assert_eq!(
            std::fs::read(fixture.0.join("target/entry")).unwrap(),
            b"target"
        );
        source.write_new("fresh", b"moved").unwrap();
        source.move_new("fresh", &target).unwrap();
        assert!(source.file("fresh").unwrap().is_none());
        assert_eq!(
            std::fs::read(fixture.0.join("target/fresh")).unwrap(),
            b"moved"
        );
    }
    #[test]
    fn stamp_wire_is_native_and_unix_shape_remains_byte_compatible() {
        let fixture = Fixture::new();
        let dir = Dir::absolute(&fixture.0).unwrap();
        dir.write_new("value", b"content").unwrap();
        let file = dir.file("value").unwrap().unwrap();
        let stamp = Stamp::from_file(&file).unwrap();
        let mut value = serde_json::to_value(&stamp).unwrap();
        assert_eq!(stamp, serde_json::from_value(value.clone()).unwrap());
        #[cfg(unix)]
        {
            let old = r#"{"device":42,"inode":7,"mode":33152,"owner":1000,"links":1,"bytes":5,"modified":[1,2],"changed":[3,4]}"#;
            assert_eq!(
                serde_json::to_string(&serde_json::from_str::<Stamp>(old).unwrap()).unwrap(),
                old
            );
            assert_eq!(Stamp::of(&file.metadata().unwrap()), stamp);
            assert!(value.get("format").is_none());
        }
        #[cfg(windows)]
        {
            let Identity::Windows {
                volume,
                file: file_id,
            } = retained_fs::metadata(&file).unwrap().identity
            else {
                panic!("native identity missing")
            };
            assert_eq!(value["format"], "windows-file-id-128-v1");
            assert_eq!(value["volume"], volume);
            assert_eq!(value["file_id"], serde_json::json!(file_id));
            assert!(value.get("device").is_none() && value.get("inode").is_none());
            let mut high = stamp.clone();
            high.file_id = [255; 16];
            let mut changed = high.clone();
            changed.file_id[15] = 254;
            assert!(!high.moved_matches(&changed));
            assert!(serde_json::from_str::<Stamp>(r#"{"device":42,"inode":7,"mode":33152,"owner":1000,"links":1,"bytes":5,"modified":[1,2],"changed":[3,4]}"#).is_err());
        }
        value["unexpected"] = true.into();
        assert!(serde_json::from_value::<Stamp>(value).is_err());
    }
    #[test]
    fn unchanged_snapshot_is_required_before_initial_publication() {
        let fixture = Fixture::new();
        let dir = Dir::absolute(&fixture.0).unwrap();
        dir.replace("value", None, b"original", 1024).unwrap();
        assert!(dir.replace("value", None, b"new", 1024).is_err());
        assert_eq!(std::fs::read(fixture.0.join("value")).unwrap(), b"original");
        let snapshot = Snapshot::open(&dir, "value", 1024).unwrap();
        std::fs::write(fixture.0.join("value"), b"external edit").unwrap();
        assert!(snapshot.recheck(&dir, "value").is_err());
        assert!(dir
            .replace("value", Some(&snapshot), b"replacement", 1024)
            .is_err());
        assert_eq!(
            std::fs::read(fixture.0.join("value")).unwrap(),
            b"external edit"
        );
    }
    #[cfg(windows)]
    #[test]
    fn unsupported_existing_replacement_creates_no_stage_or_mutation() {
        let fixture = Fixture::new();
        let dir = Dir::absolute(&fixture.0).unwrap();
        dir.write_new("value", b"original").unwrap();
        let snapshot = Snapshot::open(&dir, "value", 1024).unwrap();
        let before = dir.names().unwrap();
        let error = dir
            .replace("value", Some(&snapshot), b"replacement", 1024)
            .unwrap_err();
        assert!(error.contains("尚未实现"));
        assert_eq!(before, dir.names().unwrap());
        assert_eq!(std::fs::read(fixture.0.join("value")).unwrap(), b"original");
    }
    #[cfg(unix)]
    #[test]
    fn executable_private_entry_keeps_requested_mode() {
        let fixture = Fixture::new();
        let dir = Dir::absolute(&fixture.0).unwrap();
        dir.write_new_mode("entry", b"desktop entry", 0o700)
            .unwrap();
        assert_eq!(
            std::fs::metadata(fixture.0.join("entry")).unwrap().mode() & 0o777,
            0o700
        );
    }
}
