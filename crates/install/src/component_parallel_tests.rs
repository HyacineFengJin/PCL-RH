use super::*;
use crate::components::ComponentStats;
use std::{io::Cursor, os::unix::fs::MetadataExt, sync::Barrier};

fn dependencies(server: &Server, payload: &[u8], count: usize) -> Vec<Download> {
    (0..count)
        .map(|index| Download {
            url: format!("{}/payload", server.base),
            hash: format!("{:x}", Sha1::digest(payload)),
            size: payload.len() as u64,
            relative: format!("libraries/fixture/component{index}.jar"),
        })
        .collect()
}

fn batch(
    installer: &Installer,
    workspace: &Path,
    cache: &Path,
    downloads: &[Download],
    cancel: &AtomicBool,
) -> Result<ComponentStats> {
    let mut stats = ComponentStats::new(0, 0, 0, 0, 0);
    stats.component_total = downloads.len() as u64;
    let previous = Mutex::new((0, 0, 0.0));
    installer.component_files(
        &InstallDir::legacy(workspace.into()),
        &InstallDir::legacy(cache.into()),
        downloads,
        cancel,
        &mut stats,
        &|p| {
            let mut previous = previous.lock().unwrap();
            assert_eq!(p.stage, "component_download");
            assert!(p.completed >= previous.0);
            assert!(p.network_bytes >= previous.1);
            let progress = p.steps[0].progress.unwrap();
            assert!(progress >= previous.2);
            *previous = (p.completed, p.network_bytes, progress);
        },
    )?;
    Ok(stats)
}

#[test]
fn component_download_batch_obeys_one_or_three_connections_and_exact_counters() {
    let payload = b"verified component dependency";
    for max in [1, 3] {
        let server = server(payload.to_vec());
        let installer = installer(&server, max, 0, false);
        let (workspace, cache) = (fixture(), fixture());
        let downloads = dependencies(&server, payload, 6);
        let stats = batch(
            &installer,
            workspace.path(),
            cache.path(),
            &downloads,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(server.maximum.load(Ordering::Relaxed), u64::from(max));
        assert_eq!(
            (
                stats.completed,
                stats.total,
                stats.component_completed,
                stats.downloaded,
                stats.reused
            ),
            (6, 6, 6, 6, 0)
        );
        assert_eq!(
            (stats.bytes, stats.byte_total, stats.network),
            (
                payload.len() as u64 * 6,
                payload.len() as u64 * 6,
                payload.len() as u64 * 6
            )
        );
        assert_eq!(installer.downloads.status().active_transfers, 0);
    }
}

#[test]
fn independent_component_batches_share_global_scheduler_slots() {
    let payload = b"shared slot component";
    let server = server(payload.to_vec());
    let installer = installer(&server, 3, 0, false);
    let (left, right, cache) = (fixture(), fixture(), fixture());
    let downloads = dependencies(&server, payload, 6);
    let start = Barrier::new(3);
    thread::scope(|scope| {
        for workspace in [left.path(), right.path()] {
            let (start, installer, downloads, cache) =
                (&start, &installer, &downloads, cache.path());
            scope.spawn(move || {
                start.wait();
                batch(
                    installer,
                    workspace,
                    cache,
                    downloads,
                    &AtomicBool::new(false),
                )
                .unwrap();
            });
        }
        start.wait();
    });
    assert_eq!(server.maximum.load(Ordering::Relaxed), 3);
    assert_eq!(server.artifact_requests.load(Ordering::Relaxed), 12);
    assert_eq!(installer.downloads.status().active_transfers, 0);
}

#[test]
fn component_cache_is_copied_to_writable_distinct_inodes_before_remote_work() {
    let payload = b"cached mutable processor input";
    for explicit_source in [false, true] {
        let server = server(payload.to_vec());
        let (workspace, cache, source) = (fixture(), fixture(), fixture());
        let download = dependencies(&server, payload, 1).remove(0);
        let cached_root = if explicit_source {
            source.path()
        } else {
            cache.path()
        };
        let cached = cached_root.join(&download.relative);
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&cached, payload).unwrap();
        let mut installer = installer(&server, 3, 0, false);
        if explicit_source {
            installer = installer.with_cache_source(source.path()).unwrap();
        }
        let stats = batch(
            &installer,
            workspace.path(),
            cache.path(),
            std::slice::from_ref(&download),
            &AtomicBool::new(false),
        )
        .unwrap();
        let target = workspace.path().join(&download.relative);
        assert_ne!(
            fs::metadata(&cached).unwrap().ino(),
            fs::metadata(&target).unwrap().ino()
        );
        fs::write(target, b"processor rewrote its private copy").unwrap();
        assert_eq!(fs::read(cached).unwrap(), payload);
        assert_eq!((stats.downloaded, stats.reused, stats.network), (0, 1, 0));
        assert_eq!(server.artifact_requests.load(Ordering::Relaxed), 0);
    }
}

fn contains_temporary(path: &Path) -> bool {
    fs::read_dir(path).unwrap().any(|entry| {
        let path = entry.unwrap().path();
        if path.is_dir() {
            contains_temporary(&path)
        } else {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".tmp")
        }
    })
}

#[test]
fn bad_component_hash_aborts_and_joins_workers_without_publishing_corrupt_file() {
    let payload = b"bad hash never publishes";
    let server = server(payload.to_vec());
    let installer = installer(&server, 3, 0, false);
    let (workspace, cache) = (fixture(), fixture());
    let mut downloads = dependencies(&server, payload, 9);
    downloads[0].hash = "0".repeat(40);
    let error = batch(
        &installer,
        workspace.path(),
        cache.path(),
        &downloads,
        &AtomicBool::new(false),
    )
    .err()
    .unwrap();
    assert!(error.contains("SHA1"), "{error}");
    assert!(!workspace.path().join(&downloads[0].relative).exists());
    assert!(!contains_temporary(workspace.path()));
    assert_eq!(installer.downloads.status().active_transfers, 0);
}

#[test]
fn cancelled_component_workers_release_throttle_and_clean_all_partial_files() {
    let payload = vec![9; 1024 * 1024];
    let server = server(payload.clone());
    let installer = installer(&server, 3, 1, false);
    let (workspace, cache) = (fixture(), fixture());
    let downloads = dependencies(&server, &payload, 6);
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(70));
            cancel.store(true, Ordering::Release);
        });
        let error = batch(
            &installer,
            workspace.path(),
            cache.path(),
            &downloads,
            &cancel,
        )
        .err()
        .unwrap();
        assert!(error.contains("取消"), "{error}");
    });
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!contains_temporary(workspace.path()));
    assert!(downloads
        .iter()
        .all(|download| !workspace.path().join(&download.relative).exists()));
    assert_eq!(installer.downloads.status().active_transfers, 0);
}

fn add_loader(server: &Server, provider: &str, payload: &[u8]) -> ComponentSelection {
    let libraries = (0..6).map(|index| {
        let path = format!("org/example/component{index}/1/component{index}-1.jar");
        serde_json::json!({"name":format!("org.example:component{index}:1"),"downloads":{"artifact":{
            "url":format!("{}/component{index}",server.base),"sha1":format!("{:x}",Sha1::digest(payload)),"size":payload.len(),"path":path
        }}})
    }).collect::<Vec<_>>();
    let identity = match provider {
        "fabric" => "net.fabricmc:fabric-loader:0.16.0",
        "forge" => "net.minecraftforge:forge:fixture-1",
        _ => "net.neoforged:neoforge:1",
    };
    let mut profile = serde_json::json!({"id":"loader-fixture","inheritsFrom":"fixture","mainClass":"fixture.Main","libraries":libraries});
    profile["libraries"][0]["name"] = identity.into();
    let mut routes = server.routes.lock().unwrap();
    for index in 0..6 {
        routes.insert(format!("/component{index}"), payload.to_vec());
    }
    let version = if provider == "fabric" { "0.16.0" } else { "1" };
    if provider == "fabric" {
        routes.insert(
            format!("/v2/versions/loader/fixture/{version}/profile/json"),
            serde_json::to_vec(&profile).unwrap(),
        );
    } else {
        let coordinate = format!("{identity}:installer");
        let relative = components::maven_path(&coordinate).unwrap();
        let path = format!(
            "/{}{}",
            if provider == "neoforge" {
                "releases/"
            } else {
                ""
            },
            relative
        );
        let profile_bytes = serde_json::to_vec(&profile).unwrap();
        let install = serde_json::to_vec(&serde_json::json!({"spec":1,"minecraft":"fixture","json":"/version.json","libraries":[],"processors":[]})).unwrap();
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("install_profile.json", &install),
            ("version.json", &profile_bytes),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        routes.insert(
            format!("{path}.sha1"),
            format!("{:x}", Sha1::digest(&bytes)).into_bytes(),
        );
        routes.insert(path, bytes);
    }
    ComponentSelection {
        provider: provider.into(),
        version: version.into(),
    }
}

#[test]
fn actual_fabric_forge_neoforge_install_batches_remote_dependencies() {
    let payload = b"officially described generic loader dependency";
    for provider in ["fabric", "forge", "neoforge"] {
        for max in [1, 3] {
            let server = server(payload.to_vec());
            let component = add_loader(&server, provider, payload);
            let installer =
                installer(&server, max, 0, false).with_java("/synthetic/never-probe-no-processors");
            let root = fixture();
            installer
                .install_request(
                    root.path(),
                    &InstallRequest {
                        minecraft: "fixture".into(),
                        name: "fixture".into(),
                        components: vec![component],
                    },
                    &AtomicBool::new(false),
                    |_| {},
                )
                .unwrap();
            assert_eq!(
                server.component_requests.load(Ordering::Relaxed),
                6,
                "{provider}"
            );
            assert_eq!(
                server.component_maximum.load(Ordering::Relaxed),
                u64::from(max),
                "{provider}"
            );
            for index in 0..6 {
                assert_eq!(
                    fs::read(root.path().join(format!(
                        "libraries/org/example/component{index}/1/component{index}-1.jar"
                    )))
                    .unwrap(),
                    payload
                );
            }
        }
    }
}
