//! Read-only discovery commands. Clipboard permission and keyboard focus are
//! checked again on GTK's thread when the asynchronous selection arrives.
//! Only approved resource links cross IPC; source clipboard text is discarded.
use crate::{
    launcher_prefs::AnnouncementMode,
    plugin_host::ServiceCall,
    product_plugins::contracts::clipboard::{ClipboardService, ModrinthLink, CLIPBOARD_DISCOVERY},
    product_plugins::contracts::{AnnouncementScope, AnnouncementView, ANNOUNCEMENTS},
    Shared,
};
use gtk::prelude::Cast;
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
    let Some(service) = state.product_plugins.optional_service(ANNOUNCEMENTS)? else {
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
    let Some(request) = clipboard_service(&shared)? else {
        return Ok(None);
    };
    let request = Arc::new(request);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let clipboard_window = window.clone();
    window
        .run_on_main_thread(move || {
            let result = (|| {
                let native = clipboard_window
                    .gtk_window()
                    .map_err(|_| "无法访问系统剪贴板窗口".to_string())?;
                let reader = request.clone();
                reader.service.0.request(
                    native.upcast_ref(),
                    Box::new(move || clipboard_enabled(&shared, &request)),
                    Box::new(move |outcome| {
                        let _ = sender.send(outcome.link);
                    }),
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

struct ClipboardRequest {
    service: ServiceCall<ClipboardService>,
    preferences_revision: String,
}
fn clipboard_service(shared: &Shared) -> Result<Option<ClipboardRequest>, String> {
    let preferences = shared.launcher_preferences.snapshot();
    if shared.closing.load(Ordering::SeqCst)
        || !preferences
            .preferences
            .management
            .clipboard_resource_detection
    {
        return Ok(None);
    }
    Ok(shared
        .product_plugins
        .optional_service(CLIPBOARD_DISCOVERY)?
        .map(|service| ClipboardRequest {
            service,
            preferences_revision: preferences.revision,
        }))
}
fn clipboard_enabled(shared: &Shared, request: &ClipboardRequest) -> bool {
    let preferences = shared.launcher_preferences.snapshot();
    request.service.is_active()
        && !shared.closing.load(Ordering::SeqCst)
        && preferences.revision == request.preferences_revision
        && preferences
            .preferences
            .management
            .clipboard_resource_detection
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;
    use serde_json::json;

    fn preference(shared: &Shared, enabled: bool) {
        let view = shared.launcher_preferences.snapshot();
        shared
            .launcher_preferences
            .update(
                &view.revision,
                serde_json::from_value(
                    json!({"management":{"clipboard_resource_detection":enabled}}),
                )
                .unwrap(),
            )
            .unwrap();
    }
    fn plugin(shared: &Shared, enabled: bool) {
        let view = shared
            .product_plugins
            .call("plugin_configuration", json!({}))
            .unwrap();
        shared.product_plugins.call("plugin_control", json!({
            "id":crate::plugins::clipboard::ID,"enabled":enabled,"expectedRevision":view["revision"]
        })).unwrap();
    }
    #[test]
    fn clipboard_opt_out_never_opens_invalid_plugin_configuration() {
        let fixture = crate::integration_tests::Fixture::new();
        let shared = fixture.shared();
        let root = shared.product_plugins.root();
        std::fs::create_dir_all(root).unwrap();
        let bytes = b"retained invalid plugin control";
        std::fs::write(root.join("workspace.json"), bytes).unwrap();
        assert!(clipboard_service(&shared).unwrap().is_none());
        preference(&shared, true);
        assert!(clipboard_service(&shared).is_err());
        assert_eq!(std::fs::read(root.join("workspace.json")).unwrap(), bytes);
        assert!(shared.code.get().is_none());
    }
    #[test]
    fn clipboard_retired_request_cannot_revive_on_plugin_reenable() {
        let fixture = crate::integration_tests::Fixture::new();
        let shared = fixture.shared();
        preference(&shared, true);
        let old = clipboard_service(&shared).unwrap().unwrap();
        assert!(clipboard_enabled(&shared, &old));
        plugin(&shared, false);
        assert!(!clipboard_enabled(&shared, &old));
        assert!(clipboard_service(&shared).unwrap().is_none());
        plugin(&shared, true);
        assert!(!clipboard_enabled(&shared, &old));
        assert!(clipboard_enabled(
            &shared,
            &clipboard_service(&shared).unwrap().unwrap()
        ));
        assert!(shared.code.get().is_none());
    }
    #[test]
    fn clipboard_preference_reenable_and_shutdown_retire_pending_read() {
        let fixture = crate::integration_tests::Fixture::new();
        let shared = fixture.shared();
        preference(&shared, true);
        let old = clipboard_service(&shared).unwrap().unwrap();
        preference(&shared, false);
        assert!(!clipboard_enabled(&shared, &old));
        preference(&shared, true);
        assert!(!clipboard_enabled(&shared, &old));
        let current = clipboard_service(&shared).unwrap().unwrap();
        assert!(clipboard_enabled(&shared, &current));
        shared.closing.store(true, Ordering::SeqCst);
        assert!(!clipboard_enabled(&shared, &current));
        assert!(clipboard_service(&shared).unwrap().is_none());
    }
}
