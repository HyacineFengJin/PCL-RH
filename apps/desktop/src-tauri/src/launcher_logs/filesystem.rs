//! Small FD-relative primitives for this service. Every opened component is a
//! real directory, and output is published from an anonymous file without
//! overwriting a caller's file. Source names never become arbitrary paths.
use super::{MAX_ENTRIES, MAX_FILE_BYTES};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stamp {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub owner: u32,
    pub links: u64,
    pub bytes: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
}

impl Stamp {
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
    pub fn identity(&self) -> (u64, u64) {
        (self.device, self.inode)
    }
    pub fn regular(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
    }
    pub fn owned(&self) -> bool {
        self.owner == unsafe { libc::geteuid() } && self.links == 1
    }
    /// rename changes ctime; inode, original bytes and mtime must survive it.
    pub fn after_move_matches(&self, other: &Self) -> bool {
        self.identity() == other.identity()
            && self.mode == other.mode
            && self.owner == other.owner
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
        Ok(())
    }
}

fn cstring(name: &str) -> Result<CString, String> {
    component(name)?;
    CString::new(name).map_err(|_| "日志文件名无效".into())
}

fn error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}

pub(super) struct Dir(pub File);
impl Dir {
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
    pub fn new_private_child(&self, name: &str) -> Result<Self, String> {
        let c = cstring(name)?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
            return Err(error("无法创建新的日志恢复记录"));
        }
        self.sync()?;
        self.child(name)
    }
    pub fn stamp(&self) -> Result<Stamp, String> {
        self.0
            .metadata()
            .map(|m| Stamp::of(&m))
            .map_err(|e| e.to_string())
    }
    /// Observe an entry without opening or following it. Failed diagnostics
    /// also bind absent/invalid carriers so replacing one cannot publish an
    /// error that described a different directory or file.
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
    pub fn verify_private(&self) -> Result<(), String> {
        let stamp = self.stamp()?;
        if stamp.owner != unsafe { libc::geteuid() } || stamp.mode & 0o077 != 0 {
            Err("日志恢复目录必须由当前用户独占，请检查目录权限".into())
        } else {
            Ok(())
        }
    }
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
        let stamp = Stamp::of(&file.metadata().map_err(|e| e.to_string())?);
        if !stamp.regular() || !stamp.owned() {
            return Err("只允许当前用户独占的普通日志文件".into());
        }
        Ok(Some(file))
    }
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
    pub fn sync(&self) -> Result<(), String> {
        self.0
            .sync_all()
            .map_err(|e| format!("无法持久保存日志目录：{e}"))
    }
    pub fn anonymous(&self) -> Result<File, String> {
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
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub fn publish(&self, file: &File, name: &str) -> Result<(), String> {
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
        let mut file = self.anonymous()?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        self.publish(&file, name)
    }
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
        let stamp = Stamp::of(&file.metadata().map_err(|e| e.to_string())?);
        if !stamp.regular() || !stamp.owned() || stamp.mode & 0o077 != 0 {
            return Err("日志恢复锁不安全".into());
        }
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("另一个日志清理或恢复任务正在运行".into());
        }
        Ok(Lock(file))
    }
}

pub(super) struct Lock(File);
#[cfg(test)]
impl Lock {
    pub(super) fn duplicate_for_test(&self) -> File {
        self.0.try_clone().unwrap()
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
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
    let before = Stamp::of(&file.metadata().map_err(|e| e.to_string())?);
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
        || before != Stamp::of(&file.metadata().map_err(|e| e.to_string())?)
        || dir.file(name)?.is_none_or(|fresh| {
            fresh.metadata().ok().map(|m| Stamp::of(&m)) != Some(before.clone())
        })
    {
        return Err(format!("日志读取期间发生变化，请重试：{name}"));
    }
    let digest = format!("{:x}", Sha256::digest(&data));
    Ok(Some((before, digest, data)))
}
