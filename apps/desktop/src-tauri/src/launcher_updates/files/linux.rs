//! Anonymous staging is confined to this project's launcher data directory.
//! Descriptor identities are rechecked against visible paths before accepting
//! a result. Cancellation and process exit unlink nothing: O_TMPFILE has no name.
use super::super::{manifest::verify_elf, model::Architecture, provider::MAX_BINARY_BYTES};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(in crate::launcher_updates) struct Identity(
    pub(in crate::launcher_updates) u64,
    pub(in crate::launcher_updates) u64,
);
pub(in crate::launcher_updates) struct Directory(pub(in crate::launcher_updates) File);
impl Directory {
    pub(in crate::launcher_updates) fn open(path: &Path) -> Result<Self, String> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        {
            return Err("更新服务需要规范的绝对项目路径".into());
        }
        let fd = unsafe {
            libc::open(
                c("/")?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err("无法打开更新项目目录".into());
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for component in path.components() {
            if let Component::Normal(name) = component {
                let name = CString::new(name.as_bytes()).map_err(|_| "更新目录路径无效")?;
                dir = dir.child(&name)?;
            }
        }
        Ok(dir)
    }
    pub(in crate::launcher_updates) fn child(&self, name: &CString) -> Result<Self, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err("无法打开更新目录，不允许符号链接或特殊文件".into());
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }
    fn ensure_child(&self, name: &str) -> Result<Self, String> {
        let name = c(name)?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), name.as_ptr(), 0o700) } != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err("无法创建启动器更新暂存目录".into());
        }
        self.child(&name)
    }
    pub(in crate::launcher_updates) fn identity(&self) -> Result<Identity, String> {
        let meta = self.0.metadata().map_err(|_| "无法检查更新目录标识")?;
        Ok(Identity(meta.dev(), meta.ino()))
    }
}
pub(in crate::launcher_updates) fn c(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "更新路径含无效字符".into())
}

pub(in crate::launcher_updates) struct Staging {
    project: PathBuf,
    project_dir: Directory,
    storage: Directory,
    directory: Directory,
}
impl Staging {
    pub(in crate::launcher_updates) fn prepare(project: &Path) -> Result<Self, String> {
        let project_dir = Directory::open(project)?;
        let storage = project_dir.ensure_child(".pcl-rust")?;
        let directory = storage.ensure_child("launcher-update-staging")?;
        let staging = Self {
            project: project.into(),
            project_dir,
            storage,
            directory,
        };
        staging.check_visible()?;
        Ok(staging)
    }
    pub(in crate::launcher_updates) fn check_visible(&self) -> Result<(), String> {
        let root = Directory::open(&self.project)?;
        let storage = root.child(&c(".pcl-rust")?)?;
        let directory = storage.child(&c("launcher-update-staging")?)?;
        if root.identity()? != self.project_dir.identity()?
            || storage.identity()? != self.storage.identity()?
            || directory.identity()? != self.directory.identity()?
        {
            return Err("更新暂存目录已被外部替换，已停止本次下载".into());
        }
        Ok(())
    }
    pub(in crate::launcher_updates) fn anonymous(&self) -> Result<Stage, String> {
        self.check_visible()?;
        let fd = unsafe {
            libc::openat(
                self.directory.0.as_raw_fd(),
                c(".")?.as_ptr(),
                libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err("当前文件系统不支持安全匿名更新暂存（需要 O_TMPFILE）".into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file.metadata().map_err(|_| "无法检查更新暂存文件")?;
        if !meta.is_file() || meta.nlink() != 0 || meta.len() != 0 {
            return Err("更新暂存文件标识无效".into());
        }
        Ok(Stage {
            #[cfg(test)]
            retire_hook: None,
            file,
            original: None,
            received: 0,
            digest: Sha256::new(),
        })
    }
    pub(in crate::launcher_updates) fn original_token(&self) -> Result<FileToken, String> {
        self.check_visible()?;
        let bin = self.storage.child(&c("bin")?)?;
        let fd = unsafe {
            libc::openat(
                bin.0.as_raw_fd(),
                c("pcl-desktop")?.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err("无法读取便携启动器原文件，不允许符号链接".into());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let before = file.metadata().map_err(|_| "无法检查便携启动器原文件")?;
        if !before.is_file() || before.nlink() != 1 || before.len() > MAX_BINARY_BYTES {
            return Err("便携启动器原文件不是受支持的独立普通文件".into());
        }
        let mut digest = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|_| "无法读取便携启动器原文件")?;
            if count == 0 {
                break;
            }
            total = total.saturating_add(count as u64);
            if total > MAX_BINARY_BYTES {
                return Err("便携启动器原文件在读取期间变大".into());
            }
            digest.update(&buffer[..count]);
        }
        let after = file
            .metadata()
            .map_err(|_| "无法重新检查便携启动器原文件")?;
        let mut visible = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                bin.0.as_raw_fd(),
                c("pcl-desktop")?.as_ptr(),
                visible.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err("便携启动器原文件在读取期间已消失".into());
        }
        let visible = unsafe { visible.assume_init() };
        if stamp(&before) != stamp(&after)
            || visible.st_dev != after.dev()
            || visible.st_ino != after.ino()
            || visible.st_size < 0
            || visible.st_size as u64 != after.len()
            || visible.st_mtime != after.mtime()
            || visible.st_mtime_nsec != after.mtime_nsec()
            || visible.st_ctime != after.ctime()
            || visible.st_ctime_nsec != after.ctime_nsec()
            || visible.st_nlink != 1
            || visible.st_mode & libc::S_IFMT != libc::S_IFREG
            || total != after.len()
        {
            return Err("便携启动器原文件被外部修改，未准备替换计划".into());
        }
        // `bin` itself is also bound; checking only its open descriptor would
        // incorrectly approve a plan for a detached directory after a rename.
        if self.storage.child(&c("bin")?)?.identity()? != bin.identity()? {
            return Err("便携启动器安装目录已被外部替换".into());
        }
        self.check_visible()?;
        Ok(FileToken::from_meta(
            &after,
            format!("{:x}", digest.finalize()),
        ))
    }
}
fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

/// Rename changes ctime; inode, content, mtime and permissions remain the
/// transaction's identity. A read still checks ctime before/after hashing to
/// reject edits during the read itself.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::launcher_updates) struct FileToken {
    pub(in crate::launcher_updates) device: u64,
    pub(in crate::launcher_updates) inode: u64,
    pub(in crate::launcher_updates) size: u64,
    pub(in crate::launcher_updates) modified: (i64, i64),
    pub(in crate::launcher_updates) mode: u32,
    pub(in crate::launcher_updates) sha256: String,
}
impl FileToken {
    pub(in crate::launcher_updates) fn from_meta(meta: &std::fs::Metadata, sha256: String) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            size: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            mode: meta.mode() & 0o7777,
            sha256,
        }
    }
}

pub(in crate::launcher_updates) struct Stage {
    #[cfg(test)]
    pub(in crate::launcher_updates) retire_hook: Option<Box<dyn FnOnce() + Send>>,

    // The unnamed descriptor is kept alive in the service after validation.
    pub(in crate::launcher_updates) file: File,
    pub(in crate::launcher_updates) original: Option<FileToken>,
    received: u64,
    digest: Sha256,
}
impl Stage {
    pub(in crate::launcher_updates) fn append(
        &mut self,
        bytes: &[u8],
        expected: u64,
    ) -> Result<u64, String> {
        let count = self
            .received
            .checked_add(bytes.len() as u64)
            .ok_or("更新下载大小溢出")?;
        if count > expected || count > MAX_BINARY_BYTES {
            return Err("更新资产超过清单声明的大小".into());
        }
        self.file
            .write_all(bytes)
            .map_err(|_| "无法写入启动器更新暂存文件")?;
        self.digest.update(bytes);
        self.received = count;
        Ok(count)
    }
    pub(in crate::launcher_updates) fn finish(
        &mut self,
        size: u64,
        sha256: &str,
        architecture: &Architecture,
    ) -> Result<(), String> {
        if self.received != size
            || format!("{:x}", self.digest.clone().finalize()) != sha256.to_ascii_lowercase()
        {
            return Err("更新资产大小或 SHA256 不匹配，暂存内容已丢弃".into());
        }
        self.file.sync_all().map_err(|_| "无法同步更新暂存文件")?;
        let meta = self.file.metadata().map_err(|_| "无法检查更新暂存文件")?;
        if !meta.is_file() || meta.nlink() != 0 || meta.len() != size {
            return Err("更新匿名暂存文件在校验期间发生变化".into());
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "无法读取更新 ELF 文件头")?;
        let mut header = [0u8; 64];
        self.file
            .read_exact(&mut header)
            .map_err(|_| "更新 ELF 文件头不完整")?;
        verify_elf(&header, architecture)?;
        let offset = u64::from_le_bytes(header[32..40].try_into().unwrap());
        let count = u16::from_le_bytes([header[56], header[57]]) as u64;
        let entry = u64::from_le_bytes(header[24..32].try_into().unwrap());
        if count > 128 || offset.checked_add(count * 56).is_none_or(|end| end > size) {
            return Err("更新 ELF 程序头超过文件边界".into());
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|_| "无法读取更新 ELF 程序头")?;
        let mut executable_entry = false;
        for _ in 0..count {
            let mut program = [0u8; 56];
            self.file
                .read_exact(&mut program)
                .map_err(|_| "更新 ELF 程序头不完整")?;
            let kind = u32::from_le_bytes(program[..4].try_into().unwrap());
            if kind != 1 {
                continue;
            }
            let flags = u32::from_le_bytes(program[4..8].try_into().unwrap());
            let file_offset = u64::from_le_bytes(program[8..16].try_into().unwrap());
            let address = u64::from_le_bytes(program[16..24].try_into().unwrap());
            let file_size = u64::from_le_bytes(program[32..40].try_into().unwrap());
            let memory_size = u64::from_le_bytes(program[40..48].try_into().unwrap());
            if file_size > memory_size
                || file_offset
                    .checked_add(file_size)
                    .is_none_or(|end| end > size)
                || address.checked_add(memory_size).is_none()
            {
                return Err("更新 ELF 加载段超过文件边界".into());
            }
            if flags & 1 != 0 && entry >= address && entry < address + memory_size {
                executable_entry = true;
            }
        }
        if !executable_entry {
            return Err("更新 ELF 未包含有效可执行入口".into());
        }
        // Verify bytes actually persisted in the anonymous descriptor, rather
        // than accepting only the digest of chunks submitted to write_all.
        let before = self.file.metadata().map_err(|_| "无法检查更新暂存文件")?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "无法重新读取更新暂存文件")?;
        let mut digest = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = self
                .file
                .read(&mut buffer)
                .map_err(|_| "无法重新读取更新暂存文件")?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > size {
                return Err("更新暂存文件在校验期间变大".into());
            }
            digest.update(&buffer[..count]);
        }
        let after = self
            .file
            .metadata()
            .map_err(|_| "无法重新检查更新暂存文件")?;
        if total != size
            || stamp(&before) != stamp(&after)
            || after.nlink() != 0
            || format!("{:x}", digest.finalize()) != sha256.to_ascii_lowercase()
        {
            return Err("更新暂存文件在校验期间被修改，已丢弃".into());
        }
        Ok(())
    }
}

impl Stage {
    pub(in crate::launcher_updates) fn file(&self) -> &File {
        &self.file
    }
    #[cfg(test)]
    pub(in crate::launcher_updates) fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }
    #[allow(unused_mut)] // Only the test ownership observer mutates self.
    pub(in crate::launcher_updates) fn retire(mut self) -> Result<(), String> {
        #[cfg(test)]
        if let Some(hook) = self.retire_hook.take() {
            hook();
        }
        Ok(())
    }
}
#[derive(Clone)]
pub(in crate::launcher_updates) struct Cleanup;
impl Cleanup {
    pub(in crate::launcher_updates) fn check(&self) -> Result<(), String> {
        Ok(())
    }
}
impl Staging {
    pub(in crate::launcher_updates) fn cleanup(&self) -> Cleanup {
        Cleanup
    }
}

impl FileToken {
    pub(in crate::launcher_updates) fn valid(&self) -> bool {
        self.size >= 64
            && self.size <= MAX_BINARY_BYTES
            && self.inode != 0
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            && self.mode & !0o777 == 0
    }
    pub(in crate::launcher_updates) fn from_file(
        file: &File,
        hash: String,
    ) -> Result<Self, String> {
        Ok(Self::from_meta(
            &file.metadata().map_err(|e| e.to_string())?,
            hash,
        ))
    }
    #[cfg(test)]
    pub(in crate::launcher_updates) fn identity(&self) -> pcl_core::retained_fs::Identity {
        pcl_core::retained_fs::Identity::Unix {
            device: self.device,
            inode: self.inode,
        }
    }
    pub(in crate::launcher_updates) fn same_identity(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
    pub(in crate::launcher_updates) fn same_filesystem(
        &self,
        dir: &Directory,
    ) -> Result<bool, String> {
        Ok(self.device == dir.0.metadata().map_err(|e| e.to_string())?.dev())
    }
}

impl Directory {
    pub(in crate::launcher_updates) fn sync(&self) -> Result<(), String> {
        self.0.sync_all().map_err(|e| e.to_string())
    }
}
