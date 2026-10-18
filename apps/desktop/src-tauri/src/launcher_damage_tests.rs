use super::*;
use crate::{integration_tests::Fixture, tasks::TaskStage};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

fn fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../work/damage-java-2026-10-18/native")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(path.join("Minecraft/.minecraft/versions/fixture")).unwrap();
    fs::write(
        path.join("Minecraft/.minecraft/versions/fixture/fixture.json"),
        br#"{"id":"fixture","mainClass":"fixture.Main","libraries":[]}"#,
    )
    .unwrap();
    Fixture(path.canonicalize().unwrap())
}
fn report() -> Value {
    json!({"id":"fixture","operation_id":"fixture","backup_directory":".pcl-damage-repairs/fixture","files_repaired":1,"files_already_valid":2,"files_missing":0,"unsupported_total":0,"unsupported":[],"assets_enumerated":true})
}
#[test]
fn stale_revision_and_busy_game_reject_before_worker() {
    let f = fixture();
    let s = Arc::new(f.shared());
    let root = s.config.resolve(None).unwrap();
    assert!(start(s.clone(), "fixture", &root.id, "stale").is_err());
    s.status.lock().unwrap().stage = "running".into();
    assert!(start(
        s.clone(),
        "fixture",
        &root.id,
        &s.config.snapshot().revision
    )
    .is_err());
    assert!(s.tasks.active().is_none());
}
#[test]
fn result_is_retained_and_incomplete_does_not_claim_full_repair() {
    let f = fixture();
    let s = Arc::new(f.shared());
    let root = s.config.resolve(None).unwrap();
    let before = s.config.snapshot();
    let id = start_with(
        s.clone(),
        "fixture",
        &root.id,
        &before.revision,
        false,
        |_, _, _| {
            let mut r = report();
            r["files_missing"] = json!(3);
            Ok(r)
        },
    )
    .unwrap();
    let result = s.tasks.wait_terminal(&id).unwrap();
    assert_eq!(result.stage, TaskStage::Error);
    assert_eq!(result.kind, TaskKind::InstanceDamageRepair);
    assert_eq!(result.result.unwrap()["files_repaired"], 1);
    assert!(s.tasks.active().is_none());
    assert_eq!(s.config.snapshot().revision, before.revision);
}
#[test]
fn binding_error_survives_late_cancellation() {
    let f = fixture();
    let s = Arc::new(f.shared());
    let root = s.config.resolve(None).unwrap();
    let id = start_with(
        s.clone(),
        "fixture",
        &root.id,
        &s.config.snapshot().revision,
        false,
        |_, _, task| {
            task.cancellation_token().store(true, Ordering::SeqCst);
            Err("precise filesystem conflict".into())
        },
    )
    .unwrap();
    let result = s.tasks.wait_terminal(&id).unwrap();
    assert_eq!(result.stage, TaskStage::Error);
    assert_eq!(result.error.as_deref(), Some("precise filesystem conflict"));
}
#[test]
fn recovery_needs_no_surviving_instance_or_network() {
    let f = fixture();
    let s = Arc::new(f.shared());
    let root = s.config.resolve(None).unwrap();
    fs::remove_dir_all(Path::new(&root.path).join("versions/fixture")).unwrap();
    let id = recover(s.clone(), &root.id, &s.config.snapshot().revision).unwrap();
    let result = s.tasks.wait_terminal(&id).unwrap();
    assert_eq!(result.stage, TaskStage::Complete);
    assert_eq!(result.kind, TaskKind::InstanceDamageRecover);
    assert!(result.instance_id.is_none());
    assert_eq!(result.network_bytes, 0);
}

#[test]
fn pending_repair_blocks_writers_but_root_recovery_clears_the_guard() {
    let f = fixture();
    let s = Arc::new(f.shared());
    let root = s.config.resolve(None).unwrap();
    let pending = Path::new(&root.path).join(".pcl-linux/dependency-repairs/d-aa-bb-cc");
    fs::create_dir_all(&pending).unwrap();
    assert!(crate::ensure_instance_files_ready(&s, &root).is_err());
    assert!(crate::root_removal::require_root_removable(&s, &root.id).is_err());
    assert!(crate::bootstrap_view(&s).damage_recovery_error.is_some());
    assert!(start(
        s.clone(),
        "fixture",
        &root.id,
        &s.config.snapshot().revision
    )
    .is_err());
    fs::remove_dir_all(Path::new(&root.path).join("versions/fixture")).unwrap();
    let id = recover(s.clone(), &root.id, &s.config.snapshot().revision).unwrap();
    let result = s.tasks.wait_terminal(&id).unwrap();
    assert_eq!(result.stage, TaskStage::Complete);
    assert_eq!(result.result.unwrap()["recovered"], 1);
    assert!(crate::ensure_instance_files_ready(&s, &root).is_ok());
    assert!(crate::bootstrap_view(&s).damage_recovery_error.is_none());
}
