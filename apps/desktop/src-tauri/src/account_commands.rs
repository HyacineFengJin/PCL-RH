//! Native account actions. Device-code assistance is authorized by the current
//! challenge on GTK's thread, including automatic effect replays/cancellation.
use crate::{account_appearance::AccountAppearance, Shared};
use serde::Serialize;
use std::sync::{atomic::Ordering, Arc};
use tauri::State;

#[tauri::command]
pub async fn auth_save_login(id: String, state: State<'_, Arc<Shared>>) -> Result<(), String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let session = crate::with_account_write(&shared, || shared.accounts.begin_save_login(&id))?;
        // A keyring can prompt for a password. Keep admission/state locks free
        // throughout this wait; the saving stage blocks conflicting account edits.
        shared.accounts.save_login(&id, session)
    })
    .await
    .map_err(|_| "保存登录任务意外退出".to_string())?
}
#[derive(Default, Serialize)]
pub struct DeviceAssist {
    pub attempted: bool,
    pub copied: bool,
    pub opened: bool,
}
fn copy(code: &str) -> Result<(), String> {
    let display = gdk::Display::default().ok_or("系统剪贴板不可用")?;
    let clipboard = gtk::Clipboard::default(&display).ok_or("系统剪贴板不可用")?;
    clipboard.set_text(code);
    Ok(())
}
#[tauri::command]
pub async fn auth_device_assist(
    code: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<DeviceAssist, String> {
    let shared = state.inner().clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let result = (|| {
                if shared.closing.load(Ordering::SeqCst) {
                    return Err("启动器正在关闭".into());
                }
                let Some(challenge) = shared.accounts.claim_device_assist(&code)? else {
                    return Ok(DeviceAssist::default());
                };
                // Copy first. Failure does not prevent opening the fixed login page.
                let copied = copy(&challenge.user_code).is_ok();
                let opened = crate::open_official("https://www.microsoft.com/link").is_ok();
                Ok(DeviceAssist {
                    attempted: true,
                    copied,
                    opened,
                })
            })();
            let _ = sender.send(result);
        })
        .map_err(|_| "无法请求浏览器或剪贴板".to_string())?;
    receiver
        .await
        .map_err(|_| "登录辅助请求未完成".to_string())?
}
#[tauri::command]
pub async fn auth_copy_code(
    code: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<(), String> {
    let shared = state.inner().clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let result = (|| {
                if shared.closing.load(Ordering::SeqCst) {
                    return Err("启动器正在关闭".into());
                }
                if shared
                    .accounts
                    .snapshot()
                    .challenge
                    .is_none_or(|c| c.user_code != code)
                {
                    return Err("设备代码已更新或登录已结束".into());
                }
                copy(&code)
            })();
            let _ = sender.send(result);
        })
        .map_err(|_| "无法请求系统剪贴板".to_string())?;
    receiver.await.map_err(|_| "复制请求未完成".to_string())?
}
#[tauri::command]
pub async fn auth_appearance(
    id: String,
    refresh: Option<bool>,
    state: State<'_, Arc<Shared>>,
) -> Result<AccountAppearance, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if shared.closing.load(Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        shared.accounts.appearance(&id, refresh.unwrap_or(false))
    })
    .await
    .map_err(|_| "皮肤获取任务意外退出".to_string())?
}
