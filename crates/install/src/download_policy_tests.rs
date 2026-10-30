//! Local HTTP and generic file fixtures only; no installed launcher/game root.
use super::*;
use std::{collections::HashMap, net::TcpListener, sync::Arc, thread, time::Instant};
fn fixture() -> tempfile::TempDir {
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../work/launcher-options-2026-10-05/download-policy-fixtures");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(work).unwrap()
}
struct Server {
    base: String,
    stop: Arc<AtomicBool>,
    maximum: Arc<AtomicU64>,
    artifact_requests: Arc<AtomicU64>,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    component_maximum: Arc<AtomicU64>,
    component_requests: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn server(payload: Vec<u8>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let desc = |path: &str, body: &[u8]| serde_json::json!({"url":format!("{base}/{path}"),"sha1":format!("{:x}",Sha1::digest(body)),"size":body.len()});
    let index = serde_json::to_vec(&serde_json::json!({"objects":{}})).unwrap();
    let mut index_desc = desc("index", &index);
    index_desc["id"] = "fixture-index".into();
    let mut libraries = Vec::new();
    let mut routes = HashMap::new();
    for n in 0..6 {
        let path = format!("library{n}");
        let mut artifact = desc(&path, &payload);
        artifact["path"] = format!("org/example/library{n}/1/library{n}-1.jar").into();
        libraries.push(serde_json::json!({"name":format!("org.example:library{n}:1"),"downloads":{"artifact":artifact}}));
        routes.insert(format!("/{path}"), payload.clone());
    }
    let metadata=serde_json::to_vec(&serde_json::json!({"id":"fixture","javaVersion":{"majorVersion":21},"downloads":{"client":desc("client",&payload)},"assetIndex":index_desc,"libraries":libraries})).unwrap();
    let catalog=serde_json::to_vec(&serde_json::json!({"versions":[{"id":"fixture","type":"release","releaseTime":"2024-01-01T00:00:00Z","url":format!("{base}/metadata"),"sha1":format!("{:x}",Sha1::digest(&metadata))}]})).unwrap();
    routes.insert("/catalog".into(), catalog);
    routes.insert("/metadata".into(), metadata);
    routes.insert("/index".into(), index);
    routes.insert("/client".into(), payload.clone());
    routes.insert("/payload".into(), payload);
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let maximum = Arc::new(AtomicU64::new(0));
    let peak = maximum.clone();
    let active = Arc::new(AtomicU64::new(0));
    let requests = Arc::new(AtomicU64::new(0));
    let seen = requests.clone();
    let routes = Arc::new(Mutex::new(routes));
    let server_routes = routes.clone();
    let component_maximum = Arc::new(AtomicU64::new(0));
    let component_peak = component_maximum.clone();
    let component_active = Arc::new(AtomicU64::new(0));
    let component_requests = Arc::new(AtomicU64::new(0));
    let component_seen = component_requests.clone();
    let worker = thread::spawn(move || {
        let mut workers = Vec::new();
        while !stopping.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    let routes = server_routes.clone();
                    let active = active.clone();
                    let peak = peak.clone();
                    let seen = seen.clone();
                    let component_peak = component_peak.clone();
                    let component_active = component_active.clone();
                    let component_seen = component_seen.clone();
                    workers.push(thread::spawn(move || {
                        socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                        socket.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                        let mut buf = [0u8; 4096];
                        let count = socket.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..count]);
                        let path = request.split_whitespace().nth(1).unwrap_or("");
                        let component = path.starts_with("/component");
                        let artifact = component || path.starts_with("/library") || path == "/client" || path == "/payload";
                        if artifact {
                            seen.fetch_add(1, Ordering::Relaxed);
                            let count = active.fetch_add(1, Ordering::Relaxed) + 1;
                            peak.fetch_max(count, Ordering::Relaxed);
                            if component {
                                component_seen.fetch_add(1, Ordering::Relaxed);
                                let count = component_active.fetch_add(1, Ordering::Relaxed) + 1;
                                component_peak.fetch_max(count, Ordering::Relaxed);
                            }
                            thread::sleep(Duration::from_millis(40));
                        }
                        let body = routes.lock().unwrap().get(path).cloned();
                        if let Some(body) = body {
                            let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                            let _ = socket.write_all(&body);
                        } else {
                            let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                        if component { component_active.fetch_sub(1, Ordering::Relaxed); }
                        if artifact { active.fetch_sub(1, Ordering::Relaxed); }
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(e) => panic!("{e}"),
            }
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    Server {
        base,
        stop,
        maximum,
        artifact_requests: requests,
        routes,
        component_maximum,
        component_requests,
        worker: Some(worker),
    }
}

#[path = "component_parallel_tests.rs"]
mod component_parallel_tests;
fn installer(server: &Server, max: u16, rate: u32, forbid: bool) -> Installer {
    let scheduler = pcl_network::DownloadScheduler::new(pcl_network::DownloadPolicy {
        max_concurrent_transfers: max,
        total_rate_limit_mib_per_second: rate,
        forbid_cross_root_cache_copy: forbid,
    })
    .unwrap();
    let mut installer = Installer::new().unwrap().with_download_policy(scheduler);
    installer.endpoint = Some(format!("{}/catalog", server.base));
    installer.client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    installer
}
#[test]
fn actual_install_queue_uses_configured_one_or_three_connections_and_monotonic_progress() {
    for maximum in [1, 3] {
        let server = server(b"generic verified artifact".to_vec());
        let root = fixture();
        let installer = installer(&server, maximum, 0, false);
        let previous = Mutex::new((0, 0));
        installer
            .install(root.path(), "fixture", &AtomicBool::new(false), |p| {
                let mut previous = previous.lock().unwrap();
                assert!(p.network_bytes >= previous.0);
                previous.0 = p.network_bytes;
                if p.stage == "downloading" {
                    assert!(p.completed >= previous.1);
                    previous.1 = p.completed;
                }
            })
            .unwrap();
        assert_eq!(server.maximum.load(Ordering::Relaxed), u64::from(maximum));
        assert_eq!(server.artifact_requests.load(Ordering::Relaxed), 7);
        assert_eq!(installer.downloads.status().active_transfers, 0);
    }
}
#[test]
fn actual_http_body_paces_payload_and_metadata_bypasses_artifact_budget() {
    let server = server(vec![7; 1024 * 1024]);
    let installer = installer(&server, 2, 1, false);
    let cancel = AtomicBool::new(false);
    let url = installer.url(&format!("{}/payload", server.base)).unwrap();
    let start = Instant::now();
    let mut bytes = 0;
    installer
        .transfer(&url, 1024 * 1024, &cancel, |chunk| {
            bytes += chunk.len();
            Ok(())
        })
        .unwrap();
    assert_eq!(bytes, 1024 * 1024);
    assert!(start.elapsed() >= Duration::from_millis(995));
    let charged = installer.downloads.status().paced_payload_bytes;
    let mut metadata = 0;
    installer
        .metadata_transfer(&url, 1024 * 1024, &cancel, |chunk| {
            metadata += chunk.len();
            Ok(())
        })
        .unwrap();
    assert_eq!(metadata, 1024 * 1024);
    assert_eq!(installer.downloads.status().paced_payload_bytes, charged);
}
fn download_one(installer: &Installer, root: &Path, download: &Download) -> (bool, u64) {
    let cancel = AtomicBool::new(false);
    let done = AtomicU64::new(0);
    let bytes = AtomicU64::new(0);
    let network = AtomicU64::new(0);
    let group = AtomicU64::new(0);
    let result = installer
        .download(
            root,
            download,
            &cancel,
            &AtomicBool::new(false),
            &done,
            1,
            &bytes,
            &network,
            download.size,
            &group,
            1,
            &|_| {},
        )
        .unwrap();
    (result, network.load(Ordering::Relaxed))
}
#[test]
fn forbid_cross_root_cache_copy_downloads_but_keeps_verified_target_reuse() {
    let payload = b"cache bytes";
    let source = fixture();
    fs::create_dir_all(source.path().join("libraries")).unwrap();
    fs::write(source.path().join("libraries/example.jar"), payload).unwrap();
    for forbid in [false, true] {
        let server = server(payload.to_vec());
        let target = fixture();
        let installer = installer(&server, 1, 0, forbid)
            .with_cache_source(source.path())
            .unwrap();
        let artifact = Download {
            url: format!("{}/payload", server.base),
            relative: "libraries/example.jar".into(),
            hash: format!("{:x}", Sha1::digest(payload)),
            size: payload.len() as u64,
        };
        let (downloaded, network) = download_one(&installer, target.path(), &artifact);
        assert_eq!(downloaded, forbid);
        assert_eq!(network, if forbid { payload.len() as u64 } else { 0 });
        assert_eq!(
            server.artifact_requests.load(Ordering::Relaxed),
            u64::from(forbid)
        );
        assert_eq!(
            download_one(&installer, target.path(), &artifact),
            (false, 0)
        );
        assert_eq!(
            fs::read(source.path().join("libraries/example.jar")).unwrap(),
            payload
        );
    }
}
#[test]
fn bound_publication_conflict_preserves_target_and_is_never_a_transfer_retry() {
    let payload = b"verified replacement bytes";
    let server = server(payload.to_vec());
    let installer = installer(&server, 1, 0, false);
    let target = fixture();
    fs::create_dir_all(target.path().join("libraries")).unwrap();
    let existing = target.path().join("libraries/existing.jar");
    fs::write(&existing, b"pre-existing data must remain").unwrap();
    let root = InstallDir::bound(fs::File::open(target.path()).unwrap()).unwrap();
    let artifact = Download {
        url: format!("{}/payload", server.base),
        relative: "libraries/existing.jar".into(),
        hash: format!("{:x}", Sha1::digest(payload)),
        size: payload.len() as u64,
    };
    let cancel = AtomicBool::new(false);
    let abort = AtomicBool::new(false);
    let done = AtomicU64::new(0);
    let bytes = AtomicU64::new(0);
    let network = AtomicU64::new(0);
    let group = AtomicU64::new(0);
    let error = installer
        .download_to(
            &root,
            &artifact,
            &cancel,
            &abort,
            &done,
            1,
            &bytes,
            &network,
            artifact.size,
            &group,
            1,
            &|_| {},
        )
        .unwrap_err();
    assert!(error.contains("目标已存在"), "{error}");
    assert_eq!(server.artifact_requests.load(Ordering::Relaxed), 1);
    assert_eq!(done.load(Ordering::Relaxed), 0);
    assert_eq!(group.load(Ordering::Relaxed), 0);
    assert_eq!(
        fs::read(&existing).unwrap(),
        b"pre-existing data must remain"
    );
    assert_eq!(fs::read_dir(existing.parent().unwrap()).unwrap().count(), 1);
    assert_eq!(installer.downloads.status().active_transfers, 0);
}

#[test]
fn cancellation_during_throttled_read_stops_before_another_body_poll() {
    let server = server(vec![8; 1024 * 1024]);
    let installer = installer(&server, 1, 1, false);
    let cancel = AtomicBool::new(false);
    let url = installer.url(&format!("{}/payload", server.base)).unwrap();
    let start = Instant::now();
    thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(70));
            cancel.store(true, Ordering::Release);
        });
        assert!(installer
            .transfer(&url, 1024 * 1024, &cancel, |_| Ok(()))
            .unwrap_err()
            .contains("取消"));
    });
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(installer.downloads.status().active_transfers, 0);
}
