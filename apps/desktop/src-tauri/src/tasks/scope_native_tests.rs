//! Additional real native footprint observations; the shared scheduler suites
//! beside this module exercise FIFO, cancellation, aliases and missing stores.
use super::*;
#[cfg(windows)]
use crate::tasks::{TaskKind, TaskOutcome, TaskStage, TaskTarget, Tasks};
use pcl_core::retained_fs::{Directory, Identity};
#[cfg(windows)]
use std::sync::Arc;

fn fixture() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}
fn directory(base: &Path, name: &str) -> PathBuf {
    let path = base.join(name);
    fs::create_dir_all(&path).unwrap();
    fs::canonicalize(path).unwrap()
}
#[cfg(windows)]
fn finish(task: &crate::tasks::TaskHandle) {
    task.finish(TaskOutcome::Complete {
        result: None,
        message: "native footprint fixture".into(),
        error: None,
    });
}
#[cfg(windows)]
fn submit(tasks: &Arc<Tasks>, path: &Path, scope: TaskScope) -> crate::tasks::TaskHandle {
    tasks
        .admit_queued(
            TaskTarget {
                root_id: "native-fixture".into(),
                root_path: path.to_string_lossy().into_owned(),
                instance_id: None,
            },
            TaskKind::ResourceSave,
            scope,
        )
        .unwrap()
}

#[test]
fn native_directory_and_file_keys_are_actual_complete_platform_identities() {
    let temporary = fixture();
    let root = directory(temporary.path(), "root");
    let captured = TaskScope::root(&root).unwrap();
    let Claims::Paths(locations) = captured.claims else {
        panic!("expected bounded claim");
    };
    let actual = Directory::open_absolute(&root).unwrap().identity().unwrap();
    assert_eq!(locations[0].ancestors[0].key, actual);
    #[cfg(windows)]
    assert!(matches!(
        actual,
        Identity::Windows {
            file: [_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _],
            ..
        }
    ));
    #[cfg(unix)]
    assert!(matches!(actual, Identity::Unix { .. }));
    let first = root.join("first.bin");
    let aliases = directory(temporary.path(), "aliases");
    let second = aliases.join("same-inode.bin");
    fs::write(&first, b"hardlink fixture").unwrap();
    fs::hard_link(&first, &second).unwrap();
    let a = TaskScope::files(&[first]).unwrap();
    let b = TaskScope::files(&[second]).unwrap();
    assert!(
        a.conflicts(&b),
        "physical hard links must serialize across different parents"
    );
}

#[test]
fn newly_appeared_future_components_accept_directories_but_refuse_ordinary_files() {
    for final_only in [false, true] {
        let temporary = fixture();
        let project = directory(temporary.path(), "project");
        if final_only {
            fs::create_dir(project.join(".pcl-rust")).unwrap();
        }
        let scope = TaskScope::managed_runtime_store(&project).unwrap();
        assert!(!project.join(".pcl-rust/java").exists());
        let name = if final_only {
            ".pcl-rust/java"
        } else {
            ".pcl-rust"
        };
        fs::write(project.join(name), b"foreign ordinary file").unwrap();
        assert!(scope.recheck().is_err());
        assert_eq!(
            fs::read(project.join(name)).unwrap(),
            b"foreign ordinary file"
        );
    }
    let temporary = fixture();
    let project = directory(temporary.path(), "project");
    let waiting = TaskScope::managed_runtime_store(&project).unwrap();
    fs::create_dir_all(project.join(".pcl-rust/java/runtime")).unwrap();
    waiting.recheck().unwrap();
    let current = TaskScope::managed_runtime_store(&project).unwrap();
    assert!(waiting.conflicts(&current));
    assert!(current.conflicts(&waiting));
}

#[test]
fn managed_capture_and_future_checks_remain_observational() {
    let temporary = fixture();
    let project = directory(temporary.path(), "project");
    let before = fs::read_dir(&project).unwrap().count();
    let scope = TaskScope::managed_runtime_store(&project).unwrap();
    scope.recheck().unwrap();
    assert_eq!(fs::read_dir(&project).unwrap().count(), before);
    assert!(!project.join(".pcl-rust").exists());
    let input = Directory::open_absolute(&project)
        .unwrap()
        .try_clone_file()
        .unwrap();
    assert!(native::check_future_directory(&input, Path::new("../outside")).is_err());
    assert!(!temporary.path().join("outside").exists());
}

#[cfg(windows)]
#[test]
fn windows_missing_case_aliases_queue_but_sibling_basenames_keep_their_turn() {
    let temporary = fixture();
    let output = directory(temporary.path(), "output");
    let upper = TaskScope::files(&[output.join("Ä-MISSING.ZIP")]).unwrap();
    let lower = TaskScope::files(&[output.join("ä-missing.zip")]).unwrap();
    assert!(upper.conflicts(&lower));
    let other = TaskScope::files(&[output.join("other.zip")]).unwrap();
    assert!(!upper.conflicts(&other));
    let tasks = Arc::new(Tasks::new());
    let first = submit(&tasks, &output, upper);
    first.wait_turn().unwrap();
    let same = submit(&tasks, &output, lower);
    let sibling = submit(&tasks, &output, other);
    sibling.wait_turn().unwrap();
    assert_eq!(tasks.snapshot(same.id()).unwrap().stage, TaskStage::Queued);
    finish(&first);
    same.wait_turn().unwrap();
    finish(&same);
    finish(&sibling);
    assert_eq!(fs::read_dir(output).unwrap().count(), 0);
}

#[cfg(windows)]
#[test]
fn windows_drive_prefixes_are_explicit_and_registered_junction_aliases_still_coordinate() {
    let temporary = fixture();
    let project = directory(temporary.path(), "project");
    let plain = project
        .to_string_lossy()
        .strip_prefix(r"\\?\")
        .unwrap()
        .to_owned();
    let plain_scope = TaskScope::root(Path::new(&plain)).unwrap();
    let verbatim_scope = TaskScope::root(&project).unwrap();
    assert!(plain_scope.conflicts(&verbatim_scope));
    let alias = temporary.path().join("project-alias");
    test_links::symlink(&project, &alias).unwrap();
    assert!(TaskScope::root(&alias).unwrap().conflicts(&verbatim_scope));
    assert!(TaskScope::managed_runtime_store(&alias).is_err());
    for path in [
        r"C:relative",
        r"\implicit-drive-root",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1",
        r"\\.\C:\",
    ] {
        assert!(
            TaskScope::root(Path::new(path)).is_err(),
            "unsupported path: {path}"
        );
    }
    assert!(!project.join(".pcl-rust").exists());
}
