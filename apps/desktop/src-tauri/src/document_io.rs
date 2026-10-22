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
pub(crate) fn workspace_lease(root: &Path) -> Result<fs::File, String> {
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
    let lease = options.open(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Another launcher is updating RH Code".into());
        }
    }
    Ok(lease)
}
