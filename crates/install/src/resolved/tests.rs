use super::*;
use serde_json::json;
use std::{collections::HashMap, io::Cursor, net::TcpListener, thread, time::Instant};

struct Fixture {
    base: String,
    minecraft: String,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    hits: Arc<Mutex<Vec<String>>>,
    stall: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
impl Fixture {
    fn new(minecraft: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let index = serde_json::to_vec(&json!({"objects":{}})).unwrap();
        let descriptor = |path: &str, raw: &[u8]| json!({"url":format!("{base}/{path}"),"sha1":format!("{:x}",Sha1::digest(raw)),"size":raw.len()});
        let mut index_d = descriptor("index", &index);
        index_d["id"] = json!("fixture-index");
        let metadata = serde_json::to_vec(&json!({"id":minecraft,"mainClass":"net.minecraft.Main","javaVersion":{"majorVersion":21},"downloads":{"client":descriptor("client",b"client")},"assetIndex":index_d,"libraries":[],"arguments":{"game":[],"jvm":[]}})).unwrap();
        let catalog = serde_json::to_vec(&json!({"versions":[{"id":minecraft,"url":format!("{base}/metadata"),"sha1":format!("{:x}",Sha1::digest(&metadata))}]})).unwrap();
        let routes = Arc::new(Mutex::new(HashMap::from([
            ("/catalog".into(), catalog),
            ("/metadata".into(), metadata),
            ("/index".into(), index),
            ("/client".into(), b"client".to_vec()),
        ])));
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stall = Arc::new(Mutex::new(None::<String>));
        let stop = Arc::new(AtomicBool::new(false));
        let (serving, requested, stalled, stopping) =
            (routes.clone(), hits.clone(), stall.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let mut held = vec![];
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut buffer = [0; 4096];
                        let n = stream.read(&mut buffer).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buffer[..n]);
                        let words: Vec<_> = request.split_whitespace().take(2).collect();
                        let (method, path) = (
                            words.first().copied().unwrap_or(""),
                            words.get(1).copied().unwrap_or(""),
                        );
                        requested.lock().unwrap().push(format!("{method} {path}"));
                        if stalled.lock().unwrap().as_deref() == Some(path) {
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx");
                            held.push(stream);
                            continue;
                        }
                        let body = serving.lock().unwrap().get(path).cloned();
                        if let Some(body) = body {
                            let _ = write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
                            if method != "HEAD" {
                                let _ = stream.write_all(&body);
                            }
                        } else {
                            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            base,
            minecraft: minecraft.into(),
            routes,
            hits,
            stall,
            stop,
            worker: Some(worker),
        }
    }
    fn installer(&self) -> Installer {
        // No global network settings or ambient proxy mutations. Every test
        // transport is an injected loopback endpoint on a private factory.
        let factory = pcl_network::ClientFactory::new(pcl_network::Policy {
            proxy: pcl_network::ProxyPolicy::None,
            ..Default::default()
        })
        .unwrap();
        let mut installer = Installer::from_factory(&factory).unwrap();
        installer.endpoint = Some(format!("{}/catalog", self.base));
        installer
    }
    fn request(&self, provider: Option<&str>, version: &str) -> InstallRequest {
        InstallRequest {
            minecraft: self.minecraft.clone(),
            name: "private-instance".into(),
            components: provider
                .map(|provider| ComponentSelection {
                    provider: provider.into(),
                    version: version.into(),
                })
                .into_iter()
                .collect(),
        }
    }
    fn set_json(&self, path: &str, value: &Value) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.into(), serde_json::to_vec(value).unwrap());
    }
    fn clear_hits(&self) {
        self.hits.lock().unwrap().clear();
    }
    fn fabricate_fabric(&self) -> String {
        let library = jar(&[("loader.txt", b"fabric")]);
        let path = "/net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar";
        let profile_path = format!("/v2/versions/loader/{}/0.16.0/profile/json", self.minecraft);
        self.set_json(&profile_path,&json!({"id":"fabric-fixture","inheritsFrom":self.minecraft,"mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0","url":format!("{}/",self.base)}]}));
        let mut routes = self.routes.lock().unwrap();
        routes.insert(path.into(), library.clone());
        routes.insert(
            format!("{path}.sha1"),
            format!("{:x}", Sha1::digest(&library)).into_bytes(),
        );
        profile_path
    }
    fn fabricate_installer(
        &self,
        provider: &str,
        version: &str,
        declared_mc: &str,
        identity_version: &str,
        modern: bool,
    ) -> String {
        let component = normalized_component(
            &self.minecraft,
            &ComponentSelection {
                provider: provider.into(),
                version: version.into(),
            },
        )
        .unwrap();
        let coord = installer_coordinate(&component);
        let artifact_coord = coord.trim_end_matches(":installer");
        let parts: Vec<_> = artifact_coord.split(':').collect();
        let identity = format!("{}:{}:{identity_version}", parts[0], parts[1]);
        let library_path = maven_path(&identity).unwrap();
        let library = jar(&[("loader.txt", b"installer-loader")]);
        let d = json!({"url":"","path":library_path,"sha1":format!("{:x}",Sha1::digest(&library)),"size":library.len()});
        let runtime = json!({"name":identity,"downloads":{"artifact":d}});
        let profile = serde_json::to_vec(&json!({"id":"installer-fixture","inheritsFrom":declared_mc,"mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher","libraries":[runtime.clone()]})).unwrap();
        let mut install = json!({"spec":1,"minecraft":declared_mc,"json":"/version.json","libraries":[runtime],"processors":[]});
        if !modern {
            install.as_object_mut().unwrap().remove("spec");
        }
        let install = serde_json::to_vec(&install).unwrap();
        let archive = jar(&[
            ("install_profile.json", &install),
            ("version.json", &profile),
            (&format!("maven/{library_path}"), &library),
        ]);
        let path = format!(
            "/{}{}",
            if provider == "forge" { "" } else { "releases/" },
            maven_path(&coord).unwrap()
        );
        let mut routes = self.routes.lock().unwrap();
        routes.insert(
            format!("{path}.sha1"),
            format!("{:x}", Sha1::digest(&archive)).into_bytes(),
        );
        routes.insert(path.clone(), archive);
        path
    }
}

fn processor_authority(install: Option<Value>, major: u32) -> ResolvedInstallRequest {
    let download = Download {
        url: String::new(),
        hash: "0".repeat(40),
        size: 1,
        relative: "fixture".into(),
    };
    ResolvedInstallRequest {
        request: InstallRequest {
            minecraft: "fixture".into(),
            name: "fixture".into(),
            components: vec![],
        },
        vanilla: ResolvedVanilla {
            descriptor: download.clone(),
            metadata: json!({"javaVersion":{"majorVersion":major}}),
            required_java: major,
        },
        component: Some(ResolvedComponent {
            selection: ComponentSelection {
                provider: "forge".into(),
                version: "1".into(),
            },
            source: ComponentSource::Installer {
                coordinate: "fixture:loader:1".into(),
                download,
            },
            profile: json!({}),
            install,
            libraries: BTreeMap::new(),
        }),
    }
}

#[test]
fn processor_java_requirement_skips_legacy_empty_and_server_only_batches() {
    for install in [
        None,
        Some(json!({"install":{"version":"legacy"}})),
        Some(json!({"processors":[]})),
        Some(json!({"processors":[{"sides":["server"]}]})),
    ] {
        assert_eq!(
            processor_authority(install, 8)
                .processor_java_requirement()
                .unwrap(),
            None
        );
    }
    let mut vanilla = processor_authority(None, 8);
    vanilla.component = None;
    assert_eq!(vanilla.processor_java_requirement().unwrap(), None);
}

#[test]
fn processor_java_requirement_matches_client_filter_and_exact_major() {
    for processor in [
        json!({"args":[]}),
        json!({"sides":["client"],"args":[]}),
        json!({"sides":["server","client"],"args":[]}),
    ] {
        let resolved = processor_authority(
            Some(json!({"processors":[{"sides":["server"]},processor]})),
            17,
        );
        assert_eq!(
            resolved.processor_java_requirement().unwrap(),
            Some(ProcessorJavaRequirement {
                major: 17,
                exact: true
            })
        );
    }
}

#[test]
fn processor_java_requirement_rejects_invalid_major_only_when_processors_need_it() {
    for major in [0, 1000] {
        assert!(processor_authority(Some(json!({"processors":[{}]})), major)
            .processor_java_requirement()
            .is_err());
        assert_eq!(
            processor_authority(None, major)
                .processor_java_requirement()
                .unwrap(),
            None
        );
    }
}

#[test]
fn resolved_vanilla_uses_captured_metadata_without_catalog_reselection() {
    let fixture = Fixture::new("fixture");
    let installer = fixture.installer();
    let request = fixture.request(None, "");
    let cancel = AtomicBool::new(false);
    let resolved = installer.resolve_request(&request, &cancel).unwrap();
    assert_eq!(resolved.minecraft(), "fixture");
    assert_eq!(resolved.request().name, "private-instance");
    assert_eq!(resolved.required_java(), 21);
    assert!(!resolved.requires_exact_java());
    assert!(resolved.heap_bytes() > fixture.routes.lock().unwrap()["/metadata"].len());
    fixture.set_json("/catalog", &json!({"versions":[]}));
    fixture.set_json("/metadata", &json!({"id":"replacement"}));
    fixture.clear_hits();
    let root = tempfile::tempdir().unwrap();
    let result = installer
        .install_resolved_request(root.path(), &resolved, &cancel, |_| {})
        .unwrap();
    assert_eq!(result.java_major, 21);
    let metadata: Value = serde_json::from_slice(
        &fs::read(
            root.path()
                .join("versions/private-instance/private-instance.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["clientVersion"], "fixture");
    assert_eq!(metadata["mainClass"], "net.minecraft.Main");
    assert!(fixture
        .hits
        .lock()
        .unwrap()
        .iter()
        .all(|h| !h.ends_with("/catalog") && !h.ends_with("/metadata")));
    assert!(installer
        .install_request(root.path(), &fixture.request(None, ""), &cancel, |_| {})
        .unwrap_err()
        .contains("已存在"));
}

#[test]
fn resolved_fabric_retains_library_sidecar_and_length_evidence() {
    let fixture = Fixture::new("fixture");
    fixture.fabricate_fabric();
    let installer = fixture.installer();
    let cancel = AtomicBool::new(false);
    let resolved = installer
        .resolve_request(&fixture.request(Some("fabric"), "0.16.0"), &cancel)
        .unwrap();
    assert!(!resolved.requires_exact_java());
    let lib = "/net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar";
    assert!(fixture
        .hits
        .lock()
        .unwrap()
        .contains(&format!("HEAD {lib}")));
    fixture
        .routes
        .lock()
        .unwrap()
        .insert(format!("{lib}.sha1"), vec![b'0'; 40]);
    fixture.clear_hits();
    let root = tempfile::tempdir().unwrap();
    installer
        .install_resolved_request(root.path(), &resolved, &cancel, |_| {})
        .unwrap();
    assert!(root.path().join(format!("libraries{lib}")).is_file());
    assert!(fixture
        .hits
        .lock()
        .unwrap()
        .iter()
        .all(|h| !h.starts_with("HEAD ") && !h.ends_with(".sha1")));
}

#[test]
fn changed_fabric_source_or_library_cannot_replace_native_evidence() {
    for changed_source in [true, false] {
        let fixture = Fixture::new("fixture");
        let profile_path = fixture.fabricate_fabric();
        let installer = fixture.installer();
        let cancel = AtomicBool::new(false);
        let resolved = installer
            .resolve_request(&fixture.request(Some("fabric"), "0.16.0"), &cancel)
            .unwrap();
        let path = if changed_source {
            profile_path
        } else {
            "/net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar".into()
        };
        fixture
            .routes
            .lock()
            .unwrap()
            .insert(path, b"replacement".to_vec());
        let root = tempfile::tempdir().unwrap();
        let error = installer
            .install_resolved_request(root.path(), &resolved, &cancel, |_| {})
            .unwrap_err();
        assert!(
            error.contains(if changed_source { "已改变" } else { "SHA1" }),
            "{error}"
        );
        assert_eq!(
            fs::read_dir(root.path().join("versions")).unwrap().count(),
            0
        );
    }
}

#[test]
fn modern_forge_and_neoforge_execute_pinned_installer_without_sidecar_refresh() {
    for provider in ["forge", "neoforge"] {
        let fixture = Fixture::new("fixture");
        let version = if provider == "forge" {
            "fixture-1"
        } else {
            "1"
        };
        let path = fixture.fabricate_installer(provider, "1", "fixture", version, true);
        let installer = fixture.installer();
        let cancel = AtomicBool::new(false);
        let resolved = installer
            .resolve_request(&fixture.request(Some(provider), "1"), &cancel)
            .unwrap();
        assert!(resolved.requires_exact_java());
        fixture
            .routes
            .lock()
            .unwrap()
            .insert(format!("{path}.sha1"), vec![b'0'; 40]);
        fixture.clear_hits();
        let root = tempfile::tempdir().unwrap();
        installer
            .install_resolved_request(root.path(), &resolved, &cancel, |_| {})
            .unwrap();
        assert!(root
            .path()
            .join("versions/private-instance/private-instance.json")
            .is_file());
        assert!(fixture
            .hits
            .lock()
            .unwrap()
            .iter()
            .all(|h| !h.ends_with(".sha1")));
    }
}

#[test]
fn neoforge_1201_raw_and_prefixed_47_versions_share_the_forge_coordinate() {
    let fixture = Fixture::new("1.20.1");
    let path =
        fixture.fabricate_installer("neoforge", "47.1.106", "1.20.1", "1.20.1-47.1.106", true);
    assert!(
        path.contains("net/neoforged/forge/1.20.1-47.1.106/forge-1.20.1-47.1.106-installer.jar")
    );
    let installer = fixture.installer();
    let cancel = AtomicBool::new(false);
    for version in ["47.1.106", "1.20.1-47.1.106"] {
        let resolved = installer
            .resolve_request(&fixture.request(Some("neoforge"), version), &cancel)
            .unwrap();
        assert_eq!(resolved.request().components[0].version, version);
        assert_eq!(
            resolved.component.as_ref().unwrap().selection.version,
            "1.20.1-47.1.106"
        );
        let root = tempfile::tempdir().unwrap();
        installer
            .install_resolved_request(root.path(), &resolved, &cancel, |_| {})
            .unwrap();
        let metadata: Value = serde_json::from_slice(
            &fs::read(
                root.path()
                    .join("versions/private-instance/private-instance.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            metadata["libraries"][0]["name"],
            "net.neoforged:forge:1.20.1-47.1.106"
        );
    }
}

#[test]
fn installer_identity_legacy_format_and_changed_bytes_are_rejected() {
    for (declared_mc, identity, modern, expected) in [
        ("other", "fixture-1", true, "Minecraft"),
        ("fixture", "fixture-2", true, "加载器版本"),
        ("fixture", "fixture-1", false, "旧版"),
    ] {
        let fixture = Fixture::new("fixture");
        fixture.fabricate_installer("forge", "1", declared_mc, identity, modern);
        let error = fixture
            .installer()
            .resolve_request(
                &fixture.request(Some("forge"), "1"),
                &AtomicBool::new(false),
            )
            .err()
            .unwrap();
        assert!(error.contains(expected), "{error}");
    }
    let fixture = Fixture::new("fixture");
    let path = fixture.fabricate_installer("forge", "1", "fixture", "fixture-1", true);
    let installer = fixture.installer();
    let cancel = AtomicBool::new(false);
    let resolved = installer
        .resolve_request(&fixture.request(Some("forge"), "1"), &cancel)
        .unwrap();
    fixture
        .routes
        .lock()
        .unwrap()
        .insert(path, b"replacement".to_vec());
    let root = tempfile::tempdir().unwrap();
    assert!(installer
        .install_resolved_request(root.path(), &resolved, &cancel, |_| {})
        .unwrap_err()
        .contains("SHA1"));
    assert_eq!(
        fs::read_dir(root.path().join("versions")).unwrap().count(),
        0
    );
}

#[test]
fn resolution_cancellation_is_read_only_and_interrupts_stalled_metadata() {
    let fixture = Fixture::new("fixture");
    let installer = fixture.installer();
    let request = fixture.request(None, "");
    assert_eq!(
        installer
            .resolve_request(&request, &AtomicBool::new(true))
            .err()
            .unwrap(),
        "安装已取消"
    );
    assert!(fixture.hits.lock().unwrap().is_empty());
    *fixture.stall.lock().unwrap() = Some("/metadata".into());
    let cancel = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let worker = scope.spawn(|| installer.resolve_request(&request, &cancel));
        let start = Instant::now();
        while !fixture
            .hits
            .lock()
            .unwrap()
            .iter()
            .any(|h| h.ends_with("/metadata"))
        {
            assert!(start.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(5));
        }
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(worker.join().unwrap().err().unwrap(), "安装已取消");
        assert!(start.elapsed() < Duration::from_secs(3));
    });
}

#[test]
fn preparation_reports_unsupported_vanilla_resource_layout() {
    let fixture = Fixture::new("fixture");
    let raw = serde_json::to_vec(&json!({"objects":{},"virtual":true})).unwrap();
    let mut metadata: Value =
        serde_json::from_slice(&fixture.routes.lock().unwrap()["/metadata"]).unwrap();
    metadata["assetIndex"]["sha1"] = json!(format!("{:x}", Sha1::digest(&raw)));
    metadata["assetIndex"]["size"] = json!(raw.len());
    fixture.routes.lock().unwrap().insert("/index".into(), raw);
    fixture.set_json("/metadata", &metadata);
    let metadata = fixture.routes.lock().unwrap()["/metadata"].clone();
    fixture.set_json("/catalog",&json!({"versions":[{"id":"fixture","url":format!("{}/metadata",fixture.base),"sha1":format!("{:x}",Sha1::digest(&metadata))}]}));
    assert!(fixture
        .installer()
        .resolve_request(&fixture.request(None, ""), &AtomicBool::new(false))
        .err()
        .unwrap()
        .contains("虚拟资源"));
}

#[test]
fn resolved_execution_cancellation_keeps_the_existing_commit_boundary() {
    let fixture = Fixture::new("fixture");
    let installer = fixture.installer();
    let cancel = AtomicBool::new(false);
    let resolved = installer
        .resolve_request(&fixture.request(None, ""), &cancel)
        .unwrap();
    fixture.clear_hits();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("private-root");
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(
        installer
            .install_resolved_request(&root, &resolved, &cancel, |_| {})
            .unwrap_err(),
        "安装已取消"
    );
    assert!(!root.exists());
    assert!(fixture.hits.lock().unwrap().is_empty());
    cancel.store(false, Ordering::Relaxed);
    assert_eq!(
        installer
            .install_resolved_request(&root, &resolved, &cancel, |progress| {
                if progress.stage == "publishing" {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .unwrap_err(),
        "安装已取消"
    );
    assert_eq!(fs::read_dir(root.join("versions")).unwrap().count(), 0);
    cancel.store(false, Ordering::Relaxed);
    let result = installer
        .install_resolved_request(&root, &resolved, &cancel, |progress| {
            if progress.stage == "complete" {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
    assert_eq!(result.id, "private-instance");
    assert!(root
        .join("versions/private-instance/private-instance.json")
        .is_file());
}
