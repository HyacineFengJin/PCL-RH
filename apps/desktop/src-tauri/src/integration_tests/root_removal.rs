//! Exercise the same admission-to-commit operation as the Tauri command, using
//! disposable game roots. Registry-only tests alone missed the old disk gate.
use super::*;

#[test]
fn missing_root_removal_commits_and_bootstrap_replaces_the_active_selection() {
    let fixture = Fixture::new();
    let state = fixture.shared();
    let original = state.config.snapshot().root_id;
    let imported = fixture.second(&state.config);
    state.config.select(&imported.id).unwrap();
    fs::remove_dir_all(&imported.path).unwrap();
    assert!(
        !bootstrap_view(&state)
            .roots
            .iter()
            .find(|root| root.id == imported.id)
            .unwrap()
            .available
    );
    crate::root_removal::remove(&state, &imported.id).unwrap();
    let view = bootstrap_view(&state);
    assert_eq!(view.settings.root_id, original);
    assert!(view.roots.iter().all(|root| root.id != imported.id));
    assert!(view.scan_error.is_none());
    let (reloaded, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    assert_eq!(reloaded.snapshot().root_id, original);
    assert!(reloaded.registered(&imported.id).is_err());
    assert!(!Path::new(&imported.path).exists());
}

#[test]
#[cfg(unix)]
fn missing_root_removal_forgets_a_dangling_alias_without_touching_moved_files() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let state = fixture.shared();
    let target = fixture.0.join("portable");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("world.txt"), b"keep game bytes").unwrap();
    let alias = fixture.0.join("alias");
    symlink(&target, &alias).unwrap();
    let imported = state
        .config
        .register(alias.to_string_lossy().into(), None)
        .unwrap();
    let moved = fixture.0.join("moved");
    fs::rename(&target, &moved).unwrap();
    crate::root_removal::remove(&state, &imported.id).unwrap();
    assert_eq!(
        fs::read(moved.join("world.txt")).unwrap(),
        b"keep game bytes"
    );
    assert_eq!(fs::read_link(alias).unwrap(), target);
    assert!(!target.exists());
}

#[test]
fn missing_root_removal_waits_for_cancellation_cleanup_and_bound_game() {
    let fixture = Fixture::new();
    let state = fixture.shared();
    let imported = fixture.second(&state.config);
    let task = state
        .tasks
        .admit_queued(
            TaskTarget {
                root_id: imported.id.clone(),
                root_path: imported.path.clone(),
                instance_id: None,
            },
            TaskKind::Install,
            tasks::TaskScope::root(Path::new(&imported.path)).unwrap(),
        )
        .unwrap();
    fs::remove_dir_all(&imported.path).unwrap();
    assert!(crate::root_removal::remove(&state, &imported.id)
        .unwrap_err()
        .contains("使用"));
    state.tasks.cancel(task.id()).unwrap();
    assert!(crate::root_removal::remove(&state, &imported.id).is_err());
    task.finish(TaskOutcome::Failed("cleanup finished".into()));
    for stage in ["preparing", "running"] {
        *state.status.lock().unwrap() = RunStatus {
            stage: stage.into(),
            root_id: Some(imported.id.clone()),
            root_path: Some(imported.path.clone()),
            ..Default::default()
        };
        assert!(crate::root_removal::remove(&state, &imported.id).is_err());
    }
    *state.status.lock().unwrap() = RunStatus::default();
    assert!(crate::root_removal::remove(&state, &imported.id).is_ok());
}

#[test]
fn missing_root_removal_keeps_an_unprovable_active_file_claim_until_drain() {
    let fixture = Fixture::new();
    let state = fixture.shared();
    let first = state.config.resolve(None).unwrap();
    let imported = fixture.second(&state.config);
    let task = state
        .tasks
        .admit(
            TaskTarget {
                root_id: first.id,
                root_path: first.path,
                instance_id: None,
            },
            TaskKind::ResourceOperation,
        )
        .unwrap();
    fs::remove_dir_all(&imported.path).unwrap();
    assert!(crate::root_removal::remove(&state, &imported.id)
        .unwrap_err()
        .contains("文件操作"));
    task.finish(TaskOutcome::Failed("cleanup finished".into()));
    assert!(crate::root_removal::remove(&state, &imported.id).is_ok());
}

#[test]
fn missing_root_removal_never_bypasses_project_recovery_or_the_last_root_rule() {
    for marker in [
        "instance-rename-pending.json",
        "instance-delete-pending.json",
    ] {
        let fixture = Fixture::new();
        let state = fixture.shared();
        let imported = fixture.second(&state.config);
        fs::remove_dir_all(&imported.path).unwrap();
        let settings = fixture.0.join(".pcl-rust/settings.json");
        let before = fs::read(&settings).unwrap();
        let marker = fixture.0.join(".pcl-rust").join(marker);
        fs::write(&marker, b"retain incomplete recovery").unwrap();
        assert!(crate::root_removal::remove(&state, &imported.id).is_err());
        assert_eq!(fs::read(settings).unwrap(), before);
        assert_eq!(fs::read(marker).unwrap(), b"retain incomplete recovery");
    }
    let fixture = Fixture::new();
    let state = fixture.shared();
    let only = state.config.snapshot();
    fs::remove_dir_all(&only.root).unwrap();
    assert!(crate::root_removal::remove(&state, &only.root_id)
        .unwrap_err()
        .contains("先添加"));
    assert_eq!(state.config.snapshot(), only);
    assert!(!Path::new(&only.root).exists());
}

#[test]
fn root_removal_does_not_treat_other_filesystem_failures_as_missing() {
    let fixture = Fixture::new();
    let state = fixture.shared();
    let imported = fixture.second(&state.config);
    fs::remove_dir_all(&imported.path).unwrap();
    fs::write(&imported.path, b"replacement file").unwrap();
    assert!(crate::root_removal::remove(&state, &imported.id).is_err());
    assert!(state.config.registered(&imported.id).is_ok());
    assert_eq!(fs::read(&imported.path).unwrap(), b"replacement file");
}
