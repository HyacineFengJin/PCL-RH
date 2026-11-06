use super::*;

#[test]
fn pacing_clock_has_no_initial_credit_even_after_scheduler_setup_delay() {
    let mut clock = PayloadClock::default();
    let first = Instant::now() + Duration::from_secs(60);
    let service = Duration::from_millis(64);
    assert_eq!(clock.reserve(first, service).unwrap(), first + service);
    assert_eq!(clock.reserve(first, service).unwrap(), first + service * 2);
}

#[test]
fn pacing_clock_recovers_late_ticks_but_never_banks_more_than_two_milliseconds() {
    let start = Instant::now();
    for service in [
        Duration::from_micros(125),
        Duration::from_millis(1),
        Duration::from_millis(64),
    ] {
        let mut clock = PayloadClock::default();
        clock.reserve(start, service).unwrap();
        let late = start + Duration::from_secs(60);
        let mut immediately_funded = Duration::ZERO;
        loop {
            let deadline = clock.reserve(late, service).unwrap();
            if deadline > late {
                break;
            }
            immediately_funded += service;
            assert!(immediately_funded <= MAX_PACING_CATCHUP);
        }
        assert_eq!(
            immediately_funded.as_nanos(),
            MAX_PACING_CATCHUP.as_nanos() / service.as_nanos() * service.as_nanos()
        );
    }
}

#[test]
fn pacing_clock_retains_queued_byte_debt_across_a_late_reader() {
    let start = Instant::now();
    let mut clock = PayloadClock::default();
    let service = Duration::from_millis(64);
    for index in 1..=4 {
        assert_eq!(
            clock.reserve(start, service).unwrap(),
            start + service * index
        );
    }
    let observed = start + Duration::from_millis(100);
    assert_eq!(
        clock.reserve(observed, service).unwrap(),
        start + service * 5
    );
}

#[tokio::test]
async fn already_funded_payload_completes_without_registering_another_timer() {
    use std::{future::Future, task::Poll};
    let scheduler = scheduler(1, 1024);
    scheduler.next_payload.lock().unwrap().next = Some(Instant::now() - Duration::from_micros(100));
    let cancel = AtomicBool::new(false);
    let mut transfer = Box::pin(scheduler.throttle(DOWNLOAD_SLICE_BYTES as u64, &cancel));
    let first_poll =
        std::future::poll_fn(|context| Poll::Ready(transfer.as_mut().poll(context))).await;
    assert!(matches!(first_poll, Poll::Ready(Ok(()))));
    assert_eq!(
        scheduler.status().paced_payload_bytes,
        DOWNLOAD_SLICE_BYTES as u64
    );
}

fn scheduler(concurrent: u16, rate: u32) -> Arc<DownloadScheduler> {
    DownloadScheduler::new(DownloadPolicy {
        max_concurrent_transfers: concurrent,
        total_rate_limit_mib_per_second: rate,
        forbid_cross_root_cache_copy: false,
    })
    .unwrap()
}

#[test]
fn download_policy_bounds_and_independent_snapshots() {
    for (concurrent, rate) in [(0, 0), (65, 0), (4, 1025)] {
        assert!(DownloadScheduler::new(DownloadPolicy {
            max_concurrent_transfers: concurrent,
            total_rate_limit_mib_per_second: rate,
            forbid_cross_root_cache_copy: false
        })
        .is_err());
    }
    let old = scheduler(4, 0);
    let new = scheduler(1, 1024);
    assert_eq!(old.policy().max_concurrent_transfers, 4);
    assert_eq!(old.policy().total_rate_limit_mib_per_second, 0);
    assert_eq!(new.policy().max_concurrent_transfers, 1);
}

#[tokio::test]
async fn real_admission_never_exceeds_configured_active_transfers() {
    let scheduler = scheduler(2, 0);
    let cancel = Arc::new(AtomicBool::new(false));
    let maximum = Arc::new(AtomicU64::new(0));
    let mut jobs = Vec::new();
    for _ in 0..10 {
        let scheduler = scheduler.clone();
        let cancel = cancel.clone();
        let maximum = maximum.clone();
        jobs.push(tokio::spawn(async move {
            let _permit = scheduler.acquire(&cancel).await.unwrap();
            maximum.fetch_max(scheduler.status().active_transfers, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }));
    }
    for job in jobs {
        job.await.unwrap();
    }
    assert_eq!(maximum.load(Ordering::Relaxed), 2);
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
}

#[tokio::test]
async fn queued_cancellation_drops_admission_without_leaking_a_slot() {
    let scheduler = scheduler(1, 0);
    let cancel = AtomicBool::new(false);
    let held = scheduler.acquire(&cancel).await.unwrap();
    let waiting = scheduler.acquire(&cancel);
    let cancellation = async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(scheduler.status().queued_transfers, 1);
        cancel.store(true, Ordering::Release);
    };
    let (result, ()) = tokio::join!(waiting, cancellation);
    assert!(result.is_err());
    assert_eq!(scheduler.status().active_transfers, 1);
    assert_eq!(scheduler.status().queued_transfers, 0);
    drop(held);
    assert_eq!(scheduler.status().active_transfers, 0);
}

#[tokio::test]
async fn concurrent_consumers_share_total_rate_not_a_rate_per_file() {
    let scheduler = scheduler(4, 4);
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let (first, second) = tokio::join!(
        scheduler.throttle(512 * 1024, &cancel),
        scheduler.throttle(512 * 1024, &cancel)
    );
    first.unwrap();
    second.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(245));
    assert_eq!(scheduler.status().paced_payload_bytes, 1024 * 1024);
}

#[tokio::test]
async fn blocking_and_async_readers_consume_the_same_byte_budget() {
    let scheduler = scheduler(2, 1);
    let blocked = scheduler.clone();
    let start = Instant::now();
    let worker = std::thread::spawn(move || {
        let cancel = AtomicBool::new(false);
        let _permit = blocked.acquire_blocking(&cancel).unwrap();
        blocked.throttle_blocking(128 * 1024, &cancel).unwrap();
    });
    let cancel = AtomicBool::new(false);
    let _permit = scheduler.acquire(&cancel).await.unwrap();
    scheduler.throttle(128 * 1024, &cancel).await.unwrap();
    worker.join().unwrap();
    assert!(start.elapsed() >= Duration::from_millis(245));
    assert_eq!(scheduler.status().paced_payload_bytes, 256 * 1024);
}

#[tokio::test]
async fn waiting_for_rate_is_cancellable_and_unlimited_has_no_artificial_delay() {
    let scheduler = scheduler(1, 1);
    let cancel = AtomicBool::new(false);
    let waiting = scheduler.throttle(64 * 1024, &cancel);
    let cancellation = async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        cancel.store(true, Ordering::Release);
    };
    let (result, ()) = tokio::join!(waiting, cancellation);
    assert!(result.is_err());
    assert_eq!(scheduler.status().paced_payload_bytes, 0);
    let unlimited = super::tests::scheduler(1, 0);
    let cancel = AtomicBool::new(false);
    unlimited.throttle(8 * 1024 * 1024, &cancel).await.unwrap();
    assert_eq!(unlimited.status().paced_payload_bytes, 8 * 1024 * 1024);
}
