//! Launcher preference admission and native interaction. Global game argv
//! defaults travel with preferences; root/instance bindings and accounts are
//! deliberately absent from the settings transfer protocol.
//!
//! A chooser owns no game lock. Imports retain one bounded, expiring byte
//! snapshot until the user confirms; apply rechecks the preference revision.
use crate::{launcher_prefs::*, Shared};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::State;

struct PendingImport {
    token: String,
    revision: String,
    bytes: Vec<u8>,
    prepared: Instant,
}
#[derive(Default)]
pub struct ImportSession(Mutex<Option<PendingImport>>);

#[derive(Serialize)]
pub struct ImportPreview {
    token: String,
    revision: String,
    preferences: LauncherPreferences,
}
#[derive(Serialize)]
pub struct SettingsSaved {
    path: String,
}

fn minecraft_notice_policy_changed(
    before: &LauncherPreferences,
    after: &LauncherPreferences,
) -> bool {
    before.network != after.network
        || before.management.minecraft_release_notifications
            != after.management.minecraft_release_notifications
        || before.management.minecraft_snapshot_notifications
            != after.management.minecraft_snapshot_notifications
}

pub fn network_policy(prefs: &LauncherPreferences) -> pcl_network::Policy {
    use pcl_network::{DnsFallback, DnsPolicy, Policy, ProxyPolicy};
    Policy {
        proxy: match prefs.network.proxy_mode {
            ProxyMode::None => ProxyPolicy::None,
            ProxyMode::System => ProxyPolicy::System,
            ProxyMode::Custom => ProxyPolicy::Custom(prefs.network.custom_proxy_url.clone()),
        },
        dns: if prefs.network.doh_enabled {
            DnsPolicy::Doh {
                fallback: DnsFallback::System,
            }
        } else {
            DnsPolicy::System
        },
    }
}

/// All live submissions capture the same scheduler. Policy changes are admitted
/// only when idle, so concurrent tasks cannot split the total transfer budget.
pub fn download_policy(prefs: &LauncherPreferences) -> pcl_network::DownloadPolicy {
    pcl_network::DownloadPolicy {
        max_concurrent_transfers: prefs.management.max_concurrent_transfers,
        total_rate_limit_mib_per_second: prefs.management.total_rate_limit_mib_per_second,
        forbid_cross_root_cache_copy: prefs.advanced.forbid_download_copy,
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn download_policy_keeps_budgets_and_cache_permission_in_same_snapshot() {
        let mut prefs = LauncherPreferences::default();
        prefs.management.max_concurrent_transfers = 12;
        prefs.management.total_rate_limit_mib_per_second = 7;
        prefs.advanced.forbid_download_copy = true;
        let policy = download_policy(&prefs);
        assert_eq!(policy.max_concurrent_transfers, 12);
        assert_eq!(policy.total_rate_limit_mib_per_second, 7);
        assert!(policy.forbid_cross_root_cache_copy);
        assert!(pcl_network::DownloadScheduler::new(policy).is_ok());
        prefs.management.max_concurrent_transfers = 0;
        assert!(prefs.validate().is_err());
        assert!(pcl_network::DownloadScheduler::new(download_policy(&prefs)).is_err());
    }

    #[test]
    fn a_live_queue_cannot_split_the_shared_download_budget() {
        let fixture = crate::integration_tests::Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        let task = shared
            .tasks
            .admit_queued(
                crate::tasks::TaskTarget {
                    root_id: root.id,
                    root_path: root.path.clone(),
                    instance_id: None,
                },
                crate::tasks::TaskKind::Install,
                crate::tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
            )
            .unwrap();
        let current = pcl_network::download_snapshot().policy().clone();
        let mut next = LauncherPreferences::default();
        next.management.max_concurrent_transfers = current.max_concurrent_transfers;
        next.management.total_rate_limit_mib_per_second = current.total_rate_limit_mib_per_second;
        next.advanced.forbid_download_copy = current.forbid_cross_root_cache_copy;
        assert!(prepare_download_change(&shared, &next).unwrap().is_none());
        next.management.max_concurrent_transfers = if current.max_concurrent_transfers == 64 {
            63
        } else {
            64
        };
        assert!(prepare_download_change(&shared, &next).is_err());
        drop(task);
        assert!(prepare_download_change(&shared, &next).unwrap().is_some());
    }
}
fn prepare_download_change(
    shared: &Shared,
    next: &LauncherPreferences,
) -> Result<Option<Arc<pcl_network::DownloadScheduler>>, String> {
    let policy = download_policy(next);
    if pcl_network::download_snapshot().policy() == &policy {
        return Ok(None);
    }
    // Concurrent jobs must share one slot/rate budget. Swapping this Arc while
    // a worker or queued submission retains the previous one would create two
    // independent budgets and exceed the user's total limit.
    require_network_idle(shared)?;
    pcl_network::DownloadScheduler::new(policy).map(Some)
}

fn require_network_idle(shared: &Shared) -> Result<(), String> {
    crate::launcher_monitor_runtime::require_idle(shared)?;
    if shared.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    if shared.tasks.active().is_some()
        || shared.launcher_updates.busy()
        || matches!(
            shared.status.lock().unwrap().stage.as_str(),
            "preparing" | "running"
        )
        || shared.accounts.busy()
    {
        return Err("请在文件任务、更新、游戏和微软登录结束后更改网络策略".into());
    }
    Ok(())
}
fn prepare_network_change(
    shared: &Shared,
    next: &LauncherPreferences,
) -> Result<Option<Arc<pcl_network::ClientFactory>>, String> {
    let policy = network_policy(next);
    if shared.launcher_preferences.snapshot().preferences.network == next.network
        && pcl_network::snapshot().policy() == &policy
    {
        return Ok(None);
    }
    require_network_idle(shared)?;
    Ok(Some(Arc::new(pcl_network::ClientFactory::new(policy)?)))
}

#[tauri::command]
pub fn launcher_network_status() -> pcl_network::NetworkStatus {
    pcl_network::status()
}

pub fn apply_window(
    window: &tauri::WebviewWindow,
    prefs: &LauncherPreferences,
) -> tauri::Result<()> {
    window.set_resizable(!prefs.appearance.lock_window_size)?;
    window.set_maximizable(!prefs.appearance.lock_window_size)?;
    window.set_title(if prefs.title.mode == TitleMode::Text {
        &prefs.title.text
    } else {
        "PCL RH"
    })
}

/// Apply window hints after persistence and return the committed revision even
/// when the compositor refuses them. Runtime warnings do not make a valid store
/// readonly, and are also visible on the next independent snapshot.
pub fn finish_window_effect(
    shared: &Shared,
    window: &tauri::WebviewWindow,
    view: &mut LauncherPreferencesView,
) {
    let _effect = shared.launcher_local.window_effects.lock().unwrap();
    // A chooser or async store write can finish out of order. Always derive
    // native hints and the returned preferences from the latest valid store.
    *view = shared.launcher_view();
    shared.launcher_local.warnings.set(
        crate::launcher_runtime::WarningKind::Window,
        apply_window(window, &view.preferences)
            .err()
            .map(|_| "设置已保存，但窗口管理器未能应用窗口设置".into()),
    );
    view.runtime_warning = shared.launcher_local.warnings.message();
}

#[tauri::command]
pub async fn launcher_preferences(
    reload: Option<bool>,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<LauncherPreferencesView, String> {
    let shared = state.inner().clone();
    let fresh = reload.unwrap_or(false);
    let mut view = tauri::async_runtime::spawn_blocking(move || {
        if !fresh {
            return Ok(shared.launcher_view());
        }
        let _operation = shared.operations.lock().unwrap();
        // An explicit reread can adopt any valid externally edited field,
        // including network policy. Exclude consumers before adopting it.
        require_network_idle(&shared)?;
        let mut view = shared.launcher_preferences.reload();
        // Explicit adoption also retires an in-flight notice batch, including
        // a failed reload. Cached preferences must not acknowledge it as fresh.
        shared.minecraft_updates.invalidate();
        if view.warning.is_none() {
            let result = pcl_network::ClientFactory::new(network_policy(&view.preferences));
            match result {
                Ok(factory) => {
                    pcl_network::install_snapshot(Arc::new(factory));
                    shared
                        .launcher_local
                        .warnings
                        .set(crate::launcher_runtime::WarningKind::Network, None);
                }
                Err(_) => shared.launcher_local.warnings.set(
                    crate::launcher_runtime::WarningKind::Network,
                    Some("重新读取了启动器设置，但网络策略未能应用".into()),
                ),
            }
            if let Ok(downloads) =
                pcl_network::DownloadScheduler::new(download_policy(&view.preferences))
            {
                pcl_network::install_download_snapshot(downloads);
            }
            shared.launcher_local.apply_preferences(&view.preferences);
        }
        view.runtime_warning = shared.launcher_local.warnings.message();
        Ok::<_, String>(view)
    })
    .await
    .map_err(|_| "读取启动器设置的任务意外退出".to_string())??;
    if fresh {
        finish_window_effect(&state, &window, &mut view);
    }
    Ok(view)
}

#[tauri::command]
pub async fn launcher_preferences_update(
    revision: String,
    patch: LauncherPreferencesPatch,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<LauncherPreferencesView, String> {
    let shared = state.inner().clone();
    let mut view = tauri::async_runtime::spawn_blocking(move || {
        let _admission = shared.operations.lock().unwrap();
        if shared.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("启动器正在关闭，请稍后修改设置".into());
        }
        let before = shared.launcher_preferences.snapshot().preferences;
        let mut next = before.clone();
        patch.clone().apply(&mut next);
        next.validate()?;
        let network = prepare_network_change(&shared, &next)?;
        let downloads = prepare_download_change(&shared, &next)?;
        let view = shared.launcher_preferences.update(&revision, patch)?;
        if minecraft_notice_policy_changed(&before, &view.preferences) {
            shared.minecraft_updates.invalidate();
        }
        // The prepared factory cannot fail after persistence commits. Jobs keep
        // immutable client snapshots; admission excludes a new job until swap.
        if let Some(network) = network {
            pcl_network::install_snapshot(network);
            shared
                .launcher_local
                .warnings
                .set(crate::launcher_runtime::WarningKind::Network, None);
        }
        if let Some(downloads) = downloads {
            pcl_network::install_download_snapshot(downloads);
        }
        shared.launcher_local.apply_preferences(&view.preferences);
        Ok::<_, String>(view)
    })
    .await
    .map_err(|_| "保存启动器设置的任务意外退出".to_string())??;
    finish_window_effect(&state, &window, &mut view);
    Ok(view)
}

#[tauri::command]
pub async fn launcher_export_settings(
    revision: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<SettingsSaved>, String> {
    let shared = state.inner().clone();
    // Validate before opening the chooser, then validate again at publication.
    shared.launcher_preferences.export_settings(&revision)?;
    let choice = shared
        .desktop
        .choose_launcher_file(window, shared.project.clone(), "export_settings")
        .await?;
    if choice.status == "cancelled" {
        return Ok(None);
    }
    let path = choice.paths.first().cloned().ok_or_else(|| {
        choice
            .message
            .unwrap_or_else(|| "未选择有效的设置备份位置".into())
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::launcher_prefs::export_to_file(&shared.launcher_preferences, &revision, &path)?;
        Ok(Some(SettingsSaved {
            path: path.display().to_string(),
        }))
    })
    .await
    .map_err(|_| "导出设置的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn launcher_prepare_settings_import(
    revision: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<ImportPreview>, String> {
    let shared = state.inner().clone();
    shared.launcher_preferences.export_settings(&revision)?;
    let choice = shared
        .desktop
        .choose_launcher_file(window, shared.project.clone(), "import_settings")
        .await?;
    if choice.status == "cancelled" {
        return Ok(None);
    }
    let path: PathBuf = choice.paths.first().cloned().ok_or_else(|| {
        choice
            .message
            .unwrap_or_else(|| "未选择有效的设置备份文件".into())
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = crate::launcher_prefs::read_settings_file(&path)?;
        let preferences = crate::launcher_prefs::preview_import(&bytes)?;
        shared.launcher_preferences.export_settings(&revision)?;
        let token = format!(
            "{:x}",
            Sha256::digest(format!(
                "{}:{}:{revision}:{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                path.display()
            ))
        );
        *shared.launcher_import.0.lock().unwrap() = Some(PendingImport {
            token: token.clone(),
            revision: revision.clone(),
            bytes,
            prepared: Instant::now(),
        });
        Ok(Some(ImportPreview {
            token,
            revision,
            preferences,
        }))
    })
    .await
    .map_err(|_| "检查设置备份的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn launcher_apply_settings_import(
    token: String,
    revision: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<LauncherPreferencesView, String> {
    let shared = state.inner().clone();
    let mut view = tauri::async_runtime::spawn_blocking(move || {
        let mut session = shared.launcher_import.0.lock().unwrap();
        let pending = session.as_ref().ok_or("请重新选择设置备份")?;
        if pending.token != token
            || pending.revision != revision
            || pending.prepared.elapsed() > Duration::from_secs(600)
        {
            return Err("设置备份确认已失效，请重新选择".into());
        }
        let pending = session.take().unwrap();
        // Do not hold the import session mutex during the store's disk commit.
        drop(session);
        let _admission = shared.operations.lock().unwrap();
        if shared.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        let next = crate::launcher_prefs::preview_import(&pending.bytes)?;
        let network = prepare_network_change(&shared, &next)?;
        let downloads = prepare_download_change(&shared, &next)?;
        let view = shared
            .launcher_preferences
            .import_settings(&revision, &pending.bytes)?;
        // Import is a new user intent even when these two flags are unchanged.
        shared.minecraft_updates.invalidate();
        if let Some(network) = network {
            pcl_network::install_snapshot(network);
            shared
                .launcher_local
                .warnings
                .set(crate::launcher_runtime::WarningKind::Network, None);
        }
        if let Some(downloads) = downloads {
            pcl_network::install_download_snapshot(downloads);
        }
        shared.launcher_local.apply_preferences(&view.preferences);
        Ok::<_, String>(view)
    })
    .await
    .map_err(|_| "导入设置的任务意外退出".to_string())??;
    finish_window_effect(&state, &window, &mut view);
    Ok(view)
}
