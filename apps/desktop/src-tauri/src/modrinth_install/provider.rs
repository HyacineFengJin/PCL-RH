//! Official provider parsing and cancellable HTTP. URL policy is checked before
//! a request; redirects are disabled. API data is bounded before deserialization.
use super::*;
use serde::de::DeserializeOwned;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const API: &str = "https://api.modrinth.com/v2/";
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const MAX_HTTP_REQUESTS: u64 = 256;
const MAX_METADATA_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) type FutureResult<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub(crate) fn id(value: &str) -> Result<()> {
    if value.len() != 8 || !value.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("Modrinth项目或版本ID无效".into());
    }
    Ok(())
}
#[path = "provider/identity.rs"]
mod identity;
pub(crate) use identity::file_name;
pub(crate) fn cdn_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).map_err(|_| "Modrinth文件链接无效")?;
    if value.len() > 4096
        || url.scheme() != "https"
        || url.host_str() != Some("cdn.modrinth.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return Err("资源文件必须来自Modrinth官方HTTPS CDN".into());
    }
    Ok(url)
}
pub(crate) fn sha512(value: &str) -> Result<()> {
    if value.len() != 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Modrinth文件缺少有效SHA512校验值".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Project {
    pub id: String,
    pub title: String,
    pub project_type: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Hashes {
    pub sha512: String,
    #[serde(default)]
    pub sha1: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ApiFile {
    pub filename: String,
    pub size: u64,
    pub url: String,
    pub primary: bool,
    pub hashes: Hashes,
    #[serde(default)]
    pub file_type: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Dependency {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    pub dependency_type: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Version {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub date_published: String,
    pub version_type: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub files: Vec<ApiFile>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}
pub(crate) fn validate_version(version: &Version) -> Result<()> {
    id(&version.id)?;
    id(&version.project_id)?;
    if version.name.len() > 2048
        || version.version_number.len() > 2048
        || version.date_published.len() > 64
        || version.files.is_empty()
        || version.files.len() > 32
        || version.dependencies.len() > 128
        || version.game_versions.len() > 4096
        || version.loaders.len() > 64
        || !matches!(version.version_type.as_str(), "release" | "beta" | "alpha")
    {
        return Err("Modrinth版本数据无效或超过限制".into());
    }
    let mut names = std::collections::BTreeSet::new();
    let mut primary = 0;
    for file in &version.files {
        file_name(&file.filename)?;
        sha512(&file.hashes.sha512)?;
        cdn_url(&file.url)?;
        if !names.insert(&file.filename) || file.size == 0 || file.size > MAX_FILE_BYTES {
            return Err("Modrinth文件重复、为空或超过大小限制".into());
        }
        if file.primary {
            primary += 1
        }
        if file
            .hashes
            .sha1
            .as_ref()
            .is_some_and(|s| s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("Modrinth文件SHA1格式无效".into());
        }
    }
    if primary > 1 {
        return Err("Modrinth版本声明了多个主文件".into());
    }
    for dependency in &version.dependencies {
        if let Some(value) = &dependency.project_id {
            id(value)?
        }
        if let Some(value) = &dependency.version_id {
            id(value)?
        }
        if let Some(value) = &dependency.file_name {
            file_name(value)?
        }
        if !matches!(
            dependency.dependency_type.as_str(),
            "required" | "optional" | "incompatible" | "embedded"
        ) {
            return Err("Modrinth依赖类型不受支持".into());
        }
    }
    Ok(())
}

pub(crate) trait Provider: Sync {
    fn project<'a>(&'a self, id: &'a str) -> FutureResult<'a, Project>;
    fn version<'a>(&'a self, id: &'a str) -> FutureResult<'a, Version>;
    fn versions<'a>(
        &'a self,
        project: &'a str,
        compatibility: &'a Compatibility,
    ) -> FutureResult<'a, Vec<Version>>;
    fn from_hashes<'a>(
        &'a self,
        hashes: &'a [String],
    ) -> FutureResult<'a, BTreeMap<String, Version>>;
    /// Defaults keep small fixture providers useful; HTTP overrides these with
    /// the official bulk endpoints, so check never performs one GET per mod.
    fn projects<'a>(&'a self, ids: &'a [String]) -> FutureResult<'a, Vec<Project>> {
        Box::pin(async move {
            let mut result = Vec::new();
            for id in ids {
                result.push(self.project(id).await?)
            }
            Ok(result)
        })
    }
    fn updates<'a>(
        &'a self,
        hashes: &'a [String],
        compatibility: &'a Compatibility,
    ) -> FutureResult<'a, BTreeMap<String, Version>> {
        Box::pin(async move {
            let old = self.from_hashes(hashes).await?;
            let mut result = BTreeMap::new();
            for (hash, old) in old {
                let versions = self.versions(&old.project_id, compatibility).await?;
                if let Some(new) = versions
                    .into_iter()
                    .filter(|v| {
                        v.version_type == "release" && plan::compatible(v, "mods", compatibility)
                    })
                    .max_by_key(|v| chrono::DateTime::parse_from_rfc3339(&v.date_published).ok())
                {
                    result.insert(hash, new);
                }
            }
            Ok(result)
        })
    }
}

/// Keep the complete HTTP future inside the cancellation race, including DNS,
/// TLS and headers. Returning drops the losing future and closes its transfer.
/// The caller uses Tauri's existing Tokio runtime; this owns no worker thread.
pub(crate) async fn cancellable<F: Future>(
    cancel: &AtomicBool,
    deadline: Instant,
    future: F,
) -> Result<F::Output> {
    tokio::select! {
        biased;
        _ = async {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        } => Err(CANCELLED.into()),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err("Modrinth请求超过时间限制".into()),
        result = future => Ok(result),
    }
}

pub(crate) struct HttpProvider<'a> {
    pub client: reqwest::Client,
    pub cancel: &'a AtomicBool,
    pub network_bytes: AtomicU64,
    pub downloads: std::sync::Arc<pcl_network::DownloadScheduler>,
    metadata_bytes: AtomicU64,
    requests: AtomicU64,
    deadline: Instant,
}
impl<'a> HttpProvider<'a> {
    pub fn new(cancel: &'a AtomicBool) -> Result<Self> {
        let client = pcl_network::async_client()
            .user_agent("PCL-RH/0.2.0 (https://github.com/HyacineFengJin/PCL-RH)")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            cancel,
            network_bytes: AtomicU64::new(0),
            downloads: pcl_network::download_snapshot(),
            metadata_bytes: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            deadline: Instant::now() + Duration::from_secs(180),
        })
    }
    pub fn with_download_policy(
        mut self,
        scheduler: std::sync::Arc<pcl_network::DownloadScheduler>,
    ) -> Self {
        self.downloads = scheduler;
        self
    }
    /// Large paced artifacts can outlive the metadata preparation deadline.
    /// Renewal keeps the same immutable HTTP client/network snapshot, while a
    /// fresh bounded metadata budget permits the final authority recheck.
    pub(crate) fn renewed_metadata_session(&self) -> Self {
        Self {
            client: self.client.clone(),
            cancel: self.cancel,
            network_bytes: AtomicU64::new(0),
            downloads: self.downloads.clone(),
            metadata_bytes: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            deadline: Instant::now() + Duration::from_secs(180),
        }
    }
    async fn read<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T> {
        if self.requests.fetch_add(1, Ordering::Relaxed) >= MAX_HTTP_REQUESTS {
            return Err("Modrinth依赖请求数量超过限制".into());
        }
        let deadline = self.deadline.min(Instant::now() + Duration::from_secs(20));
        let mut response = cancellable(self.cancel, deadline, request.send())
            .await?
            .map_err(|_| "无法读取Modrinth官方数据")?;
        if !response.status().is_success() {
            return Err(format!(
                "Modrinth官方数据HTTP {}",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE as u64)
        {
            return Err("Modrinth官方响应过大".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = cancellable(self.cancel, deadline, response.chunk())
            .await?
            .map_err(|_| "Modrinth响应读取失败")?
        {
            self.network_bytes
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            if self
                .metadata_bytes
                .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                + chunk.len() as u64
                > MAX_METADATA_BYTES
                || chunk.len() > MAX_RESPONSE.saturating_sub(bytes.len())
            {
                return Err("Modrinth官方响应超过大小限制".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| "Modrinth官方数据格式无效".into())
    }
}
impl Provider for HttpProvider<'_> {
    fn projects<'a>(&'a self, ids: &'a [String]) -> FutureResult<'a, Vec<Project>> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(vec![]);
            }
            if ids.len() > 256 {
                return Err("批量项目检查超过256项".into());
            }
            for value in ids {
                id(value)?
            }
            // https://docs.modrinth.com/api/operations/getprojects/
            let query = serde_json::to_string(ids).map_err(|e| e.to_string())?;
            let projects: Vec<Project> = self
                .read(
                    self.client
                        .get(format!("{API}projects"))
                        .query(&[("ids", query)]),
                )
                .await?;
            validate_projects(ids, &projects)?;
            Ok(projects)
        })
    }
    fn updates<'a>(
        &'a self,
        hashes: &'a [String],
        compatibility: &'a Compatibility,
    ) -> FutureResult<'a, BTreeMap<String, Version>> {
        Box::pin(async move {
            if hashes.is_empty() {
                return Ok(BTreeMap::new());
            }
            if hashes.len() > 256 {
                return Err("批量更新检查超过256项".into());
            }
            for hash in hashes {
                sha512(hash)?
            }
            // https://docs.modrinth.com/api/operations/getlatestversionsfromhashes/
            let versions:BTreeMap<String,Version>=self.read(self.client.post(format!("{API}version_files/update")).json(&serde_json::json!({"hashes":hashes,"algorithm":"sha512","loaders":[compatibility.loader],"game_versions":[compatibility.minecraft_version],"version_types":["release"]}))).await?;
            validate_updates(hashes, &versions)?;
            Ok(versions)
        })
    }
    fn project<'a>(&'a self, value: &'a str) -> FutureResult<'a, Project> {
        Box::pin(async move {
            id(value)?;
            let project: Project = self
                .read(self.client.get(format!("{API}project/{value}")))
                .await?;
            if project.id != value || project.title.len() > 2048 {
                return Err("Modrinth项目ID或标题无效".into());
            }
            Ok(project)
        })
    }
    fn version<'a>(&'a self, value: &'a str) -> FutureResult<'a, Version> {
        Box::pin(async move {
            id(value)?;
            let version: Version = self
                .read(self.client.get(format!("{API}version/{value}")))
                .await?;
            validate_version(&version)?;
            if version.id != value {
                return Err("Modrinth版本ID不匹配".into());
            }
            Ok(version)
        })
    }
    fn versions<'a>(
        &'a self,
        project: &'a str,
        compatibility: &'a Compatibility,
    ) -> FutureResult<'a, Vec<Version>> {
        Box::pin(async move {
            id(project)?;
            let game = serde_json::to_string(&[&compatibility.minecraft_version])
                .map_err(|e| e.to_string())?;
            let versions: Vec<Version> = self
                .read(
                    self.client
                        .get(format!("{API}project/{project}/version"))
                        .query(&[
                            ("game_versions", game.as_str()),
                            ("include_changelog", "false"),
                        ]),
                )
                .await?;
            if versions.len() > 4096 {
                return Err("Modrinth候选版本数量超过限制".into());
            }
            for version in &versions {
                validate_version(version)?;
                if version.project_id != project {
                    return Err("Modrinth候选版本项目ID不匹配".into());
                }
            }
            Ok(versions)
        })
    }
    fn from_hashes<'a>(
        &'a self,
        hashes: &'a [String],
    ) -> FutureResult<'a, BTreeMap<String, Version>> {
        Box::pin(async move {
            if hashes.is_empty() {
                return Ok(BTreeMap::new());
            }
            if hashes.len() > 256 {
                return Err("Modrinth批量校验文件数量超过限制".into());
            }
            for value in hashes {
                sha512(value)?
            }
            let versions: BTreeMap<String, Version> = self
                .read(
                    self.client
                        .post(format!("{API}version_files"))
                        .json(&serde_json::json!({"algorithm":"sha512","hashes":hashes})),
                )
                .await?;
            for (hash, version) in &versions {
                validate_version(version)?;
                if !hashes.contains(hash) || !version.files.iter().any(|f| f.hashes.sha512 == *hash)
                {
                    return Err("Modrinth校验响应与本地文件哈希不符".into());
                }
            }
            Ok(versions)
        })
    }
}

pub(super) fn validate_projects(requested: &[String], projects: &[Project]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for project in projects {
        id(&project.id)?;
        if !requested.contains(&project.id)
            || !seen.insert(&project.id)
            || project.title.len() > 2048
        {
            return Err("Modrinth批量项目返回未请求、重复或无效项目".into());
        }
    }
    Ok(())
}
pub(super) fn validate_updates(
    requested: &[String],
    versions: &BTreeMap<String, Version>,
) -> Result<()> {
    for (hash, version) in versions {
        if !requested.contains(hash) {
            return Err("Modrinth批量更新返回未请求的旧哈希".into());
        }
        validate_version(version)?;
        // The key is the OLD file hash; it must not be matched to new bytes.
    }
    Ok(())
}
