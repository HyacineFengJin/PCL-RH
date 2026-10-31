//! User-authorized HTTP bytes have no expected integrity authority. Manual
//! redirects validate every hop without automatic authentication/referrer carry;
//! the final digest describes actual bytes, then owned-FD readback verifies disk.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    io::Write,
    time::{Duration, Instant},
};
#[derive(Clone, Copy)]
pub(crate) struct TransferLimits {
    pub maximum_bytes: u64,
    pub maximum_redirects: usize,
    pub header_timeout: Duration,
    pub read_timeout: Duration,
    pub total_timeout: Duration,
}
impl Default for TransferLimits {
    fn default() -> Self {
        Self {
            maximum_bytes: MAX_FILE_BYTES,
            maximum_redirects: 5,
            header_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(60),
            total_timeout: Duration::from_secs(24 * 60 * 60),
        }
    }
}
pub(crate) struct Received {
    pub file: files::Staging,
    pub size: u64,
    pub sha256: String,
}
async fn cancellable<F: Future>(
    cancel: &AtomicBool,
    deadline: Instant,
    future: F,
) -> Result<F::Output> {
    tokio::select! {
        biased;
        _=async { loop { if cancel.load(Ordering::Acquire) {break} tokio::time::sleep(Duration::from_millis(40)).await; } }=>Err(CANCELLED.into()),
        _=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))=>Err("工具箱下载超过时间限制".into()),
        result=future=>Ok(result),
    }
}
fn emit(
    report: &impl Fn(DownloadProgress),
    phase: &str,
    message: &str,
    done: u64,
    total: u64,
    network: u64,
) {
    report(DownloadProgress {
        phase: phase.into(),
        message: message.into(),
        bytes_done: done,
        bytes_total: total,
        network_bytes: network,
    });
}
async fn response(
    client: &reqwest::Client,
    mut url: reqwest::Url,
    cancel: &AtomicBool,
    deadline: Instant,
    limits: TransferLimits,
) -> Result<reqwest::Response> {
    for redirect in 0..=limits.maximum_redirects {
        let response = cancellable(
            cancel,
            deadline.min(Instant::now() + limits.header_timeout),
            client
                .get(url.clone())
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .send(),
        )
        .await?
        .map_err(|_| "工具箱下载请求失败，请检查网络、地址和代理设置")?;
        if response.status().is_redirection() {
            if redirect == limits.maximum_redirects {
                return Err("下载重定向超过5次，未继续请求".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .filter(|value| value.len() <= 8192)
                .ok_or("下载重定向地址缺失或无效")?;
            url = redirect_url(&url, location)?;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!("工具箱下载HTTP {}", response.status().as_u16()));
        }
        // A plain GET did not authorize ranges or a decoded representation.
        // These headers cannot turn a partial/encoded body into a full file.
        if response.status() == reqwest::StatusCode::PARTIAL_CONTENT
            || response
                .headers()
                .contains_key(reqwest::header::CONTENT_RANGE)
        {
            return Err("下载服务器返回未请求的部分内容，文件未保存".into());
        }
        if response
            .headers()
            .get_all(reqwest::header::CONTENT_ENCODING)
            .iter()
            .any(|value| {
                !value
                    .to_str()
                    .is_ok_and(|value| value.trim().eq_ignore_ascii_case("identity"))
            })
        {
            return Err("下载服务器返回未请求的编码内容，文件未保存".into());
        }
        return Ok(response);
    }
    Err("下载重定向次数超出范围".into())
}
pub(crate) async fn receive(
    client: &reqwest::Client,
    scheduler: &DownloadScheduler,
    url: reqwest::Url,
    mut file: files::Staging,
    cancel: &AtomicBool,
    report: &impl Fn(DownloadProgress),
    limits: TransferLimits,
) -> Result<Received> {
    let deadline = Instant::now() + limits.total_timeout;
    emit(report, "connecting", "正在连接下载地址", 0, 0, 0);
    let _permit = cancellable(cancel, deadline, scheduler.acquire(cancel))
        .await?
        .map_err(normalize_cancel)?;
    let mut response = response(client, url, cancel, deadline, limits).await?;
    let declared = response.content_length();
    if declared.is_some_and(|size| size > limits.maximum_bytes) {
        return Err("下载文件声明大小超过2GiB限制，未保存文件".into());
    }
    let total = declared.unwrap_or(0);
    let mut bytes = 0u64;
    let mut network = 0u64;
    let mut digest = Sha256::new();
    emit(report, "downloading", "正在下载文件", 0, total, 0);
    loop {
        let chunk = cancellable(
            cancel,
            deadline.min(Instant::now() + limits.read_timeout),
            response.chunk(),
        )
        .await?
        .map_err(|_| "工具箱下载响应未完整读取，暂存文件已丢弃")?;
        let Some(chunk) = chunk else { break };
        network = network
            .checked_add(chunk.len() as u64)
            .ok_or("下载字节计数超出范围")?;
        let next = bytes
            .checked_add(chunk.len() as u64)
            .ok_or("下载文件长度超出范围")?;
        if next > limits.maximum_bytes || declared.is_some_and(|size| next > size) {
            emit(
                report,
                "downloading",
                "响应大小超过限制，暂存文件已丢弃",
                bytes,
                total,
                network,
            );
            return Err("下载文件实际大小超过限制或响应声明，暂存文件已丢弃".into());
        }
        for slice in chunk.chunks(pcl_network::DOWNLOAD_SLICE_BYTES) {
            let pacing = cancellable(
                cancel,
                deadline,
                scheduler.throttle(slice.len() as u64, cancel),
            )
            .await?;
            pacing.map_err(normalize_cancel)?;
            cancelled(cancel)?;
            file.write_all(slice)
                .map_err(|_| "无法写入所选目录的下载暂存文件")?;
            digest.update(slice);
            bytes = bytes
                .checked_add(slice.len() as u64)
                .ok_or("下载字节计数超出范围")?;
            emit(report, "downloading", "正在下载文件", bytes, total, network);
        }
    }
    cancelled(cancel)?;
    if declared.is_some_and(|size| size != bytes) {
        return Err("下载响应未达到声明大小，暂存文件已丢弃".into());
    }
    emit(
        report,
        "verifying",
        "正在复核实际下载文件",
        bytes,
        bytes,
        network,
    );
    files::staging_file(&mut file)
        .sync_all()
        .map_err(|_| "无法同步所选目录的下载暂存文件")?;
    Ok(Received {
        file,
        size: bytes,
        sha256: format!("{:x}", digest.finalize()),
    })
}

pub(crate) fn redirect_url(current: &reqwest::Url, location: &str) -> Result<reqwest::Url> {
    if location.len() > 8192 || location.chars().any(char::is_control) {
        return Err("下载重定向地址无效或过长".into());
    }
    let next = current
        .join(location)
        .map_err(|_| "下载重定向地址格式无效")?;
    let next = authority::validate_url(next.as_str())?;
    if current.scheme() == "https" && next.scheme() == "http" {
        return Err("HTTPS下载重定向到HTTP，未发送该请求".into());
    }
    Ok(next)
}
