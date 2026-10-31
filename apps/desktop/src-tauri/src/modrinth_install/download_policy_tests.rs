//! Local HTTP and anonymous-descriptor fixtures exercise the same queue/body
//! path used by resource installs. No official URL, game root or singleton
//! policy is changed by these tests.
use super::{provider::HttpProvider, target::Dir, tests, transfer, Result, CANCELLED};
use pcl_network::{DownloadPolicy, DownloadScheduler};
use std::{
    collections::VecDeque,
    future::Future,
    io::{Read, Write},
    net::TcpListener,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
type Job<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;
struct Fixture {
    _temporary: tempfile::TempDir,
    dir: Dir,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = pcl_core::retained_fs::Directory::open_absolute(
            &temporary.path().canonicalize().unwrap(),
        )
        .unwrap();
        root.create_private(
            &pcl_core::retained_fs::Leaf::new("staging").unwrap(),
            pcl_core::retained_fs::Traversal::SameFilesystem,
        )
        .unwrap();
        let dir = Dir::open(&temporary.path().join("staging").canonicalize().unwrap())
            .unwrap()
            .with_input_sources()
            .unwrap();
        Self {
            _temporary: temporary,
            dir,
        }
    }
    fn assert_no_named_partial(&self) {
        super::test_support::assert_staging(&self.dir, 0);
    }
}

struct LocalServer {
    url: String,
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<()>>,
}
impl LocalServer {
    fn new(body: &[u8]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/fixture", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let signal = stop.clone();
        let peak_counter = peak.clone();
        let request_counter = requests.clone();
        let body = Arc::new(body.to_owned());
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            while !signal.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let body = body.clone();
                        let active = active.clone();
                        let peak = peak_counter.clone();
                        let requests = request_counter.clone();
                        workers.push(thread::spawn(move || {
                            // BSD/Windows accepted sockets can inherit the
                            // listener's nonblocking mode. This worker uses
                            // bounded blocking read/write_all, not a reactor.
                            socket.set_nonblocking(false).unwrap();
                            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                            socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
                            let mut request = [0; 2048];
                            let _ = socket.read(&mut request);
                            requests.fetch_add(1, Ordering::Relaxed);
                            let count = active.fetch_add(1, Ordering::Relaxed) + 1;
                            peak.fetch_max(count, Ordering::Relaxed);
                            // Delay headers so concurrent request lifetimes are
                            // observable independently of loopback write speed.
                            thread::sleep(Duration::from_millis(50));
                            let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                            let _ = socket.write_all(&body);
                            active.fetch_sub(1, Ordering::Relaxed);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            stop,
            peak,
            requests,
            worker: Some(worker),
        }
    }
}
impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn provider<'a>(cancel: &'a AtomicBool, scheduler: Arc<DownloadScheduler>) -> HttpProvider<'a> {
    let mut provider = HttpProvider::new(cancel)
        .unwrap()
        .with_download_policy(scheduler);
    provider.client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    provider
}
fn jobs<'a>(
    provider: &'a HttpProvider<'a>,
    fixture: &'a Fixture,
    cancel: &'a AtomicBool,
    url: &'a str,
    body: &'a [u8],
    count: usize,
) -> VecDeque<Job<'a, super::source::Input>> {
    (0..count)
        .map(|_| {
            // Source setup is not part of the transport being measured. Native
            // allocation flushes durable records and must not stall another
            // fixture socket while its bounded write deadline is running.
            let mut file = fixture.dir.allocate(body.len() as u64).unwrap();
            Box::pin(async move {
                transfer::response_into_file(
                    provider,
                    provider.client.get(url),
                    body.len() as u64,
                    &tests::sha(body),
                    &mut file,
                    cancel,
                    |_| {},
                )
                .await?;
                Ok(file)
            }) as Job<'a, super::source::Input>
        })
        .collect()
}
#[test]
fn configured_queue_runs_real_http_with_one_or_three_connections() {
    for limit in [1, 3] {
        let fixture = Fixture::new();
        let body = vec![7; 256 * 1024];
        let server = LocalServer::new(&body);
        let cancel = AtomicBool::new(false);
        let scheduler = DownloadScheduler::new(DownloadPolicy {
            max_concurrent_transfers: limit,
            ..DownloadPolicy::default()
        })
        .unwrap();
        let provider = provider(&cancel, scheduler.clone());
        let files = tests::run(transfer::collect_bounded(
            jobs(&provider, &fixture, &cancel, &server.url, &body, 6),
            limit as usize,
        ))
        .unwrap();
        assert_eq!(files.len(), 6);
        assert_eq!(server.requests.load(Ordering::Relaxed), 6);
        assert_eq!(server.peak.load(Ordering::Relaxed), limit as usize);
        assert_eq!(scheduler.status().active_transfers, 0);
        assert_eq!(scheduler.status().queued_transfers, 0);
        assert_eq!(
            provider.network_bytes.load(Ordering::Relaxed),
            (6 * body.len()) as u64
        );
        for file in files {
            assert_eq!(file.metadata().unwrap().len(), body.len() as u64);
        }
        fixture.assert_no_named_partial();
    }
}
#[test]
fn concurrent_http_files_consume_one_total_payload_budget() {
    let fixture = Fixture::new();
    let body = vec![7; 512 * 1024];
    let server = LocalServer::new(&body);
    let cancel = AtomicBool::new(false);
    let scheduler = DownloadScheduler::new(DownloadPolicy {
        max_concurrent_transfers: 2,
        total_rate_limit_mib_per_second: 1,
        ..DownloadPolicy::default()
    })
    .unwrap();
    let provider = provider(&cancel, scheduler.clone());
    let start = Instant::now();
    let files = tests::run(transfer::collect_bounded(
        jobs(&provider, &fixture, &cancel, &server.url, &body, 2),
        2,
    ))
    .unwrap();
    assert!(start.elapsed() >= Duration::from_millis(995));
    assert_eq!(server.peak.load(Ordering::Relaxed), 2);
    assert_eq!(scheduler.status().paced_payload_bytes, 1024 * 1024);
    assert_eq!(provider.network_bytes.load(Ordering::Relaxed), 1024 * 1024);
    assert_eq!(files.len(), 2);
    drop(files);
    fixture.assert_no_named_partial();
}
struct OwnedAnonymous {
    file: super::source::Input,
    live: Arc<AtomicUsize>,
}
impl Drop for OwnedAnonymous {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::Relaxed);
    }
}
#[test]
fn one_bad_hash_drops_ready_and_stalled_files_before_queue_returns() {
    let fixture = Fixture::new();
    let cancel = AtomicBool::new(false);
    let scheduler = DownloadScheduler::new(DownloadPolicy {
        max_concurrent_transfers: 2,
        ..DownloadPolicy::default()
    })
    .unwrap();
    let provider = provider(&cancel, scheduler.clone());
    let live = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    let mut jobs = VecDeque::<Job<'_, OwnedAnonymous>>::new();
    let ready = Arc::new(AtomicBool::new(false));
    let stalled = Arc::new(AtomicBool::new(false));
    for (body, delay) in [(b"good", 0), (b"bad!", 100)] {
        let response = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".as_slice(),
            body,
        ]
        .concat();
        let (url, worker) = tests::server(&response, Duration::from_millis(delay));
        workers.push(worker);
        let file = fixture.dir.allocate(4).unwrap();
        live.fetch_add(1, Ordering::Relaxed);
        let live = live.clone();
        let provider = &provider;
        let cancel = &cancel;
        let ready = ready.clone();
        let stalled = stalled.clone();
        let mut owned = OwnedAnonymous { file, live };
        jobs.push_back(Box::pin(async move {
            let result = transfer::response_into_file(
                provider,
                provider.client.get(url),
                4,
                &tests::sha(b"good"),
                &mut owned.file,
                cancel,
                |_| {},
            )
            .await;
            if body == b"good" {
                result?;
                ready.store(true, Ordering::SeqCst);
            } else {
                // Make the case deterministic: rejection happens only after
                // one verified result and a polled, non-completing owner exist.
                while !ready.load(Ordering::SeqCst) || !stalled.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                result?;
            }
            Ok(owned)
        }));
    }
    let file = fixture.dir.allocate(4).unwrap();
    live.fetch_add(1, Ordering::Relaxed);
    let owned = OwnedAnonymous {
        file,
        live: live.clone(),
    };
    let started = stalled.clone();
    jobs.push_back(Box::pin(async move {
        started.store(true, Ordering::SeqCst);
        std::future::pending::<()>().await;
        Ok(owned)
    }));
    let result = tests::run(async {
        tokio::time::timeout(Duration::from_secs(10), transfer::collect_bounded(jobs, 3))
            .await
            .expect("queue waited for a non-completing owner after rejection")
    });
    let error = match result {
        Ok(_) => panic!("bad hash must reject entire owned batch"),
        Err(error) => error,
    };
    assert!(error.contains("SHA512"));
    assert!(ready.load(Ordering::SeqCst));
    assert!(stalled.load(Ordering::SeqCst));
    assert_eq!(live.load(Ordering::Relaxed), 0);
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
    fixture.assert_no_named_partial();
    for worker in workers {
        worker.join().unwrap();
    }
}
#[test]
fn cancellation_during_queue_and_pacing_releases_every_owned_file() {
    let fixture = Fixture::new();
    let body = vec![7; 1024 * 1024];
    let server = LocalServer::new(&body);
    let cancel = AtomicBool::new(false);
    let scheduler = DownloadScheduler::new(DownloadPolicy {
        max_concurrent_transfers: 1,
        total_rate_limit_mib_per_second: 1,
        ..DownloadPolicy::default()
    })
    .unwrap();
    let provider = provider(&cancel, scheduler.clone());
    let live = Arc::new(AtomicUsize::new(0));
    let responded = AtomicUsize::new(0);
    let stalled = AtomicUsize::new(0);
    let cancelled_at = std::cell::Cell::new(None::<Instant>);
    let response_elapsed = std::cell::Cell::new(None::<Duration>);
    let hash = tests::sha(&body);
    let mut jobs = VecDeque::<Job<'_, OwnedAnonymous>>::new();
    for index in 0..3 {
        // All three real source leases exist before any HTTP future is polled.
        // Durable native allocation is not transport cancellation latency.
        let file = fixture.dir.allocate(body.len() as u64).unwrap();
        live.fetch_add(1, Ordering::Relaxed);
        let mut owned = OwnedAnonymous {
            file,
            live: live.clone(),
        };
        let provider = &provider;
        let cancel = &cancel;
        let url = &server.url;
        let body = &body;
        let hash = &hash;
        let responded = &responded;
        let stalled = &stalled;
        let cancelled_at = &cancelled_at;
        let response_elapsed = &response_elapsed;
        let live = &live;
        jobs.push_back(Box::pin(async move {
            let error = match transfer::response_into_file(
                provider,
                provider.client.get(url),
                body.len() as u64,
                hash,
                &mut owned.file,
                cancel,
                |_| {},
            )
            .await
            {
                Ok(()) => panic!("cancelled batch must not produce verified files"),
                Err(error) => error,
            };
            assert_eq!(error, CANCELLED);
            responded.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                // Both queued transports must observe cancellation before this
                // error can release any owner or retire its durable ticket.
                while stalled.load(Ordering::SeqCst) != 2 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(responded.load(Ordering::SeqCst), 3);
                assert_eq!(live.load(Ordering::Relaxed), 3);
                response_elapsed.set(Some(cancelled_at.get().unwrap().elapsed()));
                Err(error)
            } else {
                stalled.fetch_add(1, Ordering::SeqCst);
                // A collector that awaits peers after the first error never
                // completes: only dropping these futures releases their files.
                std::future::pending::<()>().await;
                Ok(owned)
            }
        }));
    }
    assert_eq!(live.load(Ordering::Relaxed), 3);
    let mut collecting = Box::pin(transfer::collect_bounded(jobs, 3));
    let result = tests::run(async {
        tokio::time::timeout(
            Duration::from_secs(10),
            std::future::poll_fn(|context| {
                let result = collecting.as_mut().poll(context);
                if result.is_pending() && !cancel.load(Ordering::Acquire) {
                    let status = scheduler.status();
                    // Inspect the collector only after all its jobs yielded.
                    // Received but unpaced bytes then prove the active HTTP
                    // future is inside throttle(), with two real slot waiters.
                    if status.active_transfers == 1
                        && status.queued_transfers == 2
                        && provider.network_bytes.load(Ordering::Relaxed)
                            > status.paced_payload_bytes
                    {
                        assert_eq!(server.requests.load(Ordering::Relaxed), 1);
                        assert_eq!(live.load(Ordering::Relaxed), 3);
                        cancelled_at.set(Some(Instant::now()));
                        cancel.store(true, Ordering::Release);
                    }
                }
                result
            }),
        )
        .await
        .expect("queue waited for non-completing owners after cancellation")
    });
    let error = match result {
        Ok(_) => panic!("cancelled batch must not produce verified files"),
        Err(error) => error,
    };
    assert_eq!(error, CANCELLED);
    // This bounds cancellation response before file Drop. Final native ticket
    // retirement may flush several durable records and is checked separately.
    assert!(response_elapsed.get().unwrap() < Duration::from_millis(400));
    assert_eq!(responded.load(Ordering::SeqCst), 3);
    assert_eq!(stalled.load(Ordering::SeqCst), 2);
    assert_eq!(live.load(Ordering::Relaxed), 0);
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
    assert!(scheduler.status().paced_payload_bytes < 1024 * 1024);
    fixture.assert_no_named_partial();
}
