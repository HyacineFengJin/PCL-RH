//! Public community search orchestration and evidence-based result grouping.
//!
//! Providers retain their real IDs, filters and errors. Partial failures remain
//! warnings when another source succeeded. Only an encyclopedia page's explicit
//! related project links can bridge providers; bounded detail reads enrich the
//! current query without crawling the catalog or claiming an unverified match.

use crate::community_curseforge::CurseforgeClient;
use crate::community_identity::{
    canonical_project_url, merge_identities, AliasTarget, CommunitySource, ProjectIdentity,
    ProjectKey, ProjectKind,
};
use crate::community_mcmod::{McmodClient, McmodDetail, McmodSearchPage};
#[path = "community_catalog/enrichment.rs"]
mod enrichment;
use enrichment::enrich_mcmod;

use reqwest::Url;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

const MAX_PROVIDER_HITS: usize = 50;
const MAX_MR_RESPONSE: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestedSource {
    All,
    One(CommunitySource),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EncyclopediaUse {
    Skip,
    Visible,
    /// Links may connect already filtered provider rows. The encyclopedia
    /// batch itself cannot produce a card or establish a successful search.
    EvidenceOnly,
}

#[derive(Clone, Debug)]
struct SearchRequest {
    query: String,
    kind: ProjectKind,
    source: RequestedSource,
    version: Option<String>,
    loader: Option<String>,
    sort: String,
    tag: Option<String>,
    page: u32,
}

impl SearchRequest {
    fn parse(
        query: String,
        project_type: Option<String>,
        source: Option<String>,
        version: Option<String>,
        loader: Option<String>,
        sort: Option<String>,
        tag: Option<String>,
    ) -> Result<Self, String> {
        if query.len() > 1024 || query.chars().any(char::is_control) {
            return Err("搜索内容过长或包含无效字符".into());
        }
        let kind = match project_type.as_deref().unwrap_or("mod") {
            "mod" => ProjectKind::Mod,
            "modpack" => ProjectKind::Modpack,
            "resourcepack" => ProjectKind::Resourcepack,
            "shader" => ProjectKind::Shader,
            "datapack" => ProjectKind::Datapack,
            _ => return Err("不支持的资源类型".into()),
        };
        let source = match source.as_deref().unwrap_or("全部") {
            "全部" => RequestedSource::All,
            "Modrinth" => RequestedSource::One(CommunitySource::Modrinth),
            "CurseForge" => RequestedSource::One(CommunitySource::Curseforge),
            "MC百科" => RequestedSource::One(CommunitySource::Mcmod),
            _ => return Err("不支持的社区来源".into()),
        };
        let sort = match sort.as_deref().unwrap_or("默认") {
            "" | "默认" | "relevance" => "relevance",
            "下载量" | "downloads" => "downloads",
            "关注量" | "follows" => "follows",
            "最新发布" | "newest" => "newest",
            "更新时间" | "最近更新" | "最新更新" | "updated" => "updated",
            _ => return Err("不支持的资源排序".into()),
        };
        Ok(Self {
            query: query.trim().into(),
            kind,
            source,
            version: active_filter(version)?,
            loader: active_filter(loader)?.map(|value| value.to_ascii_lowercase()),
            sort: sort.into(),
            tag: active_filter(tag)?,
            page: 1,
        })
    }

    fn includes(&self, source: CommunitySource) -> bool {
        self.source == RequestedSource::All || self.source == RequestedSource::One(source)
    }

    fn wants_mcmod(&self) -> bool {
        self.includes(CommunitySource::Mcmod)
            && (self.source != RequestedSource::All || !self.query.is_empty())
    }

    fn encyclopedia_use(&self) -> Result<EncyclopediaUse, &'static str> {
        if !self.wants_mcmod() {
            return Ok(EncyclopediaUse::Skip);
        }
        if !matches!(self.kind, ProjectKind::Mod | ProjectKind::Modpack) {
            return Err("MC百科当前仅支持模组与整合包检索");
        }
        if self.query.chars().count() > 255 {
            return Err("MC百科搜索内容不能超过 255 个字符，已跳过该来源");
        }
        if let Some(reason) = self.mcmod_skip_reason() {
            if self.source == RequestedSource::All {
                return Ok(EncyclopediaUse::EvidenceOnly);
            }
            return Err(reason);
        }
        Ok(EncyclopediaUse::Visible)
    }

    fn mcmod_skip_reason(&self) -> Option<&'static str> {
        if !matches!(self.kind, ProjectKind::Mod | ProjectKind::Modpack) {
            Some("MC百科当前仅支持模组与整合包检索")
        } else if self.version.is_some() || self.loader.is_some() || self.tag.is_some() {
            Some("MC百科暂不支持版本、加载器或标签筛选，已跳过该来源")
        } else if self.sort != "relevance" {
            Some("MC百科暂不支持所选排序，已跳过该来源")
        } else if self.query.chars().count() > 255 {
            Some("MC百科搜索内容不能超过 255 个字符，已跳过该来源")
        } else {
            None
        }
    }
}

fn active_filter(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    if value.len() > 128 || value.chars().any(char::is_control) {
        return Err("筛选内容过长或包含无效字符".into());
    }
    let value = value.trim();
    Ok((!matches!(value, "" | "任意" | "全部")).then(|| value.into()))
}

#[derive(Clone, Debug)]
struct CatalogRow {
    source: CommunitySource,
    hit: Value,
    identity: ProjectIdentity,
}

struct SourceBatch {
    source: CommunitySource,
    evidence_only: bool,
    result: Result<Vec<CatalogRow>, String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct SourceLink {
    source: &'static str,
    project_id: Option<String>,
    url: String,
}

#[tauri::command]
pub async fn community_search(
    query: String,
    project_type: Option<String>,
    source: Option<String>,
    version: Option<String>,
    loader: Option<String>,
    sort: Option<String>,
    tag: Option<String>,
    page: Option<u32>,
) -> Result<Value, String> {
    let mut request =
        SearchRequest::parse(query, project_type, source, version, loader, sort, tag)?;
    request.page = checked_page(page)?;
    search_catalog(request, pcl_network::snapshot()).await
}

async fn search_catalog(
    request: SearchRequest,
    network: Arc<pcl_network::ClientFactory>,
) -> Result<Value, String> {
    let mut warnings = Vec::new();
    let mut encyclopedia_use = match request.encyclopedia_use() {
        Ok(mode) => mode,
        Err(reason) => {
            if request.source != RequestedSource::All {
                return Err(reason.into());
            }
            warnings.push(reason.into());
            EncyclopediaUse::Skip
        }
    };
    let cf_enabled = source_page_allowed(&request, CommunitySource::Curseforge, &mut warnings)?;
    if encyclopedia_use != EncyclopediaUse::Skip
        && !source_page_allowed(&request, CommunitySource::Mcmod, &mut warnings)?
    {
        encyclopedia_use = EncyclopediaUse::Skip;
    }
    if encyclopedia_use == EncyclopediaUse::EvidenceOnly {
        warnings.push("MC百科仅核对来源关联，列表只展示通过所选筛选的下载来源项目".into());
    }
    let mr = async {
        if !request.includes(CommunitySource::Modrinth) {
            return None;
        }
        let result = search_modrinth(&request, &network).await;
        let has_more = result.as_ref().is_ok_and(|page| page.has_more);
        Some((
            SourceBatch {
                source: CommunitySource::Modrinth,
                evidence_only: false,
                result: result.map(|page| page.rows),
            },
            has_more,
            false,
        ))
    };
    let cf = async {
        if !cf_enabled {
            return None;
        }
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            let client = CurseforgeClient::new(network.clone())?;
            let values = client
                .search_page(
                    &request.query,
                    request.kind,
                    request.version.as_deref(),
                    request.loader.as_deref(),
                    Some(if request.sort == "relevance" {
                        "default"
                    } else {
                        &request.sort
                    }),
                    request.tag.as_deref(),
                    (request.page as usize - 1) * 50,
                )
                .await?;
            Ok::<_, String>(ProviderPage {
                rows: provider_rows(CommunitySource::Curseforge, request.kind, values.hits)?,
                has_more: values.has_more,
                limited: values.limited,
            })
        })
        .await
        .unwrap_or_else(|_| Err("CurseForge 检索超过 20 秒上限".into()));
        let has_more = result.as_ref().is_ok_and(|page| page.has_more);
        let limited = result.as_ref().is_ok_and(|page| page.limited);
        Some((
            SourceBatch {
                source: CommunitySource::Curseforge,
                evidence_only: false,
                result: result.map(|page| page.rows),
            },
            has_more,
            limited,
        ))
    };
    let encyclopedia = async {
        if encyclopedia_use == EncyclopediaUse::Skip {
            return None;
        }
        let result = async {
            let client = Arc::new(McmodClient::new(network.clone())?);
            let page = client
                .search(&request.query, request.page, request.kind)
                .await?;
            Ok::<_, String>((client, page))
        }
        .await;
        Some(result)
    };
    // Each source uses the exact same immutable network policy snapshot.
    let (mr, cf, encyclopedia) = tokio::join!(mr, cf, encyclopedia);
    let mut has_more = false;
    let mut batches = Vec::new();
    for (batch, more, limited) in [mr, cf].into_iter().flatten() {
        has_more |= more;
        if limited {
            warnings.insert(
                0,
                "CurseForge API 最多浏览前 10,000 个结果，请缩小搜索范围".into(),
            );
        }
        batches.push(batch);
    }
    if let Some(result) = encyclopedia {
        let result = match result {
            Ok((client, page)) => {
                // Evidence-only encyclopedia rows cannot extend a filtered
                // download catalog after its actual providers have ended.
                if encyclopedia_use == EncyclopediaUse::Visible {
                    has_more |= page.has_more && request.page < 100;
                }
                if page.has_more && request.page == 100 {
                    warnings.insert(
                        0,
                        "MC百科已达到支持的 100 页浏览范围，请缩小搜索范围".into(),
                    );
                }
                let details = if request.query.is_empty() {
                    BTreeMap::new()
                } else {
                    let (details, extra) = enrich_mcmod(client, &network, &page.hits).await;
                    warnings.extend(extra);
                    details
                };
                Ok(mcmod_rows(page, &details))
            }
            Err(error) => Err(error),
        };
        batches.push(SourceBatch {
            source: CommunitySource::Mcmod,
            evidence_only: encyclopedia_use == EncyclopediaUse::EvidenceOnly,
            result,
        });
    }
    finish_page(batches, warnings, request.source, request.page, has_more)
}

fn finish_page(
    batches: Vec<SourceBatch>,
    mut warnings: Vec<String>,
    source: RequestedSource,
    page: u32,
    has_more: bool,
) -> Result<Value, String> {
    if page == u32::MAX && has_more {
        warnings.insert(0, "社区资源页码达到参数范围上限，请缩小搜索范围".into());
    }
    let mut result = assemble_results(batches, warnings, source)?;
    result["pagination"] = json!({ "page": page, "has_next": has_more && page < u32::MAX });
    Ok(result)
}

fn checked_page(page: Option<u32>) -> Result<u32, String> {
    let page = page.unwrap_or(1);
    if page == 0 {
        return Err("社区资源页码必须大于零".into());
    }
    Ok(page)
}

struct ProviderPage {
    rows: Vec<CatalogRow>,
    has_more: bool,
    limited: bool,
}

fn provider_offset(page: u32, size: u64) -> Result<u64, String> {
    page.checked_sub(1)
        .and_then(|page| u64::from(page).checked_mul(size))
        .ok_or_else(|| "社区来源分页偏移超出支持范围".into())
}

fn source_page_allowed(
    request: &SearchRequest,
    source: CommunitySource,
    warnings: &mut Vec<String>,
) -> Result<bool, String> {
    if !request.includes(source) {
        return Ok(false);
    }
    let (maximum, message) = match source {
        CommunitySource::Curseforge => (
            200,
            "CurseForge API 最多浏览前 10,000 个结果，请缩小搜索范围",
        ),
        CommunitySource::Mcmod => (100, "MC百科仅支持前 100 页浏览，请缩小搜索范围"),
        CommunitySource::Modrinth => (u32::MAX, ""),
    };
    if request.page <= maximum {
        return Ok(true);
    }
    if request.source != RequestedSource::All {
        return Err(message.into());
    }
    warnings.push(message.into());
    Ok(false)
}

async fn search_modrinth(
    request: &SearchRequest,
    network: &pcl_network::ClientFactory,
) -> Result<ProviderPage, String> {
    let client = network
        .async_client()
        .user_agent("PCL-RH/0.2.0 (https://github.com/HyacineFengJin/PCL-RH)")
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("无法创建 Modrinth 客户端：{error}"))?;
    let facets = modrinth_facets(request);
    let index = if request.query.is_empty() && request.sort == "relevance" {
        "downloads"
    } else {
        &request.sort
    };
    let offset = provider_offset(request.page, 30)?.to_string();
    let mut response = client
        .get("https://api.modrinth.com/v2/search")
        .query(&[
            ("query", request.query.as_str()),
            ("facets", facets.as_str()),
            ("index", index),
            ("limit", "30"),
            ("offset", offset.as_str()),
        ])
        .send()
        .await
        .map_err(|error| format!("无法读取 Modrinth：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Modrinth 请求失败：HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_MR_RESPONSE as u64)
    {
        return Err("Modrinth 目录响应过大".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("读取 Modrinth 失败：{error}"))?
    {
        if chunk.len() > MAX_MR_RESPONSE.saturating_sub(bytes.len()) {
            return Err("Modrinth 目录响应过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let result: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Modrinth 目录格式无效：{error}"))?;
    modrinth_page(result, request.kind, request.page)
}

fn modrinth_page(result: Value, kind: ProjectKind, page: u32) -> Result<ProviderPage, String> {
    let hits = result
        .get("hits")
        .and_then(Value::as_array)
        .ok_or("Modrinth 目录缺少项目列表")?;
    let offset = provider_offset(page, 30)?;
    let total = result["total_hits"]
        .as_u64()
        .ok_or("Modrinth 目录缺少分页总数")?;
    let returned = hits.len() as u64;
    if result["offset"].as_u64() != Some(offset)
        || result["limit"].as_u64() != Some(30)
        || returned > total.saturating_sub(offset)
        || (returned == 0 && total > offset)
    {
        return Err("Modrinth 分页响应无效".into());
    }
    Ok(ProviderPage {
        rows: provider_rows(CommunitySource::Modrinth, kind, hits.clone())?,
        has_more: offset + returned < total,
        limited: false,
    })
}

fn modrinth_facets(request: &SearchRequest) -> String {
    let facet = if request.kind == ProjectKind::Datapack {
        "all_project_types:datapack".into()
    } else {
        format!("project_type:{}", kind_name(request.kind))
    };
    let mut facets = vec![vec![facet]];
    for (key, value) in [
        ("versions", &request.version),
        ("categories", &request.loader),
        ("categories", &request.tag),
    ] {
        if let Some(value) = value {
            facets.push(vec![format!("{key}:{value}")]);
        }
    }
    // All entries are strings assembled in memory, so serialization cannot fail.
    json!(facets).to_string()
}

fn provider_rows(
    source: CommunitySource,
    kind: ProjectKind,
    hits: Vec<Value>,
) -> Result<Vec<CatalogRow>, String> {
    let limit = if source == CommunitySource::Modrinth {
        30
    } else {
        MAX_PROVIDER_HITS
    };
    if hits.len() > limit {
        return Err("社区来源返回的项目数量超过上限".into());
    }
    let mut rows = Vec::new();
    for hit in hits {
        let id = hit
            .get("project_id")
            .and_then(Value::as_str)
            .ok_or("社区来源缺少项目 ID")?;
        if source == CommunitySource::Modrinth
            && (id.len() != 8 || !id.bytes().all(|ch| ch.is_ascii_alphanumeric()))
        {
            return Err("Modrinth 项目 ID 无效".into());
        }
        let project = ProjectKey::new(source, kind, id)?;
        let url = if source == CommunitySource::Modrinth {
            let slug = hit.get("slug").and_then(Value::as_str).unwrap_or(id);
            canonical_project_url(&format!("https://modrinth.com/{}/{slug}", kind_name(kind)))
        } else {
            hit.get("url")
                .and_then(Value::as_str)
                .and_then(canonical_project_url)
        }
        .ok_or("社区来源缺少有效的项目链接")?;
        if !url.starts_with(source_prefix(source)) {
            return Err("社区项目链接与来源不一致".into());
        }
        let identity = ProjectIdentity {
            project,
            canonical_url: url.clone(),
            aliases: Vec::new(),
        };
        // Validate provider URL/type bindings before admitting the whole batch.
        merge_identities(std::slice::from_ref(&identity))?;
        let title = required_text(&hit, "title", 256)?;
        let description = optional_text(&hit, "description", 4096);
        let icon_url = hit
            .get("icon_url")
            .and_then(Value::as_str)
            .and_then(|raw| safe_icon(raw, source));
        let mut categories = string_array(&hit, "categories", 64, 128);
        for loader in string_array(&hit, "loaders", 16, 32) {
            if !categories.contains(&loader) {
                categories.push(loader);
            }
        }
        let mut value = json!({
            "project_id": id, "title": title, "description": description,
            "slug": hit.get("slug").and_then(Value::as_str), "icon_url": icon_url,
            "categories": categories,
            "display_categories": string_array(&hit, "display_categories", 64, 128),
            "versions": string_array(&hit, "versions", 512, 64),
            "downloads": hit.get("downloads").and_then(Value::as_u64).unwrap_or(0),
            "date_modified": optional_text(&hit, "date_modified", 64),
            "project_type": kind_name(kind), "source": source_name(source), "url": url,
        });
        for flag in ["is_available", "download_allowed"] {
            if let Some(value_flag) = hit.get(flag).and_then(Value::as_bool) {
                value[flag] = json!(value_flag);
            }
        }
        rows.push(CatalogRow {
            source,
            hit: value,
            identity,
        });
    }
    Ok(rows)
}

fn mcmod_rows(
    page: McmodSearchPage,
    details: &BTreeMap<ProjectKey, McmodDetail>,
) -> Vec<CatalogRow> {
    page.hits.into_iter().map(|hit| {
        let detail = details.get(&hit.project);
        let identity = detail.map_or_else(|| hit.identity(), McmodDetail::identity);
        let title = detail.map_or(hit.title.as_str(), |detail| detail.title.as_str());
        let value = json!({
            "project_id": hit.project.id, "title": title, "description": hit.summary,
            "icon_url": hit.icon_url, "categories": [], "display_categories": [], "versions": [],
            "downloads": 0, "date_modified": "", "project_type": kind_name(hit.project.kind),
            "source": "MC百科", "url": hit.url, "attribution": page.attribution,
        });
        CatalogRow { source: CommunitySource::Mcmod, hit: value, identity }
    }).collect()
}

fn assemble_results(
    batches: Vec<SourceBatch>,
    mut warnings: Vec<String>,
    requested: RequestedSource,
) -> Result<Value, String> {
    let mut rows = Vec::new();
    let mut eligible_rows = BTreeSet::new();
    let mut succeeded = 0;
    for batch in batches {
        match batch.result {
            Ok(mut hits) => {
                if !batch.evidence_only {
                    succeeded += 1;
                    eligible_rows.extend(rows.len()..rows.len() + hits.len());
                }
                rows.append(&mut hits);
            }
            Err(error) => {
                let warning = format!("{}：{error}", source_name(batch.source));
                if requested != RequestedSource::All {
                    return Err(warning);
                }
                warnings.push(warning);
            }
        }
    }
    if succeeded == 0 {
        return Err(warnings.join("；"));
    }
    let identities: Vec<_> = rows.iter().map(|row| row.identity.clone()).collect();
    let resolution = merge_identities(&identities)?;
    let conflicted: BTreeSet<_> = resolution
        .conflicts
        .iter()
        .flat_map(|conflict| conflict.indices.iter().copied())
        .collect();
    if !resolution.conflicts.is_empty() {
        warnings.push("部分百科关联包含多个同站项目或不同资源类型，已保留独立条目".into());
    }
    let mut hits = Vec::new();
    for group in resolution.groups {
        let Some(&primary) = group
            .iter()
            .filter(|index| eligible_rows.contains(index))
            .min_by_key(|&&index| (source_priority(rows[index].source), index))
        else {
            // Unfiltered encyclopedia results can prove links, but cannot
            // bypass the provider's version/loader/tag filtering or ranking.
            continue;
        };
        let mut hit = rows[primary].hit.clone();
        let mut links = Vec::new();
        for &index in &group {
            let row = &rows[index];
            add_source_link(
                &mut links,
                SourceLink {
                    source: source_name(row.source),
                    project_id: Some(row.identity.project.id.clone()),
                    url: row.identity.canonical_url.clone(),
                },
            );
            if !conflicted.contains(&index) {
                for alias in &row.identity.aliases {
                    let AliasTarget::Url(raw) = &alias.target else {
                        continue;
                    };
                    let Some(url) = canonical_project_url(raw) else {
                        continue;
                    };
                    let Some(source) = source_for_url(&url) else {
                        continue;
                    };
                    let known = group
                        .iter()
                        .map(|&other| &rows[other])
                        .find(|other| other.identity.canonical_url == url);
                    add_source_link(
                        &mut links,
                        SourceLink {
                            source: source_name(source),
                            project_id: known.map(|row| row.identity.project.id.clone()),
                            url,
                        },
                    );
                }
            }
        }
        links.sort_by_key(|link| source_priority_name(link.source));
        if let Some(encyclopedia) = group
            .iter()
            .map(|&index| &rows[index])
            .find(|row| row.source == CommunitySource::Mcmod)
        {
            if hit["title"] != encyclopedia.hit["title"] {
                hit["original_title"] = hit["title"].clone();
                hit["title"] = encyclopedia.hit["title"].clone();
            }
            hit["attribution"] = encyclopedia.hit["attribution"].clone();
        }
        hit["identity_key"] = Value::String(identity_key(&rows[primary].identity.project));
        hit["sources"] = json!(links);
        hits.push(hit);
    }
    let mut seen = BTreeSet::new();
    warnings = warnings
        .into_iter()
        .map(|warning| warning.chars().take(512).collect::<String>())
        .filter(|warning| seen.insert(warning.clone()))
        .take(16)
        .collect();
    Ok(json!({"hits": hits, "warnings": warnings}))
}

fn add_source_link(links: &mut Vec<SourceLink>, next: SourceLink) {
    if !links.iter().any(|old| {
        old.source == next.source
            && ((old.project_id.is_some() && old.project_id == next.project_id)
                || old.url == next.url)
    }) {
        links.push(next);
    }
}

fn source_name(source: CommunitySource) -> &'static str {
    match source {
        CommunitySource::Modrinth => "Modrinth",
        CommunitySource::Curseforge => "CurseForge",
        CommunitySource::Mcmod => "MC百科",
    }
}
fn source_prefix(source: CommunitySource) -> &'static str {
    match source {
        CommunitySource::Modrinth => "https://modrinth.com/",
        CommunitySource::Curseforge => "https://www.curseforge.com/",
        CommunitySource::Mcmod => "https://www.mcmod.cn/",
    }
}
fn source_for_url(url: &str) -> Option<CommunitySource> {
    [
        CommunitySource::Modrinth,
        CommunitySource::Curseforge,
        CommunitySource::Mcmod,
    ]
    .into_iter()
    .find(|&source| url.starts_with(source_prefix(source)))
}
fn source_priority(source: CommunitySource) -> u8 {
    match source {
        CommunitySource::Modrinth => 0,
        CommunitySource::Curseforge => 1,
        CommunitySource::Mcmod => 2,
    }
}
fn source_priority_name(source: &str) -> u8 {
    match source {
        "Modrinth" => 0,
        "CurseForge" => 1,
        _ => 2,
    }
}
fn kind_name(kind: ProjectKind) -> &'static str {
    match kind {
        ProjectKind::Mod => "mod",
        ProjectKind::Modpack => "modpack",
        ProjectKind::Resourcepack => "resourcepack",
        ProjectKind::Shader => "shader",
        ProjectKind::Datapack => "datapack",
    }
}
fn identity_key(project: &ProjectKey) -> String {
    match project.source {
        CommunitySource::Modrinth => format!("modrinth:{}", project.id),
        CommunitySource::Curseforge => format!("curseforge:{}", project.id),
        CommunitySource::Mcmod => format!(
            "mcmod:{}/{}",
            if project.kind == ProjectKind::Modpack {
                "modpack"
            } else {
                "class"
            },
            project.id
        ),
    }
}
fn required_text(hit: &Value, key: &str, limit: usize) -> Result<String, String> {
    let value = hit
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or("社区来源缺少项目标题")?;
    Ok(value.chars().take(limit).collect())
}
fn optional_text(hit: &Value, key: &str, limit: usize) -> String {
    hit.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(limit)
        .collect()
}
fn string_array(hit: &Value, key: &str, count: usize, length: usize) -> Vec<String> {
    hit.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(count)
        .map(|text| text.chars().take(length).collect())
        .collect()
}
fn safe_icon(raw: &str, source: CommunitySource) -> Option<String> {
    if raw.len() > 2048 {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let allowed = match source {
        CommunitySource::Modrinth => url.host_str()? == "cdn.modrinth.com",
        CommunitySource::Curseforge => matches!(
            url.host_str()?,
            "media.forgecdn.net" | "mediafilez.forgecdn.net" | "edge.forgecdn.net"
        ),
        CommunitySource::Mcmod => matches!(url.host_str()?, "i.mcmod.cn" | "www.mcmod.cn"),
    };
    (allowed
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none())
    .then(|| url.into())
}

#[cfg(test)]
#[path = "community_catalog/tests.rs"]
mod tests;
