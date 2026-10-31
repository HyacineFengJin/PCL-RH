//! Original Linux export filesystem and named no-replace publication backend.
use super::{hash_file, relative, MAX_NODES, NEXT_ID};
use serde::Serialize;
use std::{
    ffi::{CStr, CString},
    fs::{File, Metadata},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
    sync::atomic::Ordering,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Stamp {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mode: u32,
    pub(super) bytes: u64,
    pub(super) modified: (i64, i64),
    pub(super) changed: (i64, i64),
}

impl Stamp {
    pub(super) fn from_file(file: &File) -> Result<Self, String> {
        file.metadata()
            .map(|m| Self::metadata(&m))
            .map_err(|e| e.to_string())
    }
    pub(super) fn same_kind(&self, other: &Self) -> bool {
        self.mode == other.mode
    }
    fn metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            bytes: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    fn stat(stat: &libc::stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            bytes: stat.st_size.max(0) as u64,
            modified: (stat.st_mtime, stat.st_mtime_nsec),
            changed: (stat.st_ctime, stat.st_ctime_nsec),
        }
    }

    pub(super) fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFDIR
    }
    pub(super) fn is_file(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
    }
    pub(super) fn identity_key(&self) -> (u64, u64) {
        self.identity()
    }
    pub(super) fn identity(&self) -> (u64, u64) {
        (self.device, self.inode)
    }
}

pub(super) struct Dir(pub(super) File);

fn cstring(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "路径含有无效字符".into())
}

fn last_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}

impl Dir {
    pub(super) fn ancestor(&self) -> Result<Self, String> {
        pcl_core::retained_fs::Directory::from_file(self.0.try_clone().map_err(|e| e.to_string())?)
            .and_then(|d| d.parent_snapshot())
            .and_then(|d| d.try_clone_file())
            .map(Self)
            .map_err(|e| e.to_string())
    }
    pub(super) fn open_absolute(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("导出目录必须是绝对路径".into());
        }
        let slash = cstring("/")?;
        let fd = unsafe {
            libc::open(
                slash.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开导出目录"));
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let name = name.to_str().ok_or("导出目录名称必须是 UTF-8")?;
                    // Existing absolute path components may include colons;
                    // they are never used as archive paths.
                    if name.contains(['\\', '\0']) || name.chars().any(char::is_control) {
                        return Err("导出目录路径无效".into());
                    }
                    dir = dir.child(name)?;
                }
                _ => return Err("导出目录不能包含相对路径跳转".into()),
            }
        }
        Ok(dir)
    }

    pub(super) fn clone_dir(&self) -> Result<Self, String> {
        self.0
            .try_clone()
            .map(Self)
            .map_err(|error| format!("无法复制目录句柄：{error}"))
    }

    pub(super) fn child(&self, name: &str) -> Result<Self, String> {
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开目录（符号链接不允许导出）"));
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }

    pub(super) fn parent(&self, path: &str) -> Result<(Self, String), String> {
        let pieces = relative(path)?;
        let name = pieces.last().ok_or("文件路径不能为空")?.to_string();
        let mut dir = self.clone_dir()?;
        for part in &pieces[..pieces.len() - 1] {
            dir = dir.child(part)?;
        }
        Ok((dir, name))
    }

    pub(super) fn stamp(&self) -> Result<Stamp, String> {
        self.0
            .metadata()
            .map(|metadata| Stamp::metadata(&metadata))
            .map_err(|error| error.to_string())
    }

    pub(super) fn metadata(&self, name: &str) -> Result<Option<Stamp>, String> {
        let name = cstring(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(Stamp::stat(&unsafe { stat.assume_init() })));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(format!("无法读取导出文件信息：{error}"))
        }
    }

    pub(super) fn regular_file(&self, name: &str) -> Result<File, String> {
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开导出文件（符号链接不允许导出）"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("导出只允许普通文件和目录".into());
        }
        Ok(file)
    }

    pub(super) fn open_file(&self, path: &str) -> Result<File, String> {
        let (dir, name) = self.parent(path)?;
        dir.regular_file(&name)
    }

    pub(super) fn create_file(&self, name: &str) -> Result<File, String> {
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(last_error("无法创建 ZIP 暂存文件"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn names(&self) -> Result<Vec<String>, String> {
        // Use a fresh open file description: dup shares readdir's seek offset.
        let dot = cstring(".")?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(last_error("无法读取导出目录"));
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe { libc::close(fd) };
            return Err(last_error("无法读取导出目录"));
        }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        let mut names = Vec::new();
        loop {
            unsafe { *libc::__errno_location() = 0 };
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if std::io::Error::last_os_error().raw_os_error() != Some(0) {
                    return Err(last_error("读取导出目录失败"));
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| "导出文件名必须是 UTF-8")?
                .to_owned();
            names.push(name);
            if names.len() > MAX_NODES {
                return Err("导出目录包含过多文件".into());
            }
        }
        names.sort();
        Ok(names)
    }

    pub(super) fn sync(&self) -> Result<(), String> {
        self.0
            .sync_all()
            .map_err(|error| format!("无法保存导出目录：{error}"))
    }
}

struct VerifiedZip {
    file: File,
    stamp: Stamp,
    digest: String,
}

#[cfg(test)]
std::thread_local! {
    static AFTER_RENAME: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}
#[cfg(test)]
pub(super) struct RenameHook;
#[cfg(test)]
impl Drop for RenameHook {
    fn drop(&mut self) {
        AFTER_RENAME.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}
#[cfg(test)]
pub(super) fn after_rename_for_test(callback: impl FnOnce() + 'static) -> RenameHook {
    AFTER_RENAME.with(|hook| *hook.borrow_mut() = Some(Box::new(callback)));
    RenameHook
}

pub(super) struct Stage {
    pub(super) parent: Dir,
    name: String,
    identity: (u64, u64),
    armed: bool,
    verified: Option<VerifiedZip>,
}

impl Stage {
    pub(super) fn new(parent: Dir) -> Result<(Self, File), String> {
        let nonce = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let name = format!(".pcl-export-{}-{now}-{nonce}.tmp", std::process::id());
        let file = parent.create_file(&name)?;
        let identity = Stamp::from_file(&file)?.identity();
        Ok((
            Self {
                parent,
                name,
                identity,
                armed: true,
                verified: None,
            },
            file,
        ))
    }

    pub(super) fn bind_verified(
        &mut self,
        file: &File,
        bytes: u64,
        digest: &str,
    ) -> Result<(), String> {
        let mut held = file.try_clone().map_err(|error| error.to_string())?;
        let stamp = Stamp::from_file(&held)?;
        if stamp.identity() != self.identity
            || !stamp.is_file()
            || stamp.bytes != bytes
            || hash_file(&mut held, None)? != digest
            || Stamp::from_file(&held)? != stamp
            || self.parent.metadata(&self.name)? != Some(stamp.clone())
        {
            return Err("ZIP 暂存文件在绑定完整性证据期间发生变化".into());
        }
        self.verified = Some(VerifiedZip {
            file: held,
            stamp,
            digest: digest.into(),
        });
        Ok(())
    }
    fn recheck_zip(&mut self, name: &str, moved: bool) -> Result<(), String> {
        let verified = self.verified.as_mut().ok_or("ZIP 缺少已验证的完整性证据")?;
        let before = Stamp::from_file(&verified.file)?;
        let matches = if moved {
            before.identity() == verified.stamp.identity()
                && before.mode == verified.stamp.mode
                && before.bytes == verified.stamp.bytes
                && before.modified == verified.stamp.modified
        } else {
            before == verified.stamp
        };
        if !matches
            || !before.is_file()
            || hash_file(&mut verified.file, None)? != verified.digest
            || Stamp::from_file(&verified.file)? != before
            || self.parent.metadata(name)? != Some(before)
        {
            return Err("ZIP 暂存或发布文件的身份、时间或 SHA-256 已变化".into());
        }
        Ok(())
    }

    pub(super) fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        let Some(stamp) = self.parent.metadata(&self.name)? else {
            self.armed = false;
            return Ok(());
        };
        if stamp.identity() != self.identity {
            return Err("ZIP 暂存文件被外部替换，请检查目标目录".into());
        }
        let name = cstring(&self.name)?;
        if unsafe { libc::unlinkat(self.parent.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(last_error("无法清理 ZIP 暂存文件"));
        }
        self.armed = false;
        self.parent.sync()
    }

    pub(super) fn publish(&mut self, destination: &str) -> Result<(), String> {
        self.recheck_zip(&self.name.clone(), false)?;
        let source = cstring(&self.name)?;
        let target = cstring(destination)?;
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.parent.0.as_raw_fd(),
                source.as_ptr(),
                self.parent.0.as_raw_fd(),
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                return Err("目标文件已经存在，请使用新的文件名".into());
            }
            return Err(format!(
                "无法原子发布 ZIP 文件（目标文件系统须支持不覆盖重命名）：{error}"
            ));
        }
        // From this point no error authorizes removal of the chosen target.
        self.armed = false;
        #[cfg(test)]
        if let Some(callback) = AFTER_RENAME.with(|hook| hook.borrow_mut().take()) {
            callback();
        }
        self.recheck_zip(destination, true).map_err(|error| format!("MayHavePublished：ZIP 已发布但完整性复核未确认，目标已保留，请检查后再操作：{error}"))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
