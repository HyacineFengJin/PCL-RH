//! Read-only launcher discovery. Announcements come only from this project's
//! public GitHub Releases. Clipboard inspection is a native, focused-window,
//! opt-in action and returns only an approved resource link, never source text.
#[cfg(test)]
#[path = "launcher_discovery/announcements.rs"]
mod announcements;
#[path = "launcher_discovery/clipboard.rs"]
mod clipboard;
#[path = "launcher_discovery/provider.rs"]
#[cfg(test)]
mod provider;
pub use clipboard::{request_clipboard_link, ModrinthLink};

#[cfg(test)]
#[path = "launcher_discovery/tests.rs"]
mod tests;
