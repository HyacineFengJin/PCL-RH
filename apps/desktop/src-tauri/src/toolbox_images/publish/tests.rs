use super::*;
use std::{
    fs,
    sync::atomic::{AtomicBool, Ordering},
};

#[test]
fn actual_export_preserves_bytes_and_existing_targets_and_checks_bounds_before_admission() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let target = root.join("报告.PNG");
    let output = publish_new(&target, b"bounded memory body", "png", 3, || Ok(())).unwrap();
    assert_eq!(output.status, "complete");
    assert_eq!(output.count, 3);
    assert_eq!(fs::read(&target).unwrap(), b"bounded memory body");
    let called = AtomicBool::new(false);
    assert!(publish_new(&target, b"replacement", "png", 1, || {
        called.store(true, Ordering::SeqCst);
        Ok(())
    })
    .is_err());
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(fs::read(&target).unwrap(), b"bounded memory body");
    for (name, bytes) in [
        ("wrong.txt", b"x".as_slice()),
        ("empty.png", b"".as_slice()),
    ] {
        assert!(publish_new(&root.join(name), bytes, "png", 1, || Ok(())).is_err());
        assert!(!root.join(name).exists());
    }
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn rejected_admission_and_last_moment_collision_clean_only_their_new_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let target = root.join("export.json");
    let error = publish_new(&target, b"{}", "json", 1, || {
        Err::<(), _>("stale selection".into())
    })
    .err()
    .unwrap();
    assert!(error.contains("stale selection"));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert!(publish_new(&target, b"{}", "json", 1, || {
        fs::write(&target, b"independent original").map_err(|e| e.to_string())
    })
    .is_err());
    assert_eq!(fs::read(&target).unwrap(), b"independent original");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn changed_chosen_parent_is_rejected_or_blocked_by_native_descendant_sharing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let chosen = root.join("chosen");
    let parked = root.join("parked");
    fs::create_dir(&chosen).unwrap();
    fs::write(chosen.join("keep"), b"original").unwrap();
    let target = chosen.join("report.txt");
    assert!(publish_new(&target, b"diagnostic", "txt", 1, || {
        match fs::rename(&chosen, &parked) {
            Ok(()) => fs::create_dir(&chosen).map_err(|e| e.to_string()),
            #[cfg(windows)]
            Err(error) if matches!(error.raw_os_error(), Some(5 | 32)) => {
                assert!(chosen.is_dir() && !parked.exists());
                Err("native descendant sharing blocked the substitution".into())
            }
            Err(error) => Err(error.to_string()),
        }
    })
    .is_err());
    assert!(!target.exists() && !parked.join("report.txt").exists());
    let original = if parked.exists() { parked } else { chosen };
    assert_eq!(fs::read(original.join("keep")).unwrap(), b"original");
    assert_eq!(fs::read_dir(original).unwrap().count(), 1);
}

#[cfg(not(target_os = "linux"))]
use super::backend as native;
#[cfg(target_os = "linux")]
use super::portable as native;

#[test]
fn actual_portable_owner_publishes_and_discards_without_named_residue() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    native::publish_new(&root.join("image.png"), b"bytes", "png", 1, || Ok(())).unwrap();
    assert!(native::publish_new(
        &root.join("stale.png"),
        b"bytes",
        "png",
        1,
        || Err::<(), _>("stale".into())
    )
    .is_err());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    assert_eq!(fs::read(root.join("image.png")).unwrap(), b"bytes");
}

#[test]
fn unknown_workspace_entry_survives_successful_publication_as_a_cleanup_warning() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let mut evidence = None;
    let outcome = native::publish_new(&root.join("report.json"), b"{}", "json", 1, || {
        let workspace = fs::read_dir(&root)
            .map_err(|e| e.to_string())?
            .next()
            .ok_or("missing private workspace")?
            .map_err(|e| e.to_string())?
            .path();
        fs::write(workspace.join("unknown"), b"keep evidence").map_err(|e| e.to_string())?;
        evidence = Some(workspace);
        Ok(())
    })
    .unwrap();
    assert_eq!(outcome.status, "complete");
    assert!(outcome.warning.is_some());
    assert_eq!(fs::read(root.join("report.json")).unwrap(), b"{}");
    assert_eq!(
        fs::read(evidence.unwrap().join("unknown")).unwrap(),
        b"keep evidence"
    );
}

#[test]
fn admission_guard_is_held_until_the_actual_target_is_published() {
    use std::sync::Arc;
    struct Guard(std::path::PathBuf, Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            assert_eq!(fs::read(&self.0).unwrap(), b"guarded export");
            assert!(!self.1.swap(true, Ordering::SeqCst));
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let target = root.join("guard.txt");
    let retired = Arc::new(AtomicBool::new(false));
    let output = native::publish_new(&target, b"guarded export", "txt", 1, || {
        Ok(Guard(target.clone(), retired.clone()))
    })
    .unwrap();
    assert_eq!(output.status, "complete");
    assert!(retired.load(Ordering::SeqCst));
}
