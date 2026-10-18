//! Worker gate tests stop at a failing application preflight, before Installer
//! construction. Every target is an isolated directory; no HTTP/game is started.
use super::*;
use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("work/multi-task-2026-10-05/backend/fixtures")
            .join(format!(
                "downloads-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn root(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn target(id: &str, root: &Path) -> TaskTarget {
    TaskTarget {
        root_id: id.into(),
        root_path: root.to_string_lossy().into_owned(),
        instance_id: Some("Fixture".into()),
    }
}
fn request() -> InstallRequest {
    InstallRequest {
        minecraft: "1.21".into(),
        name: "Fixture".into(),
        components: vec![],
    }
}
fn complete(task: &TaskHandle) {
    task.finish(TaskOutcome::Complete {
        result: None,
        message: "Fixture completed".into(),
        error: None,
    })
    .unwrap();
}

#[test]
fn complete_projection_and_exact_cancel_do_not_select_the_latest_other_job() {
    let f = Fixture::new();
    let tasks = Arc::new(Tasks::new());
    let downloads = Downloads::new(tasks.clone());
    let a = f.root("a");
    let b = f.root("b");
    let first = tasks
        .admit_queued(
            target("a", &a),
            TaskKind::ResourceDownload,
            TaskScope::root(&a).unwrap(),
        )
        .unwrap();
    let second = tasks
        .admit_queued(
            target("b", &b),
            TaskKind::ResourceSave,
            TaskScope::root(&b).unwrap(),
        )
        .unwrap();
    let queued = tasks
        .admit_queued(
            target("a", &a),
            TaskKind::ResourceDownload,
            TaskScope::root(&a).unwrap(),
        )
        .unwrap();
    first.set_resource_name("Fixture actual resource");
    downloads.track(&second);
    let list = downloads.list_snapshot();
    assert_eq!(list.tasks.len(), 3);
    assert_eq!(list.running_limit, 4);
    assert_eq!(list.pending_limit, 32);
    assert_eq!(
        downloads
            .snapshot_for(first.id())
            .unwrap()
            .display_name
            .as_deref(),
        Some("Fixture actual resource")
    );
    assert_eq!(downloads.snapshot_for(queued.id()).unwrap().stage, "queued");
    assert!(downloads.snapshot_for("unknown-fixture-id").is_none());
    assert!(downloads.cancel_and_wait(None).is_err());
    assert!(!second.cancellation_token().load(Ordering::SeqCst));
    let id = first.id().to_owned();
    let cancel = first.cancellation_token();
    let worker = thread::spawn(move || {
        while !cancel.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        first.finish(TaskOutcome::Failed(crate::tasks::CANCELLED.into()));
    });
    let cancelled = downloads.cancel_and_wait(Some(&id)).unwrap();
    worker.join().unwrap();
    assert_eq!(cancelled.task_id.as_deref(), Some(id.as_str()));
    assert_eq!(cancelled.stage, "cancelled");
    assert_eq!(downloads.snapshot().task_id.as_deref(), Some(second.id()));
    assert!(!second.cancellation_token().load(Ordering::SeqCst));
    queued.wait_turn().unwrap();
    complete(&queued);
    complete(&second);
    assert!(
        downloads.list_snapshot().revision.parse::<u64>().unwrap()
            > list.revision.parse::<u64>().unwrap()
    );
}

#[test]
fn vanilla_preflight_runs_after_queued_turn_and_preserves_real_admission_error() {
    let f = Fixture::new();
    let root = f.root("game");
    let tasks = Arc::new(Tasks::new());
    let downloads = Arc::new(Downloads::new(tasks.clone()));
    let blocker = tasks
        .admit(target("game", &root), TaskKind::InstanceReset)
        .unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let seen = called.clone();
    let (entered, observed) = mpsc::channel();
    let id = downloads
        .start(
            root.clone(),
            "game".into(),
            request(),
            TaskScope::root(&root).unwrap(),
            move |_| {
                seen.store(true, Ordering::SeqCst);
                entered.send(()).unwrap();
                Err("Fixture root selection changed".into())
            },
            |_| panic!("A refused preflight cannot commit an install"),
        )
        .unwrap();
    assert_eq!(downloads.snapshot_for(&id).unwrap().stage, "queued");
    assert!(!called.load(Ordering::SeqCst));
    complete(&blocker);
    observed.recv_timeout(Duration::from_secs(2)).unwrap();
    let ended = tasks.wait_terminal(&id).unwrap();
    assert_eq!(ended.stage, TaskStage::Error);
    assert_eq!(
        ended.error.as_deref(),
        Some("Fixture root selection changed")
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn cancelling_queued_vanilla_drops_preflight_captures_before_terminal_reply() {
    let f = Fixture::new();
    let root = f.root("game");
    let tasks = Arc::new(Tasks::new());
    let downloads = Arc::new(Downloads::new(tasks.clone()));
    let blocker = tasks
        .admit(target("game", &root), TaskKind::InstanceReset)
        .unwrap();
    let released = Arc::new(AtomicBool::new(false));
    struct Prepared(Arc<AtomicBool>);
    impl Drop for Prepared {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let prepared = Prepared(released.clone());
    let id = downloads
        .start(
            root.clone(),
            "game".into(),
            request(),
            TaskScope::root(&root).unwrap(),
            move |_| {
                let _captured = prepared;
                panic!("Cancelled queued work must not enter its preflight")
            },
            |_| panic!("Cancelled queued work must not commit"),
        )
        .unwrap();
    assert_eq!(downloads.snapshot_for(&id).unwrap().stage, "queued");
    let response = downloads.cancel_and_wait(Some(&id)).unwrap();
    assert_eq!(response.stage, "cancelled");
    assert!(released.load(Ordering::SeqCst));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert!(!blocker.cancellation_token().load(Ordering::SeqCst));
    complete(&blocker);
}

#[test]
fn java_preparation_failure_keeps_published_runtime_and_original_task_outcome() {
    for cancelled in [false, true] {
        let f = Fixture::new();
        let root = f.root("game");
        let runtime = f.0.join(".pcl-rust/java/complete-runtime");
        let published = runtime.clone();
        let tasks = Arc::new(Tasks::new());
        let preparing_tasks = tasks.clone();
        let downloads = Arc::new(Downloads::new(tasks.clone()));
        let scope = TaskScope::root(&root)
            .unwrap()
            .union(TaskScope::managed_runtime_store(&f.0).unwrap())
            .unwrap();
        let id = downloads
            .start(
                root.clone(),
                "game".into(),
                request(),
                scope,
                move |task| {
                    assert!(preparing_tasks.snapshot(task.id()).unwrap().can_cancel);
                    // Model a complete managed publication followed by an
                    // acquisition recheck failure. The outer install must not
                    // clean up this independently reusable directory.
                    fs::create_dir_all(&published).unwrap();
                    fs::write(published.join("complete"), b"retained runtime").unwrap();
                    if cancelled {
                        task.cancellation_token().store(true, Ordering::SeqCst);
                        Err(crate::java_download::CANCELLED.into())
                    } else {
                        Err("Fixture Java binding changed".into())
                    }
                },
                |_| panic!("A failed runtime preparation cannot publish a game"),
            )
            .unwrap();
        let ended = tasks.wait_terminal(&id).unwrap();
        assert_eq!(
            ended.stage,
            if cancelled {
                TaskStage::Cancelled
            } else {
                TaskStage::Error
            }
        );
        if !cancelled {
            assert_eq!(ended.error.as_deref(), Some("Fixture Java binding changed"));
        }
        assert_eq!(tasks.list().len(), 1, "Java reuses its parent task");
        assert_eq!(
            fs::read(runtime.join("complete")).unwrap(),
            b"retained runtime"
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
}
