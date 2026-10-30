use super::*;
use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
};

#[cfg(target_os = "linux")]
pub(super) const SOURCE_KIND: SourceKind = SourceKind::AnonymousFd;
#[cfg(target_os = "linux")]
pub(super) const INITIAL_LINKS: u64 = 0;
#[cfg(target_os = "macos")]
pub(super) const SOURCE_KIND: SourceKind = SourceKind::NamedLease;
#[cfg(target_os = "macos")]
pub(super) const INITIAL_LINKS: u64 = 1;

#[cfg(target_os = "linux")]
pub(super) fn new(parent: &Directory) -> io::Result<(File, Option<Leaf>)> {
    let fd = unsafe {
        libc::openat(
            parent.file().as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
            0o600 as libc::c_int,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((unsafe { File::from_raw_fd(fd) }, None))
}

#[cfg(target_os = "macos")]
pub(super) fn new(parent: &Directory) -> io::Result<(File, Option<Leaf>)> {
    for _ in 0..32 {
        let mut entropy = [0u8; 16];
        if unsafe { libc::getentropy(entropy.as_mut_ptr().cast(), entropy.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let name = format!(
            ".pcl-stage-{}.lease",
            entropy
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let leaf = Leaf::new(&name)?;
        let name = CString::new(name).unwrap();
        let fd = unsafe {
            libc::openat(
                parent.file().as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_int,
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(error);
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok((file, Some(leaf)));
    }
    Err(changed("Could not allocate a unique private stage lease"))
}

#[cfg(target_os = "linux")]
pub(super) fn publish(stage: &Stage, target: &Directory, leaf: &Leaf) -> io::Result<()> {
    let target_name = CString::new(leaf.as_os_str().as_bytes()).unwrap();
    if unsafe {
        libc::linkat(
            stage.file.as_raw_fd(),
            c"".as_ptr(),
            target.file().as_raw_fd(),
            target_name.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    } == 0
    {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if !matches!(
        error.raw_os_error(),
        Some(libc::EPERM | libc::EINVAL | libc::ENOENT)
    ) {
        return Err(error);
    }
    // Ordinary users may lack CAP_DAC_READ_SEARCH for AT_EMPTY_PATH. The proc
    // symlink adapts only this still-held FD; the destination stays dir-relative.
    // Never use a mutable stage pathname or a copy/delete fallback.
    if metadata(&stage.file)?.links != 0 {
        return Err(changed(
            "Anonymous stage acquired a link during publication",
        ));
    }
    let source = CString::new(format!("/proc/self/fd/{}", stage.file.as_raw_fd())).unwrap();
    if unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            target.file().as_raw_fd(),
            target_name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
pub(super) fn publish(stage: &Stage, target: &Directory, leaf: &Leaf) -> io::Result<()> {
    let source = CString::new(stage.source_name.as_ref().unwrap().as_os_str().as_bytes()).unwrap();
    let target_name = CString::new(leaf.as_os_str().as_bytes()).unwrap();
    // This remains a named source operation. Generic postchecks explicitly
    // detect alias no-ops, source substitutions and publication of wrong bytes.
    if unsafe {
        libc::renameatx_np(
            stage.parent.file().as_raw_fd(),
            source.as_ptr(),
            target.file().as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn sync_parent(parent: &Directory) -> io::Result<()> {
    parent.sync()
}
