//! Automatic Java acquisition is part of the same captured launch generation.
//! Explicit manual choices are left to normal plan validation. A managed Java
//! registration merges into current settings and rebinds only its own revision.
use super::*;
use crate::java_download;
use pcl_core::{
    inspection::JavaRequirement,
    java::{JavaCatalog, JavaSelection},
};

impl PreparedLaunch {
    pub(crate) fn prepare_with<T>(
        &mut self,
        shared: &Shared,
        settings: &Settings,
        build: impl FnOnce(&Settings) -> Result<T, String>,
    ) -> Result<T, String> {
        let root_path = self.root.path.clone();
        let current = self.ensure_java_using(
            shared,
            settings,
            || {
                pcl_core::java::catalog(
                    &shared.project,
                    Some(Path::new(&root_path)),
                    &settings.java_paths,
                )
            },
            |prepared, requirement, task| {
                let store = java_download::Store::capture(&shared.project)?;
                let http =
                    java_download::http(pcl_network::snapshot(), pcl_network::download_snapshot())?;
                tauri::async_runtime::block_on(java_download::install_recommended_guarded(
                    &store,
                    requirement.major,
                    http,
                    task,
                    &|| prepared.verify_fast(shared),
                    &|| prepared.verify_java(shared, task, requirement),
                ))
            },
        )?;
        self.prepare_files_with(shared, || build(&current))
    }

    fn verify_java(
        &self,
        shared: &Shared,
        task: &TaskHandle,
        requirement: &JavaRequirement,
    ) -> Result<(), String> {
        self.verify(shared, Some(task))?;
        if pcl_core::inspection::java_requirement(Path::new(&self.root.path), &self.id)?
            != *requirement
        {
            return Err("Java 下载期间实例版本要求已变化，请重新启动".into());
        }
        self.verify_fast(shared)
    }

    fn ensure_java_using(
        &mut self,
        shared: &Shared,
        settings: &Settings,
        discover: impl FnOnce() -> Result<JavaCatalog, String>,
        install: impl FnOnce(
            &Self,
            &JavaRequirement,
            &TaskHandle,
        ) -> Result<java_download::Installed, String>,
    ) -> Result<Settings, String> {
        self.ready.store(false, Ordering::SeqCst);
        self.verify(shared, None)?;
        if settings.revision != self.settings_revision {
            return Err(CHANGED.into());
        }
        // A failed explicit path must retain its normal, specific plan error.
        if !matches!(
            crate::java_service::effective_choice(settings, &self.root, &self.id),
            JavaSelection::Auto
        ) {
            return Ok(settings.clone());
        }
        let requirement =
            pcl_core::inspection::java_requirement(Path::new(&self.root.path), &self.id)?;
        let catalog = discover()?;
        self.verify(shared, None)?;
        if pcl_core::inspection::java_requirement(Path::new(&self.root.path), &self.id)?
            != requirement
        {
            return Err("Java 检查期间实例版本要求已变化，请重新启动".into());
        }
        if pcl_core::java::select_from_catalog(&catalog, requirement.major, requirement.exact)
            .is_ok()
        {
            return Ok(settings.clone());
        }
        let task = {
            let operation = shared.operations.lock().unwrap();
            self.authorize_locked(shared, None)?;
            let task = shared.tasks.admit(
                TaskTarget {
                    root_id: self.root.id.clone(),
                    root_path: self.root.path.clone(),
                    instance_id: Some(self.id.clone()),
                },
                TaskKind::JavaInstall,
            )?;
            if let Err(error) = launcher_monitor_runtime::bind_preparation_cancel(
                shared,
                self.generation,
                task.cancellation_token(),
            ) {
                drop(operation);
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
        let mut installed_runtime = None;
        let result: Result<Settings, String> =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.verify_java(shared, &task, &requirement)?;
                let installed = install(self, &requirement, &task)?;
                if installed.runtime.major != requirement.major
                    || !installed.path.is_absolute()
                    || installed.runtime.path != installed.path.to_string_lossy()
                {
                    return Err("Java 下载结果与所需运行环境不符".into());
                }
                self.verify_java(shared, &task, &requirement)?;
                installed_runtime = Some(installed.runtime);
                let current = {
                    let _operation = shared.operations.lock().unwrap();
                    self.authorize_locked(shared, Some(&task))?;
                    // Revision comparison and append share the config mutex. This
                    // also catches a direct cooperating config writer between the
                    // short admission check and registration, without replaying
                    // the captured selections or accepting an unrelated revision.
                    let current = shared.config.register_java(
                        installed.path.display().to_string(),
                        &self.settings_revision,
                        &settings.root_id,
                    )?;
                    self.settings_revision = current.revision.clone();
                    current
                };
                self.verify_java(shared, &task, &requirement)?;
                task.begin_finishing();
                self.verify_fast(shared)?;
                Ok(current)
            }))
            .unwrap_or_else(|_| {
                Err("自动 Java 下载任务意外退出；已安装的运行环境保留，请重新启动".into())
            });
        match &result {
            Ok(_) => {
                task.finish(TaskOutcome::Complete {
                    result: installed_runtime.map(|runtime| serde_json::json!({"java":runtime})),
                    message: format!("Java {} 已安装，正在继续启动准备", requirement.major),
                    error: None,
                });
            }
            Err(error)
                if matches!(
                    error.as_str(),
                    java_download::CANCELLED | crate::tasks::CANCELLED | "下载已取消"
                ) =>
            {
                task.finish(TaskOutcome::Failed(error.clone()));
            }
            Err(error) => {
                task.finish(TaskOutcome::Error(error.clone()));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests;
