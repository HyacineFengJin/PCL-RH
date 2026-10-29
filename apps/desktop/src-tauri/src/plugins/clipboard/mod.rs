//! Optional native discovery plugin. The launcher command owns admission and
//! live user preferences; this provider owns resource link recognition.
pub(crate) mod service;
use crate::{plugin_host::Registration, product_plugins::contracts::clipboard::*};
use std::sync::Arc;
pub const ID: &str = "launcher.discovery.clipboard";
struct Clipboard;
impl ClipboardDiscovery for Clipboard {
    fn request(
        &self,
        window: &tauri::WebviewWindow,
        enabled: Box<dyn Fn() -> bool>,
        complete: Box<dyn FnOnce(ClipboardOutcome)>,
    ) -> Result<(), String> {
        service::request_clipboard_link(window, enabled, complete)
    }
}
pub fn start(registration: &mut Registration<'_>) -> Result<(), String> {
    registration.provide(CLIPBOARD_DISCOVERY, ClipboardService(Arc::new(Clipboard)))
}
