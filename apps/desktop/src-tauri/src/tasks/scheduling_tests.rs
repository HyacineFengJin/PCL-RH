//! Isolated scheduler/descriptor-identity tests. Barriers acknowledge the exact
//! cleanup boundary; no sleep is used as evidence that a queued worker stopped.
#[cfg(windows)]
use super::scope::test_links::symlink;
use super::*;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::{
    fs,
    path::PathBuf,
    sync::{mpsc, Barrier},
    thread,
    time::Duration,
};

struct Fixture {
    path: PathBuf,
    _temporary: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        Self {
            path: temporary.path().canonicalize().unwrap(),
            _temporary: temporary,
        }
    }
    fn root(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn task(&self, tasks: &Arc<Tasks>, name: &str) -> TaskHandle {
        let root = self.root(name);
        tasks
            .admit_queued(
                target(name, &root),
                TaskKind::Install,
                TaskScope::root(&root).unwrap(),
            )
            .unwrap()
    }
}
fn target(id: &str, path: &Path) -> TaskTarget {
    TaskTarget {
        root_id: id.into(),
        root_path: path.to_string_lossy().into_owned(),
        instance_id: Some("Fixture instance".into()),
    }
}
fn complete(task: &TaskHandle) {
    assert!(task
        .finish(TaskOutcome::Complete {
            result: None,
            message: "Fixture complete".into(),
            error: None
        })
        .is_some());
}
fn progress(bytes: u64) -> TaskProgress {
    TaskProgress {
        stage: TaskStage::Downloading,
        phase: "fixture".into(),
        message: "Fixture progress".into(),
        completed: 1,
        total: 2,
        bytes_done: bytes,
        bytes_total: 100,
        network_bytes: bytes,
        steps: vec![],
    }
}
fn stage(tasks: &Tasks, task: &TaskHandle) -> TaskStage {
    tasks.snapshot(task.id()).unwrap().stage
}

#[test]
fn disjoint_roots_run_together_but_alias_and_nested_root_keep_fifo() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::new());
    let first = f.task(&tasks, "a");
    let other = f.task(&tasks, "b");
    first.wait_turn().unwrap();
    other.wait_turn().unwrap();
    let alias = f.path.join("alias-a");
    symlink(f.root("a"), &alias).unwrap();
    let alias_task = tasks
        .admit_queued(
            target("different-root-id", &alias),
            TaskKind::ResourceDownload,
            TaskScope::root(&alias).unwrap(),
        )
        .unwrap();
    let nested = f.task(&tasks, "a/nested");
    assert_eq!(tasks.running_all().len(), 2);
    assert_eq!(stage(&tasks, &alias_task), TaskStage::Queued);
    assert_eq!(stage(&tasks, &nested), TaskStage::Queued);
    assert!(tasks.blocks_path(&f.root("a")).unwrap());
    assert!(!tasks.blocks_path(&f.root("a-unrelated-prefix")).unwrap());
    complete(&first);
    alias_task.wait_turn().unwrap();
    assert_eq!(stage(&tasks, &nested), TaskStage::Queued);
    complete(&alias_task);
    nested.wait_turn().unwrap();
    complete(&nested);
    complete(&other);
    assert!(tasks.active_all().is_empty());
}

#[test]
fn final_files_share_a_folder_but_same_file_and_enclosing_root_queue() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::new());
    let folder = f.root("outputs");
    let alias = f.path.join("output-alias");
    symlink(&folder, &alias).unwrap();
    let admit = |_name: &str, path: PathBuf| {
        tasks
            .admit_queued(
                target("launcher", &folder),
                TaskKind::ResourceSave,
                TaskScope::files(&[path]).unwrap(),
            )
            .unwrap()
    };
    let first = admit("first", folder.join("first.zip"));
    let second = admit("second", folder.join("second.zip"));
    first.wait_turn().unwrap();
    second.wait_turn().unwrap();
    let same = admit("same", alias.join("first.zip"));
    let root = tasks
        .admit_queued(
            target("output-game", &folder),
            TaskKind::Install,
            TaskScope::root(&folder).unwrap(),
        )
        .unwrap();
    let later = admit("later", folder.join("third.zip"));
    assert_eq!(tasks.running_all().len(), 2);
    assert_eq!(stage(&tasks, &same), TaskStage::Queued);
    assert_eq!(stage(&tasks, &root), TaskStage::Queued);
    // The earlier whole-root job also protects its turn against later files.
    assert_eq!(stage(&tasks, &later), TaskStage::Queued);
    complete(&first);
    same.wait_turn().unwrap();
    complete(&same);
    assert_eq!(stage(&tasks, &root), TaskStage::Queued);
    complete(&second);
    root.wait_turn().unwrap();
    assert_eq!(stage(&tasks, &later), TaskStage::Queued);
    complete(&root);
    later.wait_turn().unwrap();
    complete(&later);
}

#[test]
fn four_running_and_thirty_two_pending_are_bounded_without_blocking_unrelated_free_slot() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::with_history_limit(1));
    let mut running: Vec<_> = (0..4)
        .map(|i| f.task(&tasks, &format!("root-{i}")))
        .collect();
    let mut pending: Vec<_> = (0..32).map(|_| f.task(&tasks, "root-0")).collect();
    let a = f.root("root-0");
    assert!(tasks
        .admit_queued(
            target("a", &a),
            TaskKind::Install,
            TaskScope::root(&a).unwrap()
        )
        .is_err());
    assert_eq!(tasks.active_all().len(), 36);
    assert_eq!(tasks.running_all().len(), 4);
    assert!(tasks
        .admit(target("legacy", &a), TaskKind::InstanceReset)
        .is_err());
    complete(&running.pop().unwrap());
    // A blocked queue head must not idle a free slot for another physical root.
    let disjoint = f.task(&tasks, "disjoint");
    disjoint.wait_turn().unwrap();
    assert_eq!(tasks.running_all().len(), 4);
    assert_eq!(tasks.active_all().len(), 36);
    for queued in pending.drain(..) {
        tasks.cancel(queued.id()).unwrap();
        assert_eq!(queued.wait_turn().unwrap_err(), CANCELLED);
        queued.finish(TaskOutcome::Failed(CANCELLED.into()));
    }
    for task in running {
        complete(&task);
    }
    complete(&disjoint);
    assert_eq!(tasks.list().len(), 1);
}

#[test]
fn queued_cancel_is_not_terminal_until_worker_cleanup_and_keeps_fifo_scope() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::with_history_limit(1));
    let running = f.task(&tasks, "a");
    let queued = f.task(&tasks, "a");
    let queued_id = queued.id().to_owned();
    let later = f.task(&tasks, "a");
    let pin = tasks.pin_history(&queued_id).unwrap();
    let cleaned = Arc::new(AtomicBool::new(false));
    struct Artifact(Arc<AtomicBool>);
    impl Drop for Artifact {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let artifact = Artifact(cleaned.clone());
    let barrier = Arc::new(Barrier::new(2));
    let cleanup_boundary = barrier.clone();
    let (ready, observed) = mpsc::channel();
    let worker = thread::spawn(move || {
        assert_eq!(queued.wait_turn().unwrap_err(), CANCELLED);
        ready.send(()).unwrap();
        cleanup_boundary.wait();
        drop(artifact);
        queued.finish(TaskOutcome::Failed(CANCELLED.into()));
    });
    let requested = tasks.cancel(&queued_id).unwrap();
    assert_eq!(requested.stage, TaskStage::Queued);
    assert!(!requested.can_cancel);
    observed.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(!cleaned.load(Ordering::SeqCst));
    complete(&running);
    assert_eq!(stage(&tasks, &later), TaskStage::Queued);
    let unrelated = f.task(&tasks, "b");
    unrelated.wait_turn().unwrap();
    barrier.wait();
    worker.join().unwrap();
    assert!(cleaned.load(Ordering::SeqCst));
    assert_eq!(
        tasks.wait_terminal(&queued_id).unwrap().stage,
        TaskStage::Cancelled
    );
    later.wait_turn().unwrap();
    complete(&later);
    complete(&unrelated);
    assert!(tasks.snapshot(&queued_id).is_some());
    assert_eq!(tasks.list().len(), 2); // pinned cancellation plus latest history
    drop(pin);
    assert!(tasks.snapshot(&queued_id).is_none());
    assert_eq!(tasks.list().len(), 1);
}

#[test]
fn running_cancel_retains_turn_and_true_cleanup_failure_is_not_masked() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::new());
    let running = f.task(&tasks, "a");
    running.wait_turn().unwrap();
    let queued = f.task(&tasks, "a");
    tasks.cancel(running.id()).unwrap();
    assert_eq!(tasks.running_all().len(), 1);
    assert_eq!(stage(&tasks, &queued), TaskStage::Queued);
    let error = running
        .finish(TaskOutcome::CleanupFailed(
            "Fixture cleanup conflict".into(),
        ))
        .unwrap();
    assert_eq!(error.stage, TaskStage::Error);
    queued.wait_turn().unwrap();
    complete(&queued);
}

#[test]
fn changed_directory_and_final_file_binding_are_refused_after_queue_wait() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::new());
    let root = f.root("a");
    let legacy = tasks
        .admit(target("legacy", &root), TaskKind::InstanceReset)
        .unwrap();
    let queued = f.task(&tasks, "a");
    fs::rename(&root, f.path.join("old-a")).unwrap();
    fs::create_dir(&root).unwrap();
    complete(&legacy);
    assert!(queued.wait_turn().unwrap_err().contains("身份已变化"));
    assert!(!stage(&tasks, &queued).is_terminal());
    queued.finish(TaskOutcome::Error("Fixture changed binding".into()));
    let outputs = f.root("outputs");
    let blocker = tasks
        .admit(target("blocker", &outputs), TaskKind::InstanceReset)
        .unwrap();
    let output = outputs.join("chosen.zip");
    let file = tasks
        .admit_queued(
            target("launcher", &outputs),
            TaskKind::ResourceSave,
            TaskScope::files(&[output.clone()]).unwrap(),
        )
        .unwrap();
    fs::write(&output, b"Foreign fixture data").unwrap();
    complete(&blocker);
    assert!(file.wait_turn().is_err());
    assert_eq!(fs::read(&output).unwrap(), b"Foreign fixture data");
    file.finish(TaskOutcome::Error("Fixture final-file changed".into()));
}

#[test]
fn terminal_waiter_pin_survives_trim_and_other_tasks_keep_their_latest_history() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::with_history_limit(1));
    let root = f.root("a");
    let first = tasks.admit(target("a", &root), TaskKind::Install).unwrap();
    let id = first.id().to_owned();
    let pin = tasks.pin_history(&id).unwrap();
    complete(&first);
    let mut latest = String::new();
    for _ in 0..8 {
        let next = tasks.admit(target("a", &root), TaskKind::Install).unwrap();
        latest = next.id().to_owned();
        complete(&next);
    }
    assert_eq!(tasks.list().len(), 2);
    assert!(tasks.snapshot(&latest).is_some());
    assert_eq!(tasks.wait_terminal(&id).unwrap().stage, TaskStage::Complete);
    drop(pin);
    assert!(tasks.snapshot(&id).is_none());
    assert!(tasks.snapshot(&latest).is_some());
}

#[test]
fn collection_revision_and_physical_blocked_roots_are_atomic_with_offline_fallback() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::new());
    let root = f.root("game");
    let output = root.join("file.zip");
    let saved = tasks
        .admit_queued(
            target("launcher", &root),
            TaskKind::ToolboxDownload,
            TaskScope::files(&[output]).unwrap(),
        )
        .unwrap();
    let alias = f.path.join("alias");
    symlink(&root, &alias).unwrap();
    let offline = f.path.join("offline");
    let roots = vec![
        ("game-a".into(), root.clone()),
        ("game-alias".into(), alias),
        ("offline".into(), offline),
    ];
    let (before, blocked) = tasks.list_snapshot_with_roots(&roots).unwrap();
    assert_eq!(blocked, vec!["game-a", "game-alias"]);
    saved.update(progress(42));
    let (updated, updated_blocked) = tasks.list_snapshot_with_roots(&roots).unwrap();
    assert!(updated.revision.parse::<u64>().unwrap() > before.revision.parse::<u64>().unwrap());
    assert_eq!(updated_blocked, blocked);
    assert_eq!(updated.tasks[0].network_bytes, 42);
    complete(&saved);
    let (after, unblocked) = tasks.list_snapshot_with_roots(&roots).unwrap();
    assert!(unblocked.is_empty());
    assert_eq!(after.tasks[0].stage, TaskStage::Complete);
    let offline_owner = f.task(&tasks, "temporarily-offline");
    let path = f.root("temporarily-offline");
    fs::rename(&path, f.path.join("offline-kept")).unwrap();
    let (_, blocked) = tasks
        .list_snapshot_with_roots(&[("temporarily-offline".into(), path)])
        .unwrap();
    assert_eq!(blocked, vec!["temporarily-offline"]);
    offline_owner.finish(TaskOutcome::Error("Fixture offline".into()));
}

#[test]
fn legacy_global_and_unsupported_job_kinds_never_bypass_turn_protocol() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::new());
    let root = f.root("a");
    let legacy = tasks
        .admit(target("a", &root), TaskKind::InstanceReset)
        .unwrap();
    let queued = f.task(&tasks, "b");
    assert_eq!(stage(&tasks, &queued), TaskStage::Queued);
    assert!(tasks
        .admit(target("a", &root), TaskKind::InstanceRename)
        .is_err());
    assert!(tasks
        .admit_queued(
            target("a", &root),
            TaskKind::ResourceOperation,
            TaskScope::root(&root).unwrap()
        )
        .is_err());
    assert!(queued.update(progress(1)).is_none());
    assert!(queued
        .finish(TaskOutcome::Complete {
            result: None,
            message: "Premature fixture".into(),
            error: None
        })
        .is_none());
    assert_eq!(stage(&tasks, &queued), TaskStage::Queued);
    complete(&legacy);
    queued.wait_turn().unwrap();
    complete(&queued);
}

#[test]
fn promotion_and_history_notifications_can_reenter_without_scheduler_mutex() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::new());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let notices = observed.clone();
    let weak = Arc::downgrade(&tasks);
    tasks.set_listener(move |snapshot| {
        let owner = weak.upgrade().unwrap();
        let view = owner.list_snapshot();
        notices.lock().unwrap().push((snapshot.id, view.revision));
    });
    let first = f.task(&tasks, "a");
    let next = f.task(&tasks, "a");
    let (done, waited) = mpsc::channel();
    let worker = thread::spawn(move || {
        complete(&first);
        done.send(()).unwrap();
    });
    waited.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    next.wait_turn().unwrap();
    complete(&next);
    assert!(observed.lock().unwrap().len() >= 5);
}
