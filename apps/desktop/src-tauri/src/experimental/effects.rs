//! Native consumers for declarative extension effects. The host resolves
//! saved settings and grants; Rust repeats the narrow type/identity checks and
//! consumes the intent while that host session is exclusively owned. No intent
//! can supply a general preference patch, native command or clipboard read.
use super::Host;
use crate::{launcher_prefs::*, Shared};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{atomic::Ordering, mpsc, Arc};

const INVALID: &str = "插件操作返回了无效的原生作用请求";

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Intent {
    #[serde(rename = "set-launcher-appearance")]
    Appearance {
        #[serde(rename = "extensionId")]
        extension_id: String,
        values: Value,
    },
    #[serde(rename = "set-launcher-downloads")]
    Downloads {
        #[serde(rename = "extensionId")]
        extension_id: String,
        values: Value,
    },
    #[serde(rename = "copy-text")]
    CopyText {
        #[serde(rename = "extensionId")]
        extension_id: String,
        text: String,
    },
}

fn extension_id(value: &str) -> bool {
    let segments: Vec<_> = value.split('.').collect();
    value.len() <= 128
        && (2..=8).contains(&segments.len())
        && segments.iter().all(|segment| {
            segment
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_lowercase)
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn clipboard_text(value: &str) -> bool {
    value.chars().count() <= 4000
        && !value.chars().any(|character| {
            matches!(
                character,
                '\u{0000}'..='\u{0008}'
                    | '\u{000b}'..='\u{001f}'
                    | '\u{007f}'..='\u{009f}'
                    | '\u{061c}'
                    | '\u{200e}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        })
}

fn appearance_patch(values: Value) -> Result<LauncherPreferencesPatch, String> {
    let values = values.as_object().ok_or(INVALID)?;
    if !(1..=6).contains(&values.len()) {
        return Err(INVALID.into());
    }
    let mut patch = AppearancePatch::default();
    // Parse each present value as its concrete type: Option deserialization
    // would silently accept null, while the action contract requires a value.
    for (key, value) in values {
        let invalid = |_| INVALID.to_string();
        match key.as_str() {
            "theme" => patch.theme = Some(serde_json::from_value(value.clone()).map_err(invalid)?),
            "custom_cursor" => {
                patch.custom_cursor = Some(serde_json::from_value(value.clone()).map_err(invalid)?)
            }
            "cursor_style" => {
                patch.cursor_style = Some(serde_json::from_value(value.clone()).map_err(invalid)?)
            }
            "cursor_size" => {
                patch.cursor_size = Some(serde_json::from_value(value.clone()).map_err(invalid)?)
            }
            "custom_context_menu" => {
                patch.custom_context_menu =
                    Some(serde_json::from_value(value.clone()).map_err(invalid)?)
            }
            "context_menu_density" => {
                patch.context_menu_density =
                    Some(serde_json::from_value(value.clone()).map_err(invalid)?)
            }
            _ => return Err(INVALID.into()),
        }
    }
    Ok(LauncherPreferencesPatch {
        appearance: Some(patch),
        ..Default::default()
    })
}

fn downloads_patch(values: Value) -> Result<LauncherPreferencesPatch, String> {
    let values = values.as_object().ok_or(INVALID)?;
    if !(1..=2).contains(&values.len()) {
        return Err(INVALID.into());
    }
    let mut patch = ManagementPatch::default();
    for (key, value) in values {
        // Only concrete JSON integers cross the native boundary. Deserializing
        // an Option would accept null; accepting a binding would trust the host
        // to choose a different preference after its permission decision.
        let value = value.as_u64().ok_or(INVALID)?;
        match key.as_str() {
            "max_concurrent_transfers" if (1..=64).contains(&value) => {
                patch.max_concurrent_transfers = Some(value as u16)
            }
            "total_rate_limit_mib_per_second" if value <= 1024 => {
                patch.total_rate_limit_mib_per_second = Some(value as u32)
            }
            _ => return Err(INVALID.into()),
        }
    }
    Ok(LauncherPreferencesPatch {
        management: Some(patch),
        ..Default::default()
    })
}

/// Caller holds operations before host.session. The store verifies its existing
/// disk identity and performs the same durable CAS used by launcher settings.
/// Runtime publication follows the successful commit and never runs on failure.
fn apply_appearance(shared: &Shared, values: Value) -> Result<(), String> {
    let patch = appearance_patch(values)?;
    let current = shared.launcher_preferences.snapshot();
    if let Some(warning) = current.warning {
        return Err(warning);
    }
    let view = shared
        .launcher_preferences
        .update(&current.revision, patch)?;
    shared.launcher_local.apply_preferences(&view.preferences);
    // These six appearance fields are applied by the existing renderer hook.
    // The completion causes an independent reread; no queued full snapshot is
    // exposed here and none of these fields changes native window hints.
    Ok(())
}

/// Operations and the host session remain owned through durable publication.
/// Preparing the existing shared scheduler first enforces all native idle
/// guards; installing it only after CAS keeps failed saves on the old budget.
fn apply_downloads(shared: &Shared, values: Value) -> Result<(), String> {
    let patch = downloads_patch(values)?;
    let current = shared.launcher_preferences.snapshot();
    if let Some(warning) = current.warning {
        return Err(warning);
    }
    let mut next = current.preferences;
    patch.clone().apply(&mut next);
    next.validate()?;
    let downloads = crate::launcher_commands::prepare_download_change(shared, &next)?;
    let view = shared
        .launcher_preferences
        .update(&current.revision, patch)?;
    if let Some(downloads) = downloads {
        pcl_network::install_download_snapshot(downloads);
    }
    shared.launcher_local.apply_preferences(&view.preferences);
    Ok(())
}

pub(super) fn execute(
    host: &Host,
    shared: &Shared,
    args: Value,
    copy: impl FnOnce(String) -> Result<(), String>,
) -> Result<Value, String> {
    let expected_id = args["extensionId"].as_str().ok_or(INVALID)?.to_owned();
    if !extension_id(&expected_id) {
        return Err(INVALID.into());
    }
    host.call_with_effect(
        "extensions_action",
        args,
        Some(&shared.operations),
        |result, operation| {
            if shared.closing.load(Ordering::SeqCst) {
                return Err("启动器正在关闭".into());
            }
            if matches!(
                result["kind"].as_str(),
                Some("navigate" | "show-instance-summary")
            ) {
                return Ok(result);
            }
            let intent: Intent = serde_json::from_value(result).map_err(|_| INVALID)?;
            match intent {
                Intent::Appearance {
                    extension_id,
                    values,
                } => {
                    if extension_id != expected_id {
                        return Err(INVALID.into());
                    }
                    apply_appearance(shared, values)?;
                    Ok(json!({"kind":"launcher-appearance-applied","extensionId":extension_id}))
                }
                Intent::Downloads {
                    extension_id,
                    values,
                } => {
                    if extension_id != expected_id {
                        return Err(INVALID.into());
                    }
                    apply_downloads(shared, values)?;
                    Ok(json!({"kind":"launcher-downloads-applied","extensionId":extension_id}))
                }
                Intent::CopyText { extension_id, text } => {
                    if extension_id != expected_id || !clipboard_text(&text) {
                        return Err(INVALID.into());
                    }
                    // GTK/CloseRequested must stay free to take operations.
                    // Session ownership still prevents permission changes,
                    // settings edits and suspend until the clipboard completes.
                    drop(operation);
                    copy(text)?;
                    Ok(json!({"kind":"text-copied","extensionId":extension_id}))
                }
            }
        },
    )
}

/// Run only from a blocking worker. GTK's callback takes no application/session
/// locks; close uses the atomic fence, while native admission keeps the window
/// lifecycle alive until this callback has completed or failed.
pub(super) fn copy_on_main_thread(
    window: &tauri::WebviewWindow,
    shared: Arc<Shared>,
    text: String,
) -> Result<(), String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let result = (|| {
                if shared.closing.load(Ordering::SeqCst) {
                    return Err("启动器正在关闭".into());
                }
                let display = gdk::Display::default().ok_or("系统剪贴板不可用")?;
                let clipboard = gtk::Clipboard::default(&display).ok_or("系统剪贴板不可用")?;
                clipboard.set_text(&text);
                Ok(())
            })();
            let _ = sender.send(result);
        })
        .map_err(|_| "无法调度系统剪贴板请求".to_string())?;
    receiver
        .recv()
        .map_err(|_| "系统剪贴板请求未完成".to_string())?
}

#[cfg(test)]
#[path = "effect_tests.rs"]
mod effect_tests;
