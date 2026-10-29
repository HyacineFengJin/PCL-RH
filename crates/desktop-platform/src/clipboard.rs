//! Clipboard source text stays native. Discovery is opt-in and focus-gated at
//! admission and completion; timing out never permits a second pending read.
use serde::Serialize;
use tauri::WebviewWindow;

pub const TEXT_LIMIT: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipboardState {
    Disabled,
    NotFocused,
    Empty,
    Unsupported,
    Found,
    TimedOut,
    Unavailable,
    Busy,
}
pub enum ClipboardRead {
    State(ClipboardState),
    Text(String),
}
pub type ReadPermission = Box<dyn Fn() -> bool>;
pub type ReadCallback = Box<dyn FnOnce(ClipboardRead)>;

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod desktop;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod flight;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Called on the desktop event thread. Permissions are live callbacks, not a
/// captured boolean: disabling/re-enabling a plugin cannot revive an old read.
pub fn request_text(
    window: &WebviewWindow,
    permitted: ReadPermission,
    complete: ReadCallback,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::request_text(window, permitted, complete);
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    return desktop::request_text(window, permitted, complete);
}

/// Only authorized account assistance / extension effects call this. There is
/// no general clipboard read/write command exposed to community plugins.
pub fn write_text(window: &WebviewWindow, text: &str) -> Result<(), String> {
    if text.len() > 16 * 1024 || text.contains('\0') {
        return Err("剪贴板文本无效或过长".into());
    }
    #[cfg(target_os = "linux")]
    return linux::write_text(window, text);
    #[cfg(target_os = "windows")]
    return windows::write_text(window, text);
    #[cfg(target_os = "macos")]
    return macos::write_text(window, text);
}

fn bounded_text(text: String) -> ClipboardRead {
    if text.is_empty() {
        ClipboardRead::State(ClipboardState::Empty)
    } else if text.len() > TEXT_LIMIT {
        ClipboardRead::State(ClipboardState::Unsupported)
    } else {
        ClipboardRead::Text(text)
    }
}

#[cfg(any(target_os = "windows", test))]
fn unicode_text(units: &[u16]) -> ClipboardRead {
    let Some(end) = units
        .iter()
        .take(TEXT_LIMIT + 1)
        .position(|unit| *unit == 0)
    else {
        return ClipboardRead::State(ClipboardState::Unsupported);
    };
    match String::from_utf16(&units[..end]) {
        Ok(text) => bounded_text(text),
        Err(_) => ClipboardRead::State(ClipboardState::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_is_terminated_valid_and_bounded_by_utf8_bytes() {
        let text = "https://modrinth.com/mod/example";
        let units: Vec<_> = text.encode_utf16().chain([0, 42, 43]).collect();
        assert!(matches!(unicode_text(&units), ClipboardRead::Text(value) if value == text));
        for units in [
            vec![65; TEXT_LIMIT + 2],
            vec![0xd800, 0],
            "喵".repeat(2000).encode_utf16().chain([0]).collect(),
        ] {
            assert!(matches!(
                unicode_text(&units),
                ClipboardRead::State(ClipboardState::Unsupported)
            ));
        }
        assert!(matches!(
            unicode_text(&[0]),
            ClipboardRead::State(ClipboardState::Empty)
        ));
    }
}
