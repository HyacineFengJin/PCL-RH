//! Atomic namespace operations, not a directory-authority or transaction layer.
//! The retained-parent variant avoids resolving mutable parent paths. Callers
//! still bind/authorize parents, verify source identities and own durability,
//! journals and conflict receipts.
use std::{ffi::OsStr, fs::File, io, path::Path};

#[path = "file_ops/hard_link.rs"]
mod hard_link;
#[path = "file_ops/relative.rs"]
mod relative;

/// Create a hard link between two retained directories without replacement.
/// Both names are literal leaves and the source must be a regular file. No
/// mutable parent path, copy, source removal or permission repair is used.
/// This is not a transaction: callers own source hashes, authority and actual
/// durability. A failed postcheck may follow publication; preserve evidence.
pub fn hard_link_noreplace_at(
    source_parent: &File,
    source: &OsStr,
    target_parent: &File,
    target: &OsStr,
) -> io::Result<()> {
    hard_link::create(source_parent, source, target_parent, target)
}

/// Rename one entry between retained parent directories without replacement.
/// Both names must be single leaf names, never paths. Neither parent's current
/// pathname is resolved, and an existing destination (including a link) fails.
/// Cross-volume moves and unsupported filesystem semantics fail without a
/// copy/delete or replacing fallback. A source link is moved, never followed.
///
/// The caller still owns source identity checks, parent admission and durable
/// transaction recovery. This operation neither checks an expected inode nor
/// flushes directory metadata. In particular, a retained directory may have
/// been moved outside the caller's intended tree; revalidate its graph before
/// publication. Windows also rejects ambiguous DOS leaf syntax. Windows and
/// macOS guard against native same-file alias/no-op exceptions. A concurrently
/// recreated source reports a conflict after possible publication; an error
/// must therefore be reconciled using the caller's transaction state.
pub fn rename_noreplace_at(
    source_parent: &File,
    source: &OsStr,
    target_parent: &File,
    target: &OsStr,
) -> io::Result<()> {
    relative::rename(source_parent, source, target_parent, target)
}

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
