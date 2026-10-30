//! Small FD-relative primitives for this service. Every opened component is a
//! real directory; native staging publishes new bytes without
//! overwriting a caller's file. Source names never become arbitrary paths.
use super::{MAX_ENTRIES, MAX_FILE_BYTES};
#[cfg(not(target_os = "linux"))]
use pcl_core::retained_fs::stage::{PublishOutcome, Stage};
#[cfg(windows)]
use pcl_core::retained_fs::Kind;
use pcl_core::retained_fs::Leaf;
#[cfg(not(target_os = "linux"))]
use pcl_core::retained_fs::{self as native_fs, Directory, Identity, Links, Traversal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::{
    ffi::{CStr, CString},
    os::fd::{AsRawFd, FromRawFd},
    path::Component,
};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
#[cfg(unix)]
use std::{fs::Metadata, os::unix::fs::MetadataExt};

#[cfg(unix)]
pub(super) type DirectoryIdentity = (u64, u64);
#[cfg(windows)]
pub(super) type DirectoryIdentity = Identity;
#[cfg(unix)]
pub(super) type NodeKind = u32;
#[cfg(windows)]
pub(super) type NodeKind = Kind;
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum WindowsFormat {
    #[serde(rename = "windows-file-id-128-v1")]
    FileId128V1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stamp {
    #[cfg(unix)]
    pub device: u64,
    #[cfg(unix)]
    pub inode: u64,
    #[cfg(unix)]
    pub mode: u32,
    #[cfg(unix)]
    pub owner: u32,
    #[cfg(windows)]
    format: WindowsFormat,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
    #[cfg(windows)]
    kind: Kind,
    #[cfg(windows)]
    security: native_fs::Security,
    #[cfg(windows)]
    reparse_tag: Option<u32>,
    pub links: u64,
    pub bytes: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
}
impl Stamp {
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
            Self::from_native(native_fs::metadata(file).map_err(|e| e.to_string())?)
        }
    }
    #[cfg(not(target_os = "linux"))]
    fn from_native(metadata: native_fs::Metadata) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let Identity::Unix { device, inode } = metadata.identity else {
                return Err("日志身份不属于当前平台".into());
            };
            let native_fs::Security::Unix { owner, mode, .. } = metadata.security else {
                return Err("日志安全信息不属于当前平台".into());
            };
            Ok(Self {
                device,
                inode,
                mode,
                owner,
                links: metadata.links,
                bytes: metadata.len,
                modified: (
                    metadata.modified.seconds,
                    i64::from(metadata.modified.nanos),
                ),
                changed: (metadata.changed.seconds, i64::from(metadata.changed.nanos)),
            })
        }
        #[cfg(windows)]
        {
            let Identity::Windows { volume, file } = metadata.identity else {
                return Err("日志身份不属于当前平台".into());
            };
            Ok(Self {
                format: WindowsFormat::FileId128V1,
                volume,
                file_id: file,
                kind: metadata.kind,
                security: metadata.security,
                reparse_tag: metadata.reparse_tag,
                links: metadata.links,
                bytes: metadata.len,
                modified: (
                    metadata.modified.seconds,
                    i64::from(metadata.modified.nanos),
                ),
                changed: (metadata.changed.seconds, i64::from(metadata.changed.nanos)),
            })
        }
    }
    pub fn identity(&self) -> DirectoryIdentity {
        #[cfg(unix)]
        {
            (self.device, self.inode)
        }
        #[cfg(windows)]
        {
            Identity::Windows {
                volume: self.volume,
                file: self.file_id,
            }
        }
    }
    pub fn type_key(&self) -> NodeKind {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32
        }
        #[cfg(windows)]
        {
            self.kind
        }
    }
    pub fn regular(&self) -> bool {
        #[cfg(unix)]
        {
            self.type_key() == libc::S_IFREG as u32
        }
        #[cfg(windows)]
        {
            self.kind == Kind::Regular && self.reparse_tag.is_none()
        }
    }
    pub fn owned(&self) -> bool {
        #[cfg(unix)]
        {
            self.owner == unsafe { libc::geteuid() } && self.links == 1
        }
        #[cfg(windows)]
        {
            matches!(
                self.security,
                native_fs::Security::Windows {
                    owner_is_current: true,
                    ..
                }
            ) && self.links == 1
                && self.reparse_tag.is_none()
        }
    }
    /// Rename changes ctime; identity, original bytes and mtime must survive it.
    pub fn after_move_matches(&self, other: &Self) -> bool {
        #[cfg(unix)]
        let security = self.mode == other.mode && self.owner == other.owner;
        #[cfg(windows)]
        let security = self.kind == other.kind
            && self.security == other.security
            && self.reparse_tag == other.reparse_tag;
        self.identity() == other.identity()
            && security
            && self.links == other.links
            && self.bytes == other.bytes
            && self.modified == other.modified
    }
}

pub(super) fn component(name: &str) -> Result<(), String> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.len() > 255
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        Err("日志文件名无效".into())
    } else {
        Leaf::new(name)
            .map(|_| ())
            .map_err(|_| "日志文件名无效".into())
    }
}

#[cfg(target_os = "linux")]
fn cstring(name: &str) -> Result<CString, String> {
    component(name)?;
    CString::new(name).map_err(|_| "日志文件名无效".into())
}

#[cfg(target_os = "linux")]
fn error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}

pub(super) struct Dir(pub File);
impl Dir {
    #[cfg(target_os = "linux")]
    pub fn absolute(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("日志目录必须是绝对路径".into());
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
                Component::Normal(part) => {
                    dir = dir.child(part.to_str().ok_or("日志目录名称必须是 UTF-8")?)?
                }
                _ => return Err("日志目录不能包含相对路径跳转".into()),
            }
        }
        Ok(dir)
    }
    pub fn child(&self, name: &str) -> Result<Self, String> {
        self.optional_child(name)?
            .ok_or_else(|| format!("日志目录不存在：{name}"))
    }
    #[cfg(target_os = "linux")]
    pub fn optional_child(&self, name: &str) -> Result<Option<Self>, String> {
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd >= 0 {
            return Ok(Some(Self(unsafe { File::from_raw_fd(fd) })));
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error("无法打开日志目录（不允许符号链接）"))
        }
    }
    #[cfg(target_os = "linux")]
    pub fn private_child(&self, name: &str) -> Result<Self, String> {
        let c = cstring(name)?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), c.as_ptr(), 0o700) } != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
        {
            return Err(error("无法创建日志恢复目录"));
        }
        let dir = self.child(name)?;
        dir.verify_private()?;
        self.sync()?;
        Ok(dir)
    }
    #[cfg(target_os = "linux")]
    pub fn new_private_child(&self, name: &str) -> Result<Self, String> {
        let c = cstring(name)?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
            return Err(error("无法创建新的日志恢复记录"));
        }
        self.sync()?;
        self.child(name)
    }
    pub fn stamp(&self) -> Result<Stamp, String> {
        Stamp::from_file(&self.0)
    }
    /// Observe an entry without opening or following it. Failed diagnostics
    /// also bind absent/invalid carriers so replacing one cannot publish an
    /// error that described a different directory or file.
    #[cfg(target_os = "linux")]
    pub fn entry_stamp(&self, name: &str) -> Result<Option<Stamp>, String> {
        let c = cstring(name)?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                c.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error("无法检查日志入口"))
            };
        }
        Ok(Some(Stamp {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            owner: stat.st_uid,
            links: stat.st_nlink,
            bytes: stat.st_size.max(0) as u64,
            modified: (stat.st_mtime, stat.st_mtime_nsec),
            changed: (stat.st_ctime, stat.st_ctime_nsec),
        }))
    }
    #[cfg(target_os = "linux")]
    pub fn verify_private(&self) -> Result<(), String> {
        let stamp = self.stamp()?;
        if stamp.owner != unsafe { libc::geteuid() } || stamp.mode & 0o077 != 0 {
            Err("日志恢复目录必须由当前用户独占，请检查目录权限".into())
        } else {
            Ok(())
        }
    }
    #[cfg(target_os = "linux")]
    pub fn file(&self, name: &str) -> Result<Option<File>, String> {
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
                Err(error("无法读取日志（不允许符号链接）"))
            };
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let stamp = Stamp::from_file(&file)?;
        if !stamp.regular() || !stamp.owned() {
            return Err("只允许当前用户独占的普通日志文件".into());
        }
        Ok(Some(file))
    }
    #[cfg(target_os = "linux")]
    pub fn names(&self) -> Result<Vec<String>, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(error("无法扫描日志目录"));
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(error("无法扫描日志目录"));
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
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if std::io::Error::last_os_error().raw_os_error() != Some(0) {
                    return Err(error("扫描日志失败"));
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            names.push(
                std::str::from_utf8(bytes)
                    .map_err(|_| "日志目录文件名必须是 UTF-8")?
                    .to_owned(),
            );
            if names.len() > MAX_ENTRIES {
                return Err("日志目录的文件数量超过安全上限".into());
            }
        }
        names.sort();
        Ok(names)
    }
    #[cfg(target_os = "linux")]
    pub fn sync(&self) -> Result<(), String> {
        self.0
            .sync_all()
            .map_err(|e| format!("无法持久保存日志目录：{e}"))
    }
    #[cfg(target_os = "linux")]
    pub fn temporary(&self) -> Result<Temporary, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(error("目标文件系统不支持匿名日志暂存文件"));
        }
        Ok(Temporary {
            file: unsafe { File::from_raw_fd(fd) },
        })
    }
    #[cfg(target_os = "linux")]
    pub fn publish(&self, output: Temporary, name: &str) -> Result<(), String> {
        let file = &output.file;
        let c = cstring(name)?;
        file.sync_all().map_err(|e| e.to_string())?;
        if unsafe {
            libc::linkat(
                file.as_raw_fd(),
                c"".as_ptr(),
                self.0.as_raw_fd(),
                c.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        } != 0
        {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
                return Err("目标文件已经存在，请选择新的文件名".into());
            }
            return Err(error("无法原子保存日志文件"));
        }
        self.sync()
            .map_err(|e| format!("日志文件已保存，但目录同步失败；请保留目标文件：{e}"))
    }
    pub fn write_new(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        component(name)?;
        // Windows security descriptors make a captured entry variable-sized.
        // Never publish a recovery journal that its bounded reader cannot
        // reopen; clear has not moved any original before this write succeeds.
        if bytes.len() as u64 > super::MAX_JOURNAL_BYTES {
            return Err("日志恢复记录超过读取上限，原日志尚未移动".into());
        }
        let mut output = self.temporary()?;
        if let Err(error) = output.file_mut().write_all(bytes) {
            return Err(output.failure(error.to_string()));
        }
        self.publish(output, name)
    }
    #[cfg(target_os = "linux")]
    pub fn move_new(&self, name: &str, destination: &Dir) -> Result<(), String> {
        let c = cstring(name)?;
        if unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.0.as_raw_fd(),
                c.as_ptr(),
                destination.0.as_raw_fd(),
                c.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(error("无法移动日志（原位置或恢复位置发生冲突）"));
        }
        self.sync()?;
        destination.sync()
    }
    #[cfg(target_os = "linux")]
    pub fn lock(&self) -> Result<Lock, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c"lock".as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(error("无法打开日志恢复锁"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let stamp = Stamp::from_file(&file)?;
        if !stamp.regular() || !stamp.owned() || stamp.mode & 0o077 != 0 {
            return Err("日志恢复锁不安全".into());
        }
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("另一个日志清理或恢复任务正在运行".into());
        }
        Ok(Lock { file })
    }
}

pub(super) struct Lock {
    #[cfg(target_os = "linux")]
    file: File,
    #[cfg(not(target_os = "linux"))]
    native: native_fs::Lock,
}
#[cfg(test)]
impl Lock {
    pub(super) fn duplicate_for_test(&self) -> File {
        #[cfg(target_os = "linux")]
        {
            self.file.try_clone().unwrap()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.native.file().try_clone().unwrap()
        }
    }
}
#[cfg(target_os = "linux")]
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(super) fn snapshot(
    dir: &Dir,
    name: &str,
    limit: u64,
) -> Result<Option<(Stamp, String, Vec<u8>)>, String> {
    let Some(mut file) = dir.file(name)? else {
        return Ok(None);
    };
    let before = Stamp::from_file(&file)?;
    if before.bytes > limit.min(MAX_FILE_BYTES) {
        return Err(format!("日志文件过大：{name}"));
    }
    let mut data = Vec::new();
    (&mut file)
        .take(limit.min(MAX_FILE_BYTES) + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > limit
        || data.len() as u64 != before.bytes
        || before != Stamp::from_file(&file)?
        || dir
            .file(name)?
            .is_none_or(|fresh| Stamp::from_file(&fresh).ok() != Some(before.clone()))
    {
        return Err(format!("日志读取期间发生变化，请重试：{name}"));
    }
    let digest = format!("{:x}", Sha256::digest(&data));
    Ok(Some((before, digest, data)))
}

/// Own the actual native staging lease until it is sealed and published. Drop
/// never removes named evidence. Callers append `failure` on precommit errors.
pub(super) struct Temporary {
    #[cfg(target_os = "linux")]
    file: File,
    #[cfg(not(target_os = "linux"))]
    stage: Stage,
    #[cfg(not(target_os = "linux"))]
    source: Directory,
    #[cfg(not(target_os = "linux"))]
    workspace: Option<(Directory, Leaf)>,
}
impl Temporary {
    pub fn file_mut(&mut self) -> &mut File {
        #[cfg(target_os = "linux")]
        {
            &mut self.file
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.stage.file_mut()
        }
    }
    #[cfg(not(target_os = "linux"))]
    fn location(&self) -> String {
        let source = self
            .stage
            .source_name()
            .map(|name| name.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_else(|| "未命名暂存".into());
        match &self.workspace {
            Some((_, workspace)) => format!("{}/{source}", workspace.as_os_str().to_string_lossy()),
            None => source,
        }
    }
    pub fn failure(self, original: String) -> String {
        #[cfg(target_os = "linux")]
        {
            original
        }
        #[cfg(not(target_os = "linux"))]
        {
            format!(
                "{original}；命名日志暂存已保留，请检查目标目录中的 {}",
                self.location()
            )
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl Dir {
    fn native(&self) -> Result<Directory, String> {
        Directory::from_file(self.0.try_clone().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
    pub fn absolute(path: &Path) -> Result<Self, String> {
        Directory::open_absolute(path)
            .and_then(|directory| directory.try_clone_file())
            .map(Self)
            .map_err(|e| format!("无法打开原生日志目录：{e}"))
    }
    pub fn optional_child(&self, name: &str) -> Result<Option<Self>, String> {
        component(name)?;
        match self.native()?.child(
            &Leaf::new(name).map_err(|e| e.to_string())?,
            Traversal::AllowMounts,
        ) {
            Ok(directory) => directory
                .try_clone_file()
                .map(Self)
                .map(Some)
                .map_err(|e| e.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("无法打开日志目录（不允许链接）：{error}")),
        }
    }
    pub fn private_child(&self, name: &str) -> Result<Self, String> {
        component(name)?;
        let native = self.native()?;
        let leaf = Leaf::new(name).map_err(|e| e.to_string())?;
        let directory = match native.create_private(&leaf, Traversal::SameFilesystem) {
            Ok(directory) => Self(directory.try_clone_file().map_err(|e| e.to_string())?),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => self.child(name)?,
            Err(error) => return Err(format!("无法创建日志恢复目录：{error}")),
        };
        directory.verify_private()?;
        self.sync()?;
        Ok(directory)
    }
    pub fn new_private_child(&self, name: &str) -> Result<Self, String> {
        component(name)?;
        let native = self.native()?;
        let directory = native
            .create_private(
                &Leaf::new(name).map_err(|e| e.to_string())?,
                Traversal::SameFilesystem,
            )
            .map_err(|e| format!("无法创建新的日志恢复记录：{e}"))?;
        self.sync()?;
        Ok(Self(directory.try_clone_file().map_err(|e| e.to_string())?))
    }
    pub fn entry_stamp(&self, name: &str) -> Result<Option<Stamp>, String> {
        component(name)?;
        self.native()?
            .entry(&Leaf::new(name).map_err(|e| e.to_string())?)
            .map_err(|e| format!("无法检查日志入口：{e}"))?
            .map(Stamp::from_native)
            .transpose()
    }
    pub fn verify_private(&self) -> Result<(), String> {
        self.native()?
            .metadata()
            .and_then(|metadata| metadata.require_private())
            .map_err(|e| format!("日志恢复目录必须由当前用户独占：{e}"))
    }
    pub fn file(&self, name: &str) -> Result<Option<File>, String> {
        component(name)?;
        let file = match self
            .native()?
            .open_regular(&Leaf::new(name).map_err(|e| e.to_string())?, Links::Single)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("无法读取日志（不允许链接或特殊文件）：{error}")),
        };
        let metadata = native_fs::metadata(&file).map_err(|e| e.to_string())?;
        metadata
            .require_owned()
            .map_err(|e| format!("只允许当前用户独占的普通日志文件：{e}"))?;
        Ok(Some(file))
    }
    pub fn names(&self) -> Result<Vec<String>, String> {
        let native = self
            .native()?
            .names(MAX_ENTRIES + 1)
            .map_err(|e| format!("无法受限扫描日志目录：{e}"))?;
        if native.len() > MAX_ENTRIES {
            return Err("日志目录的文件数量超过安全上限".into());
        }
        native
            .into_iter()
            .map(|name| {
                name.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "日志目录文件名必须是 UTF-8".into())
            })
            .collect()
    }
    pub fn sync(&self) -> Result<(), String> {
        self.native()?
            .sync()
            .map_err(|e| format!("无法持久保存日志目录：{e}"))
    }
    pub fn temporary(&self) -> Result<Temporary, String> {
        let parent = self.native()?;
        // Named sources require a private parent, while an explicitly chosen
        // export destination may be an ordinary shared/0755 directory. A new
        // random private workspace provides the source lease without chmod.
        let (source, workspace) = if parent
            .metadata()
            .map_err(|e| e.to_string())?
            .require_private()
            .is_ok()
        {
            (parent.clone(), None)
        } else {
            let (name, directory) = parent
                .create_private_workspace()
                .map_err(|e| e.to_string())?;
            parent.sync().map_err(|e| {
                format!(
                    "日志暂存目录已创建但同步失败，保留 {}：{e}",
                    name.as_os_str().to_string_lossy()
                )
            })?;
            (directory, Some((parent, name)))
        };
        let stage = Stage::new(&source).map_err(|e| {
            let where_ = workspace
                .as_ref()
                .map(|(_, name)| name.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_else(|| "当前私有目录".into());
            format!("无法创建命名日志暂存，已保留目录 {where_}：{e}")
        })?;
        let output = Temporary {
            stage,
            source,
            workspace,
        };
        if let Err(error) = output.source.sync() {
            return Err(output.failure(format!("命名日志暂存已创建但同步失败：{error}")));
        }
        Ok(output)
    }
    pub fn publish(&self, output: Temporary, name: &str) -> Result<(), String> {
        let location = output.location();
        component(name).map_err(|error| format!("{error}；命名日志暂存已保留：{location}"))?;
        let target = self
            .native()
            .map_err(|error| format!("{error}；命名日志暂存已保留：{location}"))?;
        let Temporary {
            stage,
            source,
            workspace,
        } = output;
        let sealed = stage
            .seal(super::MAX_TOTAL_BYTES + super::MAX_JOURNAL_BYTES)
            .map_err(|failed| format!("无法封存日志暂存，已保留 {location}：{}", failed.error))?;
        match sealed.publish_new(&target, &Leaf::new(name).map_err(|e| e.to_string())?) {
            Ok(PublishOutcome::Published { .. }) => {
                if let Some((parent, workspace)) = workspace {
                    let metadata = source.metadata().map_err(|e| {
                        format!("日志已保存，暂存目录核验失败，已保留 {location}：{e}")
                    })?;
                    match parent.remove_workspace_entry(&workspace, &metadata) {
                        Ok(native_fs::WorkspaceRemoval::Removed) => {}
                        Ok(native_fs::WorkspaceRemoval::MayHaveRemoved { error }) => {
                            return Err(format!(
                                "日志已保存，暂存目录清理结果不确定，已保留现存条目：{error}"
                            ))
                        }
                        Err(error) => {
                            return Err(format!(
                                "日志已保存，暂存目录保留待检查 {location}：{error}"
                            ))
                        }
                    }
                }
                Ok(())
            }
            Ok(PublishOutcome::Exists { .. }) => Err(format!(
                "目标文件已经存在，请选择新的文件名；命名日志暂存已保留：{location}"
            )),
            Ok(PublishOutcome::MayHavePublished { receipt, error, .. }) => Err(format!(
                "日志可能已保存，禁止盲目重试；暂存 {location}；发布证据 {}：{error}",
                serde_json::to_string(&receipt).map_err(|e| e.to_string())?
            )),
            Err(failed) => Err(format!(
                "无法发布日志，目标可能已保存；暂存 {location} 已保留：{}",
                failed.error
            )),
        }
    }
    pub fn move_new(&self, name: &str, destination: &Dir) -> Result<(), String> {
        component(name)?;
        pcl_core::file_ops::rename_noreplace_at(
            &self.0,
            std::ffi::OsStr::new(name),
            &destination.0,
            std::ffi::OsStr::new(name),
        )
        .map_err(|e| format!("日志移动失败或结果待核实，已保留现存文件：{e}"))?;
        self.sync()?;
        destination.sync()
    }
    pub fn lock(&self) -> Result<Lock, String> {
        let native = self
            .native()?
            .lock(&Leaf::new("lock").map_err(|e| e.to_string())?)
            .map_err(|e| format!("无法取得原生日志恢复锁：{e}"))?;
        Ok(Lock { native })
    }
}
