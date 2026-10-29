//! Atomic namespace operations, not a directory-authority or transaction layer.
//! Callers still own/bind both parents and verify source identities. These path
//! adapters cannot replace retained handles, journals or conflict receipts.
use std::{io, path::Path};

/// Atomically rename a file or directory only when the destination entry is
/// absent. Never emulate this with exists()+rename(), copy/delete, or a fallback
/// that could overwrite a concurrent publisher. Cross-volume moves fail.
pub fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    if source == target {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Source and target must differ",
        ));
    }
    native_rename_noreplace(source, target)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid source path"))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid target path"))?;
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let status = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "windows")]
fn native_rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut units: Vec<_> = path.as_os_str().encode_wide().collect();
        if units.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid rename path",
            ));
        }
        units.push(0);
        Ok(units)
    }
    let source = wide(source)?;
    let target = wide(target)?;
    // No REPLACE_EXISTING and no COPY_ALLOWED: directories/files remain on one
    // volume and an already-existing destination is never changed.
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn native_rename_noreplace(_source: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Atomic no-replace rename is unavailable on this platform",
    ))
}

#[cfg(test)]
#[path = "file_ops_tests.rs"]
mod tests;
