//! Every request captures the same immutable networking policy for its lifetime.
//! No GitHub token, cookie jar or redirects are used. Response text is bounded
//! here before parsing; errors carry closed codes and cannot echo provider text.
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
pub(crate) const MAX_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_RELEASES: usize = 30;
pub(crate) const API: &str =
    "https://api.github.com/repos/HyacineFengJin/PCL-RH/releases?per_page=30&page=1";
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[derive(Clone, Copy)]
pub(crate) enum Failure {
    Network,
    TooLarge,
    Protocol,
}
#[derive(Default)]
pub(crate) struct Headers {
    pub remaining: Option<u64>,
    pub reset_at: Option<u64>,
    pub retry_seconds: Option<u64>,
    pub has_next: bool,
}
pub(crate) struct Response {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}
pub(crate) trait Provider: Send + Sync {
    fn fetch(&self) -> BoxFuture<'_, Result<Response, Failure>>;
}
pub(crate) struct GitHub {
    network: Arc<pcl_network::ClientFactory>,
}
impl GitHub {
    pub fn new(network: Arc<pcl_network::ClientFactory>) -> Self {
        Self { network }
    }
}
impl Provider for GitHub {
    fn fetch(&self) -> BoxFuture<'_, Result<Response, Failure>> {
        Box::pin(async move {
            let client = self
                .network
                .async_client()
                .user_agent("PCL-RH-announcements/0.2.0")
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(8))
                .timeout(Duration::from_secs(20))
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .build()
                .map_err(|_| Failure::Network)?;
            let mut response = client
                .get(API)
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2026-03-10")
                .send()
                .await
                .map_err(|_| Failure::Network)?;
            let number = |key: &str| {
                response
                    .headers()
                    .get(key)
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| value.len() <= 32)
                    .and_then(|value| value.parse::<u64>().ok())
            };
            let headers = Headers {
                remaining: number("x-ratelimit-remaining"),
                reset_at: number("x-ratelimit-reset"),
                retry_seconds: number("retry-after"),
                has_next: response
                    .headers()
                    .get("link")
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| value.len() <= 8192)
                    .is_some_and(|value| value.contains("rel=\"next\"")),
            };
            let status = response.status().as_u16();
            if response
                .content_length()
                .is_some_and(|length| length > MAX_BYTES as u64)
            {
                return Err(Failure::TooLarge);
            }
            if (300..400).contains(&status) {
                return Err(Failure::Protocol);
            }
            let mut body = Vec::new();
            while let Some(bytes) = response.chunk().await.map_err(|_| Failure::Network)? {
                if body.len().saturating_add(bytes.len()) > MAX_BYTES {
                    return Err(Failure::TooLarge);
                }
                body.extend_from_slice(&bytes);
            }
            Ok(Response {
                status,
                headers,
                body,
            })
        })
    }
}
