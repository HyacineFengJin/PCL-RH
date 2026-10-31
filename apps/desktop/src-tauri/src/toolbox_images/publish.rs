//! Bounded memory exports for images, resource lists and crash diagnostics.
//! Linux preserves anonymous staging; other hosts use an owned native lease.
use serde::Serialize;
use std::path::Path;
#[cfg(target_os = "linux")]
#[path = "publish/linux.rs"]
mod backend;
#[cfg(not(target_os = "linux"))]
#[path = "publish/native.rs"]
mod backend;
#[cfg(all(test, target_os = "linux"))]
#[path = "publish/native.rs"]
mod portable;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    pub status: String,
    pub path: Option<String>,
    pub count: usize,
    pub warning: Option<String>,
}
impl ExportOutcome {
    pub fn cancelled() -> Self {
        Self {
            status: "cancelled".into(),
            path: None,
            count: 0,
            warning: None,
        }
    }
    pub fn unavailable(message: &str) -> Self {
        Self {
            status: "unavailable".into(),
            path: None,
            count: 0,
            warning: Some(message.into()),
        }
    }
}

pub fn publish_new<G>(
    path: &Path,
    bytes: &[u8],
    extension: &str,
    count: usize,
    authorize: impl FnOnce() -> Result<G, String>,
) -> Result<ExportOutcome, String> {
    backend::publish_new(path, bytes, extension, count, authorize)
}

#[cfg(test)]
#[path = "publish/tests.rs"]
mod tests;
