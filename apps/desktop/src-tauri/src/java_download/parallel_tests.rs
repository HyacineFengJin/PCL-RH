//! Actual overlapping loopback responses exercise the worker and the shared
//! scheduler. Payloads include only a shell probe fixture, never a real JVM.
use super::{tests::*, *};
use crate::{
    integration_tests::Fixture,
    tasks::{TaskOutcome, TaskSnapshot, Tasks},
};
use catalog::{Entry, Manifest};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{atomic::AtomicUsize, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Transfers {
    active: AtomicUsize,
    peak: AtomicUsize,
    started: AtomicUsize,
}
impl Transfers {
    fn enter(self: &Arc<Self>) -> Active {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.started.fetch_add(1, Ordering::SeqCst);
        Active(self.clone())
    }
}
struct Active(Arc<Transfers>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Server {
    origin: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Server {
    fn new(reply: impl Fn(&str, &mut TcpStream, &AtomicBool) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let reply = Arc::new(reply);
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let reply = reply.clone();
                        let stopped = stopped.clone();
                        requests.push(std::thread::spawn(move || {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut header = Vec::new();
                            let mut byte = [0];
                            while header.len() < 8192 && !header.ends_with(b"\r\n\r\n") {
                                if stream.read(&mut byte).unwrap_or(0) == 0 {
                                    return;
                                }
                                header.push(byte[0]);
                            }
                            let request = String::from_utf8_lossy(&header);
                            reply(
                                request.split_whitespace().nth(1).unwrap_or(""),
                                &mut stream,
                                &stopped,
                            );
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            for request in requests {
                request.join().unwrap();
            }
        });
        Self {
            origin,
            stop,
            worker: Some(worker),
        }
    }
    fn http(&self, scheduler: Arc<pcl_network::DownloadScheduler>) -> network::Http {
        let factory = pcl_network::ClientFactory::new(pcl_network::Policy {
            proxy: pcl_network::ProxyPolicy::None,
            ..Default::default()
        })
        .unwrap();
        let mut http = network::Http::new(Arc::new(factory), scheduler).unwrap();
        http.fixture = Some(self.origin.clone());
        http
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.origin.strip_prefix("http://").unwrap());
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn header(stream: &mut TcpStream, size: usize) {
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
    );
}
fn response(stream: &mut TcpStream, body: &[u8]) {
    header(stream, body.len());
    let _ = stream.write_all(body);
}
fn wait_for(mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    true
}

struct Provider {
    package: Package,
    manifest: Manifest,
    all: Vec<u8>,
    metadata: Vec<u8>,
    payloads: BTreeMap<String, Vec<u8>>,
}
impl Provider {
    fn new() -> Self {
        let java = script(21, std::env::consts::ARCH);
        let mut manifest = plan(&java);
        let mut payloads = BTreeMap::from([
            ("java".into(), java),
            ("LICENSE".into(), b"license".to_vec()),
        ]);
        manifest.files.insert("lib".into(), Entry::Directory);
        for index in 0..6 {
            let leaf = format!("payload-{index:02}");
            let body = vec![b'a' + index; 48 * 1024];
            manifest
                .files
                .insert(format!("lib/{leaf}"), file(&body, &leaf, false));
            payloads.insert(leaf, body);
        }
        let package = package(&manifest);
        Self {
            all: catalog_body(&[package.clone()]),
            metadata: serde_json::to_vec(&manifest).unwrap(),
            package,
            manifest,
            payloads,
        }
    }
    fn metadata(&self, path: &str, stream: &mut TcpStream) -> bool {
        if path.ends_with("/all.json") {
            response(stream, &self.all);
            true
        } else if path.ends_with("/manifest.json") {
            response(stream, &self.metadata);
            true
        } else {
            false
        }
    }
    fn body(&self, path: &str) -> &[u8] {
        &self.payloads[path.rsplit('/').next().unwrap()]
    }
}
fn scheduler(limit: u16) -> Arc<pcl_network::DownloadScheduler> {
    pcl_network::DownloadScheduler::new(pcl_network::DownloadPolicy {
        max_concurrent_transfers: limit,
        ..Default::default()
    })
    .unwrap()
}
fn assert_released(scheduler: &pcl_network::DownloadScheduler) {
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
}
fn assert_stage_clean(store: &Store) {
    assert_eq!(store.dir.names().unwrap(), [".java-install.lock"]);
    // Cleanup must run after every in-flight anonymous file has closed. A
    // removed stage alone would hide a detached writer retaining such an FD.
    let prefix = format!("{}/.stage-", store.path.display());
    for descriptor in fs::read_dir("/proc/self/fd").unwrap().flatten() {
        if let Ok(path) = fs::read_link(descriptor.path()) {
            assert!(!path.to_string_lossy().starts_with(&prefix), "{path:?}");
        }
    }
}
fn finish(task: &TaskHandle) {
    task.finish(TaskOutcome::Error("loopback fixture finished".into()));
}
fn collect_progress(tasks: &Arc<Tasks>) -> Arc<Mutex<Vec<TaskSnapshot>>> {
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let collected = snapshots.clone();
    tasks.set_listener(move |snapshot| {
        if snapshot.phase == "java-files" {
            collected.lock().unwrap().push(snapshot);
        }
    });
    snapshots
}
fn assert_progress(snapshots: &Arc<Mutex<Vec<TaskSnapshot>>>, total_bytes: u64) {
    let snapshots = snapshots.lock().unwrap();
    assert!(snapshots.len() > 8);
    for pair in snapshots.windows(2) {
        assert!(pair[0].bytes_done <= pair[1].bytes_done);
        assert!(pair[0].network_bytes <= pair[1].network_bytes);
        assert!(pair[0].completed <= pair[1].completed);
    }
    let last = snapshots.last().unwrap();
    assert_eq!(last.bytes_done, total_bytes);
    assert_eq!(last.network_bytes, total_bytes);
    assert_eq!(last.completed, 8);
    assert_eq!(last.total, 8);
}

#[tokio::test]
async fn java_payloads_obey_configured_parallelism_and_progress_never_rewinds() {
    for limit in [1, 3] {
        let provider = Provider::new();
        let package = provider.package.clone();
        let total_bytes = provider.manifest.validate().unwrap();
        let transfers = Arc::new(Transfers::default());
        let tracked = transfers.clone();
        let server = Server::new(move |path, stream, _| {
            if provider.metadata(path, stream) {
                return;
            }
            let active = tracked.enter();
            let body = provider.body(path);
            header(stream, body.len());
            let split = body.len() / 2;
            let _ = stream.write_all(&body[..split]);
            std::thread::sleep(Duration::from_millis(60));
            // Stop counting before final bytes become observable; the client
            // may release its slot and start another request immediately.
            drop(active);
            let _ = stream.write_all(&body[split..]);
        });
        let fixture = Fixture::new();
        let store = Store::capture(&fixture.0).unwrap();
        let tasks = Arc::new(Tasks::new());
        let task = task(&tasks, &store);
        task.wait_turn().unwrap();
        let progress = collect_progress(&tasks);
        let scheduler = scheduler(limit);
        let installed = install(
            &store,
            &package.component,
            &package.sha1,
            server.http(scheduler.clone()),
            &task,
        )
        .await
        .unwrap();
        assert_eq!(installed.runtime.major, 21);
        assert_eq!(transfers.peak.load(Ordering::SeqCst), usize::from(limit));
        assert_eq!(transfers.started.load(Ordering::SeqCst), 8);
        assert_progress(&progress, total_bytes);
        assert_released(&scheduler);
        finish(&task);
    }
}

#[tokio::test]
async fn separate_java_workers_share_the_global_transfer_slots() {
    let provider = Provider::new();
    let package = provider.package.clone();
    let transfers = Arc::new(Transfers::default());
    let tracked = transfers.clone();
    let server = Server::new(move |path, stream, _| {
        if provider.metadata(path, stream) {
            return;
        }
        let active = tracked.enter();
        let body = provider.body(path);
        header(stream, body.len());
        std::thread::sleep(Duration::from_millis(60));
        drop(active);
        let _ = stream.write_all(body);
    });
    let first = Fixture::new();
    let second = Fixture::new();
    let first_store = Store::capture(&first.0).unwrap();
    let second_store = Store::capture(&second.0).unwrap();
    let first_tasks = Arc::new(Tasks::new());
    let second_tasks = Arc::new(Tasks::new());
    let first_task = task(&first_tasks, &first_store);
    let second_task = task(&second_tasks, &second_store);
    first_task.wait_turn().unwrap();
    second_task.wait_turn().unwrap();
    let scheduler = scheduler(3);
    let (first_result, second_result) = tokio::join!(
        install(
            &first_store,
            &package.component,
            &package.sha1,
            server.http(scheduler.clone()),
            &first_task,
        ),
        install(
            &second_store,
            &package.component,
            &package.sha1,
            server.http(scheduler.clone()),
            &second_task,
        )
    );
    assert!(first_result.is_ok());
    assert!(second_result.is_ok());
    assert_eq!(transfers.started.load(Ordering::SeqCst), 16);
    assert_eq!(transfers.peak.load(Ordering::SeqCst), 3);
    assert_released(&scheduler);
    finish(&first_task);
    finish(&second_task);
}

#[tokio::test]
async fn parallel_hash_failure_drops_other_transfers_before_cleanup_and_keeps_real_error() {
    let provider = Provider::new();
    let package = provider.package.clone();
    let transfers = Arc::new(Transfers::default());
    let tracked = transfers.clone();
    let server = Server::new(move |path, stream, stop| {
        if provider.metadata(path, stream) {
            return;
        }
        let _active = tracked.enter();
        let body = provider.body(path);
        header(stream, body.len());
        if path.ends_with("/java") {
            if !wait_for(|| tracked.started.load(Ordering::SeqCst) == 3) {
                return;
            }
            // The expected size matches: only SHA-1 can reject this body.
            let _ = stream.write_all(&vec![b'!'; body.len()]);
        } else {
            let _ = stream.write_all(&body[..1]);
            let _ = wait_for(|| stop.load(Ordering::SeqCst));
        }
    });
    let fixture = Fixture::new();
    let store = Store::capture(&fixture.0).unwrap();
    let tasks = Arc::new(Tasks::new());
    let task = task(&tasks, &store);
    task.wait_turn().unwrap();
    let scheduler = scheduler(3);
    let error = install(
        &store,
        &package.component,
        &package.sha1,
        server.http(scheduler.clone()),
        &task,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error, "Java 文件大小或 SHA-1 校验失败");
    assert!(!task.cancellation_token().load(Ordering::SeqCst));
    assert!(tasks.snapshot(task.id()).unwrap().can_cancel);
    assert_eq!(transfers.started.load(Ordering::SeqCst), 3);
    assert_stage_clean(&store);
    assert_released(&scheduler);
    finish(&task);
}

#[tokio::test]
async fn cancelling_parallel_java_waits_for_all_payload_futures_and_cleans_stage() {
    let provider = Provider::new();
    let package = provider.package.clone();
    let transfers = Arc::new(Transfers::default());
    let tracked = transfers.clone();
    let server = Server::new(move |path, stream, stop| {
        if provider.metadata(path, stream) {
            return;
        }
        let _active = tracked.enter();
        let body = provider.body(path);
        header(stream, body.len());
        let _ = stream.write_all(&body[..1]);
        let _ = wait_for(|| stop.load(Ordering::SeqCst));
    });
    let fixture = Fixture::new();
    let store = Store::capture(&fixture.0).unwrap();
    let tasks = Arc::new(Tasks::new());
    let task = task(&tasks, &store);
    task.wait_turn().unwrap();
    let cancelling_tasks = tasks.clone();
    let id = task.id().to_string();
    let setter = std::thread::spawn(move || {
        assert!(
            wait_for(|| transfers.started.load(Ordering::SeqCst) == 3),
            "three Java transfers did not start"
        );
        assert!(cancelling_tasks.snapshot(&id).unwrap().can_cancel);
        cancelling_tasks.cancel(&id).unwrap();
    });
    let scheduler = scheduler(3);
    let result = install(
        &store,
        &package.component,
        &package.sha1,
        server.http(scheduler.clone()),
        &task,
    )
    .await;
    setter.join().unwrap();
    assert_eq!(result.err().unwrap(), CANCELLED);
    assert_stage_clean(&store);
    assert_released(&scheduler);
    task.finish(TaskOutcome::Failed(CANCELLED.into()));
    assert_eq!(
        tasks.snapshot(task.id()).unwrap().stage,
        TaskStage::Cancelled
    );
}
