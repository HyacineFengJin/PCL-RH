//! The cache is launcher-owned, but is still not trusted executable input. Unix
//! operations stay relative to held directory FDs; a renamed/replaced pathname
//! fails the final binding check instead of redirecting publication elsewhere.
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

fn error(error: impl std::fmt::Display) -> String {
    format!("无法准备游戏窗口标题组件：{error}")
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::{
        ffi::CString,
        fs::{File, OpenOptions},
        io::Write,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{MetadataExt, OpenOptionsExt},
        },
        sync::atomic::{AtomicU64, Ordering},
    };

    pub(super) struct Directory {
        path: PathBuf,
        chain: Vec<File>,
    }

    fn open_at(parent: &File, name: &str, flags: i32) -> Result<File, std::io::Error> {
        let name = CString::new(name)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn same(left: &File, right: &File) -> Result<bool, String> {
        let left = left.metadata().map_err(error)?;
        let right = right.metadata().map_err(error)?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }

    impl Directory {
        pub(super) fn open(project: &Path) -> Result<Self, String> {
            let project = fs::canonicalize(project).map_err(error)?;
            let root = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&project)
                .map_err(error)?;
            let mut chain = vec![root];
            for component in [".pcl-rust", "game-title-agent"] {
                let name = CString::new(component).unwrap();
                let parent = chain.last().unwrap();
                let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
                if result != 0 {
                    let failure = std::io::Error::last_os_error();
                    if failure.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error(failure));
                    }
                }
                chain.push(
                    open_at(parent, component, libc::O_RDONLY | libc::O_DIRECTORY)
                        .map_err(error)?,
                );
            }
            let directory = Self {
                path: project.join(".pcl-rust/game-title-agent"),
                chain,
            };
            directory.verify_binding()?;
            Ok(directory)
        }

        pub(super) fn path(&self) -> &Path {
            &self.path
        }

        pub(super) fn verify_binding(&self) -> Result<(), String> {
            let project = self.path.parent().unwrap().parent().unwrap();
            let root = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(project)
                .map_err(error)?;
            if !same(&root, &self.chain[0])? {
                return Err("游戏窗口标题缓存目录在准备期间变化".into());
            }
            for (index, component) in [".pcl-rust", "game-title-agent"].iter().enumerate() {
                let current = open_at(
                    &self.chain[index],
                    component,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                )
                .map_err(error)?;
                if !same(&current, &self.chain[index + 1])? {
                    return Err("游戏窗口标题缓存目录在准备期间变化".into());
                }
            }
            Ok(())
        }

        pub(super) fn read(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>, String> {
            let owner = self.chain.last().unwrap();
            let file = match open_at(owner, name, libc::O_RDONLY | libc::O_NONBLOCK) {
                Ok(file) => file,
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(failure) => return Err(error(failure)),
            };
            if !file.metadata().map_err(error)?.is_file() {
                return Err("游戏窗口标题缓存不是普通文件".into());
            }
            let mut bytes = Vec::new();
            file.take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(error)?;
            Ok(Some(bytes))
        }

        pub(super) fn publish(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
            self.verify_binding()?;
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let owner = self.chain.last().unwrap();
            let mut temporary = None;
            for _ in 0..32 {
                let name = format!(
                    ".agent-{}-{}.tmp",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                );
                match open_at(owner, &name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL) {
                    Ok(file) => {
                        temporary = Some((name, file));
                        break;
                    }
                    Err(failure) => {
                        // A pre-existing temporary name is never opened or removed.
                        if failure.kind() != std::io::ErrorKind::AlreadyExists {
                            return Err(error(failure));
                        }
                    }
                }
            }
            let (temporary_name, mut file) = temporary.ok_or("无法创建游戏窗口标题临时文件")?;
            let source = CString::new(temporary_name).unwrap();
            let destination = CString::new(name).map_err(error)?;
            // The source file belongs to this preparation; all exits remove its
            // temporary name, including failed sync, binding checks, and linkat.
            let result = (|| {
                file.write_all(bytes).map_err(error)?;
                file.sync_all().map_err(error)?;
                self.verify_binding()?;
                let result = unsafe {
                    libc::linkat(
                        owner.as_raw_fd(),
                        source.as_ptr(),
                        owner.as_raw_fd(),
                        destination.as_ptr(),
                        0,
                    )
                };
                if result != 0 {
                    let failure = std::io::Error::last_os_error();
                    if failure.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error(failure));
                    }
                }
                owner.sync_all().map_err(error)?;
                Ok(())
            })();
            unsafe {
                libc::unlinkat(owner.as_raw_fd(), source.as_ptr(), 0);
            }
            result
        }
    }
}

// Windows retains byte-for-byte verification and no-overwrite publication. The
// native Windows launcher carrier does not yet provide the Unix descriptor
// relative path guarantees; reject links and recheck canonical paths at both
// boundaries rather than claiming protection against every same-user rename.
#[cfg(not(unix))]
mod platform {
    use super::*;
    use std::{fs::File, io::Write};

    pub(super) struct Directory {
        path: PathBuf,
    }

    impl Directory {
        pub(super) fn open(project: &Path) -> Result<Self, String> {
            let mut path = fs::canonicalize(project).map_err(error)?;
            for part in [".pcl-rust", "game-title-agent"] {
                path.push(part);
                match fs::create_dir(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(failure) => return Err(error(failure)),
                }
                let metadata = fs::symlink_metadata(&path).map_err(error)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err("游戏窗口标题缓存不是普通目录".into());
                }
            }
            Ok(Self { path })
        }
        pub(super) fn path(&self) -> &Path {
            &self.path
        }
        pub(super) fn verify_binding(&self) -> Result<(), String> {
            for path in [self.path.parent().unwrap(), self.path.as_path()] {
                let metadata = fs::symlink_metadata(path).map_err(error)?;
                if !metadata.is_dir()
                    || metadata.file_type().is_symlink()
                    || fs::canonicalize(path).map_err(error)? != path
                {
                    return Err("游戏窗口标题缓存目录在准备期间变化".into());
                }
            }
            Ok(())
        }
        pub(super) fn read(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>, String> {
            self.verify_binding()?;
            let path = self.path.join(name);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(failure) => return Err(error(failure)),
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("游戏窗口标题缓存不是普通文件".into());
            }
            let mut bytes = Vec::new();
            File::open(path)
                .map_err(error)?
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(error)?;
            Ok(Some(bytes))
        }
        pub(super) fn publish(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
            self.verify_binding()?;
            let mut temporary = tempfile::NamedTempFile::new_in(&self.path).map_err(error)?;
            temporary.write_all(bytes).map_err(error)?;
            temporary.as_file().sync_all().map_err(error)?;
            self.verify_binding()?;
            match temporary.persist_noclobber(self.path.join(name)) {
                Ok(_) => Ok(()),
                Err(failure) if failure.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(failure) => Err(error(failure.error)),
            }
        }
    }
}

pub(super) struct Cache(platform::Directory);

impl Cache {
    pub(super) fn open(project: &Path) -> Result<Self, String> {
        platform::Directory::open(project).map(Self)
    }

    pub(super) fn path(&self) -> &Path {
        self.0.path()
    }

    pub(super) fn ensure(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        if self.0.read(name, bytes.len())?.is_none() {
            self.0.publish(name, bytes)?;
        }
        if self.0.read(name, bytes.len())?.as_deref() != Some(bytes) {
            return Err("游戏窗口标题组件的缓存校验失败；请移除该缓存后重试".into());
        }
        self.0.verify_binding()?;
        Ok(self.path().join(name))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn replacing_an_open_cache_directory_retires_the_owner_and_never_writes_new_path() {
        let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/game-title-agent");
        fs::create_dir_all(&work).unwrap();
        let fixture = tempfile::tempdir_in(work).unwrap();
        let cache = Cache::open(fixture.path()).unwrap();
        let old = fixture.path().join("retired");
        fs::rename(cache.path(), &old).unwrap();
        fs::create_dir(cache.path()).unwrap();
        assert!(cache.ensure("agent.jar", b"fixture").is_err());
        assert_eq!(fs::read_dir(cache.path()).unwrap().count(), 0);
        assert_eq!(fs::read_dir(old).unwrap().count(), 0);
    }
}
