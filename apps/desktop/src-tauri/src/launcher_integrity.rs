//! Explicit local dependency integrity task. The shared repair guard pins root,
//! instance, settings and shutdown ownership; the separate install API grants
//! no write or network authority. The task lease remains held until all file
//! readers have dropped, so cancellation and application close drain safely.
use crate::{
    launcher_repair::{self, CapturedRepair},
    tasks::{TaskHandle, TaskKind, TaskOutcome, TaskTarget},
    Shared,
};
use pcl_install::{IntegrityCheck, IntegrityReport};
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};

fn inspect(
    shared: &Shared,
    captured: &CapturedRepair,
    task: &TaskHandle,
) -> Result<IntegrityReport, String> {
    let request = IntegrityCheck::capture_bound(
        captured
            .directory
            .try_clone()
            .map_err(|error| error.to_string())?,
        Path::new(&captured.root.path),
        &captured.id,
    )?;
    launcher_repair::verify(shared, captured, task)?;
    request.verify(
        &task.cancellation_token(),
        &|| launcher_repair::verify_fast(shared, captured, task),
        &|| launcher_repair::verify(shared, captured, task),
        &|progress| shared.downloads.progress(task, progress),
    )
}

fn finish(task: &TaskHandle, result: Result<IntegrityReport, String>) {
    match result {
        Ok(report) => {
            task.begin_finishing();
            // Findings are a successful read-only check, never a repaired or
            // launch-ready claim. Keep the report under this terminal task ID.
            let message = format!(
                "依赖完整性检查完成：已验证 {} 项，缺失 {} 项，损坏 {} 项，无法校验 {} 项",
                report.files_verified,
                report.files_missing,
                report.files_corrupt,
                report.files_unverifiable
            );
            task.finish(TaskOutcome::Complete {
                result: Some(
                    serde_json::to_value(report).expect("integrity report is JSON-compatible"),
                ),
                message,
                error: None,
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
    run: impl FnOnce(&Shared, &CapturedRepair, &TaskHandle) -> Result<IntegrityReport, String>
        + Send
        + 'static,
) -> Result<String, String> {
    let (captured, task) = {
        let _operation = shared.operations.lock().unwrap();
        let captured =
            launcher_repair::capture_for(&shared, id, root_id, revision, TaskKind::InstanceVerify)?;
        let task = Arc::new(shared.tasks.admit(
            TaskTarget {
                root_id: captured.root.id.clone(),
                root_path: captured.root.path.clone(),
                instance_id: Some(captured.id.clone()),
            },
            TaskKind::InstanceVerify,
        )?);
        shared.downloads.track(&task);
        (captured, task)
    };
    let task_id = task.id().to_owned();
    let worker = task.clone();
    let spawn = std::thread::Builder::new()
        .name(format!("pcl-integrity-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if worker.cancellation_token().load(Ordering::SeqCst) {
                    return Err(crate::tasks::CANCELLED.into());
                }
                launcher_repair::verify(&shared, &captured, &worker)?;
                let report = run(&shared, &captured, &worker)?;
                launcher_repair::verify(&shared, &captured, &worker)?;
                if report.id != captured.id {
                    return Err("完整性检查结果属于其他实例，请重新检查".into());
                }
                Ok(report)
            }))
            .unwrap_or_else(|_| Err("依赖完整性检查线程意外退出，请重新检查".into()));
            finish(&worker, result);
        });
    if let Err(error) = spawn {
        let error = format!("无法启动依赖完整性检查：{error}");
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
    start_with(shared, id, root_id, revision, inspect)
}

#[cfg(test)]
#[path = "launcher_integrity_tests.rs"]
mod tests;
