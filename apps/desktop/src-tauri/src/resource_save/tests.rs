//! Fake metadata and loopback payloads exercise final-path transactions. Every
//! fixture lives under project work; no singleton, actual cache or game is used.
use super::*;
use crate::modrinth_install::provider::{
    ApiFile, FutureResult, Hashes, Project, Provider, Version,
};
use sha2::{Digest, Sha512};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);

fn curseforge_save(
    body: &[u8],
    supplied: &[u8],
    change_final: bool,
) -> (Result<SaveResult>, Fixture) {
    let fixture = Fixture::new();
    let target = files::SaveTarget::capture(&fixture.project, &fixture.target()).unwrap();
    let fake = Fake::new(body);
    let mut plan = fake.plan();
    plan.request.source = SaveSource::CurseForge;
    plan.request.project_id = "123".into();
    plan.request.version_id = "456".into();
    plan.sha512.clear();
    plan.sha1 = Some(format!("{:x}", sha1::Sha1::digest(body)));
    let cancel = AtomicBool::new(false);
    let provider = transport(&cancel, 0);
    let metadata = AtomicU64::new(0);
    let calls = AtomicUsize::new(0);
    let (url, worker) = server(supplied, Duration::ZERO);
    let resolve = |request: SaveRequest| {
        let mut next = plan.clone();
        assert_eq!(request, plan.request);
        if calls.fetch_add(1, Ordering::SeqCst) > 0 && change_final {
            next.revision.push('0');
        }
        let file = fake.version.files[0].clone();
        Box::pin(async move { Ok(authority::Authority { plan: next, file }) })
            as FutureAuthority<'_>
    };
    let request_file = |_: &ApiFile| Ok(provider.client.get(&url));
    let session = SaveSession {
        http: &provider,
        resolve: &resolve,
        request_file: &request_file,
        metadata_bytes: &metadata,
    };
    let result = run(save_with(
        &session,
        &target,
        plan.request.clone(),
        &plan.revision,
        &cancel,
        |_| {},
        &mut || Ok(()),
    ));
    worker.join().unwrap();
    fixture.assert_untouched_project();
    (result, fixture)
}

#[test]
fn curseforge_sha1_save_publishes_verified_bytes_and_returns_computed_sha512() {
    let body = b"synthetic CF payload";
    let (result, fixture) = curseforge_save(body, body, false);
    let result = result.unwrap();
    assert_eq!(fs::read(fixture.target()).unwrap(), body);
    assert_eq!(result.sha512, sha(body));
}

#[test]
fn curseforge_same_size_wrong_sha1_leaves_no_partial_file() {
    let (result, fixture) = curseforge_save(b"abc", b"abd", false);
    assert!(result.unwrap_err().contains("校验失败"));
    assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 0);
}

#[test]
fn curseforge_changed_final_authority_discards_verified_staging() {
    let (result, fixture) = curseforge_save(b"abc", b"abc", true);
    assert!(result.unwrap_err().contains("官方文件信息已变化"));
    assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 0);
}

#[test]
fn provider_and_digest_namespaces_cannot_be_interchanged() {
    let fake = Fake::new(b"abc");
    let mut plan = fake.plan();
    plan.request.source = SaveSource::CurseForge;
    assert!(authority::expected_digest(&plan).is_err());
    assert!(run(authority::resolve(&fake, plan.request.clone())).is_err());
    plan.request.project_id = "123".into();
    plan.request.version_id = "456".into();
    plan.sha512.clear();
    plan.sha1 = Some(format!("{:x}", sha1::Sha1::digest(b"abc")));
    assert!(authority::expected_digest(&plan).is_ok());
    plan.request.source = SaveSource::Modrinth;
    assert!(authority::expected_digest(&plan).is_err());
}
fn run<F: std::future::Future>(future: F) -> F::Output {
    tauri::async_runtime::block_on(future)
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha512::digest(bytes))
}
struct Fixture {
    path: PathBuf,
    project: PathBuf,
    folder: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/launcher-options-2026-10-05/resource-save-fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(path.join("project")).unwrap();
        fs::create_dir_all(path.join("chosen")).unwrap();
        let path = path.canonicalize().unwrap();
        Self {
            project: path.join("project"),
            folder: path.join("chosen"),
            path,
        }
    }
    fn target(&self) -> PathBuf {
        self.folder.join("chosen-name.jar")
    }
    fn assert_untouched_project(&self) {
        assert_eq!(fs::read_dir(&self.project).unwrap().count(), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
struct Fake {
    project: Project,
    version: Version,
    calls: AtomicUsize,
    change_on_final: AtomicBool,
}
impl Fake {
    fn new(body: &[u8]) -> Self {
        Self {
            project: Project {
                id: "Proj0001".into(),
                title: "Sample Project".into(),
                project_type: "mod".into(),
            },
            version: Version {
                id: "Vers0001".into(),
                project_id: "Proj0001".into(),
                name: "Sample Version".into(),
                version_number: "1.0".into(),
                date_published: "2026-01-01T00:00:00Z".into(),
                version_type: "release".into(),
                game_versions: vec![],
                loaders: vec![],
                dependencies: vec![],
                files: vec![ApiFile {
                    filename: "sample.jar".into(),
                    size: body.len() as u64,
                    url: "https://cdn.modrinth.com/data/Proj0001/versions/Vers0001/sample.jar"
                        .into(),
                    primary: true,
                    hashes: Hashes {
                        sha512: sha(body),
                        sha1: None,
                    },
                    file_type: None,
                }],
            },
            calls: AtomicUsize::new(0),
            change_on_final: AtomicBool::new(false),
        }
    }
    fn request(&self) -> SaveRequest {
        SaveRequest {
            source: crate::resource_save::SaveSource::Modrinth,
            project_id: self.project.id.clone(),
            version_id: self.version.id.clone(),
            file_name: self.version.files[0].filename.clone(),
        }
    }
    fn plan(&self) -> SavePlan {
        run(authority::resolve(self, self.request())).unwrap().plan
    }
}
impl Provider for Fake {
    fn project<'a>(&'a self, _: &'a str) -> FutureResult<'a, Project> {
        Box::pin(async move { Ok(self.project.clone()) })
    }
    fn version<'a>(&'a self, _: &'a str) -> FutureResult<'a, Version> {
        Box::pin(async move {
            let mut version = self.version.clone();
            let calls = self.calls.fetch_add(1, Ordering::Relaxed);
            if self.change_on_final.load(Ordering::Relaxed) && calls >= 2 {
                version.files[0].hashes.sha512 = sha(b"changed");
            }
            Ok(version)
        })
    }
    fn versions<'a>(
        &'a self,
        _: &'a str,
        _: &'a crate::modrinth_install::Compatibility,
    ) -> FutureResult<'a, Vec<Version>> {
        Box::pin(async { unreachable!("save has no compatibility/installer graph") })
    }
    fn from_hashes<'a>(&'a self, _: &'a [String]) -> FutureResult<'a, BTreeMap<String, Version>> {
        Box::pin(async { unreachable!("save does not inspect local files") })
    }
}
fn server(body: &[u8], delay: Duration) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let body = body.to_vec();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 2048];
        let _ = socket.read(&mut request);
        thread::sleep(delay);
        let _ = write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(&body);
    });
    (format!("http://{address}/fixture"), worker)
}
fn transport<'a>(cancel: &'a AtomicBool, rate: u32) -> HttpProvider<'a> {
    let scheduler = DownloadScheduler::new(pcl_network::DownloadPolicy {
        total_rate_limit_mib_per_second: rate,
        ..Default::default()
    })
    .unwrap();
    let mut provider = HttpProvider::new(cancel)
        .unwrap()
        .with_download_policy(scheduler);
    provider.client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    provider
}
fn exercise(
    fixture: &Fixture,
    fake: &Fake,
    body: &[u8],
    delay: Duration,
    cancel: &AtomicBool,
    rate: u32,
    report: impl Fn(SaveProgress),
    gate: CommitGate<'_>,
) -> Result<SaveResult> {
    let target = files::SaveTarget::capture(&fixture.project, &fixture.target())?;
    exercise_target(&target, fake, body, delay, cancel, rate, report, gate)
}
fn exercise_target(
    target: &CapturedSaveTarget,
    fake: &Fake,
    body: &[u8],
    delay: Duration,
    cancel: &AtomicBool,
    rate: u32,
    report: impl Fn(SaveProgress),
    gate: CommitGate<'_>,
) -> Result<SaveResult> {
    let plan = fake.plan();
    let provider = transport(cancel, rate);
    let (url, worker) = server(body, delay);
    let metadata = AtomicU64::new(0);
    let resolve = |request| Box::pin(authority::resolve(fake, request)) as FutureAuthority<'_>;
    let request_file = |_: &ApiFile| Ok(provider.client.get(&url));
    let session = SaveSession {
        http: &provider,
        resolve: &resolve,
        request_file: &request_file,
        metadata_bytes: &metadata,
    };
    let result = run(save_with(
        &session,
        target,
        plan.request,
        &plan.revision,
        cancel,
        report,
        gate,
    ));
    worker.join().unwrap();
    result
}

#[test]
fn queued_save_worker_defers_metadata_and_real_http_until_turn_then_publishes_held_target() {
    use crate::tasks::{TaskKind, TaskOutcome, TaskScope, TaskStage, TaskTarget, Tasks};
    let fixture = Fixture::new();
    let manager = Arc::new(Tasks::new());
    let target =
        Arc::new(CapturedSaveTarget::capture(&fixture.project, &fixture.target()).unwrap());
    let task_target = TaskTarget {
        root_id: "launcher".into(),
        root_path: fixture.folder.display().to_string(),
        instance_id: None,
    };
    let blocker = manager
        .admit_queued(
            task_target.clone(),
            TaskKind::Install,
            TaskScope::root(&fixture.folder).unwrap(),
        )
        .unwrap();
    blocker.wait_turn().unwrap();
    let task = manager
        .admit_queued(
            task_target,
            TaskKind::ResourceSave,
            TaskScope::files(&[target.path().to_path_buf()]).unwrap(),
        )
        .unwrap();
    let id = task.id().to_owned();
    let fake = Arc::new(Fake::new(b"fixture payload"));
    let provider = fake.clone();
    let (send, receive) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        send.send(()).unwrap();
        crate::resource_save_commands::wait_target(&task, &target).unwrap();
        let cancel = task.cancellation_token();
        let mut gate = || {
            task.begin_finishing();
            Ok(())
        };
        let result = exercise_target(
            &target,
            &provider,
            b"fixture payload",
            Duration::ZERO,
            &cancel,
            0,
            |_| {},
            &mut gate,
        )
        .unwrap();
        drop(target);
        task.finish(TaskOutcome::Complete {
            result: serde_json::to_value(&result).ok(),
            message: "fixture saved".into(),
            error: result.warning,
        });
    });
    receive.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(manager.snapshot(&id).unwrap().stage, TaskStage::Queued);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 0);
    blocker.finish(TaskOutcome::Complete {
        result: None,
        message: "fixture finished".into(),
        error: None,
    });
    worker.join().unwrap();
    assert_eq!(fs::read(fixture.target()).unwrap(), b"fixture payload");
    assert_eq!(
        manager.wait_terminal(&id).unwrap().result.unwrap()["network_bytes"],
        b"fixture payload".len()
    );
    fixture.assert_untouched_project();
}
#[test]
fn exact_official_selection_and_strict_payload_reject_forged_identity_or_urls() {
    let mut fake = Fake::new(b"good");
    let request = fake.request();
    assert!(serde_json::from_value::<SaveRequest>(serde_json::json!({"project_id":"Proj0001","version_id":"Vers0001","file_name":"sample.jar","url":"https://evil.example"})).is_err());
    fake.version.project_id = "Other001".into();
    assert!(run(authority::resolve(&fake, request.clone()))
        .unwrap_err()
        .contains("身份"));
    fake.version.project_id = request.project_id.clone();
    fake.version.files[0].url = "https://evil.example/sample.jar".into();
    assert!(run(authority::resolve(&fake, request.clone())).is_err());
    fake.version.files[0].url = "https://cdn.modrinth.com/data/sample.jar".into();
    fake.version.files[0].primary = false;
    assert!(run(authority::resolve(&fake, request)).is_ok());
    let mut request = fake.request();
    request.file_name = "other.jar".into();
    assert!(run(authority::resolve(&fake, request)).is_err());
    fake.version.files[0].filename = format!("sample.{}", "a".repeat(65));
    assert!(run(authority::resolve(&fake, fake.request()))
        .unwrap_err()
        .contains("扩展名超过64字节"));
}
#[test]
fn suggested_name_keeps_actual_extension_utf8_bounds_and_safe_readable_stem() {
    let fake = Fake::new(b"good");
    let mut plan = fake.plan();
    assert_eq!(suggested_filename(&plan, false), "sample.jar");
    plan.project_title = "../Example/:*?\\<>|\u{0000} Name".repeat(50);
    plan.version_name = "测试版本".repeat(50);
    let suggested = suggested_filename(&plan, true);
    assert!(suggested.len() <= 240);
    assert!(suggested.ends_with(".jar"));
    assert!(!suggested.starts_with('.'));
    crate::modrinth_install::provider::file_name(&suggested).unwrap();
    assert!(!suggested
        .chars()
        .any(|ch| ch.is_control()
            || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')));
}
#[test]
fn real_local_stream_paces_and_publishes_only_chosen_filename_after_gate() {
    let fixture = Fixture::new();
    let body = vec![7; 1024 * 1024];
    let fake = Fake::new(&body);
    let cancel = AtomicBool::new(false);
    let gate_calls = AtomicUsize::new(0);
    let progress = Mutex::new(Vec::new());
    let mut gate = || {
        gate_calls.fetch_add(1, Ordering::Relaxed);
        assert!(!fixture.target().exists());
        Ok(())
    };
    let start = Instant::now();
    let result = exercise(
        &fixture,
        &fake,
        &body,
        Duration::ZERO,
        &cancel,
        1,
        |p| progress.lock().unwrap().push(p),
        &mut gate,
    )
    .unwrap();
    assert!(start.elapsed() >= Duration::from_millis(995));
    assert_eq!(gate_calls.load(Ordering::Relaxed), 1);
    assert_eq!(fs::read(fixture.target()).unwrap(), body);
    assert_eq!(result.sha512, sha(&body));
    assert_eq!(result.network_bytes, body.len() as u64);
    assert!(result.warning.is_none());
    assert_eq!(
        fs::metadata(fixture.target()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 1);
    fixture.assert_untouched_project();
    let rows = progress.lock().unwrap();
    assert_eq!(rows.last().unwrap().phase, "complete");
    assert!(rows
        .windows(2)
        .all(|pair| pair[0].bytes_done <= pair[1].bytes_done
            && pair[0].network_bytes <= pair[1].network_bytes));
    assert_eq!(fake.calls.load(Ordering::Relaxed), 3);
}
#[test]
fn bad_hash_or_changed_final_authority_drops_all_anonymous_content() {
    for changed in [false, true] {
        let fixture = Fixture::new();
        let fake = Fake::new(b"good");
        fake.change_on_final.store(changed, Ordering::Relaxed);
        let cancel = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let mut gate = || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        };
        let error = exercise(
            &fixture,
            &fake,
            if changed { b"good" } else { b"bad!" },
            Duration::ZERO,
            &cancel,
            0,
            |_| {},
            &mut gate,
        )
        .unwrap_err();
        assert!(error.contains(if changed { "信息已变化" } else { "SHA512" }));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 0);
        fixture.assert_untouched_project();
    }
}
#[test]
fn cancellation_during_body_pacing_or_at_commit_gate_keeps_directory_empty() {
    for during_gate in [false, true] {
        let fixture = Fixture::new();
        let body = vec![7; 1024 * 1024];
        let fake = Fake::new(&body);
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let trigger = if !during_gate {
            Some(thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                signal.store(true, Ordering::Release)
            }))
        } else {
            None
        };
        let mut gate = || {
            cancel.store(true, Ordering::Release);
            Ok(())
        };
        let error = exercise(
            &fixture,
            &fake,
            &body,
            Duration::ZERO,
            &cancel,
            1,
            |_| {},
            &mut gate,
        )
        .unwrap_err();
        assert_eq!(error, CANCELLED);
        if let Some(trigger) = trigger {
            trigger.join().unwrap();
        }
        assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 0);
        fixture.assert_untouched_project();
    }
}
#[test]
fn preexisting_symlink_hardlink_directory_and_external_collision_are_never_overwritten() {
    let fixture = Fixture::new();
    let original = fixture.path.join("original");
    fs::write(&original, b"original").unwrap();
    symlink(&original, fixture.target()).unwrap();
    assert!(files::SaveTarget::capture(&fixture.project, &fixture.target()).is_err());
    fs::remove_file(fixture.target()).unwrap();
    fs::hard_link(&original, fixture.target()).unwrap();
    assert!(files::SaveTarget::capture(&fixture.project, &fixture.target()).is_err());
    fs::remove_file(fixture.target()).unwrap();
    fs::create_dir(fixture.target()).unwrap();
    assert!(files::SaveTarget::capture(&fixture.project, &fixture.target()).is_err());
    fs::remove_dir(fixture.target()).unwrap();
    let fake = Fake::new(b"good");
    let cancel = AtomicBool::new(false);
    let mut gate = || {
        fs::write(fixture.target(), b"external").unwrap();
        Ok(())
    };
    assert!(exercise(
        &fixture,
        &fake,
        b"good",
        Duration::ZERO,
        &cancel,
        0,
        |_| {},
        &mut gate
    )
    .unwrap_err()
    .contains("覆盖"));
    assert_eq!(fs::read(fixture.target()).unwrap(), b"external");
    assert_eq!(fs::read(original).unwrap(), b"original");
    fixture.assert_untouched_project();
}
#[test]
fn replaced_parent_and_rejected_task_gate_retain_external_content() {
    for replace_parent in [true, false] {
        let fixture = Fixture::new();
        let fake = Fake::new(b"good");
        let cancel = AtomicBool::new(false);
        let mut gate = || {
            if replace_parent {
                fs::rename(&fixture.folder, fixture.path.join("original-folder")).unwrap();
                fs::create_dir(&fixture.folder).unwrap();
                fs::write(fixture.folder.join("external"), b"external").unwrap();
                Ok(())
            } else {
                Err("task gate rejected".into())
            }
        };
        let error = exercise(
            &fixture,
            &fake,
            b"good",
            Duration::ZERO,
            &cancel,
            0,
            |_| {},
            &mut gate,
        )
        .unwrap_err();
        assert!(!fixture.target().exists());
        if replace_parent {
            assert!(error.contains("替换"));
            assert_eq!(
                fs::read(fixture.folder.join("external")).unwrap(),
                b"external"
            );
            assert_eq!(
                fs::read_dir(fixture.path.join("original-folder"))
                    .unwrap()
                    .count(),
                0
            );
        } else {
            assert_eq!(error, "task gate rejected");
            assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 0);
        }
        fixture.assert_untouched_project();
    }
}

#[test]
fn published_file_is_retained_when_directory_durability_is_uncertain() {
    let fixture = Fixture::new();
    let fake = Fake::new(b"good");
    let plan = fake.plan();
    let cancel = AtomicBool::new(false);
    let target = files::SaveTarget::capture(&fixture.project, &fixture.target()).unwrap();
    let mut stage = target.anonymous().unwrap();
    stage.write_all(b"good").unwrap();
    let verified = files::verify_anonymous(stage, &plan, &cancel).unwrap();
    let warning = target.publish_with_failed_sync(&verified).unwrap().unwrap();
    assert!(warning.contains("文件已保存"));
    assert!(warning.contains("持久性未确认"));
    assert_eq!(fs::read(fixture.target()).unwrap(), b"good");
    assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 1);
    fixture.assert_untouched_project();
}

#[test]
fn anonymous_inode_changed_after_final_hash_is_retained_without_publication() {
    let fixture = Fixture::new();
    let plan = Fake::new(b"good").plan();
    let cancel = AtomicBool::new(false);
    let target = files::SaveTarget::capture(&fixture.project, &fixture.target()).unwrap();
    let mut stage = target.anonymous().unwrap();
    stage.write_all(b"good").unwrap();
    let mut external_descriptor = stage.try_clone().unwrap();
    let verified = files::verify_anonymous(stage, &plan, &cancel).unwrap();
    use std::io::{Seek, SeekFrom};
    external_descriptor.seek(SeekFrom::Start(0)).unwrap();
    external_descriptor.write_all(b"bad!").unwrap();
    assert!(target
        .publish(&verified)
        .unwrap_err()
        .contains("最终复核后变化"));
    assert!(!fixture.target().exists());
    assert_eq!(fs::read_dir(&fixture.folder).unwrap().count(), 0);
}
