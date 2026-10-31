//! Toolbox URL download service. The worker is also compiled by the native
//! installer wrapper; the older desktop task fixtures remain Linux-specific.
#[path = "toolbox_download/service.rs"]
mod service;
// Keep the existing service types at their command-facing path.
#[allow(unused_imports)]
pub use service::{
    download, CommitGate, DirectoryView, DownloadPreview, DownloadProgress, DownloadResult,
    DownloadSession, PrepareRequest, PreparedDownload, CANCELLED,
};

#[cfg(all(test, target_os = "linux"))]
use pcl_network::{ClientFactory, DownloadScheduler};
#[cfg(all(test, target_os = "linux"))]
use service::{authority, download_with_client, files, transfer, Result};
#[cfg(all(test, target_os = "linux"))]
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
#[cfg(all(test, target_os = "linux"))]
#[path = "toolbox_download/tests.rs"]
mod tests;
