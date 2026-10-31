//! UI-free production resolution for resource panels and native transactions.
use std::path::{Path, PathBuf};

#[cfg(test)]
pub fn resource_dir(root: &Path, id: &str, kind: &str) -> Result<PathBuf, String> {
    resource_dir_with_isolation(
        root,
        id,
        kind,
        pcl_core::content_scope::IsolationPolicy::Automatic,
    )
}

pub fn resource_dir_with_isolation(
    root: &Path,
    id: &str,
    kind: &str,
    policy: pcl_core::content_scope::IsolationPolicy,
) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let instance =
        pcl_core::scan_instance_with_isolation(&root, id, policy)?.ok_or("未找到所选版本")?;
    let scope = pcl_core::content_scope::ContentScope::from_isolated(instance.isolated);
    let base = root.join(scope.relative_base(id)?);
    let child = match kind {
        "mods" => "mods",
        "saves" => "saves",
        "screenshots" => "screenshots",
        "resourcepacks" => "resourcepacks",
        "shaderpacks" => "shaderpacks",
        "litematics" => "schematics",
        "server" => "",
        _ => return Err("未知资源类型".into()),
    };
    let base = base.canonicalize().map_err(|e| e.to_string())?;
    if !base.starts_with(&root) {
        return Err("目录超出游戏目录".into());
    }
    let path = base.join(child);
    if path.exists()
        && !path
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&root)
    {
        return Err("目录超出游戏目录".into());
    }
    Ok(path)
}
