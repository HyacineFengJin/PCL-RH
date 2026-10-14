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
        log_path: format!("{}/.pcl-linux/logs/previous.log", root.path),
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
    assert_eq!(generation.0, 1);
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
