//! The actual installer and native retained-directory ports run against private
//! roots and loopback HTTP. No game, JVM, account or external download is used.
use super::*;
use pcl_core::retained_fs::Directory;
use serde_json::json;
use std::{collections::BTreeMap, io::Cursor, net::TcpListener, thread};

pub(super) fn directory_file(path: &Path) -> fs::File {
    Directory::open_absolute(&fs::canonicalize(path).unwrap())
        .unwrap()
        .try_clone_file()
        .unwrap()
}
fn fixture() -> tempfile::TempDir {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/native-installer-fixtures");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap()
}
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn native_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "fixture.dll",
        "macos" => "libfixture.dylib",
        "linux" => "libfixture.so",
        other => panic!("Unsupported native fixture host: {other}"),
    }
}
struct Fixture {
    installer: Installer,
    request: InstallRequest,
    hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
impl Fixture {
    fn new(fabric: bool, native_archive: Vec<u8>) -> Self {
        Self::with_archives(fabric, vec![native_archive])
    }
    fn with_archives(fabric: bool, native_archives: Vec<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let descriptor = |path: &str, bytes: &[u8]| {
            json!({
                "url": format!("{base}{path}"), "size": bytes.len(),
                "sha1": format!("{:x}", Sha1::digest(bytes))
            })
        };
        let index = serde_json::to_vec(&json!({"objects":{}})).unwrap();
        let mut index_descriptor = descriptor("/index", &index);
        index_descriptor["id"] = json!("native-fixture");
        let native_libraries: Vec<_> = native_archives.iter().enumerate().map(|(index, bytes)| {
            let mut native_descriptor = descriptor(&format!("/native{index}"), bytes);
            native_descriptor["path"] = json!(format!("fixture/native{index}/1/native{index}-1.jar"));
            json!({"name":format!("fixture:native{index}:1"), "extract":{"exclude":["META-INF/"]},
                "natives":{"linux":"fixture-native","windows":"fixture-native","osx":"fixture-native"},
                "downloads":{"classifiers":{"fixture-native":native_descriptor}}})
        }).collect();
        let mut logging = descriptor("/logging", b"fixture logging");
        logging["id"] = json!("fixture.xml");
        let metadata = serde_json::to_vec(&json!({
            "id":"fixture", "mainClass":"fixture.Main", "javaVersion":{"majorVersion":21},
            "downloads":{"client":descriptor("/client", b"fixture client")},
            "assetIndex":index_descriptor, "arguments":{"jvm":[],"game":[]},
            "logging":{"client":{"file":logging}},
            "libraries":native_libraries
        }))
        .unwrap();
        let catalog = serde_json::to_vec(&json!({"versions":[{
            "id":"fixture", "url":format!("{base}/metadata"),
            "sha1":format!("{:x}", Sha1::digest(&metadata))
        }]}))
        .unwrap();
        let mut routes = BTreeMap::from([
            ("/catalog".into(), catalog),
            ("/metadata".into(), metadata),
            ("/index".into(), index),
            ("/client".into(), b"fixture client".to_vec()),
            ("/logging".into(), b"fixture logging".to_vec()),
        ]);
        for (index, bytes) in native_archives.into_iter().enumerate() {
            routes.insert(format!("/native{index}"), bytes);
        }
        if fabric {
            let library = archive(&[("fixture-loader", b"synthetic Fabric jar")]);
            let path = "/net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar";
            let profile = serde_json::to_vec(&json!({
                "id":"fabric-fixture", "inheritsFrom":"fixture", "mainClass":"fixture.Fabric",
                "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0","url":format!("{base}/")}]
            }))
            .unwrap();
            routes.insert(
                "/v2/versions/loader/fixture/0.16.0/profile/json".into(),
                profile,
            );
            routes.insert(
                format!("{path}.sha1"),
                format!("{:x}", Sha1::digest(&library)).into_bytes(),
            );
            routes.insert(path.into(), library);
        }
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (requested, stopping) = (hits.clone(), stop.clone());
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
                // Accepted sockets inherit nonblocking on Windows/BSD.
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
                let request = String::from_utf8_lossy(&request);
                let mut words = request.split_whitespace();
                let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
                requested.lock().unwrap().push(format!("{method} {path}"));
                if let Some(body) = routes.get(path) {
                    let _ = write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if method != "HEAD" {
                        let _ = socket.write_all(body);
                    }
                } else {
                    let _ = socket.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        });
        let factory = pcl_network::ClientFactory::new(pcl_network::Policy {
            proxy: pcl_network::ProxyPolicy::None,
            ..Default::default()
        })
        .unwrap();
        let mut installer = Installer::from_factory(&factory)
            .unwrap()
            .with_java("synthetic-no-jvm-may-run");
        installer.endpoint = Some(format!("{base}/catalog"));
        Self {
            installer,
            request: InstallRequest {
                minecraft: "fixture".into(),
                name: "native-instance".into(),
                components: if fabric {
                    vec![ComponentSelection {
                        provider: "fabric".into(),
                        version: "0.16.0".into(),
                    }]
                } else {
                    vec![]
                },
            },
            hits,
            stop,
            worker: Some(worker),
        }
    }
    fn resolved(&self) -> ResolvedInstallRequest {
        let resolved = self
            .installer
            .resolve_request(&self.request, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(resolved.processor_java_requirement().unwrap(), None);
        resolved
    }
}

#[test]
fn native_bound_vanilla_and_fabric_install_real_metadata_downloads_and_archive_streams() {
    for fabric in [false, true] {
        let path = format!("nested/{}", native_name());
        let excluded = format!("META-INF/{}", native_name());
        let plan = Fixture::new(
            fabric,
            archive(&[
                (&path, b"native bytes"),
                (&excluded, b"excluded"),
                ("readme.txt", b"not native"),
            ]),
        );
        let resolved = plan.resolved();
        let root = fixture();
        let result = plan
            .installer
            .install_resolved_request_bound(
                directory_file(root.path()),
                &resolved,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        assert_eq!(result.id, "native-instance");
        assert_eq!(result.java_major, 21);
        assert_eq!(
            fs::read(
                root.path()
                    .join("versions/native-instance/native-instance.jar")
            )
            .unwrap(),
            b"fixture client"
        );
        let natives = root.path().join("versions/native-instance/natives");
        assert_eq!(
            fs::read(natives.join(native_name())).unwrap(),
            b"native bytes"
        );
        assert_eq!(fs::read_dir(&natives).unwrap().count(), 1);
        assert_eq!(
            fs::read(root.path().join("assets/log_configs/fixture.xml")).unwrap(),
            b"fixture logging"
        );
        let profile: Value = serde_json::from_slice(
            &fs::read(
                root.path()
                    .join("versions/native-instance/native-instance.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            profile["mainClass"],
            if fabric {
                "fixture.Fabric"
            } else {
                "fixture.Main"
            }
        );
        assert_eq!(
            fs::read_dir(root.path().join("versions")).unwrap().count(),
            1
        );
        if fabric {
            assert!(root
                .path()
                .join("libraries/net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar")
                .is_file());
            assert!(plan
                .hits
                .lock()
                .unwrap()
                .iter()
                .any(|h| h.starts_with("GET /net/fabricmc/")));
        }
    }
}

#[test]
fn native_bound_cancel_before_publish_cleans_private_workspace_and_allows_retry() {
    let path = format!("nested/{}", native_name());
    let plan = Fixture::new(true, archive(&[(&path, b"native bytes")]));
    let resolved = plan.resolved();
    let root = fixture();
    let cancel = AtomicBool::new(false);
    let err = plan
        .installer
        .install_resolved_request_bound(directory_file(root.path()), &resolved, &cancel, |p| {
            if p.stage == "publishing" {
                cancel.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
    assert!(err.contains("取消"), "{err}");
    assert_eq!(
        fs::read_dir(root.path().join("versions")).unwrap().count(),
        0
    );
    cancel.store(false, Ordering::Release);
    plan.installer
        .install_resolved_request_bound(directory_file(root.path()), &resolved, &cancel, |_| {})
        .unwrap();
    assert_eq!(
        fs::read_dir(root.path().join("versions")).unwrap().count(),
        1
    );
    assert!(root
        .path()
        .join("versions/native-instance/native-instance.json")
        .is_file());
}

#[test]
fn native_bound_unsafe_archive_is_rejected_before_native_output_and_workspace_is_cleaned() {
    let escape = format!("../{}", native_name());
    let plan = Fixture::new(false, archive(&[(&escape, b"must not publish")]));
    let resolved = plan.resolved();
    let root = fixture();
    let err = plan
        .installer
        .install_resolved_request_bound(
            directory_file(root.path()),
            &resolved,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
    assert!(err.contains("Unsafe native archive entry"), "{err}");
    assert_eq!(
        fs::read_dir(root.path().join("versions")).unwrap().count(),
        0
    );
    assert!(!root.path().join(native_name()).exists());
}

#[test]
fn native_output_explicit_failure_retains_named_evidence_until_owned_workspace_cleanup() {
    let root = fixture();
    let bound = InstallDir::bound(directory_file(root.path())).unwrap();
    let temporary = InstallTemporary::new(&bound).unwrap();
    let workspace_name = temporary.name().to_owned();
    let file = temporary.dir().file("nested/partial.bin").unwrap();
    let mut output = file.output(1024).unwrap();
    output
        .file_mut()
        .write_all(b"partial archive bytes")
        .unwrap();
    let failure = output.close("synthetic stream failure".into());
    assert!(failure.contains("synthetic stream failure"));
    assert!(file.open_optional().unwrap().is_none());
    #[cfg(target_os = "linux")]
    assert_eq!(failure, "synthetic stream failure");
    #[cfg(not(target_os = "linux"))]
    {
        assert!(failure.contains("已保留待恢复"), "{failure}");
        assert_eq!(temporary.dir().files().unwrap().len(), 1);
    }
    drop(file);
    temporary.close().unwrap();
    assert!(!root.path().join(workspace_name).exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn native_bound_duplicate_basenames_keep_last_archive_entry_and_last_jar() {
    let first = format!("first/{}", native_name());
    let second = format!("second/{}", native_name());
    for archives in [
        vec![archive(&[
            (&first, b"earlier entry"),
            (&second, b"last entry"),
        ])],
        vec![
            archive(&[(&first, b"earlier jar")]),
            archive(&[(&second, b"last entry")]),
        ],
    ] {
        let plan = Fixture::with_archives(false, archives);
        let resolved = plan.resolved();
        let root = fixture();
        plan.installer
            .install_resolved_request_bound(
                directory_file(root.path()),
                &resolved,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        let natives = root.path().join("versions/native-instance/natives");
        assert_eq!(
            fs::read(natives.join(native_name())).unwrap(),
            b"last entry"
        );
        assert_eq!(fs::read_dir(natives).unwrap().count(), 1);
        assert_eq!(
            fs::read_dir(root.path().join("versions")).unwrap().count(),
            1
        );
    }
}

#[test]
fn native_bound_streams_ignore_redirected_path_adapters() {
    let root = fixture();
    let outside = fixture();
    fs::write(outside.path().join("sentinel"), b"outside stays untouched").unwrap();
    let mut bound = InstallDir::bound(directory_file(root.path())).unwrap();
    bound.redirect_adapter_for_test(outside.path().to_path_buf());
    let mut target = bound.file("nested/actual.bin").unwrap();
    target.redirect_adapter_for_test(outside.path().join("redirected.bin"));
    let mut output = target.output(64).unwrap();
    output
        .file_mut()
        .write_all(b"held-directory bytes")
        .unwrap();
    output.publish().unwrap();
    let mut actual = Vec::new();
    target.open().unwrap().read_to_end(&mut actual).unwrap();
    assert_eq!(actual, b"held-directory bytes");
    assert_eq!(
        fs::read(root.path().join("nested/actual.bin")).unwrap(),
        actual
    );
    assert_eq!(
        bound.files().unwrap(),
        vec![PathBuf::from("nested/actual.bin")]
    );
    assert_eq!(
        fs::read(outside.path().join("sentinel")).unwrap(),
        b"outside stays untouched"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
}

#[test]
fn native_bound_root_rename_before_first_write_keeps_original_directory_authority() {
    let path = format!("nested/{}", native_name());
    let plan = Fixture::new(false, archive(&[(&path, b"native bytes")]));
    let resolved = plan.resolved();
    let container = fixture();
    let original = container.path().join("original");
    let moved = container.path().join("moved");
    fs::create_dir(&original).unwrap();
    let held = directory_file(&original);
    let changed = AtomicBool::new(false);
    let (begin, observe) = std::sync::mpsc::sync_channel(0);
    let (completed, completion) = std::sync::mpsc::sync_channel(1);
    let completion = Mutex::new(completion);
    thread::scope(|scope| {
        // Start outside the scoped installer worker so Linux Landlock cannot
        // grant/reject the observer's independent namespace mutation.
        let observer_original = original.clone();
        let observer_moved = moved.clone();
        let observer = scope.spawn(move || {
            observe.recv_timeout(Duration::from_secs(5)).unwrap();
            let result = (|| -> std::io::Result<()> {
                fs::rename(&observer_original, &observer_moved)?;
                fs::create_dir(&observer_original)?;
                fs::write(observer_original.join("sentinel"), b"replacement root")
            })();
            completed
                .send(result.map_err(|error| error.to_string()))
                .unwrap();
        });
        plan.installer
            .install_resolved_request_bound(held, &resolved, &AtomicBool::new(false), |p| {
                if p.stage == "metadata" && !changed.swap(true, Ordering::Relaxed) {
                    begin.send(()).unwrap();
                    completion
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap()
                        .unwrap();
                }
            })
            .unwrap();
        observer.join().unwrap();
    });
    assert!(changed.load(Ordering::Relaxed));
    assert!(moved
        .join("versions/native-instance/native-instance.json")
        .is_file());
    assert_eq!(
        fs::read(original.join("sentinel")).unwrap(),
        b"replacement root"
    );
    assert_eq!(fs::read_dir(original).unwrap().count(), 1);
}
