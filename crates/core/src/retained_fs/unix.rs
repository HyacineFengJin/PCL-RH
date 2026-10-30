use super::*;
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt;
use std::{
    ffi::{CStr, CString},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
};

pub(super) fn validate_leaf(name: &OsStr) -> io::Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || matches!(bytes, b"." | b"..")
        || bytes.contains(&0)
        || bytes.contains(&b'/')
        || bytes.contains(&b'\\')
    {
        Err(invalid("Expected one literal file name"))
    } else {
        Ok(())
    }
}
fn cstring(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| invalid("Invalid file name"))
}
pub(super) fn absolute_parts(path: &Path) -> io::Result<(std::path::PathBuf, Vec<OsString>)> {
    let mut names = Vec::new();
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => {
                validate_leaf(name)?;
                names.push(name.to_owned());
            }
            _ => return Err(invalid("Invalid absolute directory path")),
        }
    }
    Ok(("/".into(), names))
}
fn owned_fd(fd: i32) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
pub(super) fn open_root(_: &Path) -> io::Result<File> {
    owned_fd(unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}

#[cfg(target_os = "linux")]
fn open(parent: &File, name: &OsStr, flags: i32, mode: u32, policy: Traversal) -> io::Result<File> {
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let name = cstring(name)?;
    let how = OpenHow {
        flags: flags as u64,
        mode: mode as u64,
        resolve: 0x04
            | 0x08
            | if policy == Traversal::SameFilesystem {
                0x01
            } else {
                0
            },
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        owned_fd(fd as i32)
    }
}

#[cfg(target_os = "macos")]
fn filesystem(file: &File) -> io::Result<(u64, [i32; 2])> {
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    // libc intentionally keeps fsid_t's two native i32 words private. Its C
    // representation is fixed; compare the complete opaque identifier, never
    // the mutable mount pathname or a truncated device number.
    let fsid: [i32; 2] = unsafe { std::mem::transmute(stat.f_fsid) };
    Ok((file.metadata()?.dev(), fsid))
}
#[cfg(target_os = "macos")]
fn open(parent: &File, name: &OsStr, flags: i32, mode: u32, policy: Traversal) -> io::Result<File> {
    let before = if policy == Traversal::SameFilesystem {
        Some(filesystem(parent)?)
    } else {
        None
    };
    let name = cstring(name)?;
    let file = owned_fd(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags,
            mode as libc::mode_t,
        )
    })?;
    if before.is_some_and(|expected| filesystem(&file).ok() != Some(expected)) {
        return Err(io::Error::other(
            "Opened entry crosses the admitted filesystem boundary",
        ));
    }
    Ok(file)
}

pub(super) fn open_directory(parent: &File, name: &OsStr, policy: Traversal) -> io::Result<File> {
    open(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
        policy,
    )
}
pub(super) fn reopen_directory(parent: &File, policy: Traversal) -> io::Result<File> {
    open_directory(parent, OsStr::new("."), policy)
}
pub(super) fn create_directory(parent: &File, name: &OsStr, policy: Traversal) -> io::Result<File> {
    let c = cstring(name)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // mkdir/open is not an atomic identity claim on POSIX. No chmod, unlink or
    // cleanup by this name follows; the opened object must pass native policy.
    open_directory(parent, name, policy)
}
pub(super) fn open_regular(parent: &File, name: &OsStr) -> io::Result<File> {
    open(
        parent,
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        0,
        Traversal::SameFilesystem,
    )
}
pub(super) fn create_file(parent: &File, name: &OsStr) -> io::Result<File> {
    open(
        parent,
        name,
        libc::O_RDWR
            | libc::O_CREAT
            | libc::O_EXCL
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK,
        0o600,
        Traversal::SameFilesystem,
    )
}

fn time(seconds: i64, nanos: i64) -> io::Result<Time> {
    if !(0..1_000_000_000).contains(&nanos) {
        return Err(invalid("Invalid native timestamp"));
    }
    Ok(Time {
        seconds,
        nanos: nanos as u32,
    })
}
fn stat_metadata(s: &libc::stat, extended_acl: bool) -> io::Result<Metadata> {
    let kind = match s.st_mode as u32 & libc::S_IFMT as u32 {
        x if x == libc::S_IFDIR as u32 => Kind::Directory,
        x if x == libc::S_IFREG as u32 => Kind::Regular,
        x if x == libc::S_IFLNK as u32 => Kind::Link,
        _ => Kind::Other,
    };
    Ok(Metadata {
        identity: Identity::Unix {
            device: s.st_dev as u64,
            inode: s.st_ino as u64,
        },
        kind,
        len: u64::try_from(s.st_size).map_err(|_| invalid("Negative native file size"))?,
        links: s.st_nlink as u64,
        modified: time(s.st_mtime as i64, s.st_mtime_nsec as i64)?,
        changed: time(s.st_ctime as i64, s.st_ctime_nsec as i64)?,
        security: Security::Unix {
            owner: s.st_uid,
            mode: s.st_mode as u32,
            extended_acl,
        },
        reparse_tag: None,
    })
}

#[cfg(target_os = "macos")]
fn extended_acl(file: &File) -> io::Result<bool> {
    // Darwin extended ACLs can grant access even when POSIX mode is 0700.
    // A private object with any extended ACL is conservatively rejected.
    unsafe extern "C" {
        fn acl_get_fd_np(fd: i32, acl_type: u32) -> *mut std::ffi::c_void;
        fn acl_get_entry(
            acl: *mut std::ffi::c_void,
            entry_id: i32,
            entry: *mut *mut std::ffi::c_void,
        ) -> i32;
        fn acl_free(object: *mut std::ffi::c_void) -> i32;
    }
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) }; // ACL_TYPE_EXTENDED
    if acl.is_null() {
        return Err(io::Error::last_os_error());
    }
    struct Acl(*mut std::ffi::c_void);
    impl Drop for Acl {
        fn drop(&mut self) {
            unsafe {
                acl_free(self.0);
            }
        }
    }
    let acl = Acl(acl);
    let mut entry = std::ptr::null_mut();
    let status = unsafe { acl_get_entry(acl.0, 0, &mut entry) }; // ACL_FIRST_ENTRY
    if status == 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINVAL) {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

pub(super) fn metadata(file: &File) -> io::Result<Metadata> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    let acl = false; // Linux mode mask denies named ACL grants when group bits are zero.
    #[cfg(target_os = "macos")]
    let acl = extended_acl(file)?;
    stat_metadata(&unsafe { stat.assume_init() }, acl)
}
pub(super) fn entry(parent: &File, name: &OsStr) -> io::Result<Option<Metadata>> {
    let name = cstring(name)?;
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        };
    }
    // No private-policy evidence is inferred from this pathname-only snapshot
    // on Darwin. Reopen the held ordinary file before private admission.
    #[cfg(target_os = "linux")]
    let acl = false;
    #[cfg(target_os = "macos")]
    let acl = true;
    stat_metadata(&unsafe { stat.assume_init() }, acl).map(Some)
}
pub(super) fn require_owned(metadata: &Metadata) -> io::Result<()> {
    match metadata.security {
        Security::Unix { owner, .. } if owner == unsafe { libc::geteuid() } => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Object is not owned by the current user",
        )),
    }
}
pub(super) fn require_private(metadata: &Metadata) -> io::Result<()> {
    require_owned(metadata)?;
    match metadata.security {
        Security::Unix {
            mode,
            extended_acl: false,
            ..
        } if mode & 0o077 == 0 => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Object permissions do not establish private access",
        )),
    }
}

pub(super) fn names(parent: &File, limit: usize) -> io::Result<Vec<OsString>> {
    use std::os::fd::IntoRawFd;
    let file = reopen_directory(parent, Traversal::AllowMounts)?;
    let fd = file.into_raw_fd();
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = Stream(stream);
    let mut names = Vec::new();
    loop {
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(0) {
                break;
            } else {
                return Err(error);
            }
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if names.len() >= limit {
            return Err(invalid("Directory entry count exceeds its limit"));
        }
        names.push(OsStr::from_bytes(name).to_owned());
    }
    names.sort();
    Ok(names)
}
pub(super) fn open_lock(parent: &File, name: &OsStr) -> io::Result<File> {
    open(
        parent,
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        0o600,
        Traversal::SameFilesystem,
    )
}
pub(super) fn lock(file: &File) -> io::Result<()> {
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
pub(super) fn unlock(file: &File) {
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}
pub(super) fn sync_directory(file: &File) -> io::Result<()> {
    file.sync_all()
}
