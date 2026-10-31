//! Original Linux openat2/O_TMPFILE backend.
use super::super::{cstring, os_error};
use super::*;
use std::{
    ffi::{CStr, CString},
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};
pub(crate) struct Dir(pub(crate) File);
impl Dir {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err("导入源和游戏目录必须是绝对路径".into());
        }
        let slash = cstring("/")?;
        let fd = unsafe {
            libc::open(
                slash.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(os_error("无法打开导入目录"));
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => {
                    // Absolute ancestors may cross a legitimate mounted root. All
                    // descendants of the captured root use openat2 NO_XDEV below.
                    let name = CString::new(name.as_bytes()).map_err(error)?;
                    let fd = unsafe {
                        libc::openat(
                            dir.0.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                        )
                    };
                    if fd < 0 {
                        return Err(os_error("目录祖先已变化或包含符号链接"));
                    }
                    dir = Self(unsafe { File::from_raw_fd(fd) });
                }
                _ => return Err("目录路径不能包含相对路径跳转".into()),
            }
        }
        Ok(dir)
    }
    pub(crate) fn duplicate(&self) -> Result<Self> {
        self.0.try_clone().map(Self).map_err(error)
    }
    pub(crate) fn key(&self) -> Result<Key> {
        Ok(Stamp::of(&self.0.metadata().map_err(error)?).key)
    }
    pub(crate) fn sync(&self) -> Result<()> {
        self.0.sync_all().map_err(error)
    }
    pub(crate) fn open_at(&self, name: &str, directory: bool) -> Result<File> {
        component(name)?;
        let name = cstring(name)?;
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
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
            resolve: 0x01 | 0x02 | 0x04 | 0x08,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.0.as_raw_fd(),
                name.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            ) as i32
        };
        if fd < 0 {
            return Err(os_error(
                "路径不存在、包含链接或跨挂载点（需要 Linux 5.6 以上）",
            ));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !directory && !file.metadata().map_err(error)?.is_file() {
            return Err("导入只允许普通文件与目录".into());
        }
        Ok(file)
    }
    pub(crate) fn child(&self, name: &str) -> Result<Self> {
        self.open_at(name, true).map(Self)
    }
    pub(crate) fn regular(&self, name: &str) -> Result<File> {
        self.open_at(name, false)
    }
    pub(crate) fn stat(&self, name: &str) -> Result<Option<Stamp>> {
        component(name)?;
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
            Ok(Some(stat_stamp(&unsafe { stat.assume_init() })))
        } else {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error(e))
            }
        }
    }
    pub(crate) fn parent(&self, path: &str) -> Result<(Self, String)> {
        let parts = relative(path)?;
        let mut dir = self.duplicate()?;
        for part in &parts[..parts.len() - 1] {
            dir = dir.child(part)?;
        }
        Ok((dir, parts[parts.len() - 1].into()))
    }
    pub(crate) fn optional_parent(&self, path: &str) -> Result<Option<(Self, String)>> {
        let parts = relative(path)?;
        let mut dir = self.duplicate()?;
        for part in &parts[..parts.len() - 1] {
            if dir.stat(part)?.is_none() {
                return Ok(None);
            }
            dir = dir.child(part)?;
        }
        Ok(Some((dir, parts[parts.len() - 1].into())))
    }
    pub(crate) fn at(&self, path: &str) -> Result<Self> {
        let mut dir = self.duplicate()?;
        for part in relative(path)? {
            dir = dir.child(part)?;
        }
        Ok(dir)
    }
    pub(crate) fn file(&self, path: &str) -> Result<File> {
        let (dir, name) = self.parent(path)?;
        dir.regular(&name)
    }
    pub(crate) fn mkdir(&self, name: &str) -> Result<Self> {
        component(name)?;
        let n = cstring(name)?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), n.as_ptr(), 0o700) } != 0 {
            return Err(os_error("无法创建导入目录"));
        }
        self.sync()?;
        self.child(name)
    }
    pub(crate) fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_some() {
            self.child(name).map(Some)
        } else {
            Ok(None)
        }
    }
    pub(crate) fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(d) => Ok(d),
            None => self.mkdir(name),
        }
    }
    pub(crate) fn anonymous(&self) -> Result<File> {
        let dot = cstring(".")?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(os_error("文件系统不支持安全匿名导入暂存文件"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(crate) fn create(&self, name: &str) -> Result<File> {
        component(name)?;
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(os_error("无法创建导入锁文件"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(crate) fn unlink(&self, name: &str, directory: bool) -> Result<()> {
        component(name)?;
        let name = cstring(name)?;
        if unsafe {
            libc::unlinkat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } != 0
        {
            return Err(os_error("无法清理导入文件"));
        }
        self.sync()
    }
    pub(crate) fn names(&self) -> Result<Vec<String>> {
        let dot = cstring(".")?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(os_error("无法读取导入目录"));
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe { libc::close(fd) };
            return Err(os_error("无法读取导入目录"));
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
                    return Err(os_error("读取导入目录失败"));
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| "目录包含无效 UTF-8 文件名")?
                .to_owned();
            component(&name)?;
            names.push(name);
            if names.len() > MAX_FILES * 4 {
                return Err("导入目录节点数量过多".into());
            }
        }
        names.sort();
        Ok(names)
    }
}

pub(super) fn stat_stamp(s: &libc::stat) -> Stamp {
    Stamp {
        key: Key {
            dev: s.st_dev,
            ino: s.st_ino,
        },
        mode: s.st_mode,
        size: s.st_size.max(0) as u64,
        mtime: (s.st_mtime, s.st_mtime_nsec),
        ctime: (s.st_ctime, s.st_ctime_nsec),
    }
}
pub(crate) fn file_stamp(file: &File) -> Result<Stamp> {
    Ok(Stamp::of(&file.metadata().map_err(error)?))
}
pub(crate) fn is_anonymous(file: &File) -> Result<bool> {
    Ok(file.metadata().map_err(error)?.nlink() == 0)
}
pub(crate) fn require_mutation_support() -> Result<()> {
    Ok(())
}
pub(crate) struct ImportLock(pub(crate) File);
impl Drop for ImportLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
pub(crate) fn lock(store: &Dir) -> Result<ImportLock> {
    let file = if store.stat(".lock")?.is_some() {
        store.regular(".lock")?
    } else {
        store.create(".lock")?
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("其他进程正在导入或恢复实例，请稍后重试".into());
    }
    Ok(ImportLock(file))
}

pub(crate) fn link_anonymous(file: &mut StageFile, dir: &Dir, name: &str) -> Result<()> {
    component(name)?;
    let source = cstring(&format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    let name = cstring(name)?;
    if unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            dir.0.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    } != 0
    {
        return Err(os_error("无法发布已登记的导入暂存文件"));
    }
    dir.sync()
}

pub(crate) fn link_file(from: &Dir, source: &str, to: &Dir, target: &str) -> Result<()> {
    component(source)?;
    component(target)?;
    let source = cstring(source)?;
    let target = cstring(target)?;
    if unsafe {
        libc::linkat(
            from.0.as_raw_fd(),
            source.as_ptr(),
            to.0.as_raw_fd(),
            target.as_ptr(),
            0,
        )
    } != 0
    {
        return Err(os_error("目标已存在或无法发布导入文件"));
    }
    to.sync()
}
pub(crate) fn move_new(from: &Dir, source: &str, to: &Dir, target: &str) -> Result<()> {
    component(source)?;
    component(target)?;
    let source = cstring(source)?;
    let target = cstring(target)?;
    if unsafe {
        libc::renameat2(
            from.0.as_raw_fd(),
            source.as_ptr(),
            to.0.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(os_error("目标已存在或无法发布新实例目录"));
    }
    from.sync()?;
    to.sync()
}

pub(crate) fn replace_journal(operation: &Dir) -> Result<()> {
    if unsafe {
        libc::renameat(
            operation.0.as_raw_fd(),
            cstring("journal.next")?.as_ptr(),
            operation.0.as_raw_fd(),
            cstring("journal.json")?.as_ptr(),
        )
    } != 0
    {
        return Err(os_error("无法保存导入恢复状态"));
    }
    operation.sync()
}

pub(crate) type StageFile = File;
pub(crate) fn owns_stage(stage: &StageFile, file: &File) -> Result<bool> {
    Ok(is_anonymous(stage)? && file_stamp(stage)? == file_stamp(file)?)
}
impl Dir {
    pub(crate) fn with_sources(&self) -> Result<Self> {
        self.duplicate()
    }
    pub(crate) fn with_input_sources(&self) -> Result<Self> {
        self.duplicate()
    }
    pub(crate) fn stage_for(&self, _name: &str, _max: u64) -> Result<StageFile> {
        self.anonymous()
    }
}

impl Dir {
    pub(crate) fn unlink_snapshot(&self, name: &str, expected: &Snapshot) -> Result<()> {
        if !expected.owned_matches(&hash_file(
            self.regular(name)?,
            super::super::MAX_FILE,
            None,
        )?) {
            return Err(super::super::changed());
        }
        self.unlink(name, false)
    }
    pub(crate) fn unlink_key(&self, name: &str, expected: &Key) -> Result<()> {
        if self.child(name)?.key()? != *expected {
            return Err(super::super::changed());
        }
        self.unlink(name, true)
    }
}

impl Dir {
    pub(crate) fn with_journal(&self) -> Result<Self> {
        self.duplicate()
    }
}

impl Dir {
    pub(crate) fn check_source_cleanup(&self) -> Result<()> {
        Ok(())
    }
    pub(crate) fn reconcile_sources(&self) -> Result<()> {
        Ok(())
    }
}
