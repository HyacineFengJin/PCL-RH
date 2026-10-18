use crate::tasks::{
    TaskHandle, TaskKind, TaskOutcome, TaskProgress, TaskScope, TaskStage, TaskTarget, Tasks,
};
use pcl_install::{
    InstallRequest, InstallResult, Installer, Progress, ResolvedInstallRequest, VersionEntry,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize)]
pub struct DownloadStatus {
    pub kind: Option<TaskKind>,
    pub task_id: Option<String>,
    pub root_id: Option<String>,
    pub root_path: Option<String>,
    pub stage: String,
    pub phase: String,
    pub message: String,
    pub version: Option<String>,
    pub display_name: Option<String>,
    pub progress: f64,
    pub completed: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub network_bytes: u64,
    pub steps: Vec<pcl_install::InstallStep>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub can_cancel: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTaskList {
    pub revision: String,
    pub tasks: Vec<DownloadStatus>,
    pub running_limit: usize,
    pub pending_limit: usize,
}

impl Default for DownloadStatus {
    fn default() -> Self {
        Self {
            kind: None,
            task_id: None,
            root_id: None,
            root_path: None,
            stage: "idle".into(),
            phase: "idle".into(),
            message: "选择一个版本开始安装".into(),
            version: None,
            display_name: None,
            progress: 0.0,
            completed: 0,
            total: 0,
            bytes_done: 0,
            bytes_total: 0,
            network_bytes: 0,
            steps: Vec::new(),
            result: None,
            error: None,
            can_cancel: false,
        }
    }
}

pub struct Downloads {
    tasks: Arc<Tasks>,
    current: Mutex<Option<String>>,
    catalog: Mutex<Option<(Instant, Vec<VersionEntry>)>>,
}

/// Metadata and the processor runtime are prepared after this task acquires its
/// submitted scope. Keeping the resolved authority with its Installer prevents
/// a second metadata lookup from changing the plan after Java was selected.
pub(crate) struct PreparedInstall {
    pub installer: Installer,
    pub resolved: ResolvedInstallRequest,
    pub java_bytes: u64,
}

impl Downloads {
    pub fn new(tasks: Arc<Tasks>) -> Self {
        Self {
            tasks,
            current: Mutex::new(None),
            catalog: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> DownloadStatus {
        let current = self.current.lock().unwrap().clone();
        let Some(snapshot) = current.and_then(|id| self.tasks.snapshot(&id)) else {
            return DownloadStatus::default();
        };
        Self::project(snapshot)
    }

    pub fn snapshot_for(&self, task_id: &str) -> Option<DownloadStatus> {
        self.tasks.snapshot(task_id).map(Self::project)
    }

    #[cfg(test)]
    pub fn list_snapshot(&self) -> DownloadTaskList {
        Self::project_list(self.tasks.list_snapshot())
    }

    pub fn list_snapshot_with_roots(
        &self,
        roots: &[(String, PathBuf)],
    ) -> Result<(DownloadTaskList, Vec<String>), String> {
        let (list, blocked) = self.tasks.list_snapshot_with_roots(roots)?;
        Ok((Self::project_list(list), blocked))
    }

    fn project_list(list: crate::tasks::TaskListSnapshot) -> DownloadTaskList {
        DownloadTaskList {
            revision: list.revision,
            tasks: list.tasks.into_iter().map(Self::project).collect(),
            running_limit: list.running_limit,
            pending_limit: list.pending_limit,
        }
    }

    fn project(snapshot: crate::tasks::TaskSnapshot) -> DownloadStatus {
        DownloadStatus {
            kind: Some(snapshot.kind),
            task_id: Some(snapshot.id),
            root_id: Some(snapshot.root_id),
            root_path: Some(snapshot.root_path),
            stage: snapshot.stage.as_str().into(),
            phase: snapshot.phase,
            message: snapshot.message,
            version: snapshot.instance_id,
            display_name: snapshot.display_name,
            progress: snapshot.progress,
            completed: snapshot.completed,
            total: snapshot.total,
            bytes_done: snapshot.bytes_done,
            bytes_total: snapshot.bytes_total,
            network_bytes: snapshot.network_bytes,
            steps: snapshot.steps,
            result: snapshot.result,
            error: snapshot.error,
            can_cancel: snapshot.can_cancel,
        }
    }

    pub fn cancel_and_wait(&self, task_id: Option<&str>) -> Result<DownloadStatus, String> {
        let id = if let Some(id) = task_id {
            id.to_owned()
        } else {
            let active = self.tasks.active_all();
            if active.len() > 1 {
                return Err("有多个任务，请选择要取消的具体任务".into());
            }
            active
                .first()
                .map(|task| task.id.clone())
                .or_else(|| self.current.lock().unwrap().clone())
                .ok_or("找不到安装任务")?
        };
        let snapshot = self.tasks.snapshot(&id).ok_or("找不到此任务")?;
        if !matches!(
            snapshot.kind,
            TaskKind::Install
                | TaskKind::JavaInstall
                | TaskKind::ModpackInstall
                | TaskKind::ModpackPrepare
                | TaskKind::InstanceRepair
                | TaskKind::InstanceVerify
                | TaskKind::InstanceDamageRepair
                | TaskKind::InstanceDamageRecover
                | TaskKind::InstanceReset
                | TaskKind::InstanceExport
                | TaskKind::InstanceRename
                | TaskKind::InstanceImport
                | TaskKind::InstanceDelete
                | TaskKind::InstanceRestore
                | TaskKind::ResourceDownload
                | TaskKind::ResourceSave
                | TaskKind::ToolboxDownload
                | TaskKind::ResourceUpdate
                | TaskKind::ResourceUpdateRestore
                | TaskKind::ResourceOperation
                | TaskKind::LauncherLogs
        ) {
            return Err("此任务不在任务管理页面中".into());
        }
        // Always return the requested task. A newly admitted install must never
        // become the target of a late cancellation or its response.
        Ok(Self::project(self.tasks.cancel_and_wait(&id)?))
    }

    pub fn track(&self, task: &TaskHandle) {
        *self.current.lock().unwrap() = Some(task.id().to_owned());
        task.publish();
    }

    #[cfg(test)]
    pub fn active(&self) -> bool {
        self.tasks.active().is_some()
    }

    pub fn catalog(&self, refresh: bool) -> Result<Vec<VersionEntry>, String> {
        let mut cache = self.catalog.lock().unwrap();
        if let Some((time, entries)) = &*cache {
            if !refresh && time.elapsed() < Duration::from_secs(900) {
                return Ok(entries.clone());
            }
        }
        let entries = Installer::new()?.catalog()?;
        *cache = Some((Instant::now(), entries.clone()));
        Ok(entries)
    }

    pub fn start(
        self: &Arc<Self>,
        root: PathBuf,
        root_id: String,
        request: InstallRequest,
        scope: TaskScope,
        before_start: impl FnOnce(&TaskHandle) -> Result<PreparedInstall, String> + Send + 'static,
        on_complete: impl FnOnce(&InstallResult) -> Result<(), String> + Send + 'static,
    ) -> Result<String, String> {
        if !root.is_absolute() {
            return Err("游戏目录需要使用绝对路径，请先在设置中修改".into());
        }
        request.validate()?;
        let id = request.name.clone();
        let task = self.tasks.admit_queued(
            TaskTarget {
                root_id,
                root_path: root.to_string_lossy().into_owned(),
                instance_id: Some(id.clone()),
            },
            TaskKind::Install,
            scope,
        )?;
        let task_id = task.id().to_owned();
        *self.current.lock().unwrap() = Some(task_id.clone());
        // The admission listener may have read the legacy projection before its
        // latest ID was assigned. Publish it once more after committing that ID.
        task.publish();
        let downloads = self.clone();
        std::thread::Builder::new()
            .name(format!("pcl-install-{task_id}"))
            .spawn(move || {
                let cancel = task.cancellation_token();
                // Queuing owns the captured target but permits no installer
                // side effects. Recheck application binding/recovery after the
                // scope turn, with no operations lock held while waiting.
                let gate = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    task.wait_turn()?;
                    before_start(&task)
                }))
                .unwrap_or_else(|_| Err("安装开始前的目标检查意外退出".into()));
                let prepared = match gate {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        task.finish(
                            if matches!(
                                error.as_str(),
                                crate::tasks::CANCELLED
                                    | crate::java_download::CANCELLED
                                    | "安装已取消"
                                    | "下载已取消"
                            ) {
                                TaskOutcome::Failed(error)
                            } else {
                                TaskOutcome::Error(error)
                            },
                        );
                        return;
                    }
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    prepared.installer.install_resolved_request(
                        &root,
                        &prepared.resolved,
                        &cancel,
                        |mut progress| {
                            progress.network_bytes =
                                progress.network_bytes.saturating_add(prepared.java_bytes);
                            downloads.progress(&task, progress);
                        },
                    )
                }))
                .unwrap_or_else(|_| Err("安装任务意外退出，请重试".into()));
                match result {
                    Ok(result) => {
                        // The install has committed. Keep its writer lease while
                        // persisting the selection, and retain success if that fails.
                        task.begin_finishing();
                        let selection =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                on_complete(&result)
                            }))
                            .unwrap_or_else(|_| Err("自动选择版本的任务意外退出".into()));
                        let (message, error) = match selection {
                            Ok(()) => (format!("Minecraft {} 安装完成", result.id), None),
                            Err(error) => (
                                format!("安装完成，但自动选择版本失败：{error}"),
                                Some(error),
                            ),
                        };
                        task.finish(TaskOutcome::Complete {
                            result: Some(
                                serde_json::to_value(result)
                                    .expect("InstallResult contains only JSON-compatible fields"),
                            ),
                            message,
                            error,
                        });
                    }
                    Err(error) => {
                        if error.starts_with("取消清理失败：") {
                            task.finish(TaskOutcome::CleanupFailed(error));
                        } else if matches!(
                            error.as_str(),
                            crate::tasks::CANCELLED
                                | crate::java_download::CANCELLED
                                | "安装已取消"
                                | "下载已取消"
                        ) {
                            task.finish(TaskOutcome::Failed(error));
                        } else {
                            task.finish(TaskOutcome::Error(error));
                        }
                    }
                }
            })
            .map_err(|error| format!("无法启动安装任务：{error}"))?;
        Ok(task_id)
    }

    pub fn progress(&self, task: &TaskHandle, progress: Progress) {
        let stage = match progress.stage.as_str() {
            "downloading"
            | "pack-download"
            | "vanilla_libraries"
            | "vanilla_assets"
            | "vanilla_resources"
            | "component_download"
            | "component_metadata"
            | "component_main"
            | "component_libraries"
            | "game_libraries" => TaskStage::Downloading,
            "repair-download" | "damage-download" => TaskStage::Downloading,
            "pack-files" | "processing" | "installing" | "component_install"
            | "component_analyze" | "publishing" | "complete" | "game_install" | "game_support"
            | "reset_commit" | "reset_merge" | "export-archive" | "export-finalize"
            | "repair-verify" | "integrity-verify" | "damage-check" | "damage-commit"
            | "damage-recover" => TaskStage::Processing,
            _ => TaskStage::Preparing,
        };
        let (phase, message) = if progress.stage == "complete" {
            match self.tasks.snapshot(task.id()).map(|snapshot| snapshot.kind) {
                Some(TaskKind::InstanceRepair) => {
                    ("repair-verify".into(), "正在复核文件补全结果…".into())
                }
                Some(TaskKind::InstanceVerify) => {
                    ("integrity-verify".into(), "正在汇总依赖完整性结果…".into())
                }
                _ => ("installing".into(), "正在保存安装结果…".into()),
            }
        } else {
            (progress.stage, progress.message)
        };
        task.update(TaskProgress {
            stage,
            phase,
            message,
            completed: progress.completed,
            total: progress.total,
            bytes_done: progress.bytes_done,
            bytes_total: progress.bytes_total,
            network_bytes: progress.network_bytes,
            steps: progress.steps,
        });
    }

    #[cfg(test)]
    pub fn cancel(&self) {
        let current = self.current.lock().unwrap().clone();
        if let Some(id) = current {
            let _ = self.tasks.cancel(&id);
        }
    }
}

#[cfg(test)]
#[path = "downloads/scheduling_tests.rs"]
mod scheduling_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_name_keeps_target_identity_and_terminal_history_ownership() {
        let tasks = Arc::new(Tasks::new());
        let downloads = Downloads::new(tasks.clone());
        let target = |root: &str| TaskTarget {
            root_id: root.into(),
            root_path: format!("/fixture/{root}"),
            instance_id: Some("Example instance".into()),
        };
        let first = tasks
            .admit(target("a"), TaskKind::ResourceDownload)
            .unwrap();
        downloads.track(&first);
        first.set_resource_name("");
        first.set_resource_name(&"x".repeat(2049));
        assert!(downloads.snapshot().display_name.is_none());
        first.set_resource_name("Actual root resource");
        first.set_resource_name("Required dependency");
        let named = downloads.snapshot();
        assert_eq!(named.display_name.as_deref(), Some("Actual root resource"));
        assert_eq!(named.version.as_deref(), Some("Example instance"));
        assert_eq!(named.root_id.as_deref(), Some("a"));
        first.finish(TaskOutcome::Complete {
            result: None,
            message: "资源安装完成".into(),
            error: None,
        });
        let second = tasks
            .admit(target("b"), TaskKind::ResourceDownload)
            .unwrap();
        downloads.track(&second);
        second.set_resource_name("Next resource");
        first.set_resource_name("Late old callback");
        let old = downloads.cancel_and_wait(Some(first.id())).unwrap();
        assert_eq!(old.stage, "complete");
        assert_eq!(old.display_name.as_deref(), Some("Actual root resource"));
        assert_eq!(
            downloads.snapshot().display_name.as_deref(),
            Some("Next resource")
        );
        assert_eq!(tasks.list().len(), 2);
        assert!(!second
            .cancellation_token()
            .load(std::sync::atomic::Ordering::SeqCst));
        second.finish(TaskOutcome::Complete {
            result: None,
            message: "完成".into(),
            error: None,
        });
        let install = tasks.admit(target("c"), TaskKind::Install).unwrap();
        downloads.track(&install);
        install.set_resource_name("Must not rename an instance install");
        assert!(downloads.snapshot().display_name.is_none());
    }

    #[test]
    fn instance_job_projection_and_cancel_target_are_scoped() {
        for kind in [
            TaskKind::JavaInstall,
            TaskKind::InstanceDamageRepair,
            TaskKind::InstanceDamageRecover,
            TaskKind::InstanceVerify,
            TaskKind::InstanceReset,
            TaskKind::InstanceExport,
            TaskKind::InstanceRename,
            TaskKind::InstanceImport,
            TaskKind::InstanceDelete,
            TaskKind::InstanceRestore,
            TaskKind::ResourceDownload,
            TaskKind::ResourceSave,
            TaskKind::ToolboxDownload,
            TaskKind::ResourceUpdate,
            TaskKind::ResourceUpdateRestore,
            TaskKind::LauncherLogs,
        ] {
            let tasks = Arc::new(Tasks::new());
            let downloads = Downloads::new(tasks.clone());
            let task = tasks
                .admit(
                    TaskTarget {
                        root_id: "a".into(),
                        root_path: "/game-a".into(),
                        instance_id: Some("Example".into()),
                    },
                    kind,
                )
                .unwrap();
            let id = task.id().to_owned();
            downloads.track(&task);
            assert_eq!(downloads.snapshot().kind, Some(kind));
            assert_eq!(downloads.snapshot().root_id.as_deref(), Some("a"));
            tasks.cancel(&id).unwrap();
            assert!(tasks.active().is_some());
            task.finish(TaskOutcome::Failed("cancelled".into()));
            let result = downloads.cancel_and_wait(Some(&id)).unwrap();
            assert_eq!(result.stage, "cancelled");
            assert_eq!(result.task_id.as_deref(), Some(id.as_str()));
        }
    }

    #[test]
    fn scoped_cancel_response_does_not_cancel_a_new_install() {
        let tasks = Arc::new(Tasks::new());
        let downloads = Downloads::new(tasks.clone());
        let target = || TaskTarget {
            root_id: "fixture-root".into(),
            root_path: "/fixture".into(),
            instance_id: Some("Example".into()),
        };
        let first = tasks.admit(target(), TaskKind::Install).unwrap();
        let first_id = first.id().to_owned();
        tasks.cancel(&first_id).unwrap();
        first.finish(TaskOutcome::Failed("取消".into()));
        let second = tasks.admit(target(), TaskKind::Install).unwrap();
        *downloads.current.lock().unwrap() = Some(second.id().into());
        let response = downloads.cancel_and_wait(Some(&first_id)).unwrap();
        assert_eq!(response.task_id.as_deref(), Some(first_id.as_str()));
        assert_eq!(response.stage, "cancelled");
        assert_eq!(downloads.snapshot().task_id.as_deref(), Some(second.id()));
        assert!(!second
            .cancellation_token()
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn download_cancel_waits_for_the_exact_resource_operation_cleanup() {
        let tasks = Arc::new(Tasks::new());
        let downloads = Downloads::new(tasks.clone());
        let task = tasks
            .admit(
                TaskTarget {
                    root_id: "fixture-root".into(),
                    root_path: "/fixture".into(),
                    instance_id: Some("Example".into()),
                },
                TaskKind::ResourceOperation,
            )
            .unwrap();
        let id = task.id().to_owned();
        let cancel = task.cancellation_token();
        let worker = std::thread::spawn(move || {
            while !cancel.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::yield_now();
            }
            task.finish(TaskOutcome::Failed("Fixture cleanup complete".into()));
        });
        let response = downloads.cancel_and_wait(Some(&id)).unwrap();
        worker.join().unwrap();
        assert_eq!(response.task_id.as_deref(), Some(id.as_str()));
        assert_eq!(response.kind, Some(TaskKind::ResourceOperation));
        assert_eq!(response.stage, "cancelled");
    }

    #[test]
    fn legacy_projection_tracks_task_scope_cancellation_and_processing() {
        let tasks = Arc::new(Tasks::new());
        let downloads = Downloads::new(tasks.clone());
        downloads.cancel();
        assert_eq!(downloads.snapshot().stage, "idle");
        let task = tasks
            .admit(
                TaskTarget {
                    root_id: "fixture-root".into(),
                    root_path: format!("{}/work/task-tests/root", env!("CARGO_MANIFEST_DIR")),
                    instance_id: Some("fixture-version".into()),
                },
                TaskKind::Install,
            )
            .unwrap();
        *downloads.current.lock().unwrap() = Some(task.id().into());
        downloads.progress(
            &task,
            Progress {
                stage: "installing".into(),
                message: "安装".into(),
                completed: 2,
                total: 2,
                bytes_done: 100,
                bytes_total: 100,
                network_bytes: 20,
                steps: Vec::new(),
            },
        );
        let status = downloads.snapshot();
        assert_eq!(status.task_id.as_deref(), Some(task.id()));
        assert_eq!(status.root_id.as_deref(), Some("fixture-root"));
        assert_eq!(status.version.as_deref(), Some("fixture-version"));
        assert_eq!(status.stage, "processing");
        assert_eq!(status.phase, "installing");
        assert_eq!(status.network_bytes, 20);
        tasks.cancel(task.id()).unwrap();
        assert_eq!(downloads.snapshot().message, "正在取消安装…");
        assert!(downloads.active());
        task.finish(TaskOutcome::Failed("取消".into()));
        assert!(!downloads.active());
        assert_eq!(downloads.snapshot().stage, "cancelled");
        downloads.cancel();
        assert_eq!(downloads.snapshot().message, "安装已取消，未完成文件已清理");
    }
}
