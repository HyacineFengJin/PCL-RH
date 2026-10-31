use super::*;
use std::os::unix::fs::symlink;
struct Fixture(PathBuf, #[allow(dead_code)] tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap();
        fs::create_dir_all(path.join("versions/fixture")).unwrap();
        let fixture = Self(path, temporary);
        fixture.profile(
            json!({"id":"fixture", "jar":"fixture", "clientVersion":"1.20.1",
            "mainClass":"net.minecraft.client.main.Main", "libraries":[], "arguments":{"game":[]}}),
        );
        fs::write(fixture.instance().join("fixture.jar"), b"original jar").unwrap();
        for relative in [
            "mods/user.jar",
            "config/user.toml",
            "saves/world/level.dat",
            "options.txt",
            "PCL/Setup.ini",
        ] {
            let path = fixture.instance().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, relative.as_bytes()).unwrap();
        }
        fixture
    }
    fn root(&self) -> &Path {
        &self.0
    }
    fn instance(&self) -> PathBuf {
        self.0.join("versions/fixture")
    }
    fn profile(&self, data: Value) {
        fs::write(
            self.instance().join("fixture.json"),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
    }
    fn plan(&self) -> ResetPlan {
        prepare(self.root(), "fixture", vec![]).unwrap()
    }
    fn transaction(&self) -> (Dir, Dir, Journal) {
        let plan = self.plan();
        let root = Dir::open(self.root()).unwrap();
        let store = storage(&root, true).unwrap().unwrap();
        let (operation, mut journal) = setup(&root, &store, &plan).unwrap();
        let incoming = operation.child("incoming").unwrap();
        for (i, file) in journal.files.iter_mut().enumerate() {
            let mut output = incoming.create_file(&file.name).unwrap();
            output
                .write_all(if i == 0 {
                    b"replacement json"
                } else {
                    b"replacement jar"
                })
                .unwrap();
            output.sync_all().unwrap();
            file.replacement =
                Some(snapshot(incoming.regular(&file.name).unwrap(), limit(i), None).unwrap());
        }
        write_journal(&operation, &journal).unwrap();
        (root, operation, journal)
    }
    fn assert_content(&self) {
        for relative in [
            "mods/user.jar",
            "config/user.toml",
            "saves/world/level.dat",
            "options.txt",
            "PCL/Setup.ini",
        ] {
            assert_eq!(
                fs::read(self.instance().join(relative)).unwrap(),
                relative.as_bytes()
            );
        }
    }
    fn assert_original(&self, originals: &[CoreFile]) {
        let instance = Dir::open(&self.instance()).unwrap();
        for (i, file) in originals.iter().enumerate() {
            assert!(matches_file(&instance, &file.name, &file.original, i).unwrap());
        }
        self.assert_content();
    }
    fn crash_after(&self, root: &Dir, operation: &Dir, journal: &mut Journal, moves: usize) {
        journal.state = State::Prepared;
        write_journal(operation, journal).unwrap();
        let (_, instance) = bound_dirs(root, journal).unwrap();
        let backup = operation.child("backup").unwrap();
        let incoming = operation.child("incoming").unwrap();
        for step in 0..moves {
            let file = &journal.files[step / 2];
            if step % 2 == 0 {
                rename_new(&instance, &file.name, &backup, &file.name).unwrap();
            } else {
                rename_new(&incoming, &file.name, &instance, &file.name).unwrap();
            }
        }
    }
}

#[test]
fn reset_lock_releases_even_when_an_inherited_descriptor_remains_open() {
    let fixture = Fixture::new();
    let root = Dir::open(fixture.root()).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let owned = lock(&store).unwrap();
    let inherited = owned.file.try_clone().unwrap();
    assert!(lock(&store).is_err());
    drop(owned);
    let next = lock(&store).unwrap();
    assert!(inherited.metadata().is_ok());
    assert!(lock(&store).is_err());
    drop(next);
    assert!(lock(&store).is_ok());
}

#[test]
fn successful_core_exchange_keeps_physical_instance_and_all_content_with_backup() {
    let fixture = Fixture::new();
    let original_key = Dir::open(&fixture.instance()).unwrap().key().unwrap();
    let (root, operation, mut journal) = fixture.transaction();
    let old = journal.files.clone();
    commit(
        &root,
        &operation,
        &mut journal,
        &AtomicBool::new(false),
        &|_| Ok(()),
    )
    .unwrap();
    assert_eq!(journal.state, State::Finished);
    assert_eq!(
        Dir::open(&fixture.instance()).unwrap().key().unwrap(),
        original_key
    );
    assert_eq!(
        fs::read(fixture.instance().join("fixture.json")).unwrap(),
        b"replacement json"
    );
    assert_eq!(
        fs::read(fixture.instance().join("fixture.jar")).unwrap(),
        b"replacement jar"
    );
    for (i, file) in old.iter().enumerate() {
        assert!(matches_file(
            &operation.child("backup").unwrap(),
            &file.name,
            &file.original,
            i
        )
        .unwrap());
    }
    assert!(operation.optional("stage").unwrap().is_none());
    fixture.assert_content();
    ensure_ready(fixture.root()).unwrap();
}

#[test]
fn faults_and_cancellation_at_every_rename_restore_original_core() {
    for stop in 1..=4 {
        for cancel_run in [false, true] {
            let fixture = Fixture::new();
            let (root, operation, mut journal) = fixture.transaction();
            let old = journal.files.clone();
            let cancel = AtomicBool::new(false);
            let error = commit(&root, &operation, &mut journal, &cancel, &|moved| {
                if moved == stop {
                    if cancel_run {
                        cancel.store(true, Ordering::Relaxed);
                    } else {
                        return Err("fixture injected fault".into());
                    }
                }
                Ok(())
            })
            .unwrap_err();
            assert!(!error.starts_with("取消清理失败："), "{error}");
            assert_eq!(journal.state, State::RolledBack);
            fixture.assert_original(&old);
            ensure_ready(fixture.root()).unwrap();
        }
    }
}

#[test]
fn every_interrupted_prepared_position_recovers_originals_and_is_idempotent() {
    for moves in 0..=4 {
        let fixture = Fixture::new();
        let (root, operation, mut journal) = fixture.transaction();
        let old = journal.files.clone();
        fixture.crash_after(&root, &operation, &mut journal, moves);
        assert!(ensure_ready(fixture.root())
            .unwrap_err()
            .starts_with("存在未完成的实例重置"));
        assert_eq!(recover_pending(fixture.root()).unwrap()["recovered"], 1);
        fixture.assert_original(&old);
        ensure_ready(fixture.root()).unwrap();
        assert_eq!(recover_pending(fixture.root()).unwrap()["recovered"], 0);
    }
}

#[test]
fn committed_restart_retains_complete_reset_and_finishes_cleanup() {
    let fixture = Fixture::new();
    let (root, operation, mut journal) = fixture.transaction();
    fixture.crash_after(&root, &operation, &mut journal, 4);
    journal.state = State::Committed;
    write_journal(&operation, &journal).unwrap();
    assert!(ensure_ready(fixture.root()).is_err());
    assert_eq!(recover_pending(fixture.root()).unwrap()["recovered"], 1);
    assert_eq!(
        fs::read(fixture.instance().join("fixture.jar")).unwrap(),
        b"replacement jar"
    );
    assert_eq!(
        read_journal(&operation, &journal.operation_id)
            .unwrap()
            .state,
        State::Finished
    );
    fixture.assert_content();
}

#[test]
fn cleanup_failure_is_visible_and_guarded_without_undoing_committed_reset() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let (root, operation, mut journal) = fixture.transaction();
    let stage = operation.child("stage").unwrap();
    symlink(outside.root(), stage.fd_path().join("external")).unwrap();
    let error = commit(
        &root,
        &operation,
        &mut journal,
        &AtomicBool::new(false),
        &|_| Ok(()),
    )
    .unwrap_err();
    assert!(error.starts_with("取消清理失败：重置已完成"), "{error}");
    assert_eq!(journal.state, State::Committed);
    assert_eq!(
        fs::read(fixture.instance().join("fixture.jar")).unwrap(),
        b"replacement jar"
    );
    assert!(recover_pending(fixture.root())
        .unwrap_err()
        .starts_with("取消清理失败："));
    assert_eq!(
        fs::read(outside.instance().join("fixture.jar")).unwrap(),
        b"original jar"
    );
    outside.assert_content();
    fs::remove_file(stage.fd_path().join("external")).unwrap();
    recover_pending(fixture.root()).unwrap();
    ensure_ready(fixture.root()).unwrap();
    fixture.assert_content();
}

#[test]
fn external_edit_during_failure_is_retained_with_original_backup_and_guard() {
    let fixture = Fixture::new();
    let (root, operation, mut journal) = fixture.transaction();
    let error = commit(
        &root,
        &operation,
        &mut journal,
        &AtomicBool::new(false),
        &|moved| {
            if moved == 2 {
                fs::write(
                    fixture.instance().join("fixture.json"),
                    b"external user edit",
                )
                .unwrap();
                return Err("fixture fault".into());
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.starts_with("取消清理失败："));
    assert_eq!(
        fs::read(fixture.instance().join("fixture.json")).unwrap(),
        b"external user edit"
    );
    assert!(matches_file(
        &operation.child("backup").unwrap(),
        "fixture.json",
        &journal.files[0].original,
        0
    )
    .unwrap());
    assert!(ensure_ready(fixture.root()).is_err());
    assert!(recover_pending(fixture.root()).is_err());
    fixture.assert_content();
}

#[test]
fn detached_instance_is_never_mutated_through_replaced_path_and_can_recover_later() {
    let fixture = Fixture::new();
    let (root, operation, mut journal) = fixture.transaction();
    let old = journal.files.clone();
    let moved_path = fixture.root().join("versions/detached");
    let error = commit(
        &root,
        &operation,
        &mut journal,
        &AtomicBool::new(false),
        &|moved| {
            if moved == 1 {
                fs::rename(fixture.instance(), &moved_path).unwrap();
                fs::create_dir(fixture.instance()).unwrap();
                fs::write(
                    fixture.instance().join("user.txt"),
                    b"replacement directory",
                )
                .unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.starts_with("取消清理失败："));
    assert_eq!(
        fs::read(fixture.instance().join("user.txt")).unwrap(),
        b"replacement directory"
    );
    fs::remove_file(fixture.instance().join("user.txt")).unwrap();
    fs::remove_dir(fixture.instance()).unwrap();
    fs::rename(&moved_path, fixture.instance()).unwrap();
    recover_pending(fixture.root()).unwrap();
    fixture.assert_original(&old);
}

#[test]
fn plan_binds_requested_components_content_inode_root_and_dependencies() {
    let fixture = Fixture::new();
    let original = fixture.plan();
    let fabric = prepare(
        fixture.root(),
        "fixture",
        vec![ComponentSelection {
            provider: "Fabric".into(),
            version: "0.16.0".into(),
        }],
    )
    .unwrap();
    assert_ne!(original.revision, fabric.revision);
    assert_eq!(fabric.components[0].provider, "fabric");
    let equal = prepare(
        fixture.root(),
        "fixture",
        vec![ComponentSelection {
            provider: "fabric".into(),
            version: "0.16.0".into(),
        }],
    )
    .unwrap();
    assert_eq!(equal.revision, fabric.revision);
    fs::write(fixture.instance().join("fixture.jar"), b"modified jar").unwrap();
    assert!(execute(
        fixture.root(),
        fixture.root(),
        original,
        &AtomicBool::new(false),
        |_| {}
    )
    .unwrap_err()
    .contains("已变化"));
    let original = fixture.plan();
    fs::create_dir_all(fixture.root().join("versions/dependent")).unwrap();
    fs::write(
        fixture.root().join("versions/dependent/dependent.json"),
        br#"{"id":"dependent","inheritsFrom":"fixture"}"#,
    )
    .unwrap();
    assert!(execute(
        fixture.root(),
        fixture.root(),
        original,
        &AtomicBool::new(false),
        |_| {}
    )
    .unwrap_err()
    .contains("继承或复用"));
    let other = Fixture::new();
    assert!(execute(
        other.root(),
        other.root(),
        fabric,
        &AtomicBool::new(false),
        |_| {}
    )
    .is_err());
    fixture.assert_content();
}

#[test]
fn ambiguous_external_inherited_unsupported_and_linked_core_layouts_are_rejected() {
    for change in [
        json!({"jar":"external"}),
        json!({"inheritsFrom":"base"}),
        json!({"id":"wrong"}),
        json!({"mainClass":"custom.Main"}),
        json!({"patches":[]}),
        json!({"libraries":[{"name":"org.quiltmc:quilt-loader:1"}]}),
    ] {
        let fixture = Fixture::new();
        let mut profile: Value =
            serde_json::from_slice(&fs::read(fixture.instance().join("fixture.json")).unwrap())
                .unwrap();
        for (key, value) in change.as_object().unwrap() {
            profile[key] = value.clone();
        }
        fixture.profile(profile);
        assert!(prepare(fixture.root(), "fixture", vec![]).is_err());
        fixture.assert_content();
    }
    let fixture = Fixture::new();
    let outside = Fixture::new();
    fs::remove_file(fixture.instance().join("fixture.jar")).unwrap();
    symlink(
        outside.instance().join("fixture.jar"),
        fixture.instance().join("fixture.jar"),
    )
    .unwrap();
    assert!(prepare(fixture.root(), "fixture", vec![]).is_err());
    fs::remove_file(fixture.instance().join("fixture.jar")).unwrap();
    fs::hard_link(
        outside.instance().join("fixture.jar"),
        fixture.instance().join("fixture.jar"),
    )
    .unwrap();
    assert!(prepare(fixture.root(), "fixture", vec![]).is_err());
    assert!(prepare(fixture.root(), "../fixture", vec![]).is_err());
}

#[test]
fn verified_support_files_publish_without_overwriting_existing_cache() {
    let fixture = Fixture::new();
    let (root, operation, _) = fixture.transaction();
    let stage = operation.child("stage").unwrap();
    let source = stage.create_dir("libraries").unwrap();
    let target = root.create_dir("libraries").unwrap();
    let mut file = source.create_file("new.jar").unwrap();
    file.write_all(b"new support").unwrap();
    file.sync_all().unwrap();
    let mut file = source.create_file("same.jar").unwrap();
    file.write_all(b"same support").unwrap();
    file.sync_all().unwrap();
    let mut file = target.create_file("same.jar").unwrap();
    file.write_all(b"same support").unwrap();
    file.sync_all().unwrap();
    let old = snapshot(target.cache_file("same.jar").unwrap(), MAX_JAR, None).unwrap();
    merge_cache(&source, &target, &AtomicBool::new(false), &mut 0).unwrap();
    assert_eq!(
        snapshot(target.cache_file("same.jar").unwrap(), MAX_JAR, None).unwrap(),
        old
    );
    assert_eq!(
        fs::read(fixture.root().join("libraries/new.jar")).unwrap(),
        b"new support"
    );
    let mut file = source.create_file("conflict.jar").unwrap();
    file.write_all(b"new").unwrap();
    let mut file = target.create_file("conflict.jar").unwrap();
    file.write_all(b"user").unwrap();
    assert!(
        merge_cache(&source, &target, &AtomicBool::new(false), &mut 0)
            .unwrap_err()
            .contains("已保留现有文件")
    );
    assert_eq!(
        fs::read(fixture.root().join("libraries/conflict.jar")).unwrap(),
        b"user"
    );
    fixture.assert_content();
}

#[test]
fn source_or_destination_cache_symlinks_never_escape() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let (root, operation, _) = fixture.transaction();
    let source = operation
        .child("stage")
        .unwrap()
        .create_dir("libraries")
        .unwrap();
    let target = root.create_dir("libraries").unwrap();
    symlink(
        outside.instance().join("fixture.jar"),
        source.fd_path().join("escape.jar"),
    )
    .unwrap();
    assert!(merge_cache(&source, &target, &AtomicBool::new(false), &mut 0).is_err());
    assert!(!fixture.root().join("libraries/escape.jar").exists());
    fs::remove_file(source.fd_path().join("escape.jar")).unwrap();
    let child = source.create_dir("nested").unwrap();
    let mut file = child.create_file("x.jar").unwrap();
    file.write_all(b"cache").unwrap();
    symlink(outside.root(), target.fd_path().join("nested")).unwrap();
    assert!(merge_cache(&source, &target, &AtomicBool::new(false), &mut 0).is_err());
    assert!(!outside.root().join("x.jar").exists());
}

#[test]
fn staging_restart_cleans_only_owned_temporary_tree() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    let root = Dir::open(fixture.root()).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let (operation, journal) = setup(&root, &store, &plan).unwrap();
    let mut file = operation
        .child("stage")
        .unwrap()
        .create_file("partial")
        .unwrap();
    file.write_all(b"partial").unwrap();
    assert!(ensure_ready(fixture.root()).is_err());
    recover_pending(fixture.root()).unwrap();
    assert_eq!(
        read_journal(&operation, &journal.operation_id)
            .unwrap()
            .state,
        State::RolledBack
    );
    assert!(!operation.fd_path().join("stage").exists());
    fixture.assert_content();
    assert_eq!(
        fs::read(fixture.instance().join("fixture.jar")).unwrap(),
        b"original jar"
    );
}

#[test]
fn anonymous_core_copy_is_registered_before_named_publication_and_recovers() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    let root = Dir::open(fixture.root()).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let (operation, mut journal) = setup(&root, &store, &plan).unwrap();
    let incoming = operation.child("incoming").unwrap();
    let stage = operation.child("stage").unwrap();
    let mut source = stage.create_file("source.json").unwrap();
    source.write_all(b"complete replacement").unwrap();
    source.sync_all().unwrap();
    copy_core(
        stage.regular("source.json").unwrap(),
        &incoming,
        &operation,
        &mut journal,
        0,
        &AtomicBool::new(false),
    )
    .unwrap();
    let durable = read_journal(&operation, &journal.operation_id).unwrap();
    assert!(matches_file(
        &incoming,
        "fixture.json",
        durable.files[0].replacement.as_ref().unwrap(),
        0
    )
    .unwrap());
    assert_eq!(incoming.names().unwrap(), vec!["fixture.json"]);
    // A crash before link publication has exactly the same durable journal
    // with its registered inode absent from incoming; both states recover.
    incoming.unlink("fixture.json", false).unwrap();
    recover_pending(fixture.root()).unwrap();
    assert_eq!(
        fs::read(fixture.instance().join("fixture.jar")).unwrap(),
        b"original jar"
    );
    fixture.assert_content();
}

#[test]
fn registered_named_core_copy_recovers_and_external_edits_are_preserved() {
    for external in [false, true] {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let root = Dir::open(fixture.root()).unwrap();
        let store = storage(&root, true).unwrap().unwrap();
        let (operation, mut journal) = setup(&root, &store, &plan).unwrap();
        let incoming = operation.child("incoming").unwrap();
        let stage = operation.child("stage").unwrap();
        let mut source = stage.create_file("source.json").unwrap();
        source.write_all(b"complete replacement").unwrap();
        source.sync_all().unwrap();
        copy_core(
            stage.regular("source.json").unwrap(),
            &incoming,
            &operation,
            &mut journal,
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
        if external {
            fs::write(
                incoming.fd_path().join("fixture.json"),
                b"external changed incoming",
            )
            .unwrap();
            assert!(recover_pending(fixture.root())
                .unwrap_err()
                .starts_with("取消清理失败："));
            assert_eq!(
                fs::read(incoming.fd_path().join("fixture.json")).unwrap(),
                b"external changed incoming"
            );
            assert!(ensure_ready(fixture.root()).is_err());
        } else {
            recover_pending(fixture.root()).unwrap();
            ensure_ready(fixture.root()).unwrap();
        }
        assert_eq!(
            fs::read(fixture.instance().join("fixture.jar")).unwrap(),
            b"original jar"
        );
        fixture.assert_content();
    }
}

#[test]
fn malformed_or_relocated_journals_preserve_core_and_backup() {
    let fixture = Fixture::new();
    let (root, operation, mut journal) = fixture.transaction();
    let old = journal.files.clone();
    fixture.crash_after(&root, &operation, &mut journal, 1);
    let raw = fs::read(operation.fd_path().join("journal.json")).unwrap();
    let mut damaged: Value = serde_json::from_slice(&raw).unwrap();
    damaged["files"][0]["name"] = Value::String("../outside.json".into());
    fs::write(
        operation.fd_path().join("journal.json"),
        serde_json::to_vec(&damaged).unwrap(),
    )
    .unwrap();
    assert!(ensure_ready(fixture.root()).is_err());
    assert!(recover_pending(fixture.root()).is_err());
    assert!(matches_file(
        &operation.child("backup").unwrap(),
        "fixture.json",
        &old[0].original,
        0
    )
    .unwrap());
    fs::write(operation.fd_path().join("journal.json"), raw).unwrap();
    recover_pending(fixture.root()).unwrap();
    fixture.assert_original(&old);
}
