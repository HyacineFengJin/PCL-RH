//! Read-only demand coordination through literal native directory handles.
//! Root registration aliases are canonicalized; no child alias is followed.
use super::{CHANGED, MAX_MOD_ENTRIES};
use pcl_core::{
    content_scope::{self, ContentScope, IsolationPolicy, MarkerState},
    retained_fs::{Directory, Kind, Leaf, Traversal},
};
use std::{fs, path::Path};
type Result<T> = std::result::Result<T, String>;
fn leaf(name: &str) -> Result<Leaf> {
    Leaf::new(name).map_err(|e| e.to_string())
}
fn child(parent: &Directory, name: &str) -> Result<Directory> {
    parent
        .child(&leaf(name)?, Traversal::AllowMounts)
        .map_err(|e| e.to_string())
}
fn optional(parent: &Directory, name: &str) -> Result<Option<Directory>> {
    if parent
        .entry(&leaf(name)?)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        Ok(None)
    } else {
        child(parent, name).map(Some)
    }
}
fn scope(instance: &Directory, policy: IsolationPolicy) -> Result<ContentScope> {
    content_scope::resolve_scope(policy, |name| {
        let marker = instance.entry(&leaf(name)?).map_err(|e| e.to_string())?;
        Ok(match marker {
            None => MarkerState::Missing,
            Some(m)
                if m.reparse_tag.is_none() && matches!(m.kind, Kind::Regular | Kind::Directory) =>
            {
                MarkerState::Present
            }
            Some(_) => MarkerState::Unsupported,
        })
    })
}
pub(super) fn enabled_mods(root: &Path, id: &str, policy: IsolationPolicy) -> Result<u32> {
    pcl_core::identifier(id)?;
    let canonical = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let root_dir = Directory::open_absolute(&canonical).map_err(|e| e.to_string())?;
    let versions = child(&root_dir, "versions")?;
    let instance = child(&versions, id)?;
    let isolation = scope(&instance, policy)?;
    let base = if isolation == ContentScope::Shared {
        &root_dir
    } else {
        &instance
    };
    let mods = optional(base, "mods")?;
    let mut count = 0;
    let mut stamp = None;
    if let Some(mods) = &mods {
        stamp = Some(mods.metadata().map_err(|e| e.to_string())?);
        for name in mods
            .names(MAX_MOD_ENTRIES)
            .map_err(|e| format!("无法估算模组目录：{e}"))?
        {
            let metadata = mods
                .entry(&Leaf::new(&name).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
                .ok_or(CHANGED)?;
            if metadata.kind == Kind::Regular
                && metadata.reparse_tag.is_none()
                && Path::new(&name)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
            {
                count += 1;
            }
        }
    }
    if fs::canonicalize(root).map_err(|e| e.to_string())? != canonical
        || Directory::open_absolute(&canonical)
            .and_then(|d| d.identity())
            .map_err(|e| e.to_string())?
            != root_dir.identity().map_err(|e| e.to_string())?
        || child(&root_dir, "versions")?
            .identity()
            .map_err(|e| e.to_string())?
            != versions.identity().map_err(|e| e.to_string())?
        || child(&versions, id)?
            .identity()
            .map_err(|e| e.to_string())?
            != instance.identity().map_err(|e| e.to_string())?
        || scope(&instance, policy)? != isolation
    {
        return Err(CHANGED.into());
    }
    match (mods, optional(base, "mods")?) {
        (None, None) => {}
        (Some(held), Some(current))
            if held.identity().map_err(|e| e.to_string())?
                == current.identity().map_err(|e| e.to_string())? =>
        {
            if Some(held.metadata().map_err(|e| e.to_string())?) != stamp {
                return Err("模组列表在估算期间变化，请重试".into());
            }
        }
        _ => return Err(CHANGED.into()),
    }
    Ok(count)
}
