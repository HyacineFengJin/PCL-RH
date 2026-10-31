//! Update admission spans the whole future, including cancellation cleanup.
//! The native close handler can wait for this reservation without holding a
//! mutex across await. Applying an executable will additionally require idle
//! game/auth/task admission; a download merely owns its anonymous stage.
use crate::{launcher_updates::*, Shared};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tauri::State;

pub struct Updater {
    pub service: UpdateService,
    admitted: AtomicBool,
}
impl Updater {
    pub fn new(project: &Path, current: CurrentBuild) -> Result<Self, String> {
        Ok(Self {
            service: UpdateService::new(project, current)?,
            admitted: AtomicBool::new(false),
        })
    }
    pub fn busy(&self) -> bool {
        self.admitted.load(Ordering::Acquire) || self.service.is_busy()
    }
}
struct Permit(Arc<Shared>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0
            .launcher_updates
            .admitted
            .store(false, Ordering::Release);
    }
}
fn admit(s: Arc<Shared>) -> Result<Permit, String> {
    {
        let _operation = s.operations.lock().unwrap();
        if s.closing.load(Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        if s.launcher_updates
            .admitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("已有启动器更新操作正在进行".into());
        }
    }
    Ok(Permit(s))
}
pub fn current_build(project: &Path) -> CurrentBuild {
    let executable =
        std::env::current_exe().unwrap_or_else(|_| project.join(".pcl-rust/bin/pcl-desktop"));
    let install_channel = if executable == project.join(".pcl-rust/bin/pcl-desktop") {
        InstallChannel::Portable
    } else if executable.starts_with("/usr/bin") || executable.starts_with("/usr/lib") {
        InstallChannel::PackageManaged
    } else {
        InstallChannel::Unmanaged
    };
    CurrentBuild {
        version: env!("CARGO_PKG_VERSION").into(),
        commit: option_env!("PCL_BUILD_COMMIT")
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        executable: PathBuf::from(executable).display().to_string(),
        install_channel,
        architecture: Architecture::current(),
    }
}
#[tauri::command]
pub fn launcher_update_status(state: State<'_, Arc<Shared>>) -> UpdateView {
    state.launcher_updates.service.snapshot()
}
#[tauri::command]
pub async fn launcher_update_check(state: State<'_, Arc<Shared>>) -> Result<UpdateView, String> {
    let permit = admit(state.inner().clone())?;
    let channel = match permit
        .0
        .launcher_preferences
        .snapshot()
        .preferences
        .updates
        .channel
    {
        crate::launcher_prefs::UpdateChannel::Stable => UpdateChannel::Stable,
        crate::launcher_prefs::UpdateChannel::Beta => UpdateChannel::Beta,
    };
    permit.0.launcher_updates.service.check(channel).await
}
#[tauri::command]
pub async fn launcher_update_download(
    token: String,
    state: State<'_, Arc<Shared>>,
) -> Result<UpdateView, String> {
    let permit = admit(state.inner().clone())?;
    permit.0.launcher_updates.service.download(&token).await
}
#[tauri::command]
pub fn launcher_update_cancel(state: State<'_, Arc<Shared>>) -> bool {
    state.launcher_updates.service.cancel()
}
#[tauri::command]
pub async fn launcher_update_discard(state: State<'_, Arc<Shared>>) -> Result<UpdateView, String> {
    let permit = admit(state.inner().clone())?;
    tauri::async_runtime::spawn_blocking(move || permit.0.launcher_updates.service.discard_staged())
        .await
        .map_err(|_| "更新暂存清理任务意外退出，请核对保留证据".to_string())?
}

enum InstallationAction {
    Apply,
    Rollback,
    Acknowledge,
    Recover,
}
async fn installation(
    action: InstallationAction,
    token: String,
    shared: Arc<Shared>,
) -> Result<UpdateView, String> {
    let permit = admit(shared)?;
    let shared = permit.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit; // The worker owns admission even if its IPC future disappears.
        let _operation = shared.operations.lock().unwrap();
        crate::launcher_monitor_runtime::require_idle(&shared)?;
        if shared.tasks.active().is_some()
            || matches!(
                shared.status.lock().unwrap().stage.as_str(),
                "preparing" | "running"
            )
            || shared.accounts.busy()
        {
            return Err("请在文件任务、游戏和微软登录结束后操作启动器安装文件".into());
        }
        match action {
            InstallationAction::Apply => shared.launcher_updates.service.apply(&token),
            InstallationAction::Rollback => shared.launcher_updates.service.rollback(&token),
            InstallationAction::Acknowledge => {
                shared.launcher_updates.service.acknowledge_update(&token)
            }
            InstallationAction::Recover => shared.launcher_updates.service.recover(),
        }
    })
    .await
    .map_err(|_| "启动器安装任务意外退出，请在更新页面检查恢复状态".to_string())?
}
#[tauri::command]
pub async fn launcher_update_apply(
    token: String,
    state: State<'_, Arc<Shared>>,
) -> Result<UpdateView, String> {
    installation(InstallationAction::Apply, token, state.inner().clone()).await
}
#[tauri::command]
pub async fn launcher_update_rollback(
    token: String,
    state: State<'_, Arc<Shared>>,
) -> Result<UpdateView, String> {
    installation(InstallationAction::Rollback, token, state.inner().clone()).await
}
#[tauri::command]
pub async fn launcher_update_acknowledge(
    token: String,
    state: State<'_, Arc<Shared>>,
) -> Result<UpdateView, String> {
    installation(
        InstallationAction::Acknowledge,
        token,
        state.inner().clone(),
    )
    .await
}
#[tauri::command]
pub async fn launcher_update_recover(state: State<'_, Arc<Shared>>) -> Result<UpdateView, String> {
    installation(
        InstallationAction::Recover,
        String::new(),
        state.inner().clone(),
    )
    .await
}
