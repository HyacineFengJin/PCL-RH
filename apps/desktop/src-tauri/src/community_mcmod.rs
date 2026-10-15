//! Read-only access to MC百科's public search, lists and project pages.
//!
//! HTML is parsed as data and only fixed result/title/related-link regions are
//! read. The site's encoded outgoing links are decoded locally; this client
//! never follows them, runs JavaScript, submits forms or accesses downloads.
//! Each operation makes one bounded request through the captured network policy.

use crate::community_identity::{
    canonical_project_url, AliasEvidence, AliasTarget, CommunitySource, ProjectIdentity,
    ProjectKey, ProjectKind,
};
use base64::Engine;
use dom_query::Document;
use reqwest::{Client, Url};
use serde::Serialize;
use std::{collections::BTreeSet, sync::Arc, time::Duration};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_HITS: usize = 32;
const MAX_LINKS: usize = 32;
const ATTRIBUTION: &str = "MC百科 (mcmod.cn) · 内容依站点声明使用 CC BY-NC-SA 3.0";

#[derive(Clone, Debug, Serialize)]
pub struct McmodSearchHit {
    pub project: ProjectKey,
    pub title: String,
    pub summary: String,
    pub url: String,
    pub icon_url: Option<String>,
}

impl McmodSearchHit {
    pub fn identity(&self) -> ProjectIdentity {
        ProjectIdentity {
            project: self.project.clone(),
            canonical_url: self.url.clone(),
            aliases: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct McmodSearchPage {
    pub hits: Vec<McmodSearchHit>,
    pub page: u32,
    pub has_more: bool,
    pub source_url: String,
    pub attribution: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct McmodDetail {
    pub project: ProjectKey,
    pub title: String,
    pub secondary_title: Option<String>,
    pub summary: String,
    pub url: String,
    pub aliases: Vec<AliasEvidence>,
    pub attribution: &'static str,
}

impl McmodDetail {
    pub fn identity(&self) -> ProjectIdentity {
        ProjectIdentity {
            project: self.project.clone(),
            canonical_url: self.url.clone(),
            aliases: self.aliases.clone(),
        }
    }
}

pub struct McmodClient {
    client: Client,
}

impl McmodClient {
    pub fn new(network: Arc<pcl_network::ClientFactory>) -> Result<Self, String> {
        let client = network
            .async_client()
            .user_agent("PCL-RH/0.2.0 (https://github.com/HyacineFengJin/PCL-RH)")
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("无法创建 MC百科网络客户端：{error}"))?;
        Ok(Self { client })
    }

    /// Empty queries browse the site's public list; nonempty queries use simple
    /// title/mod-ID search. No result page triggers detail prefetch on its own.
    pub async fn search(
        &self,
        query: &str,
        page: u32,
        kind: ProjectKind,
    ) -> Result<McmodSearchPage, String> {
        let url = search_url(query, page, kind)?;
        let html = self.read_html(url.clone()).await?;
        parse_search_html(&html, url.as_str(), page, kind, query.trim().is_empty())
    }

    pub async fn detail(&self, id: &str, kind: ProjectKind) -> Result<McmodDetail, String> {
        let key = ProjectKey::new(CommunitySource::Mcmod, kind, id)?;
        let route = route(kind)?;
        let url = Url::parse(&format!("https://www.mcmod.cn/{route}/{id}.html"))
            .map_err(|_| "MC百科项目地址无效".to_owned())?;
        let html = self.read_html(url.clone()).await?;
        parse_detail_html(&html, key, url.as_str())
    }

    async fn read_html(&self, url: Url) -> Result<String, String> {
        let mut response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "text/html")
            .send()
            .await
            .map_err(|error| format!("无法读取 MC百科：{error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "MC百科请求失败：HTTP {}",
                response.status().as_u16()
            ));
        }
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .is_some_and(|kind| !kind.to_ascii_lowercase().starts_with("text/html"))
        {
            return Err("MC百科返回的内容不是 HTML".into());
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err("MC百科响应超过大小上限".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("读取 MC百科响应失败：{error}"))?
        {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
                return Err("MC百科响应超过大小上限".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes).map_err(|_| "MC百科响应不是有效的 UTF-8 文本".into())
    }
}

fn route(kind: ProjectKind) -> Result<&'static str, String> {
    match kind {
        ProjectKind::Mod => Ok("class"),
        ProjectKind::Modpack => Ok("modpack"),
        _ => Err("MC百科当前仅支持模组与整合包检索".into()),
    }
}

fn search_url(query: &str, page: u32, kind: ProjectKind) -> Result<Url, String> {
    route(kind)?;
    if query.len() > 1024 || query.chars().count() > 255 || query.chars().any(char::is_control) {
        return Err("MC百科搜索内容过长或包含无效字符".into());
    }
    if !(1..=100).contains(&page) {
        return Err("MC百科页码超出支持范围".into());
    }
    let query = query.trim();
    let mut url = if query.is_empty() {
        Url::parse(match kind {
            ProjectKind::Mod => "https://www.mcmod.cn/modlist.html",
            ProjectKind::Modpack => "https://www.mcmod.cn/modpack.html",
            _ => unreachable!("kind was validated by route"),
        })
    } else {
        Url::parse("https://search.mcmod.cn/s")
    }
    .map_err(|_| "MC百科检索地址无效".to_owned())?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs([
            ("key", query),
            ("filter", if kind == ProjectKind::Mod { "1" } else { "2" }),
            ("mold", "0"),
        ]);
    }
    url.query_pairs_mut().append_pair("page", &page.to_string());
    Ok(url)
}

fn parse_search_html(
    html: &str,
    source_url: &str,
    page: u32,
    kind: ProjectKind,
    browse: bool,
) -> Result<McmodSearchPage, String> {
    check_html_size(html)?;
    let document = Document::from(html);
    let (container, selector, title_selector, summary_selector) = if browse {
        (
            ".modlist-list-frame",
            ".modlist-list-frame .modlist-block",
            ".title .name > a[href]",
            ".intro-content",
        )
    } else {
        (
            ".search-result",
            ".search-result-list > .result-item",
            ".head > a[href]",
            ".body",
        )
    };
    if document.select(container).is_empty() {
        return Err("MC百科页面结构无法识别，可能暂时不可用或要求验证".into());
    }
    let items = document.select(selector);
    if items.length() > MAX_HITS {
        return Err("MC百科返回的项目数量超过上限".into());
    }
    let mut hits = Vec::new();
    let mut seen = BTreeSet::new();
    for item in items.iter() {
        let titles = item.select(title_selector);
        let mut candidates = Vec::new();
        for anchor in titles.iter() {
            let Some(raw_url) = anchor.attr("href") else {
                continue;
            };
            let Some((project, url)) = mcmod_reference(raw_url.as_ref(), kind) else {
                continue;
            };
            let title = plain_text(anchor.text().as_ref(), 256);
            if !title.is_empty() {
                candidates.push((project, url, title));
            }
        }
        if candidates.len() != 1 {
            return Err("MC百科结果标题或项目链接无法识别".into());
        }
        let (project, url, title) = candidates.remove(0);
        if seen.insert(project.clone()) {
            let summary = plain_text(item.select(summary_selector).text().as_ref(), 1024);
            let icon_url = item
                .select(".cover img")
                .attr("src")
                .and_then(|url| image_url(url.as_ref()));
            hits.push(McmodSearchHit {
                project,
                title,
                summary,
                url,
                icon_url,
            });
        }
    }
    if hits.is_empty() && !browse {
        let message = document.select(".search-result").text();
        if !["0 条结果", "没有找到", "未找到", "没有搜索到"]
            .iter()
            .any(|known| message.contains(known))
        {
            return Err("MC百科搜索页面未返回可识别结果".into());
        }
    }
    let has_more = document
        .select(".pagination a[data-page]")
        .iter()
        .any(|link| {
            link.attr("data-page")
                .and_then(|value| value.parse::<u32>().ok())
                .is_some_and(|next| next > page)
        });
    Ok(McmodSearchPage {
        hits,
        page,
        has_more,
        source_url: source_url.into(),
        attribution: ATTRIBUTION,
    })
}

fn parse_detail_html(html: &str, project: ProjectKey, url: &str) -> Result<McmodDetail, String> {
    check_html_size(html)?;
    let document = Document::from(html);
    let titles = document.select(".class-title h3");
    if titles.length() != 1 {
        return Err("MC百科词条页面结构无法识别，可能暂时不可用或要求验证".into());
    }
    let title = plain_text(titles.text().as_ref(), 256);
    if title.is_empty() {
        return Err("MC百科词条缺少标题".into());
    }
    let secondary_title = plain_text(document.select(".class-title h4").text().as_ref(), 256);
    let summary = document
        .select("meta[name=description]")
        .attr("content")
        .map(|text| plain_text(text.as_ref(), 1024))
        .unwrap_or_default();
    let links = document.select(".class-info .common-link-frame a[href]");
    let mut aliases = Vec::new();
    let mut seen = BTreeSet::new();
    for anchor in links.iter() {
        let Some(href) = anchor.attr("href") else {
            continue;
        };
        let Some(target) = related_project_url(href.as_ref()) else {
            continue;
        };
        if seen.insert(target.clone()) {
            if aliases.len() >= MAX_LINKS {
                return Err("MC百科相关项目链接数量超过上限".into());
            }
            aliases.push(AliasEvidence {
                target: AliasTarget::Url(target),
                evidence_url: url.into(),
            });
        }
    }
    Ok(McmodDetail {
        project,
        title,
        secondary_title: (!secondary_title.is_empty()).then_some(secondary_title),
        summary,
        url: url.into(),
        aliases,
        attribution: ATTRIBUTION,
    })
}

fn check_html_size(html: &str) -> Result<(), String> {
    if html.len() > MAX_RESPONSE_BYTES {
        Err("MC百科响应超过大小上限".into())
    } else {
        Ok(())
    }
}

fn absolute_mcmod_url(raw: &str) -> Option<Url> {
    if raw.len() > 4096 || raw.chars().any(char::is_control) {
        return None;
    }
    Url::parse("https://www.mcmod.cn/").ok()?.join(raw).ok()
}

fn mcmod_reference(raw: &str, kind: ProjectKind) -> Option<(ProjectKey, String)> {
    let url = canonical_project_url(absolute_mcmod_url(raw)?.as_str())?;
    let route = route(kind).ok()?;
    let id = url
        .strip_prefix(&format!("https://www.mcmod.cn/{route}/"))?
        .strip_suffix(".html")?;
    Some((ProjectKey::new(CommunitySource::Mcmod, kind, id).ok()?, url))
}

/// `link.mcmod.cn/target` embeds the destination in base64. Only that observed
/// encoding is accepted; named redirects and other sites are never requested.
fn related_project_url(raw: &str) -> Option<String> {
    let url = absolute_mcmod_url(raw)?;
    let decoded;
    let destination = if url.host_str()? == "link.mcmod.cn" {
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let encoded = url.path().strip_prefix("/target/")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(encoded))
            .ok()?;
        decoded = String::from_utf8(bytes).ok()?;
        decoded.as_str()
    } else {
        url.as_str()
    };
    let canonical = canonical_project_url(destination)?;
    (canonical.starts_with("https://modrinth.com/")
        || canonical.starts_with("https://www.curseforge.com/"))
    .then_some(canonical)
}

fn image_url(raw: &str) -> Option<String> {
    let mut url = absolute_mcmod_url(raw)?;
    if !matches!(url.host_str()?, "i.mcmod.cn" | "www.mcmod.cn")
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    url.set_scheme("https").ok()?;
    Some(url.into())
}

fn plain_text(value: &str, max_chars: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

#[cfg(test)]
#[path = "community_mcmod/tests.rs"]
mod tests;
