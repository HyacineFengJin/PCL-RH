//! Original Linux filesystem and anonymous publication primitives.
use super::*;
use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Component,
};
pub(super) struct Dir(pub(super) File);
fn io_error(context: &str) -> String {
    format!("{context}: {}", std::io::Error::last_os_error())
}
fn c(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(error)
}
pub(super) fn identity(file: &File) -> Result<Identity, String> {
    let m = file.metadata().map_err(error)?;
    Ok(Identity {
        device: m.dev(),
        inode: m.ino(),
    })
}

impl Dir {
    pub(super) fn absolute(path: &Path, create: bool) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("Harness requires an absolute native folder".into());
        }
        let slash = c("/")?;
        let fd = unsafe {
            libc::open(
                slash.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io_error("Cannot open native root"));
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let name = CString::new(name.as_bytes()).map_err(error)?;
                    if create
                        && unsafe { libc::mkdirat(dir.0.as_raw_fd(), name.as_ptr(), 0o700) } != 0
                        && std::io::Error::last_os_error().kind()
                            != std::io::ErrorKind::AlreadyExists
                    {
                        return Err(io_error("Cannot create native store directory"));
                    }
                    let fd = unsafe {
                        libc::openat(
                            dir.0.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                        )
                    };
                    if fd < 0 {
                        return Err(io_error("Native folder contains a link or changed"));
                    }
                    dir = Self(unsafe { File::from_raw_fd(fd) });
                }
                _ => return Err("Native folder contains relative path components".into()),
            }
        }
        Ok(dir)
    }
    pub(super) fn key(&self) -> Result<Identity, String> {
        identity(&self.0)
    }
    pub(super) fn sync(&self) -> Result<(), String> {
        self.0.sync_all().map_err(error)
    }
    pub(super) fn open(&self, path: &str, directory: bool) -> Result<File, String> {
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let name = c(if path.is_empty() { "." } else { path })?;
        let how = OpenHow {
            flags: (libc::O_RDONLY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | if directory {
                    libc::O_DIRECTORY
                } else {
                    libc::O_NONBLOCK
                }) as u64,
            mode: 0,
            // NO_XDEV | NO_MAGICLINKS | NO_SYMLINKS | BENEATH.
            resolve: 0x01 | 0x02 | 0x04 | 0x08,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.0.as_raw_fd(),
                name.as_ptr(),
                &how,
                std::mem::size_of::<OpenHow>(),
            ) as i32
        };
        if fd < 0 {
            return Err(io_error(
                "Harness path changed, contains a link, or crosses a mount (Linux 5.6+ required)",
            ));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(super) fn child(&self, path: &str) -> Result<Self, String> {
        self.open(path, true).map(Self)
    }
    pub(super) fn stat(&self, name: &str) -> Result<Option<libc::stat>, String> {
        let name = c(name)?;
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
            return Ok(Some(unsafe { stat.assume_init() }));
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(io_error("Cannot inspect Harness path"))
        }
    }
    pub(super) fn optional_child(&self, name: &str) -> Result<Option<Self>, String> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(super) fn ensure_child(&self, name: &str) -> Result<Self, String> {
        let name_c = c(name)?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(io_error("Cannot create Harness store directory"));
        }
        self.sync()?;
        self.child(name)
    }
    pub(super) fn names(&self) -> Result<Vec<String>, String> {
        let mut names = vec![];
        // Enumeration uses the pinned FD, not a path supplied by the renderer.
        for entry in fs::read_dir(format!("/proc/self/fd/{}", self.0.as_raw_fd())).map_err(error)? {
            let name = entry
                .map_err(error)?
                .file_name()
                .into_string()
                .map_err(|_| "Harness tree contains a non-UTF-8 filename")?;
            names.push(name);
            if names.len() > MAX_ENTRIES {
                return Err("Harness directory exceeds its entry budget".into());
            }
        }
        names.sort();
        Ok(names)
    }
    pub(super) fn read(&self, name: &str, limit: usize) -> Result<(FileState, Vec<u8>), String> {
        let mut file = self.open(name, false)?;
        let before = file.metadata().map_err(error)?;
        if !before.is_file() || before.nlink() != 1 || before.len() > limit as u64 {
            return Err("Harness requires a bounded regular file with one link".into());
        }
        let mut bytes = vec![];
        (&mut file)
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        let after = file.metadata().map_err(error)?;
        if bytes.len() > limit
            || before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || after.nlink() != 1
        {
            return Err("Harness file changed while it was read".into());
        }
        Ok((
            FileState {
                identity: identity(&file)?,
                sha256: hash(&bytes),
                size: bytes.len(),
                mode: after.mode() & 0o777,
            },
            bytes,
        ))
    }
    pub(super) fn optional_file(&self, name: &str) -> Result<Option<FileState>, String> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.read(name, MAX_FILE).map(|v| Some(v.0))
        }
    }
    pub(super) fn anonymous(&self, bytes: &[u8], mode: u32) -> Result<File, String> {
        let dot = c(".")?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                dot.as_ptr(),
                libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io_error(
                "Harness staging needs Linux anonymous-file support",
            ));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(error)?;
        if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(io_error("Cannot preserve source permissions"));
        }
        file.sync_all().map_err(error)?;
        Ok(file)
    }
    pub(super) fn link_anonymous(&self, file: &File, name: &str) -> Result<(), String> {
        let empty = c("")?;
        let name = c(name)?;
        if unsafe {
            libc::linkat(
                file.as_raw_fd(),
                empty.as_ptr(),
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        } != 0
        {
            return Err(io_error("Harness staging slot already exists or changed"));
        }
        self.sync()
    }
    pub(super) fn immutable(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if self.stat(name)?.is_some() {
            if self.read(name, MAX_RECORD.max(MAX_FILE))?.1 == bytes {
                return Ok(());
            }
            return Err("Immutable Harness record differs; retained for recovery".into());
        }
        let file = self.anonymous(bytes, 0o600)?;
        self.link_anonymous(&file, name)
    }
    pub(super) fn replace_record(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_RECORD {
            return Err("Harness journal exceeds its limit".into());
        }
        if self.stat(name)?.is_some() {
            self.read(name, MAX_RECORD)?;
        }
        let temporary = format!(".journal-{}", id());
        let file = self.anonymous(bytes, 0o600)?;
        self.link_anonymous(&file, &temporary)?;
        let temporary_c = c(&temporary)?;
        let name_c = c(name)?;
        if unsafe {
            libc::renameat(
                self.0.as_raw_fd(),
                temporary_c.as_ptr(),
                self.0.as_raw_fd(),
                name_c.as_ptr(),
            )
        } != 0
        {
            return Err(io_error("Cannot save Harness journal"));
        }
        self.sync()
    }
    pub(super) fn rename(&self, from: &str, to: &str, flags: u32) -> Result<(), String> {
        let from = c(from)?;
        let to = c(to)?;
        if unsafe {
            libc::renameat2(
                self.0.as_raw_fd(),
                from.as_ptr(),
                self.0.as_raw_fd(),
                to.as_ptr(),
                flags,
            )
        } != 0
        {
            return Err(io_error(
                "Harness publication conflict; retained transaction",
            ));
        }
        self.sync()
    }
    pub(super) fn unlink(&self, name: &str, expected: &FileState) -> Result<(), String> {
        if self.optional_file(name)?.as_ref() != Some(expected) {
            return Err("Harness temporary file changed; retained for recovery".into());
        }
        let name = c(name)?;
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io_error("Cannot remove owned Harness temporary"));
        }
        self.sync()
    }
}

pub(super) fn lock(store: &Dir) -> Result<File, String> {
    let name = c(".harness.lock")?;
    let fd = unsafe {
        libc::openat(
            store.0.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io_error("Cannot open Harness writer lease"));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err("Invalid Harness writer lease".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Another native Harness transaction is active".into());
    }
    Ok(file)
}

impl Dir {
    pub(super) fn facts(&self, name: &str) -> Result<Option<Facts>, String> {
        self.stat(name).map(|value| {
            value.map(|stat| Facts {
                kind: match stat.st_mode & libc::S_IFMT {
                    libc::S_IFDIR => NodeKind::Directory,
                    libc::S_IFREG => NodeKind::Regular,
                    _ => NodeKind::Other,
                },
                size: stat.st_size as u64,
                links: stat.st_nlink as u64,
            })
        })
    }
    pub(super) fn read_binding(&self, name: &str) -> Result<Vec<u8>, String> {
        self.read(name, MAX_RECORD).map(|value| value.1)
    }
    pub(super) fn remove_empty(&self, name: &str) -> Result<(), String> {
        let name = c(name)?;
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error();
            if errno != Some(libc::ENOTEMPTY) && errno != Some(libc::EEXIST) {
                return Err(io_error("Cannot clean Harness directory"));
            }
        }
        self.sync()
    }
}
