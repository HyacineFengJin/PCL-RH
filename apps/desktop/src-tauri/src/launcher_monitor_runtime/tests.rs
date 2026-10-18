//! Deterministic GUI coordination regressions. Fake marker reads/signals model
//! the exact I/O boundaries; fixtures never start Tauri, Java or a real monitor.
use super::*;
use crate::integration_tests::Fixture;
use std::sync::{
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
    mpsc, Arc, Barrier,
};
use std::time::Duration;

fn preparing(shared: &Shared, label: &str) -> RunStatus {
    let root = shared.config.resolve(None).unwrap();
    RunStatus {
        stage: "preparing".into(),
        version: Some(label.into()),
        root_id: Some(root.id),
        root_path: Some(root.path),
        ..Default::default()
    }
}
fn terminal(shared: &Shared) -> Observation {
    let root = shared.config.resolve(None).unwrap();
    Observation {
        session_id: "previous-supervisor".into(),
        busy: false,
        game_running: false,
        pid: Some(4242),
        monitor_pid: 4343,
        root_id: root.id,
        root_path: root.path.clone(),
        instance_id: None,
        log_path: format!("{}/.pcl-linux/logs/previous.log", root.path),
        log_start: None,
        boundary_pending: false,
        terminal: true,
        retired: false,
        exit_code: None,
        revision: "fake-revision".into(),
    }
}
#[test]
fn delayed_terminal_read_finishes_before_a_new_preparation_can_publish() {
    let fixture = Fixture::new();
    let shared = Arc::new(fixture.shared());
    {
        let mut session = shared.monitor.lock();
        session.attached = true;
        session.supervisor = Some("previous-supervisor".into());
    }
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let polling = shared.clone();
    let poll_entered = entered.clone();
    let poll_release = release.clone();
    let poll = std::thread::spawn(move || {
        let mut session = polling.monitor.lock();
        refresh_from(&polling, &mut session, || {
            poll_entered.wait();
            poll_release.wait();
            Ok(Some(terminal(&polling)))
        })
        .unwrap();
    });
    entered.wait();
    assert!(
        shared.monitor.sync.try_lock().is_err(),
        "read/adoption must pin session state"
    );
    let next = shared.clone();
    let (started, waiting) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let launch = std::thread::spawn(move || {
        started.send(()).unwrap();
        let generation = begin_preparation(&next, preparing(&next, "New launch")).unwrap();
        done.send(generation).unwrap();
    });
    waiting.recv().unwrap();
    assert!(completed.recv_timeout(Duration::from_millis(30)).is_err());
    release.wait();
    poll.join().unwrap();
    let generation = completed.recv_timeout(Duration::from_secs(2)).unwrap();
    launch.join().unwrap();
    assert!(generation.0 > 0);
    let run = shared.status.lock().unwrap();
    assert_eq!(run.stage, "preparing");
    assert_eq!(run.version.as_deref(), Some("New launch"));
}
#[test]
fn previous_worker_cannot_change_new_status_consume_its_stop_or_restore_its_window() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let previous = begin_preparation(&shared, preparing(&shared, "Previous")).unwrap();
    update_current(&shared, previous, "idle", "cancelled".into(), None, None);
    let current = begin_preparation(&shared, preparing(&shared, "Current")).unwrap();
    shared.stop.store(true, Ordering::SeqCst);
    update_current(
        &shared,
        previous,
        "error",
        "late old failure".into(),
        None,
        None,
    );
    let effects = AtomicUsize::new(0);
    assert!(with_current(&shared, previous, || effects
        .fetch_add(1, AtomicOrdering::SeqCst))
    .is_none());
    assert!(with_spawn_visibility(&shared, previous, || effects
        .fetch_add(1, AtomicOrdering::SeqCst))
    .unwrap()
    .is_none());
    assert!(!poll_generation(&shared, previous).unwrap());
    assert_eq!(effects.load(AtomicOrdering::SeqCst), 0);
    assert!(
        shared.stop.load(Ordering::SeqCst),
        "old poll cannot consume new launch cancellation"
    );
    let run = shared.status.lock().unwrap();
    assert_eq!(run.stage, "preparing");
    assert_eq!(run.version.as_deref(), Some("Current"));
    assert_ne!(previous, current);
}

#[test]
fn new_preparation_that_fails_before_monitor_spawn_cannot_reuse_the_previous_log() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let old_log = shared.project.join("previous.log");
    *shared.log.lock().unwrap() = Some(old_log);
    set_failure_log_start(&shared, Some(123));
    let generation = begin_preparation(&shared, preparing(&shared, "Current")).unwrap();
    update_current(
        &shared,
        generation,
        "error",
        "preparation failed".into(),
        None,
        None,
    );
    let failed = failure_snapshot(&shared).unwrap();
    assert_eq!(failed.run.message, "preparation failed");
    assert!(failed.log.is_none());
    assert_eq!(failed.log_start, None);
}
#[test]
fn successful_stop_intent_survives_a_later_failed_signal_retry() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let generation = begin_preparation(&shared, preparing(&shared, "Example")).unwrap();
    let mut session = shared.monitor.lock();
    session.attached = true;
    session.supervisor = Some("previous-supervisor".into());
    stop_locked(&mut session, || Ok(())).unwrap();
    assert!(stop_locked(&mut session, || Err("stale marker".into())).is_err());
    assert!(session.stop_requested);
    refresh_from(&shared, &mut session, || Ok(Some(terminal(&shared)))).unwrap();
    assert!(!session.stop_requested);
    assert!(!session.attached);
    assert_eq!(session.generation, generation.0);
    let run = shared.status.lock().unwrap();
    assert_eq!(run.stage, "exited");
    assert_eq!(run.message, "游戏已停止");
    assert_eq!(run.version.as_deref(), Some("Example"));
    drop(run);
    refresh_from(&shared, &mut session, || Ok(Some(terminal(&shared)))).unwrap();
    assert_eq!(shared.status.lock().unwrap().message, "游戏已停止");
    assert_eq!(shared.status.lock().unwrap().stage, "exited");
}
#[test]
fn stop_during_handshake_is_applied_at_ack_before_any_exit_or_hide_effect() {
    let fixture = Fixture::new();
    let shared = Arc::new(fixture.shared());
    let generation = begin_preparation(&shared, preparing(&shared, "Example")).unwrap();
    let ready = Arc::new(Barrier::new(2));
    let ack = Arc::new(Barrier::new(2));
    let signals = Arc::new(AtomicUsize::new(0));
    let helper = shared.clone();
    let helper_ready = ready.clone();
    let helper_ack = ack.clone();
    let helper_signals = signals.clone();
    let worker = std::thread::spawn(move || {
        // A fake bounded helper handshake pauses before ACK, exactly where GUI
        // cancellation previously missed ExitAfterLaunch's early return.
        helper_ready.wait();
        helper_ack.wait();
        adopt_with(
            &helper,
            generation,
            4242,
            4343,
            "previous-supervisor".into(),
            helper.project.join("owned.log"),
            None,
            |session| {
                stop_locked(session, || {
                    helper_signals.fetch_add(1, AtomicOrdering::SeqCst);
                    Ok(())
                })
            },
        )
        .unwrap()
    });
    ready.wait();
    request_stop(&shared).unwrap();
    ack.wait();
    assert_eq!(worker.join().unwrap(), Handoff::Cancelled);
    assert_eq!(signals.load(AtomicOrdering::SeqCst), 1);
    let window_effects = AtomicUsize::new(0);
    assert!(with_spawn_visibility(&shared, generation, || window_effects
        .fetch_add(1, AtomicOrdering::SeqCst))
    .unwrap()
    .is_none());
    assert_eq!(window_effects.load(AtomicOrdering::SeqCst), 0);
    assert!(!shared.stop.load(Ordering::SeqCst));
}
#[test]
fn stop_between_ack_and_window_action_suppresses_automatic_exit() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let generation = begin_preparation(&shared, preparing(&shared, "Example")).unwrap();
    assert_eq!(
        adopt_with(
            &shared,
            generation,
            4242,
            4343,
            "previous-supervisor".into(),
            shared.project.join("owned.log"),
            None,
            |_| panic!("not cancelled")
        )
        .unwrap(),
        Handoff::Ready
    );
    stop_locked(&mut shared.monitor.lock(), || Ok(())).unwrap();
    assert!(with_spawn_visibility(&shared, generation, || panic!(
        "a stop must suppress close/hide"
    ))
    .unwrap()
    .is_none());
}

#[test]
fn a_different_durable_supervisor_invalidates_old_worker_and_same_root_version() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let previous = begin_preparation(&shared, preparing(&shared, "Previous instance")).unwrap();
    adopt_with(
        &shared,
        previous,
        4242,
        4343,
        "previous-supervisor".into(),
        shared.project.join("owned.log"),
        None,
        |_| panic!("not cancelled"),
    )
    .unwrap();
    let mut replacement = terminal(&shared);
    replacement.session_id = "another-gui-supervisor".into();
    replacement.busy = true;
    replacement.game_running = true;
    replacement.pid = Some(4545);
    replacement.monitor_pid = 4646;
    {
        let mut session = shared.monitor.lock();
        session.stop_requested = true; // Intent belonged to the previous game.
        refresh_from(&shared, &mut session, || Ok(Some(replacement))).unwrap();
        assert_ne!(session.generation, previous.0);
        assert!(!session.stop_requested);
    }
    assert!(!poll_generation(&shared, previous).unwrap());
    update_current(
        &shared,
        previous,
        "error",
        "late failure".into(),
        None,
        None,
    );
    assert!(with_current(&shared, previous, || panic!("old guard must not restore")).is_none());
    let run = shared.status.lock().unwrap();
    assert_eq!(run.stage, "running");
    assert_eq!(run.pid, Some(4545));
    assert!(
        run.version.is_none(),
        "same root does not establish same instance"
    );
}

/// Absent synthetic process IDs avoid starting or signalling any real process.
/// Both marker versions use a deliberately misleading log basename so recovery
/// must take instance identity exclusively from the explicit version-2 field.
fn completed_marker(shared: &Shared, instance: Option<&str>) -> (PathBuf, Vec<u8>) {
    let root = shared.config.resolve(None).unwrap();
    let scope =
        crate::launcher_local::filesystem::Scope::create(&shared.project, "game-monitor").unwrap();
    let mut marker = serde_json::json!({
        "schema_version":if instance.is_some() { 2 } else { 1 },
        "project":shared.project,"root_id":root.id,"root_path":root.path,
        "log_path":format!("{}/.pcl-linux/logs/misleading-instance-name.log",root.path),
        "monitor":{"pid":i32::MAX,"start_time":1,"boot_id":"00000000-0000-0000-0000-000000000000"},
        "game":{"pid":i32::MAX-1,"start_time":1,"boot_id":"00000000-0000-0000-0000-000000000000"},
        "phase":"failed","started_at":1,"exit_code":2
    });
    if let Some(instance) = instance {
        marker["instance_id"] = instance.into();
    }
    let path = scope.path().join("active.json");
    let bytes = serde_json::to_vec(&marker).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

#[test]
fn reopened_gui_restores_new_and_legacy_failures_without_rewriting_or_guessing_identity() {
    for instance in [Some("captured-instance"), None] {
        let fixture = Fixture::new();
        let previous_gui = fixture.shared();
        let root = previous_gui.config.resolve(None).unwrap();
        let (path, bytes) = completed_marker(&previous_gui, instance);
        let log = PathBuf::from(&root.path).join(".pcl-linux/logs/misleading-instance-name.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, b"recovered failure log").unwrap();
        drop(previous_gui);
        let reopened = fixture.shared();
        // The current picker/status may contain another instance. It supplies
        // no identity evidence for a completed attempt from the durable marker.
        reopened.status.lock().unwrap().version = Some("currently-selected".into());
        refresh(&reopened).unwrap();
        let failed = failure_snapshot(&reopened).unwrap();
        assert_eq!(failed.run.stage, "error");
        assert_eq!(failed.run.version.as_deref(), instance);
        assert_eq!(failed.run.root_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(failed.run.root_path.as_deref(), Some(root.path.as_str()));
        assert_eq!(failed.run.exit_code, Some(2));
        assert!(failed.run.pid.is_none());
        assert_eq!(failed.log, Some(log));
        assert_eq!(failed.log_start, None);
        let report = crate::launcher_failure::report(&reopened, &root.id, &failed.run).unwrap();
        let report = serde_json::to_value(report).unwrap();
        assert_eq!(report["version"], serde_json::to_value(instance).unwrap());
        assert_eq!(report["exit_code"], 2);
        assert_eq!(report["log_tail"], "recovered failure log");
        assert!(!test_attached(&reopened));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn new_preparation_retires_a_persisted_terminal_even_when_it_fails_before_monitor_spawn() {
    for instance in [None, Some("previous-instance")] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (path, _) = completed_marker(&shared, instance);
        refresh(&shared).unwrap();
        let previous = failure_snapshot(&shared).unwrap();
        let log = previous.log.as_ref().unwrap();
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(log, b"retained previous log").unwrap();
        let current = begin_preparation(&shared, preparing(&shared, "current-instance")).unwrap();
        update_current(
            &shared,
            current,
            "error",
            "current preparation failed".into(),
            None,
            None,
        );
        for _ in 0..3 {
            refresh(&shared).unwrap();
            let failed = failure_snapshot(&shared).unwrap();
            assert_eq!(failed.run.version.as_deref(), Some("current-instance"));
            assert_eq!(failed.run.message, "current preparation failed");
            assert!(failed.log.is_none());
            assert_ne!(failed.generation, previous.generation);
        }
        let bytes = std::fs::read(&path).unwrap();
        let retired: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(retired["phase"], "retired");
        assert_eq!(retired["schema_version"], 2);
        assert_eq!(
            retired
                .get("instance_id")
                .and_then(serde_json::Value::as_str),
            instance
        );
        let reopened = fixture.shared();
        refresh(&reopened).unwrap();
        assert!(failure_snapshot(&reopened).is_err());
        assert!(reopened.status.lock().unwrap().version.is_none());
        assert!(reopened.log.lock().unwrap().is_none());
        require_idle(&reopened).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(std::fs::read(log).unwrap(), b"retained previous log");
    }
}

#[test]
fn cancelled_or_not_started_preparation_does_not_restore_old_history_or_create_a_new_failure_on_reopen(
) {
    for cancelled in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (path, _) = completed_marker(&shared, None);
        let generation = begin_preparation(&shared, preparing(&shared, "next-instance")).unwrap();
        if cancelled {
            request_stop(&shared).unwrap();
            update_current(&shared, generation, "idle", "启动已取消".into(), None, None);
            refresh(&shared).unwrap();
            assert_eq!(shared.status.lock().unwrap().stage, "idle");
        }
        assert!(failure_snapshot(&shared).is_err());
        let reopened = fixture.shared();
        refresh(&reopened).unwrap();
        assert!(failure_snapshot(&reopened).is_err());
        assert!(!test_attached(&reopened));
        assert!(reopened.log.lock().unwrap().is_none());
        let retired: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(retired["phase"], "retired");
    }
}

#[test]
fn failed_retirement_does_not_publish_preparation_or_destroy_the_recoverable_previous_failure() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (path, bytes) = completed_marker(&shared, Some("previous-instance"));
    refresh(&shared).unwrap();
    let previous = failure_snapshot(&shared).unwrap();
    let result = begin_preparation_with(&shared, preparing(&shared, "next-instance"), |revision| {
        assert!(!revision.is_empty());
        Err("synthetic durable retirement failure".into())
    });
    assert!(matches!(result, Err(error) if error == "synthetic durable retirement failure"));
    let after = failure_snapshot(&shared).unwrap();
    assert!(after == previous);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let reopened = fixture.shared();
    refresh(&reopened).unwrap();
    assert_eq!(
        failure_snapshot(&reopened).unwrap().run.version.as_deref(),
        Some("previous-instance")
    );
}

#[test]
fn retirement_committed_before_a_reported_sync_failure_never_publishes_preparation_or_revives_history(
) {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    completed_marker(&shared, Some("previous-instance"));
    refresh(&shared).unwrap();
    let previous = failure_snapshot(&shared).unwrap();
    let result = begin_preparation_with(&shared, preparing(&shared, "next-instance"), |revision| {
        launcher_game_monitor::retire_completed(&shared.project, revision)?;
        // An atomic exchange may be visible before a later directory sync or
        // final directory identity recheck reports failure to the caller.
        Err("synthetic post-commit synchronization failure".into())
    });
    assert!(result.is_err());
    assert!(*shared.status.lock().unwrap() == previous.run);
    refresh(&shared).unwrap();
    assert_eq!(shared.status.lock().unwrap().stage, "idle");
    assert!(failure_snapshot(&shared).is_err());
    assert!(shared.log.lock().unwrap().is_none());
    let reopened = fixture.shared();
    refresh(&reopened).unwrap();
    assert!(failure_snapshot(&reopened).is_err());
    assert!(reopened.log.lock().unwrap().is_none());
}

#[test]
fn rejected_admission_never_attempts_to_retire_previous_durable_history() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    let (path, bytes) = completed_marker(&shared, Some("previous-instance"));
    refresh(&shared).unwrap();
    let previous = failure_snapshot(&shared).unwrap();
    let _task = shared
        .tasks
        .admit(
            crate::tasks::TaskTarget {
                root_id: root.id,
                root_path: root.path,
                instance_id: None,
            },
            crate::tasks::TaskKind::LauncherLogs,
        )
        .unwrap();
    assert!(
        begin_preparation_with(&shared, preparing(&shared, "next-instance"), |_| {
            panic!("rejected preparation must not retire history");
        })
        .is_err()
    );
    assert!(failure_snapshot(&shared).unwrap() == previous);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn another_gui_retiring_a_previously_running_helper_clears_context_without_publishing_failure() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let mut running = terminal(&shared);
    running.busy = true;
    running.game_running = true;
    running.terminal = false;
    let mut session = shared.monitor.lock();
    refresh_from(&shared, &mut session, || Ok(Some(running))).unwrap();
    let mut retired = terminal(&shared);
    retired.terminal = false;
    retired.retired = true;
    refresh_from(&shared, &mut session, || Ok(Some(retired))).unwrap();
    assert!(!session.attached);
    assert_eq!(shared.status.lock().unwrap().stage, "idle");
    assert!(shared.log.lock().unwrap().is_none());
    assert!(failure_snapshot_locked(&shared, &session).is_err());
}

#[test]
fn same_named_instance_in_another_root_recovers_its_own_explicit_identity_and_log() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let previous = begin_preparation(&shared, preparing(&shared, "same-instance")).unwrap();
    adopt_with(
        &shared,
        previous,
        4242,
        4343,
        "previous-supervisor".into(),
        shared.project.join("previous.log"),
        None,
        |_| panic!("not cancelled"),
    )
    .unwrap();
    let mut next = terminal(&shared);
    next.session_id = "next-root-supervisor".into();
    next.root_id = "another-root".into();
    next.root_path = shared.project.join("another-root").display().to_string();
    next.instance_id = Some("same-instance".into());
    next.log_path = format!("{}/.pcl-linux/logs/next.log", next.root_path);
    let expected_log = PathBuf::from(&next.log_path);
    let mut session = shared.monitor.lock();
    refresh_from(&shared, &mut session, || Ok(Some(next))).unwrap();
    assert_ne!(session.generation, previous.0);
    let failed = failure_snapshot_locked(&shared, &session).unwrap();
    assert_eq!(failed.run.root_id.as_deref(), Some("another-root"));
    assert_eq!(failed.run.version.as_deref(), Some("same-instance"));
    assert_eq!(failed.log, Some(expected_log));
}

#[test]
fn external_same_session_terminal_binding_or_revision_edits_retire_the_report_and_keep_bytes() {
    for case in ["instance", "root", "log", "revision", "absent"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (path, bytes) = completed_marker(&shared, Some("captured-instance"));
        refresh(&shared).unwrap();
        let expected = failure_snapshot(&shared).unwrap();
        let mut marker: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        match case {
            "instance" => marker["instance_id"] = "another-instance".into(),
            "root" => {
                marker["root_id"] = "another-root".into();
                marker["root_path"] = shared
                    .project
                    .join("another-root")
                    .display()
                    .to_string()
                    .into();
                marker["log_path"] = format!(
                    "{}/.pcl-linux/logs/next.log",
                    marker["root_path"].as_str().unwrap()
                )
                .into();
            }
            "log" => {
                marker["log_path"] = format!(
                    "{}/.pcl-linux/logs/another.log",
                    marker["root_path"].as_str().unwrap()
                )
                .into()
            }
            "revision" | "absent" => {}
            _ => unreachable!(),
        }
        let replacement = serde_json::to_vec(&marker).unwrap();
        if case == "absent" {
            std::fs::remove_file(&path).unwrap();
        } else {
            let staged = path.with_file_name("external.json");
            std::fs::write(&staged, &replacement).unwrap();
            std::fs::rename(staged, &path).unwrap();
        }
        assert!(
            verify_failure(&shared, &expected, || Ok(())).is_err(),
            "{case}"
        );
        assert!(
            !test_attached(&shared),
            "completed history cannot revive a game"
        );
        assert_eq!(shared.status.lock().unwrap().stage, "idle");
        assert!(shared.log.lock().unwrap().is_none());
        require_idle(&shared).unwrap();
        if case != "absent" {
            assert_eq!(std::fs::read(path).unwrap(), replacement);
        }
    }
}

#[test]
fn another_completed_session_retires_a_report_even_when_visible_failure_and_log_are_identical() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (path, bytes) = completed_marker(&shared, Some("captured-instance"));
    refresh(&shared).unwrap();
    let previous = failure_snapshot(&shared).unwrap();
    let mut marker: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    marker["monitor"]["start_time"] = 2.into();
    std::fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
    assert!(verify_failure(&shared, &previous, || Ok(())).is_err());
    let current = failure_snapshot(&shared).unwrap();
    assert!(current.run == previous.run);
    assert_eq!(current.log, previous.log);
    assert_ne!(current.generation, previous.generation);
}

#[test]
fn abandoned_unfinished_history_does_not_restore_a_terminal_failure() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (path, bytes) = completed_marker(&shared, Some("captured-instance"));
    let mut marker: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    marker["phase"] = "running".into();
    marker["exit_code"] = serde_json::Value::Null;
    let bytes = serde_json::to_vec(&marker).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let initial = shared.status.lock().unwrap().clone();
    refresh(&shared).unwrap();
    assert!(*shared.status.lock().unwrap() == initial);
    assert!(shared.log.lock().unwrap().is_none());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn a_live_same_supervisor_cannot_change_its_bound_root_instance_log_or_boundary() {
    for case in ["root", "instance", "log", "boundary"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let mut live = terminal(&shared);
        live.busy = true;
        live.game_running = true;
        live.terminal = false;
        live.instance_id = Some("captured-instance".into());
        live.log_start = Some(12);
        let mut session = shared.monitor.lock();
        refresh_from(&shared, &mut session, || Ok(Some(live))).unwrap();
        let bound = shared.status.lock().unwrap().clone();
        let mut changed = terminal(&shared);
        changed.busy = true;
        changed.game_running = true;
        changed.terminal = false;
        changed.instance_id = Some("captured-instance".into());
        changed.log_start = Some(12);
        match case {
            "root" => changed.root_id = "another-root".into(),
            "instance" => changed.instance_id = Some("another-instance".into()),
            "log" => changed.log_path.push_str(".another.log"),
            "boundary" => changed.log_start = Some(13),
            _ => unreachable!(),
        }
        assert!(refresh_from(&shared, &mut session, || Ok(Some(changed))).is_err());
        assert!(session.attached);
        let run = shared.status.lock().unwrap();
        assert_eq!(run.stage, "error");
        assert_eq!(run.root_id, bound.root_id);
        assert_eq!(run.root_path, bound.root_path);
        assert_eq!(run.version, bound.version);
    }
}

#[test]
fn unknown_replacement_of_completed_history_still_preserves_bytes_and_blocks_admission() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (path, _) = completed_marker(&shared, Some("captured-instance"));
    refresh(&shared).unwrap();
    let expected = failure_snapshot(&shared).unwrap();
    let unknown = b"unknown retained monitor format";
    std::fs::write(&path, unknown).unwrap();
    assert!(verify_failure(&shared, &expected, || Ok(())).is_err());
    assert!(test_attached(&shared));
    assert!(require_idle(&shared).is_err());
    assert_eq!(std::fs::read(path).unwrap(), unknown);
}

#[test]
fn reopened_failure_keeps_its_persisted_boundary_and_boundary_edits_retire_it() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (path, bytes) = completed_marker(&shared, Some("captured-instance"));
    let mut marker: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    marker["schema_version"] = 3.into();
    marker["log_start"] = 123.into();
    let bytes = serde_json::to_vec(&marker).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    refresh(&shared).unwrap();
    let previous = failure_snapshot(&shared).unwrap();
    assert_eq!(previous.log_start, Some(123));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    drop(shared);
    let reopened = fixture.shared();
    let expected = failure_snapshot(&reopened).unwrap();
    assert_eq!(expected.log_start, Some(123));
    marker["log_start"] = 124.into();
    let edited = serde_json::to_vec(&marker).unwrap();
    std::fs::write(&path, &edited).unwrap();
    assert!(verify_failure(&reopened, &expected, || Ok(())).is_err());
    assert_eq!(reopened.status.lock().unwrap().stage, "idle");
    assert!(reopened.log.lock().unwrap().is_none());
    assert_eq!(reopened.monitor.lock().log_start, None);
    assert_eq!(std::fs::read(path).unwrap(), edited);
}

#[test]
fn ack_adoption_pins_the_boundary_before_the_first_marker_poll() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let generation = begin_preparation(&shared, preparing(&shared, "captured-instance")).unwrap();
    let root = shared.config.resolve(None).unwrap();
    let log = PathBuf::from(&root.path).join(".pcl-linux/logs/previous.log");
    adopt_with(
        &shared,
        generation,
        4242,
        4343,
        "previous-supervisor".into(),
        log,
        Some(97),
        |_| panic!("not cancelled"),
    )
    .unwrap();
    let mut session = shared.monitor.lock();
    assert_eq!(session.log_start, Some(97));
    assert_eq!(session.binding.as_ref().unwrap().log_start, Some(97));
    let mut altered = terminal(&shared);
    altered.busy = true;
    altered.game_running = true;
    altered.terminal = false;
    altered.instance_id = Some("captured-instance".into());
    altered.log_start = Some(98);
    assert!(refresh_from(&shared, &mut session, || Ok(Some(altered))).is_err());
    assert_eq!(session.log_start, Some(97));
}

#[test]
fn observing_a_starting_helper_can_commit_its_boundary_once_before_or_after_spawn() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let mut session = shared.monitor.lock();
    let mut starting = terminal(&shared);
    starting.busy = true;
    starting.terminal = false;
    starting.pid = None;
    starting.boundary_pending = true;
    starting.instance_id = Some("captured-instance".into());
    refresh_from(&shared, &mut session, || Ok(Some(starting))).unwrap();
    assert!(session.boundary_pending);
    assert_eq!(session.log_start, None);
    let mut running = terminal(&shared);
    running.busy = true;
    running.terminal = false;
    running.game_running = true;
    running.log_start = Some(97);
    running.instance_id = Some("captured-instance".into());
    refresh_from(&shared, &mut session, || Ok(Some(running))).unwrap();
    assert!(!session.boundary_pending);
    assert_eq!(session.log_start, Some(97));
    let mut edited = terminal(&shared);
    edited.log_start = Some(98);
    edited.instance_id = Some("captured-instance".into());
    assert!(refresh_from(&shared, &mut session, || Ok(Some(edited))).is_err());
    assert_eq!(session.log_start, Some(97));
}
