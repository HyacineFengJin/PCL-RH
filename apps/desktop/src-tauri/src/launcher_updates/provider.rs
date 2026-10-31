//! Read-only GitHub boundary. Every initial request is constructed from this
//! repository and numeric asset IDs obtained from its own release response.
//! Asset redirects are handled explicitly; arbitrary release URLs are ignored.
use reqwest::{header, Client, Response, Url};
use serde::Deserialize;
use std::{future::Future, pin::Pin, time::Duration};

pub(super) const REPOSITORY: &str = "HyacineFengJin/PCL-RH";
pub(super) const MANIFEST_NAME: &str = "pcl-rh-update.json";
pub(super) const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
const API_ROOT: &str = "https://api.github.com/repos/HyacineFengJin/PCL-RH";
const MAX_API_BYTES: usize = 2 * 1024 * 1024;
pub(super) const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub(super) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Release {
    pub id: u64,
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct Asset {
    pub id: u64,
    pub name: String,
    pub state: String,
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Comparison {
    pub status: String,
    pub ahead_by: u64,
    pub behind_by: u64,
}

pub(super) trait AssetStream: Send {
    fn next_chunk(&mut self) -> BoxFuture<'_, Option<Vec<u8>>>;
}
pub(super) trait ReleaseProvider: Send + Sync {
    fn releases(&self) -> BoxFuture<'_, Vec<Release>>;
    fn tag_commit<'a>(&'a self, tag: &'a str) -> BoxFuture<'a, String>;
    fn compare<'a>(&'a self, base: &'a str, head: &'a str) -> BoxFuture<'a, Comparison>;
    fn asset(&self, id: u64, prefix_only: bool) -> BoxFuture<'_, Box<dyn AssetStream>>;
}

pub(super) struct GitHubProvider {
    client: Client,
}
impl GitHubProvider {
    /// Each operation captures one immutable network snapshot. Proxy and DoH
    /// edits cannot change the route halfway through a checked download.
    pub(super) fn new() -> Result<Self, String> {
        let client = pcl_network::snapshot()
            .async_client()
            .user_agent("PCL-RH-launcher-updates")
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .build()
            .map_err(|_| "无法创建启动器更新网络客户端".to_string())?;
        Ok(Self { client })
    }
    async fn api(&self, suffix: &str) -> Result<Response, String> {
        let response = self
            .client
            .get(format!("{API_ROOT}/{suffix}"))
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| "GitHub 更新检查网络请求失败（未记录请求地址或代理信息）".to_string())?;
        check_status(&response)?;
        Ok(response)
    }
    async fn json<T: serde::de::DeserializeOwned>(&self, suffix: &str) -> Result<T, String> {
        let bytes = bounded_body(self.api(suffix).await?, MAX_API_BYTES).await?;
        serde_json::from_slice(&bytes).map_err(|_| "GitHub 更新元数据格式无效".into())
    }
}
impl ReleaseProvider for GitHubProvider {
    fn releases(&self) -> BoxFuture<'_, Vec<Release>> {
        Box::pin(async move {
            let mut releases = Vec::new();
            // The bounded window must not silently yield a false `latest` if
            // GitHub advertises additional pages beyond our limit.
            for page in 1..=3 {
                let response = self
                    .api(&format!("releases?per_page=100&page={page}"))
                    .await?;
                let more = response
                    .headers()
                    .get(header::LINK)
                    .and_then(|h| h.to_str().ok())
                    .is_some_and(|h| h.contains("rel=\"next\""));
                let bytes = bounded_body(response, MAX_API_BYTES).await?;
                let list: Vec<Release> = serde_json::from_slice(&bytes)
                    .map_err(|_| "GitHub 发布列表格式无效".to_string())?;
                if list.len() > 100 {
                    return Err("GitHub 发布列表超过数量限制".into());
                }
                releases.extend(list);
                if !more {
                    return Ok(releases);
                }
            }
            Err("GitHub 发布数量超过本次检查上限，请缩小发布历史后重试".into())
        })
    }
    fn tag_commit<'a>(&'a self, tag: &'a str) -> BoxFuture<'a, String> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Commit {
                sha: String,
            }
            // The service only passes a parsed semver tag with this restricted
            // alphabet, so no URL path/query can be supplied by release metadata.
            if !safe_tag(tag) {
                return Err("发布标签不是受支持的版本标签".into());
            }
            let commit: Commit = self.json(&format!("commits/{tag}")).await?;
            if !hex(&commit.sha, 40) {
                return Err("GitHub 标签提交标识无效".into());
            }
            Ok(commit.sha.to_ascii_lowercase())
        })
    }
    fn compare<'a>(&'a self, base: &'a str, head: &'a str) -> BoxFuture<'a, Comparison> {
        Box::pin(async move {
            if !hex(base, 40) || !hex(head, 40) {
                return Err("版本比较缺少完整提交标识".into());
            }
            self.json(&format!("compare/{base}...{head}?per_page=1"))
                .await
        })
    }
    fn asset(&self, id: u64, prefix_only: bool) -> BoxFuture<'_, Box<dyn AssetStream>> {
        Box::pin(async move {
            if id == 0 {
                return Err("GitHub 发布资产标识无效".into());
            }
            let mut url = Url::parse(&format!("{API_ROOT}/releases/assets/{id}"))
                .map_err(|_| "无法构造更新资产请求".to_string())?;
            for step in 0..=3 {
                let mut request = self
                    .client
                    .get(url.clone())
                    .header(header::ACCEPT, "application/octet-stream")
                    .header("X-GitHub-Api-Version", "2022-11-28");
                if prefix_only {
                    request = request.header(
                        header::RANGE,
                        if cfg!(target_os = "linux") {
                            "bytes=0-63"
                        } else {
                            "bytes=0-65535"
                        },
                    );
                }
                let response = request.send().await.map_err(|_| {
                    "GitHub 更新资产请求失败（未记录下载地址或代理信息）".to_string()
                })?;
                if response.status().is_redirection() {
                    if step == 3 {
                        return Err("GitHub 更新资产重定向过多".into());
                    }
                    let location = response
                        .headers()
                        .get(header::LOCATION)
                        .and_then(|h| h.to_str().ok())
                        .ok_or("GitHub 更新资产重定向缺少地址")?;
                    let next = url.join(location).map_err(|_| "更新资产重定向地址无效")?;
                    if !trusted_asset_redirect(&next) {
                        return Err("GitHub 更新资产跳转到未受信任的地址，已拒绝下载".into());
                    }
                    url = next;
                    continue;
                }
                check_status(&response)?;
                if response.status().as_u16() != 200
                    && !(prefix_only && response.status().as_u16() == 206)
                {
                    return Err("GitHub 更新资产响应状态无效".into());
                }
                return Ok(Box::new(HttpStream(response)) as Box<dyn AssetStream>);
            }
            Err("GitHub 更新资产下载失败".into())
        })
    }
}

struct HttpStream(Response);
impl AssetStream for HttpStream {
    fn next_chunk(&mut self) -> BoxFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            self.0
                .chunk()
                .await
                .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
                .map_err(|_| "更新资产接收中断（未记录下载地址）".into())
        })
    }
}

async fn bounded_body(mut response: Response, max: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|size| size > max as u64)
    {
        return Err("GitHub 更新元数据超过大小限制".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "GitHub 更新元数据接收中断".to_string())?
    {
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err("GitHub 更新元数据超过大小限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn check_status(response: &Response) -> Result<(), String> {
    match response.status().as_u16() {
        200 | 206 => Ok(()),
        403 | 429 => Err("GitHub 拒绝更新请求，可能达到公共 API 限额，请稍后重试".into()),
        404 => Err("GitHub 未找到本项目的发布元数据或资产".into()),
        code => Err(format!("GitHub 更新请求返回 HTTP {code}")),
    }
}
pub(super) fn trusted_asset_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.port().is_none_or(|port| port == 443)
        && matches!(
            url.host_str(),
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
        )
}
pub(super) fn hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(super) fn safe_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}
