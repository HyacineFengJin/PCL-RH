//! Bounded, read-only Minecraft catalog access through CurseForge's official API.
//!
//! The client captures the launcher's network policy and its process-provided
//! API key (developer override or official build credential) once. Display metadata deliberately omits file URLs and hashes;
//! downloading resolves an exact project/file pair again and checks the author
//! distribution flag before exposing metadata to the native download service.
//! API contract: <https://docs.curseforge.com/rest-api/>.

#[path = "community_curseforge/credentials.rs"]
pub(crate) mod credentials;
#[path = "community_curseforge/mapping.rs"]
mod mapping;
#[path = "community_curseforge/transport.rs"]
mod transport;
#[path = "community_curseforge/types.rs"]
mod types;

use crate::community_identity::ProjectKind;
use mapping::{
    class_id, normalize_project, normalize_version, numeric_id, project_url_query, search_query,
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use transport::{Budget, Transport};
use types::{Envelope, File, Page, Project};

const MINECRAFT_GAME: u32 = 432;
const PAGE_SIZE: usize = 50;
const MAX_FILE_PAGES: usize = 4;
const MAX_VERSIONS: usize = PAGE_SIZE * MAX_FILE_PAGES;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(45);

pub struct CurseforgeClient {
    transport: Transport,
}

pub struct CurseforgeSearchPage {
    pub hits: Vec<Value>,
    pub has_more: bool,
    pub limited: bool,
}

/// Trusted API metadata for the native file downloader. The API key never
/// travels with this value. Official CDN authentication is attached separately
/// by the native transport after validating the exact destination.
#[derive(Debug)]
pub struct CurseforgeDownload {
    pub title: String,
    pub version: String,
    pub name: String,
    pub size: u64,
    pub url: String,
    pub sha1: String,
}

/// Exact manifest-file metadata for a native pack. The official project class
/// chooses the content directory; manifests never supply a CDN or output path.
pub(crate) struct CurseforgePackDownload {
    pub directory: &'static str,
    pub download: CurseforgeDownload,
}

/// Recheck native download metadata against the official CDN authority policy.
/// This URL carries no CurseForge API credential.
pub fn download_url(raw: &str) -> Result<reqwest::Url, String> {
    mapping::download_url(raw)
}

impl CurseforgeClient {
    pub(crate) fn download_request(
        &self,
        url: &reqwest::Url,
    ) -> Result<reqwest::RequestBuilder, String> {
        self.transport.download_request(url)
    }
    pub fn new(network: Arc<pcl_network::ClientFactory>) -> Result<Self, String> {
        Ok(Self {
            transport: Transport::new(network)?,
        })
    }

    #[cfg(test)]
    pub async fn search(
        &self,
        query: &str,
        kind: ProjectKind,
        version: Option<&str>,
        loader: Option<&str>,
        sort: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        self.search_page(query, kind, version, loader, sort, tag, 0)
            .await
            .map(|page| page.hits)
    }

    /// Search offsets are provider offsets, before cross-provider grouping.
    /// Validated pagination metadata, never normalized-card counts, decides
    /// whether another source page exists.
    pub async fn search_page(
        &self,
        query: &str,
        kind: ProjectKind,
        version: Option<&str>,
        loader: Option<&str>,
        sort: Option<&str>,
        tag: Option<&str>,
        index: usize,
    ) -> Result<CurseforgeSearchPage, String> {
        // Official API limit: index + pageSize <= 10,000.
        if index > 10_000 - PAGE_SIZE || index % PAGE_SIZE != 0 {
            return Err("CurseForge 搜索页码超出支持范围".into());
        }
        let mut query = search_query(query, kind, version, loader, sort, tag)?;
        query.iter_mut().find(|(key, _)| *key == "index").unwrap().1 = index.to_string();
        tokio::time::timeout(OPERATION_TIMEOUT, async {
            let mut budget = Budget::new(1);
            let page: Page<Project> = self
                .transport
                .get("mods/search", &query, &mut budget)
                .await?;
            page.validate(index, PAGE_SIZE)?;
            let has_more = index as u64 + (page.data.len() as u64) < page.pagination.total_count;
            let limited = has_more && index + PAGE_SIZE == 10_000;
            let mut seen = BTreeSet::new();
            let hits = page
                .data
                .into_iter()
                .map(|project| {
                    if project.class_id != Some(class_id(kind)?) || !seen.insert(project.id) {
                        return Err("CurseForge 返回了重复项目或不匹配的资源类型".into());
                    }
                    normalize_project(&project, true)
                })
                .collect::<Result<_, String>>()?;
            Ok(CurseforgeSearchPage {
                hits,
                has_more: has_more && !limited,
                limited,
            })
        })
        .await
        .map_err(|_| "CurseForge 检索超过总时间上限".to_owned())?
    }

    pub async fn details(&self, id: &str) -> Result<Value, String> {
        let id = numeric_id(id)?;
        tokio::time::timeout(OPERATION_TIMEOUT, self.read_details(id))
            .await
            .map_err(|_| "CurseForge 详情超过总时间上限".to_owned())?
    }

    /// An encyclopedia's explicit project link can be resolved to an official
    /// numeric ID using the API's exact slug selector. Names and free text are
    /// never involved, and ambiguous or mismatched metadata cannot become an
    /// identity bridge.
    pub async fn project_by_url(&self, url: &str, kind: ProjectKind) -> Result<Value, String> {
        let query = project_url_query(url, kind)?;
        tokio::time::timeout(OPERATION_TIMEOUT, async {
            let mut budget = Budget::new(1);
            let mut page: Page<Project> = self
                .transport
                .get("mods/search", &query, &mut budget)
                .await?;
            page.validate(0, 2)?;
            if page.pagination.total_count != 1 || page.data.len() != 1 {
                return Err("CurseForge 项目链接未解析到唯一官方项目".into());
            }
            let project = page.data.remove(0);
            if project.class_id != Some(class_id(kind)?) {
                return Err("CurseForge 项目链接与返回的资源类型不匹配".into());
            }
            let value = normalize_project(&project, true)?;
            if value["url"].as_str() != Some(url) {
                return Err("CurseForge 返回的项目与所请求链接不匹配".into());
            }
            Ok(value)
        })
        .await
        .map_err(|_| "CurseForge 项目链接解析超过总时间上限".to_owned())?
    }

    async fn read_details(&self, id: u32) -> Result<Value, String> {
        let mut budget = Budget::new(1 + MAX_FILE_PAGES);
        let project = self.project(id, &mut budget).await?;
        let display = normalize_project(&project, false)?;
        let mut versions = Vec::new();
        let mut seen = BTreeSet::new();
        let mut index = 0;
        let mut total = None;
        for _ in 0..MAX_FILE_PAGES {
            let page: Page<File> = self
                .transport
                .get(
                    &format!("mods/{id}/files"),
                    &[
                        ("index", index.to_string()),
                        ("pageSize", PAGE_SIZE.to_string()),
                    ],
                    &mut budget,
                )
                .await?;
            page.validate(index, PAGE_SIZE)?;
            if total.is_some_and(|expected| expected != page.pagination.total_count) {
                return Err("CurseForge 文件列表在分页期间发生变化，请重试".into());
            }
            total = Some(page.pagination.total_count);
            let count = page.data.len();
            for file in page.data {
                file.check_owner(id)?;
                if !seen.insert(file.id) {
                    return Err("CurseForge 文件列表包含重复版本，请重试".into());
                }
                versions.push(normalize_version(&file)?);
            }
            index += count;
            if index as u64 >= total.unwrap_or(0) {
                break;
            }
        }
        versions.sort_by(|left, right| {
            right["date_published"]
                .as_str()
                .cmp(&left["date_published"].as_str())
        });
        let total = total.unwrap_or(0);
        Ok(json!({
            "project": display,
            "versions_returned": versions.len(),
            "versions": versions,
            "versions_truncated": (index as u64) < total,
            "versions_total": total,
            "versions_limit": MAX_VERSIONS,
        }))
    }

    /// The renderer supplies identifiers only. Re-fetching both owners means a
    /// stale display card cannot authorize a different file or bypass a newly
    /// disabled distribution permission. No CDN URL is guessed or reconstructed.
    pub async fn download(
        &self,
        project_id: &str,
        version_id: &str,
        file_name: &str,
    ) -> Result<CurseforgeDownload, String> {
        let project_id = numeric_id(project_id)?;
        let file_id = numeric_id(version_id)?;
        mapping::check_filename(file_name)?;
        tokio::time::timeout(OPERATION_TIMEOUT, async {
            let mut budget = Budget::new(2);
            let project = self.project(project_id, &mut budget).await?;
            normalize_project(&project, false)?;
            if !project.is_available || project.allow_mod_distribution == Some(false) {
                return Err("CurseForge 项目不可用或作者已禁止第三方分发，请访问项目页面".into());
            }
            let response: Envelope<File> = self
                .transport
                .get(
                    &format!("mods/{project_id}/files/{file_id}"),
                    &[],
                    &mut budget,
                )
                .await?;
            mapping::download_metadata(&project, &response.data, file_id, file_name)
        })
        .await
        .map_err(|_| "CurseForge 下载信息超过总时间上限".to_owned())?
    }

    /// Resolve exact manifest IDs with one shared metadata budget. The same
    /// official distribution checks used by standalone Save apply to each file.
    pub(crate) async fn pack_downloads(
        &self,
        ids: &[(u32, u32)],
    ) -> Result<Vec<CurseforgePackDownload>, String> {
        if ids.len() > 2000
            || ids
                .iter()
                .any(|(project, file)| *project == 0 || *file == 0)
        {
            return Err("CurseForge 整合包文件 ID 无效或超过 2000 项限制".into());
        }
        tokio::time::timeout(Duration::from_secs(300), async {
            let mut budget = Budget::new(ids.len() * 2);
            let mut files = Vec::with_capacity(ids.len());
            for &(project_id, file_id) in ids {
                let project = self.project(project_id, &mut budget).await?;
                normalize_project(&project, false)?;
                let directory = match project.class_id {
                    Some(6) => "mods",
                    Some(12) => "resourcepacks",
                    Some(6552) => "shaderpacks",
                    _ => return Err("CurseForge 整合包文件的资源类型不受支持".into()),
                };
                if !project.is_available || project.allow_mod_distribution == Some(false) {
                    return Err(
                        "CurseForge 项目不可用或作者已禁止第三方分发，请访问项目页面".into(),
                    );
                }
                let response: Envelope<File> = self
                    .transport
                    .get(
                        &format!("mods/{project_id}/files/{file_id}"),
                        &[],
                        &mut budget,
                    )
                    .await?;
                let download = mapping::download_metadata(
                    &project,
                    &response.data,
                    file_id,
                    &response.data.file_name,
                )?;
                files.push(CurseforgePackDownload {
                    directory,
                    download,
                });
            }
            Ok(files)
        })
        .await
        .map_err(|_| "CurseForge 整合包文件解析超过总时间上限".to_owned())?
    }

    #[cfg(test)]
    pub(crate) fn loopback(network: Arc<pcl_network::ClientFactory>, address: String) -> Self {
        Self {
            transport: Transport::loopback(network, address),
        }
    }

    async fn project(&self, id: u32, budget: &mut Budget) -> Result<Project, String> {
        let response: Envelope<Project> = self
            .transport
            .get(&format!("mods/{id}"), &[], budget)
            .await?;
        if response.data.id != id || response.data.game_id != MINECRAFT_GAME {
            return Err("CurseForge 项目 ID 或游戏归属不匹配".into());
        }
        Ok(response.data)
    }
}

#[cfg(test)]
#[path = "community_curseforge/tests.rs"]
mod tests;
