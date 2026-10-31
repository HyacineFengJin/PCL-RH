use super::*;
use std::{
    ffi::CString,
    fs::{self, Metadata},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Component,
};
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Key {
    pub dev: u64,
    pub ino: u64,
}
impl Key {
    pub fn of(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
        }
    }
}
pub(crate) struct Dir(pub File);
fn c(name: &str) -> Result<CString> {
    CString::new(name).map_err(|_| "资源路径含无效字符".into())
}
fn error(message: &str) -> String {
    format!("{message}：{}", std::io::Error::last_os_error())
}
impl Dir {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err("资源目标必须是规范绝对路径".into());
        }
        let fd = unsafe {
            libc::open(
                c("/")?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(error("无法打开资源目标目录"));
        }
        let mut directory = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            if let Component::Normal(part) = part {
                let name = CString::new(part.as_bytes()).map_err(|e| e.to_string())?;
                let fd = unsafe {
                    libc::openat(
                        directory.0.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(error("资源目录祖先已变化或含符号链接"));
                }
                directory = Self(unsafe { File::from_raw_fd(fd) });
            }
        }
        Ok(directory)
    }
    fn open_child(&self, name: &str, flags: i32) -> Result<File> {
        provider::file_name(name)?;
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let how = OpenHow {
            flags: (flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
            mode: 0,
            resolve: 0x01 | 0x02 | 0x04 | 0x08,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            ) as i32
        };
        if fd < 0 {
            return Err(error("资源路径已变化、含链接或挂载点（需要Linux5.6以上）"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub fn child(&self, name: &str) -> Result<Self> {
        self.open_child(name, libc::O_RDONLY | libc::O_DIRECTORY)
            .map(Self)
    }
    pub fn regular(&self, name: &str) -> Result<File> {
        let file = self.open_child(name, libc::O_RDONLY | libc::O_NONBLOCK)?;
        let m = file.metadata().map_err(|e| e.to_string())?;
        if !m.is_file() || m.nlink() != 1 {
            return Err("资源必须是独立普通文件，原内容已保留".into());
        }
        Ok(file)
    }
    pub fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub fn ensure(&self, name: &str) -> Result<Self> {
        if let Some(dir) = self.optional(name)? {
            return Ok(dir);
        }
        self.create_shared(name)
    }
    pub(super) fn create_shared(&self, name: &str) -> Result<Self> {
        // Independent game-root jobs share this anonymous staging parent.
        // Another worker may create it after our missing-path read. EEXIST
        // permits only the same strict child open below; links, ordinary files
        // and mount replacements remain rejected by openat2.
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), c(name)?.as_ptr(), 0o700) } != 0 {
            let cause = std::io::Error::last_os_error();
            if cause.raw_os_error() != Some(libc::EEXIST) {
                return Err(format!("无法创建匿名网络暂存目录：{cause}"));
            }
        }
        self.0.sync_all().map_err(|e| e.to_string())?;
        self.child(name)
    }
    pub fn anonymous(&self) -> Result<super::super::source::Input> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(".")?.as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(error("文件系统不支持匿名资源暂存文件，资源目录未改动"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub fn key(&self) -> Result<Key> {
        self.0
            .metadata()
            .map(|m| Key::of(&m))
            .map_err(|e| e.to_string())
    }
    pub fn stat(&self, name: &str) -> Result<Option<libc::stat>> {
        let mut stat = std::mem::MaybeUninit::uninit();
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(unsafe { stat.assume_init() }));
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error("无法检查资源文件"))
        }
    }
    pub(crate) fn names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
            .map_err(|e| e.to_string())?
        {
            let name = entry
                .map_err(|e| e.to_string())?
                .file_name()
                .into_string()
                .map_err(|_| "资源目录含非UTF8名称")?;
            provider::file_name(&name)?;
            names.push(name);
            if names.len() > 4096 {
                return Err("资源目录条目超过安全上限".into());
            }
        }
        names.sort();
        Ok(names)
    }
}

pub(crate) fn is_regular(stat: &libc::stat) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFREG
}
pub(crate) fn other(stat: &libc::stat) -> Other {
    (stat.st_dev, stat.st_ino, stat.st_mode)
}
pub(crate) type Other = (u64, u64, u32);
impl Key {
    pub fn from_file(file: &File) -> Result<Self> {
        file.metadata()
            .map(|m| Self::of(&m))
            .map_err(|e| e.to_string())
    }
}
impl Dir {
    pub fn allocate(&self, max: u64) -> Result<super::super::source::Input> {
        if max == 0 || max > MAX_FILE_BYTES {
            return Err("资源文件大小无效".into());
        }
        self.anonymous()
    }
}

impl Dir {
    pub fn with_input_sources(&self) -> Result<Self> {
        self.0.try_clone().map(Self).map_err(|e| e.to_string())
    }
    pub fn check_cleanup(&self) -> Result<()> {
        Ok(())
    }
}
