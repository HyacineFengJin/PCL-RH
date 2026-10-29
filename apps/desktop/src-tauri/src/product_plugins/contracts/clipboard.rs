//! The trusted discovery service returns approved resource identities only.
//! GTK interaction remains on the desktop thread; callbacks are deliberately
//! not Send, while the provider itself can be resolved by the native host.
use crate::plugin_host::Contract;
use serde::Serialize;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModrinthLink {
    pub kind: String,
    pub project_id_or_slug: String,
    pub url: String,
}
#[derive(Clone, Copy, Debug, Serialize)]
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
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardOutcome {
    pub state: ClipboardState,
    pub link: Option<ModrinthLink>,
}
impl ClipboardOutcome {
    pub(crate) fn state(state: ClipboardState) -> Self {
        Self { state, link: None }
    }
}
pub trait ClipboardDiscovery: Send + Sync {
    fn request(
        &self,
        window: &gtk::Window,
        enabled: Box<dyn Fn() -> bool>,
        complete: Box<dyn FnOnce(ClipboardOutcome)>,
    ) -> Result<(), String>;
}
pub struct ClipboardService(pub Arc<dyn ClipboardDiscovery>);
pub const CLIPBOARD_DISCOVERY: Contract<ClipboardService> =
    Contract::new("launcher.clipboard-discovery", 1);
