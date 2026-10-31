//! Original Linux resource-operation FD boundary and wire-compatible journal IO.
use super::{new_id, token_for, MAX_JOURNAL_BYTES};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Path,
};
pub(super) struct Dir {
    file: File,
}

fn c_name(name: &str) -> Result<CString, String> {
    CString::new(name).map_err(|_| "文件名含有无效字符".into())
}

fn io_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}

impl Dir {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let name = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "目录路径含有无效字符".to_string())?;
        // O_NOFOLLOW applies to the root itself; all its children are opened
        // relative to this descriptor with the same flag.
        let fd = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io_error("无法打开资源目录"));
        }
        use std::os::fd::FromRawFd;
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
        })
    }

    pub(super) fn child(&self, name: &str) -> Result<Self, String> {
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io_error("无法打开操作目录"));
        }
        use std::os::fd::FromRawFd;
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
        })
    }

    pub(super) fn optional_child(&self, name: &str) -> Result<Option<Self>, String> {
        if self.metadata(name)?.is_none() {
            return Ok(None);
        }
        self.child(name).map(Some)
    }

    pub(super) fn create_child(&self, name: &str) -> Result<Self, String> {
        let name_c = c_name(name)?;
        if unsafe { libc::mkdirat(self.file.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
            return Err(io_error("无法创建操作目录"));
        }
        self.sync()?;
        self.child(name)
    }

    pub(super) fn ensure_child(&self, name: &str) -> Result<Self, String> {
        match self.optional_child(name)? {
            Some(dir) => Ok(dir),
            None => self.create_child(name),
        }
    }

    pub(super) fn metadata(&self, name: &str) -> Result<Option<libc::stat>, String> {
        let name = c_name(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(unsafe { stat.assume_init() }));
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(format!("无法检查文件：{err}"))
        }
    }

    pub(super) fn regular_file(&self, name: &str) -> Result<File, String> {
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io_error("无法打开资源文件"));
        }
        use std::os::fd::FromRawFd;
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("只允许操作普通文件".into());
        }
        Ok(file)
    }

    pub(super) fn create_file(&self, name: &str) -> Result<File, String> {
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io_error("无法创建暂存文件"));
        }
        use std::os::fd::FromRawFd;
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn token(&self, name: &str) -> Result<String, String> {
        token_for(&self.regular_file(name)?)
    }

    pub(super) fn absent(&self, name: &str) -> Result<(), String> {
        if self.metadata(name)?.is_some() {
            Err(format!("已有同名文件，无法操作：{name}"))
        } else {
            Ok(())
        }
    }

    pub(super) fn sync(&self) -> Result<(), String> {
        self.file
            .sync_all()
            .map_err(|e| format!("无法保存目录：{e}"))
    }

    pub(super) fn names(&self) -> Result<Vec<String>, String> {
        // /proc/self/fd follows this already opened descriptor, rather than a
        // potentially replaced resource pathname. Entries themselves are never
        // opened through /proc: every operation uses openat with O_NOFOLLOW.
        let mut names = Vec::new();
        for entry in fs::read_dir(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
            .map_err(|e| e.to_string())?
        {
            let name = entry
                .map_err(|e| e.to_string())?
                .file_name()
                .into_string()
                .map_err(|_| "操作目录含有无效文件名".to_string())?;
            names.push(name);
        }
        names.sort();
        Ok(names)
    }

    pub(super) fn remove_file(
        &self,
        name: &str,
        _expected: &File,
        _evidence: &FileEvidence,
    ) -> Result<(), String> {
        let name = c_name(name)?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io_error("无法移除暂存文件"));
        }
        self.sync()
    }

    pub(super) fn remove_empty_dir(&self, name: &str, _expected: &Dir) -> Result<(), String> {
        let name = c_name(name)?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0
        {
            return Err(io_error("无法收起操作目录"));
        }
        self.sync()
    }

    pub(super) fn same(&self, other: &Self) -> Result<bool, String> {
        let a = self.file.metadata().map_err(|e| e.to_string())?;
        let b = other.file.metadata().map_err(|e| e.to_string())?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
}

pub(super) fn rename_new(from: &Dir, source: &str, to: &Dir, target: &str) -> Result<(), String> {
    let source = c_name(source)?;
    let target = c_name(target)?;
    if unsafe {
        libc::renameat2(
            from.file.as_raw_fd(),
            source.as_ptr(),
            to.file.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(io_error("无法移动资源文件"));
    }
    from.sync()?;
    to.sync()
}

pub(super) fn publish_journal(dir: &Dir, data: &[u8]) -> Result<(), String> {
    // Refuse before creating the next file or replacing a readable old record.
    if data.len() > MAX_JOURNAL_BYTES {
        return Err("资源恢复记录过大".into());
    }
    let next = format!("journal-{}.next", new_id());
    let mut file = dir.create_file(&next)?;
    file.write_all(data).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let next_c = c_name(&next)?;
    let journal_c = c_name("journal.json")?;
    if let Some(stat) = dir.metadata("journal.json")? {
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err("资源恢复记录不是普通文件".into());
        }
    }
    // Only the service's fixed journal filename is atomically replaced. User
    // resources always use RENAME_NOREPLACE above.
    if unsafe {
        libc::renameat(
            dir.file.as_raw_fd(),
            next_c.as_ptr(),
            dir.file.as_raw_fd(),
            journal_c.as_ptr(),
        )
    } != 0
    {
        return Err(io_error("无法保存资源恢复记录"));
    }
    dir.sync()
}

pub(super) fn open_source_parent(path: &Path) -> Result<Dir, String> {
    let absolute = Dir::open(Path::new("/"))?;
    if path == Path::new("/") {
        Ok(absolute)
    } else {
        super::descend(
            &absolute,
            &super::relative_string(path.strip_prefix("/").map_err(|e| e.to_string())?)?,
            false,
        )
    }
}
pub(super) fn source_identity(file: &File) -> Result<(u64, u64), String> {
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    Ok((metadata.dev(), metadata.ino()))
}
pub(super) fn journal_bytes(dir: &Dir) -> Result<Option<Vec<u8>>, String> {
    if dir.metadata("journal.json")?.is_none() {
        return Ok(None);
    }
    let file = dir.regular_file("journal.json")?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_JOURNAL_BYTES as u64 {
        return Err("资源恢复记录过大".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err("资源恢复记录过大".into());
    }
    Ok(Some(bytes))
}

pub(super) type Copy = File;
pub(super) type CopyGuard = ();
pub(super) fn copy_guard(_root: &Dir) -> Result<CopyGuard, String> {
    Ok(())
}
pub(super) fn create_copy(
    _guard: &CopyGuard,
    target: &Dir,
    name: &str,
    _max: u64,
) -> Result<Copy, String> {
    target.create_file(name)
}
pub(super) fn complete_copy(copy: Copy, target: &Dir, name: &str) -> Result<String, String> {
    drop(copy);
    target.token(name)
}
pub(super) fn check_copy_cleanup(_guard: &CopyGuard) -> Result<(), String> {
    Ok(())
}

pub(super) type FileEvidence = ();
pub(super) fn file_evidence(_file: &File) -> Result<FileEvidence, String> {
    Ok(())
}
