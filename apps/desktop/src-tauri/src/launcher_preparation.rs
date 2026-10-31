//! One launch preparation, including optional missing-file repair. Slow reads,
//! transfer scheduling and launch-plan construction never hold admission. The
//! repair task retains its global writer lease through plan reconstruction.
use crate::{
    config::{GameRoot, Settings},
    ensure_instance_files_ready, instance_reset,
    launcher_monitor_runtime::{self, Generation},
    resource_ops,
    tasks::{TaskHandle, TaskKind, TaskOutcome, TaskTarget},
    Shared,
};
use pcl_core::content_scope::IsolationPolicy;
use pcl_install::{Installer, MissingRepair, MissingRepairReport};
use std::{
    fs::File,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        MutexGuard,
    },
};

use crate::launcher_local::filesystem::DirectoryIdentity;
#[cfg(not(target_os = "linux"))]
use pcl_core::retained_fs::Directory;
#[cfg(target_os = "linux")]
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

#[cfg(all(test, not(target_os = "linux")))]
use std::fs;
const CHANGED: &str = "启动准备期间实例、游戏目录或设置已变化，请重新启动";

mod java;

pub(super) struct PreparedLaunch {
    id: String,
    root: GameRoot,
    settings_revision: String,
    preferences_revision: String,
    close_generation: u64,
    generation: Generation,
    isolation: IsolationPolicy,
    directory: File,
    identity: DirectoryIdentity,
    ready: AtomicBool,
}

pub(super) struct SpawnAuthorization<'a> {
    _operation: MutexGuard<'a, ()>,
    _preparation: launcher_monitor_runtime::PreparationAdmission<'a>,
}

/// Caller has already begun this preparation under operations. This captures
/// only a root descriptor and immutable policy identities, never game contents.
pub(super) fn capture(
    shared: &Shared,
    id: &str,
    root: &GameRoot,
    settings: &Settings,
    preferences_revision: &str,
    generation: Generation,
    isolation: IsolationPolicy,
) -> Result<PreparedLaunch, String> {
    pcl_core::identifier(id)?;
    #[cfg(target_os = "linux")]
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&root.path)
        .map_err(|error| format!("无法固定启动目录：{error}"))?;
    #[cfg(target_os = "linux")]
    let identity = {
        let metadata = directory.metadata().map_err(|error| error.to_string())?;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(target_os = "linux"))]
    let (directory, identity) = {
        let directory = Directory::open_absolute(Path::new(&root.path))
            .map_err(|error| format!("无法固定启动目录：{error}"))?;
        let local = crate::launcher_local::filesystem::Dir(
            directory
                .try_clone_file()
                .map_err(|error| error.to_string())?,
        );
        (
            directory
                .try_clone_file()
                .map_err(|error| error.to_string())?,
            local.identity()?,
        )
    };
    let captured = PreparedLaunch {
        id: id.into(),
        root: root.clone(),
        settings_revision: settings.revision.clone(),
        preferences_revision: preferences_revision.into(),
        close_generation: shared.close_generation.load(Ordering::SeqCst),
        generation,
        isolation,
        directory,
        identity,
        ready: AtomicBool::new(false),
    };
    captured.verify_fast(shared)?;
    Ok(captured)
}

impl PreparedLaunch {
    fn verify_fast(&self, shared: &Shared) -> Result<(), String> {
        launcher_monitor_runtime::require_current_preparation(shared, self.generation)?;
        if shared.closing.load(Ordering::SeqCst)
            || shared.close_generation.load(Ordering::SeqCst) != self.close_generation
            || shared.config.snapshot().revision != self.settings_revision
            || !shared
                .launcher_preferences
                .matches_revision(&self.preferences_revision)
        {
            return Err(CHANGED.into());
        }
        let run = shared.status.lock().unwrap();
        if run.version.as_deref() != Some(&self.id)
            || run.root_id.as_deref() != Some(&self.root.id)
            || run.root_path.as_deref() != Some(&self.root.path)
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }

    fn authorize_locked(&self, shared: &Shared, owner: Option<&TaskHandle>) -> Result<(), String> {
        self.verify_fast(shared)?;
        shared.config.ensure_writable()?;
        if shared.launcher_updates.busy() {
            return Err("请等待启动器更新操作结束后再启动".into());
        }
        let active = shared.tasks.active_all();
        match owner {
            Some(owner)
                if active.len() == 1
                    && active[0].id == owner.id()
                    && matches!(
                        active[0].kind,
                        TaskKind::InstanceRepair | TaskKind::JavaInstall
                    )
                    && active[0].root_id == self.root.id
                    && active[0].instance_id.as_deref() == Some(&self.id) => {}
            None if active.is_empty() => {}
            _ => return Err("启动准备的文件任务归属已变化，请重新启动".into()),
        }
        Ok(())
    }

    fn verify(&self, shared: &Shared, owner: Option<&TaskHandle>) -> Result<(), String> {
        self.verify_fast(shared)?;
        if shared.config.checked_snapshot()?.revision != self.settings_revision {
            return Err(CHANGED.into());
        }
        let preferences = shared.launcher_preferences.snapshot();
        if preferences.warning.is_some() || preferences.revision != self.preferences_revision {
            return Err(CHANGED.into());
        }
        if shared.config.resolve(Some(&self.root.id))? != self.root {
            return Err(CHANGED.into());
        }
        #[cfg(target_os = "linux")]
        {
            let actual = fs::symlink_metadata(&self.root.path).map_err(|_| CHANGED.to_owned())?;
            let held = self.directory.metadata().map_err(|_| CHANGED.to_owned())?;
            if !actual.is_dir()
                || (actual.dev(), actual.ino()) != self.identity
                || (held.dev(), held.ino()) != self.identity
            {
                return Err(CHANGED.into());
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let actual = crate::launcher_local::filesystem::Dir(
                Directory::open_absolute(Path::new(&self.root.path))
                    .map_err(|_| CHANGED.to_owned())?
                    .try_clone_file()
                    .map_err(|_| CHANGED.to_owned())?,
            );
            let held = crate::launcher_local::filesystem::Dir(
                self.directory.try_clone().map_err(|_| CHANGED.to_owned())?,
            );
            if actual.identity()? != self.identity || held.identity()? != self.identity {
                return Err(CHANGED.into());
            }
        }
        let path = Path::new(&self.root.path);
        resource_ops::ensure_ready(path)?;
        instance_reset::ensure_ready(path)?;
        ensure_instance_files_ready(shared, &self.root)?;
        crate::instance_delete::ensure_name_available(path, &self.id)?;
        let _operation = shared.operations.lock().unwrap();
        self.authorize_locked(shared, owner)
    }

    /// Final short gate immediately before monitor submission. Does no disk IO.
    /// A stop already accepted for this generation always rejects submission.
    pub(super) fn authorize<'a>(
        &self,
        shared: &'a Shared,
    ) -> Result<SpawnAuthorization<'a>, String> {
        let operation = shared.operations.lock().unwrap();
        if !self.ready.load(Ordering::SeqCst) {
            return Err("启动准备尚未成功完成，请重新启动".into());
        }
        self.authorize_locked(shared, None)?;
        let preparation = launcher_monitor_runtime::authorize_preparation(shared, self.generation)?;
        Ok(SpawnAuthorization {
            _operation: operation,
            _preparation: preparation,
        })
    }

    fn prepare_files_with<T>(
        &self,
        shared: &Shared,
        build: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.prepare_using(shared, build, |installer, request, task| {
            installer.repair_missing(
                request,
                &task.cancellation_token(),
                &|| self.verify_fast(shared),
                &|| self.verify(shared, Some(task)),
                &|progress| shared.downloads.progress(task, progress),
            )
        })
    }

    fn prepare_using<T>(
        &self,
        shared: &Shared,
        build: impl FnOnce() -> Result<T, String>,
        repair: impl FnOnce(
            &Installer,
            &MissingRepair,
            &TaskHandle,
        ) -> Result<MissingRepairReport, String>,
    ) -> Result<T, String> {
        self.ready.store(false, Ordering::SeqCst);
        let build = || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(build))
                .unwrap_or_else(|_| Err("启动方案构建意外退出，请重新启动".into()))
        };
        self.verify(shared, None)?;
        let readiness = pcl_core::inspection::inspect_files_with_isolation(
            Path::new(&self.root.path),
            &self.id,
            self.isolation,
        )?;
        self.verify(shared, None)?;
        // A healthy launch does not initialize a network client or the repair
        // engine's Linux-only descriptor graph and bounded-directory policy.
        if readiness.missing_total == 0 {
            let result = build()?;
            self.verify(shared, None)?;
            self.ready.store(true, Ordering::SeqCst);
            return Ok(result);
        }
        let request = MissingRepair::capture_bound(
            self.directory
                .try_clone()
                .map_err(|error| error.to_string())?,
            Path::new(&self.root.path),
            &self.id,
        )?;
        self.verify(shared, None)?;
        if request.missing_count()? == 0 {
            let result = build()?;
            request.verify()?;
            self.verify(shared, None)?;
            self.ready.store(true, Ordering::SeqCst);
            return Ok(result);
        }
        let factory = pcl_network::snapshot();
        let scheduler = pcl_network::download_snapshot();
        let installer = Installer::from_factory(&factory)?.with_download_policy(scheduler);
        if installer.repairable_missing_count(&request)? == 0 {
            return Err("缺失依赖没有可信的自动补全来源，请检查启动环境".into());
        }
        let task = {
            let _operation = shared.operations.lock().unwrap();
            self.authorize_locked(shared, None)?;
            let task = shared.tasks.admit(
                TaskTarget {
                    root_id: self.root.id.clone(),
                    root_path: self.root.path.clone(),
                    instance_id: Some(self.id.clone()),
                },
                TaskKind::InstanceRepair,
            )?;
            if let Err(error) = launcher_monitor_runtime::bind_preparation_cancel(
                shared,
                self.generation,
                task.cancellation_token(),
            ) {
                // Admission already published a task. Close it explicitly so
                // a real binding error cannot be projected by Drop as either
                // an unexpected exit or an unrelated cancellation.
                drop(_operation);
                drop(request);
                task.finish(if error == crate::tasks::CANCELLED {
                    TaskOutcome::Failed(error.clone())
                } else {
                    TaskOutcome::Error(error.clone())
                });
                return Err(error);
            }
            shared.downloads.track(&task);
            task
        };
        let mut report = None;
        let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.verify(shared, Some(&task))?;
            let repaired = repair(&installer, &request, &task)?;
            if repaired.id != self.id {
                return Err("补全结果属于其他实例，请重新启动".into());
            }
            let unsupported = repaired.unsupported_total;
            report = Some(repaired);
            self.verify(shared, Some(&task))?;
            if unsupported != 0 {
                return Err(format!(
                    "仍有 {unsupported} 项依赖无法自动补全，请检查启动环境"
                ));
            }
            let value = build()?;
            request.verify()?;
            self.verify(shared, Some(&task))?;
            Ok(value)
        }))
        .unwrap_or_else(|_| Err("启动补全工作意外退出；已完成的文件保留，请重新启动".into()));
        // Cancellation/terminal publication never releases a writer while its
        // engine-owned descriptors or unpublished temporary files still live.
        drop(request);
        if result.is_ok() {
            task.begin_finishing();
            if let Err(error) = self.verify_fast(shared) {
                result = Err(error);
            }
        }
        let json = report
            .as_ref()
            .map(|report| serde_json::to_value(report).expect("repair report is JSON-compatible"));
        match &result {
            Ok(_) => {
                task.finish(TaskOutcome::Complete {
                    result: json,
                    message: format!(
                        "已补全 {} 个缺失文件，启动准备完成",
                        report.as_ref().map_or(0, |report| report.files_downloaded)
                    ),
                    error: None,
                });
            }
            Err(error) if error.starts_with("取消清理失败：") => {
                task.finish(TaskOutcome::CleanupFailed(error.clone()));
            }
            Err(error)
                if matches!(
                    error.as_str(),
                    crate::tasks::CANCELLED | "安装已取消" | "下载已取消"
                ) =>
            {
                task.finish(TaskOutcome::Failed(error.clone()));
            }
            Err(error) => {
                task.finish(TaskOutcome::Incomplete {
                    result: json,
                    message: error.clone(),
                    error: error.clone(),
                });
            }
        }
        if result.is_ok() {
            self.ready.store(true, Ordering::SeqCst);
        }
        result
    }
}

#[cfg(test)]
#[path = "launcher_preparation/tests.rs"]
mod tests;
