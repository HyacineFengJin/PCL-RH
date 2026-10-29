//! Compatibility test paths for discovery. Active announcement and clipboard
//! implementations belong to the corresponding bundled plugins.
#[cfg(test)]
#[path = "launcher_discovery/announcements.rs"]
mod announcements;
#[cfg(test)]
#[path = "launcher_discovery/clipboard.rs"]
mod clipboard;
#[path = "launcher_discovery/provider.rs"]
#[cfg(test)]
mod provider;

#[cfg(test)]
#[path = "launcher_discovery/tests.rs"]
mod tests;
