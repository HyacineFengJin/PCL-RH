//! Official snapshot admission. Rust reconstructs every plan; the UI only holds
//! preview revisions. Call leases survive accepted workers, and the common
//! importer publishes into a new name with its existing rollback/recovery rules.
use crate::{
    config::IsolationSettings,
    instance_commands::{new_name, ready, recheck_target, spawn},
    instance_export, instance_import,
    plugin_host::ServiceCall,
    product_plugins::contracts::snapshots::{SnapshotMode, SnapshotService, SNAPSHOTS},
    tasks, Shared,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};
use tauri::State;

fn service(shared: &Shared) -> Result<ServiceCall<SnapshotService>, String> {
    shared
        .product_plugins
        .optional_service(SNAPSHOTS)?
        .ok_or_else(|| "实例备份插件已停用".into())
}
/// Caller holds operations. Settings and root are one captured authority.
fn capture(
    shared: &Shared,
    root_id: Option<&str>,
    name: Option<&str>,
) -> Result<IsolationSettings, String> {
    crate::require_instance_job(shared)?;
    let captured = shared.config.isolation_settings(root_id)?;
    ready(shared, &captured.root)?;
    if let Some(name) = name {
        new_name(shared, &captured.root, name)?;
        if captured.policy(name) == pcl_core::content_scope::IsolationPolicy::Shared {
            return Err("恢复目标必须使用独立游戏目录；请先将全局实例隔离设为自动或独立".into());
        }
    }
    Ok(captured)
}
fn recheck(shared: &Shared, captured: &IsolationSettings) -> Result<(), String> {
    recheck_target(shared, &captured.root)?;
    let current = shared.config.isolation_settings(Some(&captured.root.id))?;
    if current.revision != captured.revision || current.root.path != captured.root.path {
        return Err("游戏目录或设置已改变，请重新检查备份操作".into());
    }
    Ok(())
}
struct PreparedExport {
    lease: ServiceCall<SnapshotService>,
    captured: IsolationSettings,
    plan: instance_export::ExportPlan,
}
async fn prepare_export(
    shared: Arc<Shared>,
    root_id: Option<String>,
    id: String,
    mode: SnapshotMode,
    version: String,
) -> Result<PreparedExport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let service = service(&shared)?;
        let captured = {
            let _operation = shared.operations.lock().unwrap();
            capture(&shared, root_id.as_deref(), None)?
        };
        let plan = service.0.export(
            Path::new(&captured.root.path),
            &id,
            mode,
            &version,
            captured.policy(&id),
            &captured.revision,
        )?;
        let _operation = shared.operations.lock().unwrap();
        recheck(&shared, &captured)?;
        Ok(PreparedExport {
            lease: service,
            captured,
            plan,
        })
    })
    .await
    .map_err(|_| "检查实例备份的任务意外退出".to_string())?
}
#[tauri::command]
pub async fn instance_snapshot_prepare(
    root_id: Option<String>,
    id: String,
    mode: SnapshotMode,
    version: String,
    state: State<'_, Arc<Shared>>,
) -> Result<instance_export::ExportPlan, String> {
    Ok(
        prepare_export(state.inner().clone(), root_id, id, mode, version)
            .await?
            .plan,
    )
}
#[tauri::command]
pub async fn instance_snapshot_start(
    root_id: Option<String>,
    id: String,
    mode: SnapshotMode,
    version: String,
    revision: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<String>, String> {
    let PreparedExport {
        lease,
        captured,
        plan,
    } = prepare_export(state.inner().clone(), root_id, id, mode, version).await?;
    if plan.revision != revision {
        return Err("备份内容或选项已改变，请重新检查".into());
    }
    crate::instance_export_commands::start_checked(
        state.inner().clone(),
        window,
        captured,
        plan,
        lease,
    )
    .await
}

#[derive(Serialize)]
pub struct BackupChoice {
    status: &'static str,
    source: Option<String>,
    filename: Option<String>,
    suggested_name: Option<String>,
    message: Option<String>,
}
#[tauri::command]
pub async fn instance_snapshot_pick(
    root_id: Option<String>,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<BackupChoice, String> {
    let _lease = service(&state)?;
    let captured = {
        let _operation = state.operations.lock().unwrap();
        capture(&state, root_id.as_deref(), None)?
    };
    let choice = state
        .desktop
        .pick_backup_zip(window, state.project.clone())
        .await?;
    if choice.status != "selected" {
        return Ok(BackupChoice {
            status: choice.status,
            source: None,
            filename: None,
            suggested_name: None,
            message: choice.message,
        });
    }
    {
        let _operation = state.operations.lock().unwrap();
        recheck(&state, &captured)?;
    }
    let path = choice.paths.into_iter().next().ok_or("未选择备份文件")?;
    let stem = path
        .file_stem()
        .and_then(|p| p.to_str())
        .unwrap_or("Restored Instance");
    let mut name = String::new();
    for c in stem.chars() {
        if name.len() + c.len_utf8() > 96 {
            break;
        }
        name.push(if c.is_control() || "/\\:".contains(c) {
            '_'
        } else {
            c
        });
    }
    let name = name.trim();
    let name = if name.is_empty() {
        "Restored Instance"
    } else {
        name
    };
    Ok(BackupChoice {
        status: "selected",
        filename: path.file_name().and_then(|p| p.to_str()).map(str::to_owned),
        source: Some(path.to_string_lossy().into_owned()),
        suggested_name: Some(format!("{name} - Restored")),
        message: None,
    })
}
#[derive(Serialize)]
pub struct RestorePreview {
    revision: String,
    name: String,
    pack_name: String,
    pack_version: String,
    minecraft: String,
    file_count: usize,
    bytes: u64,
    warnings: Vec<String>,
}
fn restore_revision(plan: &instance_import::ImportPlan, settings_revision: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"pcl-backup-restore-v1\0");
    hash.update(plan.revision.as_bytes());
    hash.update(b"\0");
    hash.update(settings_revision.as_bytes());
    format!("{:x}", hash.finalize())
}

#[tauri::command]
pub async fn instance_snapshot_restore_prepare(
    root_id: Option<String>,
    source: String,
    name: String,
    state: State<'_, Arc<Shared>>,
) -> Result<RestorePreview, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let service = service(&shared)?;
        let captured = {
            let _operation = shared.operations.lock().unwrap();
            capture(&shared, root_id.as_deref(), Some(&name))?
        };
        let plan = service
            .0
            .restore(Path::new(&captured.root.path), Path::new(&source), &name)?;
        let _operation = shared.operations.lock().unwrap();
        recheck(&shared, &captured)?;
        new_name(&shared, &captured.root, &name)?;
        Ok(RestorePreview {
            revision: restore_revision(&plan, &captured.revision),
            name,
            pack_name: plan.pack_name,
            pack_version: plan.pack_version,
            minecraft: plan.minecraft,
            file_count: plan.file_count,
            bytes: plan.bytes,
            warnings: plan.warnings,
        })
    })
    .await
    .map_err(|_| "检查备份恢复的任务意外退出".to_string())?
}
#[tauri::command]
pub async fn instance_snapshot_restore_start(
    root_id: Option<String>,
    source: String,
    name: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let lease = service(&shared)?;
        let captured = {
            let _operation = shared.operations.lock().unwrap();
            capture(&shared, root_id.as_deref(), Some(&name))?
        };
        let plan = lease
            .0
            .restore(Path::new(&captured.root.path), Path::new(&source), &name)?;
        let _operation = shared.operations.lock().unwrap();
        recheck(&shared, &captured)?;
        new_name(&shared, &captured.root, &name)?;
        if restore_revision(&plan, &captured.revision) != revision {
            return Err("备份、目标名称或设置已改变，请重新检查".into());
        }
        let auto_select = shared
            .launcher_preferences
            .snapshot()
            .preferences
            .auto_select_installed;
        // Existing import task kind keeps all bootstrap, cancellation and
        // recovery projections aligned with the shared publication journal.
        spawn(
            shared.clone(),
            captured.root.clone(),
            name.clone(),
            tasks::TaskKind::InstanceImport,
            "备份恢复完成",
            move |shared, root, task| {
                let _lease = lease;
                let cancel = task.cancellation_token();
                let mut result = instance_import::execute_checked(
                    plan,
                    &cancel,
                    |progress| {
                        if progress.stage == "import-commit" {
                            task.begin_finishing();
                        }
                        shared.downloads.progress(task, progress);
                    },
                    || {
                        let _operation = shared.operations.lock().unwrap();
                        if shared.config.isolation_settings(Some(&root.id))?.revision
                            != captured.revision
                        {
                            return Err("实例设置已改变，请重新恢复备份".into());
                        }
                        new_name(shared, root, &name)
                    },
                )?;
                result["message"] = "备份恢复完成".into();
                if auto_select {
                    let _operation = shared.operations.lock().unwrap();
                    if let Err(error) = shared.config.select_installed(&root.id, &name) {
                        result["warning"] = format!("备份已恢复，但选择状态未保存：{error}").into();
                    }
                }
                Ok(result)
            },
        )
    })
    .await
    .map_err(|_| "启动备份恢复任务失败".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration_tests::Fixture;
    #[test]
    fn restoring_rejects_shared_targets_and_existing_reference_names() {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        assert!(capture(&shared, None, Some("Restored")).is_ok());
        let mut settings = shared.config.snapshot();
        settings.isolation = pcl_core::content_scope::IsolationPolicy::Shared;
        shared.config.save(settings).unwrap();
        assert!(capture(&shared, None, Some("Restored"))
            .err()
            .unwrap()
            .contains("独立游戏目录"));
        let mut settings = shared.config.snapshot();
        settings.isolation = pcl_core::content_scope::IsolationPolicy::Automatic;
        settings.overrides.insert("Restored".into(), 4);
        shared.config.save(settings).unwrap();
        assert!(capture(&shared, None, Some("Restored")).is_err());
    }
    #[test]
    fn stopped_snapshot_plugin_rejects_native_entry_even_with_launcher_active() {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        assert!(service(&shared).is_ok());
        let control = shared.project.join(".pcl-rust/rh-code/workspace.json");
        std::fs::create_dir_all(control.parent().unwrap()).unwrap();
        std::fs::write(
            control,
            r#"{"schemaVersion":1,"disabledPlugins":["launcher.instances.snapshots"]}"#,
        )
        .unwrap();
        assert!(service(&shared).is_err());
    }
}
