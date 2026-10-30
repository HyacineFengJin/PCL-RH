//! Java preparation belongs to the already admitted installation. Its writer
//! scope must include the managed runtime store before queue admission. Only a
//! real processor batch discovers Java; its guarded official acquisition keeps
//! the caller's task, cancellation token and frozen network policy. Managed
//! runtimes are already discovered by core, so no settings write is necessary.
use crate::{
    config::{GameRoot, Settings},
    java_download, launcher_monitor_runtime,
    tasks::{TaskHandle, TaskKind, TaskProgress},
    Shared,
};
use pcl_core::{
    java::{JavaCatalog, JavaSelection},
    retained_fs::{Directory, Identity},
};
use pcl_install::{Installer, ProcessorJavaRequirement, ResolvedInstallRequest};
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};

#[cfg(test)]
use std::fs;
#[cfg(target_os = "linux")]
use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};

type Result<T> = std::result::Result<T, String>;
const CHANGED: &str = "安装期间游戏目录、设置或启动状态已变化，请重新提交";

/// Linux retains its original final-component admission policy, including
/// existing parent aliases and no openat2 premise. Other native hosts use the
/// provider's literal-directory port; ConfigStore::resolve supplies canonical
/// physical root paths before an installation captures this context.
fn captured_directory(path: &Path) -> Result<Directory> {
    #[cfg(target_os = "linux")]
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("无法固定安装目录：{error}"))?;
    #[cfg(not(target_os = "linux"))]
    let file = crate::launcher_local::filesystem::Dir::absolute(path)?.0;
    Directory::from_file(file).map_err(|error| format!("无法固定原生安装目录：{error}"))
}

pub(crate) struct Inputs {
    pub settings: Settings,
    pub preferences_revision: String,
    pub factory: Arc<pcl_network::ClientFactory>,
    pub scheduler: Arc<pcl_network::DownloadScheduler>,
}

pub(crate) struct Context {
    root: GameRoot,
    id: String,
    kind: TaskKind,
    inputs: Inputs,
    java_policy: (String, JavaSelection, Vec<String>),
    close_generation: u64,
    idle: launcher_monitor_runtime::IdleSnapshot,
    directory: Directory,
    identity: Identity,
    store_binding: Result<java_download::StoreBinding>,
    network_bytes: u64,
}

impl Context {
    /// Caller holds operations after normal native submission gates. Target
    /// root is independent of the active-root projection in captured settings.
    pub(crate) fn capture(
        shared: &Shared,
        root: &GameRoot,
        id: &str,
        kind: TaskKind,
        inputs: Inputs,
    ) -> Result<Self> {
        pcl_core::identifier(id)?;
        if !matches!(
            kind,
            TaskKind::Install | TaskKind::ModpackInstall | TaskKind::InstanceReset
        ) {
            return Err("安装器 Java 任务类型无效".into());
        }
        if shared.config.snapshot().revision != inputs.settings.revision
            || !shared
                .launcher_preferences
                .matches_revision(&inputs.preferences_revision)
        {
            return Err(CHANGED.into());
        }
        let java_policy = shared.config.installer_java_policy(&root.id, id)?;
        if java_policy.1 != *crate::java_service::effective_choice(&inputs.settings, root, id)
            || java_policy.2 != inputs.settings.java_paths
        {
            return Err(CHANGED.into());
        }
        let directory = captured_directory(Path::new(&root.path))?;
        let identity = directory.identity().map_err(|error| error.to_string())?;
        let captured = Self {
            root: root.clone(),
            id: id.into(),
            kind,
            inputs,
            java_policy,
            close_generation: shared.close_generation.load(Ordering::SeqCst),
            idle: launcher_monitor_runtime::capture_idle(shared)?,
            directory,
            identity,
            store_binding: java_download::StoreBinding::capture(&shared.project),
            network_bytes: 0,
        };
        captured.verify_binding(shared)?;
        Ok(captured)
    }

    fn verify_binding(&self, shared: &Shared) -> Result<()> {
        if shared.closing.load(Ordering::SeqCst)
            || shared.close_generation.load(Ordering::SeqCst) != self.close_generation
            || shared
                .config
                .installer_java_policy(&self.root.id, &self.id)?
                != self.java_policy
        {
            return Err(CHANGED.into());
        }
        launcher_monitor_runtime::verify_idle(shared, &self.idle)
    }

    /// Transfer/scheduler checks use only in-memory state. Genuine operation
    /// errors are returned by their producer before any later binding check.
    pub(crate) fn verify_fast(&self, shared: &Shared, task: &TaskHandle) -> Result<()> {
        if task.cancellation_token().load(Ordering::SeqCst) {
            return Err(crate::tasks::CANCELLED.into());
        }
        self.verify_binding(shared)?;
        let owner = shared.tasks.snapshot(task.id()).ok_or(CHANGED)?;
        if owner.kind != self.kind
            || owner.root_id != self.root.id
            || owner.root_path != self.root.path
            || owner.instance_id.as_deref() != Some(self.id.as_str())
            || owner.stage.is_terminal()
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }

    /// Caller recovery/source checks stay operation-specific. This common guard
    /// checks settings, monitor generation and the captured registry/root FD;
    /// slow disk reads never hold operations. Stop calling it once the outer
    /// transaction has closed cancellation and entered its commit boundary.
    pub(crate) fn verify(&self, shared: &Shared, task: &TaskHandle) -> Result<()> {
        self.verify_fast(shared, task)?;
        shared.config.checked_snapshot()?;
        let preferences = shared.launcher_preferences.snapshot();
        if preferences.warning.is_some()
            || shared.config.resolve(Some(&self.root.id))?.path != self.root.path
        {
            return Err(CHANGED.into());
        }
        let actual = captured_directory(Path::new(&self.root.path))
            .map_err(|_| CHANGED.to_owned())?
            .identity()
            .map_err(|_| CHANGED.to_owned())?;
        let held = self.directory.identity().map_err(|_| CHANGED.to_owned())?;
        if actual != self.identity || held != self.identity {
            return Err(CHANGED.into());
        }
        launcher_monitor_runtime::require_idle(shared)?;
        let _operation = shared.operations.lock().unwrap();
        self.verify_fast(shared, task)?;
        shared.config.ensure_writable()?;
        if shared.launcher_updates.busy() {
            return Err("请等待启动器更新操作结束后再安装".into());
        }
        Ok(())
    }

    pub(crate) fn network_bytes(&self) -> u64 {
        self.network_bytes
    }

    pub(crate) fn ensure(
        &mut self,
        shared: &Shared,
        task: &TaskHandle,
        installer: Installer,
        resolved: &ResolvedInstallRequest,
    ) -> Result<Installer> {
        let root_path = self.root.path.clone();
        let java_paths = self.java_policy.2.clone();
        self.ensure_using(
            shared,
            task,
            installer,
            resolved.processor_java_requirement()?,
            || pcl_core::java::catalog(&shared.project, Some(Path::new(&root_path)), &java_paths),
            |context, requirement| {
                context.verify_fast(shared, task)?;
                let store = context
                    .store_binding
                    .as_ref()
                    .map_err(Clone::clone)?
                    .open()?;
                let http = java_download::http(
                    context.inputs.factory.clone(),
                    context.inputs.scheduler.clone(),
                )?;
                tauri::async_runtime::block_on(java_download::install_recommended_guarded(
                    &store,
                    requirement.major,
                    http,
                    task,
                    &|| context.verify_fast(shared, task),
                    &|| context.verify(shared, task),
                ))
            },
        )
    }

    fn ensure_using(
        &mut self,
        shared: &Shared,
        task: &TaskHandle,
        installer: Installer,
        requirement: Option<ProcessorJavaRequirement>,
        discover: impl FnOnce() -> Result<JavaCatalog>,
        install: impl FnOnce(&Self, &ProcessorJavaRequirement) -> Result<java_download::Installed>,
    ) -> Result<Installer> {
        let Some(requirement) = requirement else {
            return Ok(installer);
        };
        self.verify(shared, task)?;
        let choice = &self.java_policy.1;
        if let JavaSelection::Manual { path } = choice {
            // The processor engine probes exactly this path and preserves its
            // specific unavailable/exact-major failure, with no auto fallback.
            return Ok(installer.with_java(path));
        }
        let catalog = discover()?;
        self.verify(shared, task)?;
        if let Ok(path) =
            pcl_core::java::select_from_catalog(&catalog, requirement.major, requirement.exact)
        {
            return Ok(installer.with_java(path));
        }
        let result = install(self, &requirement);
        self.network_bytes = shared
            .tasks
            .snapshot(task.id())
            .map_or(0, |snapshot| snapshot.network_bytes);
        let installed = result?;
        if installed.runtime.major != requirement.major
            || !installed.path.is_absolute()
            || installed.runtime.path != installed.path.to_string_lossy()
        {
            return Err("Java 下载结果与组件所需运行环境不符".into());
        }
        self.verify(shared, task)?;
        if let Some(mut snapshot) = shared.tasks.snapshot(task.id()) {
            for step in &mut snapshot.steps {
                if step.id.starts_with("java-") {
                    step.state = "complete".into();
                    step.progress = Some(1.0);
                }
            }
            task.update(TaskProgress {
                stage: snapshot.stage,
                phase: snapshot.phase,
                message: format!("Java {} 已安装，正在继续安装", requirement.major),
                completed: snapshot.completed,
                total: snapshot.total,
                bytes_done: snapshot.bytes_done,
                bytes_total: snapshot.bytes_total,
                network_bytes: snapshot.network_bytes,
                steps: snapshot.steps,
            });
        }
        Ok(installer.with_java(installed.path))
    }
}

#[cfg(test)]
mod tests;
