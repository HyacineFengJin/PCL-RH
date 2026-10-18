use super::*;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    sync::{atomic::Ordering, Mutex},
    thread,
    time::Instant,
};

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    archive: PathBuf,
}
impl Fixture {
    fn new(files: Value) -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/launch-readiness-2026-10-18/cf-pack-fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let root = base.join("root");
        fs::create_dir_all(&root).unwrap();
        let archive = base.join("pack.zip");
        let file = fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(json!({"manifestType":"minecraftModpack", "manifestVersion":1,"name":"Synthetic pack","version":"1","minecraft":{"version":"1.21.1","modLoaders":[]},"files":files,"overrides":"overrides"}).to_string().as_bytes()).unwrap();
        zip.start_file(
            "overrides/config/example.cfg",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"synthetic").unwrap();
        zip.finish().unwrap();
        Self {
            base,
            root,
            archive,
        }
    }
    fn checked(&self) -> CheckedPack {
        prepare_checked(&self.root, &self.archive, "CF-fixture", None).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}

fn project(allowed: bool) -> Value {
    json!({"data":{"id":123,"gameId":432,"name":"Synthetic project","slug":"synthetic-project","links":{"websiteUrl":"https://www.curseforge.com/minecraft/mc-mods/synthetic-project"},"summary":"Synthetic","downloadCount":0,"classId":6,"dateModified":"2026-10-18T00:00:00Z","allowModDistribution":allowed,"isAvailable":true}})
}
fn file(id: u32) -> Value {
    json!({"data":{"id":id,"gameId":432,"modId":123,"isAvailable":true,"displayName":"Synthetic file","fileName":format!("synthetic-{id}.jar"),"releaseType":1,"fileDate":"2026-10-18T00:00:00Z","fileLength":12,"downloadCount":0,"downloadUrl":format!("https://edge.forgecdn.net/files/1/{id}/synthetic.jar"),"hashes":[{"algo":1,"value":"a".repeat(40)}],"gameVersions":["1.21.1","Fabric"]}})
}
struct Server {
    address: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(replies: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            for reply in replies {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    if stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while request.len() < 16384 && !request.windows(4).any(|s| s == b"\r\n\r\n") {
                    let mut bytes = [0; 2048];
                    match stream.read(&mut bytes) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&bytes[..count]),
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                let body = serde_json::to_vec(&reply).unwrap();
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self {
            address,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn client(&self) -> CurseforgeClient {
        let network = pcl_network::ClientFactory::new(pcl_network::Policy {
            proxy: pcl_network::ProxyPolicy::None,
            dns: pcl_network::DnsPolicy::System,
        })
        .unwrap();
        CurseforgeClient::loopback(Arc::new(network), self.address.clone())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn ids() -> Value {
    json!([{"projectID":123,"fileID":456,"required":true},{"projectID":123,"fileID":457,"required":false}])
}
fn one() -> Value {
    json!([{"projectID":123,"fileID":456,"required":true}])
}
fn resolved(id: u32, path: &str, size: u64) -> ResolvedFile {
    ResolvedFile {
        project_id: 123,
        file_id: id,
        path: path.into(),
        size,
        sha1: "a".repeat(40),
        url: format!("https://edge.forgecdn.net/files/1/{id}/synthetic.jar"),
    }
}

#[test]
fn exact_official_ids_resolve_every_manifest_entry_and_recheck_all_download_facts() {
    let fixture = Fixture::new(ids());
    let mut checked = fixture.checked();
    assert_eq!(checked.curseforge_files, [(123, 456), (123, 457)]);
    assert!(checked.plan.preview.blockers.contains(&blocker(2)));
    assert!(checked
        .plan
        .warnings
        .iter()
        .any(|value| value.contains("全部列项")));
    let mut changed = file(457);
    changed["data"]["hashes"][0]["value"] = "b".repeat(40).into();
    let server = Server::new(vec![
        project(true),
        file(456),
        project(true),
        file(457),
        project(true),
        file(456),
        project(true),
        file(457),
        project(true),
        file(456),
        project(true),
        changed,
    ]);
    let client = server.client();
    let cancel = AtomicBool::new(false);
    resolve(&mut checked, &client, &cancel).unwrap();
    assert!(checked.plan.preview.blockers.is_empty());
    assert_eq!(checked.plan.preview.download_bytes, 24);
    assert_eq!(checked.plan.file_count, 3);
    assert!(
        matches!(&checked.outputs["mods/synthetic-457.jar"], Output::CurseForge(file) if file.file_id == 457)
    );
    recheck(&checked, Some(&client), &cancel).unwrap();
    assert!(recheck(&checked, Some(&client), &cancel)
        .unwrap_err()
        .contains("已变化"));
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 12);
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("x-api-key: synthetic-test-key\r\n")));
    assert_eq!(
        requests[1].lines().next().unwrap(),
        "GET /v1/mods/123/files/456 HTTP/1.1"
    );
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[test]
fn author_revocation_blocks_worker_recheck_before_file_lookup() {
    let fixture = Fixture::new(one());
    let mut checked = fixture.checked();
    let server = Server::new(vec![project(true), file(456), project(false)]);
    let client = server.client();
    resolve(&mut checked, &client, &AtomicBool::new(false)).unwrap();
    assert!(recheck(&checked, Some(&client), &AtomicBool::new(false))
        .unwrap_err()
        .contains("禁止第三方分发"));
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[test]
fn failed_or_cancelled_metadata_cannot_leave_partial_download_authority() {
    let fixture = Fixture::new(ids());
    let mut checked = fixture.checked();
    let before = checked.plan.revision.clone();
    let mut missing = file(457);
    missing["data"]["downloadUrl"] = Value::Null;
    let server = Server::new(vec![project(true), file(456), project(true), missing]);
    let client = server.client();
    assert!(resolve(&mut checked, &client, &AtomicBool::new(true)).is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 0);
    assert!(resolve(&mut checked, &client, &AtomicBool::new(false)).is_err());
    assert_eq!(checked.outputs.len(), 1);
    assert_eq!(checked.plan.revision, before);
    assert!(checked.plan.preview.blockers.contains(&blocker(2)));
}

#[test]
fn output_collision_missing_ids_and_size_limits_reject_the_whole_resolution() {
    let fixture = Fixture::new(ids());
    for files in [
        vec![resolved(456, "mods/a.jar", 12)],
        vec![
            resolved(456, "mods/a.jar", 12),
            resolved(457, "mods/a.jar", 12),
        ],
        vec![
            resolved(456, "config/example.cfg", 12),
            resolved(457, "mods/b.jar", 12),
        ],
        vec![resolved(456, "config", 12), resolved(457, "mods/b.jar", 12)],
        vec![
            resolved(456, "mods/a.jar", MAX_OUTPUT),
            resolved(457, "mods/b.jar", 12),
        ],
    ] {
        let mut checked = fixture.checked();
        let before = checked.plan.revision.clone();
        assert!(apply(&mut checked, files).is_err());
        assert_eq!(checked.outputs.len(), 1);
        assert_eq!(checked.plan.revision, before);
    }
}

#[test]
fn native_revision_binds_official_url_hash_size_path_and_game_components() {
    let fixture = Fixture::new(one());
    let mut first = fixture.checked();
    apply(&mut first, vec![resolved(456, "mods/a.jar", 12)]).unwrap();
    for change in 0..5 {
        let mut checked = fixture.checked();
        let mut file = resolved(456, "mods/a.jar", 12);
        match change {
            0 => file.url.push_str("?v=2"),
            1 => file.sha1 = "b".repeat(40),
            2 => file.size += 1,
            3 => file.path = "mods/b.jar".into(),
            _ => checked.plan.preview.dependencies[0].version = "1.20.1".into(),
        }
        apply(&mut checked, vec![file]).unwrap();
        assert_ne!(checked.plan.revision, first.plan.revision);
    }
}
