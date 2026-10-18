use super::*;
use crate::{integration_tests::Fixture, tasks::TaskStage};
use serde_json::json;
use sha1::{Digest, Sha1};
use std::{fs, sync::mpsc, time::Duration};

fn fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../work/launch-readiness-2026-10-18/native-integrity")
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

fn instance(shared: &Shared) -> (crate::config::GameRoot, std::path::PathBuf) {
    let root = shared.config.resolve(None).unwrap();
    let path = Path::new(&root.path).join("versions/fixture");
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("fixture.json"),serde_json::to_vec(&json!({"id":"fixture","downloads":{"client":{"url":"https://piston-data.mojang.com/fixture","sha1":format!("{:x}",Sha1::digest(b"good")),"size":4}}})).unwrap()).unwrap();
    fs::write(path.join("fixture.jar"), b"evil").unwrap();
    (root, path)
}

fn report() -> IntegrityReport {
    IntegrityReport {
        id: "fixture".into(),
        files_verified: 1,
        files_missing: 0,
        files_corrupt: 0,
        files_unverifiable: 0,
        assets_enumerated: true,
        problems: Vec::new(),
        problems_total: 0,
    }
}

#[test]
fn actual_local_check_retains_damaged_file_and_readable_complete_report() {
    let fixture = fixture();
    let shared = Arc::new(fixture.shared());
    let (root, path) = instance(&shared);
    let before = shared.config.snapshot();
    let id = start(shared.clone(), "fixture", &root.id, &before.revision).unwrap();
    let result = shared.tasks.wait_terminal(&id).unwrap();
    assert_eq!(result.kind, TaskKind::InstanceVerify);
    assert_eq!(
        serde_json::to_value(result.kind).unwrap(),
        "instance_verify"
    );
    assert_eq!(result.stage, TaskStage::Complete);
    assert_eq!(result.result.as_ref().unwrap()["files_corrupt"], 1);
    assert_eq!(
        result.result.as_ref().unwrap()["problems"][0]["kind"],
        "sha1_mismatch"
    );
    assert_eq!(result.network_bytes, 0);
    assert_eq!(fs::read(path.join("fixture.jar")).unwrap(), b"evil");
    assert_eq!(shared.config.snapshot().revision, before.revision);
    assert!(shared.tasks.active().is_none());
}

#[test]
fn stale_revision_foreign_root_and_busy_game_reject_local_admission() {
    let fixture = fixture();
    let shared = Arc::new(fixture.shared());
    let (root, _) = instance(&shared);
    assert!(start(shared.clone(), "fixture", &root.id, "stale").is_err());
    assert!(start(
        shared.clone(),
        "fixture",
        "removed-root",
        &shared.config.snapshot().revision
    )
    .is_err());
    shared.status.lock().unwrap().stage = "running".into();
    assert!(start(
        shared.clone(),
        "fixture",
        &root.id,
        &shared.config.snapshot().revision
    )
    .is_err());
    assert!(shared.tasks.active().is_none());
}

#[test]
fn cancellation_holds_exclusive_lease_until_reader_cleanup_and_then_drains() {
    let fixture = fixture();
    let shared = Arc::new(fixture.shared());
    let (root, _) = instance(&shared);
    let (entered, await_entered) = mpsc::channel();
    let (release, await_release) = mpsc::channel();
    let id = start_with(
        shared.clone(),
        "fixture",
        &root.id,
        &shared.config.snapshot().revision,
        move |shared, _, task| {
            assert!(shared.operations.try_lock().is_ok());
            entered.send(()).unwrap();
            await_release.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(task.cancellation_token().load(Ordering::SeqCst));
            Err(crate::tasks::CANCELLED.into())
        },
    )
    .unwrap();
    await_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    shared.tasks.cancel(&id).unwrap();
    assert!(shared.tasks.active().is_some());
    assert!(shared.tasks.blocks_path(Path::new(&root.path)).unwrap());
    release.send(()).unwrap();
    let result = shared.downloads.cancel_and_wait(Some(&id)).unwrap();
    assert_eq!(result.stage, "cancelled");
    assert!(result.message.contains("完整性检查"));
    assert!(shared.tasks.active().is_none());
}

#[test]
fn shutdown_generation_and_settings_changes_reject_finished_reports() {
    for change in ["closing", "generation", "settings"] {
        let fixture = fixture();
        let shared = Arc::new(fixture.shared());
        let (root, _) = instance(&shared);
        let id = start_with(
            shared.clone(),
            "fixture",
            &root.id,
            &shared.config.snapshot().revision,
            move |shared, _, _| {
                match change {
                    "closing" => shared.closing.store(true, Ordering::SeqCst),
                    "generation" => {
                        shared.close_generation.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {
                        let mut settings = shared.config.snapshot();
                        settings.selected = Some("other".into());
                        shared.config.save(settings).unwrap();
                    }
                }
                Ok(report())
            },
        )
        .unwrap();
        let result = shared.tasks.wait_terminal(&id).unwrap();
        assert_eq!(result.stage, TaskStage::Error, "{change}");
        assert!(result.result.is_none());
    }
}

#[test]
fn wrong_instance_result_and_panics_never_publish_success() {
    for panic in [false, true] {
        let fixture = fixture();
        let shared = Arc::new(fixture.shared());
        let (root, _) = instance(&shared);
        let id = start_with(
            shared.clone(),
            "fixture",
            &root.id,
            &shared.config.snapshot().revision,
            move |_, _, _| {
                assert!(!panic, "synthetic integrity panic");
                let mut result = report();
                result.id = "other".into();
                Ok(result)
            },
        )
        .unwrap();
        let result = shared.tasks.wait_terminal(&id).unwrap();
        assert_eq!(result.stage, TaskStage::Error);
        assert!(result.result.is_none());
        assert!(shared.tasks.active().is_none());
    }
}
