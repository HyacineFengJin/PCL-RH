//! The chooser supplies one directory. Descriptor walks reject symlink
//! ancestors; the final FD pins that directory while anonymous staging and linkat publish
//! without replacement. A published file is retained on durability uncertainty.
use super::super::*;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, PathBuf},
};
#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}
pub(crate) struct Directory {
    file: File,
    path: PathBuf,
    identity: Identity,
}
fn c(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| "保存路径含无效字符".into())
}
fn io_error(message: &str) -> String {
    format!("{message}：{}", std::io::Error::last_os_error())
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        {
            return Err("保存位置必须是规范绝对路径".into());
        }
        let descriptor = unsafe {
            libc::open(
                c("/")?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(io_error("无法打开保存目录"));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let mut directory = Self {
            identity: Identity::of(&file.metadata().map_err(|_| "无法检查保存目录")?),
            file,
            path: PathBuf::from("/"),
        };
        for part in path.components() {
            if let Component::Normal(part) = part {
                let name = CString::new(part.as_bytes()).map_err(|_| "保存目录含无效字符")?;
                let descriptor = unsafe {
                    libc::openat(
                        directory.file.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if descriptor < 0 {
                    return Err(io_error("保存目录祖先已变化、不可访问或含符号链接"));
                }
                let file = unsafe { File::from_raw_fd(descriptor) };
                directory = Self {
                    identity: Identity::of(&file.metadata().map_err(|_| "无法检查保存目录")?),
                    file,
                    path: path.to_path_buf(),
                };
            }
        }
        directory.path = path.to_path_buf();
        directory.identity()?;
        Ok(directory)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn recheck(&self) -> Result<()> {
        if self.identity()? != self.identity || Self::open(&self.path)?.identity()? != self.identity
        {
            return Err("所选下载目录已被替换或移除，请重新选择".into());
        }
        Ok(())
    }
    fn identity(&self) -> Result<Identity> {
        let metadata = self.file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.nlink() == 0 {
            return Err("保存目录已被移除".into());
        }
        Ok(Identity::of(&metadata))
    }
    fn stat(&self, name: &str) -> Result<Option<libc::stat>> {
        let mut stat = std::mem::MaybeUninit::uninit();
        let code = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                c(name)?.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if code == 0 {
            return Ok(Some(unsafe { stat.assume_init() }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(format!("无法检查保存文件：{error}"))
        }
    }
    pub fn absent(&self, name: &str) -> Result<()> {
        if self.stat(name)?.is_some() {
            Err("保存位置已存在文件、目录或链接；不会覆盖已有内容".into())
        } else {
            Ok(())
        }
    }
    pub fn anonymous(&self) -> Result<File> {
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                c(".")?.as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(io_error("选择的文件系统不支持匿名暂存，保存目录未改动"));
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
    fn link_anonymous(&self, file: &File, name: &str) -> Result<()> {
        let code = unsafe {
            libc::linkat(
                file.as_raw_fd(),
                c("")?.as_ptr(),
                self.file.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        if code == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EPERM) | Some(libc::EINVAL) | Some(libc::ENOENT)
        ) {
            return Err(format!("无法保存文件（不会覆盖已有内容）：{error}"));
        }
        // Some unprivileged kernels require this equivalent owned-FD route.
        // The followed proc link refers only to our live anonymous descriptor.
        let source = c(&format!("/proc/self/fd/{}", file.as_raw_fd()))?;
        if unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                source.as_ptr(),
                self.file.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        } != 0
        {
            return Err(io_error("无法保存文件（不会覆盖已有内容）"));
        }
        Ok(())
    }
}
impl Directory {
    pub fn publish(&self, name: &str, file: &VerifiedAnonymous) -> Result<Option<String>> {
        self.publish_with_sync(name, file, || {
            self.file.sync_all().map_err(|error| error.to_string())
        })
    }
    #[cfg(test)]
    pub(crate) fn publish_with_failed_sync(
        &self,
        name: &str,
        file: &VerifiedAnonymous,
    ) -> Result<Option<String>> {
        self.publish_with_sync(name, file, || Err("fixture sync failure".into()))
    }
    fn publish_with_sync(
        &self,
        name: &str,
        file: &VerifiedAnonymous,
        sync: impl FnOnce() -> Result<()>,
    ) -> Result<Option<String>> {
        self.recheck()?;
        self.absent(name)?;
        if stamp(&file.file.metadata().map_err(|error| error.to_string())?) != file.stamp {
            return Err("保存暂存文件在最终复核后变化；文件未发布".into());
        }
        // linkat cannot overwrite a concurrently created target. Once it
        // succeeds, cancellation cannot roll back another program's edits.
        self.link_anonymous(&file.file, name)?;
        let mut warnings = Vec::new();
        if sync().is_err() {
            warnings.push("文件已保存，但保存目录同步失败；断电后的持久性未确认".to_string());
        }
        match self.stat(name) {
            Ok(Some(stat))
                if stat.st_dev == file.identity.device
                    && stat.st_ino == file.identity.inode
                    && stat.st_size == file.size as i64
                    && stat.st_mode & libc::S_IFMT == libc::S_IFREG =>
            {
                ()
            }
            _ => warnings.push("文件已发布，但随后目标文件被外部修改或移除；请检查保存目录".into()),
        }
        if Directory::open(&self.path)
            .and_then(|parent| parent.identity())
            .ok()
            != Some(self.identity)
        {
            warnings.push(
                "文件已保存到所选目录的原始位置，但目录路径随后被外部替换；请检查原目录".into(),
            );
        }
        Ok((!warnings.is_empty()).then(|| warnings.join("；")))
    }
}
pub(crate) struct VerifiedAnonymous {
    file: File,
    identity: Identity,
    size: u64,
    stamp: FileStamp,
}
type FileStamp = (u64, u64, u64, u64, u32, i64, i64, i64, i64);
fn stamp(metadata: &Metadata) -> FileStamp {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.nlink(),
        metadata.mode(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
pub(crate) fn verify_anonymous(
    mut file: File,
    size: u64,
    sha256: &str,
    cancel: &AtomicBool,
) -> Result<VerifiedAnonymous> {
    let before = file.metadata().map_err(|error| error.to_string())?;
    if !before.is_file() || before.nlink() != 0 || before.len() != size {
        return Err("保存暂存文件的类型、大小或所有权无效".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut buffer).map_err(|_| "无法复核保存暂存文件")?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or("保存暂存文件长度超限")?;
        if bytes > size {
            return Err("保存暂存文件读取期间变化".into());
        }
        hash.update(&buffer[..count]);
    }
    if bytes != size
        || format!("{:x}", hash.finalize()) != sha256
        || stamp(&file.metadata().map_err(|error| error.to_string())?) != stamp(&before)
    {
        return Err("保存暂存文件大小或SHA256复核失败".into());
    }
    file.sync_all()
        .map_err(|_| "无法同步已校验的保存暂存文件")?;
    Ok(VerifiedAnonymous {
        file,
        identity: Identity::of(&before),
        size,
        stamp: stamp(&before),
    })
}

pub(crate) type Staging = File;
pub(crate) fn staging_file(file: &mut Staging) -> &mut File {
    file
}
