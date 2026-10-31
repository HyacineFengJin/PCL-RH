//! Original Linux held-FD memory-demand scan.
use super::{CHANGED, MAX_MOD_ENTRIES};
use crate::launcher_local::filesystem::Dir;
use pcl_core::content_scope::{self, ContentScope, IsolationPolicy, MarkerState};
use std::{
    fs,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
fn fd_path(directory: &Dir) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", directory.0.as_raw_fd()))
}

fn content_scope(instance: &Dir, isolation: IsolationPolicy) -> Result<ContentScope> {
    content_scope::resolve_scope(isolation, |name| {
        match fs::symlink_metadata(fd_path(instance).join(name)) {
            Ok(metadata) if metadata.is_dir() || metadata.is_file() => Ok(MarkerState::Present),
            Ok(_) => Ok(MarkerState::Unsupported),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MarkerState::Missing),
            Err(error) => Err(format!("无法读取实例隔离标记：{error}")),
        }
    })
}

/// No archive or Java program is opened. Count only enabled ordinary JARs,
/// using held directory FDs; links and disabled files do not increase demand.
pub(super) fn enabled_mods(root: &Path, id: &str, isolation: IsolationPolicy) -> Result<u32> {
    pcl_core::identifier(id)?;
    // Legacy registrations retain their original spelling, including root
    // aliases. Resolve that alias like launch does, then keep every directory
    // below it nofollow and reject a retargeted alias before publishing.
    let canonical = fs::canonicalize(root).map_err(|error| error.to_string())?;
    let root_dir = Dir::absolute(&canonical)?;
    let versions = root_dir.child("versions")?;
    let instance = versions.child(id)?;
    let scope = content_scope(&instance, isolation)?;
    let base = if scope == ContentScope::Shared {
        &root_dir
    } else {
        &instance
    };
    let mods = base.optional("mods")?;
    let mut count = 0;
    let mut stamp = None;
    if let Some(mods) = &mods {
        let before = mods.0.metadata().map_err(|e| e.to_string())?;
        stamp = Some((
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ));
        for (index, entry) in fs::read_dir(fd_path(mods))
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if index >= MAX_MOD_ENTRIES {
                return Err("模组目录条目过多，无法自动估算内存，请手动设置".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().map_err(|e| e.to_string())?.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
            {
                count += 1;
            }
        }
    }
    // A replaced ancestor cannot redirect the result to another instance.
    if fs::canonicalize(root).map_err(|error| error.to_string())? != canonical
        || Dir::absolute(&canonical)?.identity()? != root_dir.identity()?
        || root_dir.child("versions")?.identity()? != versions.identity()?
        || versions.child(id)?.identity()? != instance.identity()?
        || content_scope(&instance, isolation)? != scope
    {
        return Err(CHANGED.into());
    }
    match (mods, base.optional("mods")?) {
        (None, None) => {}
        (Some(held), Some(current)) if held.identity()? == current.identity()? => {
            let after = held.0.metadata().map_err(|e| e.to_string())?;
            if stamp
                != Some((
                    after.mtime(),
                    after.mtime_nsec(),
                    after.ctime(),
                    after.ctime_nsec(),
                ))
            {
                return Err("模组列表在估算期间变化，请重试".into());
            }
        }
        _ => return Err(CHANGED.into()),
    }
    Ok(count)
}
