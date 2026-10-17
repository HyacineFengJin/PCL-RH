use super::*;
use crate::{
    integration_tests::Fixture,
    tasks::{TaskKind, TaskOutcome, TaskTarget},
};
use std::{collections::BTreeMap, sync::Arc};

fn instance(root: &GameRoot, id: &str) -> PathBuf {
    let path = Path::new(&root.path).join("versions").join(id);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join(format!("{id}.json")), b"{\"id\":\"fixture\"}").unwrap();
    path
}

fn report(captured: &CapturedInspection) -> LaunchInspection {
    LaunchInspection {
        id: captured.id.clone(),
        minecraft_version: "1.21.1".into(),
        loader: "Vanilla".into(),
        game_dir: captured.root.path.clone(),
        java_major: 21,
        java_exact: false,
        java_path: None,
        java_error: Some("fixture: no compatible Java".into()),
        missing_files: vec!["versions/fixture/fixture.jar".into()],
        missing_total: 1,
        issues: vec![],
        ready: false,
    }
}

fn tree(path: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(path: &Path, root: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        if metadata.is_dir() {
            entries.insert(path.strip_prefix(root).unwrap().to_owned(), None);
            for entry in fs::read_dir(path).unwrap() {
                visit(&entry.unwrap().path(), root, entries);
            }
        } else {
            assert!(metadata.is_file(), "fixture contains an unexpected carrier");
            entries.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                Some(fs::read(path).unwrap()),
            );
        }
    }
    let mut entries = BTreeMap::new();
    visit(path, path, &mut entries);
    entries
}

#[test]
fn read_only_report_preserves_files_and_releases_all_admission_and_settings_locks() {
    let fixture = Fixture::new();
    let shared = Arc::new(fixture.shared());
    let root = shared.config.resolve(None).unwrap();
    let path = instance(&root, "fixture");
    fs::create_dir_all(fixture.0.join(".pcl-rust")).unwrap();
    fs::write(
        fixture.0.join(".pcl-rust/accounts.json"),
        b"fixture account bytes are not consulted",
    )
    .unwrap();
    let before = tree(&fixture.0);
    let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
        assert!(shared.operations.try_lock().is_ok());
        let snapshot_shared = shared.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let settings = snapshot_shared.config.snapshot();
            let preferences = snapshot_shared.launcher_preferences.snapshot();
            send.send((settings.revision, preferences.revision))
                .unwrap();
        });
        let revisions = receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        reader.join().unwrap();
        assert_eq!(revisions.0, captured.settings_revision);
        assert_eq!(revisions.1, captured.preferences_revision);
        Ok(report(captured))
    })
    .unwrap();
    let wire = serde_json::to_value(result).unwrap();
    assert_eq!(wire["root_id"], root.id);
    assert_eq!(wire["id"], "fixture");
    assert_eq!(wire["java_major"], 21);
    assert_eq!(wire["java_error"], "fixture: no compatible Java");
    assert_eq!(wire["missing_total"], 1);
    assert!(wire.get("report").is_none());
    assert!(wire.get("arguments").is_none());
    assert!(!path.join("natives-linux").exists());
    assert_eq!(tree(&fixture.0), before);
}

#[test]
fn explicit_target_does_not_use_the_current_instance_selection() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "submitted");
    instance(&root, "selected");
    let mut settings = shared.config.snapshot();
    settings.selected = Some("selected".into());
    shared.config.save(settings).unwrap();
    let result = inspect_with(&shared, "submitted", Some(&root.id), |captured| {
        assert_eq!(captured.id, "submitted");
        assert_eq!(captured.root.selected.as_deref(), Some("selected"));
        Ok(report(captured))
    })
    .unwrap();
    assert_eq!(result.report.id, "submitted");
}

#[test]
fn switching_root_or_instance_selection_during_probe_rejects_the_old_result() {
    for switch_root in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        instance(&root, "fixture");
        let second_path = fixture.0.join("second");
        fs::create_dir_all(&second_path).unwrap();
        let second = shared
            .config
            .register(second_path.to_str().unwrap().into(), None)
            .unwrap();
        let result = inspect_with(&shared, "fixture", None, |captured| {
            assert_eq!(captured.root.id, root.id);
            if switch_root {
                shared.config.select(&second.id).unwrap();
            } else {
                let mut settings = shared.config.snapshot();
                settings.selected = Some("other".into());
                shared.config.save(settings).unwrap();
            }
            Ok(report(captured))
        });
        assert!(matches!(result, Err(error) if error == CHANGED));
    }
}

#[test]
fn launcher_preference_change_during_probe_rejects_the_old_result() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
        shared
            .launcher_preferences
            .update(
                &captured.preferences_revision,
                serde_json::from_value(serde_json::json!({
                    "game_launch": {"ip": "ipv4"}
                }))
                .unwrap(),
            )
            .unwrap();
        Ok(report(captured))
    });
    assert!(matches!(result, Err(error) if error == CHANGED));
}

#[test]
fn replacing_root_versions_or_instance_directory_during_probe_is_rejected() {
    for component in ["root", "versions", "instance"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        let instance_path = instance(&root, "fixture");
        let path = match component {
            "root" => PathBuf::from(&root.path),
            "versions" => instance_path.parent().unwrap().to_owned(),
            _ => instance_path,
        };
        let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
            let moved = path.with_file_name(format!("{component}-retained"));
            fs::rename(&path, moved).unwrap();
            fs::create_dir(&path).unwrap();
            Ok(report(captured))
        });
        assert!(matches!(result, Err(error) if error == CHANGED));
    }
}

#[test]
fn missing_instance_carrier_cannot_appear_during_probe() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    fs::create_dir(Path::new(&root.path).join("versions")).unwrap();
    let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
        instance(&root, "fixture");
        Ok(report(captured))
    });
    assert!(matches!(result, Err(error) if error == CHANGED));
}

#[test]
fn completed_writer_during_probe_invalidates_a_report_even_after_it_becomes_idle() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
        let _operation = shared.operations.lock().unwrap();
        let task = shared
            .tasks
            .admit(
                TaskTarget {
                    root_id: root.id.clone(),
                    root_path: root.path.clone(),
                    instance_id: Some("fixture".into()),
                },
                TaskKind::ResourceOperation,
            )
            .unwrap();
        task.finish(TaskOutcome::Complete {
            result: None,
            message: "complete".into(),
            error: None,
        });
        assert!(shared.tasks.active().is_none());
        Ok(report(captured))
    });
    assert!(matches!(result, Err(error) if error == CHANGED));
}

#[test]
fn game_and_closing_guards_are_checked_before_and_after_the_probe() {
    for guard in ["preparing", "running", "closing"] {
        for before in [true, false] {
            let fixture = Fixture::new();
            let shared = fixture.shared();
            let root = shared.config.resolve(None).unwrap();
            instance(&root, "fixture");
            let set_guard = || {
                if guard == "closing" {
                    shared.closing.store(true, Ordering::SeqCst);
                } else {
                    shared.status.lock().unwrap().stage = guard.into();
                }
            };
            if before {
                set_guard();
            }
            let mut called = false;
            let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
                called = true;
                if !before {
                    set_guard();
                }
                Ok(report(captured))
            });
            assert!(result.is_err());
            assert_eq!(called, !before);
        }
    }
}

#[test]
fn active_writer_and_unknown_recovery_record_block_probe_admission() {
    for recovery in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        instance(&root, "fixture");
        let task = if recovery {
            let pending = Path::new(&root.path).join(".pcl-linux/instance-resets/unknown");
            fs::create_dir_all(&pending).unwrap();
            fs::write(pending.join("retained.txt"), b"unknown recovery data").unwrap();
            None
        } else {
            Some(
                shared
                    .tasks
                    .admit(
                        TaskTarget {
                            root_id: root.id.clone(),
                            root_path: root.path.clone(),
                            instance_id: Some("fixture".into()),
                        },
                        TaskKind::ResourceOperation,
                    )
                    .unwrap(),
            )
        };
        let before = tree(&fixture.0);
        let result = inspect_with(&shared, "fixture", Some(&root.id), |_| {
            panic!("blocked diagnostic must not probe Java")
        });
        assert!(result.is_err());
        assert_eq!(tree(&fixture.0), before);
        if let Some(task) = task {
            task.finish(TaskOutcome::Failed("fixture cleanup".into()));
        }
    }
}

#[test]
fn stale_context_is_rejected_even_when_the_core_probe_failed() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let result = inspect_with(&shared, "fixture", Some(&root.id), |_| {
        let mut settings = shared.config.snapshot();
        settings.memory_gib += 1;
        shared.config.save(settings).unwrap();
        Err("obsolete synthetic probe error".into())
    });
    assert!(matches!(result, Err(error) if error == CHANGED));
}

#[test]
fn outside_settings_edits_are_rejected_before_and_after_probe_without_replacing_bytes() {
    for before in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        instance(&root, "fixture");
        shared.config.save(shared.config.snapshot()).unwrap();
        let settings_path = fixture.0.join(".pcl-rust/settings.json");
        let outside_bytes = b"outside settings edits must be retained";
        if before {
            fs::write(&settings_path, outside_bytes).unwrap();
        }
        let mut called = false;
        let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
            called = true;
            fs::write(&settings_path, outside_bytes).unwrap();
            Ok(report(captured))
        });
        assert!(matches!(result, Err(error) if error.contains("外部修改")));
        assert_eq!(called, !before);
        assert_eq!(fs::read(settings_path).unwrap(), outside_bytes);
    }
}

#[test]
fn newly_pending_recovery_during_probe_blocks_publication_and_retains_files() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let pending = Path::new(&root.path).join(".pcl-linux/instance-resets/unknown");
    let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("retained.txt"), b"unknown recovery data").unwrap();
        Ok(report(captured))
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read(pending.join("retained.txt")).unwrap(),
        b"unknown recovery data"
    );
}

#[test]
fn mismatched_core_instance_is_not_published() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let result = inspect_with(&shared, "fixture", Some(&root.id), |captured| {
        let mut report = report(captured);
        report.id = "different".into();
        Ok(report)
    });
    assert!(matches!(result, Err(error) if error.contains("其他实例")));
}

#[test]
fn inspection_captures_the_submitted_roots_arguments_and_retires_changed_settings() {
    use pcl_core::launch_options::LaunchArgumentOverrides;
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let first = shared.config.resolve(None).unwrap();
    instance(&first, "Same");
    let mut settings = shared.config.snapshot();
    settings.argument_overrides.insert(
        "Same".into(),
        LaunchArgumentOverrides {
            custom_info: Some("First".into()),
            jvm_args: Some(String::new()),
            game_args: None,
        },
    );
    shared.config.save(settings).unwrap();
    let second_path = fixture.0.join("argument-second");
    fs::create_dir(&second_path).unwrap();
    let second = shared
        .config
        .register(second_path.to_string_lossy().into_owned(), None)
        .unwrap();
    shared.config.select(&second.id).unwrap();
    let preferences = shared.launcher_preferences.snapshot();
    shared.launcher_preferences.update(&preferences.revision, serde_json::from_value(serde_json::json!({
        "game_launch": {"custom_info":"Global", "jvm_args":"-Dexample=global", "game_args":"--demo"}
    })).unwrap()).unwrap();
    let result = inspect_with(&shared, "Same", Some(&first.id), |captured| {
        assert_eq!(captured.root.id, first.id);
        assert_eq!(captured.game_options.custom_info, "First");
        assert!(captured.game_options.jvm_args.is_empty());
        assert_eq!(captured.game_options.game_args, "--demo");
        let mut changed = shared.config.snapshot();
        changed.argument_overrides.insert(
            "Same".into(),
            LaunchArgumentOverrides {
                game_args: Some(String::new()),
                ..Default::default()
            },
        );
        shared.config.save(changed).unwrap();
        Ok(report(captured))
    });
    assert!(matches!(result, Err(error) if error == CHANGED));
}

#[test]
fn invalid_global_arguments_reject_inspection_before_probe_and_keep_the_document() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let preferences = shared.launcher_preferences.snapshot();
    shared
        .launcher_preferences
        .update(
            &preferences.revision,
            serde_json::from_value(serde_json::json!({
                "game_launch": {"jvm_args":"-Dexample=valid"}
            }))
            .unwrap(),
        )
        .unwrap();
    let file = shared.project.join(".pcl-rust/launcher-preferences.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    document["preferences"]["game_launch"]["jvm_args"] =
        serde_json::json!("-Dprivate='private-value");
    let bytes = serde_json::to_vec(&document).unwrap();
    fs::write(&file, &bytes).unwrap();
    assert!(shared.launcher_preferences.reload().warning.is_some());
    let error = inspect_with(&shared, "fixture", Some(&root.id), |_| {
        panic!("invalid arguments reached Java probing")
    })
    .err()
    .unwrap();
    assert!(!error.contains("private-value"));
    assert_eq!(fs::read(file).unwrap(), bytes);
}
