//! Real loopback resolution/downloads and synthetic Java processors run only in
//! project-local fixtures. Retargeting is performed by an unconfined observer,
//! as it would be by another process, while the installer owns its worker.
use super::*;
use serde_json::json;
use std::{
    collections::HashMap,
    io::Cursor,
    net::TcpListener,
    os::unix::fs::{symlink, MetadataExt},
    process::Command,
    sync::{Barrier, OnceLock},
    thread,
    time::Instant,
};
fn fixture() -> tempfile::TempDir {
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../work/modpack-install-2026-10-06/publication/bound-installer");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(work).unwrap()
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
fn java_processor() -> &'static (Vec<u8>, u32) {
    static PROCESSOR: OnceLock<(Vec<u8>, u32)> = OnceLock::new();
    PROCESSOR.get_or_init(|| {
        let dir = fixture();
        let source = r#"import java.nio.file.*;
public class Processor {
  public static void main(String[] args) throws Exception {
    Path source=null, output=null, pause=null, outside=null;
    for(int i=0;i<args.length;i+=2) {
      switch(args[i]) {
      case "--source": source=Path.of(args[i+1]); break;
      case "--output": output=Path.of(args[i+1]); break;
      case "--pause": pause=Path.of(args[i+1]); break;
      case "--outside": outside=Path.of(args[i+1]); break;
      }
    }
    if(pause!=null) {
      Files.writeString(pause,Long.toString(ProcessHandle.current().pid()));
      while(!Files.exists(Path.of(pause.toString()+".release"))) Thread.sleep(10);
    }
    if(outside!=null) Files.writeString(outside,"incorrect outside write");
    Path temporary=Files.createTempFile("bound-processor-", ".tmp");
    if(!temporary.startsWith(Path.of(System.getProperty("java.io.tmpdir")))) throw new Exception("unbound tmpdir");
    Files.delete(temporary);
    Files.createDirectories(output.getParent());
    Files.copy(source,output,StandardCopyOption.REPLACE_EXISTING);
  }
}"#;
        fs::write(dir.path().join("Processor.java"), source).unwrap();
        let output = Command::new("javac").arg(dir.path().join("Processor.java")).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let class = fs::read(dir.path().join("Processor.class")).unwrap();
        let archive = jar(&[("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\nMain-Class: Processor\r\n\r\n"), ("Processor.class", &class)]);
        let output = Command::new("java").arg("-version").output().unwrap();
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stderr);
        let major = text.split("version \"").nth(1).unwrap().split(['.', '"']).next().unwrap().parse().unwrap();
        (archive, major)
    })
}
struct Server {
    base: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Plan {
    server: Server,
    installer: Installer,
    resolved: ResolvedInstallRequest,
    runtime: Option<Vec<u8>>,
}
fn fixture_plan(
    minecraft: &str,
    provider: Option<&str>,
    version: &str,
    pause: bool,
    outside: Option<&Path>,
) -> Plan {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let desc = |path: &str, bytes: &[u8]| json!({"url":format!("{base}/{path}"),"sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len()});
    let index = serde_json::to_vec(&json!({"objects":{}})).unwrap();
    let mut index_d = desc("index", &index);
    index_d["id"] = "fixture-index".into();
    let mut library = desc("library", b"library");
    library["path"] = "fixture/base/1/base-1.jar".into();
    let major = if provider.is_some() {
        java_processor().1
    } else {
        21
    };
    let metadata = serde_json::to_vec(&json!({"id":minecraft,"mainClass":"net.minecraft.Main","javaVersion":{"majorVersion":major},"downloads":{"client":desc("client",b"client")},"assetIndex":index_d,"libraries":[{"name":"fixture:base:1","downloads":{"artifact":library}}],"arguments":{"game":[],"jvm":[]}})).unwrap();
    let catalog = serde_json::to_vec(&json!({"versions":[{"id":minecraft,"url":format!("{base}/metadata"),"sha1":format!("{:x}",Sha1::digest(&metadata))}]})).unwrap();
    let mut routes = HashMap::from([
        ("/catalog".into(), catalog),
        ("/metadata".into(), metadata),
        ("/index".into(), index),
        ("/client".into(), b"client".to_vec()),
        ("/library".into(), b"library".to_vec()),
    ]);
    let mut runtime = None;
    if provider == Some("fabric") {
        let loader = jar(&[("loader.txt", b"fabric")]);
        let library_path = "/net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar";
        let profile = serde_json::to_vec(&json!({"id":"fabric-fixture","inheritsFrom":minecraft,"mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0","url":format!("{base}/")}],"arguments":{"game":[],"jvm":["-Dfabric.fixture=true"]}})).unwrap();
        routes.insert(
            format!("/v2/versions/loader/{minecraft}/0.16.0/profile/json"),
            profile,
        );
        routes.insert(
            format!("{library_path}.sha1"),
            format!("{:x}", Sha1::digest(&loader)).into_bytes(),
        );
        routes.insert(library_path.into(), loader);
    } else if let Some(provider) = provider {
        let (processor, _) = java_processor();
        let (group, artifact, normalized) = if provider == "forge" {
            (
                "net.minecraftforge",
                "forge",
                format!("{minecraft}-{version}"),
            )
        } else if minecraft == "1.20.1" {
            (
                "net.neoforged",
                "forge",
                format!(
                    "1.20.1-{}",
                    version.strip_prefix("1.20.1-").unwrap_or(version)
                ),
            )
        } else {
            ("net.neoforged", "neoforge", version.into())
        };
        let coordinate = format!("{group}:{artifact}:{normalized}");
        let loader_path = components::maven_path(&coordinate).unwrap();
        let loader = jar(&[("loader.txt", b"loader")]);
        let output = jar(&[("runtime.txt", b"generated runtime")]);
        let embedded = |path: &str, bytes: &[u8]| json!({"url":"","path":path,"sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len()});
        let runtime_lib =
            json!({"name":coordinate,"downloads":{"artifact":embedded(&loader_path,&loader)}});
        let processor_lib = json!({"name":"fixture:processor:1","downloads":{"artifact":embedded("fixture/processor/1/processor-1.jar",processor)}});
        let generated_lib = json!({"name":"fixture:generated:1","downloads":{"artifact":embedded("fixture/generated/1/generated-1.jar",&output)}});
        let profile = serde_json::to_vec(&json!({"id":"loader-fixture","inheritsFrom":minecraft,"mainClass":"fixture.Main","libraries":[runtime_lib.clone()],"arguments":{"game":[],"jvm":[]}})).unwrap();
        let mut args = vec![
            json!("--source"),
            json!("{RUNTIME}"),
            json!("--output"),
            json!("{PATCHED}"),
        ];
        if pause {
            args.extend([json!("--pause"), json!("{ROOT}/processor.pause")]);
        }
        if let Some(outside) = outside {
            args.extend([json!("--outside"), json!(outside)]);
        }
        let processor_spec = json!({"jar":"fixture:processor:1","classpath":[],"args":args,"sides":["client"],"outputs":{"{PATCHED}":format!("{:x}",Sha1::digest(&output))}});
        let install = serde_json::to_vec(&json!({"spec":1,"minecraft":minecraft,"json":"/version.json","data":{"PATCHED":{"client":"[fixture:generated:1]"},"RUNTIME":{"client":"/payload.jar"}},"libraries":[runtime_lib,processor_lib,generated_lib],"processors":[processor_spec.clone(),processor_spec]})).unwrap();
        let archive = jar(&[
            ("install_profile.json", &install),
            ("version.json", &profile),
            ("payload.jar", &output),
            (&format!("maven/{loader_path}"), &loader),
            ("maven/fixture/processor/1/processor-1.jar", processor),
        ]);
        let route = format!(
            "/{}{}",
            if provider == "forge" { "" } else { "releases/" },
            components::maven_path(&format!("{coordinate}:installer")).unwrap()
        );
        routes.insert(
            format!("{route}.sha1"),
            format!("{:x}", Sha1::digest(&archive)).into_bytes(),
        );
        routes.insert(route, archive);
        runtime = Some(output);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let worker = thread::spawn(move || {
        while !stopping.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut buf = [0; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("");
                    if let Some(body) = routes.get(path) {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(body);
                    } else {
                        let _=stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(e) => panic!("{e}"),
            }
        }
    });
    let server = Server {
        base: base.clone(),
        stop,
        worker: Some(worker),
    };
    let factory = pcl_network::ClientFactory::new(pcl_network::Policy {
        proxy: pcl_network::ProxyPolicy::None,
        ..Default::default()
    })
    .unwrap();
    let mut installer = Installer::from_factory(&factory).unwrap();
    installer.endpoint = Some(format!("{base}/catalog"));
    if provider.is_some() {
        installer = installer.with_java("/usr/bin/java");
    }
    let request = InstallRequest {
        minecraft: minecraft.into(),
        name: "private-instance".into(),
        components: provider
            .map(|p| ComponentSelection {
                provider: p.into(),
                version: version.into(),
            })
            .into_iter()
            .collect(),
    };
    let resolved = installer
        .resolve_request(&request, &AtomicBool::new(false))
        .unwrap();
    Plan {
        server,
        installer,
        resolved,
        runtime,
    }
}
fn root(parent: &Path) -> PathBuf {
    let root = parent.join("private-root");
    fs::create_dir(&root).unwrap();
    root
}
fn assert_outside(path: &Path) {
    assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"preserve");
    assert_eq!(fs::read_dir(path).unwrap().count(), 1);
}
fn outside(parent: &Path) -> PathBuf {
    let path = parent.join("outside");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("sentinel"), b"preserve").unwrap();
    path
}
fn workspace(root: &Path) -> PathBuf {
    fs::read_dir(root.join("versions"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".install-")
        })
        .unwrap()
        .join("component-work")
}
fn wait_pause(root: &Path) -> (PathBuf, u32) {
    let start = Instant::now();
    loop {
        if root.join("versions").exists() {
            for e in fs::read_dir(root.join("versions")).unwrap() {
                let p = e.unwrap().path().join("component-work/processor.pause");
                if let Ok(pid) = fs::read_to_string(&p) {
                    if let Ok(pid) = pid.parse() {
                        return (p, pid);
                    }
                }
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Java did not reach private pause"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn resolved_bound_root_retarget_and_cache_copy_never_write_replacement_root() {
    let mut plan = fixture_plan("fixture", None, "", false, None);
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let cache = fixture.path().join("cache");
    fs::create_dir_all(cache.join("libraries/fixture/base/1")).unwrap();
    fs::write(
        cache.join("libraries/fixture/base/1/base-1.jar"),
        b"library",
    )
    .unwrap();
    plan.installer = plan.installer.with_cache_source(&cache).unwrap();
    let held = fs::File::open(&root).unwrap();
    let barrier = Barrier::new(2);
    let cancel = AtomicBool::new(false);
    thread::scope(|scope| {
        let task = scope.spawn(|| {
            plan.installer
                .install_resolved_request_bound(held, &plan.resolved, &cancel, |p| {
                    if p.stage == "metadata" {
                        barrier.wait();
                        barrier.wait();
                    }
                })
        });
        barrier.wait();
        fs::rename(&root, fixture.path().join("moved-root")).unwrap();
        symlink(&outside, &root).unwrap();
        barrier.wait();
        let result = task.join().unwrap().unwrap();
        assert!(result.files_reused >= 1);
    });
    assert_outside(&outside);
    let copied = fixture
        .path()
        .join("moved-root/libraries/fixture/base/1/base-1.jar");
    assert_ne!(
        fs::metadata(&copied).unwrap().ino(),
        fs::metadata(cache.join("libraries/fixture/base/1/base-1.jar"))
            .unwrap()
            .ino()
    );
    fs::write(&copied, b"modified owned copy").unwrap();
    assert_eq!(
        fs::read(cache.join("libraries/fixture/base/1/base-1.jar")).unwrap(),
        b"library"
    );
    // Caller and a subsequent legacy installation retain normal write rights.
    fs::write(fixture.path().join("caller-write"), b"allowed").unwrap();
    plan.installer
        .install_resolved_request(
            &fixture.path().join("legacy-root"),
            &plan.resolved,
            &cancel,
            |_| {},
        )
        .unwrap();
    assert!(!plan.server.base.is_empty());
}
#[test]
fn resolved_bound_download_parent_retarget_preserves_external_directory() {
    let plan = fixture_plan("fixture", None, "", false, None);
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let barrier = Barrier::new(2);
    let selected = AtomicBool::new(false);
    let cancel = AtomicBool::new(false);
    thread::scope(|scope| {
        let task = scope.spawn(|| {
            plan.installer.install_resolved_request_bound(
                fs::File::open(&root).unwrap(),
                &plan.resolved,
                &cancel,
                |p| {
                    if p.message == "正在下载游戏依赖库" && !selected.swap(true, Ordering::SeqCst)
                    {
                        barrier.wait();
                        barrier.wait();
                    }
                },
            )
        });
        barrier.wait();
        fs::rename(
            root.join("libraries"),
            fixture.path().join("detached-libraries"),
        )
        .unwrap();
        symlink(&outside, root.join("libraries")).unwrap();
        barrier.wait();
        assert!(task.join().unwrap().is_err());
    });
    assert_outside(&outside);
    assert!(!root.join("versions/private-instance").exists());
}
#[test]
fn bound_leaf_hardlinks_and_special_nodes_fail_without_truncating_outside_bytes() {
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let dir = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    fs::hard_link(outside.join("sentinel"), root.join("linked")).unwrap();
    assert!(dir.file("linked").is_err());
    assert!(dir.files().is_err());
    let target = dir.file("raced").unwrap();
    fs::hard_link(outside.join("sentinel"), root.join("raced")).unwrap();
    assert!(target.open().is_err());
    assert!(target.write(b"overwrite").is_err());
    let fifo = std::ffi::CString::new(root.join("fifo").to_string_lossy().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(dir.file("fifo").is_err());
    assert_outside(&outside);
}

#[test]
fn bound_file_streams_ignore_redirected_path_adapters() {
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let dir = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    let mut source = dir.file("nested/source").unwrap();
    source.write(b"retained source bytes").unwrap();
    // A deterministic adapter redirection must not redirect native I/O. This
    // fault would make the previous path-based open read the outside sentinel.
    source.redirect_adapter_for_test(outside.join("sentinel"));
    let mut actual = Vec::new();
    source.open().unwrap().read_to_end(&mut actual).unwrap();
    assert_eq!(actual, b"retained source bytes");
    let mut target = dir.file("nested/created").unwrap();
    target.redirect_adapter_for_test(outside.join("sentinel"));
    let mut writer = target.open_new_writer().unwrap();
    writer.write_all(b"new retained bytes").unwrap();
    drop(writer);
    assert_eq!(
        fs::read(root.join("nested/created")).unwrap(),
        b"new retained bytes"
    );
    assert!(target.write(b"must not truncate").is_err());
    assert_outside(&outside);
}

#[test]
fn bound_tree_enumeration_uses_retained_directories_and_native_node_checks() {
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let mut dir = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    dir.file("nested/one").unwrap().write(b"one").unwrap();
    dir.file("two").unwrap().write(b"two").unwrap();
    dir.redirect_adapter_for_test(outside.clone());
    let mut files = dir.files().unwrap();
    files.sort();
    assert_eq!(
        files,
        vec![PathBuf::from("nested/one"), PathBuf::from("two")]
    );
    symlink(&outside, root.join("untrusted-link")).unwrap();
    assert!(dir.files().is_err());
    assert_outside(&outside);
}

#[test]
fn bound_stream_creation_preserves_existing_directory_mode_and_uses_private_new_files() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture();
    let root = root(fixture.path());
    fs::create_dir(root.join("existing")).unwrap();
    fs::set_permissions(root.join("existing"), fs::Permissions::from_mode(0o755)).unwrap();
    let dir = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    let target = dir.file("existing/library.jar").unwrap();
    target.write(b"library bytes").unwrap();
    assert_eq!(
        fs::metadata(root.join("existing"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let created = fs::metadata(root.join("existing/library.jar")).unwrap();
    assert_eq!(created.permissions().mode() & 0o777, 0o600);
    assert_eq!(created.nlink(), 1);
    assert!(target.open_new_writer().is_err());
    assert_eq!(
        fs::read(root.join("existing/library.jar")).unwrap(),
        b"library bytes"
    );
}

#[test]
fn legacy_write_overwrite_contract_and_explicit_new_writer_are_distinct() {
    let fixture = fixture();
    let root = root(fixture.path());
    let dir = InstallDir::legacy(root.clone());
    let target = dir.file("existing").unwrap();
    target.write(b"long original bytes").unwrap();
    target.write(b"short").unwrap();
    assert_eq!(fs::read(root.join("existing")).unwrap(), b"short");
    assert!(target.open_new_writer().is_err());
    let mut bytes = Vec::new();
    target.open().unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"short");
}

#[test]
fn bound_output_publishes_new_bytes_only_after_native_sealing() {
    let fixture = fixture();
    let root = root(fixture.path());
    let dir = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    let target = dir.file("nested/download.jar").unwrap();
    let mut output = target.output(1024).unwrap();
    output
        .file_mut()
        .write_all(b"verified download bytes")
        .unwrap();
    assert!(!root.join("nested/download.jar").exists());
    output.publish().unwrap();
    assert_eq!(
        fs::read(root.join("nested/download.jar")).unwrap(),
        b"verified download bytes"
    );
    assert!(target.output(1024).unwrap().publish().is_err());
    assert_eq!(
        fs::read(root.join("nested/download.jar")).unwrap(),
        b"verified download bytes"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn bound_output_cancellation_releases_anonymous_storage_without_named_cleanup() {
    let fixture = fixture();
    let root = root(fixture.path());
    let dir = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    let target = dir.file("cancelled").unwrap();
    let mut output = target.output(1024).unwrap();
    assert_eq!(output.file_mut().metadata().unwrap().nlink(), 0);
    output.file_mut().write_all(b"incomplete bytes").unwrap();
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert_eq!(output.close("安装已取消".into()), "安装已取消");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    let mut discarded = target.output(1024).unwrap();
    discarded.file_mut().write_all(b"discarded bytes").unwrap();
    discarded.discard().unwrap();
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn bound_output_no_replace_and_redirected_adapter_preserve_outside_bytes() {
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let dir = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    let mut target = dir.file("native-output").unwrap();
    target.redirect_adapter_for_test(outside.join("sentinel"));
    let mut output = target.output(1024).unwrap();
    output.file_mut().write_all(b"held output").unwrap();
    output.publish().unwrap();
    assert_eq!(
        fs::read(root.join("native-output")).unwrap(),
        b"held output"
    );
    let raced_target = dir.file("raced-output").unwrap();
    let mut raced = raced_target.output(1024).unwrap();
    raced.file_mut().write_all(b"must not replace").unwrap();
    symlink(outside.join("sentinel"), root.join("raced-output")).unwrap();
    assert!(raced.publish().is_err());
    assert!(fs::symlink_metadata(root.join("raced-output"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_outside(&outside);
}

#[test]
fn output_seal_limit_prevents_publication_and_legacy_keeps_persist_overwrite() {
    let fixture = fixture();
    let root = root(fixture.path());
    let bound = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    let target = bound.file("oversize").unwrap();
    let mut output = target.output(3).unwrap();
    output.file_mut().write_all(b"oversize bytes").unwrap();
    assert!(output.publish().is_err());
    assert!(!root.join("oversize").exists());
    let legacy = InstallDir::legacy(root.clone());
    let existing = legacy.file("legacy").unwrap();
    existing.write(b"previous longer bytes").unwrap();
    let mut output = existing.output(1024).unwrap();
    output.file_mut().write_all(b"replacement").unwrap();
    output.publish().unwrap();
    assert_eq!(fs::read(root.join("legacy")).unwrap(), b"replacement");
    let cancelled = legacy
        .file("legacy-cancelled")
        .unwrap()
        .output(1024)
        .unwrap();
    assert_eq!(
        cancelled.close("fixture transfer failure".into()),
        "fixture transfer failure"
    );
    assert!(!root.join("legacy-cancelled").exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn optional_bound_streams_distinguish_missing_from_disallowed_existing_nodes() {
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let dir = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    let missing = dir.file("missing").unwrap();
    assert!(missing.open_optional().unwrap().is_none());
    missing.write(b"now present").unwrap();
    let mut bytes = Vec::new();
    missing
        .open_optional()
        .unwrap()
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"now present");
    let linked = dir.file("linked").unwrap();
    fs::hard_link(outside.join("sentinel"), root.join("linked")).unwrap();
    assert!(linked.open_optional().is_err());
    let dangling = dir.file("dangling").unwrap();
    symlink("absent-target", root.join("dangling")).unwrap();
    assert!(dangling.open_optional().is_err());
    assert_outside(&outside);
}

#[test]
fn output_parent_preparation_uses_native_bound_parent_and_legacy_path_contract() {
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let bound = InstallDir::bound(fs::File::open(&root).unwrap()).unwrap();
    let mut target = bound.file("nested/native").unwrap();
    target.redirect_adapter_for_test(outside.join("must-not-create/target"));
    target.prepare_parent().unwrap();
    assert!(!outside.join("must-not-create").exists());
    let mut output = target.output(1024).unwrap();
    output.file_mut().write_all(b"native parent").unwrap();
    output.publish().unwrap();
    assert_eq!(
        fs::read(root.join("nested/native")).unwrap(),
        b"native parent"
    );
    let legacy = InstallDir::legacy(root.clone());
    assert!(legacy
        .file("absent")
        .unwrap()
        .open_optional()
        .unwrap()
        .is_none());
    assert!(legacy.file("nested").unwrap().open_optional().is_err());
    let deep = legacy.file("deep/not-yet-created/output").unwrap();
    let mut output = deep.output(1024).unwrap();
    output.file_mut().write_all(b"legacy parent").unwrap();
    output.publish().unwrap();
    assert_eq!(
        fs::read(root.join("deep/not-yet-created/output")).unwrap(),
        b"legacy parent"
    );
    assert_outside(&outside);
}
#[test]
fn bound_forge_and_all_neoforge_coordinate_branches_run_real_private_java_processors() {
    for (minecraft, provider, version) in [
        ("fixture", "forge", "1"),
        ("fixture", "neoforge", "1"),
        ("1.20.1", "neoforge", "47.1.106"),
        ("1.20.1", "neoforge", "1.20.1-47.1.106"),
    ] {
        let plan = fixture_plan(minecraft, Some(provider), version, false, None);
        let fixture = fixture();
        let root = root(fixture.path());
        plan.installer
            .install_resolved_request_bound(
                fs::File::open(&root).unwrap(),
                &plan.resolved,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        assert_eq!(
            fs::read(root.join("libraries/fixture/generated/1/generated-1.jar")).unwrap(),
            plan.runtime.unwrap()
        );
        assert!(root
            .join("versions/private-instance/private-instance.jar")
            .is_file());
        assert!(!root
            .join("versions/private-instance/component-work")
            .exists());
        assert_eq!(fs::read_dir(root.join("versions")).unwrap().count(), 1);
    }
}
#[test]
fn bound_java_explicit_outside_write_is_denied_and_failure_cleans_private_version() {
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let plan = fixture_plan(
        "fixture",
        Some("forge"),
        "1",
        false,
        Some(&outside.join("sentinel")),
    );
    let error = plan
        .installer
        .install_resolved_request_bound(
            fs::File::open(&root).unwrap(),
            &plan.resolved,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
    assert!(error.contains("组件安装处理器失败"), "{error}");
    assert_outside(&outside);
    assert_eq!(fs::read_dir(root.join("versions")).unwrap().count(), 0);
}
#[test]
fn bound_running_java_parent_retarget_cannot_publish_or_modify_external_files() {
    let fixture = fixture();
    let root = root(fixture.path());
    let outside = outside(fixture.path());
    let plan = fixture_plan("fixture", Some("neoforge"), "1", true, None);
    let cancel = AtomicBool::new(false);
    thread::scope(|scope| {
        let task = scope.spawn(|| {
            plan.installer.install_resolved_request_bound(
                fs::File::open(&root).unwrap(),
                &plan.resolved,
                &cancel,
                |_| {},
            )
        });
        let (pause, pid) = wait_pause(&root);
        let work = workspace(&root);
        fs::rename(
            work.join("libraries"),
            fixture.path().join("detached-processor-libraries"),
        )
        .unwrap();
        symlink(&outside, work.join("libraries")).unwrap();
        fs::write(
            PathBuf::from(format!("{}.release", pause.display())),
            b"continue",
        )
        .unwrap();
        let error = task.join().unwrap().unwrap_err();
        assert!(error.contains("组件安装处理器失败"), "{error}");
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    });
    assert_outside(&outside);
    assert!(!root.join("versions/private-instance").exists());
}
#[test]
fn bound_cancelled_java_is_reaped_before_private_workspace_cleanup_and_late_cancel_succeeds() {
    let fixture = fixture();
    let root = root(fixture.path());
    let plan = fixture_plan("fixture", Some("forge"), "1", true, None);
    let cancel = AtomicBool::new(false);
    thread::scope(|scope| {
        let task = scope.spawn(|| {
            plan.installer.install_resolved_request_bound(
                fs::File::open(&root).unwrap(),
                &plan.resolved,
                &cancel,
                |_| {},
            )
        });
        let (_, pid) = wait_pause(&root);
        cancel.store(true, Ordering::Release);
        assert_eq!(task.join().unwrap().unwrap_err(), "安装已取消");
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    });
    assert_eq!(fs::read_dir(root.join("versions")).unwrap().count(), 0);
    let plan = fixture_plan("fixture", None, "", false, None);
    cancel.store(false, Ordering::Release);
    plan.installer
        .install_resolved_request_bound(
            fs::File::open(&root).unwrap(),
            &plan.resolved,
            &cancel,
            |p| {
                if p.stage == "complete" {
                    cancel.store(true, Ordering::Release);
                }
            },
        )
        .unwrap();
    assert!(root
        .join("versions/private-instance/private-instance.json")
        .is_file());
}

#[test]
fn captured_launch_profile_matches_actual_private_install_including_fabric_evidence() {
    for (provider, version) in [
        (None, ""),
        (Some("fabric"), "0.16.0"),
        (Some("forge"), "1"),
        (Some("neoforge"), "1"),
    ] {
        let plan = fixture_plan("fixture", provider, version, false, None);
        let expected = plan.resolved.launch_profile().unwrap();
        if provider == Some("fabric") {
            let library = expected["libraries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|l| l["name"] == "net.fabricmc:fabric-loader:0.16.0")
                .unwrap();
            assert!(library["downloads"]["artifact"]["sha1"].is_string());
            assert!(library["downloads"]["artifact"]["size"].is_u64());
        }
        let fixture = fixture();
        let root = root(fixture.path());
        plan.installer
            .install_resolved_request_bound(
                fs::File::open(&root).unwrap(),
                &plan.resolved,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        let actual: Value = serde_json::from_slice(
            &fs::read(root.join("versions/private-instance/private-instance.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(expected, actual, "{provider:?}");
    }
}
