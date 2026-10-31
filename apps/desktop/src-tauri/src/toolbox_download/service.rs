//! Toolbox URL downloads bind a real chooser's directory and one confirmed
//! URL/basename. Prepared authority is session-only and consumed on admission;
//! workers retain immutable network/download policies and owned staging capabilities.
//! Cancellation stops before the task commit gate. Publication never overwrites,
//! and a post-publication sync warning cannot turn success into cancellation.
#[path = "authority.rs"]
pub(super) mod authority;
#[path = "files.rs"]
pub(super) mod files;
#[path = "model.rs"]
mod model;
#[path = "transfer.rs"]
pub(super) mod transfer;
pub use authority::{DownloadSession, PreparedDownload};
pub use model::{
    CommitGate, DirectoryView, DownloadPreview, DownloadProgress, DownloadResult, PrepareRequest,
};
use pcl_network::{ClientFactory, DownloadScheduler};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
pub(super) type Result<T> = std::result::Result<T, String>;
pub const CANCELLED: &str = "工具箱下载已取消";
pub(super) const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
fn normalize_cancel(error: String) -> String {
    if error == "下载已取消" {
        CANCELLED.into()
    } else {
        error
    }
}
/// The client factory and scheduler are captured by the command at admission.
/// No global network state is consulted after submission.
pub async fn download(
    prepared: PreparedDownload,
    network: Arc<ClientFactory>,
    scheduler: Arc<DownloadScheduler>,
    cancel: &AtomicBool,
    report: impl Fn(DownloadProgress),
    commit_gate: CommitGate<'_>,
) -> Result<DownloadResult> {
    let client = network
        .async_client()
        .user_agent(concat!("PCL-RH/", env!("CARGO_PKG_VERSION"), " Toolbox"))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(24 * 60 * 60))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .map_err(|_| "无法创建工具箱下载连接")?;
    download_with_client(
        prepared,
        &client,
        &scheduler,
        cancel,
        report,
        commit_gate,
        transfer::TransferLimits::default(),
    )
    .await
}
pub(super) async fn download_with_client(
    prepared: PreparedDownload,
    client: &reqwest::Client,
    scheduler: &DownloadScheduler,
    cancel: &AtomicBool,
    report: impl Fn(DownloadProgress),
    commit_gate: CommitGate<'_>,
    limits: transfer::TransferLimits,
) -> Result<DownloadResult> {
    cancelled(cancel)?;
    let directory = &prepared.directory.directory;
    let name = &prepared.preview.file_name;
    directory.recheck()?;
    directory.absent(name)?;
    let received = transfer::receive(
        client,
        scheduler,
        prepared.url,
        directory.anonymous()?,
        cancel,
        &report,
        limits,
    )
    .await?;
    let verified = files::verify_anonymous(received.file, received.size, &received.sha256, cancel)?;
    directory.recheck()?;
    directory.absent(name)?;
    cancelled(cancel)?;
    report(DownloadProgress {
        phase: "committing".into(),
        message: "实际文件已校验，准备保存".into(),
        bytes_done: received.size,
        bytes_total: received.size,
        network_bytes: received.size,
    });
    commit_gate()?;
    cancelled(cancel)?;
    let warning = directory.publish(name, &verified)?;
    report(DownloadProgress {
        phase: "complete".into(),
        message: "工具箱文件已保存".into(),
        bytes_done: received.size,
        bytes_total: received.size,
        network_bytes: received.size,
    });
    Ok(DownloadResult {
        path: directory.path().join(name).display().to_string(),
        file_name: name.clone(),
        size: received.size,
        sha256: received.sha256,
        network_bytes: received.size,
        warning,
    })
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod native_tests;
