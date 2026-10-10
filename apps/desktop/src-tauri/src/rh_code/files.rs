//! Bounded project reads and atomic replacements. No Agent receives a path
//! through this module; native snapshot bytes are the execution boundary.
use super::{id, Project};
use std::{fs, path::Path};
pub(super) fn safe_file(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("Expected a regular file; retained existing path".into()),
    }
}
pub(super) fn atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_with_permissions(path, bytes, None)
}
pub(super) fn atomic_with_permissions(
    path: &Path,
    bytes: &[u8],
    permissions: Option<fs::Permissions>,
) -> Result<(), String> {
    use std::io::Write;
    safe_file(path)?;
    let tmp = path.with_file_name(format!(".rh-{}.new", id()));
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
pub(super) fn read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
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
pub(super) fn inventory(p: &Project) -> Result<Vec<String>, String> {
    let root = fs::canonicalize(&p.folder).map_err(|e| e.to_string())?;
    let mut paths = vec![];
    let mut pending = vec![(root.clone(), 0)];
    let mut scanned = 0;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 12 {
            return Err("Project tree exceeds supported depth; choose a narrower folder".into());
        }
        for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
            scanned += 1;
            if scanned > 12000 {
                return Err("Project inventory is too large; choose a narrower folder".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                || ["node_modules", "target", "build", "dist"].contains(&name.as_str())
            {
                continue;
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file() {
                paths.push(
                    entry
                        .path()
                        .strip_prefix(&root)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
            if paths.len() > 1000 {
                return Err("Project contains more than 1000 visible files".into());
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// Publish a full snapshot with one directory rename. A nonempty destination
/// is rejected even if another process adds a file after the initial check.
/// No merge, deletion or overwrite of handwritten source happens here.
pub(super) fn adopt(p: &Project, source: &serde_json::Value) -> Result<(), String> {
    use super::hash;
    use base64::Engine;
    let folder = fs::canonicalize(&p.folder).map_err(|e| e.to_string())?;
    if folder != Path::new(&p.folder)
        || !folder.is_dir()
        || fs::read_dir(&folder)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(
            "Adopt source into an empty project folder; existing files were retained".into(),
        );
    }
    let entries = source["files"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 300)
        .ok_or("Invalid source artifact")?;
    let mut files = vec![];
    let mut paths = std::collections::HashSet::new();
    let mut total = 0;
    for entry in entries {
        let rel = entry["path"].as_str().ok_or("Invalid artifact path")?;
        if Path::new(rel).is_absolute()
            || rel
                .split(['/', '\\'])
                .any(|part| part.is_empty() || part.starts_with('.'))
            || !paths.insert(rel)
        {
            return Err("Invalid artifact path".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(entry["data"].as_str().ok_or("Missing artifact bytes")?)
            .map_err(|_| "Invalid artifact encoding")?;
        total += bytes.len();
        if total > 256000 || entry["sha256"].as_str() != Some(&hash(&bytes)) {
            return Err("Source artifact exceeds its budget or fails verification".into());
        }
        files.push((rel.to_owned(), bytes));
    }
    let stage = folder
        .parent()
        .ok_or("Choose a project folder below a parent directory")?
        .join(format!(".rh-source-{}", id()));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let result = (|| {
        fs::set_permissions(
            &stage,
            fs::metadata(&folder)
                .map_err(|e| e.to_string())?
                .permissions(),
        )
        .map_err(|e| e.to_string())?;
        for (rel, bytes) in files {
            let path = stage.join(rel);
            fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            atomic(&path, &bytes)?;
        }
        // On the current Linux carrier rename replaces an empty directory only.
        // Other carriers must supply their own publication primitive.
        #[cfg(not(unix))]
        {
            return Err("Source adoption is unavailable on this carrier".into());
        }
        #[cfg(unix)]
        {
            fs::rename(&stage, &folder)
                .map_err(|e| format!("Project folder changed; source was not adopted: {e}"))
        }
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}
