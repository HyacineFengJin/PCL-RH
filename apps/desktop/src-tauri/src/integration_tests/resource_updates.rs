//! Application bindings reuse the parent isolated Shared/Config fixture.
use super::*;

#[test]
fn mod_update_publication_and_async_undo_stay_bound_to_the_admitted_root() {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new();
    let state = Arc::new(fixture.shared());
    let (first, id) = rename_fixture(&state);
    let second = fixture.second(&state.config);
    state.config.select(&second.id).unwrap();
    let captured = state.config.isolation_settings(Some(&first.id)).unwrap();
    // rename_fixture returned its root before saving instance settings; native
    // update admission instead captures the current root and policy together.
    let first = captured.root.clone();
    // The provider fixture uses real loopback HTTP bodies and anonymous verified
    // descriptors. Publication then traverses the production task/commit gates.
    let batch = modrinth_install::test_update_batch(
        Path::new(&first.path),
        &state.project,
        &first.id,
        &id,
        captured.policy(&id),
        &captured.revision,
    );
    let network_bytes = batch.network_bytes;
    assert!(network_bytes > 0);
    let instance = Path::new(&first.path).join("versions").join(&id);
    let originals = ["old.jar", "old-disabled.jar.disabled", "custom-dep.jar"].map(|name| {
        let path = instance.join("mods").join(name);
        (
            name,
            fs::read(&path).unwrap(),
            fs::metadata(path).unwrap().ino(),
        )
    });
    let protected = [
        fixture.0.join(".pcl-rust/settings.json"),
        fixture.0.join(".pcl-rust/accounts.json"),
        instance.join(format!("{id}.json")),
        instance.join(format!("{id}.jar")),
        instance.join("saves/ExampleWorld/keep.txt"),
    ]
    .map(|path| {
        let bytes = fs::read(&path).ok();
        (path, bytes)
    });
    let task = state
        .tasks
        .admit(
            TaskTarget {
                root_id: first.id.clone(),
                root_path: first.path.clone(),
                instance_id: Some(id.clone()),
            },
            TaskKind::ResourceUpdate,
        )
        .unwrap();
    state.downloads.track(&task);
    let result =
        resource_update_commands::publish_batch(&state, &first, &id, batch, &task).unwrap();
    let undo_id = result["undo_id"].as_str().unwrap().to_owned();
    assert_eq!(result["changed"], 3);
    task.finish(TaskOutcome::Complete {
        result: Some(result),
        message: "fixture update complete".into(),
        error: None,
    });
    let projection = state.downloads.snapshot();
    assert_eq!(projection.kind, Some(TaskKind::ResourceUpdate));
    assert_eq!(projection.root_id.as_deref(), Some(first.id.as_str()));
    assert_eq!(projection.network_bytes, network_bytes);
    assert_eq!(projection.stage, "complete");
    assert!(!projection.can_cancel);
    assert!(instance.join("mods/new.jar").exists());
    assert!(instance.join("mods/new-disabled.jar.disabled").exists());
    assert!(!instance.join("mods/new-disabled.jar").exists());
    assert!(instance.join("resourcepacks/required.zip").exists());
    assert!(bootstrap_view(&state)
        .resource_install_recovery_error
        .is_none());

    let reply = resource_update_commands::start_restore(
        state.clone(),
        Some(&first.id),
        id.clone(),
        undo_id,
    )
    .unwrap();
    let restored = state
        .tasks
        .wait_terminal(reply["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(restored.kind, TaskKind::ResourceUpdateRestore);
    assert_eq!(
        restored.stage,
        tasks::TaskStage::Complete,
        "{:?}",
        restored.error
    );
    assert_eq!(restored.root_id, first.id);
    assert_eq!(restored.network_bytes, 0);
    assert_eq!(restored.steps.len(), 2);
    assert!(restored.steps.iter().all(|step| step.state == "complete"));
    assert!(state.tasks.active().is_none());
    assert_eq!(state.config.snapshot().root_id, second.id);
    for (name, bytes, inode) in originals {
        let path = instance.join("mods").join(name);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::metadata(path).unwrap().ino(), inode);
    }
    assert!(!instance.join("mods/new.jar").exists());
    assert!(!instance.join("mods/new-disabled.jar.disabled").exists());
    assert!(!instance.join("resourcepacks").exists());
    assert!(resource_ops::updates_history(Path::new(&first.path), &id)
        .unwrap()
        .is_empty());
    for (path, bytes) in protected {
        assert_eq!(fs::read(path).ok(), bytes);
    }
}

#[test]
fn mod_update_admission_keeps_original_root_on_pre_network_rejection() {
    let fixture = Fixture::new();
    let state = Arc::new(fixture.shared());
    let (first, id) = rename_fixture(&state);
    let second = fixture.second(&state.config);
    state.config.select(&second.id).unwrap();
    let settings = fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap();
    let reply = resource_update_commands::start_request(
        state.clone(),
        Some(&first.id),
        id.clone(),
        vec![resource_ops::ResourceFile {
            file_name: "missing.jar".into(),
            fingerprint: "stale-scan-token".into(),
        }],
        "confirmed".into(),
    )
    .unwrap();
    let task = state
        .tasks
        .wait_terminal(reply["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(task.kind, TaskKind::ResourceUpdate);
    assert_eq!(task.root_id, first.id);
    assert_eq!(task.root_path, first.path);
    assert_eq!(task.instance_id.as_deref(), Some(id.as_str()));
    assert_eq!(task.stage, tasks::TaskStage::Error);
    assert_eq!(task.network_bytes, 0);
    assert_eq!(
        state.downloads.snapshot().task_id.as_deref(),
        Some(task.id.as_str())
    );
    assert!(state.tasks.active().is_none());
    assert_eq!(state.config.snapshot().root_id, second.id);
    assert_eq!(
        settings,
        fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap()
    );
    assert!(!fixture.0.join(".pcl-rust/resource-downloads").exists());
}

#[test]
fn interrupted_mod_update_exposes_root_recovery_without_instance_json() {
    let fixture = Fixture::new();
    let state = fixture.shared();
    let (first, id) = rename_fixture(&state);
    let second = fixture.second(&state.config);
    let instance = Path::new(&first.path).join("versions").join(&id);
    let pending = instance.join(".pcl-resource-updates/unknown-record");
    fs::create_dir_all(&pending).unwrap();
    fs::write(pending.join("retain.txt"), b"external recovery evidence").unwrap();
    fs::remove_file(instance.join(format!("{id}.json"))).unwrap();
    assert!(bootstrap_view(&state)
        .resource_install_recovery_error
        .is_some());
    assert!(require_root_removable(&state, &first.id).is_err());
    assert!(resource_ops::ensure_ready(Path::new(&first.path)).is_err());
    state.config.select(&second.id).unwrap();
    assert!(bootstrap_view(&state)
        .resource_install_recovery_error
        .is_none());
    assert!(require_root_removable(&state, &second.id).is_ok());
    assert!(resource_ops::recover_verified_batches(Path::new(&first.path)).is_err());
    assert_eq!(
        fs::read(pending.join("retain.txt")).unwrap(),
        b"external recovery evidence"
    );
    assert!(state.config.resolve(Some(&first.id)).is_ok());
}

#[test]
fn prepared_mod_update_is_retired_when_isolation_changes_away_and_back_before_publication() {
    use pcl_core::content_scope::IsolationPolicy;
    let fixture = Fixture::new();
    let state = Arc::new(fixture.shared());
    let (registered, id) = rename_fixture(&state);
    let captured = state
        .config
        .isolation_settings(Some(&registered.id))
        .unwrap();
    let root = captured.root.clone();
    let batch = modrinth_install::test_update_batch(
        Path::new(&root.path),
        &state.project,
        &root.id,
        &id,
        captured.policy(&id),
        &captured.revision,
    );
    let mods = Path::new(&root.path)
        .join("versions")
        .join(&id)
        .join("mods");
    let before = fs::read(mods.join("old.jar")).unwrap();
    for policy in [IsolationPolicy::Shared, IsolationPolicy::Automatic] {
        let mut settings = state.config.snapshot();
        settings.isolation = policy;
        state.config.save(settings).unwrap();
    }
    let current = state.config.isolation_settings(Some(&root.id)).unwrap();
    assert_eq!(current.root, root);
    assert_eq!(current.policy(&id), captured.policy(&id));
    assert_ne!(current.revision, captured.revision);
    let task = state
        .tasks
        .admit(
            TaskTarget {
                root_id: root.id.clone(),
                root_path: root.path.clone(),
                instance_id: Some(id.clone()),
            },
            TaskKind::ResourceUpdate,
        )
        .unwrap();
    let error =
        resource_update_commands::publish_batch(&state, &root, &id, batch, &task).unwrap_err();
    assert!(error.contains("实例隔离设置已变化"), "{error}");
    task.finish(TaskOutcome::Error(error));
    assert_eq!(fs::read(mods.join("old.jar")).unwrap(), before);
    assert!(!mods.join("new.jar").exists());
    assert!(resource_ops::updates_history(Path::new(&root.path), &id)
        .unwrap()
        .is_empty());
    resource_ops::ensure_ready(Path::new(&root.path)).unwrap();
}
