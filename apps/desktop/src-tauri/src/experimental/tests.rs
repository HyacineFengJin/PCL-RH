//! Isolated stdio children exercise native retirement without Tauri or AI.
use super::*;
use std::time::{Duration, Instant};

struct Fixture(PathBuf, tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("experimental")).unwrap();
        fs::write(
            root.join("experimental/host.mjs"),
            r#"
import fs from 'node:fs/promises';
import readline from 'node:readline';
import path from 'node:path';
const root = path.resolve(process.argv[2], '../..');
const exists = async name => fs.access(path.join(root, name)).then(() => true, () => false);
let preset = null;
for await (const line of readline.createInterface({input: process.stdin})) {
  const request = JSON.parse(line); let result = {preset};
  if (request.operation === 'idle_prepare' && await exists('invalid')) {
    process.stdout.write('invalid reply\n'); continue;
  }
  if (request.operation === 'idle_resume') { preset = request.args.presetId; result = true; }
  if (request.operation === 'idle_prepare') {
    if (await exists('delay')) {
      await fs.writeFile(path.join(root, 'entered'), '');
      while (!await exists('release')) await new Promise(r => setTimeout(r, 5));
    }
    result = {busy: await exists('busy'), resumePresetId: 'fixture-preset'};
  }
  process.stdout.write(JSON.stringify({ok: true, result}) + '\n');
  if (request.operation === 'shutdown') break;
}
"#,
        )
        .unwrap();
        Self(root, temporary)
    }
    fn wait_for(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.0.join(name).exists() {
            assert!(Instant::now() < deadline, "fake child did not reach probe");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.1.path());
    }
}
#[test]
fn never_used_suspend_does_not_start_or_create_data() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    assert_eq!(
        host.suspend().unwrap(),
        SuspendResult {
            released: false,
            busy: false
        }
    );
    assert!(!fixture.0.join(".pcl-rust").exists());
    assert!(host.is_stopped());
}

#[test]
fn absent_passive_projections_have_no_session_store_lock_or_authority_token() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    for slot in [
        "home.secondary",
        "tools.cards",
        "rh-code.cards",
        "settings.cards",
        "home.banner",
        "home.quick-actions",
        "settings.launch",
        "settings.java",
        "settings.manage",
        "settings.language",
        "settings.misc",
    ] {
        assert_eq!(
            host.call("extensions_cards", json!({"slot":slot})).unwrap(),
            json!([])
        );
    }
    assert_eq!(
        host.call("extensions_catalog", json!({})).unwrap(),
        json!({"panels":[],"actions":[]})
    );
    let operations = Mutex::new(());
    let result = host.call_with_effect("extensions_cards", json!({"slot":"rh-code.cards", "context":{
        "rootId":"synthetic-root", "instanceId":"synthetic-instance", "revision":"fixture",
        "summary":{"minecraftVersion":"1.21", "loader":"Fabric", "modCount":0, "isolated":true}
    }}), Some(&operations), |result, guard| {
        assert!(guard.is_some());
        assert!(operations.try_lock().is_err());
        assert!(host.session.try_lock().is_err());
        assert_eq!(host.callers.load(Ordering::SeqCst), 1);
        Ok(result)
    }).unwrap();
    assert_eq!(result, json!([]));
    assert!(host.session.lock().unwrap().is_none());
    assert!(!fixture.0.join(".pcl-rust").exists());
    assert_eq!(host.retiring.load(Ordering::Acquire), 0);
    assert_eq!(host.callers.load(Ordering::SeqCst), 0);
    assert!(host.resume_preset.lock().unwrap().is_none());
    assert!(host.is_stopped());
}

#[test]
fn passive_fastpath_preserves_filters_management_close_and_retirement_routes() {
    for (operation, args) in [
        (
            "extensions_cards",
            json!({"slot":"rh-code.cards", "filter":null}),
        ),
        (
            "extensions_cards",
            json!({"slot":"rh-code.cards", "filter":{"extensionId":"x","cardId":"y"}}),
        ),
        ("extensions_cards", json!({"slot":"unknown"})),
        (
            "extensions_cards",
            json!({"slot":"rh-code.cards", "context":false}),
        ),
        ("extensions_catalog", json!({"filter":null})),
        ("extensions_list", json!({})),
        ("extensions_confirm", json!({})),
        ("extensions_action", json!({})),
        ("source_import", json!({})),
        ("status", json!({})),
        ("rh_code_run", json!({})),
    ] {
        assert!(passive_empty_projection(operation, &args).is_none());
    }
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    host.retiring.store(1, Ordering::SeqCst);
    assert!(host
        .call("extensions_cards", json!({"slot":"home.secondary"}))
        .unwrap_err()
        .contains("退役"));
    host.retiring.store(0, Ordering::SeqCst);
    host.closing.store(true, Ordering::SeqCst);
    assert_eq!(
        host.call("extensions_catalog", json!({})).unwrap_err(),
        "启动器正在关闭"
    );
    assert!(host.session.lock().unwrap().is_none());
    assert_eq!(host.callers.load(Ordering::SeqCst), 0);
    assert!(!fixture.0.join(".pcl-rust").exists());
}

#[test]
fn live_sessions_and_existing_even_invalid_store_bytes_keep_the_real_node_route() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    host.call("status", json!({})).unwrap();
    let pid = host.session.lock().unwrap().as_ref().unwrap().child.id();
    assert!(host
        .call("extensions_cards", json!({"slot":"home.secondary"}))
        .unwrap()
        .is_object());
    assert_eq!(
        host.session.lock().unwrap().as_ref().unwrap().child.id(),
        pid
    );
    assert!(host.suspend().unwrap().released);
    assert_eq!(
        host.call("extensions_cards", json!({"slot":"home.secondary"}))
            .unwrap(),
        json!([])
    );
    assert!(host.session.lock().unwrap().is_none());
    let data =
        pcl_core::retained_fs::Directory::open_absolute(&fixture.0.join(".pcl-rust/experimental"))
            .unwrap();
    use std::io::Write;
    data.create_file(&pcl_core::retained_fs::Leaf::new("extensions.json").unwrap())
        .unwrap()
        .write_all(b"not valid JSON; only the real host may interpret it")
        .unwrap();
    assert!(host
        .call("extensions_catalog", json!({}))
        .unwrap()
        .get("preset")
        .is_some());
    assert!(host.session.lock().unwrap().is_some());
    assert_eq!(
        fs::read(fixture.0.join(".pcl-rust/experimental/extensions.json")).unwrap(),
        b"not valid JSON; only the real host may interpret it"
    );
    assert!(host.suspend().unwrap().released);
}
#[test]
fn source_release_without_a_session_never_starts_or_creates_data() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    let token = "00000000-0000-4000-8000-000000000001";
    assert_eq!(
        host.release_source(json!({"sourceId": token})).unwrap(),
        false
    );
    assert_eq!(
        host.release_source(json!({"sourceId": token})).unwrap(),
        false
    );
    assert!(host.release_source(json!({"sourceId": "invalid"})).is_err());
    assert!(host
        .release_source(json!({"sourceId": token, "operation": "status"}))
        .is_err());
    assert!(host.session.lock().unwrap().is_none());
    assert!(!fixture.0.join(".pcl-rust").exists());
    assert!(host.is_stopped());
    host.closing.store(true, Ordering::SeqCst);
    assert!(host.release_source(json!({"sourceId": token})).is_err());
    assert!(host.is_stopped());
}
#[test]
fn source_release_uses_only_the_existing_session() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    host.call("status", json!({})).unwrap();
    let pid = host.session.lock().unwrap().as_ref().unwrap().child.id();
    host.release_source(json!({"sourceId":"00000000-0000-4000-8000-000000000001"}))
        .unwrap();
    assert_eq!(
        host.session.lock().unwrap().as_ref().unwrap().child.id(),
        pid
    );
    assert!(!host.is_stopped());
    assert!(host.suspend().unwrap().released);
    assert!(host.is_stopped());
    assert_eq!(
        host.release_source(json!({"sourceId":"00000000-0000-4000-8000-000000000001"}))
            .unwrap(),
        false
    );
    assert!(host.session.lock().unwrap().is_none());
    assert!(host.is_stopped());
}
#[test]
fn stale_retirement_flag_cannot_override_real_session_ownership() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    {
        let _owner = host.session.lock().unwrap();
        assert!(
            !host.is_stopped(),
            "a busy ownership lock is not an exit receipt"
        );
    }
    assert!(host.is_stopped());
    host.call("status", json!({})).unwrap();
    // Model an old retirement publication racing after this session started.
    host.stopped.store(true, Ordering::SeqCst);
    assert_eq!(host.callers.load(Ordering::SeqCst), 0);
    assert_eq!(host.retiring.load(Ordering::Acquire), 0);
    assert!(
        !host.is_stopped(),
        "the real live session still owns the host"
    );
    assert!(host.suspend().unwrap().released);
    assert!(host.is_stopped());
}
#[test]
fn busy_host_is_retained_and_idle_host_reaps_then_restarts() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    host.call("status", json!({})).unwrap();
    let pid = host.session.lock().unwrap().as_ref().unwrap().child.id();
    fs::write(fixture.0.join("busy"), "").unwrap();
    assert_eq!(
        host.suspend().unwrap(),
        SuspendResult {
            released: false,
            busy: true
        }
    );
    assert_eq!(
        host.session.lock().unwrap().as_ref().unwrap().child.id(),
        pid
    );
    fs::remove_file(fixture.0.join("busy")).unwrap();
    assert_eq!(
        host.suspend().unwrap(),
        SuspendResult {
            released: true,
            busy: false
        }
    );
    assert!(host.session.lock().unwrap().is_none());
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    assert!(host.is_stopped());
    assert!(!host.closing.load(Ordering::SeqCst));
    assert_eq!(
        host.call("status", json!({})).unwrap()["preset"],
        "fixture-preset"
    );
    assert!(host.resume_preset.lock().unwrap().is_none());
    assert!(!host.is_stopped());
    assert!(host.suspend().unwrap().released);
}
#[test]
fn in_flight_session_and_queued_admission_block_retirement() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    host.call("status", json!({})).unwrap();
    {
        let _owner = host.session.lock().unwrap();
        assert!(host.suspend().unwrap().busy);
    }
    host.callers.fetch_add(1, Ordering::SeqCst);
    assert!(host.suspend().unwrap().busy);
    host.callers.fetch_sub(1, Ordering::SeqCst);
    assert!(host.suspend().unwrap().released);
}
#[test]
fn call_racing_with_idle_probe_keeps_existing_child() {
    let fixture = Fixture::new();
    let host = Arc::new(Host::new(fixture.0.clone()));
    host.call("status", json!({})).unwrap();
    let pid = host.session.lock().unwrap().as_ref().unwrap().child.id();
    fs::write(fixture.0.join("delay"), "").unwrap();
    let retiring = host.clone();
    let retire = std::thread::spawn(move || retiring.suspend());
    fixture.wait_for("entered");
    let calling = host.clone();
    let call = std::thread::spawn(move || calling.call("status", json!({})));
    let deadline = Instant::now() + Duration::from_secs(3);
    while host.callers.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::write(fixture.0.join("release"), "").unwrap();
    assert!(retire.join().unwrap().unwrap().busy);
    call.join().unwrap().unwrap();
    assert_eq!(
        host.session.lock().unwrap().as_ref().unwrap().child.id(),
        pid
    );
    assert!(host.suspend().unwrap().released);
}

#[test]
fn failed_probe_retains_ownership_until_an_ordinary_call_recovers_transport() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    host.call("status", json!({})).unwrap();
    let pid = host.session.lock().unwrap().as_ref().unwrap().child.id();
    fs::write(fixture.0.join("invalid"), "").unwrap();
    assert!(host.suspend().is_err());
    {
        let owner = host.session.lock().unwrap();
        assert_eq!(owner.as_ref().unwrap().child.id(), pid);
        assert!(owner.as_ref().unwrap().broken);
    }
    assert!(!host.closing.load(Ordering::SeqCst));
    fs::remove_file(fixture.0.join("invalid")).unwrap();
    assert_eq!(
        host.call("status", json!({})).unwrap()["preset"],
        Value::Null
    );
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    assert!(host.suspend().unwrap().released);
}

#[test]
fn native_admission_blocks_idle_retirement_between_stdio_requests() {
    let fixture = Fixture::new();
    let host = Arc::new(Host::new(fixture.0.clone()));
    let native = host.admit_native().unwrap();
    assert!(!host.is_stopped());
    assert!(host.suspend().unwrap().busy);
    assert!(!fixture.0.join(".pcl-rust").exists());

    host.call("synthetic_export", json!({})).unwrap();
    let pid = host.session.lock().unwrap().as_ref().unwrap().child.id();
    assert_eq!(host.callers.load(Ordering::SeqCst), 1);
    // No stdio call owns the session mutex in this interval. The native guard
    // covers source application before the later receipt request enters it.
    assert!(host.session.try_lock().is_ok());
    assert_eq!(
        host.suspend().unwrap(),
        SuspendResult {
            released: false,
            busy: true
        }
    );
    assert_eq!(
        host.session.lock().unwrap().as_ref().unwrap().child.id(),
        pid
    );
    host.call("synthetic_receipt", json!({})).unwrap();
    assert_eq!(host.callers.load(Ordering::SeqCst), 1);
    drop(native);
    assert_eq!(host.callers.load(Ordering::SeqCst), 0);
    assert!(host.suspend().unwrap().released);
    assert!(host.is_stopped());
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
}

#[test]
fn app_close_remains_pending_after_child_reap_until_native_work_releases_admission() {
    let fixture = Fixture::new();
    let host = Arc::new(Host::new(fixture.0.clone()));
    let native = host.admit_native().unwrap();
    host.call("synthetic_export", json!({})).unwrap();
    let pid = host.session.lock().unwrap().as_ref().unwrap().child.id();
    host.shutdown();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !host.stopped.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "isolated host was not reaped");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(host.session.lock().unwrap().is_none());
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    assert_eq!(host.callers.load(Ordering::SeqCst), 1);
    assert!(
        !host.is_stopped(),
        "app close must wait for the native interval too"
    );
    // Shutdown prevents a late receipt call from restarting the host. Its own
    // failed CallAdmission must release without releasing the native owner.
    assert!(host.call("synthetic_receipt", json!({})).is_err());
    assert_eq!(host.callers.load(Ordering::SeqCst), 1);
    assert!(!host.is_stopped());
    drop(native);
    assert!(host.is_stopped());
    assert_eq!(host.callers.load(Ordering::SeqCst), 0);
    assert!(host.session.lock().unwrap().is_none());
}

#[test]
fn native_failure_releases_the_whole_action_admission() {
    let fixture = Fixture::new();
    let host = Arc::new(Host::new(fixture.0.clone()));
    let result = (|| {
        let _native = host.admit_native()?;
        host.call("synthetic_export", json!({}))?;
        assert_eq!(host.callers.load(Ordering::SeqCst), 1);
        Err::<(), String>("synthetic native application failure".into())
    })();
    assert_eq!(result.unwrap_err(), "synthetic native application failure");
    assert_eq!(host.callers.load(Ordering::SeqCst), 0);
    assert!(host.suspend().unwrap().released);
    assert!(host.is_stopped());
}

#[test]
fn native_admission_after_the_close_fence_does_not_start_or_pin_a_host() {
    let fixture = Fixture::new();
    let host = Arc::new(Host::new(fixture.0.clone()));
    // This is the gap between the outer Tauri check and native admission.
    host.closing.store(true, Ordering::SeqCst);
    assert!(host.admit_native().is_err());
    assert_eq!(host.callers.load(Ordering::SeqCst), 0);
    assert!(host.is_stopped());
    assert!(host.session.lock().unwrap().is_none());
    assert!(!fixture.0.join(".pcl-rust").exists());
}

#[cfg(unix)]
#[test]
fn build_artifact_open_requires_exact_owned_directory_without_links() {
    let fixture = Fixture::new();
    let root = fs::canonicalize(&fixture.0).unwrap();
    let job = "12345678-1234-1234-1234-123456789abc";
    let run = "abcdef12-1234-1234-1234-123456789abc";
    let base = root
        .join(".pcl-rust/experimental/jobs")
        .join(job)
        .join("build-runs");
    let directory = base.join(run).join("artifacts");
    fs::create_dir_all(&directory).unwrap();
    let args = json!({"jobId":job,"runId":run});
    let result = json!({"directory":directory,"runId":run,"artifactCount":1});
    assert_eq!(
        checked_build_directory(&root, &args, &result).unwrap(),
        directory
    );
    assert!(
        checked_build_directory(&root, &json!({"jobId":"../escape","runId":run}), &result).is_err()
    );
    assert!(checked_build_directory(
        &root,
        &args,
        &json!({"directory":root,"runId":run,"artifactCount":1})
    )
    .is_err());
    assert!(checked_build_directory(
        &root,
        &args,
        &json!({"directory":directory,"runId":run,"artifactCount":0})
    )
    .is_err());
    let saved = base.join("saved");
    fs::rename(base.join(run), &saved).unwrap();
    std::os::unix::fs::symlink(&saved, base.join(run)).unwrap();
    assert!(checked_build_directory(&root, &args, &result).is_err());
}
