//! Private installs retain directory capabilities through each write. Proc paths
//! are only syscall adapters for libraries accepting paths; they refer to held
//! descriptors in the launcher process, including while Java children execute.
//! A scoped Landlock worker confines processor-derived destination paths to the
//! captured private inode. This is pathname confinement, not isolation from
//! other processes under the same user: a concurrent external hard-link alias
//! can still share an inode. Archive links are rejected; captured official
//! processors run before pack overrides. Neither restriction changes the caller
//! or legacy install API.
use super::*;
use std::{
    ffi::CString,
    ops::Deref,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
};

#[derive(Clone)]
pub(super) struct InstallDir {
    path: PathBuf,
    fd: Option<Arc<fs::File>>,
}
#[derive(Clone)]
pub(super) struct InstallPath {
    path: PathBuf,
    relative: PathBuf,
    // Keep the final parent alive until copy/persist/read/child wait completes.
    _parent: Option<Arc<fs::File>>,
}
impl Deref for InstallPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}
impl AsRef<Path> for InstallPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}
impl InstallPath {
    pub fn relative(&self) -> &Path {
        &self.relative
    }
    pub fn open(&self) -> Result<fs::File> {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        if self._parent.is_some() {
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&self.path).map_err(error)?;
        let metadata = file.metadata().map_err(error)?;
        if !metadata.is_file() || (self._parent.is_some() && metadata.nlink() != 1) {
            return Err("安装文件不是独占普通文件".into());
        }
        Ok(file)
    }
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true);
        if self._parent.is_some() {
            // Never truncate a pre-existing inode: it may be an outside file's
            // hard-link alias. Private metadata and embedded artifacts are new.
            options
                .create_new(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        } else {
            options.create(true).truncate(true);
        }
        let mut file = options.open(&self.path).map_err(error)?;
        let metadata = file.metadata().map_err(error)?;
        if !metadata.is_file() || (self._parent.is_some() && metadata.nlink() != 1) {
            return Err("安装目标不是独占普通文件".into());
        }
        file.write_all(bytes).map_err(error)
    }
}
pub(super) fn anchor(file: &fs::File) -> PathBuf {
    // Address the owning confined thread, not the unconfined process leader.
    // Landlock's ptrace boundary correctly denies a child access to a less
    // restricted task's /proc FD table. The worker remains alive through wait.
    // /proc/self would instead address Java's CLOEXEC descriptor table.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    PathBuf::from(format!("/proc/{tid}/fd/{}", file.as_raw_fd()))
}

fn parts(relative: &Path) -> Result<Vec<&std::ffi::OsStr>> {
    if relative.as_os_str().as_bytes().contains(&0)
        || relative.as_os_str().as_bytes().contains(&b'\\')
    {
        return Err("安装相对路径无效".into());
    }
    let mut names = Vec::new();
    for part in relative.components() {
        match part {
            std::path::Component::Normal(name) => names.push(name),
            _ => return Err("安装相对路径无效".into()),
        }
    }
    if names.len() > 64 {
        return Err("安装路径层级过深".into());
    }
    Ok(names)
}
fn open_dir(parent: &fs::File, name: &std::ffi::OsStr, create: bool) -> Result<fs::File> {
    let name = CString::new(name.as_bytes()).map_err(error)?;
    if create && unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error(e));
        }
    }
    // NO_XDEV | NO_SYMLINKS | BENEATH. Device numbers alone do not detect bind
    // mounts; openat2 is a required fail-closed private-install capability.
    use pcl_core::retained_fs::{Directory, Leaf, Traversal};
    let open = || -> std::io::Result<fs::File> {
        let parent = Directory::from_file(parent.try_clone()?)?;
        let name = std::ffi::OsStr::from_bytes(name.as_bytes());
        let child = if name == "." {
            parent.reopen(Traversal::SameFilesystem)?
        } else {
            parent.child(&Leaf::new(name)?, Traversal::SameFilesystem)?
        };
        child.try_clone_file()
    };
    open().map_err(|e| {
        format!("无法绑定私有安装子目录（需要 Linux openat2，禁止链接或挂载替换）：{e}")
    })
}
impl InstallDir {
    pub fn legacy(path: PathBuf) -> Self {
        Self { path, fd: None }
    }
    pub fn bound(file: fs::File) -> Result<Self> {
        if !file.metadata().map_err(error)?.is_dir() {
            return Err("私有安装根 FD 不是目录".into());
        }
        // Probe openat2 before a private install can write anything.
        let probe = open_dir(&file, std::ffi::OsStr::new("."), false)?;
        drop(probe);
        Ok(Self {
            path: anchor(&file),
            fd: Some(Arc::new(file)),
        })
    }
    pub fn reanchor(&self) -> Self {
        match &self.fd {
            Some(fd) => Self {
                path: anchor(fd),
                fd: Some(fd.clone()),
            },
            None => self.clone(),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn is_bound(&self) -> bool {
        self.fd.is_some()
    }
    pub fn fd(&self) -> Option<&fs::File> {
        self.fd.as_deref()
    }
    pub fn directory(&self, relative: impl AsRef<Path>, create: bool) -> Result<Self> {
        let relative = relative.as_ref();
        let names = parts(relative)?;
        if let Some(root) = &self.fd {
            let mut dir = root.clone();
            for name in names {
                dir = Arc::new(open_dir(&dir, name, create)?);
            }
            Ok(Self {
                path: anchor(&dir),
                fd: Some(dir),
            })
        } else {
            let path = pcl_core::safe_join(&self.path, relative)?;
            if create {
                fs::create_dir_all(&path).map_err(error)?;
            }
            Ok(Self::legacy(path))
        }
    }
    pub fn file(&self, relative: impl AsRef<Path>) -> Result<InstallPath> {
        let relative = relative.as_ref();
        let names = parts(relative)?;
        let leaf = names.last().ok_or("安装文件路径为空")?;
        if let Some(root) = &self.fd {
            let mut parent = root.clone();
            for name in &names[..names.len() - 1] {
                parent = Arc::new(open_dir(&parent, name, true)?);
            }
            let path = anchor(&parent).join(leaf);
            // Refuse existing linked/special leaves before hashes or writes.
            match fs::symlink_metadata(&path) {
                Ok(m) if !m.is_file() || m.nlink() != 1 => {
                    return Err("私有安装目标不是独占普通文件".into())
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(error(e)),
            }
            Ok(InstallPath {
                path,
                relative: relative.into(),
                _parent: Some(parent),
            })
        } else {
            Ok(InstallPath {
                path: pcl_core::safe_join(&self.path, relative)?,
                relative: relative.into(),
                _parent: None,
            })
        }
    }
    pub fn files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        self.walk(Path::new(""), 0, &mut 0usize, &mut files)?;
        Ok(files)
    }
    fn walk(
        &self,
        prefix: &Path,
        depth: usize,
        count: &mut usize,
        files: &mut Vec<PathBuf>,
    ) -> Result<()> {
        if depth > 64 {
            return Err("安装树层级过深".into());
        }
        for entry in fs::read_dir(&self.path).map_err(error)? {
            let entry = entry.map_err(error)?;
            *count += 1;
            if *count > 200_000 {
                return Err("安装树文件数量过多".into());
            }
            let kind = entry.file_type().map_err(error)?;
            let relative = prefix.join(entry.file_name());
            if kind.is_dir() {
                self.directory(Path::new(&entry.file_name()), false)?.walk(
                    &relative,
                    depth + 1,
                    count,
                    files,
                )?;
            } else if kind.is_file() {
                if self.is_bound() && entry.metadata().map_err(error)?.nlink() != 1 {
                    return Err("私有安装树发现多重硬链接，保留待恢复".into());
                }
                files.push(relative);
            } else {
                return Err("组件处理器生成了不允许的链接或特殊文件".into());
            }
        }
        Ok(())
    }
    pub fn remove_directory(&self, relative: &Path) -> Result<()> {
        if self.fd.is_none() {
            return fs::remove_dir_all(pcl_core::safe_join(&self.path, relative)?).map_err(error);
        }
        let names = parts(relative)?;
        let leaf = names.last().ok_or("清理目录路径为空")?;
        let parent_relative: PathBuf = names[..names.len() - 1].iter().collect();
        let parent = self.directory(parent_relative, false)?;
        let child = parent.directory(Path::new(leaf), false)?;
        // Preflight the full owned subtree before deleting anything. Unknown
        // symlinks, mounts and special nodes retain the private journal/tree.
        child.files()?;
        child.clear(0, &mut 0)?;
        same_directory(parent.path().join(leaf), child.fd().unwrap())?;
        let name = CString::new(leaf.as_bytes()).map_err(error)?;
        if unsafe {
            libc::unlinkat(
                parent.fd().unwrap().as_raw_fd(),
                name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } != 0
        {
            return Err(error(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    fn clear(&self, depth: usize, count: &mut usize) -> Result<()> {
        if depth > 64 {
            return Err("清理安装树层级过深".into());
        }
        for entry in fs::read_dir(&self.path).map_err(error)? {
            let entry = entry.map_err(error)?;
            *count += 1;
            if *count > 200_000 {
                return Err("清理安装树文件数量过多".into());
            }
            let kind = entry.file_type().map_err(error)?;
            let name = entry.file_name();
            if kind.is_dir() {
                let child = self.directory(Path::new(&name), false)?;
                child.clear(depth + 1, count)?;
                same_directory(self.path.join(&name), child.fd().unwrap())?;
            } else if !kind.is_file() {
                return Err("私有安装清理发现链接或特殊文件，保留待恢复".into());
            }
            let name = CString::new(name.as_bytes()).map_err(error)?;
            if unsafe {
                libc::unlinkat(
                    self.fd().unwrap().as_raw_fd(),
                    name.as_ptr(),
                    if kind.is_dir() { libc::AT_REMOVEDIR } else { 0 },
                )
            } != 0
            {
                return Err(error(std::io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}
fn same_directory(path: PathBuf, expected: &fs::File) -> Result<()> {
    let actual = fs::symlink_metadata(path).map_err(error)?;
    let expected = expected.metadata().map_err(error)?;
    if !actual.is_dir() || actual.dev() != expected.dev() || actual.ino() != expected.ino() {
        return Err("私有安装目录已替换，保留待恢复".into());
    }
    Ok(())
}

pub(super) struct InstallTemporary {
    legacy: Option<tempfile::TempDir>,
    parent: InstallDir,
    name: std::ffi::OsString,
    dir: InstallDir,
}
impl InstallTemporary {
    pub fn new(parent: &InstallDir) -> Result<Self> {
        let temporary = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(parent.path())
            .map_err(error)?;
        let name = temporary
            .path()
            .file_name()
            .ok_or("临时目录名称无效")?
            .to_os_string();
        if parent.is_bound() {
            let _ = temporary.keep();
            let dir = parent.directory(Path::new(&name), false)?;
            // Private cleanup uses retained descriptors, never TempDir's
            // recursively followed mutable pathname.
            Ok(Self {
                legacy: None,
                parent: parent.clone(),
                name,
                dir,
            })
        } else {
            let dir = InstallDir::legacy(temporary.path().into());
            Ok(Self {
                legacy: Some(temporary),
                parent: parent.clone(),
                name,
                dir,
            })
        }
    }
    pub fn dir(&self) -> &InstallDir {
        &self.dir
    }
    pub fn name(&self) -> &std::ffi::OsStr {
        &self.name
    }
    pub fn publish(&mut self, name: &str) -> Result<()> {
        if let Some(fd) = self.parent.fd() {
            same_directory(self.parent.path.join(&self.name), self.dir.fd().unwrap())?;
            pcl_core::file_ops::rename_noreplace_at(fd, &self.name, fd, std::ffi::OsStr::new(name))
                .map_err(|e| format!("无法完成版本安装（目标可能已存在）：{e}"))?;
        } else {
            publish_directory(self.dir.path(), &self.parent.path.join(name))?;
        }
        if let Some(temporary) = self.legacy.take() {
            let _ = temporary.keep();
        }
        Ok(())
    }
    pub fn close(self) -> Result<()> {
        match self.legacy {
            Some(temporary) => temporary.close().map_err(error),
            None => {
                same_directory(self.parent.path.join(&self.name), self.dir.fd().unwrap())?;
                self.parent.remove_directory(Path::new(&self.name))
            }
        }
    }
}

/// Apply only in a new scoped worker. Children and its download threads inherit
/// this inode-based write boundary; the caller keeps its normal permissions.
pub(super) fn confine_writes(root: &fs::File) -> Result<()> {
    #[repr(C)]
    struct Ruleset {
        handled_access_fs: u64,
    }
    #[repr(C, packed)]
    struct Beneath {
        allowed_access: u64,
        parent_fd: i32,
    }
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0,
            1,
        )
    };
    if abi < 3 {
        let detail = if abi < 0 {
            error(std::io::Error::last_os_error())
        } else {
            format!("当前 ABI 为 {abi}")
        };
        return Err(format!("私有整合包安装需要启用 Linux Landlock ABI 3（Linux 6.2 或更新）；当前能力不可用：{detail}"));
    }
    let writes = (1 << 1)
        | (1 << 4)
        | (1 << 5)
        | (1 << 6)
        | (1 << 7)
        | (1 << 8)
        | (1 << 9)
        | (1 << 10)
        | (1 << 11)
        | (1 << 12)
        | (1 << 13)
        | (1 << 14);
    let rules = Ruleset {
        handled_access_fs: writes,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &rules,
            std::mem::size_of::<Ruleset>(),
            0,
        )
    };
    if fd < 0 {
        return Err(format!(
            "无法建立私有安装写入边界：{}",
            std::io::Error::last_os_error()
        ));
    }
    let ruleset = unsafe { fs::File::from_raw_fd(fd as i32) };
    let beneath = Beneath {
        allowed_access: writes,
        parent_fd: root.as_raw_fd(),
    };
    if unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            1,
            &beneath,
            0,
        )
    } != 0
        || unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        || unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0) } != 0
    {
        return Err(format!(
            "无法启用私有安装写入边界（Landlock）：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}
