//! Cache one bounded release window, then apply the user's display filter. The
//! cache is tied to networking-policy identity and uses monotonic expiry; mode
//! None returns before touching the factory, cache, lock or network provider.
use super::provider::{Failure, GitHub, Provider, Response, MAX_BYTES, MAX_RELEASES};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnnouncementScope {
    All,
    Important,
    None,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AnnouncementState {
    Disabled,
    Ready,
    Empty,
    RateLimited,
    Unavailable,
}
use AnnouncementState as State;
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Announcement {
    pub id: String,
    pub title: String,
    pub text: String,
    pub url: String,
    pub published_at: Option<String>,
    pub prerelease: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementView {
    pub state: AnnouncementState,
    pub scope: AnnouncementScope,
    pub items: Vec<Announcement>,
    pub fetched_at: Option<u64>,
    pub retry_at: Option<u64>,
    pub cached: bool,
    pub truncated: bool,
    pub message: Option<String>,
}
impl AnnouncementView {
    pub(crate) fn disabled() -> Self {
        Self {
            state: State::Disabled,
            scope: AnnouncementScope::None,
            items: vec![],
            fetched_at: None,
            retry_at: None,
            cached: false,
            truncated: false,
            message: None,
        }
    }
    fn unavailable(message: &str) -> Self {
        Self {
            state: State::Unavailable,
            scope: AnnouncementScope::All,
            items: vec![],
            fetched_at: Some(now()),
            retry_at: None,
            cached: false,
            truncated: false,
            message: Some(message.into()),
        }
    }
    fn filtered(mut self, scope: AnnouncementScope, cached: bool) -> Self {
        self.cached = cached;
        self.scope = scope;
        if scope == AnnouncementScope::Important {
            self.items.retain(|row| !row.prerelease);
        }
        if matches!(self.state, State::Ready | State::Empty) {
            self.state = if self.items.is_empty() {
                State::Empty
            } else {
                State::Ready
            };
            self.message = if self.items.is_empty() {
                Some(
                    match (scope, self.truncated) {
                        (AnnouncementScope::Important, true) => {
                            "近期发行中暂无稳定公告；较早公告未读取"
                        }
                        (AnnouncementScope::Important, false) => "本项目暂无稳定发行公告",
                        _ => "本项目暂无公开发行公告",
                    }
                    .into(),
                )
            } else if self.truncated {
                Some("只显示近期发行公告，较早内容或过长文字已省略".into())
            } else {
                None
            };
        }
        self
    }
}
struct Cached {
    network: Arc<pcl_network::ClientFactory>,
    at: Instant,
    view: AnnouncementView,
}
#[derive(Default)]
pub struct AnnouncementStore {
    cache: Mutex<Option<Cached>>,
    fetch: tokio::sync::Mutex<()>,
}
impl AnnouncementStore {
    pub async fn read(&self, scope: AnnouncementScope, refresh: bool) -> AnnouncementView {
        if scope == AnnouncementScope::None {
            return AnnouncementView::disabled();
        }
        let network = pcl_network::snapshot();
        let provider = GitHub::new(network.clone());
        self.read_with(scope, refresh, network, &provider).await
    }
    pub(crate) async fn read_with(
        &self,
        scope: AnnouncementScope,
        refresh: bool,
        network: Arc<pcl_network::ClientFactory>,
        provider: &dyn Provider,
    ) -> AnnouncementView {
        if scope == AnnouncementScope::None {
            return AnnouncementView::disabled();
        }
        let _fetch = self.fetch.lock().await;
        if !refresh {
            let cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(cache) = cache.as_ref() {
                let ttl = if matches!(cache.view.state, State::Unavailable) {
                    Duration::from_secs(20)
                } else {
                    Duration::from_secs(60)
                };
                if Arc::ptr_eq(&network, &cache.network) && cache.at.elapsed() < ttl {
                    return cache.view.clone().filtered(scope, true);
                }
            }
        }
        let view = match tokio::time::timeout(Duration::from_secs(20), provider.fetch()).await {
            Ok(Ok(response)) => normalize(response),
            Ok(Err(Failure::TooLarge)) => AnnouncementView::unavailable("发行公告响应超过大小上限"),
            Ok(Err(Failure::Protocol)) => AnnouncementView::unavailable("发行公告响应跳转已拒绝"),
            _ => AnnouncementView::unavailable("暂时无法读取本项目发行公告，请稍后重试"),
        };
        *self.cache.lock().unwrap_or_else(|error| error.into_inner()) = Some(Cached {
            network,
            at: Instant::now(),
            view: view.clone(),
        });
        view.filtered(scope, false)
    }
}
#[derive(Deserialize)]
struct Release {
    id: u64,
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn bounded(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}
fn release_url(tag: &str) -> Option<String> {
    if tag.is_empty()
        || tag.len() > 256
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
        || tag
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return None;
    }
    let mut url =
        reqwest::Url::parse("https://github.com/HyacineFengJin/PCL-RH/releases/tag/").ok()?;
    url.path_segments_mut().ok()?.pop_if_empty().push(tag);
    Some(url.into())
}
pub(crate) fn normalize(response: Response) -> AnnouncementView {
    if response.body.len() > MAX_BYTES {
        return AnnouncementView::unavailable("发行公告响应超过大小上限");
    }
    if response.status == 429
        || response.status == 403
            && (response.headers.remaining == Some(0) || response.headers.retry_seconds.is_some())
    {
        let clock = now();
        let retry_at = response
            .headers
            .retry_seconds
            .filter(|seconds| *seconds <= 86400)
            .map(|seconds| clock.saturating_add(seconds))
            .or_else(|| {
                response
                    .headers
                    .reset_at
                    .filter(|time| *time >= clock && *time <= clock.saturating_add(86400))
            });
        return AnnouncementView {
            state: State::RateLimited,
            scope: AnnouncementScope::All,
            items: vec![],
            fetched_at: Some(clock),
            retry_at,
            cached: false,
            truncated: false,
            message: Some("GitHub 公共 API 暂时达到请求限制，请稍后重试".into()),
        };
    }
    if response.status != 200 {
        return AnnouncementView::unavailable(if response.status == 404 {
            "未找到本项目的公开发行公告来源"
        } else {
            "发行公告服务暂时拒绝请求，请稍后重试"
        });
    }
    let releases: Vec<Release> = match serde_json::from_slice(&response.body) {
        Ok(rows) => rows,
        Err(_) => return AnnouncementView::unavailable("发行公告响应格式无效"),
    };
    if releases.len() > MAX_RELEASES {
        return AnnouncementView::unavailable("发行公告数量超过本次读取上限");
    }
    let mut items = Vec::new();
    let mut truncated = response.headers.has_next;
    let mut remaining = 256 * 1024;
    let mut seen = std::collections::HashSet::new();
    for release in releases {
        if release.draft {
            continue;
        }
        let Some(url) = release_url(&release.tag_name) else {
            truncated = true;
            continue;
        };
        if release.id == 0 || !seen.insert(release.id) {
            return AnnouncementView::unavailable("发行公告身份无效或重复");
        }
        let (title, cut) = bounded(
            release
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(&release.tag_name),
            512,
        );
        truncated |= cut;
        let (text, cut) = bounded(
            release.body.as_deref().unwrap_or_default(),
            (16 * 1024).min(remaining),
        );
        remaining -= text.len();
        truncated |= cut;
        let published_at = release
            .published_at
            .as_deref()
            .filter(|text| text.len() <= 64)
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time.with_timezone(&chrono::Utc).to_rfc3339());
        items.push(Announcement {
            id: release.id.to_string(),
            title,
            text,
            url,
            published_at,
            prerelease: release.prerelease,
        });
    }
    items.sort_by(|a, b| {
        b.published_at
            .cmp(&a.published_at)
            .then_with(|| b.id.parse::<u64>().ok().cmp(&a.id.parse::<u64>().ok()))
    });
    AnnouncementView {
        state: State::Ready,
        scope: AnnouncementScope::All,
        items,
        fetched_at: Some(now()),
        retry_at: None,
        cached: false,
        truncated,
        message: None,
    }
}
