//! Only synthetic project-local roots and loopback HTTP fixtures are used.
use super::*;
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::{net::TcpListener, thread};

pub(super) fn fixture() -> tempfile::TempDir {
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../work/launch-readiness-2026-10-18/repair-tests");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(work.canonicalize().unwrap()).unwrap()
}
pub(super) struct Server {
    pub(super) base: String,
    stop: Arc<AtomicBool>,
    pub(super) requests: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
pub(super) fn server(routes: Vec<(&str, Vec<u8>)>) -> Server {
    let routes: BTreeMap<String, Vec<u8>> = routes
        .into_iter()
        .map(|(path, bytes)| (path.into(), bytes))
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let requests = Arc::new(AtomicU64::new(0));
    let count = requests.clone();
    let worker = thread::spawn(move || {
        while !stopping.load(Ordering::Acquire) {
            let mut socket = match listener.accept() {
                Ok((socket, _)) => socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while request.len() < 8192 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match socket.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buffer[..n]),
                }
            }
            if !request.windows(4).any(|w| w == b"\r\n\r\n") {
                continue;
            }
            count.fetch_add(1, Ordering::Relaxed);
            let request = String::from_utf8_lossy(&request);
            let path = request.split_whitespace().nth(1).unwrap_or("");
            if let Some(bytes) = routes.get(path) {
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                for chunk in bytes.chunks(16384) {
                    if socket.write_all(chunk).is_err() {
                        break;
                    }
                    if bytes.len() > 65536 {
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            } else {
                let _ = socket.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        }
    });
    Server {
        base,
        stop,
        requests,
        worker: Some(worker),
    }
}
pub(super) fn installer(server: &Server) -> Installer {
    let scheduler = pcl_network::DownloadScheduler::new(pcl_network::DownloadPolicy {
        max_concurrent_transfers: 1,
        total_rate_limit_mib_per_second: 0,
        forbid_cross_root_cache_copy: true,
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
pub(super) fn desc(server: &Server, path: &str, bytes: &[u8]) -> Value {
    json!({"url":format!("{}{path}",server.base),"sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len()})
}
pub(super) fn metadata(root: &Path, id: &str, value: Value) {
    fs::create_dir_all(root.join(format!("versions/{id}"))).unwrap();
    fs::write(
        root.join(format!("versions/{id}/{id}.json")),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
}
fn capture(root: &Path, id: &str) -> MissingRepair {
    MissingRepair::capture_bound(crate::native_port_tests::directory_file(root), root, id).unwrap()
}
fn repair(installer: &Installer, request: &MissingRepair) -> MissingRepairReport {
    installer
        .repair_missing(
            request,
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|_| {},
        )
        .unwrap()
}

#[test]
fn verified_missing_files_are_added_and_mismatched_existing_files_are_never_replaced() {
    let root = fixture();
    let server = server(vec![
        ("/client", b"verified client".to_vec()),
        ("/library", b"verified library".to_vec()),
    ]);
    let mut artifact = desc(&server, "/library", b"verified library");
    artifact["path"] = "example/library.jar".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",b"verified client")},"libraries":[{"name":"org.example:library:1","downloads":{"artifact":artifact}}]}),
    );
    fs::create_dir_all(root.path().join("libraries/example")).unwrap();
    fs::write(
        root.path().join("libraries/example/library.jar"),
        b"user modified old contents",
    )
    .unwrap();
    let request = capture(root.path(), "fixture");
    let report = repair(&installer(&server), &request);
    assert_eq!(
        (
            report.files_downloaded,
            report.files_present,
            report.unsupported_total
        ),
        (1, 1, 0)
    );
    assert_eq!(
        fs::read(root.path().join("versions/fixture/fixture.jar")).unwrap(),
        b"verified client"
    );
    assert_eq!(
        fs::read(root.path().join("libraries/example/library.jar")).unwrap(),
        b"user modified old contents"
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), 1);
}

#[test]
fn missing_forge_runtime_uses_retained_installer_evidence_and_generated_outputs_are_explicit() {
    let root = fixture();
    let server = server(vec![("/fmlcore", b"verified fmlcore".to_vec())]);
    let mut value = json!({"id":"fixture","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
        "libraries":[],"arguments":{"game":["--launchTarget","forgeclient",
        "--fml.forgeGroup","net.minecraftforge","--fml.mcVersion","1.20.1",
        "--fml.forgeVersion","47.4.0","--fml.mcpVersion","20230612.114412"]},
        "downloads":{"client":desc(&server,"/client",b"client")}});
    let paths = pcl_core::repair::implicit_runtime_paths(&value).unwrap();
    let mut record = desc(&server, "/fmlcore", b"verified fmlcore");
    record["path"] = json!(paths[0]);
    value["pclRuntimeFiles"] = json!([record]);
    metadata(root.path(), "fixture", value);
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"client").unwrap();
    for path in paths.iter().skip(1) {
        let file = root.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"existing loader output").unwrap();
    }
    let request = capture(root.path(), "fixture");
    assert_eq!(request.missing_count().unwrap(), 1);
    let report = repair(&installer(&server), &request);
    assert_eq!((report.files_downloaded, report.unsupported_total), (1, 0));
    assert_eq!(
        fs::read(root.path().join(&paths[0])).unwrap(),
        b"verified fmlcore"
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), 1);
    fs::remove_file(root.path().join(paths.last().unwrap())).unwrap();
    let request = capture(root.path(), "fixture");
    assert_eq!(request.missing_count().unwrap(), 1);
    let report = repair(&installer(&server), &request);
    assert_eq!((report.files_downloaded, report.unsupported_total), (0, 1));
    assert_eq!(server.requests.load(Ordering::Relaxed), 1);
}

#[test]
fn inherited_core_uses_parent_download_and_does_not_create_child_jar() {
    let root = fixture();
    let server = server(vec![
        ("/parent", b"parent jar".to_vec()),
        ("/child", b"different child jar".to_vec()),
    ]);
    metadata(
        root.path(),
        "parent",
        json!({"id":"parent","downloads":{"client":desc(&server,"/parent",b"parent jar")}}),
    );
    metadata(
        root.path(),
        "child",
        json!({"id":"child","inheritsFrom":"parent","downloads":{"client":desc(&server,"/child",b"different child jar")}}),
    );
    let report = repair(&installer(&server), &capture(root.path(), "child"));
    assert_eq!(report.files_downloaded, 1);
    assert_eq!(
        fs::read(root.path().join("versions/parent/parent.jar")).unwrap(),
        b"parent jar"
    );
    assert!(!root.path().join("versions/child/child.jar").exists());
}

#[test]
fn missing_index_is_verified_in_memory_before_objects_are_enumerated() {
    let root = fixture();
    let object = b"official asset fixture";
    let hash = format!("{:x}", Sha1::digest(object));
    let index = serde_json::to_vec(&json!({"objects":{"../../world/unsafe-name":{"hash":hash,"size":object.len()},"duplicate":{"hash":hash,"size":object.len()}}})).unwrap();
    let object_route = format!("/{}/{hash}", &hash[..2]);
    let server = server(vec![
        ("/index", index.clone()),
        (&object_route, object.to_vec()),
        ("/client", b"client".to_vec()),
    ]);
    let mut index_desc = desc(&server, "/index", &index);
    index_desc["id"] = "fixture-index".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",b"client")},"assetIndex":index_desc}),
    );
    let request = capture(root.path(), "fixture");
    let report = repair(&installer(&server), &request);
    assert_eq!((report.files_downloaded, report.unsupported_total), (3, 0));
    assert_eq!(
        fs::read(root.path().join("assets/indexes/fixture-index.json")).unwrap(),
        index
    );
    assert_eq!(
        fs::read(
            root.path()
                .join(format!("assets/objects/{}/{hash}", &hash[..2]))
        )
        .unwrap(),
        object
    );
    assert!(!root.path().join("world").exists());
    assert_eq!(server.requests.load(Ordering::Relaxed), 3);
    let repeat = repair(&installer(&server), &request);
    assert_eq!(
        (
            repeat.files_downloaded,
            repeat.files_present,
            repeat.unsupported_total
        ),
        (0, 3, 0)
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), 3);
}

#[test]
fn bad_index_checksum_creates_neither_index_nor_objects() {
    let root = fixture();
    let index = b"{\"objects\":{}}";
    let server = server(vec![("/index", index.to_vec())]);
    let mut index_desc = desc(&server, "/index", index);
    index_desc["id"] = "fixture-index".into();
    index_desc["sha1"] = "0000000000000000000000000000000000000000".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","assetIndex":index_desc}),
    );
    let request = capture(root.path(), "fixture");
    assert!(installer(&server)
        .repair_missing(
            &request,
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|_| {}
        )
        .unwrap_err()
        .contains("SHA1"));
    assert!(!root.path().join("assets").exists());
}

#[test]
fn unsupported_sources_checksums_paths_and_sizes_are_bounded_and_explicit() {
    let root = fixture();
    let server = server(Vec::new());
    let mut libraries: Vec<Value> = (0..40)
        .map(|n| json!({"name":format!("org.example:custom{n}:1")}))
        .collect();
    libraries.extend([
        json!({"name":"org.example:untrusted:1","downloads":{"artifact":{"path":"untrusted.jar","url":"https://example.com/library.jar","size":4,"sha1":"1234567890123456789012345678901234567890"}}}),
        json!({"name":"org.example:unsafe:1","downloads":{"artifact":{"path":"../mods/user.jar","url":"https://libraries.minecraft.net/a","size":4,"sha1":"1234567890123456789012345678901234567890"}}}),
        json!({"name":"org.example:no-size:1","downloads":{"artifact":{"path":"no-size.jar","url":"https://libraries.minecraft.net/a","sha1":"1234567890123456789012345678901234567890"}}}),
    ]);
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","libraries":libraries}),
    );
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"present").unwrap();
    let report = repair(&installer(&server), &capture(root.path(), "fixture"));
    assert_eq!(
        (
            report.files_downloaded,
            report.files_present,
            report.unsupported_total,
            report.unsupported.len()
        ),
        (0, 1, 43, 32)
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    assert!(!root.path().join("libraries").exists());
    assert!(!root.path().join("mods").exists());
}

#[test]
fn existing_untrusted_library_is_present_without_network_or_content_checks() {
    let root = fixture();
    let server = server(Vec::new());
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","libraries":[{"name":"org.example:custom:1","downloads":{"artifact":{"path":"custom.jar","url":"https://example.com/custom.jar"}}}]}),
    );
    fs::create_dir(root.path().join("libraries")).unwrap();
    fs::write(root.path().join("libraries/custom.jar"), b"user owned").unwrap();
    fs::write(
        root.path().join("versions/fixture/fixture.jar"),
        b"user owned jar",
    )
    .unwrap();
    let report = repair(&installer(&server), &capture(root.path(), "fixture"));
    assert_eq!(
        (
            report.files_downloaded,
            report.files_present,
            report.unsupported_total
        ),
        (0, 2, 0)
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
}

#[test]
fn concurrently_inserted_regular_target_wins_without_overwrite() {
    let root = fixture();
    let payload = vec![0x61; 262144];
    let server = server(vec![("/client", payload.clone())]);
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",&payload)}}),
    );
    let request = capture(root.path(), "fixture");
    let inserted = AtomicBool::new(false);
    let report = installer(&server)
        .repair_missing(
            &request,
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|p| {
                if p.network_bytes > 0 && !inserted.swap(true, Ordering::Relaxed) {
                    fs::write(
                        root.path().join("versions/fixture/fixture.jar"),
                        b"concurrent user file",
                    )
                    .unwrap();
                }
            },
        )
        .unwrap();
    assert_eq!(
        (
            report.files_downloaded,
            report.files_present,
            report.unsupported_total
        ),
        (0, 1, 0)
    );
    assert_eq!(
        fs::read(root.path().join("versions/fixture/fixture.jar")).unwrap(),
        b"concurrent user file"
    );
}

#[test]
fn cancellation_during_streaming_leaves_no_named_partial_or_target() {
    let root = fixture();
    let payload = vec![0x61; 262144];
    let server = server(vec![("/client", payload.clone())]);
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",&payload)}}),
    );
    let request = capture(root.path(), "fixture");
    let cancel = AtomicBool::new(false);
    let result =
        installer(&server).repair_missing(&request, &cancel, &|| Ok(()), &|| Ok(()), &|p| {
            if p.network_bytes > 0 {
                cancel.store(true, Ordering::Release);
            }
        });
    assert!(result.unwrap_err().contains("取消"));
    let files: Vec<_> = fs::read_dir(root.path().join("versions/fixture"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(files, vec!["fixture.json"]);
}

#[test]
fn cancellation_after_first_publication_preserves_only_completed_file() {
    let root = fixture();
    let server = server(vec![("/a", b"a".to_vec()), ("/b", b"b".to_vec())]);
    let mut a = desc(&server, "/a", b"a");
    a["path"] = "a.jar".into();
    let mut b = desc(&server, "/b", b"b");
    b["path"] = "b.jar".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","libraries":[{"name":"org.example:a:1","downloads":{"artifact":a}},{"name":"org.example:b:1","downloads":{"artifact":b}}]}),
    );
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"old jar").unwrap();
    let request = capture(root.path(), "fixture");
    let cancel = AtomicBool::new(false);
    let result =
        installer(&server).repair_missing(&request, &cancel, &|| Ok(()), &|| Ok(()), &|p| {
            if p.completed == 1 {
                cancel.store(true, Ordering::Release);
            }
        });
    assert!(result.unwrap_err().contains("取消"));
    assert_eq!(fs::read(root.path().join("libraries/a.jar")).unwrap(), b"a");
    assert!(!root.path().join("libraries/b.jar").exists());
    assert_eq!(
        fs::read_dir(root.path().join("libraries")).unwrap().count(),
        1
    );
}

#[test]
fn metadata_edit_and_external_binding_guard_stop_before_publication() {
    for edit_metadata in [true, false] {
        let root = fixture();
        let payload = vec![0x61; 262144];
        let server = server(vec![("/client", payload.clone())]);
        metadata(
            root.path(),
            "fixture",
            json!({"id":"fixture","downloads":{"client":desc(&server,"/client",&payload)}}),
        );
        let request = capture(root.path(), "fixture");
        let changed = AtomicBool::new(false);
        let result = installer(&server).repair_missing(
            &request,
            &AtomicBool::new(false),
            &|| {
                if changed.load(Ordering::Relaxed) && !edit_metadata {
                    Err("native revision changed".into())
                } else {
                    Ok(())
                }
            },
            &|| Ok(()),
            &|p| {
                if p.network_bytes > 0 && !changed.swap(true, Ordering::Relaxed) && edit_metadata {
                    fs::write(root.path().join("versions/fixture/fixture.json"), b"{}").unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert!(!root.path().join("versions/fixture/fixture.jar").exists());
        assert_eq!(
            fs::read_dir(root.path().join("versions/fixture"))
                .unwrap()
                .count(),
            1
        );
    }
}

#[test]
#[cfg(unix)]
fn root_retarget_and_parent_replacement_never_follow_new_directories() {
    for replace_root in [true, false] {
        let container = fixture();
        let root = container.path().join("root");
        fs::create_dir(&root).unwrap();
        let outside = container.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let payload = vec![0x61; 262144];
        let server = server(vec![("/library", payload.clone())]);
        let mut artifact = desc(&server, "/library", &payload);
        artifact["path"] = "target.jar".into();
        metadata(
            &root,
            "fixture",
            json!({"id":"fixture","libraries":[{"name":"org.example:library:1","downloads":{"artifact":artifact}}]}),
        );
        fs::write(root.join("versions/fixture/fixture.jar"), b"old jar").unwrap();
        fs::create_dir(root.join("libraries")).unwrap();
        let request = capture(&root, "fixture");
        let changed = AtomicBool::new(false);
        let saved = container.path().join("saved");
        let result = installer(&server).repair_missing(
            &request,
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|p| {
                if p.network_bytes > 0 && !changed.swap(true, Ordering::Relaxed) {
                    if replace_root {
                        fs::rename(&root, &saved).unwrap();
                        fs::create_dir(&root).unwrap();
                    } else {
                        fs::rename(root.join("libraries"), &saved).unwrap();
                        symlink(&outside, root.join("libraries")).unwrap();
                    }
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert!(!root.join("libraries/target.jar").exists());
        assert!(!saved
            .join(if replace_root {
                "libraries/target.jar"
            } else {
                "target.jar"
            })
            .exists());
    }
}

#[test]
fn missing_instance_and_symlink_or_special_destinations_are_rejected() {
    let root = fixture();
    assert!(MissingRepair::capture_bound(
        crate::native_port_tests::directory_file(root.path()),
        root.path(),
        "missing"
    )
    .is_err());
    assert!(!root.path().join("versions").exists());
    metadata(root.path(), "fixture", json!({"id":"fixture"}));
    let outside = fixture();
    fs::write(outside.path().join("user.jar"), b"untouched").unwrap();
    #[cfg(unix)]
    {
        symlink(
            outside.path().join("user.jar"),
            root.path().join("versions/fixture/fixture.jar"),
        )
        .unwrap();
        assert!(MissingRepair::capture_bound(
            crate::native_port_tests::directory_file(root.path()),
            root.path(),
            "fixture"
        )
        .is_err());
        assert_eq!(
            fs::read(outside.path().join("user.jar")).unwrap(),
            b"untouched"
        );
        fs::remove_file(root.path().join("versions/fixture/fixture.jar")).unwrap();
    }
    fs::create_dir(root.path().join("versions/fixture/fixture.jar")).unwrap();
    assert!(MissingRepair::capture_bound(
        crate::native_port_tests::directory_file(root.path()),
        root.path(),
        "fixture"
    )
    .is_err());
}

#[test]
fn total_missing_budget_is_checked_before_directory_creation_or_network() {
    let root = fixture();
    let server = server(Vec::new());
    let libraries: Vec<_> = (0..17).map(|n|json!({"name":format!("org.example:large{n}:1"),"downloads":{"artifact":{"path":format!("large{n}.jar"),"url":format!("{}/large",server.base),"sha1":"1234567890123456789012345678901234567890","size":manifest::MAX_REPAIR_FILE_BYTES}}})).collect();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","libraries":libraries}),
    );
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"old jar").unwrap();
    let request = capture(root.path(), "fixture");
    assert!(installer(&server)
        .repair_missing(
            &request,
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|_| {}
        )
        .unwrap_err()
        .contains("8 GiB"));
    assert!(!root.path().join("libraries").exists());
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
}

#[test]
fn inherited_instance_directory_disappearance_is_not_recreated() {
    let root = fixture();
    let server = server(Vec::new());
    metadata(root.path(), "base", json!({"id":"base"}));
    metadata(
        root.path(),
        "child",
        json!({"id":"child","inheritsFrom":"base"}),
    );
    let request = capture(root.path(), "child");
    fs::remove_dir_all(root.path().join("versions/base")).unwrap();
    assert!(installer(&server)
        .repair_missing(
            &request,
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|_| {}
        )
        .is_err());
    assert!(!root.path().join("versions/base").exists());
}

#[test]
fn directory_descriptor_budget_fails_before_creating_a_new_directory() {
    let root = fixture();
    let server = server(Vec::new());
    let artifact = json!({"path":"new/path/library.jar","url":format!("{}/library",server.base),"sha1":"1234567890123456789012345678901234567890","size":1});
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","libraries":[{"name":"org.example:library:1","downloads":{"artifact":artifact}}]}),
    );
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"old jar").unwrap();
    let request = capture(root.path(), "fixture");
    {
        let mut state = request.state.lock().unwrap();
        state.filesystem.directory_limit = state.filesystem.directories.len();
    }
    let result = installer(&server).repair_missing(
        &request,
        &AtomicBool::new(false),
        &|| Ok(()),
        &|| Ok(()),
        &|_| {},
    );
    assert!(result.unwrap_err().contains("目录数量超过安全上限"));
    assert!(!root.path().join("libraries").exists());
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
}

#[test]
fn missing_count_is_local_deduplicated_and_existing_contents_are_not_checked() {
    let root = fixture();
    let server = server(Vec::new());
    let mut artifact = desc(&server, "/library", b"expected");
    artifact["path"] = "fixture.jar".into();
    let library = json!({"name":"org.example:fixture:1","downloads":{"artifact":artifact}});
    let mut index = desc(&server, "/index", b"not fetched");
    index["id"] = "fixture-index".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture", "libraries":[library,library], "assetIndex":index}),
    );
    // Presence counting deliberately accepts old, modified existing bytes.
    fs::write(
        root.path().join("versions/fixture/fixture.jar"),
        b"user modified",
    )
    .unwrap();
    let request = capture(root.path(), "fixture");
    assert_eq!(request.missing_count().unwrap(), 2);
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    assert!(!root.path().join("libraries").exists());
    assert!(!root.path().join("assets").exists());
    fs::write(root.path().join("versions/fixture/fixture.json"), b"{}").unwrap();
    assert!(request.missing_count().is_err());
}

#[test]
fn missing_count_includes_captured_objects_without_double_counting_the_index() {
    let root = fixture();
    let server = server(Vec::new());
    let hash = "1234567890123456789012345678901234567890";
    let index = serde_json::to_vec(
        &json!({"objects":{"one":{"hash":hash,"size":1},"alias":{"hash":hash,"size":1}}}),
    )
    .unwrap();
    let mut descriptor = desc(&server, "/index", &index);
    descriptor["id"] = "fixture-index".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","assetIndex":descriptor}),
    );
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"present").unwrap();
    fs::create_dir_all(root.path().join("assets/indexes")).unwrap();
    fs::write(root.path().join("assets/indexes/fixture-index.json"), index).unwrap();
    assert_eq!(capture(root.path(), "fixture").missing_count().unwrap(), 1);
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
}

#[test]
fn repairable_count_shares_writer_source_and_inheritance_eligibility_without_io() {
    let root = fixture();
    let server = server(Vec::new());
    let mut allowed = desc(&server, "/library", b"library");
    allowed["path"] = "allowed.jar".into();
    let mut denied = allowed.clone();
    denied["path"] = "denied.jar".into();
    denied["url"] = "https://example.com/custom.jar".into();
    let mut index = desc(&server, "/index", b"index");
    index["id"] = "fixture-index".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",b"client")},"libraries":[{"name":"org.example:allowed:1","downloads":{"artifact":allowed}},{"name":"org.example:denied:1","downloads":{"artifact":denied}},{"name":"org.example:unknown:1"}],"assetIndex":index}),
    );
    let request = capture(root.path(), "fixture");
    let installer = installer(&server);
    assert_eq!(request.missing_count().unwrap(), 5);
    assert_eq!(installer.repairable_missing_count(&request).unwrap(), 3);
    request
        .state
        .lock()
        .unwrap()
        .jar_probes
        .insert("versions/fixture/fixture.jar".into(), false);
    assert_eq!(installer.repairable_missing_count(&request).unwrap(), 2);
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    assert!(!root.path().join("libraries").exists());
    assert!(!root.path().join("assets").exists());
    fs::write(root.path().join("versions/fixture/fixture.json"), b"{}").unwrap();
    assert!(installer.repairable_missing_count(&request).is_err());
}
