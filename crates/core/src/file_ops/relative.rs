//! Retained-parent namespace primitive. No canonicalization, proc path, mutable
//! parent pathname, process-wide cwd, replacement flag or copy fallback.
#[cfg(not(target_os = "windows"))]
use std::ffi::OsStr;
use std::{fs::File, io};

fn invalid_name() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Expected one literal leaf name",
    )
}

fn directory(file: &File) -> io::Result<()> {
    if file.metadata()?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Rename parent is not a directory",
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename(
    source_parent: &File,
    source: &OsStr,
    target_parent: &File,
    target: &OsStr,
) -> io::Result<()> {
    use std::{ffi::CString, os::fd::AsRawFd, os::unix::ffi::OsStrExt};
    fn leaf(name: &OsStr) -> io::Result<CString> {
        let bytes = name.as_bytes();
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&b'/')
            || bytes.contains(&b'\\')
        {
            return Err(invalid_name());
        }
        CString::new(bytes).map_err(|_| invalid_name())
    }
    let source = leaf(source)?;
    let target = leaf(target)?;
    directory(source_parent)?;
    directory(target_parent)?;
    #[cfg(target_os = "macos")]
    macos::absent(target_parent, &target)?;
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::renameat2(
            source_parent.as_raw_fd(),
            source.as_ptr(),
            target_parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            source_parent.as_raw_fd(),
            source.as_ptr(),
            target_parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if status == 0 {
        #[cfg(target_os = "macos")]
        macos::published(source_parent, &source)?;
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{ffi::CStr, fs::File, io, mem::MaybeUninit, os::fd::AsRawFd};

    fn exists(parent: &File, name: &CStr) -> io::Result<bool> {
        let mut metadata = MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            Ok(true)
        } else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }

    pub(super) fn absent(parent: &File, name: &CStr) -> io::Result<()> {
        // Darwin explicitly permits same-vnode/case-only no-ops even with
        // RENAME_EXCL on case-insensitive filesystems. Reject known aliases
        // before the syscall. EXCL still protects against a racing publisher:
        // this check is never a substitute for the kernel's no-replace flag.
        if exists(parent, name)? {
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Rename destination already exists",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn published(parent: &File, name: &CStr) -> io::Result<()> {
        // A target hard-link alias inserted after the precheck can make EXCL
        // report success without moving the source. Do not bless that no-op.
        // A concurrently recreated source also fails closed. The rename may
        // already have committed in that case; the caller must reconcile its
        // journal, just as after a failed post-publication directory sync.
        if exists(parent, name)? {
            Err(io::Error::other(
                "Rename needs reconciliation: source entry remains",
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "windows")]
#[path = "relative_windows.rs"]
mod windows;
#[cfg(target_os = "windows")]
pub(super) use windows::rename;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub(super) fn rename(_: &File, _: &OsStr, _: &File, _: &OsStr) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Retained-parent no-replace rename is unavailable on this platform",
    ))
}

#[cfg(test)]
#[path = "relative_tests.rs"]
mod tests;
