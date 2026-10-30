use super::*;
use serde_json::json;
use std::{collections::HashMap, io::Cursor, net::TcpListener, sync::Arc, thread, time::Instant};
struct FixtureServer {
    base: String,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn fixture_server(provider: Option<&str>, stall: Option<&str>) -> FixtureServer {
    fixture_server_with_forge_processor(provider, stall, false)
}
fn fixture_server_with_forge_processor(
    provider: Option<&str>,
    stall: Option<&str>,
    forge_processor: bool,
) -> FixtureServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let descriptor = |name: &str, bytes: &[u8]| json!({"url":format!("{base}/{name}"),"sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len()});
    let index = serde_json::to_vec(&json!({"objects":{}})).unwrap();
    let mut index_art = descriptor("index", &index);
    index_art["id"] = json!("fixture-index");
    let lib = b"library";
    let mut lib_art = descriptor("library", lib);
    lib_art["path"] = json!("example/base/1/base-1.jar");
    let metadata = serde_json::to_vec(&json!({"id":"fixture","mainClass":"net.minecraft.Main","javaVersion":{"majorVersion":21},"downloads":{"client":descriptor("client", b"client")},"arguments":{"game":["--version","${version_name}"],"jvm":[]},"assetIndex":index_art,"libraries":[{"name":"example:base:1","downloads":{"artifact":lib_art}}]})).unwrap();
    let catalog = serde_json::to_vec(&json!({"versions":[{"id":"fixture","url":format!("{base}/metadata"),"sha1":format!("{:x}",Sha1::digest(&metadata))}]})).unwrap();
    let mut routes = HashMap::<String, Vec<u8>>::from([
        ("/catalog".into(), catalog),
        ("/metadata".into(), metadata),
        ("/index".into(), index),
        ("/client".into(), b"client".to_vec()),
        ("/library".into(), lib.to_vec()),
    ]);
    if provider == Some("fabric") {
        let library_path = "net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar";
        let loader = jar(&[("loader.txt", b"fabric")]);
        let profile = json!({"id":"fabric-loader-0.16.0-fixture","inheritsFrom":"fixture","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","arguments":{"game":[],"jvm":["-Dfabric.test=true"]},"libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0","url":format!("{base}/"),"sha1":format!("{:x}",Sha1::digest(&loader)),"size":loader.len()}]});
        routes.insert(
            "/v2/versions/loader/fixture/0.16.0/profile/json".into(),
            serde_json::to_vec(&profile).unwrap(),
        );
        routes.insert(format!("/{library_path}"), loader);
    }
    if let Some(provider @ ("forge" | "neoforge")) = provider {
        let loader = jar(&[("loader.txt", b"forge")]);
        let processor = jar(&[(
            "META-INF/MANIFEST.MF",
            b"Manifest-Version: 1.0\r\nMain-Class: fixture.Processor\r\n\r\n",
        )]);
        let loader_path = if provider == "forge" {
            "net/minecraftforge/forge/fixture-1/forge-fixture-1.jar"
        } else {
            "net/neoforged/neoforge/1/neoforge-1-universal.jar"
        };
        let loader_name = if provider == "forge" {
            "net.minecraftforge:forge:fixture-1"
        } else {
            "net.neoforged:neoforge:1:universal"
        };
        let embedded_art = |path: &str, bytes: &[u8]| json!({"url":"","path":path,"sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len()});
        let runtime_library =
            json!({"name":loader_name,"downloads":{"artifact":embedded_art(loader_path,&loader)}});
        let processor_library = json!({"name":"fixture:processor:1","downloads":{"artifact":embedded_art("fixture/processor/1/processor-1.jar",&processor)}});
        let profile = json!({"id":"loader-fixture","inheritsFrom":"fixture","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher","arguments":{"game":["--fml.mcVersion","fixture"],"jvm":[]},"libraries":if provider == "forge" { vec![runtime_library.clone()] } else { vec![] }});
        let profile_raw = serde_json::to_vec(&profile).unwrap();
        let install = json!({"spec":1,"minecraft":"fixture","json":"/version.json","data":if provider == "neoforge" { json!({"MC_SRG":{"client":"[net.minecraft:client:fixture:srg]"},"MC_EXTRA":{"client":"[net.minecraft:client:fixture:extra]"},"PATCHED":{"client":"[net.neoforged:neoforge:1:client]"}}) } else { json!({}) },"libraries":[runtime_library,processor_library],"processors":if provider == "neoforge" { vec![json!({"jar":"fixture:processor:1","classpath":[],"args":["--output","{MC_SRG}","--output","{MC_EXTRA}","--output","{PATCHED}"],"sides":["client"]})] } else if forge_processor { vec![json!({"jar":"fixture:processor:1","classpath":[],"args":[],"sides":["client"]})] } else { vec![] }});
        let install_raw = serde_json::to_vec(&install).unwrap();
        let archive = jar(&[
            ("install_profile.json", &install_raw),
            ("version.json", &profile_raw),
            (&format!("maven/{loader_path}"), &loader),
            ("maven/fixture/processor/1/processor-1.jar", &processor),
        ]);
        let route = if provider == "forge" {
            "/net/minecraftforge/forge/fixture-1/forge-fixture-1-installer.jar"
        } else {
            "/releases/net/neoforged/neoforge/1/neoforge-1-installer.jar"
        };
        routes.insert(
            format!("{route}.sha1"),
            format!("{:x}", Sha1::digest(&archive)).into_bytes(),
        );
        routes.insert(route.into(), archive);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let stall = stall.map(str::to_owned);
    let worker = thread::spawn(move || {
        let mut clients = vec![];
        while !stopping.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                    if stall.as_deref() == Some(path.as_str())
                        || stall.as_deref() == Some(format!("{path}#body").as_str())
                    {
                        if stall.as_ref().is_some_and(|s| s.ends_with("#body")) {
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nc");
                        }
                        clients.push(stream);
                        continue;
                    }
                    if let Some(body) = routes.get(&path) {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(body);
                    } else {
                        let _ = stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(e) => panic!("{e}"),
            }
        }
    });
    FixtureServer {
        base,
        stop,
        thread: Some(worker),
    }
}
fn jar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in files {
        archive
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(body).unwrap();
    }
    archive.finish().unwrap().into_inner()
}
fn installer(server: &FixtureServer) -> Installer {
    let mut i = Installer::new().unwrap();
    i.endpoint = Some(format!("{}/catalog", server.base));
    i
}
fn request(name: &str, provider: Option<&str>) -> InstallRequest {
    InstallRequest {
        minecraft: "fixture".into(),
        name: name.into(),
        components: provider
            .map(|p| ComponentSelection {
                provider: p.into(),
                version: if p.eq_ignore_ascii_case("fabric") {
                    "0.16.0".into()
                } else {
                    "1".into()
                },
            })
            .into_iter()
            .collect(),
    }
}
#[test]
fn named_install_retains_original_version_and_client_jar() {
    let server = fixture_server(None, None);
    let root = tempfile::tempdir().unwrap();
    let events = Mutex::new(vec![]);
    let result = installer(&server)
        .install_request(
            root.path(),
            &request("自定义 游戏", None),
            &AtomicBool::new(false),
            |p| events.lock().unwrap().push(p),
        )
        .unwrap();
    assert_eq!(result.id, "自定义 游戏");
    let data: Value = serde_json::from_slice(
        &fs::read(root.path().join("versions/自定义 游戏/自定义 游戏.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(data["id"], "自定义 游戏");
    assert_eq!(data["jar"], "自定义 游戏");
    assert_eq!(data["clientVersion"], "fixture");
    assert!(data.get("inheritsFrom").is_none());
    let scanned = pcl_core::scan_instances(root.path()).unwrap();
    assert_eq!(scanned[0].minecraft_version, "fixture");
    assert_eq!(
        fs::read(root.path().join("versions/自定义 游戏/自定义 游戏.jar")).unwrap(),
        b"client"
    );
    let events = events.into_inner().unwrap();
    assert!(events
        .last()
        .unwrap()
        .steps
        .iter()
        .all(|s| s.state == "complete" && !s.label.is_empty()));
    assert_eq!(
        fs::read_dir(root.path().join("versions")).unwrap().count(),
        1
    );
}
#[test]
fn invalid_selection_and_dangling_collision_do_not_download_or_write() {
    let root = tempfile::tempdir().unwrap();
    let mut i = Installer::new().unwrap();
    i.endpoint = Some("http://127.0.0.1:1/catalog".into());
    for name in [
        "",
        "../escape",
        " spaced",
        "trailing ",
        ".",
        "..",
        "line\nfeed",
    ] {
        assert!(i
            .install_request(
                root.path(),
                &request(name, None),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err());
    }
    for provider in ["OptiFine", "LabyMod", "anything"] {
        assert!(i
            .install_request(
                root.path(),
                &request("valid", Some(provider)),
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap_err()
            .contains("暂不支持"));
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    #[cfg(unix)]
    {
        fs::create_dir(root.path().join("versions")).unwrap();
        std::os::unix::fs::symlink(
            root.path().join("absent"),
            root.path().join("versions/collision"),
        )
        .unwrap();
        assert!(i
            .install_request(
                root.path(),
                &request("collision", None),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err());
        assert!(fs::symlink_metadata(root.path().join("versions/collision"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
#[test]
fn stalled_headers_cancel_promptly_before_any_version_publication() {
    for stall in ["/catalog", "/client", "/client#body"] {
        let server = fixture_server(None, Some(stall));
        let root = tempfile::tempdir().unwrap();
        let i = installer(&server);
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(150));
                cancelled.store(true, Ordering::Relaxed);
            });
            assert!(i
                .install_request(root.path(), &request("cancelled", None), &cancelled, |_| {})
                .unwrap_err()
                .contains("取消"));
        });
        assert!(started.elapsed() < Duration::from_secs(2));
        if root.path().join("versions").exists() {
            assert_eq!(
                fs::read_dir(root.path().join("versions")).unwrap().count(),
                0
            );
        }
        if root.path().join("libraries").exists() {
            assert!(!walk_files(root.path()).iter().any(|p| p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".tmp")));
        }
    }
}
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![];
    for entry in fs::read_dir(root).unwrap().flatten() {
        if entry.file_type().unwrap().is_dir() {
            files.extend(walk_files(&entry.path()));
        } else {
            files.push(entry.path());
        }
    }
    files
}
#[test]
fn fabric_and_embedded_forge_profiles_publish_complete_named_instances() {
    for provider in ["fabric", "forge"] {
        let server = fixture_server(Some(provider), None);
        let root = tempfile::tempdir().unwrap();
        let events = Mutex::new(vec![]);
        installer(&server)
            .install_request(
                root.path(),
                &request("named-loader", Some(provider)),
                &AtomicBool::new(false),
                |p| events.lock().unwrap().push(p),
            )
            .unwrap();
        let data: Value = serde_json::from_slice(
            &fs::read(root.path().join("versions/named-loader/named-loader.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(data["id"], "named-loader");
        assert!(data.get("inheritsFrom").is_none());
        assert!(root.path().join("versions/named-loader/mods").is_dir());
        let library = data["libraries"].as_array().unwrap().last().unwrap();
        let path = library["downloads"]["artifact"]["path"].as_str().unwrap();
        assert!(root.path().join("libraries").join(path).is_file());
        let events = events.into_inner().unwrap();
        assert_eq!(events.last().unwrap().steps.len(), 9);
        assert!(events
            .iter()
            .all(|p| p.steps.iter().all(|s| !s.label.is_empty())));
        assert!(events
            .windows(2)
            .all(|p| p[1].network_bytes >= p[0].network_bytes));
        assert!(events
            .last()
            .unwrap()
            .steps
            .iter()
            .all(|s| s.state == "complete"));
        assert_eq!(
            fs::read_dir(root.path().join("versions")).unwrap().count(),
            1
        );
    }
}
#[test]
#[cfg(unix)]
fn neoforge_retains_generated_runtime_libraries_missing_from_launch_json() {
    let server = fixture_server(Some("neoforge"), None);
    let root = tempfile::tempdir().unwrap();
    let java_root = tempfile::tempdir().unwrap();
    let runtime = jar(&[("runtime.txt", b"runtime")]);
    fs::write(java_root.path().join("runtime.jar"), runtime).unwrap();
    let java = java_root.path().join("java");
    fs::write(&java,format!("#!/bin/sh\nif [ \"$1\" = '-version' ]; then echo 'openjdk version \"21.0.1\"' >&2; exit 0; fi\nshift 4\nwhile [ $# -gt 0 ]; do shift; mkdir -p \"$(dirname \"$1\")\"; cp '{}' \"$1\"; shift; done\n",java_root.path().join("runtime.jar").display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&java, fs::Permissions::from_mode(0o700)).unwrap();
    installer(&server)
        .with_java(java)
        .install_request(
            root.path(),
            &request("neo", Some("NeoForge")),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    for path in [
        "net/minecraft/client/fixture/client-fixture-srg.jar",
        "net/minecraft/client/fixture/client-fixture-extra.jar",
        "net/neoforged/neoforge/1/neoforge-1-client.jar",
        "net/neoforged/neoforge/1/neoforge-1-universal.jar",
    ] {
        assert!(
            root.path().join("libraries").join(path).is_file(),
            "missing {path}"
        );
    }
    let data: Value =
        serde_json::from_slice(&fs::read(root.path().join("versions/neo/neo.json")).unwrap())
            .unwrap();
    assert_eq!(data["libraries"].as_array().unwrap().len(), 1);
    assert!(root.path().join("versions/neo/neo.jar").is_file());
}
#[test]
#[cfg(unix)]
fn fixed_java_failure_prevents_forge_and_neoforge_processors_and_publication() {
    use std::os::unix::fs::PermissionsExt;
    for provider in ["forge", "neoforge"] {
        for missing in [false, true] {
            let server = fixture_server_with_forge_processor(Some(provider), None, true);
            let root = tempfile::tempdir().unwrap();
            let project = tempfile::tempdir().unwrap();
            let fallback = project.path().join("runtime-compatible/bin/java");
            let fallback_marker = project.path().join("fallback-ran");
            fs::create_dir_all(fallback.parent().unwrap()).unwrap();
            // A valid automatic alternative makes the former fallback bug
            // observable through the real metadata/loader installation path.
            fs::write(
                &fallback,
                format!(
                    "#!/bin/sh\nprintf 'ran\\n' >> '{}'\nprintf 'openjdk version \"21.0.1\"\\n' >&2\n",
                    fallback_marker.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&fallback, fs::Permissions::from_mode(0o700)).unwrap();
            let selected = project.path().join("selected-java");
            if !missing {
                fs::write(
                    &selected,
                    "#!/bin/sh\nprintf 'openjdk version \"17.0.1\"\\n' >&2\n",
                )
                .unwrap();
                fs::set_permissions(&selected, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let reached_processor = AtomicBool::new(false);
            let error = installer(&server)
                .with_project(project.path())
                .with_java(&selected)
                .install_request(
                    root.path(),
                    &request("fixed-loader", Some(provider)),
                    &AtomicBool::new(false),
                    |p| {
                        if p.stage == "component_install" {
                            reached_processor.store(true, Ordering::Relaxed);
                        }
                    },
                )
                .unwrap_err();
            assert!(error.contains("指定的 Java"), "{provider}: {error}");
            assert!(!fallback_marker.exists(), "{provider} fell back");
            assert!(!reached_processor.load(Ordering::Relaxed));
            assert!(fs::read_dir(root.path().join("versions"))
                .unwrap()
                .next()
                .is_none());
            // A failed processor selection removes its owned workspace while
            // retaining already verified ordinary installer cache bytes.
            assert_eq!(
                fs::read(root.path().join("libraries/example/base/1/base-1.jar")).unwrap(),
                b"library"
            );
            assert!(!walk_files(root.path()).iter().any(|p| p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".tmp")));
        }
    }
}
#[test]
fn cancellation_at_publication_does_not_replace_external_collision() {
    let server = fixture_server(None, None);
    let root = tempfile::tempdir().unwrap();
    let result = installer(&server).install_request(
        root.path(),
        &request("raced", None),
        &AtomicBool::new(false),
        |p| {
            if p.stage == "publishing" {
                let dir = root.path().join("versions/raced");
                fs::create_dir(&dir).unwrap();
                fs::write(dir.join("external"), b"preserve").unwrap();
            }
        },
    );
    assert!(result.is_err());
    assert_eq!(
        fs::read(root.path().join("versions/raced/external")).unwrap(),
        b"preserve"
    );
    assert_eq!(
        fs::read_dir(root.path().join("versions")).unwrap().count(),
        1
    );
}
#[test]
fn component_cancellation_removes_installer_workspace_and_keeps_verified_base_cache() {
    for provider in ["fabric", "forge"] {
        let server = fixture_server(Some(provider), None);
        let root = tempfile::tempdir().unwrap();
        let cancelled = AtomicBool::new(false);
        let result = installer(&server).install_request(
            root.path(),
            &request("cancelled-loader", Some(provider)),
            &cancelled,
            |p| {
                if matches!(
                    p.stage.as_str(),
                    "component_download" | "component_metadata"
                ) {
                    cancelled.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(result.unwrap_err().contains("取消"));
        assert_eq!(
            fs::read_dir(root.path().join("versions")).unwrap().count(),
            0
        );
        assert_eq!(
            fs::read(root.path().join("libraries/example/base/1/base-1.jar")).unwrap(),
            b"library"
        );
        assert!(!walk_files(root.path()).iter().any(|p| p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".tmp")));
    }
}
#[test]
#[cfg(unix)]
fn cancelled_neoforge_child_is_reaped_and_its_working_files_are_removed() {
    use std::os::unix::fs::PermissionsExt;
    let server = fixture_server(Some("neoforge"), None);
    let root = tempfile::tempdir().unwrap();
    let java_root = tempfile::tempdir().unwrap();
    let java = java_root.path().join("java");
    let pid_file = java_root.path().join("pid");
    fs::write(&java, format!("#!/bin/sh\nif [ \"$1\" = '-version' ]; then echo 'openjdk version \"21.0.1\"' >&2; exit 0; fi\necho $$ > '{}'\nexec /bin/sleep 60\n", pid_file.display())).unwrap();
    fs::set_permissions(&java, fs::Permissions::from_mode(0o700)).unwrap();
    let cancelled = AtomicBool::new(false);
    let started = Instant::now();
    thread::scope(|scope| {
        scope.spawn(|| {
            while !pid_file.exists() && started.elapsed() < Duration::from_secs(2) {
                thread::sleep(Duration::from_millis(10));
            }
            cancelled.store(true, Ordering::Relaxed);
        });
        assert!(installer(&server)
            .with_java(&java)
            .install_request(
                root.path(),
                &request("cancelled-neo", Some("neoforge")),
                &cancelled,
                |_| {}
            )
            .unwrap_err()
            .contains("取消"));
    });
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid: i32 = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_eq!(
        fs::read_dir(root.path().join("versions")).unwrap().count(),
        0
    );
    assert!(!root.path().join("libraries/net/neoforged").exists());
}
#[test]
fn parallel_worker_cancellation_never_masks_cleanup_failure() {
    let failure = Mutex::new(None);
    record_failure(&failure, "网络中断".into());
    record_failure(&failure, "取消清理失败：未完成文件无法删除".into());
    record_failure(&failure, "安装已取消".into());
    assert_eq!(
        failure.into_inner().unwrap().unwrap(),
        "取消清理失败：未完成文件无法删除"
    );
}
