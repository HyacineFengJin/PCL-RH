use super::*;
use crate::community_identity::{CommunitySource, ProjectKind};

fn cached_detail(id: &str) -> McmodDetail {
    McmodDetail {
        project: ProjectKey::new(CommunitySource::Mcmod, ProjectKind::Mod, id).unwrap(),
        title: "Synthetic".into(),
        secondary_title: None,
        summary: "Synthetic".into(),
        url: format!("https://www.mcmod.cn/class/{id}.html"),
        aliases: Vec::new(),
        attribution: "MC百科",
    }
}

#[test]
fn detail_cache_expires_and_evicts_oldest_entries_without_extending_reads() {
    let mut cache = DetailCache {
        network: pcl_network::snapshot(),
        entries: BTreeMap::new(),
    };
    let start = Instant::now();
    cache.insert(cached_detail("1"), start);
    for number in 2..=DETAIL_CACHE_SIZE + 1 {
        cache.insert(
            cached_detail(&number.to_string()),
            start + Duration::from_millis(number as u64),
        );
    }
    assert_eq!(cache.entries.len(), DETAIL_CACHE_SIZE);
    assert!(!cache.entries.contains_key(&cached_detail("1").project));
    cache.prune(start + DETAIL_CACHE_TTL + Duration::from_secs(1));
    assert!(cache.entries.is_empty());
}

#[test]
fn detail_cache_is_partitioned_by_exact_network_policy_snapshot() {
    let factory = || {
        Arc::new(
            pcl_network::ClientFactory::new(pcl_network::Policy {
                proxy: pcl_network::ProxyPolicy::None,
                dns: pcl_network::DnsPolicy::System,
            })
            .unwrap(),
        )
    };
    let first = factory();
    let second = factory();
    with_detail_cache(&first, |cache, now| cache.insert(cached_detail("123"), now));
    assert_eq!(with_detail_cache(&first, |cache, _| cache.entries.len()), 1);
    assert_eq!(
        with_detail_cache(&second, |cache, _| cache.entries.len()),
        0
    );
}

#[derive(Default)]
struct ReadStats {
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    calls: std::sync::atomic::AtomicUsize,
}
struct ActiveRead(Arc<ReadStats>);
impl ActiveRead {
    fn new(stats: Arc<ReadStats>) -> Self {
        use std::sync::atomic::Ordering::SeqCst;
        stats.calls.fetch_add(1, SeqCst);
        let active = stats.active.fetch_add(1, SeqCst) + 1;
        stats.peak.fetch_max(active, SeqCst);
        Self(stats)
    }
}
impl Drop for ActiveRead {
    fn drop(&mut self) {
        self.0
            .active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
fn pending(count: usize) -> Vec<ProjectKey> {
    (1..=count)
        .map(|id| cached_detail(&id.to_string()).project)
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn link_verification_limits_total_requests_and_concurrency() {
    use std::sync::atomic::Ordering::SeqCst;
    let stats = Arc::new(ReadStats::default());
    let reader_stats = stats.clone();
    let result = read_pending(pending(40), Duration::from_secs(1), move |project| {
        let stats = reader_stats.clone();
        async move {
            let _active = ActiveRead::new(stats);
            tokio::time::sleep(Duration::from_millis(2)).await;
            Ok(cached_detail(&project.id))
        }
    })
    .await;
    assert_eq!(result.details.len(), MAX_DETAIL_READS);
    assert_eq!(stats.calls.load(SeqCst), MAX_DETAIL_READS);
    assert_eq!(stats.peak.load(SeqCst), DETAIL_PARALLELISM);
    assert_eq!(stats.active.load(SeqCst), 0);
    assert!(!result.timed_out);
}

#[tokio::test(flavor = "current_thread")]
async fn verification_deadline_keeps_completed_evidence_and_aborts_remaining_reads() {
    use std::sync::atomic::Ordering::SeqCst;
    let stats = Arc::new(ReadStats::default());
    let reader_stats = stats.clone();
    let started = Instant::now();
    let result = read_pending(pending(12), Duration::from_millis(20), move |project| {
        let stats = reader_stats.clone();
        async move {
            let _active = ActiveRead::new(stats);
            tokio::time::sleep(if project.id == "1" {
                Duration::from_millis(1)
            } else {
                Duration::from_secs(2)
            })
            .await;
            Ok(cached_detail(&project.id))
        }
    })
    .await;
    assert!(result.timed_out);
    assert_eq!(result.details.len(), 1);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(stats.calls.load(SeqCst) <= 4);
    assert_eq!(stats.active.load(SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn one_failed_detail_does_not_discard_other_evidence() {
    let result = read_pending(pending(6), Duration::from_secs(1), |project| async move {
        if project.id == "2" {
            Err("synthetic HTTP 403".into())
        } else {
            Ok(cached_detail(&project.id))
        }
    })
    .await;
    assert_eq!(result.details.len(), 5);
    assert_eq!(result.failed, 1);
    assert_eq!(result.first_error.as_deref(), Some("synthetic HTTP 403"));
    assert!(!result.timed_out);
}
