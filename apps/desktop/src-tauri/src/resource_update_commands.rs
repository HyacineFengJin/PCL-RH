//! Mod update command binding and task lifetime.
//!
//! Only local names and their scan tokens cross the client boundary. Provider
//! identities, replacement permissions and target facts are derived again by
//! the planner. A confirmed task owns metadata, transfer, publication and
//! rollback until the worker returns; navigation cannot retarget that lifetime.

use crate::{
    config::GameRoot, modrinth_install, resource_install_commands, resource_ops, tasks, Shared,
};
use pcl_install::InstallStep;
use serde_json::{json, Value};
use std::{path::Path, sync::atomic::Ordering, sync::Arc};
use tauri::State;

type Result<T> = std::result::Result<T, String>;

#[tauri::command]
pub async fn resource_update_check(
    root_id: Option<String>,
    id: String,
    state: State<'_, Arc<Shared>>,
) -> Result<modrinth_install::UpdateCheck> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let isolation = capture_isolation(&shared, root_id.as_deref(), &id)?;
        let root = &isolation.root;
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let result =
            tauri::async_runtime::block_on(modrinth_install::check_updates_with_isolation(
                Path::new(&root.path),
                &shared.project,
                &root.id,
                &id,
                &cancel,
                isolation.policy(&id),
                &isolation.revision,
            ))?;
        capture_again(&shared, root, &id)?;
        resource_install_commands::verify_isolation(
            &shared,
            root,
            &id,
            isolation.policy(&id),
            &isolation.revision,
        )?;
        Ok(result)
    })
    .await
    .map_err(|_| "模组更新检查意外退出".to_string())?
}

#[tauri::command]
pub async fn resource_update_plan(
    root_id: Option<String>,
    id: String,
    files: Vec<resource_ops::ResourceFile>,
    state: State<'_, Arc<Shared>>,
) -> Result<modrinth_install::UpdatePlan> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let isolation = capture_isolation(&shared, root_id.as_deref(), &id)?;
        let root = &isolation.root;
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let plan =
            tauri::async_runtime::block_on(modrinth_install::prepare_update_with_isolation(
                Path::new(&root.path),
                &shared.project,
                &root.id,
                &id,
                files,
                &cancel,
                isolation.policy(&id),
                &isolation.revision,
            ))?;
        modrinth_install::recheck_update_target(&plan, &cancel)?;
        capture_again(&shared, root, &id)?;
        resource_install_commands::verify_isolation(
            &shared,
            root,
            &id,
            isolation.policy(&id),
            &isolation.revision,
        )?;
        Ok(plan)
    })
    .await
    .map_err(|_| "模组更新方案检查意外退出".to_string())?
}

fn capture_isolation(
    shared: &Shared,
    root_id: Option<&str>,
    id: &str,
) -> Result<crate::config::IsolationSettings> {
    let _operation = shared.operations.lock().unwrap();
    let root = resource_install_commands::capture(shared, root_id, id)?;
    shared.config.isolation_settings(Some(&root.id))
}

fn capture(shared: &Shared, root_id: Option<&str>, id: &str) -> Result<GameRoot> {
    let _operation = shared.operations.lock().unwrap();
    resource_install_commands::capture(shared, root_id, id)
}

fn capture_again(shared: &Shared, root: &GameRoot, id: &str) -> Result<()> {
    let _operation = shared.operations.lock().unwrap();
    resource_install_commands::same_root(shared, root)?;
    resource_install_commands::capture(shared, Some(&root.id), id)?;
    Ok(())
}

#[tauri::command]
pub fn resource_update_start(
    root_id: Option<String>,
    id: String,
    files: Vec<resource_ops::ResourceFile>,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value> {
    start_request(
        state.inner().clone(),
        root_id.as_deref(),
        id,
        files,
        revision,
    )
}

/// Admit before metadata work. The worker re-derives the graph before obtaining
/// anonymous files, then keeps admission through any required rollback.
pub(super) fn start_request(
    shared: Arc<Shared>,
    root_id: Option<&str>,
    id: String,
    files: Vec<resource_ops::ResourceFile>,
    revision: String,
) -> Result<Value> {
    let _operation = shared.operations.lock().unwrap();
    let root = resource_install_commands::capture(&shared, root_id, &id)?;
    let isolation = shared.config.isolation_settings(Some(&root.id))?;
    if revision.is_empty() || revision.len() > 256 || files.is_empty() || files.len() > 128 {
        return Err("缺少有效的模组更新方案，请重新检查".into());
    }
    let task = admit(&shared, &root, &id, tasks::TaskKind::ResourceUpdate)?;
    let task_id = task.id().to_owned();
    let download_policy = pcl_network::download_snapshot();
    let worker = shared.clone();
    std::thread::Builder::new()
        .name(format!("pcl-mod-update-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                update_with_policy(
                    &worker,
                    &root,
                    &id,
                    files,
                    &revision,
                    &task,
                    download_policy,
                    &isolation,
                )
            }))
            .unwrap_or_else(|_| Err("模组更新意外退出，请检查资源恢复记录".into()));
            finish(task, result, "模组更新完成");
        })
        .map_err(|error| format!("无法启动模组更新任务：{error}"))?;
    Ok(json!({"id":task_id}))
}

fn admit(
    shared: &Shared,
    root: &GameRoot,
    id: &str,
    kind: tasks::TaskKind,
) -> Result<tasks::TaskHandle> {
    let task = shared.tasks.admit(
        tasks::TaskTarget {
            root_id: root.id.clone(),
            root_path: root.path.clone(),
            instance_id: Some(id.into()),
        },
        kind,
    )?;
    shared.downloads.track(&task);
    Ok(task)
}

fn update_with_policy(
    shared: &Shared,
    root: &GameRoot,
    id: &str,
    files: Vec<resource_ops::ResourceFile>,
    revision: &str,
    task: &tasks::TaskHandle,
    download_policy: Arc<pcl_network::DownloadScheduler>,
    isolation: &crate::config::IsolationSettings,
) -> Result<Value> {
    resource_install_commands::verify_isolation(
        shared,
        root,
        id,
        isolation.policy(id),
        &isolation.revision,
    )?;
    let cancel = task.cancellation_token();
    task.update(progress(0, 0, 0, 0));
    let batch = tauri::async_runtime::block_on(
        modrinth_install::download_update_request_with_policy_and_isolation(
            Path::new(&root.path),
            &shared.project,
            &root.id,
            id,
            files,
            revision,
            download_policy,
            &cancel,
            |p| transfer_progress(task, p),
            isolation.policy(id),
            &isolation.revision,
        ),
    )?;
    publish_batch(shared, root, id, batch, task)
}

/// Publication keeps the same task admission established before the transfer.
/// Keeping it separate from HTTP makes the scope/commit boundary explicit: only
/// verified descriptors and the planner's captured target enter the publisher.
pub(super) fn publish_batch(
    shared: &Shared,
    root: &GameRoot,
    id: &str,
    batch: modrinth_install::VerifiedUpdateBatch,
    task: &tasks::TaskHandle,
) -> Result<Value> {
    let cancel = task.cancellation_token();
    let modrinth_install::VerifiedUpdateBatch {
        plan,
        files,
        network_bytes,
    } = batch;
    let mut files = files
        .into_iter()
        .map(|file| resource_ops::VerifiedImport {
            kind: file.kind,
            file_name: file.file_name,
            file: file.file,
            size: file.size,
            sha512: file.sha512,
        })
        .collect::<Vec<_>>();
    let replacements = plan
        .replacements
        .iter()
        .map(|entry| resource_ops::Replacement {
            kind: entry.kind.clone(),
            old_file_name: entry.old_file_name.clone(),
            old_fingerprint: entry.old_fingerprint.clone(),
            old_sha512: entry.old_sha512.clone(),
            new_file_name: entry.new_file_name.clone(),
        })
        .collect::<Vec<_>>();
    let mut staging = progress(2, plan.download_bytes, plan.download_bytes, network_bytes);
    staging.phase = "resource-update-staging".into();
    staging.message = "下载已校验，正在暂存新文件与原文件备份…".into();
    task.update(staging);
    let mut commit = || {
        task.begin_finishing();
        if cancel.load(Ordering::SeqCst) {
            return Err(modrinth_install::CANCELLED.into());
        }
        // The publisher stages outside resource inventories. Recheck the whole
        // original snapshot before replacement; the combined pending guard
        // would reject this task's own durable journal and must not run here.
        modrinth_install::recheck_update_target(&plan, &cancel)?;
        let (policy, settings_revision) = modrinth_install::update_isolation(&plan);
        resource_install_commands::verify_isolation(shared, root, id, policy, settings_revision)?;
        commit_scope(shared, root, id)?;
        task.update(progress(
            2,
            plan.download_bytes,
            plan.download_bytes,
            network_bytes,
        ));
        Ok(())
    };
    let result = resource_ops::update_verified_batch_with_isolation(
        Path::new(&root.path),
        id,
        &mut files,
        &replacements,
        &cancel,
        &mut commit,
        &mut |_, _| {},
        modrinth_install::update_isolation(&plan).0,
    )?;
    task.update(progress(
        3,
        plan.download_bytes,
        plan.download_bytes,
        network_bytes,
    ));
    Ok(
        json!({"id":id,"changed":result.changed,"undo_id":result.undo_id,
        "message":result.message,"warnings":plan.warnings}),
    )
}

fn commit_scope(shared: &Shared, root: &GameRoot, id: &str) -> Result<()> {
    let _operation = shared.operations.lock().unwrap();
    resource_install_commands::same_root(shared, root)?;
    crate::ensure_instance_files_ready(shared, root)?;
    crate::instance_delete::ensure_name_available(Path::new(&root.path), id)?;
    crate::instance_reset::ensure_ready(Path::new(&root.path))?;
    resource_ops::ensure_local_resources_ready(Path::new(&root.path))
}

#[tauri::command]
pub async fn resource_update_history(
    root_id: Option<String>,
    id: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Vec<resource_ops::UpdateHistory>> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let root = capture(&shared, root_id.as_deref(), &id)?;
        let history = resource_ops::updates_history(Path::new(&root.path), &id)?;
        capture_again(&shared, &root, &id)?;
        Ok(history)
    })
    .await
    .map_err(|_| "读取模组更新恢复记录失败".to_string())?
}

#[tauri::command]
pub fn resource_update_restore(
    root_id: Option<String>,
    id: String,
    undo_id: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value> {
    start_restore(state.inner().clone(), root_id.as_deref(), id, undo_id)
}

pub(super) fn start_restore(
    shared: Arc<Shared>,
    root_id: Option<&str>,
    id: String,
    undo_id: String,
) -> Result<Value> {
    let _operation = shared.operations.lock().unwrap();
    let root = resource_install_commands::capture(&shared, root_id, &id)?;
    let isolation = shared.config.isolation_settings(Some(&root.id))?;
    if undo_id.is_empty() || undo_id.len() > 256 {
        return Err("无效的模组更新恢复记录".into());
    }
    let task = admit(&shared, &root, &id, tasks::TaskKind::ResourceUpdateRestore)?;
    let task_id = task.id().to_owned();
    let worker = shared.clone();
    std::thread::Builder::new()
        .name(format!("pcl-mod-restore-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let cancel = task.cancellation_token();
                task.update(restore_progress(false));
                let mut commit = || {
                    task.begin_finishing();
                    if cancel.load(Ordering::SeqCst) {
                        return Err(modrinth_install::CANCELLED.into());
                    }
                    resource_install_commands::verify_isolation(
                        &worker,
                        &root,
                        &id,
                        isolation.policy(&id),
                        &isolation.revision,
                    )?;
                    commit_scope(&worker, &root, &id)?;
                    task.update(restore_progress(true));
                    Ok(())
                };
                let result = resource_ops::restore_update(
                    Path::new(&root.path),
                    &id,
                    &undo_id,
                    &cancel,
                    &mut commit,
                )?;
                let mut done = restore_progress(true);
                done.steps
                    .iter_mut()
                    .for_each(|step| step.state = "complete".into());
                task.update(done);
                Ok(json!({"id":id,"changed":result.changed,"message":result.message}))
            }))
            .unwrap_or_else(|_| Err("模组恢复意外退出，恢复记录已保留".into()));
            finish(task, result, "已恢复更新前的模组文件");
        })
        .map_err(|error| format!("无法启动模组恢复任务：{error}"))?;
    Ok(json!({"id":task_id}))
}

fn steps(current: usize, ratio: Option<f64>) -> Vec<InstallStep> {
    [
        ("resource-update-check", "检查模组更新与必需前置"),
        ("resource-update-download", "下载并校验文件"),
        ("resource-update-publish", "替换文件并保存恢复记录"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (id, label))| InstallStep {
        id: id.into(),
        label: label.into(),
        state: if index < current {
            "complete"
        } else if index == current {
            "running"
        } else {
            "pending"
        }
        .into(),
        progress: if index == current { ratio } else { None },
    })
    .collect()
}

fn restore_progress(applying: bool) -> tasks::TaskProgress {
    tasks::TaskProgress {
        stage: tasks::TaskStage::Processing,
        phase: if applying {
            "resource-update-restore-apply"
        } else {
            "resource-update-restore-check"
        }
        .into(),
        message: if applying {
            "正在恢复原文件并保存恢复结果…"
        } else {
            "正在校验更新结果与原文件备份…"
        }
        .into(),
        completed: 0,
        total: 0,
        bytes_done: 0,
        bytes_total: 0,
        network_bytes: 0,
        steps: [
            ("resource-update-restore-check", "检查模组更新恢复记录"),
            ("resource-update-restore-apply", "恢复原文件并保存记录"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (id, label))| InstallStep {
            id: id.into(),
            label: label.into(),
            state: if applying && index == 0 {
                "complete"
            } else if index == usize::from(applying) {
                "running"
            } else {
                "pending"
            }
            .into(),
            progress: None,
        })
        .collect(),
    }
}

fn progress(
    current: usize,
    bytes_done: u64,
    bytes_total: u64,
    network_bytes: u64,
) -> tasks::TaskProgress {
    tasks::TaskProgress {
        stage: if current == 0 {
            tasks::TaskStage::Preparing
        } else {
            tasks::TaskStage::Processing
        },
        phase: if current == 0 {
            "resource-update-check"
        } else {
            "resource-update-publish"
        }
        .into(),
        message: if current == 0 {
            "正在检查模组更新与必需前置…"
        } else if current == 3 {
            "模组文件已更新，原文件已保留供恢复"
        } else {
            "正在替换文件并保存恢复记录…"
        }
        .into(),
        completed: 0,
        total: 0,
        bytes_done,
        bytes_total,
        network_bytes,
        steps: steps(current, None),
    }
}

fn transfer_progress(task: &tasks::TaskHandle, download: modrinth_install::DownloadProgress) {
    let metadata = download.phase == "metadata";
    let ratio = (download.bytes_total > 0)
        .then(|| download.bytes_done as f64 / download.bytes_total as f64);
    let mut state = progress(
        if metadata { 0 } else { 1 },
        download.bytes_done,
        download.bytes_total,
        download.network_bytes,
    );
    state.stage = if metadata {
        tasks::TaskStage::Preparing
    } else {
        tasks::TaskStage::Downloading
    };
    state.phase = format!("resource-update-{}", download.phase);
    state.message = download.message;
    state.completed = download.completed;
    state.total = download.total;
    state.steps = steps(if metadata { 0 } else { 1 }, ratio);
    task.update(state);
}

fn finish(task: tasks::TaskHandle, result: Result<Value>, message: &str) {
    match result {
        Ok(value) => {
            task.finish(tasks::TaskOutcome::Complete {
                result: Some(value),
                message: message.into(),
                error: None,
            });
        }
        Err(error) if error == modrinth_install::CANCELLED => {
            task.finish(tasks::TaskOutcome::Failed(error));
        }
        Err(error) => {
            task.finish(tasks::TaskOutcome::Error(error));
        }
    }
}

#[cfg(test)]
#[path = "resource_update_commands/tests.rs"]
mod tests;
