use super::*;
use std::fs::{self, File};
#[cfg(unix)]
use std::{
    ffi::CString,
    os::unix::{ffi::OsStrExt, fs::symlink},
};
#[cfg(windows)]
#[path = "../../../../../crates/core/src/file_ops/junction_fixture.rs"]
mod junction_fixture;
fn directory_alias(source: &Path, target: &Path) {
    #[cfg(unix)]
    symlink(source, target).unwrap();
    #[cfg(windows)]
    junction_fixture::create(source, target);
}
fn remove_directory_alias(path: &Path) {
    #[cfg(unix)]
    fs::remove_file(path).unwrap();
    #[cfg(windows)]
    fs::remove_dir(path).unwrap();
}
fn file_identity(path: &Path) -> pcl_core::retained_fs::Identity {
    pcl_core::retained_fs::metadata(&File::open(path).unwrap())
        .unwrap()
        .identity
}
fn private_file(parent: &Path, name: &str, bytes: &[u8]) {
    use pcl_core::retained_fs::{Directory, Leaf};
    let directory = Directory::open_absolute(parent).unwrap();
    let mut file = directory.create_file(&Leaf::new(name).unwrap()).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    project: PathBuf,
    _temporary: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        use pcl_core::retained_fs::{Directory, Leaf, Traversal};
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap();
        fs::create_dir_all(path.join("game/versions/sample/saves/world")).unwrap();
        let parent = Directory::open_absolute(&path).unwrap();
        let project = parent
            .create_private(&Leaf::new("project").unwrap(), Traversal::AllowMounts)
            .unwrap();
        project
            .create_private(&Leaf::new(".pcl-rust").unwrap(), Traversal::AllowMounts)
            .unwrap();
        let root = path.join("game");
        let project = path.join("project");
        fs::write(
            root.join("versions/sample/sample.json"),
            br#"{"id":"sample","jar":"sample"}"#,
        )
        .unwrap();
        fs::write(root.join("versions/sample/sample.jar"), b"client jar").unwrap();
        fs::write(
            root.join("versions/sample/saves/world/level.dat"),
            b"world data",
        )
        .unwrap();
        for (name, data) in [
            ("libraries/shared.jar", b"shared jar".as_slice()),
            ("assets/indexes/shared.json", b"asset index"),
            ("mods/shared.jar", b"global mod"),
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
        let result = Self {
            path,
            root,
            project,
            _temporary: temporary,
        };
        result.write_settings(result.settings());
        private_file(
            &result.project.join(".pcl-rust"),
            "instance-metadata.json",
            br#"{"kept":"sample metadata"}"#,
        );
        fs::create_dir_all(result.project.join(".pcl-rust/export-presets")).unwrap();
        fs::write(
            result.project.join(".pcl-rust/export-presets/sample.json"),
            b"sample preset",
        )
        .unwrap();
        result
    }
    fn source(&self) -> PathBuf {
        self.root.join("versions/sample")
    }
    fn settings(&self) -> Value {
        json!({"schema_version":3,"active_root_id":"root-fixture","player":"Player","memory_gib":6,
            "java":{"mode":"auto"},"java_paths":[],
            "roots":[{"id":"root-fixture","name":"Game","path":self.root,"selected":"sample","overrides":{"sample":8},"java_overrides":{}}]})
    }
    fn write_settings(&self, value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        let app = self.project.join(".pcl-rust");
        if !app.join("settings.json").exists() {
            private_file(&app, "settings.json", &bytes);
        } else {
            let (state, _) = instance_rename_refs::store_snapshot(
                &self.project,
                "settings.json",
                2 * 1024 * 1024,
            )
            .unwrap();
            instance_rename_refs::write_store_checked(
                &self.project,
                "settings.json",
                &bytes,
                &state,
                2 * 1024 * 1024,
            )
            .unwrap();
        }
    }
    fn settings_bytes(&self) -> Vec<u8> {
        instance_rename_refs::store_snapshot(&self.project, "settings.json", 2 * 1024 * 1024)
            .unwrap()
            .1
            .unwrap()
    }
    fn plan(&self) -> DeletePlan {
        prepare(&self.root, &self.project, "root-fixture", "sample").unwrap()
    }
    fn delete(&self) -> DeleteHistoryEntry {
        execute(
            &self.root,
            &self.project,
            self.plan(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        history(&self.root, &self.project, "root-fixture")
            .unwrap()
            .remove(0)
    }
    fn staged(&self) -> (Dir, Dir, Dir, Record) {
        let plan = self.plan();
        let root = Dir::open(&self.root).unwrap();
        let store = storage(&root, true).unwrap().unwrap();
        let name = nonce();
        let operation = store.create(&name).unwrap();
        let journal = Journal {
            schema: journal_schema(),
            operation_id: name,
            root_id: plan.root_id,
            id: plan.id,
            root: plan.root,
            project: plan.project,
            root_key: plan.root_key,
            project_key: plan.project_key,
            versions_key: plan.versions_key,
            storage_key: store.key().unwrap(),
            operation_key: operation.key().unwrap(),
            created_ms: 1,
            state: State::Prepared,
            tree: plan.tree,
            marker_cleared: false,
            generation: 0,
            previous_hash: None,
        };
        let mut record = Record {
            journal,
            token: None,
        };
        write_record(&operation, &mut record).unwrap();
        create_marker(&record.journal).unwrap();
        (root, store, operation, record)
    }
    fn restore(&self, entry: &DeleteHistoryEntry) -> Result<Value> {
        undo(
            &self.root,
            &self.project,
            "root-fixture",
            &entry.operation_id,
            entry.revision.as_ref().unwrap(),
            &AtomicBool::new(false),
            |_| {},
        )
    }
}

#[test]
fn deletion_and_restore_preserve_content_inodes_shared_resources_and_launcher_references() {
    let f = Fixture::new();
    let world = f.source().join("saves/world/level.dat");
    let inode = file_identity(&world);
    let settings = f.settings_bytes();
    let metadata = fs::read(f.project.join(".pcl-rust/instance-metadata.json")).unwrap();
    let entry = f.delete();
    assert!(entry.can_restore);
    assert!(!f.source().exists());
    assert!(ensure_ready(&f.root).is_ok());
    assert!(ensure_name_available(&f.root, "sample").is_err());
    assert!(ensure_name_available(&f.root, "other").is_ok());
    assert_eq!(
        fs::read(f.root.join("libraries/shared.jar")).unwrap(),
        b"shared jar"
    );
    assert_eq!(
        fs::read(f.root.join("mods/shared.jar")).unwrap(),
        b"global mod"
    );
    assert_eq!(
        fs::read(f.root.join("assets/indexes/shared.json")).unwrap(),
        b"asset index"
    );
    assert_eq!(f.settings_bytes(), settings);
    assert_eq!(
        fs::read(f.project.join(".pcl-rust/instance-metadata.json")).unwrap(),
        metadata
    );
    assert_eq!(
        fs::read(f.project.join(".pcl-rust/export-presets/sample.json")).unwrap(),
        b"sample preset"
    );
    f.restore(&entry).unwrap();
    assert_eq!(file_identity(&world), inode);
    assert_eq!(fs::read(world).unwrap(), b"world data");
    assert!(ensure_name_available(&f.root, "sample").is_ok());
    assert!(history(&f.root, &f.project, "root-fixture")
        .unwrap()
        .is_empty());
}

#[test]
fn dependency_instances_are_rejected_for_inheritance_and_jar() {
    for field in ["inheritsFrom", "jar"] {
        let f = Fixture::new();
        let directory = f.root.join("versions/dependent");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("dependent.json"),
            serde_json::to_vec(&json!({"id":"dependent",field:"sample"})).unwrap(),
        )
        .unwrap();
        assert!(f.plan_error().contains(field));
        assert!(f.source().exists());
    }
}
impl Fixture {
    fn plan_error(&self) -> String {
        prepare(&self.root, &self.project, "root-fixture", "sample").unwrap_err()
    }
}

#[test]
fn stale_tree_or_new_dependency_is_rejected_before_move() {
    let f = Fixture::new();
    let plan = f.plan();
    fs::write(f.source().join("saves/world/level.dat"), b"external edit").unwrap();
    assert!(execute(&f.root, &f.project, plan, &AtomicBool::new(false), |_| {}).is_err());
    assert!(f.source().exists());
    let plan = f.plan();
    let directory = f.root.join("versions/new-dependent");
    fs::create_dir(&directory).unwrap();
    fs::write(
        directory.join("new-dependent.json"),
        br#"{"id":"new-dependent","jar":"sample"}"#,
    )
    .unwrap();
    assert!(execute(&f.root, &f.project, plan, &AtomicBool::new(false), |_| {}).is_err());
    assert!(f.source().exists());
}

#[test]
fn runtime_dependencies_include_aliases_and_manual_choices_from_other_roots() {
    let f = Fixture::new();
    let runtime = f.source().join("runtime/bin/java");
    fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    fs::write(&runtime, b"not executed").unwrap();
    let alias = f.path.join("java-alias");
    #[cfg(unix)]
    let alias_java = {
        symlink(&runtime, &alias).unwrap();
        alias.clone()
    };
    #[cfg(windows)]
    let alias_java = {
        directory_alias(runtime.parent().unwrap(), &alias);
        alias.join("java")
    };
    let mut settings = f.settings();
    settings["java_paths"] = json!([alias_java]);
    f.write_settings(settings);
    assert!(f.plan_error().contains("Java"));
    let other = f.path.join("other-game");
    fs::create_dir(&other).unwrap();
    let mut settings = f.settings();
    settings["roots"].as_array_mut().unwrap().push(
        json!({"id":"root-other","name":"Other","path":other,"selected":"other","overrides":{},
        "java_overrides":{"other":{"mode":"manual","path":f.source().join("missing/java")}}}),
    );
    f.write_settings(settings);
    assert!(f.plan_error().contains("Java"));
}

#[test]
fn symlinks_hard_links_and_special_content_are_preserved_and_rejected() {
    let f = Fixture::new();
    directory_alias(&f.root.join("mods"), &f.source().join("external-mods"));
    assert!(prepare(&f.root, &f.project, "root-fixture", "sample").is_err());
    assert!(f.root.join("mods/shared.jar").exists());
    remove_directory_alias(&f.source().join("external-mods"));
    fs::hard_link(
        f.root.join("mods/shared.jar"),
        f.source().join("hardlink.jar"),
    )
    .unwrap();
    assert!(prepare(&f.root, &f.project, "root-fixture", "sample").is_err());
    fs::remove_file(f.source().join("hardlink.jar")).unwrap();
    #[cfg(unix)]
    {
        let pipe = CString::new(f.source().join("pipe").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        assert!(prepare(&f.root, &f.project, "root-fixture", "sample").is_err());
    }
}

#[test]
fn cancellation_before_commit_and_at_task_gate_rolls_back_and_releases_reservation() {
    let f = Fixture::new();
    let cancelled_token = AtomicBool::new(true);
    assert_eq!(
        execute(&f.root, &f.project, f.plan(), &cancelled_token, |_| {}).unwrap_err(),
        CANCELLED
    );
    let cancel = AtomicBool::new(false);
    assert_eq!(
        execute(&f.root, &f.project, f.plan(), &cancel, |p| {
            if p.phase == "committing" {
                cancel.store(true, Ordering::Relaxed)
            }
        })
        .unwrap_err(),
        CANCELLED
    );
    assert!(f.source().exists());
    assert!(pending_root(&f.project).unwrap().is_none());
    assert!(ensure_name_available(&f.root, "sample").is_ok());
}

#[test]
fn failed_commit_reports_error_and_retains_externally_changed_source() {
    let f = Fixture::new();
    let result = execute(
        &f.root,
        &f.project,
        f.plan(),
        &AtomicBool::new(false),
        |p| {
            if p.phase == "committing" {
                fs::write(f.source().join("saves/world/level.dat"), b"changed at gate").unwrap()
            }
        },
    );
    assert!(result.unwrap_err().starts_with("取消清理失败："));
    assert_eq!(
        fs::read(f.source().join("saves/world/level.dat")).unwrap(),
        b"changed at gate"
    );
    assert!(ensure_ready(&f.root).is_err());
    assert!(recover_pending(&f.root, &f.project).is_err());
    assert!(pending_root(&f.project).unwrap().is_some());
}

#[test]
fn interrupted_delete_before_and_after_move_rolls_back() {
    for move_first in [false, true] {
        let f = Fixture::new();
        let (root, _store, operation, record) = f.staged();
        if move_first {
            rename_new(
                &root.child("versions").unwrap(),
                "sample",
                &operation,
                "instance",
            )
            .unwrap()
        }
        assert!(ensure_ready(&f.root).is_err());
        assert_eq!(pending_root(&f.project).unwrap(), Some(f.root.clone()));
        recover_pending(&f.root, &f.project).unwrap();
        assert!(f.source().exists());
        assert!(ensure_ready(&f.root).is_ok());
        assert!(ensure_name_available(&f.root, "sample").is_ok());
        assert!(!operation.optional("instance").unwrap().is_some());
        assert_eq!(
            read_record(&operation, &record.journal.operation_id)
                .unwrap()
                .journal
                .state,
            State::RolledBack
        );
    }
}

#[test]
fn interrupted_restore_before_and_after_move_returns_copy_to_recovery() {
    for move_first in [false, true] {
        let f = Fixture::new();
        let entry = f.delete();
        let root = Dir::open(&f.root).unwrap();
        let store = storage(&root, false).unwrap().unwrap();
        let operation = store.child(&entry.operation_id).unwrap();
        let mut record = read_record(&operation, &entry.operation_id).unwrap();
        record.journal.state = State::Restoring;
        record.journal.marker_cleared = false;
        write_record(&operation, &mut record).unwrap();
        create_marker(&record.journal).unwrap();
        if move_first {
            rename_new(
                &operation,
                "instance",
                &root.child("versions").unwrap(),
                "sample",
            )
            .unwrap()
        }
        recover_pending(&f.root, &f.project).unwrap();
        assert!(!f.source().exists());
        assert!(operation.child("instance").is_ok());
        let refreshed = history(&f.root, &f.project, "root-fixture")
            .unwrap()
            .remove(0);
        assert!(refreshed.can_restore);
        f.restore(&refreshed).unwrap();
    }
}

#[test]
fn recovery_copy_edits_are_warned_and_preserved() {
    let f = Fixture::new();
    let entry = f.delete();
    let copy = f
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(&entry.operation_id)
        .join("instance/saves/world/level.dat");
    fs::write(&copy, b"external recovery edit").unwrap();
    let entry = history(&f.root, &f.project, "root-fixture")
        .unwrap()
        .remove(0);
    assert!(!entry.can_restore);
    assert!(entry.revision.is_none());
    assert!(entry.warning.unwrap().contains("外部修改"));
    assert!(recover_pending(&f.root, &f.project).is_err());
    assert_eq!(fs::read(copy).unwrap(), b"external recovery edit");
    assert!(!f.source().exists());
}

#[test]
fn restore_destination_conflict_preserves_both_directories_and_tombstone() {
    let f = Fixture::new();
    let entry = f.delete();
    fs::create_dir(f.source()).unwrap();
    fs::write(f.source().join("new.txt"), b"external new instance").unwrap();
    assert!(f.restore(&entry).unwrap_err().contains("占用"));
    let entry = history(&f.root, &f.project, "root-fixture")
        .unwrap()
        .remove(0);
    assert!(!entry.can_restore);
    assert!(ensure_name_available(&f.root, "sample").is_err());
    assert_eq!(
        fs::read(f.source().join("new.txt")).unwrap(),
        b"external new instance"
    );
    assert!(f
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(entry.operation_id)
        .join("instance/sample.jar")
        .exists());
}

#[test]
fn restore_confirmation_is_scoped_and_stale_generation_is_rejected() {
    let f = Fixture::new();
    let entry = f.delete();
    assert!(undo(
        &f.root,
        &f.project,
        "root-readded",
        &entry.operation_id,
        entry.revision.as_ref().unwrap(),
        &AtomicBool::new(false),
        |_| {}
    )
    .unwrap_err()
    .contains("原游戏目录标识"));
    assert!(undo(
        &f.root,
        &f.project,
        "root-fixture",
        &entry.operation_id,
        "restore:stale",
        &AtomicBool::new(false),
        |_| {}
    )
    .is_err());
    assert!(!f.source().exists());
    f.restore(&entry).unwrap();
}

#[test]
fn externally_replaced_journal_and_root_directories_are_never_adopted() {
    let f = Fixture::new();
    let (_root, _store, operation, mut record) = f.staged();
    let journal = f
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(&record.journal.operation_id)
        .join("journal.json");
    let original = fs::read(&journal).unwrap();
    fs::remove_file(&journal).unwrap();
    fs::write(&journal, &original).unwrap();
    record.journal.state = State::RolledBack;
    assert!(write_record(&operation, &mut record).is_err());
    assert_eq!(fs::read(journal).unwrap(), original);
    // Windows correctly refuses moving an ancestor while record snapshot
    // handles are live; retire those readers before testing replacement.
    drop(record);
    drop(operation);
    drop(_store);
    drop(_root);
    let moved = f.path.join("original-game");
    fs::rename(&f.root, &moved).unwrap();
    fs::create_dir_all(f.root.join("versions")).unwrap();
    assert!(recover_pending(&f.root, &f.project).is_err());
    assert!(moved.join("versions/sample").exists());
}

#[cfg(unix)]
#[test]
fn journal_exchange_crash_artifacts_are_checked_and_safely_cleaned() {
    for after_exchange in [false, true] {
        let f = Fixture::new();
        let (_root, _store, operation, record) = f.staged();
        let mut next = record.journal.clone();
        next.generation += 1;
        next.previous_hash = Some(record.token.as_ref().unwrap().hash.clone());
        next.marker_cleared = true;
        let mut file = operation.anonymous().unwrap();
        file.write_all(&serde_json::to_vec(&next).unwrap()).unwrap();
        file.sync_all().unwrap();
        let name = format!("journal-{}.next", nonce());
        operation.link(&file, &name).unwrap();
        if after_exchange {
            operation
                .exchange(&name, "journal.json", "测试日志交换失败")
                .unwrap();
        }
        recover_pending(&f.root, &f.project).unwrap();
        assert!(f.source().exists());
        assert_eq!(operation.names().unwrap(), vec!["journal.json"]);
    }
}

#[test]
fn operation_lock_is_released_explicitly_even_with_inherited_descriptor() {
    let f = Fixture::new();
    let root = Dir::open(&f.root).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let owner = lock(&store).unwrap();
    let inherited = owner.duplicate_descriptor().unwrap();
    assert!(lock(&store).is_err());
    drop(owner);
    let next = lock(&store).unwrap();
    assert!(lock(&store).is_err());
    drop(inherited);
    assert!(lock(&store).is_err());
    drop(next);
    assert!(lock(&store).is_ok());
}

#[cfg(unix)]
#[test]
fn recovery_preserves_temporary_journals_in_a_replaced_operation_directory() {
    let f = Fixture::new();
    let (_root, _store, operation, record) = f.staged();
    let mut next = record.journal.clone();
    next.generation += 1;
    next.previous_hash = Some(record.token.as_ref().unwrap().hash.clone());
    next.marker_cleared = true;
    let temporary = format!("journal-{}.next", nonce());
    let mut file = operation.anonymous().unwrap();
    file.write_all(&serde_json::to_vec(&next).unwrap()).unwrap();
    file.sync_all().unwrap();
    operation.link(&file, &temporary).unwrap();

    // A valid generation chain does not grant ownership of another directory.
    // Simulate an external replacement carrying copies of both known journals.
    let original = f
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(&next.operation_id);
    let retained = f.path.join("retained-operation");
    fs::rename(&original, &retained).unwrap();
    fs::create_dir(&original).unwrap();
    for name in ["journal.json", temporary.as_str()] {
        fs::copy(retained.join(name), original.join(name)).unwrap();
    }
    let marker = fs::read(f.project.join(".pcl-rust").join(MARKER)).unwrap();
    let temporary_bytes = fs::read(original.join(&temporary)).unwrap();

    assert!(recover_pending(&f.root, &f.project).is_err());
    assert_eq!(fs::read(original.join(temporary)).unwrap(), temporary_bytes);
    assert_eq!(
        fs::read(f.project.join(".pcl-rust").join(MARKER)).unwrap(),
        marker
    );
    assert_eq!(
        fs::read(f.source().join("saves/world/level.dat")).unwrap(),
        b"world data"
    );
}

#[test]
fn empty_setup_slots_recover_but_unknown_nonempty_slots_are_preserved() {
    let f = Fixture::new();
    let root = Dir::open(&f.root).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let empty = nonce();
    store.create(&empty).unwrap();
    assert!(ensure_ready(&f.root).is_err());
    assert_eq!(
        recover_pending(&f.root, &f.project).unwrap()["recovered"],
        1
    );
    assert!(store.stat(&empty).unwrap().is_none());
    let nonempty = nonce();
    store.create(&nonempty).unwrap();
    let path = f
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(&nonempty)
        .join("unknown");
    fs::write(&path, b"external content").unwrap();
    assert!(recover_pending(&f.root, &f.project).is_err());
    assert_eq!(fs::read(path).unwrap(), b"external content");
    assert!(f.source().exists());
}

#[test]
fn durable_delete_commit_cleans_marker_and_retains_recovery_copy() {
    let f = Fixture::new();
    let (root, _store, operation, mut record) = f.staged();
    rename_new(
        &root.child("versions").unwrap(),
        "sample",
        &operation,
        "instance",
    )
    .unwrap();
    record.journal.state = State::Deleted;
    write_record(&operation, &mut record).unwrap();
    recover_pending(&f.root, &f.project).unwrap();
    assert!(!f.source().exists());
    assert!(pending_root(&f.project).unwrap().is_none());
    let entry = history(&f.root, &f.project, "root-fixture")
        .unwrap()
        .remove(0);
    assert!(entry.can_restore);
    f.restore(&entry).unwrap();
}

#[test]
fn cancellation_of_restore_at_gate_keeps_copy_and_late_cancellation_keeps_commit() {
    let f = Fixture::new();
    let entry = f.delete();
    let cancel = AtomicBool::new(false);
    assert_eq!(
        undo(
            &f.root,
            &f.project,
            "root-fixture",
            &entry.operation_id,
            entry.revision.as_ref().unwrap(),
            &cancel,
            |p| {
                if p.phase == "committing" {
                    cancel.store(true, Ordering::Relaxed)
                }
            }
        )
        .unwrap_err(),
        CANCELLED
    );
    assert!(!f.source().exists());
    assert!(ensure_ready(&f.root).is_ok());
    assert!(pending_root(&f.project).unwrap().is_none());
    let entry = history(&f.root, &f.project, "root-fixture")
        .unwrap()
        .remove(0);
    cancel.store(false, Ordering::Relaxed);
    undo(
        &f.root,
        &f.project,
        "root-fixture",
        &entry.operation_id,
        entry.revision.as_ref().unwrap(),
        &cancel,
        |p| {
            if p.phase == "complete" {
                cancel.store(true, Ordering::Relaxed)
            }
        },
    )
    .unwrap();
    assert!(f.source().exists());
    assert!(ensure_name_available(&f.root, "sample").is_ok());
}

#[test]
fn settings_changes_at_gate_block_delete_without_erasing_runtime_references() {
    let f = Fixture::new();
    let result = execute(
        &f.root,
        &f.project,
        f.plan(),
        &AtomicBool::new(false),
        |p| {
            if p.phase == "committing" {
                let mut settings = f.settings();
                settings["java"] = json!({"mode":"manual","path":f.source().join("runtime/java")});
                f.write_settings(settings)
            }
        },
    );
    assert!(result.unwrap_err().starts_with("取消清理失败："));
    assert!(f.source().exists());
    let settings: Value = serde_json::from_slice(&f.settings_bytes()).unwrap();
    assert_eq!(settings["java"]["mode"], "manual");
    assert!(pending_root(&f.project).unwrap().is_none());
    assert!(ensure_ready(&f.root).is_ok());
}

#[test]
fn root_reregistration_keeps_history_visible_with_original_scope_warning() {
    let f = Fixture::new();
    f.delete();
    let entry = history(&f.root, &f.project, "root-readded")
        .unwrap()
        .remove(0);
    assert_eq!(entry.root_id, "root-readded");
    assert_eq!(entry.original_root_id, "root-fixture");
    assert!(!entry.can_restore);
    assert!(entry.warning.unwrap().contains("原游戏目录标识"));
    assert!(reserved_names(&f.root).unwrap().contains("sample"));
    fs::create_dir(f.source()).unwrap();
    assert!(prepare(&f.root, &f.project, "root-fixture", "sample")
        .unwrap_err()
        .contains("保留"));
}

#[test]
fn restore_and_recovery_refuse_an_independent_pending_rename() {
    let f = Fixture::new();
    let entry = f.delete();
    let identity = |path: &Path| {
        let file = if path.is_dir() {
            pcl_core::retained_fs::Directory::open_absolute(path)
                .unwrap()
                .try_clone_file()
                .unwrap()
        } else {
            File::open(path).unwrap()
        };
        match pcl_core::retained_fs::metadata(&file).unwrap().identity {
            #[cfg(unix)]
            pcl_core::retained_fs::Identity::Unix { device, inode } => {
                json!({"device":device,"inode":inode})
            }
            #[cfg(windows)]
            pcl_core::retained_fs::Identity::Windows { volume, file } => {
                json!({"format":"windows-file-id-128-v1","volume":volume,"file_id":file})
            }
            _ => panic!("native fixture identity"),
        }
    };
    let marker = f.project.join(".pcl-rust/instance-rename-pending.json");
    private_file(
        marker.parent().unwrap(),
        marker.file_name().unwrap().to_str().unwrap(),
        b"pending",
    );
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&marker)
        .unwrap();
    let data = json!({"schema_version":if cfg!(windows) {2} else {1},"root_path":f.root,"root_id":"root-fixture","root_identity":identity(&f.root),
        "project_identity":identity(&f.project),"file_identity":identity(&marker),"old_id":"other","new_id":"renamed",
        "operation_id":"n-1-2-3","revision":"0".repeat(64)});
    file.write_all(&serde_json::to_vec(&data).unwrap()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(f.restore(&entry).unwrap_err().contains("重命名"));
    assert!(recover_pending(&f.root, &f.project)
        .unwrap_err()
        .contains("重命名"));
    assert_eq!(
        fs::read(&marker).unwrap(),
        serde_json::to_vec(&data).unwrap()
    );
    assert!(!f.source().exists());
    fs::remove_file(marker).unwrap();
    f.restore(&entry).unwrap();
}

#[cfg(windows)]
fn journal_document(operation: &Dir) -> crate::launcher_local::filesystem::Dir {
    crate::launcher_local::filesystem::Dir(operation.document_file().unwrap())
}

#[cfg(windows)]
#[test]
fn windows_physical_case_alias_dependencies_block_both_reference_fields_before_setup() {
    for field in ["inheritsFrom", "jar"] {
        let f = Fixture::new();
        let versions = Dir::open(&f.root).unwrap().child("versions").unwrap();
        assert_eq!(
            versions.child("SAMPLE").unwrap().key().unwrap(),
            versions.child("sample").unwrap().key().unwrap()
        );
        drop(versions);
        let dependent = f.root.join("versions/dependent");
        fs::create_dir(&dependent).unwrap();
        let bytes = serde_json::to_vec(&json!({"id":"dependent",field:"SAMPLE"})).unwrap();
        fs::write(dependent.join("dependent.json"), &bytes).unwrap();
        assert!(f.plan_error().contains(field));
        assert_eq!(fs::read(dependent.join("dependent.json")).unwrap(), bytes);
        assert!(f.source().exists());
        assert!(!f.root.join(".pcl-rust").exists());
    }
}

#[cfg(windows)]
#[test]
fn windows_deleted_name_reserves_case_aliases_in_both_scan_and_writer_admission() {
    let f = Fixture::new();
    let entry = f.delete();
    let reserved = reserved_names(&f.root).unwrap();
    assert!(reserved.contains("sample"));
    assert!(reserved.contains("SAMPLE"));
    assert!(reserved.contains("SaMpLe"));
    assert!(!reserved.contains("sample-other"));
    assert!(ensure_name_available(&f.root, "SAMPLE").is_err());
    assert!(ensure_name_available(&f.root, "sample-other").is_ok());
    f.restore(&entry).unwrap();
    assert!(!reserved_names(&f.root).unwrap().contains("SAMPLE"));
    assert!(ensure_name_available(&f.root, "SAMPLE").is_ok());
}

#[cfg(windows)]
#[test]
fn windows_journal_cas_preserves_physical_fence_chains_logical_hash_and_rejects_stale_record() {
    use crate::launcher_local::filesystem::documents::{Document, DocumentPresence};
    let f = Fixture::new();
    let (_root, _store, operation, mut record) = f.staged();
    let name = record.journal.operation_id.clone();
    let physical = f
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(&name)
        .join("journal.json");
    let fence = fs::read(&physical).unwrap();
    let fence_id = file_identity(&physical);
    let before = serde_json::to_vec(&record.journal).unwrap();
    let mut stale = read_record(&operation, &name).unwrap();
    record.journal.state = State::RolledBack;
    write_record(&operation, &mut record).unwrap();
    assert_eq!(record.journal.schema, 2);
    assert_eq!(record.journal.generation, 1);
    assert_eq!(record.journal.previous_hash, Some(hash(&before)));
    let committed = serde_json::to_vec(&record.journal).unwrap();
    assert_eq!(fs::read(&physical).unwrap(), fence);
    assert_eq!(file_identity(&physical), fence_id);
    stale.journal.state = State::Deleted;
    assert!(write_record(&operation, &mut stale).is_err());
    let local = journal_document(&operation);
    let document = Document::new(&local, "journal.json", MAX_JOURNAL as u64).unwrap();
    let mut state = document.state().unwrap();
    assert_eq!(state.presence(), DocumentPresence::Present);
    assert_eq!(document.read_state(&mut state).unwrap().unwrap(), committed);
    assert!(document
        .auxiliary_names()
        .unwrap()
        .iter()
        .all(|name| operation.names().unwrap().contains(name)));
    drop(state);
    recover_pending(&f.root, &f.project).unwrap();
    assert!(f.source().exists());
    assert!(pending_root(&f.project).unwrap().is_none());
}

#[cfg(windows)]
#[test]
fn windows_may_after_terminal_delete_cas_reopens_committed_state_without_rollback() {
    let f = Fixture::new();
    // Enable the hook only after the physical game move, at the terminal CAS.
    // The normal service must retain Deleted in memory and must not run its
    // Prepared rollback when publication returns an uncertain error.
    let result = execute(
        &f.root,
        &f.project,
        f.plan(),
        &AtomicBool::new(false),
        |progress| {
            if progress.phase == "committing" {
                journal_windows::inject_post_compare_failure();
            }
        },
    );
    assert!(result.unwrap_err().contains("MayHavePublished"));
    assert!(!f.source().exists());
    assert!(pending_root(&f.project).unwrap().is_some());
    let root = Dir::open(&f.root).unwrap();
    let store = storage(&root, false).unwrap().unwrap();
    let name = store
        .names()
        .unwrap()
        .into_iter()
        .find(|name| name != ".lock")
        .unwrap();
    let operation = store.child(&name).unwrap();
    assert_eq!(
        read_record(&operation, &name).unwrap().journal.state,
        State::Deleted
    );
    assert!(operation.child("instance").is_ok());
    recover_pending(&f.root, &f.project).unwrap();
    assert!(!f.source().exists());
    let entry = history(&f.root, &f.project, "root-fixture")
        .unwrap()
        .remove(0);
    f.restore(&entry).unwrap();
    assert_eq!(
        fs::read(f.source().join("saves/world/level.dat")).unwrap(),
        b"world data"
    );
}

#[cfg(windows)]
#[test]
fn windows_may_after_terminal_restore_cas_does_not_move_original_back_to_recovery() {
    let f = Fixture::new();
    let entry = f.delete();
    let original = f
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(&entry.operation_id)
        .join("instance/sample.jar");
    let original_id = file_identity(&original);
    let result = undo(
        &f.root,
        &f.project,
        "root-fixture",
        &entry.operation_id,
        entry.revision.as_ref().unwrap(),
        &AtomicBool::new(false),
        |progress| {
            if progress.phase == "committing" {
                journal_windows::inject_post_compare_failure();
            }
        },
    );
    assert!(result.unwrap_err().contains("MayHavePublished"));
    assert!(f.source().exists());
    assert_eq!(file_identity(&f.source().join("sample.jar")), original_id);
    assert!(!original.exists());
    recover_pending(&f.root, &f.project).unwrap();
    assert!(f.source().exists());
    assert!(pending_root(&f.project).unwrap().is_none());
    assert!(ensure_name_available(&f.root, "SAMPLE").is_ok());
}

#[cfg(windows)]
#[test]
fn journal_exchange_crash_artifacts_are_checked_and_safely_cleaned() {
    // Windows has an actual Document CAS rather than a .next/exchange protocol.
    // Reopen both Prepared and post-CAS RolledBack interruption points.
    for after_compare in [false, true] {
        let f = Fixture::new();
        let (_root, _store, operation, mut record) = f.staged();
        if after_compare {
            record.journal.state = State::RolledBack;
            journal_windows::inject_post_compare_failure();
            assert!(write_record(&operation, &mut record)
                .unwrap_err()
                .contains("MayHavePublished"));
        }
        recover_pending(&f.root, &f.project).unwrap();
        let loaded = read_record(&operation, &record.journal.operation_id).unwrap();
        assert_eq!(loaded.journal.state, State::RolledBack);
        assert!(loaded.journal.marker_cleared);
        assert!(f.source().exists());
        assert!(!operation
            .names()
            .unwrap()
            .iter()
            .any(|name| name.ends_with(".next")));
    }
}

#[cfg(windows)]
#[test]
fn windows_logical_tombstone_and_unknown_auxiliary_namespace_never_become_empty_setup() {
    use crate::launcher_local::filesystem::documents::Document;
    let f = Fixture::new();
    let (_root, _store, operation, record) = f.staged();
    let local = journal_document(&operation);
    let document = Document::new(&local, "journal.json", MAX_JOURNAL as u64).unwrap();
    let state = document.state().unwrap();
    document.compare_write(&state, None).unwrap();
    assert!(read_record(&operation, &record.journal.operation_id).is_err());
    assert!(recover_pending(&f.root, &f.project).is_err());
    assert!(f.source().exists());
    assert!(pending_root(&f.project).unwrap().is_some());
    assert!(!operation.names().unwrap().is_empty());
    let other = Fixture::new();
    let (_root, _store, operation, record) = other.staged();
    let unknown = other
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(&record.journal.operation_id)
        .join(".pcl-document-not-authorized");
    fs::create_dir(&unknown).unwrap();
    fs::write(unknown.join("keep"), b"unknown evidence").unwrap();
    assert!(read_record(&operation, &record.journal.operation_id).is_err());
    assert!(recover_pending(&other.root, &other.project).is_err());
    assert_eq!(fs::read(unknown.join("keep")).unwrap(), b"unknown evidence");
}

#[cfg(windows)]
#[test]
fn recovery_preserves_temporary_journals_in_a_replaced_operation_directory() {
    let f = Fixture::new();
    let (root, store, operation, record) = f.staged();
    let operation_id = record.journal.operation_id.clone();
    let original = f.root.join(".pcl-rust").join(STORE).join(&operation_id);
    let retained = f.path.join("retained-operation");
    assert!(
        fs::rename(&original, &retained).is_err(),
        "a live payload snapshot holds an actual descendant"
    );
    drop(record);
    drop(operation);
    drop(store);
    drop(root);
    fs::rename(&original, &retained).unwrap();
    fs::create_dir(&original).unwrap();
    // A copied permanent base is not authority for a replacement operation.
    fs::copy(retained.join("journal.json"), original.join("journal.json")).unwrap();
    fs::write(
        original.join("journal-d-1-2-3.next"),
        b"unknown next evidence",
    )
    .unwrap();
    let marker = fs::read(f.project.join(".pcl-rust").join(MARKER)).unwrap();
    assert!(recover_pending(&f.root, &f.project).is_err());
    assert_eq!(
        fs::read(original.join("journal-d-1-2-3.next")).unwrap(),
        b"unknown next evidence"
    );
    assert_eq!(
        fs::read(f.project.join(".pcl-rust").join(MARKER)).unwrap(),
        marker
    );
    assert_eq!(
        fs::read(f.source().join("saves/world/level.dat")).unwrap(),
        b"world data"
    );
}

#[cfg(windows)]
#[test]
fn windows_external_descendant_handle_blocks_move_and_source_is_retained_for_retry() {
    let f = Fixture::new();
    let held = File::open(f.source().join("saves/world/level.dat")).unwrap();
    let error = execute(
        &f.root,
        &f.project,
        f.plan(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert!(error.starts_with("取消清理失败："), "{error}");
    assert_eq!(
        fs::read(f.source().join("saves/world/level.dat")).unwrap(),
        b"world data"
    );
    assert!(pending_root(&f.project).unwrap().is_none());
    assert!(ensure_name_available(&f.root, "sample").is_ok());
    drop(held);
    let entry = f.delete();
    f.restore(&entry).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_marker_cleanup_uses_its_physical_self_identity_and_rejects_replacement() {
    let f = Fixture::new();
    let (_root, _store, operation, mut record) = f.staged();
    let path = f.project.join(".pcl-rust").join(MARKER);
    let original = fs::read(&path).unwrap();
    let marker: Marker = serde_json::from_slice(&original).unwrap();
    assert_eq!(
        serde_json::to_value(&marker.file_key).unwrap(),
        serde_json::to_value(
            &read_bytes(&app(&f.project, false).unwrap().unwrap(), MARKER, 64 * 1024)
                .unwrap()
                .0
                .stamp
                .key
        )
        .unwrap()
    );
    fs::remove_file(&path).unwrap();
    private_file(path.parent().unwrap(), MARKER, &original);
    record.journal.state = State::RolledBack;
    write_record(&operation, &mut record).unwrap();
    assert!(recover_pending(&f.root, &f.project).is_err());
    assert_eq!(fs::read(path).unwrap(), original);
    assert!(f.source().exists());
}

#[test]
fn user_instance_journal_basename_is_physical_and_keeps_original_identity() {
    let f = Fixture::new();
    let path = f.source().join("journal.json");
    fs::write(&path, b"user instance file, not a deletion record").unwrap();
    let identity = file_identity(&path);
    let entry = f.delete();
    let retained = f
        .root
        .join(".pcl-rust")
        .join(STORE)
        .join(&entry.operation_id)
        .join("instance/journal.json");
    assert_eq!(file_identity(&retained), identity);
    assert_eq!(
        fs::read(&retained).unwrap(),
        b"user instance file, not a deletion record"
    );
    f.restore(&entry).unwrap();
    assert_eq!(file_identity(&path), identity);
    assert_eq!(
        fs::read(path).unwrap(),
        b"user instance file, not a deletion record"
    );
}

#[cfg(windows)]
#[test]
fn windows_future_old_logical_journal_is_never_overwritten_or_used_for_recovery() {
    use crate::launcher_local::filesystem::documents::Document;
    let f = Fixture::new();
    let (_root, _store, operation, mut record) = f.staged();
    let local = journal_document(&operation);
    let document = Document::new(&local, "journal.json", MAX_JOURNAL as u64).unwrap();
    let mut future = record.journal.clone();
    future.schema = 3;
    let future_bytes = serde_json::to_vec(&future).unwrap();
    let state = document.state().unwrap();
    document.compare_write(&state, Some(&future_bytes)).unwrap();
    record.journal.state = State::RolledBack;
    assert!(write_record(&operation, &mut record).is_err());
    assert!(read_record(&operation, &future.operation_id).is_err());
    assert!(recover_pending(&f.root, &f.project).is_err());
    let mut actual = document.state().unwrap();
    assert_eq!(
        document.read_state(&mut actual).unwrap().unwrap(),
        future_bytes
    );
    assert!(pending_root(&f.project).unwrap().is_some());
    assert!(f.source().exists());
}

#[cfg(unix)]
#[test]
fn unix_delete_journal_and_physical_marker_keep_schema_one_and_original_key_wire() {
    let f = Fixture::new();
    let (_root, _store, operation, record) = f.staged();
    let (_, bytes) = read_bytes(&operation, "journal.json", MAX_JOURNAL).unwrap();
    let journal: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(journal["schema"], 1);
    assert_eq!(journal["generation"], 0);
    assert!(journal["previous_hash"].is_null());
    let root = fs::metadata(&f.root).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        journal["root_key"],
        json!({"dev":root.dev(),"ino":root.ino()})
    );
    let marker: Value =
        serde_json::from_slice(&fs::read(f.project.join(".pcl-rust").join(MARKER)).unwrap())
            .unwrap();
    assert_eq!(marker["schema"], 1);
    assert_eq!(marker["root_key"], journal["root_key"]);
    assert_eq!(marker["operation_id"], record.journal.operation_id);
}

#[cfg(windows)]
#[test]
fn windows_requested_case_alias_is_rejected_before_journal_marker_or_source_mutation() {
    let f = Fixture::new();
    let versions = Dir::open(&f.root).unwrap().child("versions").unwrap();
    assert!(versions.names().unwrap().contains(&"sample".to_string()));
    assert!(!versions.names().unwrap().contains(&"SAMPLE".to_string()));
    assert_eq!(
        versions.child("SAMPLE").unwrap().key().unwrap(),
        versions.child("sample").unwrap().key().unwrap()
    );
    let original = scan(&versions.child("sample").unwrap(), None).unwrap();
    drop(versions);
    let settings = f.settings_bytes();
    let error = prepare(&f.root, &f.project, "root-fixture", "SAMPLE").unwrap_err();
    assert!(error.contains("实际目录名称完全一致"), "{error}");
    assert!(!f.root.join(".pcl-rust").exists());
    assert!(!f.project.join(".pcl-rust").join(MARKER).exists());
    assert_eq!(
        scan(&Dir::open(&f.source()).unwrap(), None).unwrap(),
        original
    );
    assert_eq!(f.settings_bytes(), settings);
    assert!(ensure_name_available(&f.root, "sample").is_ok());
    assert_eq!(f.plan().id, "sample");
}

#[cfg(windows)]
#[test]
fn windows_actual_short_name_alias_when_available_is_rejected_before_mutation() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let f = Fixture::new();
    let long_name = "LongDeletionFixtureInstance";
    let source = f.root.join("versions").join(long_name);
    fs::create_dir(&source).unwrap();
    let bytes = serde_json::to_vec(&json!({"id":long_name})).unwrap();
    fs::write(source.join(format!("{long_name}.json")), &bytes).unwrap();
    let input: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let needed = unsafe { GetShortPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
    assert!(
        needed > 0 && needed <= 32768,
        "GetShortPathNameW size: {}",
        std::io::Error::last_os_error()
    );
    let mut short = vec![0u16; needed as usize];
    let written = unsafe { GetShortPathNameW(input.as_ptr(), short.as_mut_ptr(), needed) };
    assert!(
        written > 0 && written < needed,
        "GetShortPathNameW read: {}",
        std::io::Error::last_os_error()
    );
    let short = PathBuf::from(std::ffi::OsString::from_wide(&short[..written as usize]));
    let short_name = short.file_name().unwrap().to_str().unwrap();
    if short_name == long_name {
        eprintln!("8.3 observation: this filesystem supplies no distinct short leaf; alias rejection branch was not exercised");
        return;
    }
    let versions = Dir::open(&f.root).unwrap().child("versions").unwrap();
    assert!(
        !versions
            .names()
            .unwrap()
            .iter()
            .any(|name| name == short_name),
        "short spelling must be an alias rather than an enumerated real leaf"
    );
    assert_eq!(
        versions.child(short_name).unwrap().key().unwrap(),
        versions.child(long_name).unwrap().key().unwrap()
    );
    let original = scan(&versions.child(long_name).unwrap(), None).unwrap();
    drop(versions);
    let settings = f.settings_bytes();
    let error = prepare(&f.root, &f.project, "root-fixture", short_name).unwrap_err();
    assert!(error.contains("实际目录名称完全一致"), "{error}");
    assert!(!f.root.join(".pcl-rust").exists());
    assert!(!f.project.join(".pcl-rust").join(MARKER).exists());
    assert_eq!(scan(&Dir::open(&source).unwrap(), None).unwrap(), original);
    assert_eq!(
        fs::read(source.join(format!("{long_name}.json"))).unwrap(),
        bytes
    );
    assert_eq!(f.settings_bytes(), settings);
    assert_eq!(
        prepare(&f.root, &f.project, "root-fixture", long_name)
            .unwrap()
            .id,
        long_name
    );
}
