//! Loopback provider and executable fixtures cover the actual publication and
//! failure paths. They neither contact Mojang nor execute an installed JVM.
use super::*;
use crate::{
    integration_tests::Fixture,
    tasks::{TaskKind, TaskOutcome, TaskScope, TaskTarget, Tasks},
};
use catalog::{Download, Entry, Manifest};
use network::Http;
use sha1::{Digest, Sha1};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::symlink,
    time::Duration,
};

fn hash(body: &[u8]) -> String {
    format!("{:x}", Sha1::digest(body))
}
fn source(body: &[u8], leaf: &str) -> Download {
    let sha1 = hash(body);
    Download {
        url: format!("https://piston-data.mojang.com/v1/objects/{sha1}/{leaf}"),
        sha1,
        size: body.len() as u64,
    }
}
fn file(body: &[u8], leaf: &str, executable: bool) -> Entry {
    Entry::File {
        executable,
        downloads: BTreeMap::from([("raw".into(), source(body, leaf))]),
    }
}
fn script(major: u32, arch: &str) -> Vec<u8> {
    format!("#!/bin/sh\nprintf 'java.specification.version = {major}\\njava.vendor = Fixture JVM\\nos.arch = {arch}\\n' >&2\n").into_bytes()
}
fn plan(body: &[u8]) -> Manifest {
    Manifest {
        files: BTreeMap::from([
            ("bin".into(), Entry::Directory),
            ("bin/java".into(), file(body, "java", true)),
            ("legal".into(), Entry::Directory),
            ("legal/LICENSE".into(), file(b"license", "LICENSE", false)),
            (
                "legal/alias".into(),
                Entry::Link {
                    target: "LICENSE".into(),
                },
            ),
            ("man".into(), Entry::Directory),
            ("man/ja_JP.UTF-8".into(), Entry::Directory),
            (
                "man/ja".into(),
                Entry::Link {
                    target: "ja_JP.UTF-8".into(),
                },
            ),
        ]),
    }
}
fn package(manifest: &Manifest) -> Package {
    let raw = serde_json::to_vec(manifest).unwrap();
    let sha1 = hash(&raw);
    Package {
        component: "java-runtime-delta".into(),
        version: "21.0.1".into(),
        major: 21,
        platform: catalog::platform().unwrap().into(),
        released: "2026-01-01T00:00:00Z".into(),
        manifest: Download {
            url: format!("https://piston-meta.mojang.com/v1/packages/{sha1}/manifest.json"),
            sha1: sha1.clone(),
            size: raw.len() as u64,
        },
        sha1,
    }
}
fn catalog_body(packages: &[Package]) -> Vec<u8> {
    let mut components = serde_json::Map::new();
    for p in packages {
        components.insert(p.component.clone(), serde_json::json!([{"manifest":p.manifest,"version":{"name":p.version,"released":p.released}}]));
    }
    serde_json::to_vec(&serde_json::json!({catalog::platform().unwrap():components})).unwrap()
}
fn task(tasks: &Arc<Tasks>, store: &Store) -> TaskHandle {
    tasks
        .admit_queued(
            TaskTarget {
                root_id: "launcher".into(),
                root_path: store.path.display().to_string(),
                instance_id: None,
            },
            TaskKind::JavaInstall,
            TaskScope::root(&store.path).unwrap(),
        )
        .unwrap()
}

#[test]
fn java_queue_retains_cancelled_scope_until_cleanup_and_allows_other_roots() {
    let f = Fixture::new();
    let store = Store::capture(&f.0).unwrap();
    let tasks = Arc::new(Tasks::new());
    let first = task(&tasks, &store);
    let second = task(&tasks, &store);
    let other_root = f.0.join("Minecraft/.minecraft");
    let other = tasks
        .admit_queued(
            TaskTarget {
                root_id: "game".into(),
                root_path: other_root.display().to_string(),
                instance_id: Some("Example".into()),
            },
            TaskKind::Install,
            TaskScope::root(&other_root).unwrap(),
        )
        .unwrap();
    other.wait_turn().unwrap();
    assert_eq!(
        tasks.snapshot(second.id()).unwrap().stage,
        crate::tasks::TaskStage::Queued
    );
    tasks.cancel(first.id()).unwrap();
    assert_eq!(
        tasks.snapshot(second.id()).unwrap().stage,
        crate::tasks::TaskStage::Queued
    );
    first.finish(TaskOutcome::Failed(CANCELLED.into()));
    second.wait_turn().unwrap();
    second.begin_finishing();
    tasks.cancel(second.id()).unwrap();
    assert!(!second.cancellation_token().load(Ordering::SeqCst));
    second.finish(TaskOutcome::Complete {
        result: None,
        message: "fixture complete".into(),
        error: None,
    });
    other.finish(TaskOutcome::Complete {
        result: None,
        message: "fixture complete".into(),
        error: None,
    });
}
fn populate(stage: &files::Stage<'_>, manifest: &Manifest, java: &[u8]) {
    stage.prepare_directories().unwrap();
    for (path, entry) in &manifest.files {
        if let Entry::File { executable, .. } = entry {
            let mut anonymous = stage.anonymous(path).unwrap();
            anonymous
                .write_all(if path == "bin/java" { java } else { b"license" })
                .unwrap();
            stage.publish_file(path, &anonymous, *executable).unwrap();
        }
    }
    stage.links().unwrap();
}

#[test]
fn catalog_prefers_latest_stable_per_major_and_keeps_exact_identity() {
    let m = plan(&script(21, std::env::consts::ARCH));
    let p = package(&m);
    let mut old = p.clone();
    old.component = "java-runtime-beta".into();
    old.released = "2025-01-01T00:00:00Z".into();
    let mut snapshot = p.clone();
    snapshot.component = "java-runtime-delta-snapshot".into();
    let mut legacy = p.clone();
    legacy.component = "jre-legacy".into();
    legacy.version = "8u202".into();
    legacy.major = 8;
    let catalog = catalog::parse_catalog(
        &catalog_body(&[p.clone(), old, snapshot, legacy]),
        catalog::platform().unwrap(),
    )
    .unwrap();
    assert_eq!(catalog.iter().map(|p| p.major).collect::<Vec<_>>(), [21, 8]);
    assert_eq!(catalog[0], p);
    assert!(catalog::parse_catalog(&catalog_body(&[p]), "linux-arm64").is_err());
}
#[test]
fn catalog_rejects_foreign_manifest_identity_and_malformed_requests() {
    let mut p = package(&plan(&script(21, std::env::consts::ARCH)));
    p.manifest.url = "https://example.invalid/manifest.json".into();
    assert!(catalog::parse_catalog(&catalog_body(&[p]), catalog::platform().unwrap()).is_err());
    for (name, digest) in [
        ("../delta", "a".repeat(40)),
        ("delta", "A".repeat(40)),
        ("delta", "a".repeat(39)),
    ] {
        assert!(validate_request(name, &digest).is_err());
    }
}
#[test]
fn manifest_rejects_traversal_link_escape_cycles_and_link_parents() {
    let base = plan(&script(21, std::env::consts::ARCH));
    assert!(base.validate().is_ok()); // Includes the legacy JRE's directory alias.
    for path in [
        "../outside",
        "bin/../../outside",
        "bin\\java",
        "/outside",
        "bin//java",
    ] {
        let mut m = base.clone();
        m.files.insert(path.into(), Entry::Directory);
        assert!(m.validate().is_err(), "{path}");
    }
    for target in ["../../outside", "/outside", "alias"] {
        let mut m = base.clone();
        m.files.insert(
            "legal/alias".into(),
            Entry::Link {
                target: target.into(),
            },
        );
        assert!(m.validate().is_err(), "{target}");
    }
    let mut m = base.clone();
    m.files
        .insert("man/ja/file".into(), file(b"license", "LICENSE", false));
    assert!(m.validate().is_err());
    let mut m = base.clone();
    m.files.insert("man/deep".into(), Entry::Directory);
    m.files.insert(
        "man/deep/short".into(),
        Entry::Link {
            target: "../../legal".into(),
        },
    );
    m.files.insert("outside".into(), Entry::Directory);
    m.files.insert(
        "legal/alias".into(),
        Entry::Link {
            target: "../man/deep/short/../../../outside".into(),
        },
    );
    assert!(
        m.validate().is_err(),
        "lexical normalization must not authorize alias/.. traversal"
    );
    let mut m = base;
    m.files.remove("bin");
    assert!(m.validate().is_err());
}
#[test]
fn manifest_binds_raw_url_hash_size_and_executable_entry() {
    let base = plan(&script(21, std::env::consts::ARCH));
    for suffix in [
        "../different/java",
        "java?token=x",
        "java#fragment",
        "nested/java",
        "",
    ] {
        let mut m = base.clone();
        if let Entry::File { downloads, .. } = m.files.get_mut("bin/java").unwrap() {
            let raw = downloads.get_mut("raw").unwrap();
            raw.url = format!(
                "https://piston-data.mojang.com/v1/objects/{}/{suffix}",
                raw.sha1
            );
        }
        assert!(m.validate().is_err(), "{suffix}");
    }
    let mut m = base.clone();
    if let Entry::File { downloads, .. } = m.files.get_mut("bin/java").unwrap() {
        downloads.get_mut("raw").unwrap().size = 512 * 1024 * 1024 + 1;
    }
    assert!(m.validate().is_err());
    let mut m = base;
    if let Entry::File { executable, .. } = m.files.get_mut("bin/java").unwrap() {
        *executable = false;
    }
    assert!(m.validate().is_err());
}
#[test]
fn cancelled_partial_file_has_no_name_and_cleanup_removes_only_owned_stage() {
    let f = Fixture::new();
    let store = Store::capture(&f.0).unwrap();
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let java = script(21, std::env::consts::ARCH);
    let m = plan(&java);
    let p = package(&m);
    let stage = store.create(&p, &m, "partial", &lock).unwrap();
    stage.prepare_directories().unwrap();
    let mut anonymous = stage.anonymous("bin/java").unwrap();
    anonymous.write_all(b"incomplete").unwrap();
    assert!(!store.path.join(".stage-partial/bin/java").exists());
    drop(anonymous);
    stage.cleanup().unwrap();
    assert_eq!(store.dir.names().unwrap(), [".java-install.lock"]);
}
#[test]
fn complete_runtime_publishes_without_replacement_and_preserves_leaf_links() {
    let f = Fixture::new();
    let store = Store::capture(&f.0).unwrap();
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let java = script(21, std::env::consts::ARCH);
    let m = plan(&java);
    let p = package(&m);
    let mut stage = store.create(&p, &m, "complete", &lock).unwrap();
    populate(&stage, &m, &java);
    stage.verify_complete().unwrap();
    let path = stage.publish().unwrap();
    stage.cleanup().unwrap();
    assert_eq!(fs::read(&path).unwrap(), java);
    assert_eq!(
        fs::read_link(path.parent().unwrap().parent().unwrap().join("man/ja")).unwrap(),
        PathBuf::from("ja_JP.UTF-8")
    );
    assert_eq!(pcl_core::java::managed_runtime_paths(&f.0), [path]);
    assert!(store.create(&p, &m, "duplicate", &lock).is_err());
}
#[test]
fn publication_race_never_overwrites_target_and_can_clean_its_own_stage() {
    let f = Fixture::new();
    let store = Store::capture(&f.0).unwrap();
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let java = script(21, std::env::consts::ARCH);
    let m = plan(&java);
    let p = package(&m);
    let mut stage = store.create(&p, &m, "race", &lock).unwrap();
    populate(&stage, &m, &java);
    let target = store.path.join(format!("{}-{}", p.component, p.sha1));
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep"), b"mine").unwrap();
    assert!(stage.publish().is_err());
    stage.cleanup().unwrap();
    assert_eq!(fs::read(target.join("keep")).unwrap(), b"mine");
}
#[test]
fn changed_file_extra_entry_and_changed_link_are_retained_on_cleanup() {
    for change in ["file", "extra", "link"] {
        let f = Fixture::new();
        let store = Store::capture(&f.0).unwrap();
        let lock = store.dir.lock(".java-install.lock").unwrap();
        let java = script(21, std::env::consts::ARCH);
        let m = plan(&java);
        let p = package(&m);
        let stage = store.create(&p, &m, "changed", &lock).unwrap();
        populate(&stage, &m, &java);
        let root = store.path.join(".stage-changed");
        match change {
            "file" => fs::write(root.join("bin/java"), b"edited by user").unwrap(),
            "extra" => fs::write(root.join("keep"), b"user bytes").unwrap(),
            "link" => {
                fs::remove_file(root.join("legal/alias")).unwrap();
                symlink("/outside", root.join("legal/alias")).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(stage.cleanup().is_err(), "{change}");
        assert!(root.join(".pcl-java-owner.json").exists());
        assert!(root.join("bin/java").exists());
    }
}
#[test]
fn recovery_cleans_owned_subset_but_retains_unknown_or_future_records() {
    let f = Fixture::new();
    let store = Store::capture(&f.0).unwrap();
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let java = script(21, std::env::consts::ARCH);
    let m = plan(&java);
    let p = package(&m);
    {
        let stage = store.create(&p, &m, "interrupted", &lock).unwrap();
        stage.prepare_directories().unwrap();
    }
    store.recover(&lock).unwrap();
    assert!(!store.path.join(".stage-interrupted").exists());
    {
        let stage = store.create(&p, &m, "future", &lock).unwrap();
        stage.prepare_directories().unwrap();
    }
    let marker = store.path.join(".stage-future/.pcl-java-owner.json");
    let mut owner: serde_json::Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    owner["schema"] = serde_json::json!(2);
    let bytes = serde_json::to_vec(&owner).unwrap();
    fs::write(&marker, &bytes).unwrap();
    assert!(store.recover(&lock).is_err());
    assert_eq!(fs::read(&marker).unwrap(), bytes);
}
#[test]
fn replaced_stage_and_symlinked_parent_are_never_cleanup_authority() {
    let f = Fixture::new();
    let store = Store::capture(&f.0).unwrap();
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let java = script(21, std::env::consts::ARCH);
    let m = plan(&java);
    let p = package(&m);
    let stage = store.create(&p, &m, "retarget", &lock).unwrap();
    stage.prepare_directories().unwrap();
    let root = store.path.join(".stage-retarget");
    let kept = store.path.join("original");
    fs::rename(&root, &kept).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("keep"), b"new owner").unwrap();
    assert!(stage.java_path().is_err());
    assert!(stage.cleanup().is_err());
    assert_eq!(fs::read(root.join("keep")).unwrap(), b"new owner");
    assert!(kept.join(".pcl-java-owner.json").exists());
    fs::remove_dir(kept.join("bin")).unwrap();
    symlink(&root, kept.join("bin")).unwrap();
    assert!(stage.anonymous("bin/java").is_err());
}

/// A bounded local provider keeps production URL validation in place; only the
/// test transport rewrites the already-validated official URL to loopback.
struct Server {
    origin: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new(mut reply: impl FnMut(&str) -> (String, Vec<u8>) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if stopped.load(Ordering::SeqCst) {
                            break;
                        }
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
                                break;
                            }
                            header.push(byte[0]);
                        }
                        let request = String::from_utf8_lossy(&header);
                        let path = request.split_whitespace().nth(1).unwrap_or("");
                        let (status, body) = reply(path);
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            origin,
            stop,
            worker: Some(worker),
        }
    }
    fn http(&self, scheduler: Arc<pcl_network::DownloadScheduler>) -> Http {
        let factory = pcl_network::ClientFactory::new(pcl_network::Policy {
            proxy: pcl_network::ProxyPolicy::None,
            ..Default::default()
        })
        .unwrap();
        let mut http = Http::new(Arc::new(factory), scheduler).unwrap();
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
async fn installation(failure: &str) -> (Fixture, Store, Package, Result<Installed>) {
    let f = Fixture::new();
    let store = Store::capture(&f.0).unwrap();
    let tasks = Arc::new(Tasks::new());
    let task = task(&tasks, &store);
    task.wait_turn().unwrap();
    let java = script(
        if failure == "major" { 17 } else { 21 },
        if failure == "arch" {
            "unsupported-arch"
        } else {
            std::env::consts::ARCH
        },
    );
    let m = plan(&java);
    let p = package(&m);
    let all = catalog_body(&[p.clone()]);
    let metadata = serde_json::to_vec(&m).unwrap();
    let cancel = task.cancellation_token();
    let mode = failure.to_string();
    let mut reads = 0;
    let server = Server::new(move |path| {
        if path.ends_with("/all.json") {
            reads += 1;
            return (
                "200 OK".into(),
                if mode == "identity" && reads > 1 {
                    b"{}".to_vec()
                } else {
                    all.clone()
                },
            );
        }
        if path.ends_with("/manifest.json") {
            return (
                "200 OK".into(),
                if mode == "manifest" {
                    vec![b' '; metadata.len()]
                } else {
                    metadata.clone()
                },
            );
        }
        if mode == "cancel" {
            cancel.store(true, Ordering::SeqCst);
        }
        let body = if path.ends_with("/java") {
            if mode == "checksum" {
                vec![b' '; java.len()]
            } else {
                java.clone()
            }
        } else {
            b"license".to_vec()
        };
        (
            if mode == "redirect" {
                "302 Found"
            } else {
                "200 OK"
            }
            .into(),
            body,
        )
    });
    let scheduler = pcl_network::DownloadScheduler::new(Default::default()).unwrap();
    let result = install(
        &store,
        &p.component,
        &p.sha1,
        server.http(scheduler.clone()),
        &task,
    )
    .await;
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
    task.finish(TaskOutcome::Failed("fixture finished".into()));
    (f, store, p, result)
}
#[tokio::test]
async fn official_identity_checked_installation_publishes_probe_verified_runtime() {
    let (f, store, p, result) = installation("").await;
    let installed = result.unwrap();
    assert_eq!(installed.runtime.major, 21);
    assert_eq!(
        installed.path,
        store
            .path
            .join(format!("{}-{}", p.component, p.sha1))
            .join("bin/java")
    );
    assert_eq!(
        pcl_core::java::managed_runtime_paths(&f.0),
        [installed.path]
    );
    assert!(!store
        .dir
        .names()
        .unwrap()
        .iter()
        .any(|s| s.starts_with(".stage-")));
}
#[tokio::test]
async fn checksum_manifest_redirect_identity_and_probe_failures_do_not_publish() {
    for mode in [
        "checksum", "manifest", "redirect", "identity", "major", "arch",
    ] {
        let (_f, store, _p, result) = installation(mode).await;
        assert!(result.is_err(), "{mode}");
        assert_eq!(store.dir.names().unwrap(), [".java-install.lock"], "{mode}");
    }
}
#[tokio::test]
async fn cancelled_download_cleans_its_stage_and_releases_shared_transfer_slot() {
    let (_f, store, _p, result) = installation("cancel").await;
    assert_eq!(result.err().unwrap(), CANCELLED);
    assert_eq!(store.dir.names().unwrap(), [".java-install.lock"]);
}
#[tokio::test]
async fn cancellation_while_waiting_for_transfer_never_sends_payload_request() {
    let scheduler = pcl_network::DownloadScheduler::new(pcl_network::DownloadPolicy {
        max_concurrent_transfers: 1,
        ..Default::default()
    })
    .unwrap();
    let token = Arc::new(AtomicBool::new(false));
    let permit = scheduler.acquire(&token).await.unwrap();
    let server = Server::new(|_| panic!("queued request contacted provider"));
    let http = server.http(scheduler.clone());
    let f = Fixture::new();
    let mut out = fs::File::create(f.0.join("anonymous-fixture")).unwrap();
    let cancel = token.clone();
    let setter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        cancel.store(true, Ordering::SeqCst);
    });
    assert_eq!(
        http.file(
            &source(b"java", "java"),
            &mut out,
            &token,
            &|| Ok(()),
            |_| {}
        )
        .await
        .unwrap_err(),
        CANCELLED
    );
    setter.join().unwrap();
    drop(permit);
    assert_eq!(scheduler.status().active_transfers, 0);
    assert_eq!(scheduler.status().queued_transfers, 0);
    assert_eq!(out.metadata().unwrap().len(), 0);
}

fn auto_fixture() -> Fixture {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../work/launch-readiness-2026-10-18/auto-java-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(path.join("Minecraft/.minecraft")).unwrap();
    Fixture(path.canonicalize().unwrap())
}

#[tokio::test]
async fn recommended_java8_uses_the_official_managed_installation_engine() {
    let fixture = auto_fixture();
    let store = Store::capture(&fixture.0).unwrap();
    let tasks = Arc::new(Tasks::new());
    let task = task(&tasks, &store);
    let java = script(8, std::env::consts::ARCH);
    let manifest = plan(&java);
    let mut package = package(&manifest);
    package.major = 8;
    package.version = "1.8.0_451".into();
    package.component = "jre-legacy".into();
    let all = catalog_body(&[package]);
    let metadata = serde_json::to_vec(&manifest).unwrap();
    let server = Server::new(move |path| {
        (
            "200 OK".into(),
            if path.ends_with("all.json") {
                all.clone()
            } else if path.ends_with("manifest.json") {
                metadata.clone()
            } else if path.ends_with("/java") {
                java.clone()
            } else {
                b"license".to_vec()
            },
        )
    });
    let scheduler = pcl_network::DownloadScheduler::new(Default::default()).unwrap();
    let result = install_recommended_guarded(
        &store,
        8,
        server.http(scheduler),
        &task,
        &|| Ok(()),
        &|| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(result.runtime.major, 8);
    assert_eq!(
        pcl_core::java::managed_runtime_paths(&fixture.0),
        [result.path]
    );
    task.finish(TaskOutcome::Complete {
        result: None,
        message: "fixture".into(),
        error: None,
    });
}

#[tokio::test]
async fn automatic_java_binding_guard_cleans_unpublished_runtime() {
    let fixture = auto_fixture();
    let store = Store::capture(&fixture.0).unwrap();
    let tasks = Arc::new(Tasks::new());
    let task = task(&tasks, &store);
    let java = script(21, std::env::consts::ARCH);
    let manifest = plan(&java);
    let package = package(&manifest);
    let all = catalog_body(&[package.clone()]);
    let metadata = serde_json::to_vec(&manifest).unwrap();
    let changed = Arc::new(AtomicBool::new(false));
    let server_changed = changed.clone();
    let server = Server::new(move |path| {
        if path.ends_with("/java") {
            server_changed.store(true, Ordering::SeqCst);
        }
        (
            "200 OK".into(),
            if path.ends_with("all.json") {
                all.clone()
            } else if path.ends_with("manifest.json") {
                metadata.clone()
            } else if path.ends_with("/java") {
                java.clone()
            } else {
                b"license".to_vec()
            },
        )
    });
    let scheduler = pcl_network::DownloadScheduler::new(Default::default()).unwrap();
    let result = install_guarded(
        &store,
        &package.component,
        &package.sha1,
        server.http(scheduler),
        &task,
        &|| Ok(()),
        &|| {
            if changed.load(Ordering::SeqCst) {
                Err("synthetic Java binding invalid".into())
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert_eq!(result.err().unwrap(), "synthetic Java binding invalid");
    assert!(pcl_core::java::managed_runtime_paths(&fixture.0).is_empty());
    assert_eq!(store.dir.names().unwrap(), [".java-install.lock"]);
    task.finish(TaskOutcome::Error("fixture binding invalid".into()));
}

#[tokio::test]
async fn recommended_java_never_substitutes_a_higher_major() {
    let fixture = auto_fixture();
    let store = Store::capture(&fixture.0).unwrap();
    let tasks = Arc::new(Tasks::new());
    let task = task(&tasks, &store);
    let package = package(&plan(&script(21, std::env::consts::ARCH)));
    let all = catalog_body(&[package]);
    let server = Server::new(move |path| {
        assert!(path.ends_with("all.json"));
        ("200 OK".into(), all.clone())
    });
    let scheduler = pcl_network::DownloadScheduler::new(Default::default()).unwrap();
    let result = install_recommended_guarded(
        &store,
        8,
        server.http(scheduler),
        &task,
        &|| Ok(()),
        &|| Ok(()),
    )
    .await;
    assert!(result.err().unwrap().contains("Java 8"));
    assert!(pcl_core::java::managed_runtime_paths(&fixture.0).is_empty());
    task.finish(TaskOutcome::Error("fixture unavailable major".into()));
}
