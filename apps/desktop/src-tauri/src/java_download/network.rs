//! A Java worker owns one immutable network policy and scheduler snapshot.
//! Official identities are checked before each request; redirects never widen
//! that authority. Raw runtime files stream into anonymous descriptors.
use super::{check, Result};
use pcl_network::DownloadScheduler;
use sha1::{Digest, Sha1};
use std::{
    fs::File,
    future::Future,
    io::Write,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

pub(crate) struct Http {
    client: reqwest::Client,
    scheduler: Arc<DownloadScheduler>,
    #[cfg(test)]
    pub fixture: Option<String>,
}
impl Http {
    pub fn new(
        factory: Arc<pcl_network::ClientFactory>,
        scheduler: Arc<DownloadScheduler>,
    ) -> Result<Self> {
        Ok(Self {
            client: factory
                .async_client()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .user_agent("PCL-RH/0.2")
                .build()
                .map_err(|e| format!("无法创建 Java 下载连接：{e}"))?,
            scheduler,
            #[cfg(test)]
            fixture: None,
        })
    }
    async fn response(&self, url: &str, cancel: &AtomicBool) -> Result<reqwest::Response> {
        let parsed = reqwest::Url::parse(url).map_err(|_| "Java 官方下载地址无效")?;
        if parsed.scheme() != "https"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.port().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !matches!(
                parsed.host_str(),
                Some("piston-meta.mojang.com" | "piston-data.mojang.com")
            )
        {
            return Err("Java 下载只接受 Mojang 官方地址".into());
        }
        #[cfg(test)]
        let url = self
            .fixture
            .as_ref()
            .map(|origin| format!("{origin}{}", parsed.path()))
            .unwrap_or_else(|| url.into());
        let response = wait(
            cancel,
            Instant::now() + Duration::from_secs(30),
            self.client
                .get(url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .send(),
        )
        .await?
        .map_err(|e| format!("Java 下载连接失败：{e}"))?;
        if !response.status().is_success()
            || response.status() == reqwest::StatusCode::PARTIAL_CONTENT
            || response
                .headers()
                .contains_key(reqwest::header::CONTENT_RANGE)
            || response
                .headers()
                .get_all(reqwest::header::CONTENT_ENCODING)
                .iter()
                .any(|v| !v.to_str().is_ok_and(|s| s.eq_ignore_ascii_case("identity")))
        {
            return Err(format!(
                "Java 下载响应无效：HTTP {}",
                response.status().as_u16()
            ));
        }
        Ok(response)
    }
    pub async fn metadata(
        &self,
        url: &str,
        expected: Option<(&str, u64)>,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>> {
        let mut response = self.response(url, cancel).await?;
        let maximum = expected
            .map(|e| e.1)
            .unwrap_or(super::catalog::MAX_METADATA);
        if maximum == 0
            || maximum > super::catalog::MAX_METADATA
            || response
                .content_length()
                .is_some_and(|n| n > maximum || expected.is_some_and(|(_, size)| n != size))
        {
            return Err("Java 元数据大小无效".into());
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut body = Vec::new();
        while let Some(chunk) = wait(
            cancel,
            deadline.min(Instant::now() + Duration::from_secs(30)),
            response.chunk(),
        )
        .await?
        .map_err(|e| e.to_string())?
        {
            if body.len() as u64 + chunk.len() as u64 > maximum {
                return Err("Java 元数据超过限制".into());
            }
            body.extend_from_slice(&chunk);
        }
        if let Some((hash, size)) = expected {
            if body.len() as u64 != size || format!("{:x}", Sha1::digest(&body)) != hash {
                return Err("Java 清单大小或 SHA-1 校验失败".into());
            }
        }
        Ok(body)
    }
    pub async fn file(
        &self,
        source: &super::catalog::Download,
        target: &mut File,
        cancel: &AtomicBool,
        guard: &impl Fn() -> Result<()>,
        mut progress: impl FnMut(u64),
    ) -> Result<()> {
        let deadline = Instant::now()
            + Duration::from_secs(
                if self.scheduler.policy().total_rate_limit_mib_per_second > 0 {
                    86400
                } else {
                    1800
                },
            );
        guard()?;
        let _permit = wait(cancel, deadline, self.scheduler.acquire(cancel))
            .await?
            .map_err(cancel_error)?;
        guard()?;
        let mut response = self.response(&source.url, cancel).await?;
        if response.content_length().is_some_and(|n| n != source.size) {
            return Err("Java 文件声明大小不匹配".into());
        }
        let mut received = 0u64;
        let mut digest = Sha1::new();
        while let Some(chunk) = wait(
            cancel,
            deadline.min(Instant::now() + Duration::from_secs(30)),
            response.chunk(),
        )
        .await?
        .map_err(|e| format!("Java 文件下载中断：{e}"))?
        {
            if received + chunk.len() as u64 > source.size {
                return Err("Java 文件实际大小超过清单".into());
            }
            for bytes in chunk.chunks(pcl_network::DOWNLOAD_SLICE_BYTES) {
                wait(
                    cancel,
                    deadline,
                    self.scheduler.throttle(bytes.len() as u64, cancel),
                )
                .await?
                .map_err(cancel_error)?;
                check(cancel)?;
                guard()?;
                target
                    .write_all(bytes)
                    .map_err(|e| format!("无法写入 Java 暂存文件：{e}"))?;
                digest.update(bytes);
                received += bytes.len() as u64;
                progress(received);
            }
        }
        if received != source.size || format!("{:x}", digest.finalize()) != source.sha1 {
            return Err("Java 文件大小或 SHA-1 校验失败".into());
        }
        target.sync_all().map_err(|e| e.to_string())
    }
}
fn cancel_error(error: String) -> String {
    if error == "下载已取消" {
        super::CANCELLED.into()
    } else {
        error
    }
}
pub(super) async fn wait<T>(
    cancel: &AtomicBool,
    deadline: Instant,
    future: impl Future<Output = T>,
) -> Result<T> {
    tokio::pin!(future);
    loop {
        check(cancel)?;
        if Instant::now() >= deadline {
            return Err("Java 下载等待超时".into());
        }
        tokio::select! { value=&mut future => return Ok(value), _=tokio::time::sleep(Duration::from_millis(40))=>{} }
    }
}
