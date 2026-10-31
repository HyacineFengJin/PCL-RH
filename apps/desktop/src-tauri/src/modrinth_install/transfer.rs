//! Download writers retain their real generated-source ownership through batch
//! publication. Linux keeps anonymous FDs; native cancellation records durable
//! discard, while unknown/unconfirmed evidence is preserved and reported.
use super::{provider::HttpProvider, target::Dir, *};
use std::{
    collections::VecDeque,
    future::Future,
    io::{Seek, SeekFrom, Write},
    pin::Pin,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

type QueuedTransfer<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;
/// Poll only the configured number of borrowed transfers. No detached task can
/// outlive this owner: an error drops every in-flight response, anonymous file
/// and already-verified result before returning to the publisher.
pub(super) async fn collect_bounded<'a, T>(
    pending: VecDeque<QueuedTransfer<'a, T>>,
    limit: usize,
) -> Result<Vec<T>> {
    let mut ready = Vec::new();
    pcl_network::for_each_bounded(pending, limit, |value| {
        ready.push(value);
        Ok(())
    })
    .await?;
    Ok(ready)
}

#[derive(Clone, Debug, Serialize)]
pub struct DownloadProgress {
    pub phase: String,
    pub message: String,
    pub completed: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub network_bytes: u64,
}
/// Downloaded files only. Reused resources stay in place and are covered by the
/// plan's target snapshot; they never enter an import/enable mutation.
pub struct VerifiedFile {
    pub kind: String,
    pub file_name: String,
    pub size: u64,
    pub sha512: String,
    pub file: source::Input,
}
pub struct VerifiedBatch {
    pub plan: InstallPlan,
    pub files: Vec<VerifiedFile>,
    pub network_bytes: u64,
}
fn progress(
    provider: &HttpProvider<'_>,
    plan: &InstallPlan,
    report: &impl Fn(DownloadProgress),
    phase: &str,
    message: String,
    completed: u64,
    bytes_done: u64,
) {
    report(DownloadProgress {
        phase: phase.into(),
        message,
        completed,
        total: plan.files.len() as u64,
        bytes_done,
        bytes_total: plan.download_bytes,
        network_bytes: provider.network_bytes.load(Ordering::Relaxed),
    });
}
pub(crate) async fn response_into_file(
    provider: &HttpProvider<'_>,
    request: reqwest::RequestBuilder,
    expected_size: u64,
    expected_sha512: &str,
    destination: &mut File,
    cancel: &AtomicBool,
    received: impl FnMut(u64),
) -> Result<()> {
    response_into_file_digest(
        provider,
        request,
        expected_size,
        crate::resource_digest::ExpectedDigest::Sha512(expected_sha512),
        destination,
        cancel,
        received,
    )
    .await
}
pub(crate) async fn response_into_file_digest(
    provider: &HttpProvider<'_>,
    request: reqwest::RequestBuilder,
    expected_size: u64,
    expected_digest: crate::resource_digest::ExpectedDigest<'_>,
    destination: &mut File,
    cancel: &AtomicBool,
    mut received: impl FnMut(u64),
) -> Result<()> {
    expected_digest.validate()?;
    if expected_size == 0 || expected_size > MAX_FILE_BYTES {
        return Err("资源文件大小无效或超过限制".into());
    }
    // Payload pacing consumes wall time, so limited transfers receive a larger
    // bounded total deadline. DNS/headers and stalled reads keep short limits.
    let duration = if provider.downloads.policy().total_rate_limit_mib_per_second > 0 {
        24 * 60 * 60
    } else {
        30 * 60
    };
    let deadline = Instant::now() + Duration::from_secs(duration);
    let _permit = provider
        .downloads
        .acquire(cancel)
        .await
        .map_err(|error| transfer_error(cancel, error))?;
    let mut response = provider::cancellable(
        cancel,
        Instant::now() + Duration::from_secs(30),
        request.send(),
    )
    .await?
    .map_err(|_| "资源文件请求失败")?;
    if response.status().is_redirection() {
        return Err("资源文件返回了重定向，未继续请求".into());
    }
    if !response.status().is_success() {
        return Err(format!("资源文件HTTP {}", response.status().as_u16()));
    }
    if response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .is_some_and(|encoding| encoding.as_bytes() != b"identity")
    {
        return Err("资源文件返回了未请求的压缩编码".into());
    }
    if response
        .content_length()
        .is_some_and(|size| size != expected_size)
    {
        return Err("资源响应大小与官方文件数据不符".into());
    }
    let mut bytes = 0u64;
    let mut digest = expected_digest.hasher();
    while let Some(chunk) = provider::cancellable(
        cancel,
        deadline.min(Instant::now() + Duration::from_secs(60)),
        response.chunk(),
    )
    .await?
    .map_err(|_| "资源文件读取失败")?
    {
        // Count received bytes even when an overlong body is rejected. Cached
        // files and disk copies never contribute to this network counter.
        provider
            .network_bytes
            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        let next = bytes
            .checked_add(chunk.len() as u64)
            .ok_or("资源文件长度超出范围")?;
        if next > expected_size {
            return Err("资源文件实际大小超过官方声明，暂存内容已丢弃".into());
        }
        for slice in chunk.chunks(pcl_network::DOWNLOAD_SLICE_BYTES) {
            provider::cancellable(
                cancel,
                deadline,
                provider.downloads.throttle(slice.len() as u64, cancel),
            )
            .await?
            .map_err(|error| transfer_error(cancel, error))?;
            target::cancelled(cancel)?;
            destination
                .write_all(slice)
                .map_err(|e| format!("资源匿名暂存写入失败：{e}"))?;
            digest.update(slice);
            received(slice.len() as u64);
        }
        bytes = next;
    }
    target::cancelled(cancel)?;
    if bytes != expected_size {
        return Err("资源文件大小校验失败，暂存内容已丢弃".into());
    }
    digest.verify(&expected_digest)?;
    destination.sync_all().map_err(|e| e.to_string())?;
    destination
        .seek(SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn transfer_error(cancel: &AtomicBool, error: String) -> String {
    if cancel.load(Ordering::Acquire) {
        CANCELLED.into()
    } else {
        error
    }
}
pub(super) async fn download(
    provider: &HttpProvider<'_>,
    plan: InstallPlan,
    cancel: &AtomicBool,
    report: impl Fn(DownloadProgress),
) -> Result<VerifiedBatch> {
    target::cancelled(cancel)?;
    target::check(&plan.target, cancel)?;
    let project = Dir::open(&plan.target.project)?;
    if project.key() != Ok(plan.target.project_key.clone()) {
        return Err("应用目录已被替换，资源目标未改动".into());
    }
    let staging = project
        .ensure(".pcl-rust")?
        .ensure("resource-downloads")?
        .with_input_sources()?;
    let stage_key = staging.key()?;
    let mut completed = 0u64;
    for file in &plan.files {
        if file.reused {
            completed += 1;
            progress(
                provider,
                &plan,
                &report,
                "reuse",
                format!("复用相同内容：{}", file.file_name),
                completed,
                0,
            );
        }
    }
    let counters = std::sync::Mutex::new((completed, 0u64));
    progress(
        provider,
        &plan,
        &report,
        "checking",
        "官方文件与必需依赖已核对".into(),
        completed,
        0,
    );
    let jobs = plan
        .files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.reused)
        .map(|(index, file)| {
            let counters = &counters;
            let plan = &plan;
            let report = &report;
            let staging = &staging;
            let project = &project;
            let stage_key = &stage_key;
            Box::pin(async move {
                target::cancelled(cancel)?;
                let url = provider::cdn_url(&file.url)?;
                let mut destination = staging.allocate(file.size)?;
                response_into_file(
                    provider,
                    provider
                        .client
                        .get(url)
                        .header(reqwest::header::ACCEPT_ENCODING, "identity"),
                    file.size,
                    &file.sha512,
                    &mut destination,
                    cancel,
                    |amount| {
                        let mut counters = counters.lock().unwrap();
                        counters.1 = counters.1.saturating_add(amount);
                        progress(
                            provider,
                            plan,
                            report,
                            "downloading",
                            format!("下载{}", file.file_name),
                            counters.0,
                            counters.1,
                        );
                    },
                )
                .await?;
                if Dir::open(&plan.target.project)?.key() != Ok(plan.target.project_key.clone())
                    || project
                        .child(".pcl-rust")?
                        .child("resource-downloads")?
                        .key()
                        != Ok(stage_key.clone())
                {
                    return Err("匿名资源暂存目录已被替换，暂存内容已丢弃".into());
                }
                let mut counters = counters.lock().unwrap();
                counters.0 += 1;
                progress(
                    provider,
                    plan,
                    report,
                    "verified",
                    format!("校验完成：{}", file.file_name),
                    counters.0,
                    counters.1,
                );
                Ok((
                    index,
                    VerifiedFile {
                        kind: file.kind.clone(),
                        file_name: file.file_name.clone(),
                        size: file.size,
                        sha512: file.sha512.clone(),
                        file: destination,
                    },
                ))
            }) as QueuedTransfer<'_, (usize, VerifiedFile)>
        })
        .collect();
    let collected = collect_bounded(
        jobs,
        usize::from(provider.downloads.policy().max_concurrent_transfers),
    )
    .await;
    let mut ordered = match collected {
        Ok(ordered) => ordered,
        Err(error) => {
            return match staging.check_cleanup() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}；来源退役未确认：{cleanup}")),
            }
        }
    };
    ordered.sort_by_key(|(index, _)| *index);
    let files = ordered.into_iter().map(|(_, file)| file).collect();
    let (completed, bytes_done) = counters.into_inner().map_err(|_| "资源下载进度锁异常")?;
    if let Err(error) = target::check(&plan.target, cancel) {
        drop(files);
        return match staging.check_cleanup() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}；来源退役未确认：{cleanup}")),
        };
    }
    progress(
        provider,
        &plan,
        &report,
        "ready",
        "全部资源文件已下载并校验，等待原子导入".into(),
        completed,
        bytes_done,
    );
    Ok(VerifiedBatch {
        plan,
        files,
        network_bytes: provider.network_bytes.load(Ordering::Relaxed),
    })
}
