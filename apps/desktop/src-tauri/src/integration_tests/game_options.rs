//! Game-option admission tests use only private synthetic roots. They exercise
//! the command's real save helper under the same admission/reference guards;
//! no Tauri window, Java, game, chooser or user configuration is involved.
use super::*;
use pcl_core::content_scope::IsolationPolicy;

fn save(shared: &Shared, settings: Settings) -> Result<Settings, String> {
    let _operation = shared.operations.lock().unwrap();
    require_reference_write(shared)?;
    save_game_settings(shared, settings)
}

fn persisted(shared: &Shared) -> Settings {
    save(shared, shared.config.snapshot()).unwrap()
}

fn assert_rejected(shared: &Shared, next: Settings) {
    assert_rejected_for(shared, next, "busy or stale context");
}

fn assert_rejected_for(shared: &Shared, next: Settings, context: &str) {
    let before = shared.config.snapshot();
    let path = shared.project.join(".pcl-rust/settings.json");
    let bytes = fs::read(&path).unwrap();
    assert!(save(shared, next).is_err(), "save admitted for {context}");
    assert_eq!(shared.config.snapshot(), before);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

fn changed(mut settings: Settings, global: bool) -> Settings {
    if global {
        settings.isolation = IsolationPolicy::Isolated;
    } else {
        settings
            .isolation_overrides
            .insert("Example".into(), IsolationPolicy::Shared);
    }
    settings
}

#[test]
fn global_and_instance_isolation_changes_reject_preparing_running_and_close() {
    for stage in ["preparing", "running", "closing"] {
        for global in [false, true] {
            let fixture = Fixture::new();
            let shared = fixture.shared();
            let before = persisted(&shared);
            if stage == "closing" {
                shared.closing.store(true, Ordering::SeqCst);
            } else {
                shared.status.lock().unwrap().stage = stage.into();
            }
            assert_rejected(&shared, changed(before, global));
        }
    }
}

#[test]
fn isolation_changes_reject_each_existing_file_task_and_succeed_after_release() {
    for kind in [
        TaskKind::Install,
        TaskKind::ModpackInstall,
        TaskKind::ModpackPrepare,
        TaskKind::JavaInstall,
        TaskKind::InstanceRepair,
        TaskKind::InstanceReset,
        TaskKind::InstanceExport,
        TaskKind::InstanceRename,
        TaskKind::InstanceImport,
        TaskKind::InstanceDelete,
        TaskKind::InstanceRestore,
        TaskKind::ResourceDownload,
        TaskKind::ResourceSave,
        TaskKind::ResourceUpdate,
        TaskKind::ResourceUpdateRestore,
        TaskKind::ResourceOperation,
    ] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let before = persisted(&shared);
        let root = shared.config.resolve(None).unwrap();
        let task = shared
            .tasks
            .admit(
                TaskTarget {
                    root_id: root.id,
                    root_path: root.path,
                    instance_id: Some("Example".into()),
                },
                kind,
            )
            .unwrap();
        assert_rejected(&shared, changed(before.clone(), true));
        assert_rejected(&shared, changed(before, false));
        task.finish(TaskOutcome::Failed("fixture released".into()));
        save(&shared, changed(shared.config.snapshot(), true)).unwrap();
    }
}

#[test]
fn queued_and_cancelled_other_root_workers_block_isolation_until_cleanup_finishes() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let other = fixture.second(&shared.config);
    let before = persisted(&shared);
    let target = || TaskTarget {
        root_id: other.id.clone(),
        root_path: other.path.clone(),
        instance_id: Some("Same".into()),
    };
    let active = shared
        .tasks
        .admit_queued(
            target(),
            TaskKind::Install,
            tasks::TaskScope::root(Path::new(&other.path)).unwrap(),
        )
        .unwrap();
    let queued = shared
        .tasks
        .admit_queued(
            target(),
            TaskKind::ResourceDownload,
            tasks::TaskScope::root(Path::new(&other.path)).unwrap(),
        )
        .unwrap();
    assert_eq!(
        shared.tasks.snapshot(queued.id()).unwrap().stage,
        tasks::TaskStage::Queued
    );
    assert_rejected(&shared, changed(before.clone(), true));
    shared.tasks.cancel(queued.id()).unwrap();
    assert_rejected(&shared, changed(before, false));
    queued.finish(TaskOutcome::Failed("queued cleanup acknowledged".into()));
    assert_rejected(&shared, changed(shared.config.snapshot(), true));
    active.finish(TaskOutcome::Failed("active cleanup acknowledged".into()));
    save(&shared, changed(shared.config.snapshot(), false)).unwrap();
}

#[test]
fn title_and_memory_saves_keep_ordinary_behavior_during_game_and_file_work() {
    for stage in ["preparing", "running", "file-task"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let before = persisted(&shared);
        let root = shared.config.resolve(None).unwrap();
        let task = if stage == "file-task" {
            Some(
                shared
                    .tasks
                    .admit(
                        TaskTarget {
                            root_id: root.id,
                            root_path: root.path,
                            instance_id: Some("Example".into()),
                        },
                        TaskKind::ResourceOperation,
                    )
                    .unwrap(),
            )
        } else {
            shared.status.lock().unwrap().stage = stage.into();
            None
        };
        let mut next = before.clone();
        next.memory_gib = 10;
        next.window_title_overrides
            .insert("Example".into(), String::new());
        let saved = save(&shared, next).unwrap();
        assert_eq!(saved.memory_gib, 10);
        assert_eq!(
            saved.window_title_overrides.get("Example"),
            Some(&String::new())
        );
        assert_eq!(saved.isolation, before.isolation);
        assert_eq!(saved.isolation_overrides, before.isolation_overrides);
        if let Some(task) = task {
            task.finish(TaskOutcome::Failed("fixture released".into()));
        }
    }
}

#[test]
fn every_root_recovery_namespace_blocks_isolation_without_mutating_its_evidence() {
    for namespace in [
        ".pcl-linux/instance-renames",
        ".pcl-linux/instance-imports",
        ".pcl-rust/instance-deletions",
        ".pcl-linux/instance-resets",
        ".pcl-linux/resource-operations",
        ".pcl-linux/resource-batches",
        ".pcl-linux/resource-update-pending",
    ] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let other = fixture.second(&shared.config);
        let before = persisted(&shared);
        assert_ne!(before.root_id, other.id);
        let pending = Path::new(&other.path)
            .join(namespace)
            .join("unknown-record");
        fs::create_dir_all(&pending).unwrap();
        let evidence = pending.join("retain.txt");
        fs::write(&evidence, b"unknown recovery evidence").unwrap();
        assert_rejected_for(&shared, changed(before.clone(), true), namespace);
        assert_rejected_for(&shared, changed(before, false), namespace);
        assert_eq!(fs::read(evidence).unwrap(), b"unknown recovery evidence");
    }
}

#[test]
fn global_isolation_change_preserves_an_offline_roots_identity_and_overrides() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let first = shared.config.snapshot().root_id;
    let other = fixture.second(&shared.config);
    let mut settings = shared.config.select(&other.id).unwrap();
    settings
        .isolation_overrides
        .insert("Same".into(), IsolationPolicy::Shared);
    settings
        .window_title_overrides
        .insert("Same".into(), "Offline title".into());
    save(&shared, settings).unwrap();
    let retained = shared.config.registered(&other.id).unwrap();
    shared.config.select(&first).unwrap();
    fs::remove_dir(&other.path).unwrap();
    let updated = save(&shared, changed(shared.config.snapshot(), true)).unwrap();
    assert_eq!(updated.isolation, IsolationPolicy::Isolated);
    assert_eq!(shared.config.registered(&other.id).unwrap(), retained);
    assert!(!Path::new(&other.path).exists());
    assert!(
        !shared
            .config
            .roots()
            .iter()
            .find(|root| root.id == other.id)
            .unwrap()
            .available
    );
}

#[test]
fn project_recovery_and_unknown_monitor_markers_block_isolation_and_remain_intact() {
    for relative in [
        "instance-rename-pending.json",
        "instance-delete-pending.json",
        "game-monitor/active.json",
    ] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let before = persisted(&shared);
        let marker = fixture.0.join(".pcl-rust").join(relative);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let bytes = br#"{"schema_version":999,"retain":"unknown recovery"}"#;
        fs::write(&marker, bytes).unwrap();
        assert_rejected(&shared, changed(before, true));
        assert_eq!(fs::read(marker).unwrap(), bytes);
    }
}

#[test]
fn captured_options_use_the_explicit_root_for_same_named_instances_and_retain_their_title() {
    use pcl_core::launch_options::{IpPreference, WindowSize};
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let first = shared.config.snapshot().root_id;
    let second = fixture.second(&shared.config);
    let mut settings = shared.config.snapshot();
    settings.isolation = IsolationPolicy::Isolated;
    settings
        .isolation_overrides
        .insert("Same".into(), IsolationPolicy::Shared);
    settings
        .window_title_overrides
        .insert("Same".into(), "第一处游戏".into());
    save(&shared, settings).unwrap();
    let mut settings = shared.config.select(&second.id).unwrap();
    settings
        .window_title_overrides
        .insert("Same".into(), String::new());
    let settings = save(&shared, settings).unwrap();
    let view = shared.launcher_preferences.snapshot();
    shared
        .launcher_preferences
        .update(
            &view.revision,
            serde_json::from_value(serde_json::json!({
                "game_launch": {
                    "window_title": "Global title",
                    "window": {"mode": "custom", "width": 1280, "height": 720},
                    "ip": "ipv6"
                }
            }))
            .unwrap(),
        )
        .unwrap();
    let preferences = captured_launch_preferences(&shared).unwrap();
    let first_root = shared.config.resolve(Some(&first)).unwrap();
    let second_root = shared.config.resolve(Some(&second.id)).unwrap();
    let first_options =
        captured_game_options(&settings, &first_root, &preferences, "Same").unwrap();
    let second_options =
        captured_game_options(&settings, &second_root, &preferences, "Same").unwrap();
    assert_eq!(settings.root_id, second.id);
    assert_eq!(first_options.window_title, "第一处游戏");
    assert_eq!(first_options.isolation, IsolationPolicy::Shared);
    assert!(second_options.window_title.is_empty());
    assert_eq!(second_options.isolation, IsolationPolicy::Isolated);
    assert_eq!(first_options.ip, IpPreference::Ipv6);
    assert_eq!(
        first_options.window,
        WindowSize::Custom {
            width: 1280,
            height: 720
        }
    );

    let mut settings = shared.config.snapshot();
    settings.window_title_overrides.remove("Same");
    let settings = save(&shared, settings).unwrap();
    let inherited = captured_game_options(
        &settings,
        &shared.config.resolve(Some(&second.id)).unwrap(),
        &preferences,
        "Same",
    )
    .unwrap();
    assert_eq!(inherited.window_title, "Global title");
    let view = shared.launcher_preferences.snapshot();
    shared
        .launcher_preferences
        .update(
            &view.revision,
            serde_json::from_value(
                serde_json::json!({"game_launch":{"window_title":"Next global"}}),
            )
            .unwrap(),
        )
        .unwrap();
    let later = captured_game_options(
        &settings,
        &shared.config.resolve(Some(&second.id)).unwrap(),
        &captured_launch_preferences(&shared).unwrap(),
        "Same",
    )
    .unwrap();
    assert_eq!(later.window_title, "Next global");
    assert_eq!(inherited.window_title, "Global title");
    assert_eq!(first_options.window_title, "第一处游戏");
    assert!(second_options.window_title.is_empty());
    assert_eq!(
        shared
            .config
            .resolve(Some(&first))
            .unwrap()
            .window_title_overrides["Same"],
        "第一处游戏"
    );
}

#[test]
fn resource_chooser_revision_stays_retired_after_isolation_changes_away_and_back() {
    for global in [false, true] {
        let fixture = Fixture::new();
        let shared = Arc::new(fixture.shared());
        let original = persisted(&shared);
        let root = shared.config.resolve(None).unwrap();
        let intermediate = save(&shared, changed(original.clone(), global)).unwrap();
        let mut returned = original.clone();
        returned.revision = intermediate.revision;
        let returned = save(&shared, returned).unwrap();
        assert_eq!(returned.isolation, original.isolation);
        assert_eq!(returned.isolation_overrides, original.isolation_overrides);
        assert_ne!(returned.revision, original.revision);
        let before = fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap();
        let error = admit_resource(
            &shared,
            "Example",
            Some(&root.id),
            Some(&root.path),
            Some(&original.revision),
        )
        .err()
        .expect("old chooser must not revive when the directory policy returns");
        assert!(error.contains("实例设置已变化"), "{error}");
        assert!(shared.tasks.active_all().is_empty());
        assert_eq!(
            fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap(),
            before
        );
        assert!(!Path::new(&root.path).join(".pcl-linux").exists());

        let (captured, task) = admit_resource(
            &shared,
            "Example",
            Some(&root.id),
            Some(&root.path),
            Some(&returned.revision),
        )
        .unwrap();
        assert_eq!(captured.revision, returned.revision);
        assert_eq!(captured.policy("Example"), IsolationPolicy::Automatic);
        task.finish(TaskOutcome::Failed("fixture released".into()));
    }
}
