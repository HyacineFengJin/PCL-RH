//! Explicit damaged-dependency repair owns a global writer lease. The engine
//! alone authorizes replacements from pinned declarations, keeps original-byte
//! backups and journals each publication. Only its captured operation may pass
//! its own pending guard; a separate recovery task never starts downloads.
use crate::{
    launcher_repair::{self, CapturedRepair},
    tasks::{TaskHandle, TaskKind, TaskOutcome, TaskTarget},
    Shared,
};
use pcl_install::{DamageRepair, Installer};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};

fn repair(shared: &Shared, captured: &CapturedRepair, task: &TaskHandle) -> Result<Value, String> {
    let request = DamageRepair::capture_bound(
        captured.directory.try_clone().map_err(|e| e.to_string())?,
        Path::new(&captured.root.path),
        &captured.id,
    )?;
    *captured.damage_operation.lock().unwrap() = Some(request.operation_id().to_owned());
    launcher_repair::verify(shared, captured, task)?;
    let report = Installer::from_factory(&captured.factory)?
        .with_download_policy(captured.scheduler.clone())
        .repair_damaged(
            &request,
            &task.cancellation_token(),
            &|| launcher_repair::verify_fast(shared, captured, task),
            &|| launcher_repair::verify(shared, captured, task),
            &|progress| shared.downloads.progress(task, progress),
        )?;
    if report.id != captured.id {
        return Err("损坏修复结果属于其他实例，请重新检查".into());
    }
    serde_json::to_value(report).map_err(|e| e.to_string())
}

fn recover_files(
    shared: &Shared,
    captured: &CapturedRepair,
    task: &TaskHandle,
) -> Result<Value, String> {
    let report = DamageRepair::recover_pending_bound(
        captured.directory.try_clone().map_err(|e| e.to_string())?,
        Path::new(&captured.root.path),
        &task.cancellation_token(),
        &|| launcher_repair::verify_fast(shared, captured, task),
        &|| launcher_repair::verify(shared, captured, task),
        &|progress| shared.downloads.progress(task, progress),
    )?;
    DamageRepair::ensure_ready(Path::new(&captured.root.path))?;
    Ok(json!({"recovered": report.recovered}))
}

fn finish(task: &TaskHandle, result: Result<Value, String>, recovery: bool) {
    match result {
        Ok(result) => {
            task.begin_finishing();
            if recovery {
                task.finish(TaskOutcome::Complete {
                    result: Some(result),
                    message: "未完成的文件修复已恢复，原件备份保留".into(),
                    error: None,
                });
                return;
            }
            // Keep the complete report visible: repaired does not imply that
            // absent or undeclared files have also been repaired.
            let repaired = result["files_repaired"].as_u64().unwrap_or(0);
            let warning = result["unsupported_total"].as_u64().unwrap_or(0) > 0
                || result["files_missing"].as_u64().unwrap_or(0) > 0
                || result["assets_enumerated"].as_bool() != Some(true);
            let message = if repaired > 0 {
                format!("已修复 {repaired} 个损坏文件，原件备份已保留")
            } else {
                "损坏依赖检查完成，未替换文件".into()
            };
            task.finish(if warning {
                TaskOutcome::Incomplete {
                    result: Some(result),
                    message,
                    error: "仍有缺失或无法修复的依赖，请查看修复结果".into(),
                }
            } else {
                TaskOutcome::Complete {
                    result: Some(result),
                    message,
                    error: None,
                }
            });
        }
        Err(error) => {
            task.finish(
                if matches!(error.as_str(), "安装已取消" | crate::tasks::CANCELLED) {
                    TaskOutcome::Failed(error)
                } else {
                    TaskOutcome::Error(error)
                },
            );
        }
    }
}

fn start_with(
    shared: Arc<Shared>,
    id: &str,
    root_id: &str,
    revision: &str,
    recovery: bool,
    run: impl FnOnce(&Shared, &CapturedRepair, &TaskHandle) -> Result<Value, String> + Send + 'static,
) -> Result<String, String> {
    let (captured, task) = {
        let _operation = shared.operations.lock().unwrap();
        let kind = if recovery {
            TaskKind::InstanceDamageRecover
        } else {
            TaskKind::InstanceDamageRepair
        };
        let captured = launcher_repair::capture_for(&shared, id, root_id, revision, kind)?;
        let task = Arc::new(shared.tasks.admit(
            TaskTarget {
                root_id: captured.root.id.clone(),
                root_path: captured.root.path.clone(),
                instance_id: (!recovery).then(|| id.to_owned()),
            },
            kind,
        )?);
        shared.downloads.track(&task);
        (captured, task)
    };
    let task_id = task.id().to_owned();
    let worker = task.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("pcl-damage-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if worker.cancellation_token().load(Ordering::SeqCst) {
                    return Err(crate::tasks::CANCELLED.into());
                }
                launcher_repair::verify(&shared, &captured, &worker)?;
                let report = run(&shared, &captured, &worker)?;
                launcher_repair::verify(&shared, &captured, &worker)?;
                Ok(report)
            }))
            .unwrap_or_else(|_| Err("文件修复线程意外退出，请先恢复未完成修复".into()));
            finish(&worker, result, recovery);
        })
    {
        let error = format!("无法启动文件修复任务：{error}");
        task.finish(TaskOutcome::Error(error.clone()));
        return Err(error);
    }
    Ok(task_id)
}

pub(super) fn start(
    shared: Arc<Shared>,
    id: &str,
    root_id: &str,
    revision: &str,
) -> Result<String, String> {
    start_with(shared, id, root_id, revision, false, repair)
}
pub(super) fn recover(
    shared: Arc<Shared>,
    root_id: &str,
    revision: &str,
) -> Result<String, String> {
    start_with(shared, "", root_id, revision, true, recover_files)
}

#[cfg(test)]
#[path = "launcher_damage_tests.rs"]
mod tests;
