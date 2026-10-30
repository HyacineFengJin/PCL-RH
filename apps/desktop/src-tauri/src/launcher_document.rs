//! Fixed launcher preference/favorite documents. Platform implementations keep
//! the same CAS API; Unix retains its physical JSON names and byte schema.
#[cfg(unix)]
#[path = "launcher_document/unix.rs"]
mod native;
#[cfg(windows)]
#[path = "launcher_document/windows.rs"]
mod native;
pub(super) use native::*;

#[cfg(not(target_os = "linux"))]
fn publish_named_export(
    target: pcl_core::retained_fs::Directory,
    visible_parent: &std::path::Path,
    name: &str,
    bytes: &[u8],
    max: usize,
) -> Result<(), String> {
    use pcl_core::retained_fs::{
        stage::{PublishOutcome, Stage},
        Directory, Leaf, WorkspaceRemoval,
    };
    use std::io::Write;
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err("本地数据备份名称无效".into());
    }
    let leaf = Leaf::new(name).map_err(|e| format!("本地数据备份名称无效：{e}"))?;
    if bytes.len() > max {
        return Err(format!("本地数据备份超过 {} KiB", max / 1024));
    }
    if target.entry(&leaf).map_err(|e| e.to_string())?.is_some() {
        return Err("目标文件已经存在，设置导出不会覆盖已有文件，请选择新文件名".into());
    }
    let identity = target.identity().map_err(|e| e.to_string())?;
    let recheck = || -> Result<(), String> {
        if Directory::open_absolute(visible_parent)
            .and_then(|d| d.identity())
            .map_err(|e| e.to_string())?
            != identity
        {
            return Err("本地数据备份目标目录已被外部替换".into());
        }
        Ok(())
    };
    recheck()?;
    let (workspace_name, workspace) = target
        .create_private_workspace()
        .map_err(|e| e.to_string())?;
    target.sync().map_err(|e| e.to_string())?;
    let workspace_meta = workspace.metadata().map_err(|e| e.to_string())?;
    let mut stage = Stage::new(&workspace).map_err(|e| e.to_string())?;
    stage
        .file_mut()
        .write_all(bytes)
        .map_err(|e| e.to_string())?;
    let sealed = stage.seal(max as u64).map_err(|e| e.to_string())?;
    recheck()?;
    match sealed
        .publish_new(&target, &leaf)
        .map_err(|e| e.to_string())?
    {
        PublishOutcome::Published { .. } => {}
        PublishOutcome::Exists { .. } => {
            return Err("目标文件已经存在，设置导出不会覆盖已有文件".into())
        }
        PublishOutcome::MayHavePublished { error, .. } => {
            return Err(format!("备份可能已发布，请检查目标文件后重新操作：{error}"))
        }
    }
    recheck()?;
    match target
        .remove_workspace_entry(&workspace_name, &workspace_meta)
        .map_err(|e| e.to_string())?
    {
        WorkspaceRemoval::Removed => Ok(()),
        WorkspaceRemoval::MayHaveRemoved { error } => {
            Err(format!("备份已发布，暂存目录清理未确认：{error}"))
        }
    }
}
