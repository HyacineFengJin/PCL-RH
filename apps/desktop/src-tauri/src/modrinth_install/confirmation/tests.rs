//! Native planner/cache fixtures. Each local inventory belongs to this test;
//! provider facts are fake official records, never frontend plan JSON.
use super::*;
use crate::modrinth_install::tests::{dependency, run, sha, FakeProvider};
use std::{fs, path::PathBuf, sync::atomic::Ordering};
struct Fixture {
    _temporary: tempfile::TempDir,
    path: PathBuf,
    root: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let (temporary, path, root, project) = super::super::test_support::tree();
        fs::write(root.join("versions/sample/sample.json"), br#"{"id":"sample","clientVersion":"1.20.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]}"#).unwrap();
        Self {
            _temporary: temporary,
            path,
            root,
            project,
        }
    }
    fn target(&self) -> TargetSnapshot {
        target::capture(
            &self.root,
            &self.project,
            "root-fixture",
            "sample",
            &AtomicBool::new(false),
        )
        .unwrap()
    }
    fn local(&self, kind: &str, name: &str, bytes: &[u8]) {
        let path = self.root.join("versions/sample").join(kind).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn prepare(&self, p: &FakeProvider, q: &InstallRequest) -> Result<InstallPlan> {
        run(plan::prepare(
            p,
            self.target(),
            q.clone(),
            &AtomicBool::new(false),
        ))
    }
    fn claim(&self, p: &FakeProvider, q: InstallRequest) -> ConfirmedInstall {
        let cache = ConfirmationCache::default();
        let view = cache
            .remember(q.clone(), self.prepare(p, &q).unwrap())
            .unwrap();
        cache
            .claim(
                &self.root,
                &self.project,
                "root-fixture",
                "sample",
                &q,
                &view.revision,
            )
            .unwrap()
    }
    fn publish(&self, p: &mut FakeProvider, v: &str, kind: &str, name: &str, bytes: &[u8]) {
        self.local(kind, name, bytes);
        p.hashes.insert(sha(bytes), p.versions[v].clone());
    }
}

fn request(project: &str, version: &str) -> InstallRequest {
    InstallRequest {
        project_id: project.into(),
        version_id: version.into(),
        file_name: None,
    }
}
fn basic() -> FakeProvider {
    let mut p = FakeProvider::default();
    p.add("Proj0001", "Vers0001", "mod", "first.jar", b"first");
    p.add("Proj0002", "Vers0002", "mod", "second.jar", b"second");
    p
}
fn replan(c: &ConfirmedInstall, p: &FakeProvider) -> Result<InstallPlan> {
    let cancel = AtomicBool::new(false);
    let target = c.capture_target(&cancel)?;
    run(c.replan(p, target, &cancel))
}

#[test]
fn unrelated_preceding_mod_is_allowed_without_reinterpreting_second_intent() {
    let f = Fixture::new();
    let mut p = basic();
    let q = request("Proj0002", "Vers0002");
    let confirmed = f.claim(&p, q.clone());
    f.publish(&mut p, "Vers0001", "mods", "first.jar", b"first");
    let current = replan(&confirmed, &p).unwrap();
    assert_eq!(confirmed.request(), &q);
    assert_eq!(current.files.len(), 1);
    assert_eq!(current.files[0].version_id, "Vers0002");
    assert!(!current.files[0].reused);
    assert_eq!(current.installed.len(), 1);
    assert_eq!(current.download_bytes, 6);
}

#[test]
fn identical_shared_beta_dependency_becomes_enabled_alias_reuse() {
    let f = Fixture::new();
    let mut p = basic();
    p.add("Proj0003", "Vers0003", "mod", "shared.jar", b"shared")
        .version_type = "beta".into();
    for v in ["Vers0001", "Vers0002"] {
        p.versions.get_mut(v).unwrap().dependencies =
            vec![dependency(Some("Proj0003"), Some("Vers0003"), "required")];
    }
    let confirmed = f.claim(&p, request("Proj0002", "Vers0002"));
    assert!(confirmed
        .original
        .warnings
        .iter()
        .any(|w| w.contains("beta版本")));
    f.publish(&mut p, "Vers0001", "mods", "first.jar", b"first");
    f.publish(&mut p, "Vers0003", "mods", "custom-shared.jar", b"shared");
    let current = replan(&confirmed, &p).unwrap();
    let shared = current
        .files
        .iter()
        .find(|file| file.project_id == "Proj0003")
        .unwrap();
    assert!(shared.reused);
    assert_eq!(
        shared.existing_file_name.as_deref(),
        Some("custom-shared.jar")
    );
    assert_eq!(shared.file_name, "shared.jar");
    assert_eq!(current.download_bytes, 6);
    assert_eq!(current.warnings, confirmed.original.warnings);
}

#[test]
fn preceding_cancel_or_failure_leaves_original_authority_usable() {
    let f = Fixture::new();
    let p = basic();
    let confirmed = f.claim(&p, request("Proj0002", "Vers0002"));
    for _ in 0..2 {
        // No publication follows either terminal outcome.
        let current = replan(&confirmed, &p).unwrap();
        assert_eq!(current.revision, confirmed.original.revision);
        assert_eq!(current.download_bytes, confirmed.original.download_bytes);
    }
}

#[test]
fn new_conflicts_unknown_files_and_incompatible_consumers_require_reconfirmation() {
    for scenario in ["incompatible", "unknown", "loader", "game", "wrong-kind"] {
        let f = Fixture::new();
        let mut p = basic();
        let confirmed = f.claim(&p, request("Proj0002", "Vers0002"));
        match scenario {
            "incompatible" => {
                p.versions.get_mut("Vers0001").unwrap().dependencies =
                    vec![dependency(Some("Proj0002"), None, "incompatible")]
            }
            "loader" => p.versions.get_mut("Vers0001").unwrap().loaders = vec!["forge".into()],
            "game" => p.versions.get_mut("Vers0001").unwrap().game_versions = vec!["1.21".into()],
            _ => {}
        }
        if scenario == "unknown" {
            f.local("mods", "unknown.jar", b"unknown");
        } else if scenario == "wrong-kind" {
            f.publish(&mut p, "Vers0001", "resourcepacks", "first.zip", b"first");
        } else {
            f.publish(&mut p, "Vers0001", "mods", "first.jar", b"first");
        }
        assert!(replan(&confirmed, &p).is_err(), "{scenario}");
    }
}

#[test]
fn original_inventory_delete_disable_or_same_hash_inode_replacement_is_rejected() {
    for scenario in ["delete", "disable", "replace"] {
        let f = Fixture::new();
        let mut p = basic();
        f.publish(&mut p, "Vers0001", "mods", "first.jar", b"first");
        let confirmed = f.claim(&p, request("Proj0002", "Vers0002"));
        let path = f.root.join("versions/sample/mods/first.jar");
        match scenario {
            "delete" => fs::remove_file(&path).unwrap(),
            "disable" => fs::rename(&path, path.with_file_name("first.jar.disabled")).unwrap(),
            _ => {
                let next = path.with_file_name("replacement");
                fs::write(&next, b"first").unwrap();
                fs::rename(next, path).unwrap();
            }
        }
        // This is the same target gate the production worker calls before
        // constructing its HTTP provider, so metadata is never submitted.
        assert!(
            confirmed.capture_target(&AtomicBool::new(false)).is_err(),
            "{scenario}"
        );
    }
}

#[test]
fn profile_root_and_project_physical_changes_fail_before_provider_work() {
    for scenario in ["profile", "root", "project"] {
        let f = Fixture::new();
        let p = basic();
        let confirmed = f.claim(&p, request("Proj0002", "Vers0002"));
        match scenario {
            "profile"=>fs::write(f.root.join("versions/sample/sample.json"),br#"{"id":"sample","clientVersion":"1.20.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.1"}]}"#).unwrap(),
            "root"=>{let old=f.path.join("old-game");fs::rename(&f.root,&old).unwrap();fs::create_dir_all(f.root.join("versions/sample/mods")).unwrap();fs::copy(old.join("versions/sample/sample.json"),f.root.join("versions/sample/sample.json")).unwrap();},
            _=>{fs::rename(&f.project,f.path.join("old-project")).unwrap();fs::create_dir_all(f.project.join(".pcl-rust")).unwrap();},
        }
        assert!(
            confirmed.capture_target(&AtomicBool::new(false)).is_err(),
            "{scenario}"
        );
    }
}

#[test]
fn source_graph_and_original_identification_changes_do_not_reuse_consent() {
    for scenario in ["url", "optional", "embedded", "installed-source"] {
        let f = Fixture::new();
        let mut p = basic();
        if scenario == "installed-source" {
            f.publish(&mut p, "Vers0001", "mods", "first.jar", b"first");
        }
        let confirmed = f.claim(&p, request("Proj0002", "Vers0002"));
        match scenario {
            "url" => p.versions.get_mut("Vers0002").unwrap().files[0]
                .url
                .push_str("?changed=1"),
            "optional" => p
                .versions
                .get_mut("Vers0002")
                .unwrap()
                .dependencies
                .push(dependency(Some("Proj0001"), None, "optional")),
            "embedded" => p
                .versions
                .get_mut("Vers0002")
                .unwrap()
                .dependencies
                .push(dependency(Some("Proj0001"), None, "embedded")),
            _ => {
                let v = p.hashes.get_mut(&sha(b"first")).unwrap();
                v.version_number = "2.0.0".into();
            }
        }
        assert!(replan(&confirmed, &p).is_err(), "{scenario}");
    }
}

#[test]
fn reverse_exact_version_pins_block_both_added_and_initial_consumers() {
    for baseline in [false, true] {
        let f = Fixture::new();
        let mut p = basic();
        p.add("Proj0002", "Vers0003", "mod", "newer.jar", b"newer");
        p.versions.get_mut("Vers0001").unwrap().dependencies =
            vec![dependency(None, Some("Vers0003"), "required")];
        let confirmed = (!baseline).then(|| f.claim(&p, request("Proj0002", "Vers0002")));
        f.publish(&mut p, "Vers0001", "mods", "first.jar", b"first");
        assert!(f
            .prepare(&p, &request("Proj0002", "Vers0002"))
            .unwrap_err()
            .contains("精确前置版本"));
        if let Some(c) = confirmed {
            assert!(replan(&c, &p).is_err());
        }
    }
}

fn companion(p: &mut FakeProvider, file_type: &str, bytes: &[u8]) {
    let mut file = p.versions["Vers0002"].files[0].clone();
    file.filename = "assets.zip".into();
    file.primary = false;
    file.file_type = Some(file_type.into());
    file.hashes.sha512 = sha(bytes);
    file.size = bytes.len() as u64;
    file.url = "https://cdn.modrinth.com/data/Proj0002/versions/Vers0002/assets.zip".into();
    p.versions.get_mut("Vers0002").unwrap().files.push(file);
}

#[test]
fn reverse_file_roles_do_not_accept_identical_bytes_in_another_kind() {
    for local_kind in [None, Some("shaderpacks"), Some("resourcepacks")] {
        let f = Fixture::new();
        let mut p = basic();
        companion(&mut p, "optional-resource-pack", b"second");
        let mut pin = dependency(Some("Proj0002"), Some("Vers0002"), "required");
        pin.file_name = Some("assets.zip".into());
        p.versions.get_mut("Vers0001").unwrap().dependencies = vec![pin];
        f.publish(&mut p, "Vers0001", "mods", "first.jar", b"first");
        if let Some(kind) = local_kind {
            f.publish(&mut p, "Vers0002", kind, "custom-assets.zip", b"second");
        }
        let result = f.prepare(&p, &request("Proj0002", "Vers0002"));
        if local_kind == Some("resourcepacks") {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn project_only_required_pin_needs_runtime_file_even_when_companion_selected() {
    let f = Fixture::new();
    let mut p = basic();
    companion(&mut p, "optional-resource-pack", b"assets");
    p.versions.get_mut("Vers0001").unwrap().dependencies =
        vec![dependency(Some("Proj0002"), Some("Vers0002"), "required")];
    f.publish(&mut p, "Vers0001", "mods", "first.jar", b"first");
    let mut q = request("Proj0002", "Vers0002");
    q.file_name = Some("assets.zip".into());
    assert!(f.prepare(&p, &q).unwrap_err().contains("前置文件"));
    f.publish(&mut p, "Vers0002", "mods", "custom-runtime.jar", b"second");
    assert!(f.prepare(&p, &q).is_ok());
}

#[test]
fn disabled_shared_dependency_and_same_project_other_version_are_not_new_authority() {
    for scenario in ["disabled", "version"] {
        let f = Fixture::new();
        let mut p = basic();
        p.add("Proj0003", "Vers0003", "mod", "shared.jar", b"shared");
        p.add("Proj0003", "Vers0004", "mod", "other.jar", b"other");
        p.versions.get_mut("Vers0002").unwrap().dependencies =
            vec![dependency(Some("Proj0003"), Some("Vers0003"), "required")];
        let c = f.claim(&p, request("Proj0002", "Vers0002"));
        if scenario == "disabled" {
            f.publish(&mut p, "Vers0003", "mods", "shared.jar.disabled", b"shared");
        } else {
            f.publish(&mut p, "Vers0004", "mods", "other.jar", b"other");
        }
        assert!(replan(&c, &p).is_err(), "{scenario}");
    }
}

#[test]
fn unclaimed_expiry_eviction_and_one_use_binding_are_explicit() {
    let f = Fixture::new();
    let p = basic();
    let q = request("Proj0002", "Vers0002");
    let cache = ConfirmationCache::default();
    let plan = f.prepare(&p, &q).unwrap();
    let view = cache.remember(q.clone(), plan.clone()).unwrap();
    assert!(cache
        .claim(
            &f.root,
            &f.project,
            "other-root",
            "sample",
            &q,
            &view.revision
        )
        .is_err());
    assert!(cache
        .claim(
            &f.root,
            &f.project,
            "root-fixture",
            "sample",
            &request("Proj0001", "Vers0001"),
            &view.revision
        )
        .is_err());
    assert!(cache
        .claim(&f.root, &f.project, "root-fixture", "sample", &q, "forged")
        .is_err());
    let claimed = cache
        .claim(
            &f.root,
            &f.project,
            "root-fixture",
            "sample",
            &q,
            &view.revision,
        )
        .unwrap();
    assert!(cache
        .claim(
            &f.root,
            &f.project,
            "root-fixture",
            "sample",
            &q,
            &view.revision
        )
        .is_err());
    let expires = cache.remember(q.clone(), plan.clone()).unwrap();
    cache.0.lock().unwrap()[0].expires = Instant::now() - Duration::from_secs(1);
    assert!(cache
        .claim(
            &f.root,
            &f.project,
            "root-fixture",
            "sample",
            &q,
            &expires.revision
        )
        .is_err());
    let evicted = cache.remember(q.clone(), plan.clone()).unwrap();
    for _ in 0..MAX_ENTRIES {
        cache.remember(q.clone(), plan.clone()).unwrap();
    }
    assert_eq!(cache.0.lock().unwrap().len(), MAX_ENTRIES);
    assert!(cache
        .claim(
            &f.root,
            &f.project,
            "root-fixture",
            "sample",
            &q,
            &evicted.revision
        )
        .is_err());
    drop(cache);
    assert!(replan(&claimed, &p).is_ok()); // Claimed queue job has no cache TTL.
}

fn pack(count: usize, game_versions: usize) -> (Fixture, FakeProvider, InstallRequest) {
    let f = Fixture::new();
    let mut p = basic();
    for i in 0..count {
        let project = format!("P{i:07}");
        let version = format!("V{i:07}");
        let bytes = format!("installed-{i}");
        let file = format!("local-{i}.jar");
        p.add(&project, &version, "mod", &file, bytes.as_bytes())
            .game_versions = vec!["1.20.1".into(); game_versions];
        f.publish(&mut p, &version, "mods", &file, bytes.as_bytes());
    }
    (f, p, request("Proj0002", "Vers0002"))
}

#[test]
fn native_short_string_and_private_capacity_budget_accepts_large_pack_but_bounds_cache() {
    let (f, p, q) = pack(200, 30);
    let plan = f.prepare(&p, &q).unwrap();
    assert_eq!(plan.installed.len(), 200);
    assert!(footprint(&q, &plan).unwrap() < MAX_CONFIRMATION_BYTES);
    ConfirmationCache::default()
        .remember(q.clone(), plan)
        .unwrap();
    let (large, p, q) = pack(200, 1000);
    let plan = large.prepare(&p, &q).unwrap();
    // The public view omits installed metadata, so it cannot bound native heap.
    assert!(serde_json::to_vec(&plan).unwrap().len() < 64 * 1024);
    assert!(footprint(&q, &plan).is_err());
    let mut plan = large.prepare(&p, &q).unwrap();
    plan.installed.truncate(1);
    plan.installed[0].version.files[0]
        .url
        .reserve(MAX_CONFIRMATION_BYTES);
    assert!(footprint(&q, &plan).is_err()); // Omitted URL capacity is counted too.
}

#[test]
fn total_native_byte_eviction_applies_before_count_limit() {
    let (f, p, q) = pack(200, 300);
    let plan = f.prepare(&p, &q).unwrap();
    let bytes = footprint(&q, &plan).unwrap();
    assert!(bytes > MAX_CACHE_BYTES / MAX_ENTRIES);
    let cache = ConfirmationCache::default();
    let first = cache.remember(q.clone(), plan.clone()).unwrap();
    for _ in 0..MAX_ENTRIES {
        cache.remember(q.clone(), plan.clone()).unwrap();
    }
    let entries = cache.0.lock().unwrap();
    assert!(entries.len() < MAX_ENTRIES);
    assert!(entries.iter().map(|r| r.bytes).sum::<usize>() + CACHE_STORAGE <= MAX_CACHE_BYTES);
    drop(entries);
    assert!(cache
        .claim(
            &f.root,
            &f.project,
            "root-fixture",
            "sample",
            &q,
            &first.revision
        )
        .is_err());
}

fn archive(id: &str) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(format!(r#"{{"schemaVersion":1,"id":"{id}","version":"1.0.0"}}"#).as_bytes())
        .unwrap();
    writer.finish().unwrap().into_inner()
}

fn stage(
    plan: &InstallPlan,
    bodies: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Vec<crate::resource_ops::VerifiedImport> {
    let cancel = AtomicBool::new(false);
    let mut http = provider::HttpProvider::new(&cancel)
        .unwrap()
        .with_download_policy(
            pcl_network::DownloadScheduler::new(pcl_network::DownloadPolicy::default()).unwrap(),
        );
    http.client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let stage = target::Dir::open(&plan.target.project)
        .unwrap()
        .ensure(".pcl-rust")
        .unwrap()
        .ensure("resource-downloads")
        .unwrap()
        .with_input_sources()
        .unwrap();
    let mut inputs = Vec::new();
    for item in plan.files.iter().filter(|f| !f.reused) {
        let body = &bodies[&item.file_name];
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend(body);
        let (url, worker) = crate::modrinth_install::tests::server(&response, Duration::ZERO);
        let mut file = stage.allocate(item.size).unwrap();
        run(transfer::response_into_file(
            &http,
            http.client.get(url),
            item.size,
            &item.sha512,
            &mut file,
            &cancel,
            |_| {},
        ))
        .unwrap();
        worker.join().unwrap();
        super::super::test_support::assert_source(&file);
        inputs.push(crate::resource_ops::VerifiedImport {
            kind: item.kind.clone(),
            file_name: item.file_name.clone(),
            file,
            size: item.size,
            sha512: item.sha512.clone(),
        });
    }
    assert_eq!(
        http.network_bytes.load(Ordering::Relaxed),
        plan.download_bytes
    );
    super::super::test_support::assert_staging(&stage, inputs.len());
    inputs
}
fn publish(plan: &InstallPlan, mut inputs: Vec<crate::resource_ops::VerifiedImport>) {
    let cancel = AtomicBool::new(false);
    let result = crate::resource_ops::import_verified_batch(
        &plan.target.root,
        &plan.instance_id,
        &mut inputs,
        &cancel,
        &mut || super::super::recheck_target(plan, &cancel),
        &mut |_, _| {},
    )
    .unwrap();
    crate::resource_ops::retire_sources(&mut inputs).unwrap();
    assert_eq!(
        result.changed as usize,
        plan.files.iter().filter(|f| !f.reused).count()
    );
}

#[test]
fn same_instance_claimed_workers_replan_then_stream_and_publish_without_new_authority() {
    use crate::tasks::{TaskKind, TaskOutcome, TaskScope, TaskTarget, Tasks};
    use std::sync::{mpsc, Arc};
    for scenario in ["independent", "shared", "cancel", "fail"] {
        let f = Fixture::new();
        fs::remove_dir(f.root.join("versions/sample/mods")).unwrap();
        // Keep an explicit local-isolation marker while the destination is
        // absent; otherwise core correctly selects the root-wide mods folder.
        fs::write(
            f.root.join("versions/sample/options.txt"),
            b"fixture isolation",
        )
        .unwrap();
        let mut p = FakeProvider::default();
        let bodies = std::collections::BTreeMap::from([
            ("first.jar".into(), archive("first")),
            ("second.jar".into(), archive("second")),
            ("shared.jar".into(), archive("shared")),
        ]);
        p.add(
            "Proj0001",
            "Vers0001",
            "mod",
            "first.jar",
            &bodies["first.jar"],
        );
        p.add(
            "Proj0002",
            "Vers0002",
            "mod",
            "second.jar",
            &bodies["second.jar"],
        );
        p.add(
            "Proj0003",
            "Vers0003",
            "mod",
            "shared.jar",
            &bodies["shared.jar"],
        );
        if scenario != "independent" {
            for v in ["Vers0001", "Vers0002"] {
                p.versions.get_mut(v).unwrap().dependencies =
                    vec![dependency(Some("Proj0003"), Some("Vers0003"), "required")];
            }
        }
        let a = f.claim(&p, request("Proj0001", "Vers0001"));
        let b = f.claim(&p, request("Proj0002", "Vers0002"));
        let provider = Arc::new(Mutex::new(p));
        let tasks = Arc::new(Tasks::new());
        let task_target = TaskTarget {
            root_id: "root-fixture".into(),
            root_path: f.root.display().to_string(),
            instance_id: Some("sample".into()),
        };
        let first = tasks
            .admit_queued(
                task_target.clone(),
                TaskKind::ResourceDownload,
                TaskScope::root(&f.root).unwrap(),
            )
            .unwrap();
        first.wait_turn().unwrap();
        let second = tasks
            .admit_queued(
                task_target,
                TaskKind::ResourceDownload,
                TaskScope::root(&f.root).unwrap(),
            )
            .unwrap();
        let id = second.id().to_owned();
        let (begun, received) = mpsc::channel();
        let remote = provider.clone();
        let bodies_b = bodies.clone();
        let worker = std::thread::spawn(move || {
            second.wait_turn().unwrap();
            let plan = replan(&b, &remote.lock().unwrap()).unwrap();
            let has_shared_reuse = plan
                .files
                .iter()
                .any(|f| f.project_id == "Proj0003" && f.reused);
            let download_count = plan.files.iter().filter(|f| !f.reused).count();
            begun.send((has_shared_reuse, download_count)).unwrap();
            let inputs = stage(&plan, &bodies_b);
            publish(&plan, inputs);
            second.finish(TaskOutcome::Complete {
                result: None,
                message: "fixture published".into(),
                error: None,
            });
        });
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            tasks.snapshot(&id).unwrap().stage,
            crate::tasks::TaskStage::Queued
        );
        let first_plan = replan(&a, &provider.lock().unwrap()).unwrap();
        let inputs = stage(&first_plan, &bodies);
        if scenario == "cancel" || scenario == "fail" {
            drop(inputs); // Cleanup precedes terminal admission release.
            assert!(!f.root.join("versions/sample/mods").exists());
            first.finish(if scenario == "cancel" {
                TaskOutcome::Failed(crate::tasks::CANCELLED.into())
            } else {
                TaskOutcome::Error("fixture transfer failed before publication".into())
            });
        } else {
            publish(&first_plan, inputs);
            let mut p = provider.lock().unwrap();
            for item in &first_plan.files {
                let version = p.versions[&item.version_id].clone();
                p.hashes.insert(item.sha512.clone(), version);
            }
            first.finish(TaskOutcome::Complete {
                result: None,
                message: "fixture published".into(),
                error: None,
            });
        }
        let (reused, count) = received.recv_timeout(Duration::from_secs(4)).unwrap();
        assert_eq!(reused, scenario == "shared");
        assert_eq!(
            count,
            if scenario == "cancel" || scenario == "fail" {
                2
            } else {
                1
            }
        );
        worker.join().unwrap();
        assert!(tasks.active_all().is_empty());
        assert_eq!(
            fs::read(f.root.join("versions/sample/mods/second.jar")).unwrap(),
            bodies["second.jar"]
        );
        assert_eq!(
            f.root.join("versions/sample/mods/first.jar").exists(),
            scenario == "independent" || scenario == "shared"
        );
        if scenario != "independent" {
            assert_eq!(
                fs::read(f.root.join("versions/sample/mods/shared.jar")).unwrap(),
                bodies["shared.jar"]
            );
        }
        super::super::test_support::assert_staging_path(&f.project, 0);
    }
}
