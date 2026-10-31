//! Synthetic scheduling probes for combined game/runtime ownership. Scope
//! capture is observational; only fixture code below creates directories.
#[cfg(windows)]
use super::test_links::symlink;
use super::*;
use crate::tasks::{TaskHandle, TaskKind, TaskOutcome, TaskStage, TaskTarget, Tasks, CANCELLED};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::sync::Arc;

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
    fn directory(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
}

fn submit(tasks: &Arc<Tasks>, path: &Path, kind: TaskKind, scope: TaskScope) -> TaskHandle {
    tasks
        .admit_queued(
            TaskTarget {
                root_id: path.file_name().unwrap().to_string_lossy().into_owned(),
                root_path: path.display().to_string(),
                instance_id: None,
            },
            kind,
            scope,
        )
        .unwrap()
}
fn complete(task: &TaskHandle) {
    assert!(task
        .finish(TaskOutcome::Complete {
            result: None,
            message: "Fixture complete".into(),
            error: None,
        })
        .is_some());
}
fn queued(tasks: &Tasks, task: &TaskHandle) -> bool {
    tasks.snapshot(task.id()).unwrap().stage == TaskStage::Queued
}
fn combined(game: &Path, project: &Path) -> TaskScope {
    TaskScope::root(game)
        .unwrap()
        .union(TaskScope::managed_runtime_store(project).unwrap())
        .unwrap()
}

#[test]
fn combined_claims_serialize_runtime_users_without_blocking_unrelated_tasks() {
    let f = Fixture::new();
    let project = f.directory("project");
    let game_a = f.directory("game-a");
    let game_b = f.directory("game-b");
    let resources = f.directory("resources");
    let plain_game = f.directory("plain-game");
    let tasks = Arc::new(Tasks::new());
    let first = submit(
        &tasks,
        &game_a,
        TaskKind::Install,
        combined(&game_a, &project),
    );
    first.wait_turn().unwrap();
    let second = submit(
        &tasks,
        &game_b,
        TaskKind::ModpackInstall,
        combined(&game_b, &project),
    );
    let resource = submit(
        &tasks,
        &resources,
        TaskKind::ResourceDownload,
        TaskScope::root(&resources).unwrap(),
    );
    resource.wait_turn().unwrap();
    let no_processors = submit(
        &tasks,
        &plain_game,
        TaskKind::Install,
        TaskScope::root(&plain_game).unwrap(),
    );
    no_processors.wait_turn().unwrap();
    assert!(queued(&tasks, &second));
    assert_eq!(tasks.running_all().len(), 3);
    assert!(!project.join(".pcl-rust").exists());

    // This is the earlier worker's store publication, not scope capture. The
    // queued missing-store claim must remain compatible with a new exact one.
    let store = project.join(".pcl-rust/java");
    fs::create_dir_all(&store).unwrap();
    let explicit = submit(
        &tasks,
        &store,
        TaskKind::JavaInstall,
        TaskScope::managed_runtime_store(&project).unwrap(),
    );
    assert!(queued(&tasks, &explicit));
    complete(&first);
    second.wait_turn().unwrap();
    assert!(queued(&tasks, &explicit));
    complete(&second);
    explicit.wait_turn().unwrap();
    complete(&explicit);
    complete(&resource);
    complete(&no_processors);
    assert!(tasks.active_all().is_empty());
}

#[test]
fn explicit_java_download_blocks_automatic_install_but_sibling_outputs_run() {
    let f = Fixture::new();
    let project = f.directory("project");
    let game = f.directory("game");
    let outputs = f.directory("outputs");
    let tasks = Arc::new(Tasks::new());
    let explicit = submit(
        &tasks,
        &project,
        TaskKind::JavaInstall,
        TaskScope::managed_runtime_store(&project).unwrap(),
    );
    explicit.wait_turn().unwrap();
    let automatic = submit(&tasks, &game, TaskKind::Install, combined(&game, &project));
    let first = submit(
        &tasks,
        &outputs,
        TaskKind::ResourceSave,
        TaskScope::files(&[outputs.join("a.zip")]).unwrap(),
    );
    let second = submit(
        &tasks,
        &outputs,
        TaskKind::ToolboxDownload,
        TaskScope::files(&[outputs.join("b.zip")]).unwrap(),
    );
    first.wait_turn().unwrap();
    second.wait_turn().unwrap();
    assert!(queued(&tasks, &automatic));
    assert_eq!(tasks.running_all().len(), 3);
    complete(&explicit);
    automatic.wait_turn().unwrap();
    assert!(!project.join(".pcl-rust").exists());
    complete(&automatic);
    complete(&first);
    complete(&second);
}

#[test]
fn cancelled_joint_claim_keeps_both_turns_until_worker_cleanup_finishes() {
    let f = Fixture::new();
    let project = f.directory("project");
    let game = f.directory("game");
    let tasks = Arc::new(Tasks::new());
    let blocker = submit(
        &tasks,
        &project,
        TaskKind::JavaInstall,
        TaskScope::managed_runtime_store(&project).unwrap(),
    );
    blocker.wait_turn().unwrap();
    let cancelled = submit(&tasks, &game, TaskKind::Install, combined(&game, &project));
    let game_writer = submit(
        &tasks,
        &game,
        TaskKind::ResourceDownload,
        TaskScope::root(&game).unwrap(),
    );
    let runtime_writer = submit(
        &tasks,
        &project,
        TaskKind::JavaInstall,
        TaskScope::managed_runtime_store(&project).unwrap(),
    );
    tasks.cancel(cancelled.id()).unwrap();
    assert_eq!(cancelled.wait_turn().unwrap_err(), CANCELLED);
    complete(&blocker);
    assert!(queued(&tasks, &game_writer));
    assert!(queued(&tasks, &runtime_writer));
    assert!(cancelled
        .finish(TaskOutcome::Failed(CANCELLED.into()))
        .is_some());
    game_writer.wait_turn().unwrap();
    runtime_writer.wait_turn().unwrap();
    assert_eq!(tasks.running_all().len(), 2);
    complete(&game_writer);
    complete(&runtime_writer);
}

#[test]
fn an_existing_runtime_directory_cannot_be_replaced_during_queue_wait() {
    let f = Fixture::new();
    let project = f.directory("project");
    let store = f.directory("project/.pcl-rust/java");
    let game = f.directory("game");
    let tasks = Arc::new(Tasks::new());
    let blocker = submit(
        &tasks,
        &store,
        TaskKind::JavaInstall,
        TaskScope::managed_runtime_store(&project).unwrap(),
    );
    blocker.wait_turn().unwrap();
    let waiting = submit(&tasks, &game, TaskKind::Install, combined(&game, &project));
    fs::rename(&store, project.join(".pcl-rust/original-java")).unwrap();
    fs::create_dir(&store).unwrap();
    complete(&blocker);
    assert!(waiting.wait_turn().unwrap_err().contains("身份已变化"));
    waiting.finish(TaskOutcome::Error("Fixture changed store".into()));
}

#[test]
fn a_missing_runtime_store_retains_its_existing_ancestor_binding() {
    let f = Fixture::new();
    let project = f.directory("project");
    let tasks = Arc::new(Tasks::new());
    let blocker = submit(
        &tasks,
        &project,
        TaskKind::JavaInstall,
        TaskScope::managed_runtime_store(&project).unwrap(),
    );
    blocker.wait_turn().unwrap();
    let waiting = submit(
        &tasks,
        &project,
        TaskKind::JavaInstall,
        TaskScope::managed_runtime_store(&project).unwrap(),
    );
    fs::rename(&project, f.path.join("original-project")).unwrap();
    fs::create_dir(&project).unwrap();
    complete(&blocker);
    assert!(waiting.wait_turn().unwrap_err().contains("身份已变化"));
    waiting.finish(TaskOutcome::Error("Fixture changed ancestor".into()));
    assert!(!project.join(".pcl-rust").exists());
}

#[test]
fn a_new_symlink_never_satisfies_a_missing_runtime_store_claim() {
    for final_only in [false, true] {
        let f = Fixture::new();
        let project = f.directory("project");
        let foreign = f.directory("foreign/java");
        if final_only {
            f.directory("project/.pcl-rust");
        }
        let tasks = Arc::new(Tasks::new());
        let blocker = submit(
            &tasks,
            &project,
            TaskKind::JavaInstall,
            TaskScope::managed_runtime_store(&project).unwrap(),
        );
        blocker.wait_turn().unwrap();
        let waiting = submit(
            &tasks,
            &project,
            TaskKind::JavaInstall,
            TaskScope::managed_runtime_store(&project).unwrap(),
        );
        if final_only {
            symlink(&foreign, project.join(".pcl-rust/java")).unwrap();
        } else {
            symlink(foreign.parent().unwrap(), project.join(".pcl-rust")).unwrap();
        }
        complete(&blocker);
        assert!(waiting.wait_turn().is_err());
        assert!(TaskScope::managed_runtime_store(&project).is_err());
        waiting.finish(TaskOutcome::Error("Fixture symlink appeared".into()));
        assert!(fs::read_dir(&foreign).unwrap().next().is_none());
    }
}

#[test]
fn existing_store_aliases_conflict_with_the_combined_physical_footprint() {
    let f = Fixture::new();
    let project = f.directory("project");
    let store = f.directory("project/.pcl-rust/java");
    let alias = f.path.join("runtime-alias");
    symlink(&store, &alias).unwrap();
    let game = f.directory("game");
    let tasks = Arc::new(Tasks::new());
    let physical = submit(
        &tasks,
        &alias,
        TaskKind::ResourceDownload,
        TaskScope::root(&alias).unwrap(),
    );
    physical.wait_turn().unwrap();
    let automatic = submit(&tasks, &game, TaskKind::Install, combined(&game, &project));
    assert!(queued(&tasks, &automatic));
    complete(&physical);
    automatic.wait_turn().unwrap();
    complete(&automatic);
}

#[test]
fn union_deduplicates_bounds_and_preserves_global_ownership() {
    let f = Fixture::new();
    let outputs = f.directory("outputs");
    let files = TaskScope::files(
        &(0..MAX_FILES)
            .map(|n| outputs.join(format!("{n}.zip")))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let same = files.clone().union(files.clone()).unwrap();
    assert!(matches!(same.claims, Claims::Paths(ref paths) if paths.len() == MAX_FILES));
    assert!(files
        .clone()
        .union(TaskScope::root(&outputs).unwrap())
        .is_err());
    assert!(files.union(TaskScope::global()).unwrap().is_global());
    assert!(TaskScope::global()
        .union(TaskScope::root(&outputs).unwrap())
        .unwrap()
        .is_global());
}

#[test]
fn managed_store_capture_rejects_unsafe_paths_without_creating_anything() {
    let f = Fixture::new();
    let project = f.directory("project");
    let alias = f.path.join("project-alias");
    symlink(&project, &alias).unwrap();
    assert!(TaskScope::managed_runtime_store(&alias).is_err());
    // PathBuf::join normalizes '..' on Windows verbatim paths, so build the
    // literal invalid input without path joining before testing admission.
    let mut spelling = project.as_os_str().to_owned();
    spelling.push(std::path::MAIN_SEPARATOR_STR);
    spelling.push("..");
    spelling.push(std::path::MAIN_SEPARATOR_STR);
    spelling.push("project");
    let parent_hop = PathBuf::from(spelling);
    assert!(parent_hop
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir)));
    assert!(TaskScope::managed_runtime_store(&parent_hop).is_err());
    assert!(TaskScope::managed_runtime_store(Path::new("relative")).is_err());
    assert!(TaskScope::managed_runtime_store(&project.join("x".repeat(MAX_PATH_BYTES))).is_err());
    assert!(!project.join(".pcl-rust").exists());
    fs::write(project.join(".pcl-rust"), b"Fixture ordinary file").unwrap();
    assert!(TaskScope::managed_runtime_store(&project).is_err());
    assert_eq!(
        fs::read(project.join(".pcl-rust")).unwrap(),
        b"Fixture ordinary file"
    );
}
