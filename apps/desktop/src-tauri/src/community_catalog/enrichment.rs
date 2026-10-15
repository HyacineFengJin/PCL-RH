//! Limited encyclopedia link verification and a policy-scoped success cache.
//! The worker drains only the current search's small prefix and aborts remaining
//! requests at its deadline. Cached errors never suppress an explicit retry.
use crate::community_identity::ProjectKey;
use crate::community_mcmod::{McmodClient, McmodDetail, McmodSearchHit};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{task::JoinSet, time::timeout_at};

const MAX_DETAIL_READS: usize = 12;
const DETAIL_PARALLELISM: usize = 3;
const DETAIL_DEADLINE: Duration = Duration::from_secs(15);
const DETAIL_CACHE_SIZE: usize = 128;
const DETAIL_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

struct CachedDetail {
    fetched: Instant,
    detail: McmodDetail,
}
struct DetailCache {
    network: Arc<pcl_network::ClientFactory>,
    entries: BTreeMap<ProjectKey, CachedDetail>,
}

impl DetailCache {
    fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.fetched) < DETAIL_CACHE_TTL);
    }

    fn insert(&mut self, detail: McmodDetail, now: Instant) {
        self.entries.insert(
            detail.project.clone(),
            CachedDetail {
                fetched: now,
                detail,
            },
        );
        while self.entries.len() > DETAIL_CACHE_SIZE {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.fetched)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.entries.remove(&oldest);
        }
    }
}
fn detail_cache() -> &'static Mutex<Option<DetailCache>> {
    static CACHE: OnceLock<Mutex<Option<DetailCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}
fn with_detail_cache<T>(
    network: &Arc<pcl_network::ClientFactory>,
    operation: impl FnOnce(&mut DetailCache, Instant) -> T,
) -> T {
    let mut guard = detail_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard
        .as_ref()
        .is_none_or(|cache| !Arc::ptr_eq(&cache.network, network))
    {
        *guard = Some(DetailCache {
            network: network.clone(),
            entries: BTreeMap::new(),
        });
    }
    let now = Instant::now();
    let cache = guard.as_mut().unwrap();
    cache.prune(now);
    operation(cache, now)
}

pub(super) async fn enrich_mcmod(
    client: Arc<McmodClient>,
    network: &Arc<pcl_network::ClientFactory>,
    hits: &[McmodSearchHit],
) -> (BTreeMap<ProjectKey, McmodDetail>, Vec<String>) {
    let (mut details, pending) = with_detail_cache(network, |cache, _| {
        let mut details = BTreeMap::new();
        let mut pending = Vec::new();
        for hit in hits.iter().take(MAX_DETAIL_READS) {
            if let Some(entry) = cache.entries.get(&hit.project) {
                details.insert(hit.project.clone(), entry.detail.clone());
            } else {
                pending.push(hit.project.clone());
            }
        }
        (details, pending)
    });
    let outcome = read_pending(pending, DETAIL_DEADLINE, move |project| {
        let client = client.clone();
        async move { client.detail(&project.id, project.kind).await }
    })
    .await;
    with_detail_cache(network, |cache, now| {
        for detail in outcome.details {
            cache.insert(detail.clone(), now);
            details.insert(detail.project.clone(), detail);
        }
    });
    let mut warnings = Vec::new();
    if hits.len() > MAX_DETAIL_READS {
        warnings.push(format!(
            "本次仅核对前 {MAX_DETAIL_READS} 条 MC百科结果的跨站关联，其余保持独立"
        ));
    }
    if outcome.failed > 0 {
        warnings.push(format!(
            "MC百科部分关联详情读取失败（{} 条），未确认项目保持独立：{}",
            outcome.failed,
            outcome.first_error.unwrap_or_default()
        ));
    }
    if outcome.timed_out {
        warnings.push("MC百科关联核对达到 15 秒上限，未确认项目保持独立".into());
    }
    (details, warnings)
}

#[derive(Default)]
struct DetailReadOutcome {
    details: Vec<McmodDetail>,
    failed: usize,
    first_error: Option<String>,
    timed_out: bool,
}

/// The reader is injectable for synthetic scheduling tests. The limits and
/// cancellation ownership remain here, independent of the transport's timeout.
async fn read_pending<F, Fut>(
    pending: Vec<ProjectKey>,
    budget: Duration,
    read: F,
) -> DetailReadOutcome
where
    F: Fn(ProjectKey) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<McmodDetail, String>> + Send + 'static,
{
    let deadline = tokio::time::Instant::now() + budget;
    let mut pending = pending.into_iter().take(MAX_DETAIL_READS);
    let mut tasks = JoinSet::new();
    let spawn = |tasks: &mut JoinSet<_>, project: ProjectKey| {
        let read = read.clone();
        tasks.spawn(async move { read(project).await });
    };
    for project in pending.by_ref().take(DETAIL_PARALLELISM) {
        spawn(&mut tasks, project);
    }
    let mut outcome = DetailReadOutcome::default();
    while !tasks.is_empty() {
        match timeout_at(deadline, tasks.join_next()).await {
            Ok(Some(Ok(Ok(detail)))) => outcome.details.push(detail),
            Ok(Some(Ok(Err(error)))) => {
                outcome.failed += 1;
                outcome.first_error.get_or_insert(error);
            }
            Ok(Some(Err(error))) => {
                outcome.failed += 1;
                outcome
                    .first_error
                    .get_or_insert(format!("关联读取任务失败：{error}"));
            }
            Ok(None) => break,
            Err(_) => {
                outcome.timed_out = true;
                // Await cancellation of our own tasks before releasing the
                // operation, so timed-out queries do not keep fetching pages.
                tasks.shutdown().await;
                break;
            }
        }
        if let Some(project) = pending.next() {
            spawn(&mut tasks, project);
        }
    }
    outcome
}

#[cfg(test)]
#[path = "enrichment_tests.rs"]
mod tests;
