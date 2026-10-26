mod account_appearance;
mod account_commands;
mod accounts;
mod code_runner;
mod community_catalog;
mod community_curseforge;
mod community_details;
mod community_identity;
mod community_mcmod;
mod config;
mod document_io;
mod downloads;
mod editing;
mod experimental;
mod export_presets;
mod installer_java;
mod instance_commands;
mod instance_delete;
mod instance_export;
mod instance_import;
mod instance_log_commands;
mod instance_logs;
mod instance_meta;
mod instance_rename;
mod instance_rename_refs;
mod instance_rename_service;
mod instance_reset;
mod java_commands;
mod java_download;
mod java_download_commands;
mod java_service;
mod launcher_asset_commands;
mod launcher_assets;
mod launcher_commands;
mod launcher_damage;
mod launcher_delay;
mod launcher_discovery;
mod launcher_discovery_commands;
mod launcher_document;
mod launcher_failure;
mod launcher_failure_ai;
mod launcher_failure_analysis;
mod launcher_failure_commands;
mod launcher_favorite_commands;
mod launcher_favorites;
mod launcher_game_monitor;
mod launcher_inspection;
mod launcher_integrity;
mod launcher_local;
mod launcher_local_commands;
mod launcher_log_commands;
mod launcher_logs;
mod launcher_memory;
mod launcher_minecraft_update_commands;
mod launcher_minecraft_updates;
mod launcher_monitor_runtime;
mod launcher_prefs;
mod launcher_preparation;
mod launcher_repair;
mod launcher_repair_commands;
mod launcher_runtime;
mod launcher_update_commands;
mod launcher_updates;
mod launcher_visibility;
#[path = "local_resource_info/mod.rs"]
mod local_resource_info;
mod local_resource_info_commands;
mod modrinth_install;
mod platform;
pub mod plugin_host;
mod plugins;
mod product_plugins;
mod resource_details;
mod resource_digest;
mod resource_install_commands;
mod resource_ops;
mod resource_save;
mod resource_save_commands;
mod resource_update_commands;
mod rh_code;
mod root_removal;
mod tasks;
mod toolbox_download;
mod toolbox_download_commands;
mod toolbox_image_commands;
#[path = "toolbox_images/mod.rs"]
mod toolbox_images;
mod ui_catalog;
mod ui_data;
use config::{ConfigStore, GameRoot, RootSummary, Settings};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{Emitter, Manager, State};
#[derive(Clone, Serialize, serde::Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RunStatus {
    stage: String,
    message: String,
    version: Option<String>,
    pid: Option<u32>,
    exit_code: Option<i32>,
    root_id: Option<String>,
    root_path: Option<String>,
}
struct Shared {
    project: PathBuf,
    accounts: Arc<accounts::Accounts>,
    downloads: Arc<downloads::Downloads>,
    tasks: Arc<tasks::Tasks>,
    config: ConfigStore,
    instance_metadata: instance_meta::MetadataStore,
    launcher_preferences: launcher_prefs::LauncherPreferencesStore,
    launcher_import: launcher_commands::ImportSession,
    launcher_assets: launcher_assets::AssetStore,
    launcher_local: launcher_runtime::LocalRuntime,
    launcher_updates: launcher_update_commands::Updater,
    product_plugins: Arc<product_plugins::Runtime>,
    code: std::sync::OnceLock<Arc<rh_code::Store>>,
    minecraft_updates: launcher_minecraft_updates::MinecraftUpdates,
    launcher_favorites: launcher_favorites::FavoriteStore,
    desktop: Arc<platform::Desktop>,
    operations: Mutex<()>,
    status: Mutex<RunStatus>,
    stop: AtomicBool,
    closing: AtomicBool,
    close_generation: AtomicU64,
    monitor: launcher_monitor_runtime::MonitorRuntime,
    resource_save: resource_save_commands::SaveSession,
    resource_confirmations: modrinth_install::ConfirmationCache,
    pack_confirmations: instance_import::mrpack::ConfirmationCache,
    toolbox_download: toolbox_download::DownloadSession,
    toolbox_images: toolbox_images::ImageSession,
    log: Mutex<Option<PathBuf>>,
}
impl Shared {
    /// Only Code document operations instantiate the store. Launcher services
    /// share the product runtime without loading its project schema or state.
    fn code_store(&self) -> &Arc<rh_code::Store> {
        self.code
            .get_or_init(|| Arc::new(rh_code::Store::with_runtime(self.product_plugins.clone())))
    }
    fn launcher_view(&self) -> launcher_prefs::LauncherPreferencesView {
        let mut view = self.launcher_preferences.snapshot();
        view.runtime_warning = self.launcher_local.warnings.message();
        view
    }
}

struct CloseDecision {
    active_task_ids: Vec<String>,
    wait_for_workers: bool,
    first_request: bool,
    event: LauncherClosing,
}

#[derive(Clone, Copy, Serialize)]
struct LauncherClosing {
    closing: bool,
    generation: u64,
}

// Call only while operations is held. The generation assigns rollback ownership
// and lets passive frontend subscribers reject events delivered out of order.
fn set_launcher_closing(state: &Shared, closing: bool) -> LauncherClosing {
    let generation = state.close_generation.fetch_add(1, Ordering::SeqCst) + 1;
    state.closing.store(closing, Ordering::SeqCst);
    state.minecraft_updates.invalidate();
    LauncherClosing {
        closing,
        generation,
    }
}

fn begin_launcher_close(state: &Shared) -> CloseDecision {
    // Closing participates in the same admission gate as short document commits
    // and task reservations. Even an idle close must refuse subsequent work;
    // checking for a worker before entering this gate leaves a shutdown race.
    let _operation = state.operations.lock().unwrap();
    let first_request = !state.closing.load(Ordering::SeqCst);
    let event = set_launcher_closing(state, true);
    // Network login can be retired immediately; a keyring publication already
    // admitted must finish before the process loses its public account update.
    state.accounts.cancel();
    // Capture queued workers too: they may own prepared file descriptors and
    // must release those resources before the native window is destroyed.
    let active_task_ids: Vec<_> = state
        .tasks
        .active_all()
        .into_iter()
        .map(|task| task.id)
        .collect();
    let wait_for_workers =
        !active_task_ids.is_empty() || state.launcher_updates.busy() || state.accounts.busy();
    CloseDecision {
        active_task_ids,
        wait_for_workers,
        first_request,
        event,
    }
}
#[derive(Serialize)]
struct Bootstrap {
    settings: Settings,
    launcher_preferences: launcher_prefs::LauncherPreferencesView,
    instances: Vec<InstanceView>,
    roots: Vec<RootSummary>,
    scan_issues: Vec<pcl_core::ScanIssue>,
    scan_error: Option<String>,
    reset_recovery_error: Option<String>,
    import_recovery_error: Option<String>,
    damage_recovery_error: Option<String>,
    resource_install_recovery_error: Option<String>,
    delete_recovery_error: Option<String>,
    delete_recovery_root_id: Option<String>,
    rename_recovery_error: Option<String>,
    rename_recovery_root_id: Option<String>,
    config_warning: Option<String>,
    status: RunStatus,
    auth: accounts::AuthState,
}
#[derive(Serialize)]
struct InstanceView {
    #[serde(flatten)]
    instance: pcl_core::Instance,
    metadata: instance_meta::Metadata,
    metadata_revision: String,
}
fn project() -> PathBuf {
    std::env::var_os("PCL_LINUX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .canonicalize()
        .expect("项目路径不存在")
}
fn bootstrap_view(s: &Shared) -> Bootstrap {
    let _ = launcher_monitor_runtime::refresh(s);
    let (settings, roots) = s.config.view();
    // Bind scanning and metadata to the same directory even if a registered
    // alias is retargeted while this read is running.
    let canonical = Path::new(&settings.root).canonicalize().ok();
    let scan_path = canonical.as_deref().unwrap_or(Path::new(&settings.root));
    let (instances, mut scan_issues, scan_error) =
        match pcl_core::scan_instances_report_with_isolation(scan_path, |id| {
            settings.resolved_isolation(id)
        }) {
            Ok(report) => (report.instances, report.issues, None),
            Err(error) => (Vec::new(), Vec::new(), Some(error)),
        };
    // Deleted names remain reserved. An external recreation must not acquire
    // retained metadata or become a target of stale selection/confirmation.
    let reservations = instance_delete::reserved_names(scan_path);
    let instances = instances
        .into_iter()
        .filter(|instance| {
            let error = match &reservations {
                Ok(names) if names.contains(&instance.id) => {
                    Some("此实例名称由可恢复删除记录保留，请先恢复实例或移开冲突目录".into())
                }
                Err(error) => Some(error.clone()),
                _ => None,
            };
            if let Some(message) = error {
                scan_issues.push(pcl_core::ScanIssue {
                    id: instance.id.clone(),
                    message,
                });
                false
            } else {
                true
            }
        })
        .map(|instance| {
            let view = s
                .instance_metadata
                .get(&settings.root_id, scan_path, &instance.id);
            InstanceView {
                metadata: view.metadata(),
                metadata_revision: view.revision,
                instance,
            }
        })
        .collect();
    let warnings: Vec<_> = [s.config.warning(), s.instance_metadata.warning()]
        .into_iter()
        .flatten()
        .collect();
    // Queued jobs have not touched a recovery journal yet. Suppression belongs
    // only to workers holding a turn, across every root rather than the latest job.
    let running = s.tasks.running_all();
    let resetting_here = running.iter().any(|task| {
        task.kind == tasks::TaskKind::InstanceReset && task.root_id == settings.root_id
    });
    let reset_recovery_error = if resetting_here {
        None
    } else {
        canonical
            .as_ref()
            .and_then(|path| instance_reset::ensure_ready(path).err())
    };
    let renaming = running
        .iter()
        .any(|task| task.kind == tasks::TaskKind::InstanceRename);
    let (rename_recovery_error, rename_recovery_root_id) = if renaming {
        (None, None)
    } else {
        match instance_rename_refs::pending_root(&s.project) {
            Ok(Some(path)) => {
                let id = roots
                    .iter()
                    .find(|root| {
                        Path::new(&root.path) == path
                            || Path::new(&root.path)
                                .canonicalize()
                                .is_ok_and(|canonical| canonical == path)
                    })
                    .map(|root| root.id.clone());
                (
                    Some("存在未完成的实例重命名，请先恢复后再操作或启动".into()),
                    id,
                )
            }
            Ok(None) => (
                canonical
                    .as_ref()
                    .and_then(|path| instance_rename::ensure_ready(path).err()),
                Some(settings.root_id.clone()),
            ),
            Err(error) => (Some(error), None),
        }
    };
    let damage_recovery_error = if running.iter().any(|task| {
        matches!(
            task.kind,
            tasks::TaskKind::InstanceDamageRepair | tasks::TaskKind::InstanceDamageRecover
        ) && task.root_id == settings.root_id
    }) {
        None
    } else {
        canonical
            .as_ref()
            .and_then(|path| pcl_install::DamageRepair::ensure_ready(path).err())
    };
    let import_recovery_error = if running.iter().any(|task| {
        task.kind == tasks::TaskKind::InstanceImport && task.root_id == settings.root_id
    }) {
        None
    } else {
        canonical
            .as_ref()
            .and_then(|path| s.pack_confirmations.ensure_ready(path).err())
    };
    let (delete_recovery_error, delete_recovery_root_id) = if running.iter().any(|task| {
        matches!(
            task.kind,
            tasks::TaskKind::InstanceDelete | tasks::TaskKind::InstanceRestore
        )
    }) {
        (None, None)
    } else {
        match instance_delete::pending_root(&s.project) {
            Ok(Some(path)) => (
                Some("存在未完成的实例删除或恢复，请先恢复后再操作".into()),
                roots
                    .iter()
                    .find(|root| {
                        Path::new(&root.path)
                            .canonicalize()
                            .is_ok_and(|canonical| canonical == path)
                    })
                    .map(|root| root.id.clone()),
            ),
            Ok(None) => (
                canonical
                    .as_ref()
                    .and_then(|path| instance_delete::ensure_ready(path).err()),
                Some(settings.root_id.clone()),
            ),
            Err(error) => (Some(error), None),
        }
    };
    let resource_install_recovery_error = if running.iter().any(|task| {
        matches!(
            task.kind,
            tasks::TaskKind::ResourceDownload
                | tasks::TaskKind::ResourceUpdate
                | tasks::TaskKind::ResourceUpdateRestore
                | tasks::TaskKind::ResourceOperation
        ) && task.root_id == settings.root_id
    }) {
        None
    } else {
        canonical
            .as_ref()
            .and_then(|path| resource_ops::ensure_verified_batches_ready(path).err())
    };
    Bootstrap {
        settings,
        launcher_preferences: s.launcher_view(),
        roots,
        instances,
        scan_issues,
        scan_error,
        reset_recovery_error,
        import_recovery_error,
        damage_recovery_error,
        resource_install_recovery_error,
        delete_recovery_error,
        delete_recovery_root_id,
        rename_recovery_error,
        rename_recovery_root_id,
        config_warning: (!warnings.is_empty()).then(|| warnings.join("\n")),
        status: s.status.lock().unwrap().clone(),
        auth: s.accounts.snapshot(),
    }
}

fn update_instance_metadata(
    s: &Shared,
    root_id: Option<&str>,
    id: &str,
    revision: &str,
    patch: instance_meta::MetadataPatch,
) -> Result<instance_meta::MetaView, String> {
    let _operation = s.operations.lock().unwrap();
    require_reference_write(s)?;
    s.config.ensure_writable()?;
    if s.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请重新打开后再修改实例信息".into());
    }
    if s.tasks.active().is_some() {
        return Err("请在文件操作结束后修改实例信息".into());
    }
    if matches!(
        s.status.lock().unwrap().stage.as_str(),
        "preparing" | "running"
    ) {
        return Err("请在游戏退出后修改实例信息".into());
    }
    pcl_core::identifier(id)?;
    let captured = s.config.isolation_settings(root_id)?;
    let root = &captured.root;
    ensure_instance_files_ready(s, root)?;
    instance_delete::ensure_name_available(Path::new(&root.path), id)?;
    pcl_core::scan_instance_with_isolation(Path::new(&root.path), id, captured.policy(id))?;
    if s.config.isolation_settings(Some(&root.id))?.revision != captured.revision {
        return Err("实例设置已变化，请重新读取列表".into());
    }
    s.instance_metadata
        .patch(&root.id, Path::new(&root.path), id, revision, patch)
}

#[tauri::command]
async fn instance_metadata_update(
    root_id: Option<String>,
    id: String,
    revision: String,
    patch: instance_meta::MetadataPatch,
    state: State<'_, Arc<Shared>>,
) -> Result<instance_meta::MetaView, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        update_instance_metadata(&s, root_id.as_deref(), &id, &revision, patch)
    })
    .await
    .map_err(|_| "保存实例信息的任务未能完成")?
}

#[tauri::command]
async fn instance_metadata_read(
    root_id: Option<String>,
    id: String,
    state: State<'_, Arc<Shared>>,
) -> Result<instance_meta::MetaView, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        pcl_core::identifier(&id)?;
        let captured = s.config.isolation_settings(root_id.as_deref())?;
        let root = &captured.root;
        instance_delete::ensure_name_available(Path::new(&root.path), &id)?;
        pcl_core::scan_instance_with_isolation(Path::new(&root.path), &id, captured.policy(&id))?;
        if s.config.isolation_settings(Some(&root.id))?.revision != captured.revision {
            return Err("实例设置已变化，请重新读取列表".into());
        }
        if let Some(warning) = s.instance_metadata.warning() {
            return Err(warning);
        }
        Ok(s.instance_metadata
            .get(&root.id, Path::new(&root.path), &id))
    })
    .await
    .map_err(|_| "读取实例信息的任务未能完成")?
}
#[tauri::command]
async fn bootstrap(state: State<'_, Arc<Shared>>) -> Result<Bootstrap, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || Ok(bootstrap_view(&s)))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
fn save_settings(settings: Settings, state: State<'_, Arc<Shared>>) -> Result<Settings, String> {
    let _operation = state.operations.lock().unwrap();
    require_reference_write(&state)?;
    save_game_settings(&state, settings)
}

/// Caller holds operations. Directory policy is shared by every file service.
/// Titles and arguments affect the next launch; changing directory selection
/// while a worker owns the old directory would split that ownership.
fn save_game_settings(state: &Shared, settings: Settings) -> Result<Settings, String> {
    let before = state.config.snapshot();
    if settings.isolation != before.isolation
        || settings.isolation_overrides != before.isolation_overrides
    {
        require_instance_job(state)?;
        if !state.tasks.active_all().is_empty() {
            return Err("请等待所有文件任务结束后再切换实例隔离".into());
        }
        for root in state.config.roots().into_iter().filter(|r| r.available) {
            let root = state.config.resolve(Some(&root.id))?;
            ensure_instance_files_ready(state, &root)?;
            resource_ops::ensure_ready(Path::new(&root.path))?;
            instance_reset::ensure_ready(Path::new(&root.path))?;
        }
    }
    state.config.save(settings)?;
    Ok(state.config.snapshot())
}

/// Rename recovery snapshots include launcher references. Keep all cooperating
/// writers out until those references and the physical directory agree.
fn require_reference_write(s: &Shared) -> Result<(), String> {
    launcher_monitor_runtime::refresh(s)?;
    if s.tasks
        .active_all()
        .iter()
        .any(|task| task.kind == tasks::TaskKind::InstanceRename)
    {
        return Err("请等待实例重命名完成后再保存或切换游戏目录".into());
    }
    instance_rename_refs::ensure_project_ready(&s.project)
}

#[tauri::command]
async fn roots_list(state: State<'_, Arc<Shared>>) -> Result<Vec<RootSummary>, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || s.config.roots())
        .await
        .map_err(|_| "读取游戏目录失败".into())
}

#[tauri::command]
async fn root_pick(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<platform::DirectoryChoice, String> {
    state
        .desktop
        .pick_root(window, PathBuf::from(state.config.snapshot().root))
        .await
}

#[derive(Serialize)]
struct RootRegistration {
    root: GameRoot,
    bootstrap: Bootstrap,
}

#[tauri::command]
async fn root_register(
    path: String,
    name: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<RootRegistration, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let root = {
            let _operation = s.operations.lock().unwrap();
            require_reference_write(&s)?;
            s.config.register(path, name)?
        };
        Ok(RootRegistration {
            root,
            bootstrap: bootstrap_view(&s),
        })
    })
    .await
    .map_err(|_| "登记游戏目录失败")?
}

#[tauri::command]
async fn root_select(id: String, state: State<'_, Arc<Shared>>) -> Result<Bootstrap, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        {
            let _operation = s.operations.lock().unwrap();
            require_reference_write(&s)?;
            s.config.select(&id)?;
        }
        Ok(bootstrap_view(&s))
    })
    .await
    .map_err(|_| "切换游戏目录失败")?
}

#[tauri::command]
async fn root_update(
    id: String,
    name: Option<String>,
    position: Option<usize>,
    state: State<'_, Arc<Shared>>,
) -> Result<Bootstrap, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        {
            let _operation = s.operations.lock().unwrap();
            require_reference_write(&s)?;
            s.config.update(&id, name, position)?;
        }
        Ok(bootstrap_view(&s))
    })
    .await
    .map_err(|_| "更新游戏目录失败")?
}

#[cfg(test)]
use root_removal::{require_root_removable, require_root_unused};

#[tauri::command]
async fn root_remove(id: String, state: State<'_, Arc<Shared>>) -> Result<Bootstrap, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        root_removal::remove(&s, &id)?;
        Ok(bootstrap_view(&s))
    })
    .await
    .map_err(|_| "移除游戏目录失败")?
}
#[tauri::command]
fn process_status(state: State<'_, Arc<Shared>>) -> RunStatus {
    let _ = launcher_monitor_runtime::refresh(&state);
    state.status.lock().unwrap().clone()
}
#[tauri::command]
async fn download_catalog(
    refresh: Option<bool>,
    state: State<'_, Arc<Shared>>,
) -> Result<Vec<pcl_install::VersionEntry>, String> {
    let downloads = state.downloads.clone();
    tauri::async_runtime::spawn_blocking(move || downloads.catalog(refresh.unwrap_or(false)))
        .await
        .map_err(|_| "获取版本列表的任务失败")?
}
#[tauri::command]
fn download_status(
    task_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<downloads::DownloadStatus, String> {
    match task_id {
        Some(id) => state
            .downloads
            .snapshot_for(&id)
            .ok_or_else(|| "下载任务不存在或已从历史记录移除".into()),
        None => Ok(state.downloads.snapshot()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadTasksView {
    #[serde(flatten)]
    collection: downloads::DownloadTaskList,
    blocked_root_ids: Vec<String>,
}

#[tauri::command]
fn download_tasks(state: State<'_, Arc<Shared>>) -> Result<DownloadTasksView, String> {
    download_tasks_view(&state)
}

fn download_tasks_view(state: &Shared) -> Result<DownloadTasksView, String> {
    let _operation = state.operations.lock().unwrap();
    let roots: Vec<_> = state
        .config
        .roots()
        .into_iter()
        .map(|root| (root.id, PathBuf::from(root.path)))
        .collect();
    // Scope projection and task snapshots share one registry revision. A
    // concurrent finish must not leave a stale busy root attached to new data.
    let (collection, blocked_root_ids) = state.downloads.list_snapshot_with_roots(&roots)?;
    Ok(DownloadTasksView {
        collection,
        blocked_root_ids,
    })
}
#[tauri::command]
async fn download_cancel(
    task_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<downloads::DownloadStatus, String> {
    let downloads = state.downloads.clone();
    tauri::async_runtime::spawn_blocking(move || downloads.cancel_and_wait(task_id.as_deref()))
        .await
        .map_err(|_| "取消安装的任务意外退出")?
}
#[tauri::command]
fn download_start(
    id: String,
    name: Option<String>,
    components: Option<Vec<pcl_install::ComponentSelection>>,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<String, String> {
    let _operation = state.operations.lock().unwrap();
    require_network_submission(&state)?;
    let root = state.config.resolve(root_id.as_deref())?;
    ensure_instance_files_ready(&state, &root)?;
    let request = pcl_install::InstallRequest {
        name: name.unwrap_or_else(|| id.clone()),
        minecraft: id,
        components: components.unwrap_or_default(),
    };
    request.validate()?;
    instance_commands::new_name(&state, &root, &request.name)?;
    let path = pcl_core::safe_join(Path::new(&root.path), format!("versions/{}", request.name))?;
    match fs::symlink_metadata(&path) {
        Ok(_) => return Err("实例名称已存在，请换一个名称".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("无法检查实例名称：{e}")),
    }
    // Installation uses the policy submitted with this job. Editing the
    // launcher later must not change its post-publication selection behavior.
    let policy = captured_launch_policy(&state)?;
    let auto_select = policy.preferences.auto_select_installed;
    let factory = pcl_network::snapshot();
    let scheduler = pcl_network::download_snapshot();
    let mut java = installer_java::Context::capture(
        &state,
        &root,
        &request.name,
        tasks::TaskKind::Install,
        installer_java::Inputs {
            settings: state.config.checked_snapshot()?,
            preferences_revision: policy.revision,
            factory: factory.clone(),
            scheduler: scheduler.clone(),
        },
    )?;
    let mut scope = tasks::TaskScope::root(Path::new(&root.path))?;
    // Metadata resolution waits for the queue. Reserve the shared runtime
    // store conservatively for processor-capable loaders before admission;
    // the resolved plan still decides whether Java work is actually needed.
    if request.components.iter().any(|component| {
        matches!(
            component.provider.to_ascii_lowercase().as_str(),
            "forge" | "neoforge"
        )
    }) {
        scope = scope.union(tasks::TaskScope::managed_runtime_store(&state.project)?)?;
    }
    let shared = state.inner().clone();
    let target = root.id.clone();
    let captured_root = root.clone();
    let captured_name = request.name.clone();
    let submitted = request.clone();
    let before_start = shared.clone();
    state.downloads.start(
        PathBuf::from(root.path),
        root.id,
        request,
        scope,
        move |task| {
            {
                let _operation = before_start.operations.lock().unwrap();
                require_network_submission(&before_start)?;
                let current = before_start.config.resolve(Some(&captured_root.id))?;
                if current.path != captured_root.path {
                    return Err("游戏目录登记已变化，请重新提交安装".into());
                }
                ensure_instance_files_ready(&before_start, &current)?;
                instance_reset::ensure_ready(Path::new(&current.path))?;
                resource_ops::ensure_verified_batches_ready(Path::new(&current.path))?;
                instance_commands::new_name(&before_start, &current, &captured_name)?;
            }
            java.verify(&before_start, task)?;
            let installer = pcl_install::Installer::from_factory(&factory)?
                .with_download_policy(scheduler)
                .with_project(&before_start.project);
            let resolved = installer.resolve_request(&submitted, &task.cancellation_token())?;
            let installer = java.ensure(&before_start, task, installer, &resolved)?;
            java.verify(&before_start, task)?;
            Ok(downloads::PreparedInstall {
                installer,
                resolved,
                java_bytes: java.network_bytes(),
            })
        },
        move |result| {
            if auto_select {
                shared.config.select_installed(&target, &result.id)
            } else {
                Ok(())
            }
        },
    )
}

#[tauri::command]
fn task_list(state: State<'_, Arc<Shared>>) -> Vec<tasks::TaskSnapshot> {
    state.tasks.list()
}

#[tauri::command]
fn task_snapshot(id: String, state: State<'_, Arc<Shared>>) -> Option<tasks::TaskSnapshot> {
    state.tasks.snapshot(&id)
}

#[tauri::command]
fn task_cancel(id: String, state: State<'_, Arc<Shared>>) -> Result<tasks::TaskSnapshot, String> {
    state.tasks.cancel(&id)
}

fn require_instance_job(s: &Shared) -> Result<(), String> {
    launcher_monitor_runtime::require_idle(s)?;
    s.config.ensure_writable()?;
    if s.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请重新打开后再操作实例".into());
    }
    if matches!(
        s.status.lock().unwrap().stage.as_str(),
        "preparing" | "running"
    ) {
        return Err("请在游戏退出后操作实例文件".into());
    }
    if s.tasks.active().is_some() {
        return Err("请在当前文件操作结束后操作实例".into());
    }
    Ok(())
}

// Caller holds operations. Network submissions may join the queue while other
// downloads run; their worker must revalidate its captured target after its
// turn is granted. Legacy mutations keep require_instance_job's exclusive gate.
fn require_network_submission(s: &Shared) -> Result<(), String> {
    launcher_monitor_runtime::require_idle(s)?;
    s.config.ensure_writable()?;
    if s.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请重新打开后再下载".into());
    }
    if s.launcher_updates.busy() {
        return Err("请等待启动器更新操作结束后再下载".into());
    }
    if matches!(
        s.status.lock().unwrap().stage.as_str(),
        "preparing" | "running"
    ) {
        return Err("请在游戏退出后下载或安装文件".into());
    }
    instance_rename_refs::ensure_project_ready(&s.project)
}

fn ensure_instance_files_ready(s: &Shared, root: &GameRoot) -> Result<(), String> {
    ensure_instance_files_ready_without_damage(s, root)?;
    pcl_install::DamageRepair::ensure_ready(Path::new(&root.path))
}

// Repair workers apply their own exact operation exception after this common
// guard. Ordinary callers must use ensure_instance_files_ready instead.
fn ensure_instance_files_ready_without_damage(s: &Shared, root: &GameRoot) -> Result<(), String> {
    instance_rename_refs::ensure_project_ready(&s.project)?;
    instance_rename::ensure_ready(Path::new(&root.path))?;
    instance_import::ensure_ready(Path::new(&root.path))?;
    instance_delete::ensure_ready(Path::new(&root.path))
}

#[derive(Deserialize)]
struct ResetSubmission {
    revision: String,
    id: String,
    components: Vec<pcl_install::ComponentSelection>,
}
#[derive(Deserialize)]
struct ExportSubmission {
    revision: String,
    instance_id: String,
    request: instance_export::ExportRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameSubmission {
    revision: String,
    id: String,
    new_name: String,
}

#[tauri::command]
async fn instance_rename_prepare(
    id: String,
    new_name: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let root = s.config.resolve(root_id.as_deref())?;
        let checked = instance_rename_service::prepare(&s, &root, &id, &new_name)?;
        let mut view = serde_json::to_value(checked.plan).map_err(|e| e.to_string())?;
        view["revision"] = checked.revision.into();
        Ok(view)
    })
    .await
    .map_err(|_| "检查改名方案的任务意外退出".to_string())?
}

#[tauri::command]
async fn instance_rename_start(
    plan: RenameSubmission,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let root = s.config.resolve(root_id.as_deref())?;
        let checked = instance_rename_service::prepare(&s, &root, &plan.id, &plan.new_name)?;
        if checked.revision != plan.revision {
            return Err("实例、引用或保存的资料已改变，请重新检查改名方案".into());
        }
        let task = s.tasks.admit(
            tasks::TaskTarget {
                root_id: root.id.clone(),
                root_path: root.path.clone(),
                instance_id: Some(plan.id),
            },
            tasks::TaskKind::InstanceRename,
        )?;
        let task_id = task.id().to_owned();
        s.downloads.track(&task);
        let worker = s.clone();
        std::thread::Builder::new()
            .name(format!("pcl-rename-{task_id}"))
            .spawn(move || {
                let result = instance_rename_service::execute(&worker, &root, checked, &task);
                finish_instance_task(task, result, "实例重命名完成，游戏内容已保留");
            })
            .map_err(|e| format!("无法启动改名任务：{e}"))?;
        Ok(serde_json::json!({"id": task_id}))
    })
    .await
    .map_err(|_| "启动改名任务失败".to_string())?
}

#[tauri::command]
async fn instance_rename_recover(
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let task;
        let root;
        {
            let _operation = s.operations.lock().unwrap();
            require_instance_job(&s)?;
            root = s.config.resolve(root_id.as_deref())?;
            task = s.tasks.admit(
                tasks::TaskTarget {
                    root_id: root.id.clone(),
                    root_path: root.path.clone(),
                    instance_id: None,
                },
                tasks::TaskKind::ResourceOperation,
            )?;
            task.begin_finishing();
        }
        let result = instance_rename_service::recover(&s, &root);
        finish_instance_task(task, result.clone(), "已恢复未完成的实例重命名");
        result
    })
    .await
    .map_err(|_| "恢复实例重命名失败".to_string())?
}

#[tauri::command]
async fn instance_reset_plan(
    id: String,
    components: Vec<pcl_install::ComponentSelection>,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<instance_reset::ResetPlan, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let root = s.config.resolve(root_id.as_deref())?;
        resource_ops::ensure_ready(Path::new(&root.path))?;
        ensure_instance_files_ready(&s, &root)?;
        instance_reset::prepare(Path::new(&root.path), &id, components)
    })
    .await
    .map_err(|_| "检查重置方案的任务意外退出".to_string())?
}

#[tauri::command]
async fn instance_reset_start(
    plan: ResetSubmission,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<String, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let root = s.config.resolve(root_id.as_deref())?;
        resource_ops::ensure_ready(Path::new(&root.path))?;
        ensure_instance_files_ready(&s, &root)?;
        let checked = instance_reset::prepare(Path::new(&root.path), &plan.id, plan.components)?;
        if checked.revision != plan.revision {
            return Err("实例或重置方案已改变，请重新检查".into());
        }
        let policy = captured_launch_policy(&s)?;
        let factory = pcl_network::snapshot();
        let scheduler = pcl_network::download_snapshot();
        let mut java = installer_java::Context::capture(
            &s,
            &root,
            &plan.id,
            tasks::TaskKind::InstanceReset,
            installer_java::Inputs {
                settings: s.config.checked_snapshot()?,
                preferences_revision: policy.revision,
                factory: factory.clone(),
                scheduler: scheduler.clone(),
            },
        )?;
        // Reset retains its existing exclusive writer admission, which also
        // covers the shared runtime store throughout preparation and rollback.
        let task = s.tasks.admit(
            tasks::TaskTarget {
                root_id: root.id.clone(),
                root_path: root.path.clone(),
                instance_id: Some(plan.id),
            },
            tasks::TaskKind::InstanceReset,
        )?;
        let task_id = task.id().to_owned();
        s.downloads.track(&task);
        let worker = s.clone();
        std::thread::Builder::new()
            .name(format!("pcl-reset-{task_id}"))
            .spawn(move || {
                let cancel = task.cancellation_token();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    java.verify(&worker, &task)?;
                    let installer = pcl_install::Installer::from_factory(&factory)?
                        .with_download_policy(scheduler)
                        .with_project(&worker.project)
                        .with_cache_source(&root.path)?;
                    let resolved =
                        installer.resolve_request(&checked.install_request(), &cancel)?;
                    let installer = java.ensure(&worker, &task, installer, &resolved)?;
                    java.verify(&worker, &task)?;
                    let java_bytes = java.network_bytes();
                    instance_reset::execute_prepared(
                        Path::new(&root.path),
                        checked,
                        installer,
                        resolved,
                        &cancel,
                        |mut p| {
                            p.network_bytes = p.network_bytes.saturating_add(java_bytes);
                            worker.downloads.progress(&task, p);
                        },
                        || java.verify(&worker, &task),
                    )
                }))
                .unwrap_or_else(|_| {
                    Err("取消清理失败：重置任务意外退出，请恢复未完成的重置后重试".into())
                });
                finish_reset_task(task, result);
            })
            .map_err(|e| format!("无法启动重置任务：{e}"))?;
        Ok(task_id)
    })
    .await
    .map_err(|_| "启动重置任务失败".to_string())?
}

fn finish_reset_task(task: tasks::TaskHandle, result: Result<serde_json::Value, String>) {
    // A late click on Cancel cannot turn a genuine Java/checksum/binding
    // failure into successful cleanup. Only a producer's cancellation result
    // may be projected as Cancelled by the task coordinator.
    match result {
        Err(error)
            if !error.starts_with("取消清理失败：")
                && !matches!(
                    error.as_str(),
                    tasks::CANCELLED
                        | java_download::CANCELLED
                        | "安装已取消"
                        | "下载已取消"
                        | "组件重置已取消"
                ) =>
        {
            task.finish(tasks::TaskOutcome::Error(error));
        }
        other => finish_instance_task(task, other, "实例核心重置完成，游戏内容已保留"),
    }
}

fn finish_instance_task(
    task: tasks::TaskHandle,
    result: Result<serde_json::Value, String>,
    message: &str,
) {
    match result {
        Ok(result) => {
            let warning = result
                .get("warning")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            task.finish(tasks::TaskOutcome::Complete {
                message: warning
                    .as_ref()
                    .map_or_else(|| message.into(), |warning| format!("{message}；{warning}")),
                result: Some(result),
                error: warning,
            })
        }
        Err(error) if error.starts_with("取消清理失败：") => {
            task.finish(tasks::TaskOutcome::CleanupFailed(error))
        }
        Err(error) => task.finish(tasks::TaskOutcome::Failed(error)),
    };
}

#[tauri::command]
async fn instance_reset_recover(
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let root = s.config.resolve(root_id.as_deref())?;
        ensure_instance_files_ready(&s, &root)?;
        let task = s.tasks.admit(
            tasks::TaskTarget {
                root_id: root.id,
                root_path: root.path.clone(),
                instance_id: None,
            },
            tasks::TaskKind::ResourceOperation,
        )?;
        task.begin_finishing();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            instance_reset::recover_pending(Path::new(&root.path))
        }))
        .unwrap_or_else(|_| Err("恢复重置任务意外退出，原文件和备份已保留".into()));
        finish_instance_task(task, result.clone(), "已恢复未完成的实例重置");
        result
    })
    .await
    .map_err(|_| "恢复重置任务失败".to_string())?
}

#[tauri::command]
async fn instance_export_plan(
    id: String,
    request: instance_export::ExportRequest,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<instance_export::ExportPlan, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let root = s.config.resolve(root_id.as_deref())?;
        resource_ops::ensure_ready(Path::new(&root.path))?;
        instance_reset::ensure_ready(Path::new(&root.path))?;
        ensure_instance_files_ready(&s, &root)?;
        let captured = s.config.isolation_settings(Some(&root.id))?;
        instance_export::prepare_with_isolation(
            Path::new(&root.path),
            &id,
            request,
            captured.policy(&id),
            &captured.revision,
        )
    })
    .await
    .map_err(|_| "检查导出文件的任务意外退出".to_string())?
}

#[tauri::command]
async fn instance_export_start(
    plan: ExportSubmission,
    root_id: Option<String>,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<String>, String> {
    let s = state.inner().clone();
    let (root, checked) = tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let root = s.config.resolve(root_id.as_deref())?;
        resource_ops::ensure_ready(Path::new(&root.path))?;
        instance_reset::ensure_ready(Path::new(&root.path))?;
        ensure_instance_files_ready(&s, &root)?;
        let captured = s.config.isolation_settings(Some(&root.id))?;
        let checked = instance_export::prepare_with_isolation(
            Path::new(&root.path),
            &plan.instance_id,
            plan.request,
            captured.policy(&plan.instance_id),
            &captured.revision,
        )?;
        if checked.revision != plan.revision {
            return Err("导出内容或选项已改变，请重新检查".to_string());
        }
        Ok((captured, checked))
    })
    .await
    .map_err(|_| "检查导出方案的任务意外退出".to_string())??;
    let filename = format!("{}-{}.zip", checked.request.name, checked.request.version)
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let choice = state
        .desktop
        .save_zip(window, state.project.clone(), filename)
        .await?;
    if choice.status == "cancelled" {
        return Ok(None);
    }
    if choice.status != "selected" {
        return Err(choice
            .message
            .unwrap_or_else(|| "无法选择导出文件位置".into()));
    }
    let destination = choice
        .paths
        .into_iter()
        .next()
        .ok_or("未选择导出文件位置")?;
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let current = s.config.isolation_settings(Some(&root.root.id))?;
        ensure_instance_files_ready(&s, &current.root)?;
        if current.revision != root.revision || current.root.path != root.root.path {
            return Err("游戏目录或实例设置已改变，请重新导出".into());
        }
        let task = s.tasks.admit(
            tasks::TaskTarget {
                root_id: root.root.id.clone(),
                root_path: root.root.path.clone(),
                instance_id: Some(checked.instance_id.clone()),
            },
            tasks::TaskKind::InstanceExport,
        )?;
        let task_id = task.id().to_owned();
        s.downloads.track(&task);
        let worker = s.clone();
        std::thread::Builder::new()
            .name(format!("pcl-export-{task_id}"))
            .spawn(move || {
                let cancel = task.cancellation_token();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let instance_id = checked.instance_id.clone();
                    instance_export::execute_with_guard(
                        checked,
                        &destination,
                        &cancel,
                        |p| worker.downloads.progress(&task, p),
                        || {
                            let operation = worker.operations.lock().unwrap();
                            if worker.closing.load(Ordering::SeqCst) {
                                return Err("启动器正在关闭".into());
                            }
                            let current = worker.config.isolation_settings(Some(&root.root.id))?;
                            if current.revision != root.revision
                                || current.root.path != root.root.path
                                || current.policy(&instance_id) != root.policy(&instance_id)
                            {
                                return Err("实例设置已改变，请重新导出".into());
                            }
                            Ok(operation)
                        },
                    )
                }))
                .unwrap_or_else(|_| {
                    Err("取消清理失败：导出任务意外退出，请检查保存目录中的临时文件".into())
                });
                let message = result
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("file_name"))
                    .and_then(|v| v.as_str())
                    .map(|name| format!("导出完成：{name}"))
                    .unwrap_or_else(|| "导出完成".into());
                finish_instance_task(task, result, &message);
            })
            .map_err(|e| format!("无法启动导出任务：{e}"))?;
        Ok(Some(task_id))
    })
    .await
    .map_err(|_| "启动导出任务失败".to_string())?
}

#[tauri::command]
async fn instance_export_config_read(
    id: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<instance_export::ExportRequest>, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        pcl_core::identifier(&id)?;
        let root = s.config.resolve(root_id.as_deref())?;
        export_presets::read(&s.project, &root.id, Path::new(&root.path), &id)
    })
    .await
    .map_err(|_| "读取导出配置失败".to_string())?
}
#[tauri::command]
async fn instance_export_config_save(
    id: String,
    request: instance_export::ExportRequest,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<(), String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        require_instance_job(&s)?;
        let root = s.config.resolve(root_id.as_deref())?;
        // Validate the exact choices before retaining them for this instance.
        ensure_instance_files_ready(&s, &root)?;
        let captured = s.config.isolation_settings(Some(&root.id))?;
        instance_export::prepare_with_isolation(
            Path::new(&root.path),
            &id,
            request.clone(),
            captured.policy(&id),
            &captured.revision,
        )?;
        export_presets::save(&s.project, &root.id, Path::new(&root.path), &id, request)
    })
    .await
    .map_err(|_| "保存导出配置失败".to_string())?
}

enum ResourceRequest {
    SetEnabled {
        files: Vec<resource_ops::ResourceFile>,
        enabled: bool,
    },
    Remove(Vec<resource_ops::ResourceFile>),
    Import(Vec<PathBuf>),
    Restore(String),
    Recover,
}

fn require_resource_write(s: &Shared) -> Result<(), String> {
    if s.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请重新打开后操作资源文件".into());
    }
    launcher_monitor_runtime::require_idle(s)?;
    s.config.ensure_writable()?;
    if matches!(
        s.status.lock().unwrap().stage.as_str(),
        "preparing" | "running"
    ) {
        return Err("请在游戏退出后修改资源文件".into());
    }
    Ok(())
}

fn admit_resource(
    s: &Arc<Shared>,
    id: &str,
    root_id: Option<&str>,
    expected_path: Option<&str>,
    expected_revision: Option<&str>,
) -> Result<(config::IsolationSettings, tasks::TaskHandle), String> {
    let _operation = s.operations.lock().unwrap();
    require_resource_write(s)?;
    pcl_core::identifier(id)?;
    let captured = s.config.isolation_settings(root_id)?;
    if expected_revision.is_some_and(|r| r != captured.revision) {
        return Err("实例设置已变化，请重新选择资源文件".into());
    }
    let root = &captured.root;
    ensure_instance_files_ready(s, &root)?;
    instance_delete::ensure_name_available(Path::new(&root.path), id)?;
    if expected_path.is_some_and(|path| path != root.path) {
        return Err("游戏目录位置已改变，请重新选择文件".into());
    }
    let task = s.tasks.admit(
        tasks::TaskTarget {
            root_id: root.id.clone(),
            root_path: root.path.clone(),
            instance_id: Some(id.into()),
        },
        tasks::TaskKind::ResourceOperation,
    )?;
    Ok((captured, task))
}

async fn run_resource_request(
    shared: Arc<Shared>,
    captured: config::IsolationSettings,
    id: String,
    kind: String,
    request: ResourceRequest,
    task: tasks::TaskHandle,
) -> Result<resource_ops::MutationResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cancel = task.cancellation_token();
        let policy = captured.policy(&id);
        let mut commit = || {
            let _operation = shared.operations.lock().unwrap();
            if shared.closing.load(Ordering::SeqCst) {
                return Err("启动器正在关闭".into());
            }
            let current = shared.config.isolation_settings(Some(&captured.root.id))?;
            if current.revision != captured.revision
                || current.root.path != captured.root.path
                || current.policy(&id) != policy
            {
                return Err("实例设置已改变，已取消资源操作".into());
            }
            task.begin_finishing();
            if cancel.load(Ordering::SeqCst) {
                Err("资源操作已取消".into())
            } else {
                Ok(())
            }
        };
        let mut progress = |completed, total| {
            task.update(tasks::TaskProgress {
                stage: tasks::TaskStage::Processing,
                phase: "resources".into(),
                message: "正在处理资源文件…".into(),
                completed,
                total,
                bytes_done: 0,
                bytes_total: 0,
                network_bytes: 0,
                steps: Vec::new(),
            });
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let root = Path::new(&captured.root.path);
            match request {
                ResourceRequest::SetEnabled { files, enabled } => {
                    resource_ops::set_enabled_with_isolation(
                        root,
                        &id,
                        &kind,
                        &files,
                        enabled,
                        &cancel,
                        &mut commit,
                        &mut progress,
                        policy,
                    )
                }
                ResourceRequest::Remove(files) => resource_ops::remove_with_isolation(
                    root,
                    &id,
                    &kind,
                    &files,
                    &cancel,
                    &mut commit,
                    &mut progress,
                    policy,
                ),
                ResourceRequest::Import(sources) => resource_ops::import_files_with_isolation(
                    root,
                    &id,
                    &kind,
                    &sources,
                    &cancel,
                    &mut commit,
                    &mut progress,
                    policy,
                ),
                ResourceRequest::Restore(operation) => resource_ops::restore_with_isolation(
                    root,
                    &id,
                    &kind,
                    &operation,
                    &cancel,
                    &mut commit,
                    &mut progress,
                    policy,
                ),
                ResourceRequest::Recover => {
                    commit()?;
                    resource_ops::recover_pending(root, &id, &kind)?;
                    Ok(resource_ops::MutationResult {
                        changed: 0,
                        undo_id: None,
                        message: "已恢复未完成的资源操作，请检查资源列表".into(),
                    })
                }
            }
        }))
        .unwrap_or_else(|_| Err("资源操作意外中断，请刷新资源列表后重试".into()));
        match &result {
            Ok(report) => {
                task.finish(tasks::TaskOutcome::Complete {
                    result: serde_json::to_value(report).ok(),
                    message: report.message.clone(),
                    error: None,
                });
            }
            Err(error) => {
                task.finish(tasks::TaskOutcome::Failed(error.clone()));
            }
        }
        result
    })
    .await
    .map_err(|_| "资源操作任务未能完成".to_owned())?
}

#[tauri::command]
async fn resource_set_enabled(
    id: String,
    kind: String,
    files: Vec<resource_ops::ResourceFile>,
    enabled: bool,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<resource_ops::MutationResult, String> {
    let (captured, task) = admit_resource(state.inner(), &id, root_id.as_deref(), None, None)?;
    run_resource_request(
        state.inner().clone(),
        captured,
        id,
        kind,
        ResourceRequest::SetEnabled { files, enabled },
        task,
    )
    .await
}

#[tauri::command]
async fn resource_remove(
    id: String,
    kind: String,
    files: Vec<resource_ops::ResourceFile>,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<resource_ops::MutationResult, String> {
    let (captured, task) = admit_resource(state.inner(), &id, root_id.as_deref(), None, None)?;
    run_resource_request(
        state.inner().clone(),
        captured,
        id,
        kind,
        ResourceRequest::Remove(files),
        task,
    )
    .await
}

#[tauri::command]
async fn resource_restore(
    id: String,
    kind: String,
    operation_id: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<resource_ops::MutationResult, String> {
    let (captured, task) = admit_resource(state.inner(), &id, root_id.as_deref(), None, None)?;
    run_resource_request(
        state.inner().clone(),
        captured,
        id,
        kind,
        ResourceRequest::Restore(operation_id),
        task,
    )
    .await
}

#[tauri::command]
async fn resource_recover(
    id: String,
    kind: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<resource_ops::MutationResult, String> {
    let (captured, task) = admit_resource(state.inner(), &id, root_id.as_deref(), None, None)?;
    run_resource_request(
        state.inner().clone(),
        captured,
        id,
        kind,
        ResourceRequest::Recover,
        task,
    )
    .await
}

#[tauri::command]
async fn resource_removed(
    id: String,
    kind: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<Vec<resource_ops::RemovedOperation>, String> {
    let shared = state.inner().clone();
    let captured = shared.config.isolation_settings(root_id.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || {
        let result = resource_ops::removed_with_isolation(
            Path::new(&captured.root.path),
            &id,
            &kind,
            captured.policy(&id),
        )?;
        if shared
            .config
            .isolation_settings(Some(&captured.root.id))?
            .revision
            != captured.revision
        {
            return Err("实例设置已变化，请刷新资源列表".into());
        }
        Ok(result)
    })
    .await
    .map_err(|_| "读取可恢复资源失败".to_owned())?
}

#[derive(Serialize)]
struct ResourceImportResult {
    status: &'static str,
    changed: usize,
    undo_id: Option<String>,
    message: Option<String>,
}

#[tauri::command]
async fn resource_import(
    id: String,
    kind: String,
    root_id: Option<String>,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<ResourceImportResult, String> {
    let initial = {
        let _operation = state.operations.lock().unwrap();
        require_resource_write(state.inner())?;
        pcl_core::identifier(&id)?;
        if state.tasks.active().is_some() {
            return Err("请在当前文件操作结束后安装资源".into());
        }
        state.config.isolation_settings(root_id.as_deref())?
    };
    let choice = state
        .desktop
        .pick_resource_files(window, PathBuf::from(&initial.root.path), &kind)
        .await?;
    if choice.status != "selected" {
        return Ok(ResourceImportResult {
            status: choice.status,
            changed: 0,
            undo_id: None,
            message: choice.message,
        });
    }
    let (captured, task) = admit_resource(
        state.inner(),
        &id,
        Some(&initial.root.id),
        Some(&initial.root.path),
        Some(&initial.revision),
    )?;
    let result = run_resource_request(
        state.inner().clone(),
        captured,
        id,
        kind,
        ResourceRequest::Import(choice.paths),
        task,
    )
    .await?;
    Ok(ResourceImportResult {
        status: "complete",
        changed: result.changed,
        undo_id: result.undo_id,
        message: Some(result.message),
    })
}
fn require_account_edit(st: &RunStatus) -> Result<(), String> {
    if st.stage == "preparing" || st.stage == "running" {
        return Err("请在游戏退出后管理账号".into());
    }
    Ok(())
}
/// Account writes publish their new stage while holding the same admission lock
/// as network-policy adoption and game preparation. In particular, `start`
/// captures its client and sets Accounts.preparing before this lock is released.
/// The action is synchronous; no native UI or asynchronous wait occurs here.
fn with_account_write<T>(
    s: &Shared,
    action: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _operation = s.operations.lock().unwrap();
    if s.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请稍后管理账号".into());
    }
    launcher_monitor_runtime::require_idle(s)?;
    if s.tasks.active().is_some() || s.launcher_updates.busy() {
        return Err("请在文件任务和启动器更新结束后管理账号".into());
    }
    let run = s.status.lock().unwrap();
    require_account_edit(&run)?;
    if s.accounts.busy() {
        return Err("请先完成或取消当前微软登录".into());
    }
    action()
}
#[tauri::command]
fn auth_status(state: State<'_, Arc<Shared>>) -> accounts::AuthState {
    state.accounts.snapshot()
}
#[tauri::command]
fn auth_configure(client_id: String, state: State<'_, Arc<Shared>>) -> Result<(), String> {
    with_account_write(&state, || state.accounts.configure(client_id))
}
#[tauri::command]
fn auth_start(remember: bool, state: State<'_, Arc<Shared>>) -> Result<(), String> {
    with_account_write(&state, || state.accounts.start(remember))
}
#[tauri::command]
fn auth_cancel(state: State<'_, Arc<Shared>>) {
    state.accounts.cancel();
}
#[tauri::command]
fn auth_select(id: Option<String>, state: State<'_, Arc<Shared>>) -> Result<(), String> {
    with_account_write(&state, || state.accounts.select(id))
}
#[tauri::command]
async fn auth_remove(id: String, state: State<'_, Arc<Shared>>) -> Result<(), String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let remembered = with_account_write(&s, || s.accounts.begin_remove(&id))?;
        s.accounts.remove_account(id, remembered)
    })
    .await
    .map_err(|_| "账号操作任务失败")?
}
fn open_official(url: &str) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map_err(|_| "无法打开默认浏览器")?;
    Ok(())
}
#[tauri::command]
fn auth_open_browser(state: State<'_, Arc<Shared>>) -> Result<(), String> {
    if state.accounts.snapshot().challenge.is_none() {
        return Err("请先开始微软登录，获取设备代码".into());
    }
    open_official("https://www.microsoft.com/link")
}
#[tauri::command]
fn auth_open_help(kind: String) -> Result<(), String> {
    open_official(match kind.as_str() {
        "register" => {
            "https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app"
        }
        "tenant" => "https://azure.microsoft.com/en-us/pricing/purchase-options/azure-account",
        "review" => "https://aka.ms/mce-reviewappid",
        _ => return Err("未知帮助类型".into()),
    })
}
#[tauri::command]
async fn inspect_instance(
    id: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<launcher_inspection::Inspection, String> {
    let s = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        launcher_inspection::inspect(&s, &id, root_id.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}
fn instance_context(
    config: &ConfigStore,
    root_id: Option<&str>,
    id: &str,
) -> Result<(Settings, GameRoot), String> {
    pcl_core::identifier(id)?;
    let settings = config.snapshot();
    let root = config.resolve(Some(root_id.unwrap_or(&settings.root_id)))?;
    instance_delete::ensure_name_available(Path::new(&root.path), id)?;
    Ok((settings, root))
}
/// Preference warnings represent retained, unverified bytes. A launch must not
/// silently substitute window/network defaults for such a policy. Capture the
/// complete valid policy before preparing, alongside the root and Java choice.
fn captured_launch_policy(
    shared: &Shared,
) -> Result<launcher_prefs::LauncherPreferencesView, String> {
    let view = shared.launcher_preferences.snapshot();
    if let Some(warning) = &view.warning {
        return Err(warning.clone());
    }
    view.preferences.validate()?;
    Ok(view)
}
#[cfg(test)]
fn captured_launch_preferences(
    shared: &Shared,
) -> Result<launcher_prefs::LauncherPreferences, String> {
    Ok(captured_launch_policy(shared)?.preferences)
}
fn captured_game_options(
    settings: &Settings,
    root: &GameRoot,
    preferences: &launcher_prefs::LauncherPreferences,
    id: &str,
) -> Result<pcl_core::launch_options::LaunchOptions, String> {
    let mut options = preferences.game_launch.clone();
    options.isolation = root.resolved_isolation(id, settings.isolation);
    if let Some(title) = root.window_title_overrides.get(id) {
        options.window_title = title.clone();
    }
    if let Some(arguments) = root.argument_overrides.get(id) {
        arguments.apply_to(&mut options)?;
    }
    options.validate()?;
    Ok(options)
}
#[cfg(test)]
fn update(s: &Shared, stage: &str, message: String, pid: Option<u32>, exit_code: Option<i32>) {
    let mut st = s.status.lock().unwrap();
    st.stage = stage.into();
    st.message = message;
    st.pid = pid;
    st.exit_code = exit_code;
}
#[tauri::command]
fn launch_game(
    id: String,
    root_id: Option<String>,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<(), String> {
    let _operation = state.operations.lock().unwrap();
    if state.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    launcher_monitor_runtime::require_idle(&state)?;
    state.config.ensure_writable()?;
    let (cfg, root) = instance_context(&state.config, root_id.as_deref(), &id)?;
    let policy = captured_launch_policy(&state)?;
    let launcher_policy = &policy.preferences;
    let game_options = captured_game_options(&cfg, &root, launcher_policy, &id)?;
    resource_ops::ensure_ready(Path::new(&root.path))?;
    instance_reset::ensure_ready(Path::new(&root.path))?;
    ensure_instance_files_ready(&state, &root)?;
    let generation = launcher_monitor_runtime::begin_preparation(
        &state,
        RunStatus {
            stage: "preparing".into(),
            message: "正在检查启动依赖…".into(),
            version: Some(id.clone()),
            root_id: Some(root.id.clone()),
            root_path: Some(root.path.clone()),
            ..Default::default()
        },
    )?;
    let mut preparation = match launcher_preparation::capture(
        &state,
        &id,
        &root,
        &cfg,
        &policy.revision,
        generation,
        game_options.isolation,
    ) {
        Ok(preparation) => preparation,
        Err(error) => {
            let cancelled = matches!(
                error.as_str(),
                tasks::CANCELLED | "安装已取消" | "下载已取消" | java_download::CANCELLED
            );
            launcher_monitor_runtime::update_current(
                &state,
                generation,
                if cancelled { "idle" } else { "error" },
                if cancelled {
                    "启动已取消".into()
                } else {
                    error.clone()
                },
                None,
                None,
            );
            return Err(error);
        }
    };
    let visibility_policy = launcher_policy.launch_visibility;
    let preparation_delay = launcher_policy.advanced.artificial_delay_ms;
    let s = state.inner().clone();
    let spawn = std::thread::Builder::new()
        .name("pcl-launch-preparation".into())
        .spawn(move || {
            let mut visibility = launcher_visibility::VisibilityGuard::new(
                window.clone(),
                visibility_policy,
                s.clone(),
                generation,
            );
            let mut task = || -> Result<(), String> {
                if launcher_delay::preparation_delay(preparation_delay, &s.stop) {
                    launcher_monitor_runtime::update_current(
                        &s,
                        generation,
                        "idle",
                        "启动已取消".into(),
                        None,
                        None,
                    );
                    return Ok(());
                }
                let plan = preparation.prepare_with(&s, &cfg, |launch_settings| {
                    let memory = launcher_memory::allocation(
                        launch_settings,
                        &root,
                        &id,
                        game_options.isolation,
                    )?;
                    match s.accounts.identity()? {
                        Some(identity) => pcl_core::build_launch_plan_authenticated_with_options(
                            Path::new(&root.path),
                            &s.project,
                            &id,
                            &pcl_core::OnlineIdentity {
                                name: identity.name,
                                uuid: identity.uuid,
                                access_token: identity.access_token,
                                xuid: identity.xuid,
                                client_id: identity.client_id,
                            },
                            memory,
                            java_service::effective_choice(launch_settings, &root, &id),
                            &launch_settings.java_paths,
                            &game_options,
                        ),
                        None => pcl_core::build_launch_plan_with_options(
                            Path::new(&root.path),
                            &s.project,
                            &id,
                            &cfg.player,
                            memory,
                            java_service::effective_choice(launch_settings, &root, &id),
                            &launch_settings.java_paths,
                            &game_options,
                        ),
                    }
                })?;
                if s.stop.load(Ordering::SeqCst) {
                    launcher_monitor_runtime::update_current(
                        &s,
                        generation,
                        "idle",
                        "启动已取消".into(),
                        None,
                        None,
                    );
                    return Ok(());
                }
                let request = launcher_game_monitor::MonitorRequest::new(
                    s.project.clone(),
                    root.id.clone(),
                    PathBuf::from(&root.path),
                    id.clone(),
                    plan.java,
                    plan.args,
                    plan.game_dir,
                    plan.log_path,
                    s.accounts.redaction_snapshot(),
                )?;
                let spawn_admission = preparation.authorize(&s)?;
                let started = launcher_game_monitor::spawn_monitor(request)?;
                drop(spawn_admission);
                // Statistics reflect actual ACKed spawn even when the user cancels
                // during the handshake; cancellation is checked before window effects.
                let handoff = launcher_monitor_runtime::adopt_spawned(
                    &s,
                    generation,
                    started.pid,
                    started.monitor_pid,
                    started.session_id,
                    started.log_path,
                    started.log_start,
                );
                s.launcher_local.game_spawned();
                let handoff = handoff?;
                if handoff == launcher_monitor_runtime::Handoff::Superseded {
                    return Ok(());
                }
                if handoff == launcher_monitor_runtime::Handoff::Ready {
                    if visibility_policy == launcher_prefs::LaunchVisibility::ExitAfterLaunch {
                        let closed = launcher_monitor_runtime::with_spawn_visibility(
                            &s,
                            generation,
                            || window.close(),
                        )?;
                        if let Some(result) = closed {
                            launcher_monitor_runtime::with_current(&s, generation, || {
                                s.launcher_local.warnings.set(
                                    launcher_runtime::WarningKind::Visibility,
                                    result.err().map(|_| {
                                        "游戏已启动，但启动器窗口未能关闭；可以手动关闭".into()
                                    }),
                                )
                            });
                            return Ok(());
                        }
                    } else {
                        let warning = visibility
                            .spawned()?
                            .and_then(|result| result.err())
                            .map(|_| "游戏已启动，但窗口管理器未能隐藏启动器".into());
                        launcher_monitor_runtime::with_current(&s, generation, || {
                            s.launcher_local
                                .warnings
                                .set(launcher_runtime::WarningKind::Visibility, warning)
                        });
                    }
                }
                while launcher_monitor_runtime::poll_generation(&s, generation)? {
                    std::thread::sleep(Duration::from_millis(300));
                }
                let warning = visibility
                    .restore()
                    .err()
                    .map(|_| "游戏已退出，但窗口管理器未能恢复启动器，请从应用入口重新打开".into());
                launcher_monitor_runtime::with_current(&s, generation, || {
                    s.launcher_local
                        .warnings
                        .set(launcher_runtime::WarningKind::Visibility, warning)
                });
                Ok(())
            };
            if let Err(error) = task() {
                let cancelled = matches!(
                    error.as_str(),
                    tasks::CANCELLED | "安装已取消" | "下载已取消" | java_download::CANCELLED
                );
                launcher_monitor_runtime::update_current(
                    &s,
                    generation,
                    if cancelled { "idle" } else { "error" },
                    if cancelled {
                        "启动已取消".into()
                    } else {
                        error
                    },
                    None,
                    None,
                );
            }
        });
    if let Err(error) = spawn {
        let error = format!("无法启动游戏准备任务：{error}");
        launcher_monitor_runtime::update_current(
            &state,
            generation,
            "error",
            error.clone(),
            None,
            None,
        );
        return Err(error);
    }
    Ok(())
}
#[tauri::command]
fn stop_game(state: State<'_, Arc<Shared>>) -> Result<(), String> {
    launcher_monitor_runtime::request_stop(&state)
}
#[tauri::command]
fn read_log(state: State<'_, Arc<Shared>>) -> Result<String, String> {
    let p = state
        .log
        .lock()
        .unwrap()
        .clone()
        .ok_or("这次会话还没有游戏日志")?;
    let folder = p
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or("当前日志目录无效")?;
    let name = p
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("当前日志文件名无效")?;
    let lines = state
        .launcher_preferences
        .snapshot()
        .preferences
        .realtime_log_line_limit;
    launcher_logs::read_tail(folder, name, lines as usize, |text| {
        state.accounts.redact(text.to_owned())
    })
}
#[tauri::command]
fn open_folder(
    kind: String,
    id: Option<String>,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<(), String> {
    let captured = state.config.isolation_settings(root_id.as_deref())?;
    let root = PathBuf::from(&captured.root.path);
    let path = match kind.as_str() {
        "game" => root.clone(),
        "instance" => {
            let name = id.ok_or("未选择版本")?;
            pcl_core::identifier(&name)?;
            root.join("versions").join(name)
        }
        "logs" => root.join(".pcl-linux/logs"),
        "mods" | "saves" | "screenshots" | "resourcepacks" | "shaderpacks" | "litematics"
        | "server" => {
            let id = id.ok_or("未选择版本")?;
            ui_data::resource_dir_with_isolation(&root, &id, &kind, captured.policy(&id))?
        }
        _ => return Err("未知目录类型".into()),
    };
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(root.canonicalize().map_err(|e| e.to_string())?) {
        return Err("目录超出游戏目录".into());
    }
    if state
        .config
        .isolation_settings(Some(&captured.root.id))?
        .revision
        != captured.revision
    {
        return Err("实例设置已变化，请重新打开目录".into());
    }
    Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn main() {
    if let Some(code) = code_runner::main_if_requested() {
        std::process::exit(code);
    }
    if let Some(code) = launcher_game_monitor::dispatch_cli() {
        std::process::exit(code);
    }
    let project = project();
    let launcher_preferences = launcher_prefs::LauncherPreferencesStore::load(&project);
    let initial_preferences = launcher_preferences.snapshot().preferences;
    let network_warning =
        pcl_network::install_policy(launcher_commands::network_policy(&initial_preferences))
            .err()
            .map(|_| "启动器网络设置未能应用，本次会话暂用系统网络策略".to_string());
    pcl_network::install_download_policy(launcher_commands::download_policy(&initial_preferences))
        .expect("已验证的下载策略无效");
    let xdg = launcher_local_commands::xdg_paths().unwrap_or(launcher_local::XdgPaths {
        applications: PathBuf::new(),
        desktop: None,
    });
    let launcher_local =
        launcher_runtime::LocalRuntime::new(project.clone(), xdg, &initial_preferences);
    launcher_local
        .warnings
        .set(launcher_runtime::WarningKind::Network, network_warning);
    // This renderer process option is set before Tauri or worker threads start.
    // Runtime edits are persisted and reported as requiring a restart.
    if launcher_preferences
        .snapshot()
        .preferences
        .disable_hardware_acceleration
    {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }
    let launcher_updates = launcher_update_commands::Updater::new(
        &project,
        launcher_update_commands::current_build(&project),
    )
    .expect("更新服务初始化失败");
    // Recover our executable transaction before any configuration migration or
    // task can write. This is read-only when no update journal exists.
    if let Err(error) = launcher_updates.service.recover() {
        eprintln!("启动器更新恢复服务不可用：{error}");
    }
    let (config, warning) = ConfigStore::load(&project);
    let tasks = Arc::new(tasks::Tasks::new());
    let state = Arc::new(Shared {
        accounts: Arc::new(accounts::Accounts::new(&project)),
        downloads: Arc::new(downloads::Downloads::new(tasks.clone())),
        tasks,
        config,
        instance_metadata: instance_meta::MetadataStore::load(&project),
        launcher_preferences,
        launcher_import: launcher_commands::ImportSession::default(),
        launcher_assets: launcher_assets::AssetStore::new(project.clone()),
        launcher_local,
        launcher_updates,
        product_plugins: Arc::new(product_plugins::Runtime::new(&project)),
        code: std::sync::OnceLock::new(),
        minecraft_updates: launcher_minecraft_updates::MinecraftUpdates::new(project.clone()),
        launcher_favorites: launcher_favorites::FavoriteStore::load(&project),
        desktop: Arc::new(platform::Desktop::default()),
        operations: Mutex::new(()),
        project,
        status: Mutex::new(RunStatus {
            stage: if warning.is_some() { "error" } else { "idle" }.into(),
            message: warning.unwrap_or_else(|| "准备就绪".into()),
            ..Default::default()
        }),
        stop: AtomicBool::new(false),
        closing: AtomicBool::new(false),
        close_generation: AtomicU64::new(0),
        monitor: launcher_monitor_runtime::MonitorRuntime::default(),
        resource_save: resource_save_commands::SaveSession::default(),
        resource_confirmations: modrinth_install::ConfirmationCache::default(),
        pack_confirmations: instance_import::mrpack::ConfirmationCache::default(),
        toolbox_download: toolbox_download::DownloadSession::default(),
        toolbox_images: toolbox_images::ImageSession::default(),
        log: Mutex::new(None),
    });
    let _ = launcher_monitor_runtime::refresh(&state);
    tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol(
            "pcl-media",
            launcher_asset_commands::serve_media,
        )
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(experimental::Host::new(state.project.clone())))
        .manage(state)
        .setup(|app| {
            let shared = app.state::<Arc<Shared>>().inner().clone();
            let prefs = shared.launcher_preferences.snapshot();
            if let Some(window) = app.get_webview_window("main") {
                shared.launcher_local.warnings.set(
                    launcher_runtime::WarningKind::Window,
                    launcher_commands::apply_window(&window, &prefs.preferences)
                        .err()
                        .map(|_| "窗口管理器未能应用启动器的窗口设置".to_string()),
                );
            }
            shared.launcher_local.opened();
            let handle = app.handle().clone();
            let weak = Arc::downgrade(&shared);
            shared.tasks.set_listener(move |snapshot| {
                if let Some(shared) = weak.upgrade() {
                    shared.launcher_local.task_changed(&snapshot);
                }
                let _ = handle.emit("task_changed", snapshot);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Arc<Shared>>();
                let decision = begin_launcher_close(&state);
                let experiments = window.state::<Arc<experimental::Host>>();
                experiments.shutdown();
                // Observational only: frontend subscribers must never take
                // over the native cancellation/drain-and-close decision.
                let _ = window.emit("launcher_closing", decision.event);
                if decision.wait_for_workers || !experiments.is_stopped() {
                    api.prevent_close();
                    for id in decision.active_task_ids {
                        let _ = state.tasks.cancel(&id);
                    }
                    state.launcher_updates.service.cancel();
                    if decision.first_request {
                        let shared = state.inner().clone();
                        let experiments = experiments.inner().clone();
                        let window = window.clone();
                        std::thread::spawn(move || {
                            while !shared.tasks.active_all().is_empty()
                                || shared.launcher_updates.busy()
                                || shared.accounts.busy()
                                || !experiments.is_stopped()
                            {
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            let _ = window.close();
                        });
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            experimental::experimental_call,
            experimental::experimental_suspend,
            rh_code::rh_code_call,
            rh_code::rh_code_choose,
            experimental::rh_code_adopt,
            experimental::rh_code_harness_reviews,
            experimental::rh_code_harness_apply,
            experimental::rh_code_build_open,
            experimental::experimental_choose,
            experimental::experimental_open,
            instance_metadata_update,
            instance_metadata_read,
            roots_list,
            root_pick,
            root_register,
            root_select,
            root_update,
            root_remove,
            task_list,
            task_snapshot,
            task_cancel,
            resource_set_enabled,
            resource_remove,
            resource_restore,
            resource_recover,
            resource_removed,
            resource_import,
            resource_install_commands::resource_install_plan,
            resource_install_commands::resource_install_start,
            resource_install_commands::resource_install_recover,
            resource_update_commands::resource_update_check,
            resource_update_commands::resource_update_plan,
            resource_update_commands::resource_update_start,
            resource_save_commands::resource_save_prepare,
            resource_save_commands::resource_save_start,
            resource_update_commands::resource_update_history,
            resource_update_commands::resource_update_restore,
            ui_catalog::ui_open_link,
            ui_catalog::loader_catalog,
            ui_catalog::loader_candidates,
            resource_details::resource_details,
            community_catalog::community_search,
            community_details::community_details,
            resource_details::resource_dependencies,
            resource_details::resource_open_link,
            ui_catalog::upstream_contributors,
            ui_catalog::project_feedback,
            launcher_log_commands::launcher_logs,
            instance_log_commands::instance_logs,
            instance_log_commands::instance_read_log,
            instance_log_commands::instance_export_log,
            launcher_log_commands::launcher_read_log,
            launcher_log_commands::launcher_prepare_log_export,
            launcher_log_commands::launcher_export_logs,
            launcher_log_commands::launcher_prepare_log_clear,
            launcher_log_commands::launcher_clear_logs,
            launcher_log_commands::launcher_log_recovery,
            launcher_log_commands::launcher_restore_logs,
            launcher_commands::launcher_preferences,
            launcher_commands::launcher_preferences_update,
            launcher_commands::launcher_export_settings,
            launcher_commands::launcher_prepare_settings_import,
            launcher_commands::launcher_apply_settings_import,
            launcher_commands::launcher_network_status,
            launcher_discovery_commands::launcher_announcements,
            launcher_minecraft_update_commands::launcher_minecraft_updates_check,
            launcher_minecraft_update_commands::launcher_minecraft_updates_ack,
            launcher_discovery_commands::launcher_clipboard_link,
            editing::launcher_edit,
            launcher_favorite_commands::launcher_favorites_read,
            launcher_favorite_commands::launcher_favorites_patch,
            toolbox_image_commands::toolbox_skin_pick,
            toolbox_image_commands::toolbox_avatar_render,
            toolbox_image_commands::toolbox_avatar_export,
            toolbox_image_commands::toolbox_png_export,
            toolbox_download_commands::toolbox_download_choose,
            toolbox_download_commands::toolbox_download_prepare,
            toolbox_download_commands::toolbox_download_start,
            toolbox_download_commands::toolbox_download_open_directory,
            local_resource_info_commands::local_resource_info_export,
            launcher_local_commands::launcher_statistics,
            launcher_local_commands::launcher_shortcut_plan,
            launcher_local_commands::launcher_shortcut_apply,
            launcher_local_commands::launcher_shortcut_recovery,
            launcher_local_commands::launcher_shortcut_restore,
            launcher_local_commands::launcher_finish_using,
            launcher_update_commands::launcher_update_status,
            launcher_update_commands::launcher_update_check,
            launcher_update_commands::launcher_update_download,
            launcher_update_commands::launcher_update_cancel,
            launcher_update_commands::launcher_update_discard,
            launcher_update_commands::launcher_update_apply,
            launcher_update_commands::launcher_update_rollback,
            launcher_update_commands::launcher_update_acknowledge,
            launcher_update_commands::launcher_update_recover,
            launcher_asset_commands::launcher_assets,
            launcher_asset_commands::launcher_open_media_folder,
            launcher_asset_commands::launcher_fonts,
            launcher_asset_commands::launcher_title_asset,
            launcher_asset_commands::launcher_title_pick,
            launcher_asset_commands::launcher_home_pick,
            launcher_asset_commands::launcher_home_refresh,
            launcher_asset_commands::launcher_home_open_link,
            ui_catalog::instance_servers,
            ui_data::system_info,
            launcher_memory::memory_preview,
            java_commands::java_catalog,
            java_commands::java_add,
            java_download_commands::java_download_catalog,
            java_download_commands::java_download_start,
            launcher_repair_commands::instance_repair_start,
            launcher_repair_commands::instance_verify_start,
            launcher_repair_commands::instance_damage_repair_start,
            launcher_repair_commands::instance_damage_recover,
            launcher_failure_commands::launch_failure_report,
            launcher_failure_commands::launch_failure_export,
            launcher_failure_ai::launch_failure_ai_preview,
            launcher_failure_ai::launch_failure_ai_start,
            launcher_failure_ai::launch_failure_ai_status,
            launcher_failure_ai::launch_failure_ai_cancel,
            ui_data::instance_resources,
            ui_data::modrinth_search,
            save_settings,
            process_status,
            inspect_instance,
            launch_game,
            stop_game,
            read_log,
            open_folder,
            auth_status,
            auth_configure,
            auth_start,
            auth_cancel,
            auth_select,
            auth_remove,
            auth_open_browser,
            account_commands::auth_save_login,
            account_commands::auth_device_assist,
            account_commands::auth_copy_code,
            account_commands::auth_appearance,
            auth_open_help,
            download_catalog,
            download_status,
            download_tasks,
            download_start,
            instance_reset_plan,
            instance_reset_start,
            instance_reset_recover,
            instance_rename_prepare,
            instance_rename_start,
            instance_rename_recover,
            instance_export_plan,
            instance_export_start,
            instance_export_config_read,
            instance_export_config_save,
            instance_commands::instance_import_pick,
            instance_commands::instance_pack_download,
            instance_commands::instance_pack_prepare,
            instance_commands::instance_pack_start,
            instance_commands::instance_pack_release,
            instance_commands::instance_import_prepare,
            instance_commands::instance_import_discard,
            instance_commands::instance_import_start,
            instance_commands::instance_copy_prepare,
            instance_commands::instance_copy_start,
            instance_commands::instance_import_recover,
            instance_commands::instance_delete_prepare,
            instance_commands::instance_delete_start,
            instance_commands::instance_deleted_list,
            instance_commands::instance_restore_start,
            instance_commands::instance_delete_recover,
            download_cancel
        ])
        .run(tauri::generate_context!())
        .expect("桌面应用运行失败");
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::thread;
    use tasks::{TaskKind, TaskOutcome, TaskTarget};

    #[path = "content_scope.rs"]
    mod content_scope;
    #[path = "game_options.rs"]
    mod game_options;
    #[path = "resource_updates.rs"]
    mod resource_updates;
    #[path = "root_removal.rs"]
    mod root_removal_tests;

    #[test]
    fn reset_retains_real_preparation_errors_after_late_cancel() {
        for error in ["Java 文件校验失败", "安装期间游戏目录已变化"] {
            let fixture = Fixture::new();
            let shared = fixture.shared();
            let root = shared.config.resolve(None).unwrap();
            let task = shared
                .tasks
                .admit(
                    TaskTarget {
                        root_id: root.id,
                        root_path: root.path,
                        instance_id: Some("fixture".into()),
                    },
                    TaskKind::InstanceReset,
                )
                .unwrap();
            let id = task.id().to_owned();
            shared.tasks.cancel(&id).unwrap();
            finish_reset_task(task, Err(error.into()));
            let ended = shared.tasks.snapshot(&id).unwrap();
            assert_eq!(ended.stage, tasks::TaskStage::Error);
            assert_eq!(ended.error.as_deref(), Some(error));
        }
    }

    #[test]
    fn reset_cancellation_and_committed_success_preserve_their_outcomes() {
        for cancelled_result in [true, false] {
            let fixture = Fixture::new();
            let shared = fixture.shared();
            let root = shared.config.resolve(None).unwrap();
            let task = shared
                .tasks
                .admit(
                    TaskTarget {
                        root_id: root.id,
                        root_path: root.path,
                        instance_id: Some("fixture".into()),
                    },
                    TaskKind::InstanceReset,
                )
                .unwrap();
            let id = task.id().to_owned();
            shared.tasks.cancel(&id).unwrap();
            let result = if cancelled_result {
                Err("组件重置已取消".into())
            } else {
                Ok(serde_json::json!({"id":"fixture", "backup_id":"retained"}))
            };
            finish_reset_task(task, result);
            let ended = shared.tasks.snapshot(&id).unwrap();
            assert_eq!(
                ended.stage,
                if cancelled_result {
                    tasks::TaskStage::Cancelled
                } else {
                    tasks::TaskStage::Complete
                }
            );
            if !cancelled_result {
                assert_eq!(ended.result.unwrap()["backup_id"], "retained");
            }
        }
    }

    #[test]
    fn game_preparation_uses_a_captured_policy_and_refuses_unverified_preferences() {
        use pcl_core::launch_options::{IpPreference, WindowSize};
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let view = shared.launcher_preferences.snapshot();
        let patch = |value| serde_json::from_value(value).unwrap();
        let first = shared
            .launcher_preferences
            .update(
                &view.revision,
                patch(serde_json::json!({
                    "game_launch": {
                        "window": {"mode": "custom", "width": 1280, "height": 720},
                        "ip": "ipv6"
                    }
                })),
            )
            .unwrap();
        let captured = captured_launch_preferences(&shared).unwrap();
        shared
            .launcher_preferences
            .update(
                &first.revision,
                patch(serde_json::json!({"game_launch":{"ip":"ipv4"}})),
            )
            .unwrap();
        assert_eq!(captured.game_launch.ip, IpPreference::Ipv6);
        assert_eq!(
            captured.game_launch.window,
            WindowSize::Custom {
                width: 1280,
                height: 720
            }
        );
        assert_eq!(
            captured_launch_preferences(&shared).unwrap().game_launch.ip,
            IpPreference::Ipv4
        );
        let path = fixture.0.join(".pcl-rust/launcher-preferences.json");
        let bytes = b"{\"schema_version\":200}";
        fs::write(&path, bytes).unwrap();
        assert!(captured_launch_preferences(&shared).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    pub(crate) struct Fixture(pub(crate) PathBuf);
    impl Fixture {
        pub(crate) fn new() -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../work/desktop-integration-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            fs::create_dir_all(path.join("Minecraft/.minecraft")).unwrap();
            Self(path.canonicalize().unwrap())
        }
        pub(crate) fn shared(&self) -> Shared {
            let (config, warning) = ConfigStore::load(&self.0);
            assert!(warning.is_none());
            let tasks = Arc::new(tasks::Tasks::new());
            Shared {
                project: self.0.clone(),
                accounts: Arc::new(accounts::Accounts::new(&self.0)),
                downloads: Arc::new(downloads::Downloads::new(tasks.clone())),
                tasks,
                config,
                instance_metadata: instance_meta::MetadataStore::load(&self.0),
                launcher_preferences: launcher_prefs::LauncherPreferencesStore::load(&self.0),
                launcher_import: launcher_commands::ImportSession::default(),
                launcher_assets: launcher_assets::AssetStore::new(self.0.clone()),
                product_plugins: Arc::new(product_plugins::Runtime::new(&self.0)),
                code: std::sync::OnceLock::new(),
                minecraft_updates: launcher_minecraft_updates::MinecraftUpdates::new(
                    self.0.clone(),
                ),
                launcher_favorites: launcher_favorites::FavoriteStore::load(&self.0),
                launcher_updates: launcher_update_commands::Updater::new(
                    &self.0,
                    launcher_update_commands::current_build(&self.0),
                )
                .unwrap(),
                launcher_local: launcher_runtime::LocalRuntime::new(
                    self.0.clone(),
                    launcher_local::XdgPaths {
                        applications: self.0.join("xdg/applications"),
                        desktop: Some(self.0.join("xdg/desktop")),
                    },
                    &launcher_prefs::LauncherPreferences::default(),
                ),
                desktop: Arc::new(platform::Desktop::default()),
                operations: Mutex::new(()),
                status: Mutex::new(RunStatus::default()),
                stop: AtomicBool::new(false),
                closing: AtomicBool::new(false),
                close_generation: AtomicU64::new(0),
                monitor: launcher_monitor_runtime::MonitorRuntime::default(),
                resource_save: resource_save_commands::SaveSession::default(),
                resource_confirmations: modrinth_install::ConfirmationCache::default(),
                pack_confirmations: instance_import::mrpack::ConfirmationCache::default(),
                toolbox_download: toolbox_download::DownloadSession::default(),
                toolbox_images: toolbox_images::ImageSession::default(),
                log: Mutex::new(None),
            }
        }
        fn second(&self, config: &ConfigStore) -> GameRoot {
            let path = self.0.join("second");
            fs::create_dir_all(&path).unwrap();
            config
                .register(path.to_str().unwrap().into(), Some("Second".into()))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn idle_close_refuses_later_admission_and_is_idempotent() {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let decision = begin_launcher_close(&shared);
        assert!(decision.first_request);
        assert!(!decision.wait_for_workers);
        assert!(decision.active_task_ids.is_empty());
        assert!(require_instance_job(&shared).is_err());
        assert!(require_resource_write(&shared).is_err());
        assert!(with_account_write(&shared, || -> Result<(), String> {
            panic!("closed admission")
        })
        .is_err());
        assert!(!begin_launcher_close(&shared).first_request);
    }

    #[test]
    fn close_waits_for_admission_then_captures_the_admitted_worker() {
        let fixture = Fixture::new();
        let shared = Arc::new(fixture.shared());
        let operation = shared.operations.lock().unwrap();
        let (entered, started) = std::sync::mpsc::channel();
        let (finished, decision) = std::sync::mpsc::channel();
        let closing = shared.clone();
        let closer = thread::spawn(move || {
            entered.send(()).unwrap();
            finished.send(begin_launcher_close(&closing)).unwrap();
        });
        started.recv().unwrap();
        assert!(decision.recv_timeout(Duration::from_millis(30)).is_err());
        assert!(!shared.closing.load(Ordering::SeqCst));
        let root = shared.config.resolve(None).unwrap();
        let task = shared
            .tasks
            .admit(
                TaskTarget {
                    root_id: root.id,
                    root_path: root.path,
                    instance_id: None,
                },
                TaskKind::ToolboxDownload,
            )
            .unwrap();
        let id = task.id().to_owned();
        drop(operation);
        let decision = decision.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(decision.first_request && decision.wait_for_workers);
        assert_eq!(decision.active_task_ids, vec![id]);
        assert!(require_instance_job(&shared).is_err());
        task.finish(TaskOutcome::Failed(
            "finished isolated close fixture".into(),
        ));
        closer.join().unwrap();
        let final_close = begin_launcher_close(&shared);
        assert!(!final_close.first_request && !final_close.wait_for_workers);
    }

    #[test]
    fn close_captures_running_and_queued_workers_from_all_roots() {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let first_root = shared.config.resolve(None).unwrap();
        let second_root = fixture.second(&shared.config);
        let queued = |root: &GameRoot| {
            shared
                .tasks
                .admit_queued(
                    TaskTarget {
                        root_id: root.id.clone(),
                        root_path: root.path.clone(),
                        instance_id: None,
                    },
                    TaskKind::Install,
                    tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
                )
                .unwrap()
        };
        let first = queued(&first_root);
        let second = queued(&second_root);
        first.wait_turn().unwrap();
        second.wait_turn().unwrap();
        let waiting = queued(&first_root);
        assert_eq!(shared.tasks.running_all().len(), 2);
        let decision = begin_launcher_close(&shared);
        assert!(decision.wait_for_workers);
        let captured: std::collections::HashSet<_> = decision
            .active_task_ids
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            captured,
            [first.id(), second.id(), waiting.id()]
                .into_iter()
                .collect()
        );
        assert!(require_network_submission(&shared).is_err());
        for id in &decision.active_task_ids {
            shared.tasks.cancel(id).unwrap();
        }
        assert!(waiting.wait_turn().is_err());
        // Accepted cancellation does not grant another worker a turn or permit
        // close until each owner has released its prepared artifacts.
        assert!(begin_launcher_close(&shared).wait_for_workers);
        waiting.finish(TaskOutcome::Failed(tasks::CANCELLED.into()));
        first.finish(TaskOutcome::Failed(tasks::CANCELLED.into()));
        assert!(begin_launcher_close(&shared).wait_for_workers);
        second.finish(TaskOutcome::Failed(tasks::CANCELLED.into()));
        assert!(!begin_launcher_close(&shared).wait_for_workers);
    }

    #[test]
    fn launcher_file_scope_blocks_containing_root_without_root_id_guessing() {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        let output = Path::new(&root.path).join("download.bin");
        let task = shared
            .tasks
            .admit_queued(
                TaskTarget {
                    root_id: "launcher".into(),
                    root_path: root.path.clone(),
                    instance_id: None,
                },
                TaskKind::ToolboxDownload,
                tasks::TaskScope::files(&[output]).unwrap(),
            )
            .unwrap();
        assert!(require_root_unused(&shared, &root.id).is_err());
        let view = download_tasks_view(&shared).unwrap();
        assert!(view.blocked_root_ids.contains(&root.id));
        assert_eq!(view.collection.tasks.len(), 1);
        task.finish(TaskOutcome::Failed(tasks::CANCELLED.into()));
        assert!(require_root_unused(&shared, &root.id).is_ok());
        assert!(download_tasks_view(&shared)
            .unwrap()
            .blocked_root_ids
            .is_empty());
    }

    #[test]
    fn account_write_keeps_admission_until_its_stage_publication_finishes() {
        let fixture = Fixture::new();
        let shared = Arc::new(fixture.shared());
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let worker = shared.clone();
        let worker_entered = entered.clone();
        let worker_release = release.clone();
        let account = thread::spawn(move || {
            with_account_write(&worker, || {
                worker_entered.wait();
                worker_release.wait();
                Ok(())
            })
        });
        entered.wait();
        assert!(
            shared.operations.try_lock().is_err(),
            "policy adoption cannot enter between admission and account stage publication"
        );
        assert!(
            shared.status.try_lock().is_err(),
            "game preparation cannot enter the same account action"
        );
        let (started, waiting) = std::sync::mpsc::channel();
        let (done, completed) = std::sync::mpsc::channel();
        let policy = shared.clone();
        let adoption = thread::spawn(move || {
            started.send(()).unwrap();
            let _operation = policy.operations.lock().unwrap();
            done.send(()).unwrap();
        });
        waiting.recv().unwrap();
        assert!(completed.recv_timeout(Duration::from_millis(30)).is_err());
        release.wait();
        account.join().unwrap().unwrap();
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        adoption.join().unwrap();
    }

    #[test]
    fn account_write_refuses_close_file_task_game_and_unknown_monitor_without_action() {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let action = || -> Result<(), String> { panic!("blocked account action must not run") };
        shared.closing.store(true, Ordering::SeqCst);
        assert!(with_account_write(&shared, action).is_err());
        shared.closing.store(false, Ordering::SeqCst);
        let root = shared.config.resolve(None).unwrap();
        let task = shared
            .tasks
            .admit(
                TaskTarget {
                    root_id: root.id,
                    root_path: root.path,
                    instance_id: None,
                },
                TaskKind::ResourceOperation,
            )
            .unwrap();
        assert!(with_account_write(&shared, action).is_err());
        task.finish(TaskOutcome::Failed("cancelled fixture".into()));
        for stage in ["preparing", "running"] {
            shared.status.lock().unwrap().stage = stage.into();
            assert!(with_account_write(&shared, action).is_err());
        }
        shared.status.lock().unwrap().stage = "idle".into();
        let marker = fixture.0.join(".pcl-rust/game-monitor/active.json");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let foreign = br#"{"schema_version":999,"retain":"unknown"}"#;
        fs::write(&marker, foreign).unwrap();
        assert!(with_account_write(&shared, action).is_err());
        assert_eq!(fs::read(&marker).unwrap(), foreign);
        assert!(!fixture.0.join(".pcl-rust/accounts.json").exists());
    }

    #[test]
    fn unknown_monitor_marker_blocks_writes_without_replacing_known_context() {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        *shared.status.lock().unwrap() = RunStatus {
            root_id: Some(root.id.clone()),
            root_path: Some(root.path.clone()),
            version: Some("Example".into()),
            ..Default::default()
        };
        let marker = fixture.0.join(".pcl-rust/game-monitor/active.json");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, br#"{"schema_version":999,"external":"keep"}"#).unwrap();
        let before = fs::read(&marker).unwrap();
        assert!(launcher_monitor_runtime::require_idle(&shared).is_err());
        assert!(launcher_monitor_runtime::require_root_unused(&shared, &root.id).is_err());
        assert!(require_reference_write(&shared).is_err());
        assert_eq!(fs::read(&marker).unwrap(), before);
        let run = shared.status.lock().unwrap();
        assert_eq!(run.root_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(run.version.as_deref(), Some("Example"));
        assert_eq!(run.stage, "error");
    }

    #[test]
    fn missing_attached_monitor_is_not_evidence_that_game_exited() {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        launcher_monitor_runtime::test_attach(&shared);
        assert!(launcher_monitor_runtime::refresh(&shared).is_err());
        assert!(launcher_monitor_runtime::test_attached(&shared));
        assert!(launcher_monitor_runtime::require_idle(&shared).is_err());
        assert!(!fixture.0.join(".pcl-rust/game-monitor").exists());
    }

    fn rename_fixture(state: &Shared) -> (GameRoot, String) {
        let root = state.config.resolve(None).unwrap();
        let id = "1.21.1".to_owned();
        let folder = Path::new(&root.path).join("versions").join(&id);
        fs::create_dir_all(folder.join("mods")).unwrap();
        fs::create_dir_all(folder.join("saves/ExampleWorld")).unwrap();
        fs::write(folder.join("saves/ExampleWorld/keep.txt"), b"world data").unwrap();
        fs::write(folder.join("1.21.1.jar"), b"fixture core").unwrap();
        fs::write(
            folder.join("1.21.1.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": id, "mainClass":"net.minecraft.client.main.Main", "libraries": [],
            }))
            .unwrap(),
        )
        .unwrap();
        let mut settings = state.config.snapshot();
        settings.selected = Some(id.clone());
        settings.memory_gib = 14;
        settings.overrides.insert(id.clone(), 12);
        state.config.save(settings).unwrap();
        (root, id)
    }

    #[test]
    fn deleted_name_never_rebinds_old_settings_and_metadata_to_external_recreation() {
        let fixture = Fixture::new();
        let state = Arc::new(fixture.shared());
        let (root, id) = rename_fixture(&state);
        let path = Path::new(&root.path);
        let revision = state.instance_metadata.get(&root.id, path, &id).revision;
        update_instance_metadata(
            &state,
            Some(&root.id),
            &id,
            &revision,
            instance_meta::MetadataPatch {
                description: Some("Retained description".into()),
                favorite: Some(true),
                icon: None,
                category: None,
            },
        )
        .unwrap();
        let settings_before = fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap();
        let metadata_before = fs::read(fixture.0.join(".pcl-rust/instance-metadata.json")).unwrap();
        let plan = instance_delete::prepare(path, &fixture.0, &root.id, &id).unwrap();
        instance_delete::execute(path, &fixture.0, plan, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(bootstrap_view(&state).instances.is_empty());
        assert!(require_root_removable(&state, &root.id)
            .unwrap_err()
            .contains("已删除"));
        assert!(instance_commands::new_name(&state, &root, &id).is_err());
        let folder = path.join("versions").join(&id);
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("{id}.json")),
            serde_json::to_vec(&serde_json::json!({"id":id,"mainClass":"Example","libraries":[]}))
                .unwrap(),
        )
        .unwrap();
        let view = bootstrap_view(&state);
        assert!(view.instances.is_empty());
        assert!(view
            .scan_issues
            .iter()
            .any(|issue| issue.id == id && issue.message.contains("保留")));
        assert!(instance_context(&state.config, Some(&root.id), &id).is_err());
        assert!(admit_resource(&state, &id, Some(&root.id), None, None).is_err());
        let mut entry = instance_delete::history(path, &fixture.0, &root.id)
            .unwrap()
            .remove(0);
        assert!(!entry.can_restore);
        fs::remove_dir_all(&folder).unwrap();
        entry = instance_delete::history(path, &fixture.0, &root.id)
            .unwrap()
            .remove(0);
        instance_delete::undo(
            path,
            &fixture.0,
            &root.id,
            &entry.operation_id,
            entry.revision.as_deref().unwrap(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(require_root_removable(&state, &root.id).is_ok());
        let view = bootstrap_view(&state);
        assert_eq!(view.instances.len(), 1);
        assert_eq!(
            view.instances[0].metadata.description,
            "Retained description"
        );
        assert_eq!(view.settings.memory_gib, 14);
        assert_eq!(view.settings.overrides[&id], 12);
        assert_eq!(
            fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            fs::read(fixture.0.join(".pcl-rust/instance-metadata.json")).unwrap(),
            metadata_before
        );
    }

    #[test]
    fn pending_delete_defers_v2_migration_until_recovery_and_preserves_exact_backup() {
        let fixture = Fixture::new();
        let state = fixture.shared();
        let (root, id) = rename_fixture(&state);
        let path = Path::new(&root.path);
        let file = fixture.0.join(".pcl-rust/settings.json");
        let current: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        // V2 has a frozen field layout. Construct that original document rather
        // than retaining fields added to the current schema by later features.
        let legacy = serde_json::json!({
            "schema_version": 2,
            "active_root_id": current["active_root_id"],
            "player": current["player"],
            "memory_gib": current["memory_gib"],
            "roots": current["roots"].as_array().unwrap().iter().map(|registered| {
                serde_json::json!({
                    "id": registered["id"], "name": registered["name"],
                    "path": registered["path"], "selected": registered["selected"],
                    "overrides": registered["overrides"],
                })
            }).collect::<Vec<_>>(),
        });
        let before = serde_json::to_vec_pretty(&legacy).unwrap();
        fs::write(&file, &before).unwrap();
        let checked = instance_delete::prepare(path, &fixture.0, &root.id, &id).unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            instance_delete::execute(
                path,
                &fixture.0,
                checked,
                &AtomicBool::new(false),
                |progress| {
                    if progress.phase == "committing" {
                        panic!("Interrupted before directory move");
                    }
                },
            )
            .unwrap();
        }))
        .is_err());
        let (config, warning) = ConfigStore::load(&fixture.0);
        assert!(warning.is_none(), "{warning:?}");
        assert_eq!(config.snapshot().memory_gib, 14);
        assert_eq!(fs::read(&file).unwrap(), before);
        assert!(instance_rename_refs::ensure_project_ready(&fixture.0).is_err());
        instance_delete::recover_pending(path, &fixture.0).unwrap();
        config.refresh_after_rename().unwrap();
        let mut next = config.snapshot();
        next.player = "ExamplePlayer".into();
        config.save(next).unwrap();
        let migrated: serde_json::Value =
            serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(migrated["schema_version"], 3);
        assert_eq!(migrated["memory_gib"], 14);
        assert_eq!(migrated["isolation"], "automatic");
        assert_eq!(
            migrated["roots"][0]["isolation_overrides"],
            serde_json::json!({})
        );
        assert_eq!(
            migrated["roots"][0]["window_title_overrides"],
            serde_json::json!({})
        );
        let backups: Vec<_> = fs::read_dir(fixture.0.join(".pcl-rust"))
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.v2-backup-")
                    .then_some(entry.path())
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), before);
        assert!(path.join("versions").join(id).exists());
    }

    #[test]
    fn new_instance_name_rejects_cleared_metadata_and_root_scoped_old_references() {
        let fixture = Fixture::new();
        let state = fixture.shared();
        let (root, id) = rename_fixture(&state);
        let path = Path::new(&root.path);
        let revision = state.instance_metadata.get(&root.id, path, &id).revision;
        let patch = |description| instance_meta::MetadataPatch {
            description: Some(description),
            favorite: None,
            icon: None,
            category: None,
        };
        let changed = update_instance_metadata(
            &state,
            Some(&root.id),
            &id,
            &revision,
            patch("Edited".into()),
        )
        .unwrap();
        update_instance_metadata(
            &state,
            Some(&root.id),
            &id,
            &changed.revision,
            patch(String::new()),
        )
        .unwrap();
        let mut settings = state.config.snapshot();
        settings.selected = None;
        settings.overrides.clear();
        state.config.save(settings).unwrap();
        assert!(instance_commands::new_name(&state, &root, &id)
            .unwrap_err()
            .contains("资料"));
        assert!(instance_commands::new_name(&state, &root, "Fresh Instance").is_ok());
        let second = fixture.second(&state.config);
        assert!(instance_commands::new_name(&state, &second, &id).is_ok());
    }

    #[test]
    fn rename_refresh_preserves_browsed_root_memory_metadata_presets_and_resource_undo() {
        use std::os::unix::fs::MetadataExt;
        let fixture = Fixture::new();
        let state = fixture.shared();
        let (root, old) = rename_fixture(&state);
        let new = "Example Renamed";
        let path = Path::new(&root.path);
        let world = path
            .join("versions")
            .join(&old)
            .join("saves/ExampleWorld/keep.txt");
        let inode = world.metadata().unwrap().ino();
        let revision = state.instance_metadata.get(&root.id, path, &old).revision;
        update_instance_metadata(
            &state,
            Some(&root.id),
            &old,
            &revision,
            instance_meta::MetadataPatch {
                description: Some("Keep my description".into()),
                favorite: Some(true),
                icon: None,
                category: None,
            },
        )
        .unwrap();
        let preset = instance_export::ExportRequest {
            name: "Example Pack".into(),
            version: "1.0".into(),
            checks: Default::default(),
            excluded: Default::default(),
        };
        export_presets::save(&fixture.0, &root.id, path, &old, preset.clone()).unwrap();
        let mod_path = path.join("versions").join(&old).join("mods/example.jar");
        fs::write(&mod_path, b"example resource").unwrap();
        let removal = resource_ops::remove(
            path,
            &old,
            "mods",
            &[resource_ops::ResourceFile {
                file_name: "example.jar".into(),
                fingerprint: resource_ops::fingerprint(&mod_path).unwrap(),
            }],
            &AtomicBool::new(false),
            &mut || Ok(()),
            &mut |_, _| {},
        )
        .unwrap();
        let second = fixture.second(&state.config);
        let mut settings = state.config.select(&second.id).unwrap();
        settings.selected = Some("Other".into());
        settings.overrides.insert("Other".into(), 4);
        state.config.save(settings.clone()).unwrap();
        let checked = instance_rename_service::prepare(&state, &root, &old, new).unwrap();
        let task = state
            .tasks
            .admit(
                TaskTarget {
                    root_id: root.id.clone(),
                    root_path: root.path.clone(),
                    instance_id: Some(old.clone()),
                },
                TaskKind::InstanceRename,
            )
            .unwrap();
        let task_id = task.id().to_owned();
        let result = instance_rename_service::execute(&state, &root, checked, &task).unwrap();
        assert_eq!(result["id"], new);
        finish_instance_task(task, Ok(result), "实例重命名完成");
        assert_eq!(
            state.tasks.snapshot(&task_id).unwrap().stage,
            tasks::TaskStage::Complete
        );
        let mut refreshed = state.config.snapshot();
        assert_ne!(refreshed.revision, settings.revision);
        refreshed.revision = settings.revision.clone();
        assert_eq!(refreshed, settings);
        let renamed_root = state.config.resolve(Some(&root.id)).unwrap();
        assert_eq!(renamed_root.selected.as_deref(), Some(new));
        assert_eq!(renamed_root.overrides.get(new), Some(&12));
        assert!(!renamed_root.overrides.contains_key(&old));
        let metadata = state.instance_metadata.get(&root.id, path, new);
        assert!(metadata.favorite);
        assert_eq!(metadata.description, "Keep my description");
        assert_eq!(
            export_presets::read(&fixture.0, &root.id, path, new).unwrap(),
            Some(preset)
        );
        assert!(export_presets::read(&fixture.0, &root.id, path, &old)
            .unwrap()
            .is_none());
        let new_world = path
            .join("versions")
            .join(new)
            .join("saves/ExampleWorld/keep.txt");
        assert_eq!(new_world.metadata().unwrap().ino(), inode);
        assert_eq!(fs::read(new_world).unwrap(), b"world data");
        let removed = resource_ops::removed(path, new, "mods").unwrap();
        assert_eq!(removed[0].id, removal.undo_id.unwrap());
        resource_ops::restore(
            path,
            new,
            "mods",
            &removed[0].id,
            &AtomicBool::new(false),
            &mut || Ok(()),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(
            fs::read(path.join("versions").join(new).join("mods/example.jar")).unwrap(),
            b"example resource"
        );
        state.config.select(&root.id).unwrap();
        let bootstrap = bootstrap_view(&state);
        let instance = bootstrap
            .instances
            .iter()
            .find(|instance| instance.instance.id == new)
            .unwrap();
        assert_eq!(instance.instance.minecraft_version, old);
        assert!(instance.metadata.favorite);
        assert!(bootstrap.rename_recovery_error.is_none());
    }

    #[test]
    fn rename_revision_and_global_recovery_guard_include_scoped_launcher_references() {
        let fixture = Fixture::new();
        let state = fixture.shared();
        let (root, old) = rename_fixture(&state);
        let before = instance_rename_service::prepare(&state, &root, &old, "Example Renamed")
            .unwrap()
            .revision;
        let mut settings = state.config.snapshot();
        settings.overrides.insert(old.clone(), 10);
        state.config.save(settings).unwrap();
        let after = instance_rename_service::prepare(&state, &root, &old, "Example Renamed")
            .unwrap()
            .revision;
        assert_ne!(before, after);
        let second = fixture.second(&state.config);
        state.config.select(&second.id).unwrap();
        // Recapture references after the legitimate navigation, then emulate
        // a durable pending transaction without touching any game contents.
        let refs = instance_rename_refs::prepare(
            &fixture.0,
            &root.id,
            Path::new(&root.path),
            &old,
            "Example Renamed",
        )
        .unwrap();
        refs.mark_pending(&fixture.0, Path::new(&root.path), "n-abcdef-123-1")
            .unwrap();
        let bootstrap = bootstrap_view(&state);
        assert!(bootstrap.rename_recovery_error.is_some());
        assert_eq!(
            bootstrap.rename_recovery_root_id.as_deref(),
            Some(root.id.as_str())
        );
        assert_eq!(bootstrap.settings.root_id, second.id);
        assert!(require_reference_write(&state).is_err());
        assert!(ensure_instance_files_ready(&state, &root).is_err());
        refs.clear_pending(&fixture.0, Path::new(&root.path), "n-abcdef-123-1")
            .unwrap();
        assert!(require_reference_write(&state).is_ok());
    }

    #[test]
    fn rename_recovery_resolves_registered_directory_alias() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let state = fixture.shared();
        let physical = fixture.0.join("alias-target");
        fs::create_dir_all(&physical).unwrap();
        let alias = fixture.0.join("registered-alias");
        symlink(&physical, &alias).unwrap();
        let registered = state
            .config
            .register(alias.to_str().unwrap().into(), None)
            .unwrap();
        state.config.select(&registered.id).unwrap();
        let (root, old) = rename_fixture(&state);
        assert_eq!(Path::new(&root.path), physical);
        let second = fixture.second(&state.config);
        state.config.select(&second.id).unwrap();
        let refs = instance_rename_refs::prepare(
            &fixture.0,
            &root.id,
            Path::new(&root.path),
            &old,
            "Example Renamed",
        )
        .unwrap();
        refs.mark_pending(&fixture.0, Path::new(&root.path), "n-abcdef-123-2")
            .unwrap();
        let bootstrap = bootstrap_view(&state);
        assert!(bootstrap.rename_recovery_error.is_some());
        assert_eq!(
            bootstrap.rename_recovery_root_id.as_deref(),
            Some(root.id.as_str())
        );
        assert!(bootstrap
            .roots
            .iter()
            .any(|item| item.id == root.id && item.available && Path::new(&item.path) == alias));
        refs.clear_pending(&fixture.0, Path::new(&root.path), "n-abcdef-123-2")
            .unwrap();
    }

    #[test]
    fn captured_instance_context_survives_browsing_another_root() {
        use pcl_core::java::JavaSelection;
        let fixture = Fixture::new();
        let state = fixture.shared();
        let first = state.config.snapshot().root_id;
        let second = fixture.second(&state.config);
        let mut settings = state.config.snapshot();
        settings.memory_gib = 14;
        settings.overrides.insert("Same".into(), 12);
        let global_java = JavaSelection::Manual {
            path: fixture.0.join("global/bin/java").to_str().unwrap().into(),
        };
        settings.java = global_java.clone();
        settings
            .java_overrides
            .insert("Same".into(), JavaSelection::Auto);
        state.config.save(settings).unwrap();
        let (captured_settings, captured_root) =
            instance_context(&state.config, None, "Same").unwrap();
        let mut settings = state.config.select(&second.id).unwrap();
        settings.overrides.insert("Same".into(), 4);
        let second_java = JavaSelection::Manual {
            path: fixture.0.join("other/bin/java").to_str().unwrap().into(),
        };
        settings
            .java_overrides
            .insert("Same".into(), second_java.clone());
        state.config.save(settings).unwrap();
        assert_eq!(captured_root.id, first);
        assert_eq!(captured_settings.memory_gib, 14);
        assert_eq!(captured_root.overrides["Same"], 12);
        assert_eq!(
            java_service::effective_choice(&captured_settings, &captured_root, "Same"),
            &JavaSelection::Auto,
        );
        assert_eq!(
            java_service::effective_choice(&captured_settings, &captured_root, "Other"),
            &global_java,
        );
        let (_, bound) = instance_context(&state.config, Some(&first), "Same").unwrap();
        assert_eq!(bound, captured_root);
        let (current_settings, current_root) =
            instance_context(&state.config, None, "Same").unwrap();
        assert_eq!(
            java_service::effective_choice(&current_settings, &current_root, "Same"),
            &second_java,
        );
        assert_eq!(
            instance_context(&state.config, None, "Same")
                .unwrap()
                .1
                .overrides["Same"],
            4
        );
    }

    fn java_fixture(fixture: &Fixture, wait_for_release: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = fixture.0.join("jdk/bin/java");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let wait = if wait_for_release {
            "while [ ! -f \"$0.release\" ]; do sleep 0.01; done\n"
        } else {
            ""
        };
        fs::write(&path, format!(
            "#!/bin/sh\nprintf inspected > \"$0.inspected\"\n{wait}printf 'java.specification.version = 21\\njava.vendor = Fixture\\nos.arch = {}\\n' >&2\n",
            std::env::consts::ARCH,
        )).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn java_registration_rejects_stale_requests_before_execution_and_keeps_selection() {
        use pcl_core::java::JavaSelection;
        let fixture = Fixture::new();
        let state = fixture.shared();
        let java = java_fixture(&fixture, false);
        let settings = state.config.snapshot();
        let context =
            java_service::registration_context(&state, Some(&settings.root_id), &settings.revision)
                .unwrap();
        let mut newer = settings;
        newer.memory_gib = 14;
        state.config.save(newer).unwrap();
        assert!(java_service::register(&state, &java, &context).is_err());
        assert!(!java.with_file_name("java.inspected").exists());
        assert!(state.config.snapshot().java_paths.is_empty());

        let settings = state.config.snapshot();
        let context =
            java_service::registration_context(&state, Some(&settings.root_id), &settings.revision)
                .unwrap();
        let registered = java_service::register(&state, &java, &context).unwrap();
        assert_ne!(registered.revision, settings.revision);
        assert_eq!(registered.memory_gib, 14);
        assert_eq!(registered.java, JavaSelection::Auto);
        assert_eq!(registered.java_paths, vec![java.to_str().unwrap()]);
        let repeated = java_service::registration_context(
            &state,
            Some(&registered.root_id),
            &registered.revision,
        )
        .unwrap();
        assert_eq!(
            java_service::register(&state, &java, &repeated)
                .unwrap()
                .java_paths,
            registered.java_paths
        );

        let second = fixture.second(&state.config);
        state.config.select(&second.id).unwrap();
        fs::remove_file(java.with_file_name("java.inspected")).unwrap();
        assert!(java_service::register(&state, &java, &repeated).is_err());
        assert!(!java.with_file_name("java.inspected").exists());
    }

    #[test]
    fn java_registration_rechecks_settings_after_the_probe() {
        let fixture = Fixture::new();
        let state = Arc::new(fixture.shared());
        let java = java_fixture(&fixture, true);
        let settings = state.config.snapshot();
        let context =
            java_service::registration_context(&state, Some(&settings.root_id), &settings.revision)
                .unwrap();
        let worker_state = state.clone();
        let worker_java = java.clone();
        let worker =
            thread::spawn(move || java_service::register(&worker_state, &worker_java, &context));
        let started = java.with_file_name("java.inspected");
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !started.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(started.exists());
        let mut changed = state.config.snapshot();
        changed.memory_gib = 14;
        state.config.save(changed).unwrap();
        fs::write(java.with_file_name("java.release"), b"continue").unwrap();
        assert!(worker.join().unwrap().is_err());
        assert_eq!(state.config.snapshot().memory_gib, 14);
        assert!(state.config.snapshot().java_paths.is_empty());
    }

    #[test]
    fn uncaptured_resource_confirmation_keeps_explicit_root_and_settings_before_admission() {
        let fixture = Fixture::new();
        let state = Arc::new(fixture.shared());
        let (first, id) = rename_fixture(&state);
        let second = fixture.second(&state.config);
        state.config.select(&second.id).unwrap();
        let settings = fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap();
        let request = modrinth_install::InstallRequest {
            project_id: "ABCD1234".into(),
            version_id: "12345678".into(),
            file_name: None,
        };
        let rejected = resource_install_commands::start_request(
            state.clone(),
            Some(&first.id),
            id.clone(),
            request,
            "confirmed".into(),
        )
        .unwrap_err();
        assert!(rejected.contains("重新检查"));
        // A fabricated UI credential cannot admit a worker, make an HTTP
        // request or retarget the currently selected root. Genuine queued
        // confirmations are exercised at the service's production boundary.
        assert!(state.tasks.list().is_empty());
        assert!(state.tasks.active().is_none());
        assert_eq!(state.config.snapshot().root_id, second.id);
        assert_eq!(
            settings,
            fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap()
        );
        assert!(Path::new(&first.path)
            .join("versions")
            .join(&id)
            .join("mods")
            .read_dir()
            .unwrap()
            .next()
            .is_none());
        assert!(!fixture.0.join(".pcl-rust/resource-downloads").exists());
    }

    #[test]
    fn interrupted_resource_batch_exposes_recovery_and_keeps_root_registered() {
        let fixture = Fixture::new();
        let state = fixture.shared();
        let first = state.config.resolve(None).unwrap();
        let second = fixture.second(&state.config);
        let pending = Path::new(&first.path).join(".pcl-linux/resource-batches/unknown-record");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("retain.txt"), b"unknown recovery data").unwrap();
        assert!(bootstrap_view(&state)
            .resource_install_recovery_error
            .is_some());
        assert!(require_root_removable(&state, &first.id).is_err());
        state.config.select(&second.id).unwrap();
        assert!(bootstrap_view(&state)
            .resource_install_recovery_error
            .is_none());
        assert!(require_root_removable(&state, &second.id).is_ok());
        assert!(resource_ops::recover_verified_batches(Path::new(&first.path)).is_err());
        assert_eq!(
            fs::read(pending.join("retain.txt")).unwrap(),
            b"unknown recovery data"
        );
        assert!(state.config.resolve(Some(&first.id)).is_ok());
    }

    #[test]
    fn instance_jobs_wait_for_game_and_all_writers_and_refuse_shutdown() {
        let fixture = Fixture::new();
        let state = fixture.shared();
        assert!(require_instance_job(&state).is_ok());
        for stage in ["preparing", "running"] {
            state.status.lock().unwrap().stage = stage.into();
            assert!(require_instance_job(&state).is_err());
        }
        state.status.lock().unwrap().stage = "idle".into();
        let root = state.config.resolve(None).unwrap();
        for kind in [
            TaskKind::Install,
            TaskKind::ResourceOperation,
            TaskKind::InstanceReset,
            TaskKind::InstanceRepair,
            TaskKind::InstanceExport,
            TaskKind::InstanceRename,
            TaskKind::InstanceImport,
            TaskKind::InstanceDelete,
            TaskKind::InstanceRestore,
            TaskKind::ResourceDownload,
            TaskKind::ResourceUpdate,
            TaskKind::ResourceUpdateRestore,
        ] {
            let task = state
                .tasks
                .admit(
                    TaskTarget {
                        root_id: root.id.clone(),
                        root_path: root.path.clone(),
                        instance_id: Some("Example".into()),
                    },
                    kind,
                )
                .unwrap();
            assert!(require_instance_job(&state).is_err());
            state.tasks.cancel(task.id()).unwrap();
            assert!(require_instance_job(&state).is_err());
            task.finish(TaskOutcome::Failed("stopped".into()));
            assert!(require_instance_job(&state).is_ok());
        }
        state.closing.store(true, Ordering::SeqCst);
        assert!(require_instance_job(&state).is_err());
    }

    #[test]
    fn root_removal_guard_tracks_bound_task_and_process_after_switching() {
        let fixture = Fixture::new();
        let state = fixture.shared();
        let first = state.config.resolve(None).unwrap();
        let second = fixture.second(&state.config);
        let task = state
            .tasks
            .admit_queued(
                TaskTarget {
                    root_id: first.id.clone(),
                    root_path: first.path.clone(),
                    instance_id: Some("Same".into()),
                },
                TaskKind::Install,
                tasks::TaskScope::root(Path::new(&first.path)).unwrap(),
            )
            .unwrap();
        state.config.select(&second.id).unwrap();
        assert!(require_root_unused(&state, &first.id).is_err());
        assert!(require_root_unused(&state, &second.id).is_ok());
        state.tasks.cancel(task.id()).unwrap();
        assert!(require_root_unused(&state, &first.id).is_err());
        task.finish(TaskOutcome::Failed("cancelled".into()));
        assert!(require_root_unused(&state, &first.id).is_ok());
        *state.status.lock().unwrap() = RunStatus {
            stage: "running".into(),
            root_id: Some(first.id.clone()),
            root_path: Some(first.path),
            ..Default::default()
        };
        assert!(require_root_unused(&state, &first.id).is_err());
        assert!(require_root_unused(&state, &second.id).is_ok());
        update(&state, "exited", "finished".into(), None, Some(0));
        assert!(require_root_unused(&state, &first.id).is_ok());
    }

    #[test]
    fn resource_admission_keeps_captured_root_and_excludes_all_other_writers() {
        let fixture = Fixture::new();
        let state = Arc::new(fixture.shared());
        let first = state.config.resolve(None).unwrap();
        let second = fixture.second(&state.config);
        let (bound, task) = admit_resource(&state, "Same", Some(&first.id), None, None).unwrap();
        assert_eq!(bound.root.id, first.id);
        state.config.select(&second.id).unwrap();
        assert!(admit_resource(&state, "Same", Some(&second.id), None, None).is_err());
        assert!(require_root_unused(&state, &first.id).is_err());
        // This legacy mutation keeps a global lease until it is migrated to
        // explicit queue scopes; registry changes wait alongside other writers.
        assert!(require_root_unused(&state, &second.id).is_err());
        state.tasks.cancel(task.id()).unwrap();
        assert!(state.tasks.active().is_some());
        assert!(admit_resource(&state, "Same", Some(&second.id), None, None).is_err());
        task.finish(TaskOutcome::Failed("cancelled".into()));
        assert!(state.tasks.active().is_none());
        assert!(require_root_unused(&state, &first.id).is_ok());
        assert!(require_root_unused(&state, &second.id).is_ok());
        let (_, task) = admit_resource(&state, "Same", Some(&second.id), None, None).unwrap();
        task.finish(TaskOutcome::Complete {
            result: None,
            message: "done".into(),
            error: None,
        });
        *state.status.lock().unwrap() = RunStatus {
            stage: "running".into(),
            root_id: Some(first.id),
            ..Default::default()
        };
        assert!(admit_resource(&state, "Same", Some(&second.id), None, None).is_err());
        assert!(state.tasks.active().is_none());
    }

    #[test]
    fn import_admission_rejects_directory_retargeted_after_file_selection() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let state = Arc::new(fixture.shared());
        let first = fixture.0.join("source-a");
        let second = fixture.0.join("source-b");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let alias = fixture.0.join("chosen-folder");
        symlink(&first, &alias).unwrap();
        let registered = state
            .config
            .register(alias.to_str().unwrap().into(), None)
            .unwrap();
        let initial = state.config.resolve(Some(&registered.id)).unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(&second, &alias).unwrap();
        assert!(
            admit_resource(&state, "Same", Some(&initial.id), Some(&initial.path), None).is_err()
        );
        assert!(state.tasks.active().is_none());
        state.config.remove(&registered.id).unwrap();
        assert!(
            admit_resource(&state, "Same", Some(&initial.id), Some(&initial.path), None).is_err()
        );
        assert!(state.tasks.active().is_none());
    }

    fn metadata_fixture_version(root: &GameRoot) {
        let folder = Path::new(&root.path).join("versions/Same");
        fs::create_dir_all(folder.join("mods")).unwrap();
        fs::write(
            folder.join("Same.json"),
            r#"{"id":"Same","clientVersion":"1.21.1","libraries":[]}"#,
        )
        .unwrap();
    }

    #[test]
    fn instance_metadata_keeps_root_scope_and_bootstrap_physical_identity() {
        let fixture = Fixture::new();
        let state = fixture.shared();
        let first = state.config.resolve(None).unwrap();
        let second = fixture.second(&state.config);
        metadata_fixture_version(&first);
        metadata_fixture_version(&second);
        let view = state
            .instance_metadata
            .get(&first.id, Path::new(&first.path), "Same");
        state.config.select(&second.id).unwrap();
        let config_before = fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap();
        let saved = update_instance_metadata(
            &state,
            Some(&first.id),
            "Same",
            &view.revision,
            instance_meta::MetadataPatch {
                description: Some("Local description".into()),
                favorite: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(saved.metadata().favorite);
        assert_eq!(
            fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap(),
            config_before
        );
        let second_view = bootstrap_view(&state);
        assert_eq!(second_view.instances.len(), 1);
        assert!(!second_view.instances[0].metadata.favorite);
        assert!(second_view.instances[0].metadata.description.is_empty());
        state.config.select(&first.id).unwrap();
        let first_view = bootstrap_view(&state);
        assert_eq!(first_view.instances[0].instance.id, "Same");
        assert_eq!(
            first_view.instances[0].metadata.description,
            "Local description"
        );
        let projection = serde_json::to_value(&first_view.instances[0]).unwrap();
        assert_eq!(projection["id"], "Same");
        assert_eq!(projection["metadata_revision"], saved.revision);
        assert!(Path::new(&first.path)
            .join("versions/Same/Same.json")
            .is_file());
    }

    #[test]
    fn instance_metadata_rejects_wrong_scope_busy_missing_and_removed_targets() {
        let fixture = Fixture::new();
        let state = fixture.shared();
        let first = state.config.resolve(None).unwrap();
        let second = fixture.second(&state.config);
        metadata_fixture_version(&first);
        metadata_fixture_version(&second);
        let first_view = state
            .instance_metadata
            .get(&first.id, Path::new(&first.path), "Same");
        let patch = || instance_meta::MetadataPatch {
            favorite: Some(true),
            ..Default::default()
        };
        assert!(update_instance_metadata(
            &state,
            Some(&second.id),
            "Same",
            &first_view.revision,
            patch()
        )
        .is_err());
        assert!(update_instance_metadata(
            &state,
            Some(&first.id),
            "Missing",
            &first_view.revision,
            patch()
        )
        .is_err());
        let task = state
            .tasks
            .admit(
                TaskTarget {
                    root_id: first.id.clone(),
                    root_path: first.path.clone(),
                    instance_id: Some("Same".into()),
                },
                TaskKind::ResourceOperation,
            )
            .unwrap();
        assert!(update_instance_metadata(
            &state,
            Some(&first.id),
            "Same",
            &first_view.revision,
            patch()
        )
        .is_err());
        task.finish(TaskOutcome::Failed("stopped".into()));
        for stage in ["preparing", "running"] {
            state.status.lock().unwrap().stage = stage.into();
            assert!(update_instance_metadata(
                &state,
                Some(&first.id),
                "Same",
                &first_view.revision,
                patch()
            )
            .is_err());
        }
        state.status.lock().unwrap().stage = "idle".into();
        let second_view = state
            .instance_metadata
            .get(&second.id, Path::new(&second.path), "Same");
        state.config.remove(&second.id).unwrap();
        assert!(update_instance_metadata(
            &state,
            Some(&second.id),
            "Same",
            &second_view.revision,
            patch()
        )
        .is_err());
        assert!(!fixture.0.join(".pcl-rust/instance-metadata.json").exists());
        assert!(
            !state
                .instance_metadata
                .get(&first.id, Path::new(&first.path), "Same")
                .metadata()
                .favorite
        );
    }
}
