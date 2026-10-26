//! Desktop admission for local instance import and recoverable deletion.
//!
//! Commands bind a registered canonical root and re-create a checked plan before
//! admitting the single writer. Long-running file transactions own their task;
//! the operations mutex is released before progress callbacks or selection
//! persistence. Recovery bypasses only its own pending guard, never game/task
//! admission. Transaction modules own files and journals, not window state.

use crate::{config::GameRoot, instance_delete, instance_import, tasks, Shared};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::State;

fn ready(shared: &Shared, root: &GameRoot) -> Result<(), String> {
    crate::instance_rename_refs::ensure_project_ready(&shared.project)?;
    let path = Path::new(&root.path);
    crate::resource_ops::ensure_ready(path)?;
    crate::instance_reset::ensure_ready(path)?;
    crate::instance_rename::ensure_ready(path)?;
    instance_import::ensure_ready(path)?;
    pcl_install::DamageRepair::ensure_ready(path)?;
    instance_delete::ensure_ready(path)
}

/// The caller holds operations. Read-only hashing runs without that mutex;
/// repeat admission and root binding when the plan comes back.
fn recheck_target(shared: &Shared, root: &GameRoot) -> Result<(), String> {
    crate::require_instance_job(shared)?;
    if shared.config.resolve(Some(&root.id))?.path != root.path {
        return Err("游戏目录位置已改变，请重新检查实例操作".into());
    }
    ready(shared, root)
}

pub(super) fn new_name(shared: &Shared, root: &GameRoot, id: &str) -> Result<(), String> {
    crate::instance_rename_refs::ensure_project_ready(&shared.project)?;
    if shared.config.resolve(Some(&root.id))?.path != root.path {
        return Err("导入目标目录已改变，请重新检查".into());
    }
    instance_delete::ensure_name_available(Path::new(&root.path), id)?;
    shared.config.ensure_new_instance_name(&root.id, id)?;
    shared
        .instance_metadata
        .ensure_new_instance_name(&root.id, Path::new(&root.path), id)?;
    if crate::export_presets::read(&shared.project, &root.id, Path::new(&root.path), id)?.is_some()
    {
        return Err("此名称仍有保存的导出配置，请使用新的实例名称".into());
    }
    crate::resource_ops::ensure_new_instance_name(Path::new(&root.path), id)
}

/// Called with operations held. Spawn failures release the admitted handle;
/// panics remain errors and leave transaction recovery records visible.
fn spawn(
    shared: Arc<Shared>,
    root: GameRoot,
    id: String,
    kind: tasks::TaskKind,
    success: &'static str,
    work: impl FnOnce(&Shared, &GameRoot, &tasks::TaskHandle) -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    let task = shared.tasks.admit(
        tasks::TaskTarget {
            root_id: root.id.clone(),
            root_path: root.path.clone(),
            instance_id: Some(id),
        },
        kind,
    )?;
    let task_id = task.id().to_owned();
    shared.downloads.track(&task);
    std::thread::Builder::new()
        .name(format!("pcl-instance-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work(&shared, &root, &task)
            }))
            .unwrap_or_else(|_| Err("实例任务意外退出，文件已保留，请先恢复未完成操作".into()));
            match result {
                Ok(value) => crate::finish_instance_task(task, Ok(value), success),
                // Only an explicit transaction cancellation is a cancelled task.
                // A late cancel must never conceal hash, conflict or cleanup errors.
                Err(error)
                    if matches!(
                        error.as_str(),
                        "实例导入已取消"
                            | "导入已取消"
                            | "实例删除已取消"
                            | "实例恢复已取消"
                            | "导出已取消"
                            | "实例复制已取消"
                    ) =>
                {
                    task.finish(tasks::TaskOutcome::Failed(error));
                }
                Err(error) => {
                    task.finish(tasks::TaskOutcome::Error(error));
                }
            }
        })
        .map_err(|error| format!("无法启动实例任务：{error}"))?;
    Ok(json!({"id":task_id}))
}

#[derive(Serialize)]
pub struct ImportChoice {
    status: &'static str,
    source: Option<String>,
    suggested_name: Option<String>,
    message: Option<String>,
}

fn require_copy_isolation(
    captured: &crate::config::IsolationSettings,
    name: &str,
) -> Result<(), String> {
    if captured.policy(name) == pcl_core::content_scope::IsolationPolicy::Shared {
        return Err("副本必须使用独立游戏目录；请先将全局实例隔离设为自动或独立后再复制".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn instance_copy_prepare(
    root_id: Option<String>,
    id: String,
    request: crate::product_plugins::contracts::copy::CopyRequest,
    state: State<'_, Arc<Shared>>,
) -> Result<crate::product_plugins::contracts::copy::CopyPreview, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let service = shared
            .product_plugins
            .optional_service(crate::product_plugins::contracts::copy::INSTANCE_COPY)?
            .ok_or("实例复制插件已停用")?;
        let captured = {
            let _operation = shared.operations.lock().unwrap();
            crate::require_instance_job(&shared)?;
            let captured = shared.config.isolation_settings(root_id.as_deref())?;
            ready(&shared, &captured.root)?;
            new_name(&shared, &captured.root, &request.name)?;
            require_copy_isolation(&captured, &request.name)?;
            captured
        };
        let plan = service.0.prepare(
            Path::new(&captured.root.path),
            &id,
            request,
            captured.policy(&id),
            &captured.revision,
        )?;
        let _operation = shared.operations.lock().unwrap();
        recheck_target(&shared, &captured.root)?;
        let current = shared.config.isolation_settings(Some(&captured.root.id))?;
        if current.revision != captured.revision {
            return Err("实例设置已改变，请重新检查复制方案".into());
        }
        new_name(&shared, &captured.root, &plan.preview().name)?;
        Ok(plan.preview().clone())
    })
    .await
    .map_err(|_| "检查实例复制的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn instance_copy_start(
    root_id: Option<String>,
    id: String,
    request: crate::product_plugins::contracts::copy::CopyRequest,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // A retired provider cannot admit a new call. This lease survives until
        // the accepted worker finishes, including cancellation and rollback.
        let service = shared
            .product_plugins
            .optional_service(crate::product_plugins::contracts::copy::INSTANCE_COPY)?
            .ok_or("实例复制插件已停用")?;
        let captured = {
            let _operation = shared.operations.lock().unwrap();
            crate::require_instance_job(&shared)?;
            let captured = shared.config.isolation_settings(root_id.as_deref())?;
            ready(&shared, &captured.root)?;
            new_name(&shared, &captured.root, &request.name)?;
            require_copy_isolation(&captured, &request.name)?;
            captured
        };
        let plan = service.0.prepare(
            Path::new(&captured.root.path),
            &id,
            request,
            captured.policy(&id),
            &captured.revision,
        )?;
        let _operation = shared.operations.lock().unwrap();
        recheck_target(&shared, &captured.root)?;
        if plan.preview().revision != revision
            || shared
                .config
                .isolation_settings(Some(&captured.root.id))?
                .revision
                != captured.revision
        {
            return Err("源文件、复制选项或实例设置已改变，请重新检查".into());
        }
        let name = plan.preview().name.clone();
        new_name(&shared, &captured.root, &name)?;
        let auto_select = shared
            .launcher_preferences
            .snapshot()
            .preferences
            .auto_select_installed;
        spawn(
            shared.clone(),
            captured.root.clone(),
            name.clone(),
            tasks::TaskKind::InstanceCopy,
            "实例复制完成",
            move |shared, root, task| {
                let _lease = service;
                let cancel = task.cancellation_token();
                let mut result = plan.execute(
                    &cancel,
                    &|p| {
                        if p.stage == "import-commit" {
                            task.begin_finishing();
                        }
                        shared.downloads.progress(task, p);
                    },
                    &|| {
                        let _operation = shared.operations.lock().unwrap();
                        let current = shared.config.isolation_settings(Some(&root.id))?;
                        if current.revision != captured.revision || current.root.path != root.path {
                            return Err("实例设置已改变，请重新复制".into());
                        }
                        require_copy_isolation(&current, &name)?;
                        new_name(shared, root, &name)
                    },
                )?;
                result["message"] = "实例复制完成".into();
                if auto_select {
                    let _operation = shared.operations.lock().unwrap();
                    if let Err(error) = shared.config.select_installed(&root.id, &name) {
                        result["warning"] = format!("实例已复制，但选择状态未保存：{error}").into();
                    }
                }
                Ok(result)
            },
        )
    })
    .await
    .map_err(|_| "启动实例复制任务失败".to_string())?
}

/// Preserve the existing local ZIP DTO. Network packs use native one-use
/// confirmation authority; serialized views never grant filesystem permission.
#[derive(Serialize)]
#[serde(untagged)]
pub enum LocalPackPlan {
    Zip(instance_import::ImportPlan),
    Pack(instance_import::mrpack::PackPlan),
}

fn prepare_local_pack(
    root: &Path,
    source: &Path,
    name: &str,
    optional_paths: Option<&[String]>,
) -> Result<LocalPackPlan, String> {
    if instance_import::mrpack::recognizes(source)? {
        instance_import::mrpack::inspect(root, source, name, optional_paths)
            .map(LocalPackPlan::Pack)
    } else {
        if optional_paths.is_some_and(|paths| !paths.is_empty()) {
            return Err("本地 ZIP 导入不接受 mrpack 可选文件，请重新检查".into());
        }
        instance_import::prepare(root, source, name).map(LocalPackPlan::Zip)
    }
}

fn require_local_zip(source: &Path) -> Result<(), String> {
    if !instance_import::mrpack::is_local_export(source)? {
        Err("此整合包需要原生安装确认；本地 ZIP 写入入口的安装尚未开放".into())
    } else {
        Ok(())
    }
}

#[tauri::command]
pub async fn instance_import_pick(
    root_id: Option<String>,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<ImportChoice, String> {
    let root = {
        let _operation = state.operations.lock().unwrap();
        crate::require_network_submission(&state)?;
        let root = state.config.resolve(root_id.as_deref())?;
        pack_ready(&state, &root)?;
        root
    };
    let choice = state
        .desktop
        .pick_instance_zip(window, state.project.clone())
        .await?;
    if choice.status != "selected" {
        return Ok(ImportChoice {
            status: choice.status,
            source: None,
            suggested_name: None,
            message: choice.message,
        });
    }
    let current = state.config.resolve(Some(&root.id))?;
    if current.path != root.path {
        return Err("导入目标目录已改变，请重新选择".into());
    }
    let path = choice.paths.into_iter().next().ok_or("未选择整合包文件")?;
    let suggested = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Imported Instance")
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>();
    // This suggestion is editable. Authoritative identifier and archive checks run
    // in prepare; picker cancellation does not create a plan or file task.
    Ok(ImportChoice {
        status: "selected",
        source: Some(path.to_string_lossy().into_owned()),
        suggested_name: Some(suggested.trim().to_owned()),
        message: None,
    })
}

#[tauri::command]
pub async fn instance_import_prepare(
    root_id: Option<String>,
    source: String,
    name: String,
    optional_paths: Option<Vec<String>>,
    state: State<'_, Arc<Shared>>,
) -> Result<LocalPackPlan, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Admit even the short classifier before opening a source FD. The same
        // window then bounds inner extraction; shutdown is checked by every
        // scan/hash chunk through the existing shared closing flag.
        let preparation = shared.pack_confirmations.preparation()?;
        if shared.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("启动器正在关闭，不能检查整合包".into());
        }
        let network_pack = !instance_import::mrpack::is_local_export_cancellable(
            Path::new(&source),
            &shared.closing,
        )?;
        let root = {
            let _operation = shared.operations.lock().unwrap();
            if network_pack {
                crate::require_network_submission(&shared)?;
            } else {
                crate::require_instance_job(&shared)?;
            }
            let root = shared.config.resolve(root_id.as_deref())?;
            if network_pack {
                pack_ready(&shared, &root)?;
            } else {
                ready(&shared, &root)?;
            }
            new_name(&shared, &root, &name)?;
            root
        };
        let plan = if network_pack {
            LocalPackPlan::Pack(shared.pack_confirmations.prepare_admitted(
                preparation,
                &root,
                &shared.project,
                Path::new(&source),
                &name,
                optional_paths.as_deref(),
                &shared.closing,
            )?)
        } else {
            prepare_local_pack(
                Path::new(&root.path),
                Path::new(&source),
                &name,
                optional_paths.as_deref(),
            )?
        };
        finish_prepare(&shared, &root, &name, network_pack, plan)
    })
    .await
    .map_err(|_| "检查整合包导入方案的任务意外退出".to_string())?
}

/// Releasing an abandoned preview is allowed during shutdown and root changes.
/// The opaque token only removes an unclaimed cache record; a submitted worker
/// has already moved that record out and retains its source/disk lease.
#[tauri::command]
pub fn instance_import_discard(revision: String, state: State<'_, Arc<Shared>>) {
    state.pack_confirmations.discard_unclaimed(&revision);
}

/// Only a plan that passes the native command's final short gates is exposed.
/// Any late failure retires its exact still-unclaimed authority before return;
/// queued/running ownership is outside the cache and is never revoked here.
pub(crate) fn finish_prepare(
    shared: &Shared,
    root: &GameRoot,
    name: &str,
    network_pack: bool,
    plan: LocalPackPlan,
) -> Result<LocalPackPlan, String> {
    let projection = match &plan {
        LocalPackPlan::Pack(plan) if plan.installable => {
            Some(shared.pack_confirmations.pending_projection(&plan.revision))
        }
        _ => None,
    };
    let _operation = shared.operations.lock().unwrap();
    if network_pack {
        crate::require_network_submission(shared)?;
        if shared.config.resolve(Some(&root.id))?.path != root.path {
            return Err("游戏目录位置已改变，请重新检查".into());
        }
        pack_ready(shared, root)?;
    } else {
        recheck_target(shared, root)?;
    }
    new_name(shared, root, name)?;
    if let Some(projection) = projection {
        projection.expose();
    }
    Ok(plan)
}

/// A running pack transaction is guarded by its root-scoped worker. Read-only
/// planning and queued submissions may coexist with it; abandoned journals still
/// block unless the exact recorded build operation is owned by this worker.
fn pack_ready(shared: &Shared, root: &GameRoot) -> Result<(), String> {
    crate::resource_ops::ensure_ready(Path::new(&root.path))?;
    crate::instance_reset::ensure_ready(Path::new(&root.path))?;
    crate::instance_rename::ensure_ready(Path::new(&root.path))?;
    instance_delete::ensure_ready(Path::new(&root.path))?;
    shared
        .pack_confirmations
        .ensure_ready(Path::new(&root.path))?;
    Ok(())
}

#[tauri::command]
pub fn instance_pack_download(
    root_id: Option<String>,
    request: crate::resource_save::SaveRequest,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    let _operation = shared.operations.lock().unwrap();
    crate::require_network_submission(&shared)?;
    let root = shared.config.resolve(root_id.as_deref())?;
    pack_ready(&shared, &root)?;
    instance_import::mrpack::official::begin(shared.clone(), root, request)
}

/// Closing a download view retires pending ownership before asking the task to
/// cancel, so a completion racing this command cannot resurrect its input.
#[tauri::command]
pub fn instance_pack_release(source_id: String, state: State<'_, Arc<Shared>>) {
    if let Some(id) = state.pack_confirmations.release_source(&source_id) {
        let _ = state.tasks.cancel(&id);
    }
}

#[tauri::command]
pub async fn instance_pack_prepare(
    root_id: Option<String>,
    source_id: String,
    name: String,
    optional_paths: Option<Vec<String>>,
    state: State<'_, Arc<Shared>>,
) -> Result<LocalPackPlan, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let root = {
            let _operation = shared.operations.lock().unwrap();
            crate::require_network_submission(&shared)?;
            let root = shared.config.resolve(root_id.as_deref())?;
            pack_ready(&shared, &root)?;
            new_name(&shared, &root, &name)?;
            root
        };
        let plan = instance_import::mrpack::official::prepare(
            &shared.pack_confirmations,
            &root,
            &shared.project,
            &source_id,
            &name,
            optional_paths.as_deref(),
            &shared.closing,
        )?;
        finish_prepare(&shared, &root, &name, true, LocalPackPlan::Pack(plan))
    })
    .await
    .map_err(|_| "检查官方整合包的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn instance_pack_start(
    root_id: Option<String>,
    source_id: String,
    name: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = shared.operations.lock().unwrap();
        crate::require_network_submission(&shared)?;
        let root = shared.config.resolve(root_id.as_deref())?;
        pack_ready(&shared, &root)?;
        new_name(&shared, &root, &name)?;
        instance_import::mrpack::official::start(shared.clone(), root, source_id, name, revision)
    })
    .await
    .map_err(|_| "提交官方整合包安装的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn instance_import_start(
    root_id: Option<String>,
    source: String,
    name: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if revision.starts_with("pack-confirm-v1:") {
            let _operation = shared.operations.lock().unwrap();
            crate::require_network_submission(&shared)?;
            let root = shared.config.resolve(root_id.as_deref())?;
            pack_ready(&shared, &root)?;
            new_name(&shared, &root, &name)?;
            return instance_import::mrpack::service::start(
                shared.clone(),
                root,
                PathBuf::from(source),
                name,
                revision,
            );
        }
        let _operation = shared.operations.lock().unwrap();
        crate::require_instance_job(&shared)?;
        let root = shared.config.resolve(root_id.as_deref())?;
        ready(&shared, &root)?;
        new_name(&shared, &root, &name)?;
        drop(_operation);
        // Preview revisions are not execution permission. Even a caller that
        // bypasses the UI cannot route another format through legacy ZIP publication.
        require_local_zip(Path::new(&source))?;
        let checked = instance_import::prepare(Path::new(&root.path), Path::new(&source), &name)?;
        let _operation = shared.operations.lock().unwrap();
        recheck_target(&shared, &root)?;
        new_name(&shared, &root, &name)?;
        if checked.revision != revision {
            return Err("ZIP、导入名称或目标文件已改变，请重新检查".into());
        }
        let auto_select = shared
            .launcher_preferences
            .snapshot()
            .preferences
            .auto_select_installed;
        spawn(
            shared.clone(),
            root,
            name,
            tasks::TaskKind::InstanceImport,
            "本地 ZIP 导入完成",
            move |shared, root, task| {
                let cancel = task.cancellation_token();
                let name = checked.name.clone();
                let mut result = instance_import::execute_checked(
                    checked,
                    &cancel,
                    |progress| {
                        if progress.stage == "import-commit" {
                            task.begin_finishing();
                        }
                        shared.downloads.progress(task, progress);
                    },
                    || {
                        // Extraction can take minutes. Recheck launcher references at
                        // publication so an external settings edit cannot attach an
                        // old instance's preferences to the newly imported files.
                        let _operation = shared.operations.lock().unwrap();
                        new_name(shared, root, &name)
                    },
                )?;
                // A selection write failure does not invalidate a committed import.
                // Keep the installed result and expose the persistence warning.
                let _operation = shared.operations.lock().unwrap();
                if auto_select {
                    if let Err(error) = shared.config.select_installed(
                        &root.id,
                        result["id"].as_str().ok_or("导入结果缺少实例名称")?,
                    ) {
                        result["warning"] = format!("实例已导入，但选择状态未保存：{error}").into();
                    }
                }
                Ok(result)
            },
        )
    })
    .await
    .map_err(|_| "启动 ZIP 导入任务失败".to_string())?
}

#[tauri::command]
pub async fn instance_delete_prepare(
    root_id: Option<String>,
    id: String,
    state: State<'_, Arc<Shared>>,
) -> Result<instance_delete::DeletePlan, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = shared.operations.lock().unwrap();
        crate::require_instance_job(&shared)?;
        let root = shared.config.resolve(root_id.as_deref())?;
        ready(&shared, &root)?;
        shared.config.ensure_rename_snapshot()?;
        drop(_operation);
        let plan = instance_delete::prepare(Path::new(&root.path), &shared.project, &root.id, &id)?;
        let _operation = shared.operations.lock().unwrap();
        recheck_target(&shared, &root)?;
        shared.config.ensure_rename_snapshot()?;
        Ok(plan)
    })
    .await
    .map_err(|_| "检查实例删除方案的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn instance_delete_start(
    root_id: Option<String>,
    id: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = shared.operations.lock().unwrap();
        crate::require_instance_job(&shared)?;
        let root = shared.config.resolve(root_id.as_deref())?;
        ready(&shared, &root)?;
        shared.config.ensure_rename_snapshot()?;
        drop(_operation);
        let checked =
            instance_delete::prepare(Path::new(&root.path), &shared.project, &root.id, &id)?;
        let _operation = shared.operations.lock().unwrap();
        recheck_target(&shared, &root)?;
        shared.config.ensure_rename_snapshot()?;
        if checked.revision != revision {
            return Err("实例文件、引用或设置已改变，请重新检查".into());
        }
        spawn(
            shared.clone(),
            root,
            id,
            tasks::TaskKind::InstanceDelete,
            "实例已移入可恢复区",
            move |shared, root, task| {
                let cancel = task.cancellation_token();
                instance_delete::execute(
                    Path::new(&root.path),
                    &shared.project,
                    checked,
                    &cancel,
                    |progress| delete_progress(task, progress),
                )
            },
        )
    })
    .await
    .map_err(|_| "启动实例删除任务失败".to_string())?
}

fn delete_progress(task: &tasks::TaskHandle, progress: instance_delete::DeleteProgress) {
    if progress.phase == "committing" {
        task.begin_finishing();
    }
    let current = match progress.phase.as_str() {
        "committing" => 1,
        "complete" => 3,
        _ => 0,
    };
    let steps = [
        ("instance-check", "检查实例文件与引用"),
        ("instance-move", "移动实例目录"),
        ("instance-record", "校验文件并保存恢复记录"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (id, label))| pcl_install::InstallStep {
        id: id.into(),
        label: label.into(),
        progress: None,
        state: if index < current {
            "complete"
        } else if index == current {
            "running"
        } else {
            "pending"
        }
        .into(),
    })
    .collect();
    task.update(tasks::TaskProgress {
        stage: tasks::TaskStage::Processing,
        phase: progress.phase,
        message: progress.message,
        completed: progress.completed,
        total: progress.total,
        bytes_done: 0,
        bytes_total: 0,
        network_bytes: 0,
        steps,
    });
}

#[tauri::command]
pub async fn instance_deleted_list(
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<Vec<instance_delete::DeleteHistoryEntry>, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let root = shared.config.resolve(root_id.as_deref())?;
        instance_delete::history(Path::new(&root.path), &shared.project, &root.id)
    })
    .await
    .map_err(|_| "读取实例恢复记录的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn instance_restore_start(
    root_id: Option<String>,
    operation_id: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = shared.operations.lock().unwrap();
        crate::require_instance_job(&shared)?;
        let root = shared.config.resolve(root_id.as_deref())?;
        ready(&shared, &root)?;
        let record = instance_delete::history(Path::new(&root.path), &shared.project, &root.id)?
            .into_iter()
            .find(|record| record.operation_id == operation_id)
            .ok_or("实例恢复记录已改变，请刷新")?;
        if !record.can_restore || record.revision.as_deref() != Some(&revision) {
            return Err(record
                .warning
                .unwrap_or_else(|| "恢复文件或目标已改变，请刷新恢复记录".into()));
        }
        spawn(
            shared.clone(),
            root,
            record.id,
            tasks::TaskKind::InstanceRestore,
            "实例已恢复",
            move |shared, root, task| {
                let cancel = task.cancellation_token();
                instance_delete::undo(
                    Path::new(&root.path),
                    &shared.project,
                    &root.id,
                    &operation_id,
                    &revision,
                    &cancel,
                    |progress| delete_progress(task, progress),
                )
            },
        )
    })
    .await
    .map_err(|_| "启动实例恢复任务失败".to_string())?
}

#[tauri::command]
pub async fn instance_import_recover(
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    recover(state.inner().clone(), root_id, false).await
}
#[tauri::command]
pub async fn instance_delete_recover(
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<Value, String> {
    recover(state.inner().clone(), root_id, true).await
}

async fn recover(
    shared: Arc<Shared>,
    root_id: Option<String>,
    deletion: bool,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = shared.operations.lock().unwrap();
        crate::require_instance_job(&shared)?;
        let root = shared.config.resolve(root_id.as_deref())?;
        let path = PathBuf::from(&root.path);
        if crate::instance_rename_refs::pending_root(&shared.project)?.is_some() {
            return Err("请先恢复未完成的实例重命名".into());
        }
        crate::instance_rename::ensure_ready(&path)?;
        crate::instance_reset::ensure_ready(&path)?;
        crate::resource_ops::ensure_ready(&path)?;
        if deletion {
            instance_import::ensure_ready(&path)?;
        } else {
            instance_delete::ensure_project_ready(&shared.project)?;
            instance_delete::ensure_ready(&path)?;
        }
        let task = shared.tasks.admit(
            tasks::TaskTarget {
                root_id: root.id,
                root_path: root.path,
                instance_id: None,
            },
            tasks::TaskKind::ResourceOperation,
        )?;
        task.begin_finishing();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if deletion {
                instance_delete::recover_pending(&path, &shared.project)
            } else {
                instance_import::recover_pending(&path)
            }
        }))
        .unwrap_or_else(|_| Err("恢复实例操作意外退出，恢复记录和文件已保留".into()));
        crate::finish_instance_task(task, result.clone(), "已恢复未完成的实例操作");
        result
    })
    .await
    .map_err(|_| "恢复实例操作的任务意外退出".to_string())?
}

#[cfg(test)]
#[path = "instance_commands/import_plan_tests.rs"]
mod import_plan_tests;
