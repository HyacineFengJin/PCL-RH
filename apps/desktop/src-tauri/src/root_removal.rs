//! Forgetting a registered root is a metadata operation. A deleted directory
//! must not make its entry permanent, but live workers and recovery journals
//! still own their registry identities until their work has been settled.
use crate::{config::Settings, launcher_monitor_runtime, resource_ops, Shared};
use std::{fs, io::ErrorKind, path::PathBuf};

const OCCUPIED: &str = "该目录正在被游戏或文件操作使用，请等待结束后移除";

/// The application admission lock spans checks and the settings commit, so a
/// new task cannot acquire a root between the last check and forgetting it.
pub fn remove(shared: &Shared, id: &str) -> Result<Settings, String> {
    let _operation = shared.operations.lock().unwrap();
    crate::require_reference_write(shared)?;
    require_root_removable(shared, id)?;
    shared.config.remove(id)
}

fn unused_path(shared: &Shared, id: &str) -> Result<Option<PathBuf>, String> {
    let root = shared.config.registered(id)?;
    launcher_monitor_runtime::require_root_unused(shared, id)?;
    let running = {
        let run = shared.status.lock().unwrap();
        run.root_id.as_deref() == Some(id) && matches!(run.stage.as_str(), "preparing" | "running")
    };
    if shared.tasks.uses_root(id) || running {
        return Err(OCCUPIED.into());
    }
    let path = match fs::canonicalize(&root.path) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // Without a filesystem identity we cannot prove that another task's
            // nested/aliased footprint is unrelated. Wait for existing writers
            // to drain; never recreate the path just to inspect it.
            if shared.tasks.active().is_some() {
                return Err("目录已不存在，请等待文件操作结束后移除登记".into());
            }
            return Ok(None);
        }
        Err(error) => return Err(format!("无法检查游戏目录：{error}")),
    };
    if shared.tasks.blocks_path(&path)? {
        return Err(OCCUPIED.into());
    }
    Ok(Some(path))
}

#[cfg(test)]
pub(crate) fn require_root_unused(shared: &Shared, id: &str) -> Result<(), String> {
    unused_path(shared, id).map(|_| ())
}

pub(crate) fn require_root_removable(shared: &Shared, id: &str) -> Result<(), String> {
    let Some(path) = unused_path(shared, id)? else {
        return Ok(());
    };
    // An accessible root can still own unpublished files or recoverable deleted
    // instances. Preserve those checks, including invalid/unreadable journals.
    resource_ops::ensure_verified_batches_ready(&path)?;
    pcl_install::DamageRepair::ensure_ready(&path)?;
    if !crate::instance_delete::reserved_names(&path)?.is_empty() {
        return Err("此游戏目录仍有已删除实例，请先恢复它们后再移除目录登记".into());
    }
    Ok(())
}
