//! Existing Linux openat2/O_TMPFILE/exchange implementation.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, File},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) struct Dir(File);
fn error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}
pub(crate) fn name_ok(name: &str) -> Result<()> {
    pcl_core::identifier(name)?;
    if name.len() > 255 || name.chars().any(char::is_control) {
        return Err("实例或文件名称无效".into());
    }
    Ok(())
}
fn c(name: &str) -> Result<CString> {
    name_ok(name)?;
    CString::new(name).map_err(|e| e.to_string())
}
pub(crate) fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.as_os_str().len() > 4096
        || path
            .components()
            .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
        || path
            .to_str()
            .is_none_or(|p| p.chars().any(char::is_control))
    {
        return Err("实例操作需要规范的绝对路径".into());
    }
    Ok(())
}
pub(crate) fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize().map_err(|e| e.to_string())
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Dir {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        absolute(path)?;
        let slash = CString::new("/").map_err(|e| e.to_string())?;
        let fd = unsafe {
            libc::open(
                slash.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(error("无法打开实例操作目录"));
        }
        let mut current = Self(unsafe { File::from_raw_fd(fd) });
        // Absolute ancestors may cross filesystems; every descendant of the
        // bound root uses openat2 NO_XDEV (including bind-mounted regular files).
        for p in path.components() {
            if let Component::Normal(p) = p {
                let p = CString::new(p.as_bytes()).map_err(|e| e.to_string())?;
                let fd = unsafe {
                    libc::openat(
                        current.0.as_raw_fd(),
                        p.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    )
                };
                if fd < 0 {
                    return Err(error("实例操作目录祖先已变化或包含符号链接"));
                }
                current = Self(unsafe { File::from_raw_fd(fd) });
            }
        }
        Ok(current)
    }
    fn open_child(&self, name: &str, flags: i32) -> Result<File> {
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
            return Err(error(
                "目录或文件已变化、含符号链接或挂载点（需要 Linux 5.6 以上）",
            ));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(crate) fn child(&self, name: &str) -> Result<Self> {
        self.open_child(name, libc::O_RDONLY | libc::O_DIRECTORY)
            .map(Self)
    }
    pub(crate) fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(crate) fn create(&self, name: &str) -> Result<Self> {
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), c(name)?.as_ptr(), 0o700) } != 0 {
            return Err(error("无法创建实例恢复目录"));
        }
        self.sync()?;
        self.child(name)
    }
    pub(crate) fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(d) => Ok(d),
            None => self.create(name),
        }
    }
    pub(crate) fn key(&self) -> Result<Key> {
        self.0
            .metadata()
            .map(|m| Key::of(&m))
            .map_err(|e| e.to_string())
    }
    pub(crate) fn sync(&self) -> Result<()> {
        self.0.sync_all().map_err(|e| e.to_string())
    }
    pub(crate) fn stat(&self, name: &str) -> Result<Option<libc::stat>> {
        let mut st = std::mem::MaybeUninit::uninit();
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(unsafe { st.assume_init() }));
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error("无法检查实例操作文件"))
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
                .map_err(|_| "目录含非UTF-8名称")?;
            name_ok(&name)?;
            names.push(name);
            if names.len() > MAX_FILES {
                return Err("实例文件数量超过安全上限".into());
            }
        }
        names.sort();
        Ok(names)
    }
    pub(crate) fn regular(&self, name: &str) -> Result<File> {
        let file = self.open_child(name, libc::O_RDONLY | libc::O_NONBLOCK)?;
        let m = file.metadata().map_err(|e| e.to_string())?;
        if !m.is_file() || m.nlink() != 1 {
            return Err("操作文件必须是独立普通文件，原文件已保留".into());
        }
        Ok(file)
    }
    pub(crate) fn anonymous(&self) -> Result<File> {
        let dot = CString::new(".").map_err(|e| e.to_string())?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(error("文件系统不支持安全的匿名事务记录"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(crate) fn link(&self, file: &File, name: &str) -> Result<()> {
        let path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|e| e.to_string())?;
        if unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                path.as_ptr(),
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        } != 0
        {
            return Err(error("无法无覆盖发布实例操作记录"));
        }
        self.sync()
    }
    pub(crate) fn unlink_file(&self, name: &str, expected: &FileToken) -> Result<()> {
        let current = read_bytes(self, name, MAX_JOURNAL)?.0;
        if expected != &current {
            return Err("操作记录被外部修改，已保留".into());
        }
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), c(name)?.as_ptr(), 0) } != 0 {
            return Err(error("无法清理实例操作记录"));
        }
        self.sync()
    }
    /// The caller owns the journal transition and any conflict rollback. Keep
    /// exchange separate from sync so both publication orders remain explicit.
    pub(crate) fn exchange(&self, first: &str, second: &str, context: &str) -> Result<()> {
        if unsafe {
            libc::renameat2(
                self.0.as_raw_fd(),
                c(first)?.as_ptr(),
                self.0.as_raw_fd(),
                c(second)?.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        } != 0
        {
            return Err(error(context));
        }
        Ok(())
    }

    /// Only empty setup artifacts may be removed. Recheck the linked directory
    /// after the caller verifies the root/store binding; never erase contents.
    pub(crate) fn remove_empty(&self, name: &str, expected: &Key) -> Result<()> {
        let current = self.child(name)?;
        if &current.key()? != expected {
            return Err("空实例恢复记录在清理前已被替换，已保留".into());
        }
        if !current.names()?.is_empty() {
            return Err("未登记的实例恢复目录含文件，已保留".into());
        }
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), c(name)?.as_ptr(), libc::AT_REMOVEDIR) } != 0
        {
            return Err(error("无法清理空实例恢复记录"));
        }
        self.sync()
    }
}

fn file_token(mut file: File, cancel: Option<&AtomicBool>) -> Result<FileToken> {
    let before = Stamp::of(&file.metadata().map_err(|e| e.to_string())?);
    if before.mode & libc::S_IFMT != libc::S_IFREG || before.links != 1 {
        return Err("实例含符号链接、硬链接或特殊文件，已保留".into());
    }
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes = bytes.checked_add(count as u64).ok_or("实例内容过大")?;
        digest.update(&buffer[..count]);
    }
    if before != Stamp::of(&file.metadata().map_err(|e| e.to_string())?) || bytes != before.size {
        return Err("实例内容在读取期间变化，已保留".into());
    }
    Ok(FileToken {
        stamp: before,
        hash: format!("{:x}", digest.finalize()),
    })
}
pub(crate) fn read_bytes(dir: &Dir, name: &str, limit: usize) -> Result<(FileToken, Vec<u8>)> {
    let mut file = dir.regular(name)?;
    let before = Stamp::of(&file.metadata().map_err(|e| e.to_string())?);
    if before.size > limit as u64 {
        return Err("实例操作记录超过安全上限，已保留".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit || before != Stamp::of(&file.metadata().map_err(|e| e.to_string())?) {
        return Err("实例操作记录读取期间变化".into());
    }
    let linked = dir.regular(name)?.metadata().map_err(|e| e.to_string())?;
    if Stamp::of(&linked) != before {
        return Err("实例操作记录路径已被外部替换".into());
    }
    Ok((
        FileToken {
            stamp: before,
            hash: hash(&bytes),
        },
        bytes,
    ))
}
pub(crate) fn scan(dir: &Dir, cancel: Option<&AtomicBool>) -> Result<Tree> {
    fn visit(
        dir: &Dir,
        prefix: &str,
        tree: &mut Tree,
        depth: usize,
        cancel: Option<&AtomicBool>,
    ) -> Result<()> {
        cancelled(cancel)?;
        if depth > MAX_DEPTH || tree.entries.len() >= MAX_FILES {
            return Err("实例目录深度或文件数量超过安全上限".into());
        }
        let before = Stamp::of(&dir.0.metadata().map_err(|e| e.to_string())?);
        tree.entries.push(TreeEntry {
            path: prefix.into(),
            stamp: before.clone(),
            hash: None,
        });
        for name in dir.names()? {
            cancelled(cancel)?;
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let st = dir.stat(&name)?.ok_or("实例内容在扫描期间消失")?;
            if st.st_dev != before.key.dev {
                return Err("实例内容跨越文件系统，已保留".into());
            }
            match st.st_mode & libc::S_IFMT {
                libc::S_IFDIR => visit(&dir.child(&name)?, &path, tree, depth + 1, cancel)?,
                libc::S_IFREG => {
                    if tree.entries.len() >= MAX_FILES {
                        return Err("实例文件数量超过安全上限".into());
                    }
                    let token = file_token(dir.regular(&name)?, cancel)?;
                    let final_st = dir.stat(&name)?.ok_or("实例内容在读取期间消失")?;
                    if token.stamp.key
                        != (Key {
                            dev: final_st.st_dev,
                            ino: final_st.st_ino,
                        })
                    {
                        return Err("实例文件路径已被替换".into());
                    }
                    tree.files += 1;
                    tree.bytes = tree
                        .bytes
                        .checked_add(token.stamp.size)
                        .ok_or("实例内容过大")?;
                    tree.entries.push(TreeEntry {
                        path,
                        stamp: token.stamp,
                        hash: Some(token.hash),
                    });
                }
                _ => return Err("实例含符号链接或特殊文件，无法安全删除，原内容已保留".into()),
            }
        }
        if Stamp::of(&dir.0.metadata().map_err(|e| e.to_string())?) != before {
            return Err("实例目录在扫描期间变化，已保留".into());
        }
        Ok(())
    }
    let mut tree = Tree {
        entries: vec![],
        files: 0,
        bytes: 0,
    };
    visit(dir, "", &mut tree, 0, cancel)?;
    Ok(tree)
}

pub(crate) struct OperationLock(File);
impl OperationLock {
    #[cfg(test)]
    pub(crate) fn duplicate_descriptor(&self) -> Result<File> {
        self.0.try_clone().map_err(|e| e.to_string())
    }
}
impl Drop for OperationLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
pub(crate) fn lock(store: &Dir) -> Result<OperationLock> {
    let fd = unsafe {
        libc::openat(
            store.0.as_raw_fd(),
            c(".lock")?.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(error("无法锁定实例删除事务"));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let m = file.metadata().map_err(|e| e.to_string())?;
    if !m.is_file() || m.nlink() != 1 || Key::of(&m).dev != store.key()?.dev {
        return Err("实例删除锁已被替换或不安全".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("另一个进程正在修改实例删除记录".into());
    }
    let guard = OperationLock(file);
    let linked = store
        .regular(".lock")?
        .metadata()
        .map_err(|e| e.to_string())?;
    if Key::of(&linked) != Key::of(&m) {
        return Err("实例删除锁路径已被替换".into());
    }
    Ok(guard)
}
pub(crate) fn rename_new(source: &Dir, old: &str, destination: &Dir, new: &str) -> Result<()> {
    if source.key()?.dev != destination.key()?.dev {
        return Err("实例恢复目录跨文件系统，原目录已保留".into());
    }
    // Opening the final directory with NO_XDEV rejects a bind mount even when
    // its st_dev matches. The move never merges into an existing destination.
    source.child(old)?;
    if unsafe {
        libc::renameat2(
            source.0.as_raw_fd(),
            c(old)?.as_ptr(),
            destination.0.as_raw_fd(),
            c(new)?.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(error("无法无覆盖移动实例目录（目标可能已被占用）"));
    }
    source.sync()?;
    destination.sync()
}

pub(crate) fn anonymous_key(file: &File) -> Result<Key> {
    file.metadata()
        .map(|m| Key::of(&m))
        .map_err(|e| e.to_string())
}
pub(crate) fn require_mutation_support() -> Result<()> {
    Ok(())
}
