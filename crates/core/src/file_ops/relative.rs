//! Retained-parent namespace primitive. No canonicalization, proc path, mutable
//! parent pathname, process-wide cwd, replacement flag or copy fallback.
use std::{ffi::OsStr, fs::File, io};

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
        Ok(())
    } else {
        Err(io::Error::last_os_error())
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
