//! Read-only discovery commands. Clipboard permission and keyboard focus are
//! checked again on GTK's thread when the asynchronous selection arrives.
//! Only approved resource links cross IPC; source clipboard text is discarded.
use crate::{launcher_discovery::*, launcher_prefs::AnnouncementMode, Shared};
use std::sync::{atomic::Ordering, Arc};
use tauri::State;

#[tauri::command]
pub async fn launcher_announcements(
    refresh: Option<bool>,
    state: State<'_, Arc<Shared>>,
) -> Result<AnnouncementView, String> {
    if state.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    let scope = match state
        .launcher_preferences
        .snapshot()
        .preferences
        .announcement_mode
    {
        AnnouncementMode::All => AnnouncementScope::All,
        AnnouncementMode::Important => AnnouncementScope::Important,
        AnnouncementMode::None => AnnouncementScope::None,
    };
    if scope == AnnouncementScope::None {
        return Ok(AnnouncementView::disabled());
    }
    let Some(service) = state.product_plugins.optional_native_service(
        crate::plugins::home::ID,
        crate::plugins::home::ANNOUNCEMENTS,
    )?
    else {
        return Ok(AnnouncementView::disabled());
    };
    Ok(service.0.read(scope, refresh.unwrap_or(false)).await)
}

#[tauri::command]
pub async fn launcher_clipboard_link(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<ModrinthLink>, String> {
    let shared = state.inner().clone();
    if shared.closing.load(Ordering::SeqCst)
        || !shared
            .launcher_preferences
            .snapshot()
            .preferences
            .management
            .clipboard_resource_detection
    {
        return Ok(None);
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let clipboard_window = window.clone();
    window
        .run_on_main_thread(move || {
            let result = (|| {
                let native = clipboard_window
                    .gtk_window()
                    .map_err(|_| "无法访问系统剪贴板窗口".to_string())?;
                request_clipboard_link(
                    &native,
                    move || {
                        !shared.closing.load(Ordering::SeqCst)
                            && shared
                                .launcher_preferences
                                .snapshot()
                                .preferences
                                .management
                                .clipboard_resource_detection
                    },
                    move |outcome| {
                        let _ = sender.send(outcome.link);
                    },
                )
            })();
            // Dropping the callback on an API failure closes the oneshot. Neither
            // failure nor timeout causes a shell fallback or background polling.
            if result.is_err() {
                return;
            }
        })
        .map_err(|_| "无法调度系统剪贴板请求".to_string())?;
    receiver
        .await
        .map_err(|_| "系统剪贴板服务暂时不可用".to_string())
}
