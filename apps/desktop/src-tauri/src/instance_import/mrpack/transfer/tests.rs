//! Logical HTTPS routing is replaced only at the last request construction
//! step under cfg(test). Every production declaration/Location check still
//! runs, so a blocked redirect cannot silently become a loopback request.
use super::*;
use crate::instance_import::Dir;
use pcl_network::{DownloadPolicy, DownloadScheduler};
use std::{
    fs,
    io::Read,
    net::{TcpListener, TcpStream},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::atomic::AtomicUsize,
    thread,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct AnonymousIdentity {
    device: u64,
    inode: u64,
    target: PathBuf,
}
impl AnonymousIdentity {
    fn of(file: &File) -> Self {
        let metadata = file.metadata().unwrap();
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            target: fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap(),
        }
    }
    fn assert_closed(&self) {
        // Other workspace tests may immediately reuse a closed FD number.
        // Inspect the owned inode instead, including its unique fixture target
        // so reuse of a deleted inode number in another fixture is also safe.
        // This also detects a leaked duplicate, not merely the original number.
        for entry in fs::read_dir("/proc/self/fd").unwrap() {
            let path = entry.unwrap().path();
            let target = match fs::read_link(&path) {
                Ok(target) => target,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => panic!("cannot inspect fixture descriptor: {error}"),
            };
            if target != self.target {
                continue;
            }
            match fs::metadata(&path) {
                Ok(metadata) => assert!(
                    metadata.dev() != self.device || metadata.ino() != self.inode,
                    "download retained its anonymous inode after cleanup"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("cannot inspect fixture inode: {error}"),
            }
        }
    }
}
struct Fixture {
    path: PathBuf,
    dir: Dir,
}
impl Fixture {
    fn new() -> Self {
        let repository = option_env!("PCL_MRPACK_TEST_REPOSITORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let path = repository
            .join("work/modpack-install-2026-10-06/mirrors/fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let dir = Dir::open(&path).unwrap();
        Self { path, dir }
    }
    fn file(&self) -> File {
        self.dir.anonymous().unwrap()
    }
    fn assert_empty(&self) {
        assert_eq!(fs::read_dir(&self.path).unwrap().count(), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir(&self.path).unwrap();
    }
}
#[derive(Clone)]
struct Response {
    status: u16,
    location: Option<String>,
    body: Vec<u8>,
    length: Option<u64>,
    headers_delay: Duration,
    stall: bool,
}
impl Response {
    fn body(body: &[u8]) -> Self {
        Self {
            status: 200,
            location: None,
            body: body.to_vec(),
            length: Some(body.len() as u64),
            headers_delay: Duration::ZERO,
            stall: false,
        }
    }
    fn redirect(location: &str) -> Self {
        Self {
            status: 302,
            location: Some(location.into()),
            ..Self::body(b"unread redirect body")
        }
    }
}
struct Server {
    base: reqwest::Url,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    peak: Arc<AtomicUsize>,
    closed_stalls: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(routes: impl IntoIterator<Item = (String, Response)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base =
            reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let peak = Arc::new(AtomicUsize::new(0));
        let closed_stalls = Arc::new(AtomicUsize::new(0));
        let signal = stop.clone();
        let received = requests.clone();
        let highest = peak.clone();
        let closed = closed_stalls.clone();
        let routes = Arc::new(routes.into_iter().collect::<BTreeMap<_, _>>());
        let active = Arc::new(AtomicUsize::new(0));
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            while !signal.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let routes = routes.clone();
                        let received = received.clone();
                        let active = active.clone();
                        let highest = highest.clone();
                        let signal = signal.clone();
                        let closed = closed.clone();
                        workers.push(thread::spawn(move || {
                            serve(
                                socket, &routes, &received, &active, &highest, &signal, &closed,
                            );
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
            base,
            stop,
            requests,
            peak,
            closed_stalls,
            worker: Some(worker),
        }
    }
    fn paths(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.split_whitespace().nth(1).unwrap().to_owned())
            .collect()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn serve(
    mut socket: TcpStream,
    routes: &BTreeMap<String, Response>,
    requests: &Mutex<Vec<String>>,
    active: &AtomicUsize,
    peak: &AtomicUsize,
    stop: &AtomicBool,
    closed: &AtomicUsize,
) {
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut request = Vec::new();
    while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
        let mut bytes = [0; 1024];
        match socket.read(&mut bytes) {
            Ok(0) | Err(_) => return,
            Ok(length) => request.extend_from_slice(&bytes[..length]),
        }
    }
    let request = String::from_utf8(request).unwrap();
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap()
        .split('?')
        .next()
        .unwrap();
    let response = routes.get(path).cloned().unwrap_or_else(|| Response {
        status: 404,
        ..Response::body(b"")
    });
    requests.lock().unwrap().push(request);
    peak.fetch_max(
        active.fetch_add(1, Ordering::Relaxed) + 1,
        Ordering::Relaxed,
    );
    thread::sleep(response.headers_delay);
    let mut headers = format!(
        "HTTP/1.1 {} Fixture\r\nConnection: close\r\n",
        response.status
    );
    if let Some(length) = response.length {
        headers.push_str(&format!("Content-Length: {length}\r\n"));
    }
    if let Some(location) = response.location {
        headers.push_str(&format!("Location: {location}\r\n"));
    }
    headers.push_str("\r\n");
    let _ = socket.write_all(headers.as_bytes());
    let _ = socket.write_all(&response.body);
    if response.stall {
        while !stop.load(Ordering::Acquire) {
            let mut byte = [0];
            match socket.read(&mut byte) {
                Ok(0) => {
                    closed.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => {
                    closed.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
        }
    }
    active.fetch_sub(1, Ordering::Relaxed);
}
fn run<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
fn scheduler(concurrent: u16, rate: u32) -> Arc<DownloadScheduler> {
    DownloadScheduler::new(DownloadPolicy {
        max_concurrent_transfers: concurrent,
        total_rate_limit_mib_per_second: rate,
        ..DownloadPolicy::default()
    })
    .unwrap()
}
fn client(server: &Server, scheduler: Arc<DownloadScheduler>) -> Client {
    let mut client = Client::new(&pcl_network::snapshot(), scheduler).unwrap();
    client.curseforge_key = Ok(crate::community_curseforge::credentials::Credential::synthetic());
    client.http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    client.route = Some(server.base.clone());
    client
}
fn hashes(body: &[u8]) -> (String, String) {
    (
        format!("{:x}", Sha1::digest(body)),
        format!("{:x}", Sha512::digest(body)),
    )
}
fn remote<'a>(urls: &'a [String], body: &[u8], hashes: &'a (String, String)) -> Remote<'a> {
    Remote {
        downloads: urls,
        size: body.len() as u64,
        sha1: &hashes.0,
        sha512: &hashes.1,
    }
}
fn url(path: &str) -> String {
    format!("https://cdn.modrinth.com{path}")
}
fn failure(result: Result<VerifiedFile>) -> String {
    match result {
        Ok(_) => panic!("unexpected download success"),
        Err(error) => error,
    }
}

#[test]
fn anonymous_inode_probe_detects_retained_alias_after_original_fd_closes() {
    let fixture = Fixture::new();
    let file = fixture.file();
    let identity = AnonymousIdentity::of(&file);
    let alias = file.try_clone().unwrap();
    drop(file);
    assert!(std::panic::catch_unwind(|| identity.assert_closed()).is_err());
    drop(alias);
    identity.assert_closed();
    fixture.assert_empty();
}

#[test]
fn real_relative_and_cross_allowed_host_redirects_return_verified_rewound_fd() {
    let fixture = Fixture::new();
    let body = b"verified generic artifact";
    let server = Server::new([
        (
            "/folder/start".into(),
            Response::redirect("../middle?token=private"),
        ),
        (
            "/middle".into(),
            Response::redirect("https://raw.githubusercontent.com/end"),
        ),
        ("/end".into(), Response::body(body)),
    ]);
    let client = client(&server, scheduler(1, 0));
    let urls = [url("/folder/start")];
    let expected = hashes(body);
    let mut verified = run(client.download(
        remote(&urls, body, &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {},
    ))
    .unwrap();
    assert_eq!(verified.size, body.len() as u64);
    assert_eq!((verified.sha1, verified.sha512), expected);
    let mut bytes = Vec::new();
    verified.file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, body);
    assert_eq!(verified.file.metadata().unwrap().nlink(), 0);
    assert_eq!(
        server.paths(),
        ["/folder/start", "/middle?token=private", "/end"]
    );
    assert_eq!(client.network_bytes(), body.len() as u64);
    assert!(server.requests.lock().unwrap().iter().all(|request| request
        .to_ascii_lowercase()
        .contains("accept-encoding: identity\r\n")));
    fixture.assert_empty();
}

#[test]
fn every_invalid_redirect_is_rejected_before_its_http_request_and_sanitized() {
    for location in [
        "http://github.com/blocked?token=private",
        "https://evil.test/blocked?token=private",
        "https://user:pass@github.com/blocked?token=private",
        "//@github.com/blocked?token=private",
        "https://github.com:444/blocked?token=private",
        "/blocked#private",
        "/bad%ZZ?token=private",
        "https://objects.githubusercontent.com/blocked?token=private",
    ] {
        let fixture = Fixture::new();
        let server = Server::new([("/start".into(), Response::redirect(location))]);
        let client = client(&server, scheduler(1, 0));
        let urls = [url("/start?token=private")];
        let expected = hashes(b"artifact");
        let error = failure(run(client.download(
            remote(&urls, b"artifact", &expected),
            fixture.file(),
            &AtomicBool::new(false),
            |_| {},
        )));
        assert!(!error.contains("private"));
        assert!(!error.contains("token"));
        assert_eq!(server.paths(), ["/start?token=private"]);
        assert_eq!(client.network_bytes(), 0);
        fixture.assert_empty();
    }
}

#[test]
fn real_redirect_cycles_and_five_hop_limit_do_not_make_seventh_request() {
    let fixture = Fixture::new();
    let expected = hashes(b"artifact");
    let urls = [url("/start")];
    let loop_server = Server::new([
        ("/start".into(), Response::redirect("/again")),
        ("/again".into(), Response::redirect("/start")),
    ]);
    let loop_client = client(&loop_server, scheduler(1, 0));
    assert!(failure(run(loop_client.download(
        remote(&urls, b"artifact", &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("循环"));
    assert_eq!(loop_server.paths().len(), 2);
    let routes = (0..=6).map(|i| {
        (
            if i == 0 {
                "/start".into()
            } else {
                format!("/hop{i}")
            },
            Response::redirect(&format!("/hop{}", i + 1)),
        )
    });
    let server = Server::new(routes);
    let client = client(&server, scheduler(1, 0));
    assert!(failure(run(client.download(
        remote(&urls, b"artifact", &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("5 次"));
    assert_eq!(server.paths().len(), 6);
    assert!(!server.paths().contains(&"/hop6".into()));
    fixture.assert_empty();
}

#[test]
fn corrupt_first_mirror_fails_over_in_order_and_all_read_bytes_remain_monotonic() {
    let fixture = Fixture::new();
    let body = b"good";
    let server = Server::new([
        ("/bad".into(), Response::body(b"evil")),
        ("/good".into(), Response::body(body)),
    ]);
    let client = client(&server, scheduler(1, 0));
    let urls = [
        "https://unsupported.test/skip".into(),
        url("/bad?token=private"),
        url("/good"),
    ];
    let expected = hashes(body);
    let telemetry = Mutex::new(Vec::new());
    let mut file = run(client.download(
        remote(&urls, body, &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |progress| telemetry.lock().unwrap().push(progress.network_bytes),
    ))
    .unwrap()
    .file;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, body);
    assert_eq!(server.paths(), ["/bad?token=private", "/good"]);
    assert_eq!(client.network_bytes(), 8);
    assert!(telemetry
        .lock()
        .unwrap()
        .windows(2)
        .all(|values| values[0] <= values[1]));
    assert_eq!(telemetry.lock().unwrap().last(), Some(&8));
    fixture.assert_empty();
}

#[test]
fn either_hash_mismatch_is_rejected_and_zero_bytes_are_verified() {
    let fixture = Fixture::new();
    let server = Server::new([
        ("/file".into(), Response::body(b"body")),
        ("/empty".into(), Response::body(b"")),
    ]);
    let client = client(&server, scheduler(1, 0));
    let urls = [url("/file")];
    for hash_index in 0..2 {
        let mut expected = hashes(b"body");
        if hash_index == 0 {
            expected.0 = "0".repeat(40)
        } else {
            expected.1 = "0".repeat(128)
        };
        assert!(failure(run(client.download(
            remote(&urls, b"body", &expected),
            fixture.file(),
            &AtomicBool::new(false),
            |_| {}
        )))
        .contains("SHA1 或 SHA512"));
    }
    let expected = hashes(b"body");
    let upper = (expected.0.to_uppercase(), expected.1.to_uppercase());
    assert!(run(client.download(
        remote(&urls, b"body", &upper),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    ))
    .is_ok());
    let urls = [url("/empty")];
    let empty = hashes(b"");
    let verified = run(client.download(
        remote(&urls, b"", &empty),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {},
    ))
    .unwrap();
    assert_eq!(verified.size, 0);
    assert_eq!(verified.file.metadata().unwrap().len(), 0);
    assert_eq!((verified.sha1, verified.sha512), empty);
    assert_eq!(client.network_bytes(), 12);
    fixture.assert_empty();
}

#[test]
fn declared_and_actual_body_lengths_are_enforced_and_oversized_bytes_count() {
    let fixture = Fixture::new();
    let server = Server::new([
        ("/declared".into(), Response::body(b"longer")),
        (
            "/short".into(),
            Response {
                length: None,
                ..Response::body(b"x")
            },
        ),
        (
            "/long".into(),
            Response {
                length: None,
                ..Response::body(b"longer")
            },
        ),
        (
            "/truncated".into(),
            Response {
                length: Some(4),
                ..Response::body(b"x")
            },
        ),
    ]);
    let client = client(&server, scheduler(1, 0));
    let expected = hashes(b"body");
    let before = client.network_bytes();
    let urls = [url("/declared")];
    assert!(failure(run(client.download(
        remote(&urls, b"body", &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("Content-Length"));
    assert_eq!(client.network_bytes(), before);
    for (path, reason) in [
        ("/short", "实际大小与声明不符"),
        ("/long", "实际大小超过声明"),
        ("/truncated", "响应读取失败"),
    ] {
        let urls = [url(path)];
        assert!(failure(run(client.download(
            remote(&urls, b"body", &expected),
            fixture.file(),
            &AtomicBool::new(false),
            |_| {}
        )))
        .contains(reason));
    }
    assert_eq!(client.network_bytes(), 8);
    fixture.assert_empty();
}

fn cancel_after(cancel: Arc<AtomicBool>, duration: Duration) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        thread::sleep(duration);
        cancel.store(true, Ordering::Release);
    })
}
#[test]
fn stalled_body_cancel_closes_response_and_fd_without_next_mirror() {
    let fixture = Fixture::new();
    let server = Server::new([
        (
            "/stall".into(),
            Response {
                body: vec![],
                length: Some(4),
                stall: true,
                ..Response::body(b"")
            },
        ),
        ("/unused".into(), Response::body(b"body")),
    ]);
    let policy = scheduler(1, 0);
    let client = client(&server, policy.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel_after(cancel.clone(), Duration::from_millis(150));
    let urls = [url("/stall"), url("/unused")];
    let expected = hashes(b"body");
    let file = fixture.file();
    let identity = AnonymousIdentity::of(&file);
    let start = Instant::now();
    assert_eq!(
        failure(run(client.download(
            remote(&urls, b"body", &expected),
            file,
            &cancel,
            |_| {}
        ))),
        CANCELLED
    );
    cancel_worker.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    identity.assert_closed();
    assert_eq!(policy.status().active_transfers, 0);
    assert_eq!(policy.status().queued_transfers, 0);
    assert_eq!(server.paths(), ["/stall"]);
    for _ in 0..50 {
        if server.closed_stalls.load(Ordering::Relaxed) != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.closed_stalls.load(Ordering::Relaxed), 1);
    fixture.assert_empty();
}

#[test]
fn queue_and_rate_waits_cancel_and_release_the_same_captured_scheduler() {
    let fixture = Fixture::new();
    let payload = vec![7; 2 * 1024 * 1024];
    let server = Server::new([("/file".into(), Response::body(&payload))]);
    let urls = [url("/file"), url("/unused")];
    let expected = hashes(&payload);
    let policy = scheduler(1, 0);
    let held = run(policy.acquire(&AtomicBool::new(false))).unwrap();
    let queued = client(&server, policy.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = cancel_after(cancel.clone(), Duration::from_millis(100));
    assert_eq!(
        failure(run(queued.download(
            remote(&urls, &payload, &expected),
            fixture.file(),
            &cancel,
            |_| {}
        ))),
        CANCELLED
    );
    worker.join().unwrap();
    assert!(server.paths().is_empty());
    assert_eq!(policy.status().active_transfers, 1);
    assert_eq!(policy.status().queued_transfers, 0);
    drop(held);
    assert_eq!(policy.status().active_transfers, 0);

    let policy = scheduler(1, 1);
    let limited = client(&server, policy.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = cancel_after(cancel.clone(), Duration::from_millis(100));
    let start = Instant::now();
    assert_eq!(
        failure(run(limited.download(
            remote(&urls, &payload, &expected),
            fixture.file(),
            &cancel,
            |_| {}
        ))),
        CANCELLED
    );
    worker.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(server.paths(), ["/file"]);
    assert!(limited.network_bytes() > 0);
    assert!(policy.status().paced_payload_bytes < payload.len() as u64);
    assert_eq!(policy.status().active_transfers, 0);
    assert_eq!(policy.status().queued_transfers, 0);
    fixture.assert_empty();
}

#[test]
fn shared_connection_slots_and_aggregate_rate_apply_to_real_concurrent_files() {
    for concurrent in [1, 3] {
        let fixture = Fixture::new();
        let body = vec![3; 256 * 1024];
        let server = Server::new([(
            "/file".into(),
            Response {
                headers_delay: Duration::from_millis(80),
                ..Response::body(&body)
            },
        )]);
        let policy = scheduler(concurrent, 1);
        let client = client(&server, policy.clone());
        let urls = [url("/file")];
        let expected = hashes(&body);
        let cancel = AtomicBool::new(false);
        let telemetry = Mutex::new(0);
        let report = |progress: TransferProgress| {
            let mut last = telemetry.lock().unwrap();
            assert!(progress.network_bytes >= *last);
            *last = progress.network_bytes;
        };
        let started = Instant::now();
        let (one, two, three) = run(async {
            tokio::join!(
                client.download(
                    remote(&urls, &body, &expected),
                    fixture.file(),
                    &cancel,
                    &report
                ),
                client.download(
                    remote(&urls, &body, &expected),
                    fixture.file(),
                    &cancel,
                    &report
                ),
                client.download(
                    remote(&urls, &body, &expected),
                    fixture.file(),
                    &cancel,
                    &report
                ),
            )
        });
        one.unwrap();
        two.unwrap();
        three.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(700));
        assert_eq!(server.peak.load(Ordering::Relaxed), concurrent as usize);
        assert_eq!(policy.status().paced_payload_bytes, body.len() as u64 * 3);
        assert_eq!(client.network_bytes(), body.len() as u64 * 3);
        assert_eq!(policy.status().active_transfers, 0);
        assert_eq!(policy.status().queued_transfers, 0);
        fixture.assert_empty();
    }
}

#[test]
fn bounded_deadline_stops_real_headers_and_stalled_body_reads() {
    let fixture = Fixture::new();
    let server = Server::new([
        (
            "/headers".into(),
            Response {
                headers_delay: Duration::from_millis(200),
                ..Response::body(b"body")
            },
        ),
        (
            "/body".into(),
            Response {
                body: vec![],
                length: Some(4),
                stall: true,
                ..Response::body(b"")
            },
        ),
    ]);
    let policy = scheduler(1, 0);
    let client = client(&server, policy.clone());
    let cancel = AtomicBool::new(false);
    let urls = [url("/unused")];
    let expected = hashes(b"body");
    for path in ["/headers", "/body"] {
        let mut file = fixture.file();
        let result = run(async {
            let _permit = policy.acquire(&cancel).await.unwrap();
            client
                .attempt(
                    mirror::declaration(&url(path)).unwrap().unwrap(),
                    &remote(&urls, b"body", &expected),
                    &mut file,
                    &cancel,
                    Instant::now() + Duration::from_millis(80),
                    &|_| {},
                    Source::Modrinth,
                )
                .await
        });
        assert!(matches!(result, Err(Failure::Mirror(ref reason)) if reason.contains("时间限制")));
        assert_eq!(policy.status().active_transfers, 0);
    }
    assert_eq!(server.paths(), ["/headers", "/body"]);
    fixture.assert_empty();
}

#[test]
fn declaration_and_destination_rejection_precede_http() {
    let fixture = Fixture::new();
    let server = Server::new([]);
    let client = client(&server, scheduler(1, 0));
    let expected = hashes(b"body");
    let urls = (0..17).map(|_| url("/file")).collect::<Vec<_>>();
    assert!(failure(run(client.download(
        remote(&urls, b"body", &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("镜像数量"));
    let urls = ["https://localhost/file".into()];
    assert!(failure(run(client.download(
        remote(&urls, b"body", &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("没有受支持"));
    let urls = [url("/file")];
    let mut file = fixture.file();
    file.write_all(b"existing").unwrap();
    assert!(failure(run(client.download(
        remote(&urls, b"body", &expected),
        file,
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("空的匿名"));
    assert!(server.paths().is_empty());
    fixture.assert_empty();
}

#[test]
fn dropping_pending_download_releases_response_wait_permit_and_owned_fd() {
    let fixture = Fixture::new();
    let server = Server::new([(
        "/stall".into(),
        Response {
            body: b"b".to_vec(),
            length: Some(4),
            stall: true,
            ..Response::body(b"")
        },
    )]);
    let policy = scheduler(1, 0);
    let client = client(&server, policy.clone());
    let urls = [url("/stall")];
    let expected = hashes(b"body");
    let cancel = AtomicBool::new(false);
    for queued in [false, true] {
        let held = queued.then(|| run(policy.acquire(&cancel)).unwrap());
        let file = fixture.file();
        let identity = AnonymousIdentity::of(&file);
        run(async {
            let mut download =
                Box::pin(client.download(remote(&urls, b"body", &expected), file, &cancel, |_| {}));
            let ready = async {
                loop {
                    if if queued {
                        policy.status().queued_transfers == 1
                    } else {
                        client.network_bytes() == 1
                    } {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            };
            tokio::select! {
                _=&mut download=>panic!("pending fixture unexpectedly completed"),
                result=tokio::time::timeout(Duration::from_secs(5),ready)=>
                    result.expect("download did not reach its real body/permit wait"),
            }
            assert_eq!(policy.status().queued_transfers, if queued { 1 } else { 0 });
            drop(download);
        });
        identity.assert_closed();
        assert_eq!(policy.status().queued_transfers, 0);
        assert_eq!(policy.status().active_transfers, if queued { 1 } else { 0 });
        drop(held);
    }
    assert_eq!(server.paths(), ["/stall"]);
    assert_eq!(policy.status().active_transfers, 0);
    assert!(!cancel.load(Ordering::Acquire));
    let closed_deadline = Instant::now() + Duration::from_secs(5);
    while server.closed_stalls.load(Ordering::Relaxed) == 0 && Instant::now() < closed_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.closed_stalls.load(Ordering::Relaxed), 1);
    fixture.assert_empty();
}

fn curseforge_remote(body: &[u8]) -> super::super::curseforge::ResolvedFile {
    super::super::curseforge::ResolvedFile {
        project_id: 123,
        file_id: 456,
        path: "mods/fixture.jar".into(),
        size: body.len() as u64,
        sha1: hashes(body).0,
        url: "https://edge.forgecdn.net/files/1/456/fixture.jar".into(),
    }
}

#[test]
fn curseforge_stream_authenticates_official_files_and_checks_sha1_and_local_sha512() {
    let fixture = Fixture::new();
    let body = b"synthetic CurseForge payload";
    let server = Server::new([("/files/1/456/fixture.jar".into(), Response::body(body))]);
    let client = client(&server, scheduler(1, 0));
    let file = curseforge_remote(body);
    let mut verified =
        run(client.download_curseforge(&file, fixture.file(), &AtomicBool::new(false), |_| {}))
            .unwrap();
    assert_eq!((verified.sha1, verified.sha512), hashes(body));
    let mut bytes = Vec::new();
    verified.file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, body);
    assert!(server.requests.lock().unwrap().iter().all(|request| request
        .to_ascii_lowercase()
        .contains("x-api-key: synthetic-test-key\r\n")));
}

#[test]
fn curseforge_does_not_broaden_mrpack_authority_and_rejects_external_redirects() {
    let fixture = Fixture::new();
    let body = b"synthetic";
    let file = curseforge_remote(body);
    let server = Server::new([(
        "/files/1/456/fixture.jar".into(),
        Response::redirect("https://cdn.modrinth.com/files/1/456/redirect.jar"),
    )]);
    let client = client(&server, scheduler(1, 0));
    assert!(super::super::mirror::declaration(&file.url)
        .unwrap()
        .is_none());
    assert!(failure(run(client.download_curseforge(
        &file,
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("重定向"));
    assert_eq!(server.paths(), ["/files/1/456/fixture.jar"]);
    let mut wrong_origin = curseforge_remote(body);
    wrong_origin.url = "https://cdn.modrinth.com/files/fixture.jar".into();
    assert!(run(client.download_curseforge(
        &wrong_origin,
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    ))
    .is_err());
    assert_eq!(server.paths().len(), 1);
}

#[test]
fn curseforge_official_node_redirect_keeps_authentication_and_verifies_the_final_bytes() {
    let fixture = Fixture::new();
    let body = b"synthetic redirected CurseForge payload";
    let server = Server::new([
        (
            "/files/1/456/fixture.jar".into(),
            Response::redirect("https://mediafilez.forgecdn.net/files/1/456/final.jar"),
        ),
        ("/files/1/456/final.jar".into(), Response::body(body)),
    ]);
    let client = client(&server, scheduler(1, 0));
    let verified = run(client.download_curseforge(
        &curseforge_remote(body),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {},
    ))
    .unwrap();
    assert_eq!((verified.sha1, verified.sha512), hashes(body));
    assert_eq!(
        server.paths(),
        ["/files/1/456/fixture.jar", "/files/1/456/final.jar"]
    );
    assert!(server.requests.lock().unwrap().iter().all(|request| request
        .to_ascii_lowercase()
        .contains("x-api-key: synthetic-test-key\r\n")));
}

#[test]
fn curseforge_corrupt_or_oversized_body_never_returns_a_verified_file() {
    for oversized in [false, true] {
        let fixture = Fixture::new();
        let body = b"synthetic";
        let mut file = curseforge_remote(body);
        if oversized {
            file.size -= 1;
        } else {
            file.sha1 = "0".repeat(40);
        }
        let server = Server::new([("/files/1/456/fixture.jar".into(), Response::body(body))]);
        let client = client(&server, scheduler(1, 0));
        let destination = fixture.file();
        let identity = AnonymousIdentity::of(&destination);
        assert!(run(client.download_curseforge(
            &file,
            destination,
            &AtomicBool::new(false),
            |_| {}
        ))
        .is_err());
        identity.assert_closed();
        fixture.assert_empty();
    }
}

#[test]
fn local_manifest_forge_mirrors_keep_double_hashes_and_reject_external_redirects() {
    let fixture = Fixture::new();
    let body = b"synthetic manifest-pinned file";
    let server = Server::new([
        ("/files/1/2/ok.jar".into(), Response::body(body)),
        (
            "/files/1/2/redirect.jar".into(),
            Response::redirect("https://edge.forgecdn.net.evil.test/files/other.jar"),
        ),
    ]);
    let standard = client(&server, scheduler(1, 0));
    let urls = ["https://edge.forgecdn.net/files/1/2/ok.jar".into()];
    let expected = hashes(body);
    assert!(run(standard.download(
        remote(&urls, body, &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    ))
    .is_err());
    assert!(server.paths().is_empty());
    let local = client(&server, scheduler(1, 0)).with_local_mirrors(true);
    let verified = run(local.download(
        remote(&urls, body, &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {},
    ))
    .unwrap();
    assert_eq!((verified.sha1, verified.sha512), expected);
    let wrong = (expected.0.clone(), "0".repeat(128));
    assert!(failure(run(local.download(
        remote(&urls, body, &wrong),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("SHA512"));
    let redirect = ["https://mediafilez.forgecdn.net/files/1/2/redirect.jar".into()];
    assert!(run(local.download(
        remote(&redirect, body, &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {}
    ))
    .is_err());
    assert_eq!(
        server.paths(),
        [
            "/files/1/2/ok.jar",
            "/files/1/2/ok.jar",
            "/files/1/2/redirect.jar"
        ]
    );
    assert!(server.requests.lock().unwrap().iter().all(|request| request
        .to_ascii_lowercase()
        .contains("x-api-key: synthetic-test-key\r\n")));
}

#[test]
fn local_forge_redirect_drops_authentication_on_non_cf_host() {
    let fixture = Fixture::new();
    let body = b"synthetic mixed mirror payload";
    let server = Server::new([
        (
            "/files/1/2/start.jar".into(),
            Response::redirect("https://cdn.modrinth.com/finish.jar"),
        ),
        ("/finish.jar".into(), Response::body(body)),
    ]);
    let client = client(&server, scheduler(1, 0)).with_local_mirrors(true);
    let urls = ["https://edge.forgecdn.net/files/1/2/start.jar".into()];
    let expected = hashes(body);
    run(client.download(
        remote(&urls, body, &expected),
        fixture.file(),
        &AtomicBool::new(false),
        |_| {},
    ))
    .unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("synthetic-test-key"));
    assert!(!requests[1].to_ascii_lowercase().contains("x-api-key"));
    assert!(!requests[1].contains("synthetic-test-key"));
}

#[test]
fn missing_cf_credential_stops_before_network_and_closes_staging() {
    let fixture = Fixture::new();
    let body = b"synthetic";
    let server = Server::new([]);
    let mut client = client(&server, scheduler(1, 0));
    client.curseforge_key = Err("CurseForge 服务尚未配置，暂不可用".into());
    let file = fixture.file();
    let identity = AnonymousIdentity::of(&file);
    assert!(failure(run(client.download_curseforge(
        &curseforge_remote(body),
        file,
        &AtomicBool::new(false),
        |_| {}
    )))
    .contains("尚未配置"));
    identity.assert_closed();
    assert!(server.paths().is_empty());
}
