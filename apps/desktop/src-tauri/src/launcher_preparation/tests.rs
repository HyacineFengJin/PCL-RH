use super::*;
use crate::{integration_tests::Fixture, tasks::TaskStage, RunStatus};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize},
    mpsc, Arc,
};
use std::time::Duration;

pub(super) fn fixture(missing: bool, supported: bool) -> (Fixture, Arc<Shared>, PreparedLaunch) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../work/launch-readiness-2026-10-18/preparation-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(path.join("Minecraft/.minecraft")).unwrap();
    let fixture = Fixture(path.canonicalize().unwrap());
    let shared = Arc::new(fixture.shared());
    let root = shared.config.resolve(None).unwrap();
    let version = Path::new(&root.path).join("versions/fixture");
    fs::create_dir_all(&version).unwrap();
    let mut data = json!({"id":"fixture", "mainClass":"Main", "libraries":[]});
    if missing {
        let artifact = if supported {
            json!({"path":"fixture/file.jar", "url":"https://libraries.minecraft.net/fixture/file.jar", "sha1":"0000000000000000000000000000000000000000", "size":7})
        } else {
            json!({"path":"fixture/file.jar"})
        };
        data["libraries"] = json!([{"name":"fixture:file:1", "downloads":{"artifact":artifact}}]);
        fs::create_dir_all(Path::new(&root.path).join("libraries/fixture")).unwrap();
    }
    fs::write(version.join("fixture.json"), data.to_string()).unwrap();
    fs::write(version.join("fixture.jar"), b"fixture").unwrap();
    let settings = shared.config.snapshot();
    let preferences = shared.launcher_preferences.snapshot();
    let generation = {
        let _operation = shared.operations.lock().unwrap();
        launcher_monitor_runtime::begin_preparation(
            &shared,
            RunStatus {
                stage: "preparing".into(),
                version: Some("fixture".into()),
                root_id: Some(root.id.clone()),
                root_path: Some(root.path.clone()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let prepared = capture(
        &shared,
        "fixture",
        &root,
        &settings,
        &preferences.revision,
        generation,
        IsolationPolicy::Automatic,
    )
    .unwrap();
    (fixture, shared, prepared)
}

pub(super) fn repaired() -> MissingRepairReport {
    MissingRepairReport {
        id: "fixture".into(),
        files_downloaded: 1,
        files_present: 1,
        unsupported: Vec::new(),
        unsupported_total: 0,
    }
}

#[test]
fn healthy_files_build_without_repair_task_java_or_network() {
    let (_fixture, shared, prepared) = fixture(false, true);
    let result = prepared
        .prepare_using(
            &shared,
            || {
                assert!(shared.operations.try_lock().is_ok());
                assert!(shared.tasks.active().is_none());
                Ok(17)
            },
            |_, _, _| panic!("healthy launch must bypass repair"),
        )
        .unwrap();
    assert_eq!(result, 17);
    assert!(shared.tasks.list().is_empty());
    let gate = prepared.authorize(&shared).unwrap();
    assert!(shared.operations.try_lock().is_err());
    let stopping = shared.clone();
    let (entered, waiting) = mpsc::channel();
    let (finished, done) = mpsc::channel();
    let stop = std::thread::spawn(move || {
        entered.send(()).unwrap();
        finished
            .send(launcher_monitor_runtime::request_stop(&stopping))
            .unwrap();
    });
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(matches!(
        done.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(gate);
    done.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
    stop.join().unwrap();
    assert!(prepared.authorize(&shared).is_err());
}

#[test]
fn missing_repair_holds_global_lease_through_rebuilding_the_plan() {
    let (_fixture, shared, prepared) = fixture(true, true);
    let output = Path::new(&prepared.root.path).join("libraries/fixture/file.jar");
    let built = AtomicUsize::new(0);
    let result = prepared
        .prepare_using(
            &shared,
            || {
                assert_eq!(fs::read(&output).unwrap(), b"fixture");
                assert!(shared.tasks.active().is_some());
                assert!(shared.operations.try_lock().is_ok());
                built.fetch_add(1, Ordering::SeqCst);
                Ok("plan")
            },
            |_, _, _| {
                assert!(shared.operations.try_lock().is_ok());
                assert!(shared
                    .tasks
                    .admit(
                        TaskTarget {
                            root_id: "other".into(),
                            root_path: "/synthetic/other".into(),
                            instance_id: None
                        },
                        TaskKind::InstanceRepair
                    )
                    .is_err());
                fs::write(&output, b"fixture").unwrap();
                Ok(repaired())
            },
        )
        .unwrap();
    assert_eq!(result, "plan");
    assert_eq!(built.load(Ordering::SeqCst), 1);
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Complete);
    assert!(shared.tasks.active().is_none());
    assert_eq!(shared.status.lock().unwrap().stage, "preparing");
    assert!(prepared.authorize(&shared).is_ok());
}

#[test]
fn unsupported_missing_files_do_not_create_an_empty_task() {
    let (_fixture, shared, prepared) = fixture(true, false);
    assert!(prepared
        .prepare_using(&shared, || Ok(()), |_, _, _| panic!("unsupported"))
        .is_err());
    assert!(shared.tasks.list().is_empty());
}

#[test]
fn stop_before_repair_does_not_admit_or_build() {
    let (_fixture, shared, prepared) = fixture(true, true);
    launcher_monitor_runtime::request_stop(&shared).unwrap();
    assert_eq!(
        prepared
            .prepare_using(
                &shared,
                || -> Result<(), String> { panic!("cancelled plan") },
                |_, _, _| panic!("cancelled repair")
            )
            .unwrap_err(),
        crate::tasks::CANCELLED
    );
    assert!(shared.tasks.list().is_empty());
    assert!(prepared.authorize(&shared).is_err());
}

#[test]
fn a_cancelled_preparation_does_not_poison_the_next_healthy_generation() {
    let (_fixture, shared, prepared) = fixture(true, true);
    prepared
        .prepare_using(
            &shared,
            || Ok(()),
            |_, _, _| {
                launcher_monitor_runtime::request_stop(&shared).unwrap();
                Err(crate::tasks::CANCELLED.into())
            },
        )
        .unwrap_err();
    launcher_monitor_runtime::update_current(
        &shared,
        prepared.generation,
        "idle",
        "cancelled".into(),
        None,
        None,
    );
    let root = shared.config.resolve(None).unwrap();
    fs::write(
        Path::new(&root.path).join("libraries/fixture/file.jar"),
        b"fixture",
    )
    .unwrap();
    let settings = shared.config.snapshot();
    let preferences = shared.launcher_preferences.snapshot();
    let generation = {
        let _operation = shared.operations.lock().unwrap();
        launcher_monitor_runtime::begin_preparation(
            &shared,
            RunStatus {
                stage: "preparing".into(),
                version: Some("fixture".into()),
                root_id: Some(root.id.clone()),
                root_path: Some(root.path.clone()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let next = capture(
        &shared,
        "fixture",
        &root,
        &settings,
        &preferences.revision,
        generation,
        IsolationPolicy::Automatic,
    )
    .unwrap();
    assert_eq!(next.prepare_files_with(&shared, || Ok(5)).unwrap(), 5);
    assert!(next.authorize(&shared).is_ok());
    assert!(prepared.authorize(&shared).is_err());
}

#[test]
fn a_real_healthy_plan_failure_survives_a_concurrent_stop() {
    let (_fixture, shared, prepared) = fixture(false, true);
    let result: Result<(), String> = prepared.prepare_files_with(&shared, || {
        launcher_monitor_runtime::request_stop(&shared).unwrap();
        Err("synthetic Java failure".into())
    });
    assert_eq!(result.unwrap_err(), "synthetic Java failure");
    assert!(shared.tasks.list().is_empty());
}

#[test]
fn cancellation_never_hides_a_real_repair_failure_or_authorizes_an_unprepared_plan() {
    let (_fixture, shared, prepared) = fixture(true, true);
    assert!(prepared.authorize(&shared).is_err());
    let result = prepared.prepare_using(
        &shared,
        || Ok(()),
        |_, _, _| {
            launcher_monitor_runtime::request_stop(&shared).unwrap();
            Err("synthetic SHA1 mismatch".into())
        },
    );
    assert_eq!(result.unwrap_err(), "synthetic SHA1 mismatch");
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Error);
    assert!(prepared.authorize(&shared).is_err());
}

#[test]
fn stop_accepted_by_the_admit_listener_finishes_the_unbound_task_as_cancelled() {
    let (_fixture, shared, prepared) = fixture(true, true);
    let weak = Arc::downgrade(&shared);
    shared.tasks.set_listener(move |snapshot| {
        if snapshot.stage == TaskStage::Preparing {
            launcher_monitor_runtime::request_stop(&weak.upgrade().unwrap()).unwrap();
        }
    });
    let result: Result<(), String> = prepared.prepare_using(
        &shared,
        || panic!("cancelled plan must not build"),
        |_, _, _| panic!("unbound cancelled task must not repair"),
    );
    assert_eq!(result.unwrap_err(), crate::tasks::CANCELLED);
    let task = shared.tasks.list().pop().unwrap();
    assert_eq!(task.stage, TaskStage::Cancelled);
    assert!(task.error.is_none());
    assert!(task.message.starts_with("文件补全已取消"));
    assert!(shared.tasks.active().is_none());
    assert!(prepared.authorize(&shared).is_err());
}

#[test]
fn binding_failure_in_the_admit_listener_retains_its_exact_error() {
    let (_fixture, shared, prepared) = fixture(true, true);
    let weak = Arc::downgrade(&shared);
    let generation = prepared.generation;
    shared.tasks.set_listener(move |snapshot| {
        if snapshot.stage == TaskStage::Preparing {
            launcher_monitor_runtime::update_current(
                &weak.upgrade().unwrap(),
                generation,
                "error",
                "synthetic binding change".into(),
                None,
                None,
            );
        }
    });
    let result: Result<(), String> = prepared.prepare_using(
        &shared,
        || panic!("stale plan must not build"),
        |_, _, _| panic!("unbound stale task must not repair"),
    );
    let error = result.unwrap_err();
    assert_eq!(error, "启动准备会话已变化，请重新启动");
    let task = shared.tasks.list().pop().unwrap();
    assert_eq!(task.stage, TaskStage::Error);
    assert_eq!(task.error.as_deref(), Some(error.as_str()));
    assert_eq!(task.message, error);
    assert!(shared.tasks.active().is_none());
    assert!(prepared.authorize(&shared).is_err());
}

#[test]
fn stop_cancels_the_exact_task_token_while_waiting_for_a_download_slot() {
    let (_fixture, shared, prepared) = fixture(true, true);
    let scheduler = pcl_network::DownloadScheduler::new(pcl_network::DownloadPolicy {
        max_concurrent_transfers: 1,
        ..Default::default()
    })
    .unwrap();
    let held = scheduler.acquire_blocking(&AtomicBool::new(false)).unwrap();
    let (entered, waiting) = mpsc::channel();
    let worker_shared = shared.clone();
    let worker = std::thread::spawn(move || {
        prepared.prepare_using(
            &worker_shared,
            || -> Result<(), String> { panic!("cancelled plan") },
            |_, _, task| {
                entered.send(()).unwrap();
                scheduler
                    .acquire_blocking(&task.cancellation_token())
                    .map(|_| repaired())
            },
        )
    });
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    launcher_monitor_runtime::request_stop(&shared).unwrap();
    assert_eq!(worker.join().unwrap().unwrap_err(), "下载已取消");
    drop(held);
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Cancelled);
    assert!(shared.tasks.active().is_none());
}

#[test]
fn task_cancel_during_rebuild_prevents_success_and_spawn_authorization() {
    let (_fixture, shared, prepared) = fixture(true, true);
    let output = Path::new(&prepared.root.path).join("libraries/fixture/file.jar");
    let result = prepared.prepare_using(
        &shared,
        || {
            shared
                .tasks
                .cancel(&shared.tasks.active().unwrap().id)
                .unwrap();
            Ok(())
        },
        |_, _, _| {
            fs::write(&output, b"fixture").unwrap();
            Ok(repaired())
        },
    );
    assert_eq!(result.unwrap_err(), crate::tasks::CANCELLED);
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Cancelled);
    assert!(prepared.authorize(&shared).is_err());
    assert!(output.exists());
}

#[test]
fn settings_preferences_and_transient_close_changes_retire_preparation() {
    for change in ["settings", "preferences", "close"] {
        let (_fixture, shared, prepared) = fixture(false, true);
        let result = prepared.prepare_using(
            &shared,
            || {
                match change {
                    "settings" => {
                        let mut settings = shared.config.snapshot();
                        settings.memory_gib = 3;
                        shared.config.save(settings).unwrap();
                    }
                    "preferences" => {
                        let view = shared.launcher_preferences.snapshot();
                        shared
                            .launcher_preferences
                            .update(
                                &view.revision,
                                serde_json::from_value(
                                    json!({"game_launch":{"custom_info":"changed"}}),
                                )
                                .unwrap(),
                            )
                            .unwrap();
                    }
                    _ => {
                        shared.close_generation.fetch_add(2, Ordering::SeqCst);
                    }
                }
                Ok(())
            },
            |_, _, _| panic!("healthy"),
        );
        assert!(result.is_err(), "{change}");
        assert!(prepared.authorize(&shared).is_err());
    }
}

#[test]
fn directory_replacement_and_worker_panics_preserve_errors_and_release_lease() {
    let (_fixture, shared, prepared) = fixture(true, true);
    let result = prepared.prepare_using(
        &shared,
        || Ok(()),
        |_, _, _| panic!("synthetic worker panic"),
    );
    assert!(result.unwrap_err().contains("意外退出"));
    assert!(shared.tasks.active().is_none());
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Error);
    let old = Path::new(&prepared.root.path).with_extension("old");
    fs::rename(&prepared.root.path, &old).unwrap();
    fs::create_dir(&prepared.root.path).unwrap();
    assert!(prepared
        .prepare_using(&shared, || Ok(()), |_, _, _| panic!("replaced root"))
        .is_err());
}
