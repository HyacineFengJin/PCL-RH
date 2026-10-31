//! Original Linux metadata storage contract, retained without path/rename changes.
use super::{FileToken, FILE_NAME, LOCK_NAME, MAX_FILE_BYTES};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, Metadata as FileMetadata},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    pub(super) fn of(metadata: &FileMetadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileStamp {
    identity: Identity,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    pub(super) fn of(metadata: &FileMetadata) -> Self {
        Self {
            identity: Identity::of(metadata),
            bytes: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    fn of_stat(stat: &libc::stat) -> Self {
        Self {
            identity: Identity {
                device: stat.st_dev,
                inode: stat.st_ino,
            },
            bytes: stat.st_size as u64,
            modified: (stat.st_mtime, stat.st_mtime_nsec),
            changed: (stat.st_ctime, stat.st_ctime_nsec),
        }
    }
}

pub(super) struct Directory(pub(super) File);

pub(super) struct WriteLock(File);

impl Drop for WriteLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn name(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "元数据文件路径含有无效字符".into())
}

fn last_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}

impl Directory {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "项目路径含有无效字符".to_string())?;
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开元数据项目目录"));
        }
        use std::os::fd::FromRawFd;
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }

    pub(super) fn identity(&self) -> Result<Identity, String> {
        self.0
            .metadata()
            .map(|metadata| Identity::of(&metadata))
            .map_err(|error| error.to_string())
    }

    pub(super) fn stat(&self, child: &str) -> Result<Option<libc::stat>, String> {
        let child = name(child)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                child.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(unsafe { stat.assume_init() }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(format!("无法检查元数据文件：{error}"))
        }
    }

    pub(super) fn storage(&self) -> Result<Option<Self>, String> {
        let Some(stat) = self.stat(".pcl-rust")? else {
            return Ok(None);
        };
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err("元数据目录不是普通目录，不能使用符号链接".into());
        }
        let child = name(".pcl-rust")?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                child.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开元数据目录"));
        }
        use std::os::fd::FromRawFd;
        let directory = Self(unsafe { File::from_raw_fd(fd) });
        if directory.identity()?
            != (Identity {
                device: stat.st_dev,
                inode: stat.st_ino,
            })
        {
            return Err("元数据目录在读取期间发生改变，请重新打开启动器".into());
        }
        Ok(Some(directory))
    }

    pub(super) fn create_storage(&self) -> Result<Self, String> {
        let child = name(".pcl-rust")?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), child.as_ptr(), 0o700) } != 0 {
            return Err(last_error("无法创建元数据目录"));
        }
        // Creation is a separate directory-only change. Failure to persist the
        // metadata file must still allow a later retry using this directory.
        let _ = self.0.sync_all();
        self.storage()?.ok_or("新元数据目录已消失".into())
    }

    pub(super) fn read_file(&self) -> Result<Option<(FileToken, Vec<u8>)>, String> {
        let Some(stat) = self.stat(FILE_NAME)? else {
            return Ok(None);
        };
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err("元数据文件不是普通文件，不能使用符号链接".into());
        }
        if stat.st_size < 0 || stat.st_size as u64 > MAX_FILE_BYTES as u64 {
            return Err("实例元数据文件超过 8 MiB 上限".into());
        }
        let file_name = name(FILE_NAME)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                file_name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开实例元数据文件"));
        }
        use std::os::fd::FromRawFd;
        let mut file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        let before = FileStamp::of(&metadata);
        if !metadata.is_file() || before != FileStamp::of_stat(&stat) {
            return Err("实例元数据文件在读取期间发生改变，请重新打开启动器".into());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err("实例元数据文件超过 8 MiB 上限".into());
        }
        let after = FileStamp::of(&file.metadata().map_err(|error| error.to_string())?);
        let named = self
            .stat(FILE_NAME)?
            .ok_or("实例元数据文件在读取期间已移除")?;
        if before != after || after != FileStamp::of_stat(&named) {
            return Err("实例元数据文件在读取期间发生改变，请重新打开启动器".into());
        }
        Ok(Some((
            FileToken {
                stamp: after,
                digest: Sha256::digest(&bytes).into(),
            },
            bytes,
        )))
    }

    pub(super) fn temporary(&self, temporary: &str) -> Result<File, String> {
        let temporary = name(temporary)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(last_error("无法创建元数据临时文件"));
        }
        use std::os::fd::FromRawFd;
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(last_error("无法设置元数据文件权限"));
        }
        Ok(file)
    }

    pub(super) fn write_lock(&self) -> Result<WriteLock, String> {
        let lock_name = name(LOCK_NAME)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                lock_name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开实例元数据写锁"));
        }
        use std::os::fd::FromRawFd;
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err("实例元数据写锁不是独立的普通文件".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("另一个启动器正在保存实例资料，请稍后刷新重试".into());
        }
        let locked = WriteLock(file);
        let stat = self.stat(LOCK_NAME)?.ok_or("实例元数据写锁已消失")?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_nlink != 1
            || FileStamp::of_stat(&stat).identity != Identity::of(&metadata)
        {
            return Err("实例元数据写锁在使用期间发生改变，请重新打开启动器".into());
        }
        Ok(locked)
    }

    pub(super) fn remove_temporary(&self, temporary: &str) {
        if let Ok(temporary) = name(temporary) {
            unsafe { libc::unlinkat(self.0.as_raw_fd(), temporary.as_ptr(), 0) };
        }
    }

    pub(super) fn replace(&self, temporary: &str) -> Result<(), String> {
        let temporary = name(temporary)?;
        let target = name(FILE_NAME)?;
        if unsafe {
            libc::renameat(
                self.0.as_raw_fd(),
                temporary.as_ptr(),
                self.0.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            return Err(last_error("无法原子保存实例元数据"));
        }
        Ok(())
    }
}
