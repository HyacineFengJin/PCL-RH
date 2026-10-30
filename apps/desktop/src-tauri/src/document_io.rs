//! Bounded byte reads and atomic file replacement shared by plugin control and
//! project documents. This layer has no knowledge of project schemas or plugins.
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
fn nonce() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
pub(crate) fn safe_file(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("Expected a regular file; retained existing path".into()),
    }
}
pub(crate) fn atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_with_permissions(path, bytes, None)
}
pub(crate) fn atomic_with_permissions(
    path: &Path,
    bytes: &[u8],
    permissions: Option<fs::Permissions>,
) -> Result<(), String> {
    use std::io::Write;
    safe_file(path)?;
    let tmp = path.with_file_name(format!(".rh-{}.new", nonce()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)
                .map_err(|e| e.to_string())?;
        }
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        safe_file(path)?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub(crate) fn read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    safe_file(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut bytes = vec![];
    options
        .open(path)
        .map_err(|e| e.to_string())?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        Err("File exceeds the supported size".into())
    } else {
        Ok(bytes)
    }
}

/// Every writer of the historical workspace uses this same nonblocking lease.
/// The in-process gate is shared separately by the Code and product adapters.
/// This is a cooperative writer lock, not a directory capability or a guarantee
/// about the pathname-based document reader and replacement implementation.
pub(crate) struct WorkspaceLease(fs::File);

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        // Unlock explicitly: closing this handle alone need not release the
        // lease while another handle to the same open file remains alive.
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{Storage::FileSystem::UnlockFileEx, System::IO::OVERLAPPED};
            let mut overlapped = OVERLAPPED::default();
            unsafe {
                UnlockFileEx(self.0.as_raw_handle(), 0, 1, 0, &mut overlapped);
            }
        }
    }
}

pub(crate) fn workspace_lease(root: &Path) -> Result<WorkspaceLease, String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let path = root.join("store.lock");
    safe_file(&path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let lease = options.open(path).map_err(|e| e.to_string())?;
    if !lease.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Expected a regular workspace lock file".into());
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Another launcher is updating RH Code".into());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
        use windows_sys::Win32::{
            Storage::FileSystem::{
                LockFileEx, FILE_ATTRIBUTE_REPARSE_POINT, LOCKFILE_EXCLUSIVE_LOCK,
                LOCKFILE_FAIL_IMMEDIATELY,
            },
            System::IO::OVERLAPPED,
        };
        if lease
            .metadata()
            .map_err(|e| e.to_string())?
            .file_attributes()
            & FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err("Expected a regular workspace lock file".into());
        }
        // All historical-workspace writers contend on the same byte of this
        // reserved lock file. The handle is synchronous and this never waits.
        let mut overlapped = OVERLAPPED::default();
        if unsafe {
            LockFileEx(
                lease.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        } == 0
        {
            return Err("Another launcher is updating RH Code".into());
        }
    }
    #[cfg(not(any(unix, windows)))]
    return Err("Workspace writer leases are unsupported on this platform".into());
    Ok(WorkspaceLease(lease))
}

#[cfg(test)]
mod workspace_lease_tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .find(|root| {
                    root.join("crates/core/src").is_dir() && root.join("Cargo.lock").is_file()
                })
                .expect("fixture must remain inside the PCL RH workspace");
            Self(root.join("work/document-io-lock-tests").join(nonce()))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn workspace_writer_lease_is_nonblocking_and_reusable() {
        let fixture = Fixture::new();
        let first = workspace_lease(&fixture.0).unwrap();
        assert!(workspace_lease(&fixture.0).is_err());
        drop(first);
        let _next = workspace_lease(&fixture.0).unwrap();
    }

    #[test]
    fn workspace_writer_lease_unlocks_with_duplicate_still_open() {
        let fixture = Fixture::new();
        let lease = workspace_lease(&fixture.0).unwrap();
        let duplicate = lease.0.try_clone().unwrap();
        assert!(workspace_lease(&fixture.0).is_err());
        drop(lease);
        let _next = workspace_lease(&fixture.0).unwrap();
        drop(duplicate);
    }
}
