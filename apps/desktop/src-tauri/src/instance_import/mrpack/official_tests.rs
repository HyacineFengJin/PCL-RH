//! Real loopback archive transfers and native source/confirmation ownership.
//! Fixture metadata passes the same exact-file policy; loopback URLs enter only
//! the injected transport helper. Transaction fixtures inject a bundled core
//! after the real mrpack parse, leaving production core resolution unchanged.
use super::*;
use crate::{
    integration_tests::Fixture as ProjectFixture,
    modrinth_install::{
        provider::{FutureResult, Project, Version},
        Compatibility,
    },
    tasks::{TaskKind, TaskOutcome, TaskScope, TaskStage, TaskTarget},
};
use serde_json::{json, Value};
use sha2::Sha512;
use std::{
    fs,
    io::{Cursor, Read, Write},
    net::TcpListener,
    sync::atomic::{AtomicBool, AtomicU64},
    thread,
    time::Duration,
};
use zip::{write::SimpleFileOptions, ZipWriter};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Server {
    base: String,
    routes: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    hits: Arc<Mutex<Vec<String>>>,
    stall: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(pack: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(Mutex::new(BTreeMap::from([
            ("/pack".into(), pack.clone()),
            ("/project".into(), serde_json::to_vec(&json!({"id":"Project1","title":"Example pack","project_type":"modpack"})).unwrap()),
            ("/version".into(), serde_json::to_vec(&json!({"id":"Version1","project_id":"Project1","name":"Example release","version_number":"1","date_published":"2026-01-01T00:00:00Z","version_type":"release","game_versions":["1.21.1"],"loaders":["minecraft"],"files":[{"filename":"example.mrpack","size":pack.len(),"url":"https://cdn.modrinth.com/data/Project1/versions/Version1/example.mrpack","primary":true,"hashes":{"sha512":format!("{:x}",Sha512::digest(&pack))}}]})).unwrap()),
        ])));
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stall = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (data, seen, wait, done) = (routes.clone(), hits.clone(), stall.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let mut connections = Vec::new();
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let (data, seen, wait, done) =
                            (data.clone(), seen.clone(), wait.clone(), done.clone());
                        connections.push(thread::spawn(move || {
                            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                            stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                            let mut request = Vec::new();
                            let mut byte = [0];
                            while request.len() < 8192 && stream.read_exact(&mut byte).is_ok() {
                                request.push(byte[0]);
                                if request.ends_with(b"\r\n\r\n") { break; }
                            }
                            let path = String::from_utf8_lossy(&request).split_whitespace().nth(1).unwrap_or("").to_string();
                            seen.lock().unwrap().push(path.clone());
                            let body = data.lock().unwrap().get(&path).cloned().unwrap_or_default();
                            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                            while path == "/pack" && wait.load(Ordering::SeqCst) && !done.load(Ordering::SeqCst) { thread::sleep(Duration::from_millis(2)); }
                            if !done.load(Ordering::SeqCst) { let _ = stream.write_all(&body); }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            for connection in connections {
                connection.join().unwrap();
            }
        });
        Self {
            base,
            routes,
            hits,
            stall,
            stop,
            worker: Some(worker),
        }
    }
    fn edit(&self, path: &str, edit: impl FnOnce(&mut Value)) {
        let mut routes = self.routes.lock().unwrap();
        let mut value: Value = serde_json::from_slice(&routes[path]).unwrap();
        edit(&mut value);
        routes.insert(path.into(), serde_json::to_vec(&value).unwrap());
    }
    fn metadata(&self) -> Arc<Metadata> {
        Arc::new(Metadata {
            base: self.base.clone(),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        })
    }
    fn payload_hits(&self) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|hit| hit.as_str() == "/pack")
            .count()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Metadata {
    base: String,
    client: reqwest::Client,
}
impl Metadata {
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let bytes = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
}
impl Provider for Metadata {
    fn project<'a>(&'a self, _: &'a str) -> FutureResult<'a, Project> {
        Box::pin(self.get("/project"))
    }
    fn version<'a>(&'a self, _: &'a str) -> FutureResult<'a, Version> {
        Box::pin(self.get("/version"))
    }
    fn versions<'a>(&'a self, _: &'a str, _: &'a Compatibility) -> FutureResult<'a, Vec<Version>> {
        Box::pin(async { Err("unused fixture route".into()) })
    }
    fn from_hashes<'a>(&'a self, _: &'a [String]) -> FutureResult<'a, BTreeMap<String, Version>> {
        Box::pin(async { Err("unused fixture route".into()) })
    }
}
fn request() -> SaveRequest {
    SaveRequest {
        source: crate::resource_save::SaveSource::Modrinth,
        project_id: "Project1".into(),
        version_id: "Version1".into(),
        file_name: "example.mrpack".into(),
    }
}
fn pack(extra: &str) -> Vec<u8> {
    let manifest = json!({"formatVersion":1,"game":"minecraft","versionId":"1","name":"Example pack","dependencies":{"minecraft":"1.21.1"},"files":[{"path":"mods/optional.jar","hashes":{"sha1":"a".repeat(40),"sha512":"a".repeat(128)},"env":{"client":"optional","server":"unsupported"},"downloads":["https://cdn.modrinth.com/data/Project2/versions/Version2/optional.jar"],"fileSize":12}]});
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (path, bytes) in [
        (INDEX, serde_json::to_vec(&manifest).unwrap()),
        ("overrides/config/example.cfg", b"content".to_vec()),
        ("overrides/fixture-core.jar", b"fixture client".to_vec()),
        (extra, b"ignored file".to_vec()),
    ] {
        zip.start_file(path, options).unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
struct Fixture {
    project: ProjectFixture,
    shared: Arc<Shared>,
    root: GameRoot,
}
impl Fixture {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/direct-pack-2026-10-07/fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(path.join("Minecraft/.minecraft")).unwrap();
        let project = ProjectFixture(path.canonicalize().unwrap());
        let shared = Arc::new(project.shared());
        let root = shared.config.resolve(None).unwrap();
        fs::write(
            Path::new(&root.path).join("existing-user-file"),
            b"preserve",
        )
        .unwrap();
        Self {
            project,
            shared,
            root,
        }
    }
    fn pending(&self, server: &Server, id: &str) -> PendingDownload {
        let mut preparation = self.shared.pack_confirmations.preparation().unwrap();
        let authority =
            tauri::async_runtime::block_on(resolve_pack(server.metadata().as_ref(), request()))
                .unwrap();
        self.shared
            .pack_confirmations
            .reserve_input(&mut preparation, authority.file.size)
            .unwrap();
        PendingDownload {
            capture: Capture {
                id: id.into(),
                root_id: self.root.id.clone(),
                root: PathBuf::from(&self.root.path),
                root_key: Dir::open(Path::new(&self.root.path))
                    .unwrap()
                    .key()
                    .unwrap(),
                project: self.project.0.clone(),
            },
            file: service::input_staging(&self.project.0, Path::new(&self.root.path))
                .unwrap()
                .anonymous()
                .unwrap(),
            authority,
            preparation,
        }
    }
    fn input(&self, server: &Server, id: &str) -> Arc<Input> {
        let cancel = AtomicBool::new(false);
        let pending = self.pending(server, id);
        let mut http = HttpProvider::new(&cancel).unwrap();
        http.client = server.metadata().client.clone();
        let metadata = server.metadata();
        let mut input = tauri::async_runtime::block_on(download_checked(
            &http,
            metadata.as_ref(),
            pending,
            http.client.get(format!("{}/pack", server.base)),
            &cancel,
            |_| {},
        ))
        .unwrap();
        input.evidence_provider = Some(metadata);
        let input = Arc::new(input);
        self.shared
            .pack_confirmations
            .inputs
            .register(id.into(), "fixture-input-job".into())
            .unwrap();
        self.shared
            .pack_confirmations
            .inputs
            .publish(input.clone())
            .unwrap();
        input
    }
    fn confirmation(&self, input: Arc<Input>, name: &str) -> PackPlan {
        let cache = &self.shared.pack_confirmations;
        let preparation = cache.preparation().unwrap();
        let mut checked = prepare_checked(input, name, Some(&[]), &AtomicBool::new(false)).unwrap();
        // Exercise real native build/seal/publication without fetching Mojang
        // or starting Java. Production mrpacks always resolve their real core.
        let Output::Override(jar) = checked.outputs.remove("fixture-core.jar").unwrap() else {
            panic!()
        };
        checked.bundled = Some(formats::BundledCore { metadata: serde_json::to_vec(&json!({"id":name,"jar":name,"clientVersion":"1.21.1","mainClass":"net.minecraft.client.main.Main","javaVersion":{"majorVersion":21},"libraries":[],"arguments":{"game":[],"jvm":[]}})).unwrap(), jar, shared: BTreeMap::new() });
        cache
            .confirm_checked(
                preparation,
                &self.root,
                &self.project.0,
                checked,
                &AtomicBool::new(false),
            )
            .unwrap()
    }
    fn gate(&self) -> tasks::TaskHandle {
        self.shared
            .tasks
            .admit_queued(
                TaskTarget {
                    root_id: self.root.id.clone(),
                    root_path: self.root.path.clone(),
                    instance_id: Some("gate".into()),
                },
                TaskKind::Install,
                TaskScope::root(Path::new(&self.root.path)).unwrap(),
            )
            .unwrap()
    }
    fn submit(&self, id: &str, plan: &PackPlan) -> String {
        let _operation = self.shared.operations.lock().unwrap();
        start(
            self.shared.clone(),
            self.root.clone(),
            id.into(),
            plan.name.clone(),
            plan.revision.clone(),
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn wait(&self, id: &str) -> tasks::TaskSnapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = self.shared.tasks.snapshot(id).unwrap();
            if state.stage.is_terminal() {
                return state;
            }
            assert!(Instant::now() < deadline, "worker did not finish");
            thread::sleep(Duration::from_millis(2));
        }
    }
}
fn complete(task: &tasks::TaskHandle) {
    task.finish(TaskOutcome::Complete {
        result: None,
        message: "fixture done".into(),
        error: None,
    })
    .unwrap();
}

#[test]
fn official_identity_and_explicit_mrpack_file_reject_before_payload() {
    let server = Server::new(pack("ignored.txt"));
    for (path, key, value) in [
        ("/project", "project_type", json!("mod")),
        ("/project", "id", json!("Wrong001")),
        ("/version", "project_id", json!("Wrong001")),
        ("/version", "id", json!("Wrong001")),
    ] {
        let before = server.routes.lock().unwrap()[path].clone();
        server.edit(path, |v| v[key] = value);
        assert!(tauri::async_runtime::block_on(resolve_pack(
            server.metadata().as_ref(),
            request()
        ))
        .is_err());
        server.routes.lock().unwrap().insert(path.into(), before);
    }
    let mut wrong_file = request();
    wrong_file.file_name = "other.mrpack".into();
    assert!(
        tauri::async_runtime::block_on(resolve_pack(server.metadata().as_ref(), wrong_file))
            .is_err()
    );
    server.edit("/version", |v| {
        v["files"][0]["size"] = json!(MAX_ARCHIVE + 1)
    });
    assert!(
        tauri::async_runtime::block_on(resolve_pack(server.metadata().as_ref(), request()))
            .is_err()
    );
    assert_eq!(server.payload_hits(), 0);
    assert!(provider::cdn_url(&format!("{}/pack", server.base)).is_err());
}

#[test]
fn verified_anonymous_input_reuses_body_for_names_and_optional_choices() {
    let f = Fixture::new();
    let server = Server::new(pack("ignored.txt"));
    let input = f.input(&server, "pack-source-v1:fixture");
    assert_eq!(input.file.metadata().unwrap().nlink(), 0);
    let first =
        prepare_checked(input.clone(), "First", Some(&[]), &AtomicBool::new(false)).unwrap();
    let second = prepare_checked(
        input.clone(),
        "Second",
        Some(&["mods/optional.jar".into()]),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!first.outputs.contains_key("mods/optional.jar"));
    assert!(second.outputs.contains_key("mods/optional.jar"));
    first.recheck_source(&AtomicBool::new(false)).unwrap();
    second.recheck_source(&AtomicBool::new(false)).unwrap();
    assert_eq!(server.payload_hits(), 1);
    assert!(!Path::new(&f.root.path).join("versions").exists());
    f.shared.pack_confirmations.release_source(&input.id);
    drop((input, first, second));
    assert_eq!(f.shared.pack_confirmations.input_usage(), (0, 0, 0, 0));
}

#[test]
fn corrupt_body_and_metadata_change_release_download_reservation() {
    let f = Fixture::new();
    let server = Server::new(pack("ignored.txt"));
    let cancel = AtomicBool::new(false);
    for changed_metadata in [false, true] {
        let pending = f.pending(&server, "pack-source-v1:failure");
        if changed_metadata {
            server.edit("/version", |v| v["name"] = json!("changed"));
        } else {
            server.routes.lock().unwrap().get_mut("/pack").unwrap()[0] ^= 1;
        }
        let mut http = HttpProvider::new(&cancel).unwrap();
        http.client = server.metadata().client.clone();
        let result = tauri::async_runtime::block_on(download_checked(
            &http,
            server.metadata().as_ref(),
            pending,
            http.client.get(format!("{}/pack", server.base)),
            &cancel,
            |_| {},
        ));
        assert!(result.is_err());
        assert_eq!(f.shared.pack_confirmations.input_usage(), (0, 0, 0, 0));
        if !changed_metadata {
            server.routes.lock().unwrap().get_mut("/pack").unwrap()[0] ^= 1;
        }
    }
    assert_eq!(
        fs::read(Path::new(&f.root.path).join("existing-user-file")).unwrap(),
        b"preserve"
    );
}

#[test]
fn download_cancellation_closes_body_and_preparing_window() {
    let f = Fixture::new();
    let server = Server::new(pack("ignored.txt"));
    let cancel = AtomicBool::new(false);
    let pending = f.pending(&server, "pack-source-v1:cancel");
    assert_eq!(f.shared.pack_confirmations.input_usage().3, 1);
    server.stall.store(true, Ordering::SeqCst);
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let mut http = HttpProvider::new(&cancel).unwrap();
            http.client = server.metadata().client.clone();
            tauri::async_runtime::block_on(download_checked(
                &http,
                server.metadata().as_ref(),
                pending,
                http.client.get(format!("{}/pack", server.base)),
                &cancel,
                |_| {},
            ))
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while server.payload_hits() == 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        cancel.store(true, Ordering::SeqCst);
        assert!(worker.join().unwrap().is_err());
    });
    assert_eq!(f.shared.pack_confirmations.input_usage(), (0, 0, 0, 0));
}

#[test]
fn malformed_archive_and_replaced_root_cannot_be_confirmed() {
    let f = Fixture::new();
    let server = Server::new(pack("../escape.txt"));
    let input = f.input(&server, "pack-source-v1:invalid");
    assert!(prepare_checked(input.clone(), "Bad", Some(&[]), &AtomicBool::new(false)).is_err());
    fs::rename(&f.root.path, format!("{}.old", f.root.path)).unwrap();
    fs::create_dir(&f.root.path).unwrap();
    assert!(f
        .shared
        .pack_confirmations
        .inputs
        .get(&input.id, &f.root, &f.project.0)
        .is_err());
    assert!(prepare_checked(input, "Bad", Some(&[]), &AtomicBool::new(false)).is_err());
    assert!(!Path::new(&f.root.path).join("versions").exists());
}

#[test]
fn late_input_cannot_return_after_release_and_unclaimed_plans_are_retired() {
    let f = Fixture::new();
    let server = Server::new(pack("ignored.txt"));
    let input = f.input(&server, "pack-source-v1:late");
    let plan = f.confirmation(input.clone(), "Late");
    f.shared.pack_confirmations.release_source(&input.id);
    assert!(f
        .shared
        .pack_confirmations
        .inputs
        .publish(input.clone())
        .is_err());
    assert!(start(
        f.shared.clone(),
        f.root.clone(),
        input.id.clone(),
        plan.name.clone(),
        plan.revision
    )
    .is_err());
    drop(input);
    assert_eq!(f.shared.pack_confirmations.input_usage(), (0, 0, 0, 0));
}

#[test]
fn real_mrpack_input_queue_and_publication_preserve_existing_files() {
    let f = Fixture::new();
    let server = Server::new(pack("ignored.txt"));
    let input = f.input(&server, "pack-source-v1:publish");
    let source = input.id.clone();
    let plan = f.confirmation(input, "Installed");
    let gate = f.gate();
    let id = f.submit(&source, &plan);
    assert_eq!(
        f.shared.tasks.snapshot(&id).unwrap().stage,
        TaskStage::Queued
    );
    assert!(!Path::new(&f.root.path).join("versions").exists());
    assert!(f
        .shared
        .pack_confirmations
        .inputs
        .get(&source, &f.root, &f.project.0)
        .is_err());
    assert!(f.shared.pack_confirmations.input_usage().2 > 0);
    complete(&gate);
    let done = f.wait(&id);
    assert_eq!(done.stage, TaskStage::Complete, "{:?}", done.error);
    let version = Path::new(&f.root.path).join("versions/Installed");
    assert_eq!(
        fs::read(version.join("Installed.jar")).unwrap(),
        b"fixture client"
    );
    assert_eq!(
        fs::read(version.join("config/example.cfg")).unwrap(),
        b"content"
    );
    assert!(!version.join("mods/optional.jar").exists());
    assert_eq!(
        fs::read(Path::new(&f.root.path).join("existing-user-file")).unwrap(),
        b"preserve"
    );
    assert_eq!(server.payload_hits(), 1);
    assert_eq!(f.shared.pack_confirmations.input_usage(), (0, 0, 0, 0));
}

#[test]
fn queued_cancel_and_changed_official_file_never_publish_instance() {
    for changed in [false, true] {
        let f = Fixture::new();
        let server = Server::new(pack("ignored.txt"));
        let input = f.input(&server, "pack-source-v1:queued");
        let source = input.id.clone();
        let plan = f.confirmation(input, "Refused");
        let gate = f.gate();
        let id = f.submit(&source, &plan);
        if changed {
            server.edit("/version", |v| {
                v["files"][0]["url"] = json!("https://cdn.modrinth.com/changed.mrpack")
            });
            complete(&gate);
        } else {
            f.shared.tasks.cancel(&id).unwrap();
        }
        let done = f.wait(&id);
        assert_eq!(
            done.stage,
            if changed {
                TaskStage::Error
            } else {
                TaskStage::Cancelled
            }
        );
        assert!(!Path::new(&f.root.path).join("versions/Refused").exists());
        assert_eq!(f.shared.pack_confirmations.input_usage(), (0, 0, 0, 0));
        if !changed {
            complete(&gate);
        }
    }
}

#[test]
fn preparation_limits_and_source_scope_hold_before_body_and_across_eviction() {
    let f = Fixture::new();
    let server = Server::new(pack("ignored.txt"));
    let pending = (0..4)
        .map(|n| f.pending(&server, &format!("pack-source-v1:{n}")))
        .collect::<Vec<_>>();
    assert_eq!(f.shared.pack_confirmations.input_usage().3, 4);
    assert!(f.shared.pack_confirmations.preparation().is_err());
    assert_eq!(server.payload_hits(), 0);
    drop(pending);
    assert_eq!(f.shared.pack_confirmations.input_usage(), (0, 0, 0, 0));
    let input = f.input(&server, "pack-source-v1:scoped");
    let mut other = f.root.clone();
    other.id = "another-root".into();
    assert!(f
        .shared
        .pack_confirmations
        .inputs
        .get(&input.id, &other, &f.project.0)
        .is_err());
    let plan = f.confirmation(input.clone(), "Queued");
    let gate = f.gate();
    let id = f.submit(&input.id, &plan);
    // The source record has been removed, but the queued worker's input lease
    // survives. Closing an old view cannot revoke it or the admitted turn.
    f.shared.pack_confirmations.release_source(&input.id);
    drop(input);
    assert!(f.shared.pack_confirmations.input_usage().2 > 0);
    f.shared.tasks.cancel(&id).unwrap();
    assert_eq!(f.wait(&id).stage, TaskStage::Cancelled);
    assert_eq!(f.shared.pack_confirmations.input_usage(), (0, 0, 0, 0));
    complete(&gate);
}
