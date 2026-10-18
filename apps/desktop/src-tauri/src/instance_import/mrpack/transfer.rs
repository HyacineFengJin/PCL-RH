//! Cancellable mirror chains and verified streaming into caller-owned FDs.
//!
//! One captured network factory and download scheduler serve the submission.
//! A permit spans all headers/bodies of one file, including mirror failover.
//! There are no detached HTTP tasks: cancelling/dropping the future drops its
//! response, queued permit wait and owned anonymous descriptor before returning.
//! Partial bytes never gain a name; successful descriptors are rewound and
//! passed to the publication service, which retains transaction ownership.
use super::{mirror, Result};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::{
    collections::BTreeMap,
    fs::File,
    future::Future,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const CANCELLED: &str = "实例导入已取消";
const HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) struct Remote<'a> {
    pub downloads: &'a [String],
    pub size: u64,
    pub sha1: &'a str,
    pub sha512: &'a str,
}
pub(super) struct VerifiedFile {
    pub file: File,
    pub size: u64,
    pub sha1: String,
    pub sha512: String,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct TransferProgress {
    /// Submission-local, cumulative bytes actually returned by body reads.
    /// Failed mirrors and rejected overlong chunks count; disk copies do not.
    pub network_bytes: u64,
}
pub(super) struct Client {
    http: reqwest::Client,
    downloads: Arc<pcl_network::DownloadScheduler>,
    network_bytes: AtomicU64,
    // Serialize updates and callbacks across concurrent files so callbacks
    // observe cumulative byte values in the same order as the counter updates.
    telemetry: Mutex<()>,
    local_mirrors: bool,
    #[cfg(test)]
    route: Option<reqwest::Url>,
}
#[derive(Clone, Copy)]
enum Source {
    Modrinth,
    CurseForge,
}

enum Failure {
    Cancelled,
    Mirror(String),
    Local(String),
}
type Attempt<T> = std::result::Result<T, Failure>;

impl Client {
    pub(super) fn new(
        factory: &pcl_network::ClientFactory,
        downloads: Arc<pcl_network::DownloadScheduler>,
    ) -> Result<Self> {
        let http = factory
            .async_client()
            .user_agent("PCL-RH/0.2.0 (https://github.com/HyacineFengJin/PCL-RH)")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "无法创建 mrpack 下载客户端")?;
        Ok(Self {
            http,
            downloads,
            network_bytes: AtomicU64::new(0),
            telemetry: Mutex::new(()),
            local_mirrors: false,
            #[cfg(test)]
            route: None,
        })
    }
    pub(super) fn with_local_mirrors(mut self, local: bool) -> Self {
        self.local_mirrors = local;
        self
    }
    pub(super) fn network_bytes(&self) -> u64 {
        self.network_bytes.load(Ordering::Relaxed)
    }
    fn received(&self, bytes: u64, report: &impl Fn(TransferProgress)) {
        let _guard = self.telemetry.lock().unwrap_or_else(|p| p.into_inner());
        let network_bytes = self.network_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        report(TransferProgress { network_bytes });
    }
    fn request_url(&self, logical: &reqwest::Url) -> reqwest::Url {
        #[cfg(test)]
        if let Some(route) = &self.route {
            let mut route = route.clone();
            route.set_path(logical.path());
            route.set_query(logical.query());
            return route;
        }
        logical.clone()
    }
    /// `destination` must be a fresh, empty anonymous regular file with no
    /// aliases retained by the caller. Failure consumes/closes it; mirror retry
    /// truncates this same owned inode. Only final effective remotes belong here.
    pub(super) async fn download(
        &self,
        remote: Remote<'_>,
        destination: File,
        cancel: &AtomicBool,
        report: impl Fn(TransferProgress),
    ) -> Result<VerifiedFile> {
        self.download_with(remote, destination, cancel, report, Source::Modrinth)
            .await
    }

    pub(super) async fn download_curseforge(
        &self,
        file: &super::curseforge::ResolvedFile,
        destination: File,
        cancel: &AtomicBool,
        report: impl Fn(TransferProgress),
    ) -> Result<VerifiedFile> {
        let downloads = [file.url.clone()];
        self.download_with(
            Remote {
                downloads: &downloads,
                size: file.size,
                sha1: &file.sha1,
                sha512: "",
            },
            destination,
            cancel,
            report,
            Source::CurseForge,
        )
        .await
    }

    async fn download_with(
        &self,
        remote: Remote<'_>,
        mut destination: File,
        cancel: &AtomicBool,
        report: impl Fn(TransferProgress),
        source: Source,
    ) -> Result<VerifiedFile> {
        check(cancel).map_err(failure_text)?;
        if remote.size > MAX_FILE_BYTES || remote.downloads.len() > mirror::MAX_MIRRORS {
            return Err("mrpack 单文件大小或镜像数量超过限制".into());
        }
        let hashes = match source {
            Source::Modrinth => vec![(remote.sha1, 40), (remote.sha512, 128)],
            Source::CurseForge => vec![(remote.sha1, 40)],
        };
        for (hash, length) in hashes {
            if hash.len() != length || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(match source {
                    Source::Modrinth => "mrpack 文件必须提供有效 SHA1 与 SHA512",
                    Source::CurseForge => "CurseForge 文件必须提供有效官方 SHA1",
                }
                .into());
            }
        }
        let metadata = destination
            .metadata()
            .map_err(|_| "无法检查 mrpack 匿名暂存文件")?;
        if !metadata.is_file() || metadata.nlink() != 0 || metadata.len() != 0 {
            return Err("mrpack 下载目标必须是空的匿名普通文件".into());
        }
        let candidates = remote
            .downloads
            .iter()
            .map(|value| match source {
                Source::Modrinth if self.local_mirrors => mirror::local_declaration(value),
                Source::Modrinth => mirror::declaration(value),
                Source::CurseForge => crate::community_curseforge::download_url(value).map(Some),
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err("mrpack 文件没有受支持的 HTTPS 下载镜像".into());
        }
        // Shared rate pacing consumes wall time. The whole file, including
        // admission and every mirror, has one bounded deadline; retry never
        // renews the budget. Headers and idle body reads retain short limits.
        let seconds = if self.downloads.policy().total_rate_limit_mib_per_second > 0 {
            24 * 60 * 60
        } else {
            30 * 60
        };
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let _permit = wait(cancel, deadline, self.downloads.acquire(cancel))
            .await
            .map_err(failure_text)?
            .map_err(|_| {
                failure_text(if cancel.load(Ordering::Acquire) {
                    Failure::Cancelled
                } else {
                    Failure::Local("mrpack 下载调度器无法分配连接".into())
                })
            })?;
        let mut failures = BTreeMap::<String, usize>::new();
        let count = candidates.len();
        for candidate in candidates {
            check(cancel).map_err(failure_text)?;
            reset(&mut destination).map_err(failure_text)?;
            match self
                .attempt(
                    candidate,
                    &remote,
                    &mut destination,
                    cancel,
                    deadline,
                    &report,
                    source,
                )
                .await
            {
                Ok((sha1, sha512)) => {
                    destination
                        .sync_all()
                        .map_err(|_| "mrpack 匿名暂存同步失败")?;
                    destination
                        .seek(SeekFrom::Start(0))
                        .map_err(|_| "mrpack 匿名暂存定位失败")?;
                    check(cancel).map_err(failure_text)?;
                    return Ok(VerifiedFile {
                        file: destination,
                        size: remote.size,
                        sha1,
                        sha512,
                    });
                }
                Err(Failure::Mirror(reason)) => {
                    // Preserve an already observed failure if cancellation
                    // arrives afterward, but never start another mirror.
                    if cancel.load(Ordering::Acquire) {
                        return Err(reason);
                    }
                    *failures.entry(reason).or_default() += 1;
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(error) => return Err(failure_text(error)),
            }
        }
        let attempted = failures.values().sum::<usize>();
        let reasons = failures
            .into_iter()
            .map(|(reason, count)| format!("{reason}（{count} 次）"))
            .collect::<Vec<_>>()
            .join("；");
        Err(format!(
            "mrpack 镜像下载失败，已尝试 {attempted}/{count}：{reasons}"
        ))
    }

    async fn attempt(
        &self,
        candidate: reqwest::Url,
        remote: &Remote<'_>,
        destination: &mut File,
        cancel: &AtomicBool,
        deadline: Instant,
        report: &impl Fn(TransferProgress),
        source: Source,
    ) -> Attempt<(String, String)> {
        let mut chain = if self.local_mirrors {
            mirror::Chain::local(candidate)
        } else {
            mirror::Chain::new(candidate)
        };
        let mut response = loop {
            // Initial declarations and every Location have already passed the
            // same authority checks; reqwest never chooses a redirect itself.
            let response = wait(
                cancel,
                deadline.min(Instant::now() + HEADERS_TIMEOUT),
                self.http
                    .get(self.request_url(chain.current()))
                    .header(reqwest::header::ACCEPT_ENCODING, "identity")
                    .send(),
            )
            .await?
            .map_err(|_| Failure::Mirror("mrpack 镜像请求失败".into()))?;
            if response.status().is_redirection() && matches!(source, Source::CurseForge) {
                return Err(Failure::Mirror(
                    "CurseForge 官方文件返回重定向，已停止下载".into(),
                ));
            }
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .ok_or_else(|| Failure::Mirror("mrpack 镜像重定向缺少 Location".into()))?
                    .to_str()
                    .map_err(|_| Failure::Mirror("mrpack 镜像重定向 Location 无效".into()))?
                    .to_owned();
                // Do not drain redirect bodies. Unread bytes cannot affect the
                // payload counter; dropping the response closes this request.
                drop(response);
                chain.follow(&location).map_err(Failure::Mirror)?;
                continue;
            }
            if !response.status().is_success() {
                return Err(Failure::Mirror(format!(
                    "mrpack 镜像 HTTP {}",
                    response.status().as_u16()
                )));
            }
            break response;
        };
        if response
            .content_length()
            .is_some_and(|length| length != remote.size)
        {
            return Err(Failure::Mirror(
                "mrpack 镜像 Content-Length 与声明不符".into(),
            ));
        }
        let mut bytes = 0u64;
        let mut sha1 = Sha1::new();
        let mut sha512 = Sha512::new();
        while let Some(chunk) = wait(
            cancel,
            deadline.min(Instant::now() + STALL_TIMEOUT),
            response.chunk(),
        )
        .await?
        .map_err(|_| Failure::Mirror("mrpack 镜像响应读取失败".into()))?
        {
            self.received(chunk.len() as u64, report);
            let next = bytes
                .checked_add(chunk.len() as u64)
                .filter(|next| *next <= remote.size)
                .ok_or_else(|| Failure::Mirror("mrpack 镜像实际大小超过声明".into()))?;
            for slice in chunk.chunks(pcl_network::DOWNLOAD_SLICE_BYTES) {
                wait(
                    cancel,
                    deadline,
                    self.downloads.throttle(slice.len() as u64, cancel),
                )
                .await?
                .map_err(|_| {
                    if cancel.load(Ordering::Acquire) {
                        Failure::Cancelled
                    } else {
                        Failure::Local("mrpack 下载限速器失败".into())
                    }
                })?;
                check(cancel)?;
                destination
                    .write_all(slice)
                    .map_err(|_| Failure::Local("mrpack 匿名暂存写入失败".into()))?;
                sha1.update(slice);
                sha512.update(slice);
            }
            bytes = next;
        }
        drop(response);
        if bytes != remote.size {
            return Err(Failure::Mirror("mrpack 镜像实际大小与声明不符".into()));
        }
        let sha1 = format!("{:x}", sha1.finalize());
        let sha512 = format!("{:x}", sha512.finalize());
        if !sha1.eq_ignore_ascii_case(remote.sha1)
            || (matches!(source, Source::Modrinth) && !sha512.eq_ignore_ascii_case(remote.sha512))
        {
            return Err(Failure::Mirror(
                match source {
                    Source::Modrinth => "mrpack 镜像 SHA1 或 SHA512 校验失败",
                    Source::CurseForge => "CurseForge 官方文件 SHA1 校验失败",
                }
                .into(),
            ));
        }
        check(cancel)?;
        Ok((sha1, sha512))
    }
}

fn reset(file: &mut File) -> Attempt<()> {
    file.set_len(0)
        .and_then(|_| file.seek(SeekFrom::Start(0)).map(|_| ()))
        .map_err(|_| Failure::Local("mrpack 匿名暂存重置失败".into()))
}
fn check(cancel: &AtomicBool) -> Attempt<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Failure::Cancelled)
    } else {
        Ok(())
    }
}
fn failure_text(failure: Failure) -> String {
    match failure {
        Failure::Cancelled => CANCELLED.into(),
        Failure::Mirror(reason) | Failure::Local(reason) => reason,
    }
}
async fn wait<F: Future>(cancel: &AtomicBool, deadline: Instant, future: F) -> Attempt<F::Output> {
    check(cancel)?;
    tokio::select! {
        biased;
        _ = async {
            loop {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        } => Err(Failure::Cancelled),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) =>
            Err(Failure::Mirror("mrpack 下载超过时间限制".into())),
        result = future => Ok(result),
    }
}

#[cfg(test)]
#[path = "transfer/tests.rs"]
mod tests;
