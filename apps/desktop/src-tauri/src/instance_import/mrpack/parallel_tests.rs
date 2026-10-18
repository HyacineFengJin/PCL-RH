//! Exercise the real private-build service with logical official URLs routed
//! only at the final HTTP request. Gates acknowledge live transfers and cleanup;
//! no provider, Java executable or real game directory participates.
use super::*;
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::{
    net::{TcpListener, TcpStream},
    sync::atomic::AtomicUsize,
    thread,
};

const BODY: &[u8] = b"verified parallel pack dependency";

#[derive(Clone)]
struct Reply {
    body: Vec<u8>,
    stall: bool,
}
impl Reply {
    fn body() -> Self {
        Self {
            body: BODY.to_vec(),
            stall: false,
        }
    }
    fn stall() -> Self {
        Self {
            body: Vec::new(),
            stall: true,
        }
    }
}
#[derive(Default)]
struct ServerState {
    stop: AtomicBool,
    release: AtomicBool,
    active: AtomicUsize,
    peak: AtomicUsize,
    stalls: AtomicUsize,
    closed: AtomicUsize,
    paths: Mutex<Vec<String>>,
}
struct Server {
    base: reqwest::Url,
    state: Arc<ServerState>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(routes: BTreeMap<String, Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base =
            reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let state = Arc::new(ServerState::default());
        let signal = state.clone();
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            let routes = Arc::new(routes);
            while !signal.stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        assert!(workers.len() < 64, "bounded fixture request budget");
                        socket.set_nonblocking(false).unwrap();
                        let routes = routes.clone();
                        let state = signal.clone();
                        workers.push(thread::spawn(move || serve(socket, &routes, &state)));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("parallel fixture accept: {error}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            base,
            state,
            worker: Some(worker),
        }
    }
    fn release(&self) {
        self.state.release.store(true, Ordering::Release);
    }
    fn wait_active(&self, count: usize) {
        until(|| self.state.active.load(Ordering::Acquire) == count);
    }
    fn assert_drained(&self, expected_closed: usize) {
        until(|| {
            self.state.active.load(Ordering::Acquire) == 0
                && self.state.closed.load(Ordering::Acquire) >= expected_closed
        });
        assert_eq!(self.state.closed.load(Ordering::Acquire), expected_closed);
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "fixture boundary was not acknowledged"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn serve(mut socket: TcpStream, routes: &BTreeMap<String, Reply>, state: &ServerState) {
    socket
        .set_read_timeout(Some(Duration::from_millis(30)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut request = Vec::new();
    while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
        let mut bytes = [0; 1024];
        match socket.read(&mut bytes) {
            Ok(0) | Err(_) => return,
            Ok(count) => request.extend_from_slice(&bytes[..count]),
        }
    }
    if !request.ends_with(b"\r\n\r\n") {
        return;
    }
    let request = String::from_utf8(request).unwrap();
    let path = request.split_whitespace().nth(1).unwrap().to_owned();
    let reply = routes.get(&path).expect("declared fixture route");
    state.paths.lock().unwrap().push(path);
    state.peak.fetch_max(
        state.active.fetch_add(1, Ordering::AcqRel) + 1,
        Ordering::AcqRel,
    );
    while !state.release.load(Ordering::Acquire) && !state.stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(2));
    }
    let headers = format!(
        "HTTP/1.1 200 Fixture\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        BODY.len()
    );
    let _ = socket.write_all(headers.as_bytes());
    let _ = socket.write_all(&reply.body);
    if reply.stall {
        state.stalls.fetch_add(1, Ordering::AcqRel);
        while !state.stop.load(Ordering::Acquire) {
            let mut byte = [0];
            match socket.read(&mut byte) {
                Ok(0) => {
                    state.closed.fetch_add(1, Ordering::AcqRel);
                    break;
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => {
                    state.closed.fetch_add(1, Ordering::AcqRel);
                    break;
                }
            }
        }
    }
    state.active.fetch_sub(1, Ordering::AcqRel);
}
fn scheduler(limit: u16) -> Arc<pcl_network::DownloadScheduler> {
    pcl_network::DownloadScheduler::new(pcl_network::DownloadPolicy {
        max_concurrent_transfers: limit,
        ..pcl_network::DownloadPolicy::default()
    })
    .unwrap()
}
fn routes(prefix: &str, count: usize, reply: Reply) -> BTreeMap<String, Reply> {
    (0..count)
        .map(|index| (format!("/{prefix}-{index}"), reply.clone()))
        .collect()
}
fn remote_authority(
    fixture: &Fixture,
    name: &str,
    prefix: &str,
    count: usize,
    server: &Server,
    scheduler: Arc<pcl_network::DownloadScheduler>,
) -> (TaskHandle, ConfirmedPack) {
    let plan = fixture.prepare(name);
    let mut authority = fixture
        .shared
        .pack_confirmations
        .claim(
            &fixture.root,
            &fixture.project.0,
            &fixture.source,
            name,
            &plan.revision,
        )
        .unwrap();
    for index in 0..count {
        let path = format!("mods/parallel/{prefix}-{index}.jar");
        authority.checked.outputs.insert(
            path.clone(),
            Output::Remote {
                path,
                size: BODY.len() as u64,
                sha1: format!("{:x}", Sha1::digest(BODY)),
                sha512: format!("{:x}", Sha512::digest(BODY)),
                downloads: vec![format!("https://cdn.modrinth.com/{prefix}-{index}")],
            },
        );
    }
    authority.scheduler = scheduler;
    authority.transfer_route = Some(server.base.clone());
    let task = fixture
        .shared
        .tasks
        .admit_queued(
            TaskTarget {
                root_id: fixture.root.id.clone(),
                root_path: fixture.root.path.clone(),
                instance_id: Some(name.into()),
            },
            TaskKind::ModpackInstall,
            TaskScope::root(Path::new(&fixture.root.path)).unwrap(),
        )
        .unwrap();
    fixture.shared.downloads.track(&task);
    (task, authority)
}
fn worker(
    fixture: &Fixture,
    task: TaskHandle,
    authority: ConfirmedPack,
) -> thread::JoinHandle<(TaskHandle, Result<serde_json::Value>)> {
    let shared = fixture.shared.clone();
    let root = fixture.root.clone();
    thread::spawn(move || {
        let result = execute(&shared, &root, &task, authority, None);
        (task, result)
    })
}
fn no_private_residue(fixture: &Fixture, name: &str) {
    assert!(!Path::new(&fixture.root.path)
        .join("versions")
        .join(name)
        .exists());
    super::super::super::super::ensure_ready(Path::new(&fixture.root.path)).unwrap();
    let store = Path::new(&fixture.root.path).join(".pcl-linux/instance-imports");
    if store.exists() {
        for entry in fs::read_dir(store).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".lock" {
                continue;
            }
            // Durable terminal receipts are retained by the transaction. Its
            // payload and private build must already be gone before return.
            assert!(fs::read_dir(entry.path()).unwrap().all(|child| {
                matches!(
                    child.unwrap().file_name().to_str(),
                    Some("journal.json" | "journal.next")
                )
            }));
        }
    }
    let private_root = Path::new(&fixture.root.path).join(".pcl-linux");
    // No anonymous descriptor may still refer to this operation's private tree.
    for entry in fs::read_dir("/proc/self/fd").unwrap() {
        if let Ok(target) = fs::read_link(entry.unwrap().path()) {
            assert!(
                !target.starts_with(&private_root),
                "retained private descriptor: {target:?}"
            );
        }
    }
    assert_eq!(
        input_usage(&fixture.shared.pack_confirmations),
        (0, 0, 0, 0)
    );
}

#[test]
fn pack_remote_files_obey_one_and_three_slots_with_monotonic_aggregate_progress() {
    for limit in [1, 3] {
        let fixture = Fixture::new();
        let server = Server::new(routes("pack", 6, Reply::body()));
        let scheduler = scheduler(limit);
        let (task, authority) = remote_authority(
            &fixture,
            "ParallelPack",
            "pack",
            6,
            &server,
            scheduler.clone(),
        );
        let samples = Arc::new(Mutex::new(Vec::new()));
        let observed = samples.clone();
        fixture.shared.tasks.set_listener(move |snapshot| {
            observed.lock().unwrap().push(snapshot.network_bytes);
        });
        let handle = worker(&fixture, task, authority);
        server.wait_active(usize::from(limit));
        server.release();
        let (task, result) = handle.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
        crate::finish_instance_task(task, result, "parallel fixture complete");
        server.assert_drained(0);
        assert_eq!(
            server.state.peak.load(Ordering::Acquire),
            usize::from(limit)
        );
        assert_eq!(scheduler.status().active_transfers, 0);
        assert_eq!(scheduler.status().queued_transfers, 0);
        assert_eq!(
            scheduler.status().paced_payload_bytes,
            6 * BODY.len() as u64
        );
        let samples = samples.lock().unwrap();
        assert!(
            samples.windows(2).all(|pair| pair[0] <= pair[1]),
            "{samples:?}"
        );
        assert_eq!(samples.last().copied(), Some(6 * BODY.len() as u64));
        for index in 0..6 {
            assert_eq!(
                fs::read(Path::new(&fixture.root.path).join(format!(
                    "versions/ParallelPack/mods/parallel/pack-{index}.jar"
                )))
                .unwrap(),
                BODY
            );
        }
        assert_eq!(
            fs::read(Path::new(&fixture.root.path).join("versions/ParallelPack/config/local.cfg"))
                .unwrap(),
            b"pack configuration"
        );
        assert_eq!(
            input_usage(&fixture.shared.pack_confirmations),
            (0, 0, 0, 0)
        );
    }
}

#[test]
fn independent_pack_tasks_share_captured_connection_slots() {
    let first = Fixture::new();
    let second = Fixture::new();
    let mut all_routes = routes("first", 6, Reply::body());
    all_routes.extend(routes("second", 6, Reply::body()));
    let server = Server::new(all_routes);
    let scheduler = scheduler(3);
    let (task_a, authority_a) = remote_authority(
        &first,
        "FirstParallel",
        "first",
        6,
        &server,
        scheduler.clone(),
    );
    let (task_b, authority_b) = remote_authority(
        &second,
        "SecondParallel",
        "second",
        6,
        &server,
        scheduler.clone(),
    );
    let worker_a = worker(&first, task_a, authority_a);
    let worker_b = worker(&second, task_b, authority_b);
    server.wait_active(3);
    until(|| scheduler.status().queued_transfers > 0);
    server.release();
    let (task_a, result_a) = worker_a.join().unwrap();
    let (task_b, result_b) = worker_b.join().unwrap();
    assert!(result_a.is_ok(), "{result_a:?}");
    assert!(result_b.is_ok(), "{result_b:?}");
    crate::finish_instance_task(task_a, result_a, "first fixture complete");
    crate::finish_instance_task(task_b, result_b, "second fixture complete");
    server.assert_drained(0);
    assert_eq!(server.state.peak.load(Ordering::Acquire), 3);
    assert_eq!(server.state.paths.lock().unwrap().len(), 12);
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
    assert_eq!(
        scheduler.status().paced_payload_bytes,
        12 * BODY.len() as u64
    );
}

#[test]
fn pack_hash_failure_drops_inflight_responses_before_aborting_private_build() {
    let fixture = Fixture::new();
    let mut all_routes = routes("broken", 6, Reply::stall());
    let mut wrong = BODY.to_vec();
    wrong[0] ^= 1;
    all_routes.insert(
        "/broken-0".into(),
        Reply {
            body: wrong,
            stall: false,
        },
    );
    let server = Server::new(all_routes);
    let scheduler = scheduler(3);
    let (task, authority) = remote_authority(
        &fixture,
        "HashFailure",
        "broken",
        6,
        &server,
        scheduler.clone(),
    );
    let handle = worker(&fixture, task, authority);
    server.wait_active(3);
    server.release();
    let (task, result) = handle.join().unwrap();
    let error = result.unwrap_err();
    assert!(error.contains("SHA1 或 SHA512 校验失败"), "{error}");
    // A later stop request must not rewrite the observed hash failure.
    task.cancellation_token().store(true, Ordering::Release);
    let id = task.id().to_owned();
    task.finish(TaskOutcome::Error(error)).unwrap();
    assert_eq!(
        fixture.shared.tasks.snapshot(&id).unwrap().stage,
        TaskStage::Error
    );
    server.assert_drained(2);
    assert_eq!(server.state.paths.lock().unwrap().len(), 3);
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
    no_private_residue(&fixture, "HashFailure");
}

#[test]
fn pack_cancel_drops_all_inflight_files_before_aborting_private_build() {
    let fixture = Fixture::new();
    let server = Server::new(routes("cancel", 6, Reply::stall()));
    let scheduler = scheduler(3);
    let (task, authority) = remote_authority(
        &fixture,
        "ParallelCancel",
        "cancel",
        6,
        &server,
        scheduler.clone(),
    );
    let cancel = task.cancellation_token();
    let handle = worker(&fixture, task, authority);
    server.wait_active(3);
    server.release();
    until(|| server.state.stalls.load(Ordering::Acquire) == 3);
    cancel.store(true, Ordering::Release);
    let (task, result) = handle.join().unwrap();
    let error = result.unwrap_err();
    assert_eq!(error, "实例导入已取消");
    task.finish(TaskOutcome::Failed(error)).unwrap();
    server.assert_drained(3);
    assert_eq!(server.state.paths.lock().unwrap().len(), 3);
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
    no_private_residue(&fixture, "ParallelCancel");
}
