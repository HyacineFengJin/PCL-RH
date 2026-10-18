//! Missing-file repair at the desktop task boundary.
//!
//! Submission pins the explicit registry root, instance, settings revision and
//! physical root descriptor under admission. The globally exclusive task owns
//! shared libraries and assets through worker cleanup. Slow metadata checks and
//! downloads run outside admission; their binding callback rechecks application
//! state before the engine publishes a file. Selection and settings are never
//! changed by completion, and incomplete repair is a visible terminal warning.

use crate::{
    config::GameRoot,
    ensure_instance_files_ready, instance_context, instance_reset, require_network_submission,
    resource_ops,
    tasks::{TaskHandle, TaskKind, TaskOutcome, TaskTarget},
    RunStatus, Shared,
};
use pcl_install::{Installer, MissingRepair, MissingRepairReport};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::{atomic::Ordering, Arc},
};

const CHANGED: &str = "操作期间实例、游戏目录或设置已变化，请重新检查实例依赖";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RootIdentity {
    device: u64,
    inode: u64,
    mode: u32,
}
impl RootIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode() & libc::S_IFMT,
        }
    }
}

pub(super) struct CapturedRepair {
    pub(super) id: String,
    pub(super) root: GameRoot,
    kind: TaskKind,
    settings_revision: String,
    close_generation: u64,
    run: RunStatus,
    pub(super) directory: File,
    identity: RootIdentity,
    factory: Arc<pcl_network::ClientFactory>,
    scheduler: Arc<pcl_network::DownloadScheduler>,
}

fn require_files_ready(shared: &Shared, root: &GameRoot, id: &str) -> Result<(), String> {
    let path = Path::new(&root.path);
    resource_ops::ensure_ready(path)?;
    instance_reset::ensure_ready(path)?;
    ensure_instance_files_ready(shared, root)?;
    crate::instance_delete::ensure_name_available(path, id)
}

/// Caller holds operations. The descriptor stays owned even if the registered
/// path is renamed, preventing inode reuse or a later active-root switch from
/// substituting a different directory during metadata capture.
fn capture(
    shared: &Shared,
    id: &str,
    root_id: &str,
    revision: &str,
) -> Result<CapturedRepair, String> {
    capture_for(shared, id, root_id, revision, TaskKind::InstanceRepair)
}

/// Both operations retain the same application/filesystem guard. Verification
/// requires exclusive read admission but never constructs a network client.
pub(super) fn capture_for(
    shared: &Shared,
    id: &str,
    root_id: &str,
    revision: &str,
    kind: TaskKind,
) -> Result<CapturedRepair, String> {
    match kind {
        TaskKind::InstanceRepair => require_network_submission(shared)?,
        TaskKind::InstanceVerify => {
            crate::require_instance_job(shared)?;
            if shared.launcher_updates.busy() {
                return Err("请等待启动器更新结束后检查实例依赖".into());
            }
        }
        _ => return Err("实例依赖操作类型无效".into()),
    }
    if shared.tasks.active().is_some() {
        return Err("请在当前文件操作结束后补全缺失文件".into());
    }
    let settings = shared.config.checked_snapshot()?;
    if settings.revision != revision {
        return Err("设置已改变，请刷新后重新补全缺失文件".into());
    }
    let (current, root) = instance_context(&shared.config, Some(root_id), id)?;
    if current.revision != settings.revision {
        return Err(CHANGED.into());
    }
    require_files_ready(shared, &root, id)?;
    let metadata = fs::symlink_metadata(&root.path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err("游戏目录不是文件夹".into());
    }
    let identity = RootIdentity::of(&metadata);
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&root.path)
        .map_err(|error| format!("无法固定补全目录：{error}"))?;
    if RootIdentity::of(&directory.metadata().map_err(|error| error.to_string())?) != identity {
        return Err(CHANGED.into());
    }
    Ok(CapturedRepair {
        id: id.into(),
        root,
        kind,
        settings_revision: settings.revision,
        close_generation: shared.close_generation.load(Ordering::SeqCst),
        run: shared.status.lock().unwrap().clone(),
        directory,
        identity,
        factory: pcl_network::snapshot(),
        scheduler: pcl_network::download_snapshot(),
    })
}

/// Transfer chunks only inspect in-memory generations. Disk settings and
/// recovery journals are checked separately before publication; reading those
/// on every chunk would serialize unrelated UI actions behind filesystem IO.
pub(super) fn verify_fast(
    shared: &Shared,
    captured: &CapturedRepair,
    task: &TaskHandle,
) -> Result<(), String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请稍后重新检查实例依赖".into());
    }
    if shared.config.snapshot().revision != captured.settings_revision
        || shared.close_generation.load(Ordering::SeqCst) != captured.close_generation
        || *shared.status.lock().unwrap() != captured.run
    {
        return Err(CHANGED.into());
    }
    let owner = shared.tasks.snapshot(task.id()).ok_or(CHANGED)?;
    if owner.kind != captured.kind
        || owner.root_id != captured.root.id
        || owner.instance_id.as_deref() != Some(&captured.id)
        || owner.stage.is_terminal()
    {
        return Err(CHANGED.into());
    }
    Ok(())
}

pub(super) fn verify(
    shared: &Shared,
    captured: &CapturedRepair,
    task: &TaskHandle,
) -> Result<(), String> {
    // The global writer lease excludes cooperating filesystem transactions.
    // Keep disk settings and recovery enumeration outside operations; neither
    // that mutex nor a disk check can fence arbitrary external filesystem edits.
    // A final short admission check catches launcher state changes during IO,
    // while the engine separately rechecks its retained filesystem descriptors.
    verify_fast(shared, captured, task)?;
    crate::launcher_monitor_runtime::require_idle(shared)?;
    let settings = shared.config.checked_snapshot()?;
    if settings.revision != captured.settings_revision {
        return Err(CHANGED.into());
    }
    verify_fast(shared, captured, task)?;
    let root = shared.config.resolve(Some(&captured.root.id))?;
    if root != captured.root {
        return Err(CHANGED.into());
    }
    let metadata = fs::symlink_metadata(&root.path).map_err(|_| CHANGED.to_owned())?;
    if RootIdentity::of(&metadata) != captured.identity
        || RootIdentity::of(
            &captured
                .directory
                .metadata()
                .map_err(|_| CHANGED.to_owned())?,
        ) != captured.identity
    {
        return Err(CHANGED.into());
    }
    require_files_ready(shared, &root, &captured.id)?;
    let _operation = shared.operations.lock().unwrap();
    verify_fast(shared, captured, task)?;
    shared.config.ensure_writable()?;
    if shared.launcher_updates.busy() {
        return Err("请等待启动器更新操作结束后再补全缺失文件".into());
    }
    Ok(())
}

fn repair(
    shared: &Shared,
    captured: &CapturedRepair,
    task: &TaskHandle,
) -> Result<MissingRepairReport, String> {
    // Engine capture validates the whole inheritance chain and all eligible
    // targets. Holding only the worker lease here keeps UI admission responsive.
    let request = MissingRepair::capture_bound(
        captured
            .directory
            .try_clone()
            .map_err(|error| error.to_string())?,
        Path::new(&captured.root.path),
        &captured.id,
    )?;
    verify(shared, captured, task)?;
    let installer = Installer::from_factory(&captured.factory)?
        .with_download_policy(captured.scheduler.clone());
    installer.repair_missing(
        &request,
        &task.cancellation_token(),
        &|| verify_fast(shared, captured, task),
        &|| verify(shared, captured, task),
        &|progress| shared.downloads.progress(task, progress),
    )
}

fn finish(task: &TaskHandle, result: Result<MissingRepairReport, String>) {
    match result {
        Ok(report) => {
            task.begin_finishing();
            let warning = (report.unsupported_total > 0).then(|| {
                format!(
                    "仍有 {} 项无法自动补全，请手动处理并重新检查启动环境",
                    report.unsupported_total
                )
            });
            let message = match &warning {
                Some(warning) => format!("已补全 {} 个文件；{warning}", report.files_downloaded),
                None => format!(
                    "已补全 {} 个缺失文件；请重新检查启动环境",
                    report.files_downloaded
                ),
            };
            let result =
                Some(serde_json::to_value(report).expect("repair report is JSON-compatible"));
            task.finish(match warning {
                Some(error) => TaskOutcome::Incomplete {
                    result,
                    message,
                    error,
                },
                None => TaskOutcome::Complete {
                    result,
                    message,
                    error: None,
                },
            });
        }
        Err(error) => {
            // A cancellation request alone must never hide a binding, network,
            // integrity or cleanup error. Only the engine's explicit result is
            // projected as cancelled; successful files are always retained.
            task.finish(if error.starts_with("取消清理失败：") {
                TaskOutcome::CleanupFailed(error)
            } else if matches!(error.as_str(), "安装已取消" | crate::tasks::CANCELLED) {
                TaskOutcome::Failed(error)
            } else {
                TaskOutcome::Error(error)
            });
        }
    }
}

fn execute_with(
    shared: &Shared,
    captured: &CapturedRepair,
    task: &TaskHandle,
    run: impl FnOnce(&Shared, &CapturedRepair, &TaskHandle) -> Result<MissingRepairReport, String>,
) -> Result<MissingRepairReport, String> {
    if task.cancellation_token().load(Ordering::SeqCst) {
        return Err(crate::tasks::CANCELLED.into());
    }
    verify(shared, captured, task)?;
    let report = run(shared, captured, task)?;
    verify(shared, captured, task)?;
    if report.id != captured.id {
        return Err("补全结果属于其他实例，请重新检查".into());
    }
    Ok(report)
}

fn start_with(
    shared: Arc<Shared>,
    id: &str,
    root_id: &str,
    revision: &str,
    run: impl FnOnce(&Shared, &CapturedRepair, &TaskHandle) -> Result<MissingRepairReport, String>
        + Send
        + 'static,
) -> Result<String, String> {
    let (captured, task) = {
        let _operation = shared.operations.lock().unwrap();
        let captured = capture(&shared, id, root_id, revision)?;
        let task = Arc::new(shared.tasks.admit(
            TaskTarget {
                root_id: captured.root.id.clone(),
                root_path: captured.root.path.clone(),
                instance_id: Some(captured.id.clone()),
            },
            TaskKind::InstanceRepair,
        )?);
        shared.downloads.track(&task);
        (captured, task)
    };
    let task_id = task.id().to_owned();
    let worker_task = task.clone();
    let spawn = std::thread::Builder::new()
        .name(format!("pcl-repair-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute_with(&shared, &captured, &worker_task, run)
            }))
            .unwrap_or_else(|_| {
                Err("文件补全工作线程意外退出；已完成的文件保留，请重新检查".into())
            });
            // Engine-owned temporary descriptors have already been dropped.
            // Terminal publication is the point that releases shared writers.
            finish(&worker_task, result);
        });
    if let Err(error) = spawn {
        let error = format!("无法启动文件补全任务：{error}");
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
    start_with(shared, id, root_id, revision, repair)
}

#[cfg(test)]
#[path = "launcher_repair/tests.rs"]
mod tests;
