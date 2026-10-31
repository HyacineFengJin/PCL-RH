use super::*;
use crate::{integration_tests::Fixture, tasks::TaskStage};
use pcl_install::RepairUnsupported;
use std::fs;
use std::{path::PathBuf, sync::mpsc, time::Duration};

fn instance(root: &GameRoot, id: &str) -> PathBuf {
    let path = Path::new(&root.path).join("versions").join(id);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join(format!("{id}.json")), b"{\"id\":\"fixture\"}").unwrap();
    path
}

fn report(id: &str) -> MissingRepairReport {
    MissingRepairReport {
        id: id.into(),
        files_downloaded: 2,
        files_present: 3,
        unsupported: Vec::new(),
        unsupported_total: 0,
    }
}

fn admitted(shared: &Shared, root: &GameRoot) -> (CapturedRepair, TaskHandle) {
    let _operation = shared.operations.lock().unwrap();
    let captured = capture(
        shared,
        "fixture",
        &root.id,
        &shared.config.snapshot().revision,
    )
    .unwrap();
    let task = shared
        .tasks
        .admit(
            TaskTarget {
                root_id: root.id.clone(),
                root_path: root.path.clone(),
                instance_id: Some("fixture".into()),
            },
            TaskKind::InstanceRepair,
        )
        .unwrap();
    shared.downloads.track(&task);
    (captured, task)
}

#[test]
fn submission_requires_explicit_valid_root_instance_and_current_settings() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    let revision = shared.config.snapshot().revision;
    let _operation = shared.operations.lock().unwrap();
    assert!(capture(&shared, "fixture", &root.id, "stale").is_err());
    assert!(capture(&shared, "../foreign", &root.id, &revision).is_err());
    assert!(capture(&shared, "fixture", "removed-root", &revision).is_err());
    assert!(shared.tasks.active().is_none());
}

#[test]
fn game_close_and_monitor_guards_reject_admission_without_running_a_game() {
    for guard in ["preparing", "running", "closing", "monitor"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        let revision = shared.config.snapshot().revision;
        match guard {
            "closing" => shared.closing.store(true, Ordering::SeqCst),
            "monitor" => {
                fs::create_dir_all(fixture.0.join(".pcl-rust/game-monitor")).unwrap();
                fs::write(
                    fixture.0.join(".pcl-rust/game-monitor/active.json"),
                    b"invalid fixture monitor record",
                )
                .unwrap();
            }
            stage => shared.status.lock().unwrap().stage = stage.into(),
        }
        let _operation = shared.operations.lock().unwrap();
        assert!(
            capture(&shared, "fixture", &root.id, &revision).is_err(),
            "{guard}"
        );
        assert!(shared.tasks.active().is_none());
    }
}

#[test]
fn recovery_records_block_capture_and_publication_without_recovery_mutations() {
    for before in [true, false] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        instance(&root, "fixture");
        let storage = Path::new(&root.path).join(".pcl-linux/instance-resets");
        let corrupt = || {
            fs::create_dir_all(&storage).unwrap();
            fs::write(storage.join("unknown"), b"retained fixture recovery bytes").unwrap();
        };
        if before {
            corrupt();
            let _operation = shared.operations.lock().unwrap();
            assert!(capture(
                &shared,
                "fixture",
                &root.id,
                &shared.config.snapshot().revision
            )
            .is_err());
        } else {
            let (captured, task) = admitted(&shared, &root);
            corrupt();
            assert!(verify(&shared, &captured, &task).is_err());
        }
        assert_eq!(
            fs::read(storage.join("unknown")).unwrap(),
            b"retained fixture recovery bytes"
        );
    }
}

#[test]
fn exclusive_lease_blocks_writers_and_root_removal_until_worker_cleanup() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let (_, task) = admitted(&shared, &root);
    assert!(shared
        .tasks
        .admit_queued(
            TaskTarget {
                root_id: root.id.clone(),
                root_path: root.path.clone(),
                instance_id: Some("fixture".into())
            },
            TaskKind::InstanceRepair,
            crate::tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
        )
        .is_err());
    assert!(shared
        .tasks
        .admit(
            TaskTarget {
                root_id: "other".into(),
                root_path: fixture.0.display().to_string(),
                instance_id: None
            },
            TaskKind::ResourceOperation,
        )
        .is_err());
    assert!(crate::root_removal::require_root_removable(&shared, &root.id).is_err());
    shared.tasks.cancel(task.id()).unwrap();
    assert!(shared.tasks.blocks_path(Path::new(&root.path)).unwrap());
    assert!(crate::root_removal::require_root_removable(&shared, &root.id).is_err());
    finish(&task, Err(crate::tasks::CANCELLED.into()));
    assert!(crate::root_removal::require_root_removable(&shared, &root.id).is_ok());
}

#[test]
fn slow_worker_releases_operation_and_settings_locks_and_preserves_configuration() {
    let fixture = Fixture::new();
    let shared = Arc::new(fixture.shared());
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let account = fixture.0.join(".pcl-rust/accounts.json");
    fs::create_dir_all(account.parent().unwrap()).unwrap();
    let account_bytes = b"opaque fixture account bytes must never be read for repair";
    fs::write(&account, account_bytes).unwrap();
    let before = shared.config.snapshot();
    let preferences = shared.launcher_preferences.snapshot();
    let (entered, await_entered) = mpsc::channel();
    let (release, await_release) = mpsc::channel();
    let task_id = start_with(
        shared.clone(),
        "fixture",
        &root.id,
        &before.revision,
        move |shared, captured, _| {
            assert!(shared.operations.try_lock().is_ok());
            assert_eq!(
                shared.config.snapshot().revision,
                captured.settings_revision
            );
            entered.send(()).unwrap();
            await_release.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(report(&captured.id))
        },
    )
    .unwrap();
    await_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(shared.operations.try_lock().is_ok());
    assert_eq!(shared.config.snapshot().revision, before.revision);
    assert_eq!(
        shared.launcher_preferences.snapshot().revision,
        preferences.revision
    );
    release.send(()).unwrap();
    let terminal = shared.tasks.wait_terminal(&task_id).unwrap();
    assert_eq!(terminal.kind, TaskKind::InstanceRepair);
    assert_eq!(terminal.stage, TaskStage::Complete);
    assert_eq!(terminal.root_id, root.id);
    assert_eq!(terminal.instance_id.as_deref(), Some("fixture"));
    assert_eq!(shared.config.snapshot().revision, before.revision);
    assert_eq!(fs::read(account).unwrap(), account_bytes);
}

#[test]
fn submitted_instance_uses_its_explicit_root_without_changing_the_selected_instance() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let first = shared.config.resolve(None).unwrap();
    let second_path = fixture.0.join("second");
    fs::create_dir_all(&second_path).unwrap();
    let second = shared
        .config
        .register(second_path.to_str().unwrap().into(), None)
        .unwrap();
    shared.config.select(&first.id).unwrap();
    let mut settings = shared.config.snapshot();
    settings.selected = Some("selected".into());
    shared.config.save(settings).unwrap();
    instance(&second, "fixture");
    let (captured, task) = admitted(&shared, &second);
    let result = execute_with(&shared, &captured, &task, |shared, captured, _| {
        assert_eq!(captured.root.id, second.id);
        assert_eq!(shared.config.snapshot().root_id, first.id);
        assert_eq!(
            shared.config.snapshot().selected.as_deref(),
            Some("selected")
        );
        Ok(report(&captured.id))
    })
    .unwrap();
    finish(&task, Ok(result));
    assert_eq!(shared.config.snapshot().root_id, first.id);
    assert_eq!(
        shared.config.snapshot().selected.as_deref(),
        Some("selected")
    );
}

#[test]
fn settings_and_root_switches_invalidate_captured_target_instead_of_following_selection() {
    for switch_root in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let first = shared.config.resolve(None).unwrap();
        instance(&first, "fixture");
        let second_path = fixture.0.join("second");
        fs::create_dir_all(&second_path).unwrap();
        let second = shared
            .config
            .register(second_path.to_str().unwrap().into(), None)
            .unwrap();
        let (captured, task) = admitted(&shared, &first);
        let result = execute_with(&shared, &captured, &task, |shared, _, _| {
            if switch_root {
                shared.config.select(&second.id).unwrap();
            } else {
                let mut settings = shared.config.snapshot();
                settings.selected = Some("other".into());
                shared.config.save(settings).unwrap();
            }
            Ok(report("fixture"))
        });
        assert!(matches!(result, Err(error) if error == CHANGED));
        assert_eq!(captured.root.id, first.id);
        assert_eq!(fs::read_dir(second_path).unwrap().count(), 0);
    }
}

#[test]
fn external_config_changes_are_rejected_by_full_binding_check() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let (captured, task) = admitted(&shared, &root);
    fs::create_dir_all(fixture.0.join(".pcl-rust")).unwrap();
    let bytes = b"external fixture settings";
    fs::write(fixture.0.join(".pcl-rust/settings.json"), bytes).unwrap();
    assert!(verify_fast(&shared, &captured, &task).is_ok());
    assert!(verify(&shared, &captured, &task).is_err());
    assert_eq!(
        fs::read(fixture.0.join(".pcl-rust/settings.json")).unwrap(),
        bytes
    );
}

#[test]
fn root_directory_replacement_is_rejected_with_original_descriptor_still_owned() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let (captured, task) = admitted(&shared, &root);
    let moved = Path::new(&root.path).with_file_name("retained-fixture-root");
    fs::rename(&root.path, &moved).unwrap();
    fs::create_dir(&root.path).unwrap();
    assert_eq!(
        RootIdentity::of(&captured.directory.metadata().unwrap()),
        captured.identity
    );
    assert!(matches!(verify(&shared, &captured, &task), Err(error) if error == CHANGED));
    assert_eq!(fs::read_dir(&root.path).unwrap().count(), 0);
    assert!(moved.join("versions/fixture/fixture.json").exists());
}

#[test]
fn transient_close_generation_and_run_changes_invalidate_worker() {
    for guard in ["generation", "running", "closing"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        instance(&root, "fixture");
        let (captured, task) = admitted(&shared, &root);
        match guard {
            "generation" => {
                shared.close_generation.fetch_add(2, Ordering::SeqCst);
            }
            "running" => shared.status.lock().unwrap().stage = "running".into(),
            _ => shared.closing.store(true, Ordering::SeqCst),
        }
        assert!(verify_fast(&shared, &captured, &task).is_err());
        assert!(verify(&shared, &captured, &task).is_err());
    }
}

#[test]
fn unsupported_items_keep_an_error_card_and_structured_result() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let (_, task) = admitted(&shared, &root);
    let mut result = report("fixture");
    result.unsupported_total = 9;
    result.unsupported.push(RepairUnsupported {
        path: "libraries/custom.jar".into(),
        reason: "missing official SHA1 and size".into(),
    });
    finish(&task, Ok(result));
    let wire = shared.downloads.snapshot_for(task.id()).unwrap();
    assert_eq!(wire.stage, "error");
    assert!(wire.message.contains("已补全 2 个文件"));
    assert!(wire.error.unwrap().contains("9 项无法自动补全"));
    let result = wire.result.unwrap();
    assert_eq!(result["id"], "fixture");
    assert_eq!(result["unsupported_total"], 9);
    assert_eq!(result["unsupported"][0]["path"], "libraries/custom.jar");
    assert!(!wire.can_cancel);
}

#[test]
fn cancellation_cleanup_failure_and_other_error_remain_distinct_and_keep_completed_files() {
    for (error, expected) in [
        ("安装已取消", TaskStage::Cancelled),
        ("取消清理失败：fixture cleanup failure", TaskStage::Error),
        ("fixture checksum mismatch", TaskStage::Error),
    ] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        let path = instance(&root, "fixture");
        let (_, task) = admitted(&shared, &root);
        let completed = path.join("fixture.jar");
        fs::write(&completed, b"verified fixture completed bytes").unwrap();
        shared.tasks.cancel(task.id()).unwrap();
        finish(&task, Err(error.into()));
        let terminal = shared.tasks.snapshot(task.id()).unwrap();
        assert_eq!(terminal.stage, expected);
        assert_eq!(
            fs::read(completed).unwrap(),
            b"verified fixture completed bytes"
        );
        if expected == TaskStage::Cancelled {
            assert!(terminal.message.contains("已完成文件保留"));
            assert!(terminal.error.is_none());
        } else {
            assert_eq!(terminal.error.as_deref(), Some(error));
        }
    }
}

#[test]
fn worker_panic_releases_lease_and_foreign_result_cannot_be_published() {
    let fixture = Fixture::new();
    let shared = Arc::new(fixture.shared());
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let revision = shared.config.snapshot().revision;
    let id = start_with(shared.clone(), "fixture", &root.id, &revision, |_, _, _| {
        panic!("fixture worker panic")
    })
    .unwrap();
    assert_eq!(
        shared.tasks.wait_terminal(&id).unwrap().stage,
        TaskStage::Error
    );
    assert!(shared.tasks.active().is_none());
    let (captured, task) = admitted(&shared, &root);
    assert!(execute_with(&shared, &captured, &task, |_, _, _| Ok(report(
        "foreign-instance"
    )))
    .is_err());
}

#[test]
fn repair_progress_uses_download_projection_without_claiming_install_completion() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    instance(&root, "fixture");
    let (_, task) = admitted(&shared, &root);
    let progress = |stage: &str| pcl_install::Progress {
        steps: vec![],
        stage: stage.into(),
        message: "fixture progress".into(),
        completed: 1,
        total: 2,
        bytes_done: 4,
        bytes_total: 8,
        network_bytes: 4,
    };
    shared
        .downloads
        .progress(&task, progress("repair-download"));
    assert_eq!(shared.downloads.snapshot().stage, "downloading");
    shared.downloads.progress(&task, progress("repair-verify"));
    assert_eq!(shared.downloads.snapshot().stage, "processing");
    shared.downloads.progress(&task, progress("complete"));
    let projected = shared.downloads.snapshot();
    assert_eq!(projected.stage, "processing");
    assert_eq!(projected.phase, "repair-verify");
    assert!(projected.message.contains("补全结果"));
}

#[test]
fn production_service_checks_existing_fixture_without_network_or_java() {
    let fixture = Fixture::new();
    let shared = Arc::new(fixture.shared());
    let root = shared.config.resolve(None).unwrap();
    let path = instance(&root, "fixture");
    let retained = b"existing custom fixture client bytes are not verified or replaced";
    fs::write(path.join("fixture.jar"), retained).unwrap();
    let before = shared.config.snapshot();
    let id = start(shared.clone(), "fixture", &root.id, &before.revision).unwrap();
    let terminal = shared.tasks.wait_terminal(&id).unwrap();
    assert_eq!(terminal.stage, TaskStage::Complete, "{:?}", terminal.error);
    let result = terminal.result.unwrap();
    assert_eq!(result["files_downloaded"], 0);
    assert_eq!(result["files_present"], 1);
    assert_eq!(result["unsupported_total"], 0);
    assert_eq!(fs::read(path.join("fixture.jar")).unwrap(), retained);
    assert_eq!(shared.config.snapshot().revision, before.revision);
    assert!(!path.join("natives-linux").exists());
}
