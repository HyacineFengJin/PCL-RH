//! Actual reset journals, generated copies, move barriers and restart recovery.
use super::*;
use pcl_core::retained_fs::{Directory, Leaf, Traversal};

struct Fixture {
    _temporary: tempfile::TempDir,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap();
        let parent = Directory::open_absolute(&path).unwrap();
        parent
            .create_private(&Leaf::new("game").unwrap(), Traversal::AllowMounts)
            .unwrap();
        let fixture = Self {
            _temporary: temporary,
            path: path.join("game"),
        };
        let root = Dir::open(&fixture.path).unwrap();
        let versions = root.create_dir("versions").unwrap();
        let instance = versions.create_dir("fixture").unwrap();
        fixture.put(&instance,"fixture.json",&serde_json::to_vec(&json!({"id":"fixture","jar":"fixture","clientVersion":"1.20.1","mainClass":"net.minecraft.client.main.Main","libraries":[],"arguments":{"game":[]}})).unwrap());
        fixture.put(&instance, "fixture.jar", b"original jar");
        for name in ["mods", "config", "saves"] {
            let child = instance.create_dir(name).unwrap();
            fixture.put(&child, "user-data", name.as_bytes());
        }
        fixture
    }
    fn put(&self, dir: &Dir, name: &str, bytes: &[u8]) {
        let mut file = dir.create_file(name).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        drop(file);
        dir.sync().unwrap();
    }
    fn instance(&self) -> PathBuf {
        self.path.join("versions").join("fixture")
    }
    fn stage(&self) -> (Dir, Dir, Journal) {
        let plan = prepare(&self.path, "fixture", vec![]).unwrap();
        let root = Dir::open(&self.path).unwrap();
        let store = storage(&root, true).unwrap().unwrap();
        let (operation, mut journal) = setup(&root, &store, &plan).unwrap();
        let stage = operation.child("stage").unwrap();
        let incoming = operation.child("incoming").unwrap();
        for index in 0..2 {
            let name = format!("source-{index}");
            self.put(&stage,&name,if index==0 {br#"{"id":"fixture","clientVersion":"1.20.1","mainClass":"net.minecraft.client.main.Main","libraries":[],"arguments":{"game":[]},"replacement":true}"#}else{b"replacement jar"});
            copy_core(
                stage.regular(&name).unwrap(),
                &incoming,
                &operation,
                &mut journal,
                index,
                &AtomicBool::new(false),
            )
            .unwrap();
        }
        (root, operation, journal)
    }
    fn unchanged_content(&self) {
        for name in ["mods", "config", "saves"] {
            assert_eq!(
                fs::read(self.instance().join(name).join("user-data")).unwrap(),
                name.as_bytes()
            );
        }
    }
    fn originals(&self, files: &[CoreFile]) {
        let instance = Dir::open(&self.instance()).unwrap();
        for (index, file) in files.iter().enumerate() {
            assert!(matches_file(&instance, &file.name, &file.original, index).unwrap());
        }
        self.unchanged_content();
    }
    fn crash(&self, root: &Dir, operation: &Dir, journal: &mut Journal, moves: usize) {
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
fn actual_registered_sources_commit_only_two_core_files_and_keep_original_backup() {
    let fixture = Fixture::new();
    let before = Dir::open(&fixture.instance()).unwrap().key().unwrap();
    let (root, operation, mut journal) = fixture.stage();
    let originals = journal.files.clone();
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
        before
    );
    assert_eq!(
        fs::read(fixture.instance().join("fixture.jar")).unwrap(),
        b"replacement jar"
    );
    let metadata: Value =
        serde_json::from_slice(&fs::read(fixture.instance().join("fixture.json")).unwrap())
            .unwrap();
    assert_eq!(metadata["replacement"], true);
    let backup = operation.child("backup").unwrap();
    for (index, file) in originals.iter().enumerate() {
        assert!(matches_file(&backup, &file.name, &file.original, index).unwrap());
    }
    assert!(operation.optional("stage").unwrap().is_none());
    fixture.unchanged_content();
    ensure_ready(&fixture.path).unwrap();
    assert_eq!(
        read_journal(&operation, &journal.operation_id)
            .unwrap()
            .state,
        State::Finished
    );
    #[cfg(not(target_os = "linux"))]
    {
        let raw = serde_json::to_value(&journal).unwrap();
        assert_eq!(raw["schema"], 2);
        assert_eq!(raw["root_key"]["format"], "retained-identity-v1");
    }
}
#[test]
fn every_actual_prepared_move_position_recovers_originals_after_reopen() {
    for moves in 0..=4 {
        let fixture = Fixture::new();
        let (root, operation, mut journal) = fixture.stage();
        let originals = journal.files.clone();
        fixture.crash(&root, &operation, &mut journal, moves);
        assert!(ensure_ready(&fixture.path).is_err());
        drop(operation);
        drop(root);
        recover_pending(&fixture.path).unwrap();
        fixture.originals(&originals);
        recover_pending(&fixture.path).unwrap();
        fixture.originals(&originals);
        ensure_ready(&fixture.path).unwrap();
    }
}
#[test]
fn failures_and_cancellation_at_each_move_roll_back_without_touching_user_content() {
    for at in 1..=4 {
        for cancel_now in [false, true] {
            let fixture = Fixture::new();
            let (root, operation, mut journal) = fixture.stage();
            let originals = journal.files.clone();
            let cancel = AtomicBool::new(false);
            let result = commit(&root, &operation, &mut journal, &cancel, &|step| {
                if step == at {
                    if cancel_now {
                        cancel.store(true, Ordering::Relaxed);
                    } else {
                        return Err("injected physical move failure".into());
                    }
                }
                Ok(())
            });
            assert!(result.is_err());
            fixture.originals(&originals);
            assert_eq!(
                read_journal(&operation, &journal.operation_id)
                    .unwrap()
                    .state,
                State::RolledBack
            );
        }
    }
}
#[test]
fn uncertain_committed_publication_recovers_from_actual_durable_phase() {
    for fault in [1, 2] {
        let fixture = Fixture::new();
        let (root, operation, mut journal) = fixture.stage();
        let originals = journal.files.clone();
        COMMIT_JOURNAL_FAULT.with(|value| value.set(fault));
        assert!(commit(
            &root,
            &operation,
            &mut journal,
            &AtomicBool::new(false),
            &|_| Ok(())
        )
        .is_err());
        COMMIT_JOURNAL_FAULT.with(|value| value.set(0));
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(
                fs::read(fixture.instance().join("fixture.jar")).unwrap(),
                b"replacement jar"
            );
            assert_eq!(
                read_journal(&operation, &journal.operation_id)
                    .unwrap()
                    .state,
                if fault == 1 {
                    State::Prepared
                } else {
                    State::Committed
                }
            );
        }
        recover_pending(&fixture.path).unwrap();
        #[cfg(not(target_os = "linux"))]
        if fault == 2 {
            assert_eq!(
                fs::read(fixture.instance().join("fixture.jar")).unwrap(),
                b"replacement jar"
            );
            fixture.unchanged_content();
            continue;
        }
        fixture.originals(&originals);
    }
}
#[cfg(not(target_os = "linux"))]
#[test]
fn uncertain_first_journal_retains_its_base_and_recovers_staging_without_original_changes() {
    let fixture = Fixture::new();
    let plan = prepare(&fixture.path, "fixture", vec![]).unwrap();
    let root = Dir::open(&fixture.path).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    INITIAL_JOURNAL_FAULT.with(|fault| fault.set(2));
    let error = setup(&root, &store, &plan).err().unwrap();
    assert!(error.starts_with("MayHaveCommitted"));
    let names = store.names().unwrap();
    assert_eq!(names.len(), 1);
    let operation = store.child(&names[0]).unwrap();
    let journal = read_journal(&operation, &names[0]).unwrap();
    assert_eq!(journal.state, State::Staging);
    let original = filesystem::journal_bytes(&operation).unwrap().unwrap();
    recover_pending(&fixture.path).unwrap();
    fixture.originals(&journal.files);
    assert_eq!(
        read_journal(&operation, &names[0]).unwrap().state,
        State::RolledBack
    );
    #[cfg(windows)]
    {
        let raw = operation.regular("journal.json").unwrap();
        use std::io::Read;
        let mut bytes = Vec::new();
        raw.take(MAX_JOURNAL).read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, original);
    }
    #[cfg(unix)]
    let _ = original;
}
#[test]
fn staged_copy_cancellation_preserves_sources_and_leaves_no_unregistered_incoming_file() {
    let fixture = Fixture::new();
    let plan = prepare(&fixture.path, "fixture", vec![]).unwrap();
    let root = Dir::open(&fixture.path).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let (operation, mut journal) = setup(&root, &store, &plan).unwrap();
    let stage = operation.child("stage").unwrap();
    fixture.put(&stage, "source", b"complete source");
    let incoming = operation.child("incoming").unwrap();
    assert!(copy_core(
        stage.regular("source").unwrap(),
        &incoming,
        &operation,
        &mut journal,
        0,
        &AtomicBool::new(true)
    )
    .is_err());
    assert!(incoming.names().unwrap().is_empty());
    assert!(journal.files[0].replacement.is_none());
    recover_pending(&fixture.path).unwrap();
    fixture.originals(&journal.files);
}
#[test]
fn cache_publication_reuses_equal_bytes_and_preserves_conflicting_existing_files() {
    let fixture = Fixture::new();
    let (root, operation, _journal) = fixture.stage();
    let source = operation
        .child("stage")
        .unwrap()
        .create_dir("libraries")
        .unwrap();
    let target = root.create_dir("libraries").unwrap();
    fixture.put(&source, "new.jar", b"new library");
    fixture.put(&source, "reused.jar", b"same library");
    fixture.put(&target, "reused.jar", b"same library");
    let before = snapshot(target.regular("reused.jar").unwrap(), MAX_JAR, None).unwrap();
    filesystem::merge_cache_bound(&source, &target, &root, &AtomicBool::new(false), &mut 0)
        .unwrap();
    assert_eq!(
        snapshot(target.regular("reused.jar").unwrap(), MAX_JAR, None).unwrap(),
        before
    );
    let mut copy = target.cache_file("new.jar").unwrap();
    let mut bytes = Vec::new();
    copy.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"new library");
    drop(copy);
    fixture.put(&source, "conflict.jar", b"new content");
    fixture.put(&target, "conflict.jar", b"keep original");
    let before = snapshot(target.regular("conflict.jar").unwrap(), MAX_JAR, None).unwrap();
    assert!(filesystem::merge_cache_bound(
        &source,
        &target,
        &root,
        &AtomicBool::new(false),
        &mut 0
    )
    .is_err());
    assert_eq!(
        snapshot(target.regular("conflict.jar").unwrap(), MAX_JAR, None).unwrap(),
        before
    );
    fixture.unchanged_content();
}
#[test]
fn unknown_incoming_and_changed_replacement_are_preserved_for_recovery() {
    for unknown in [false, true] {
        let fixture = Fixture::new();
        let (_root, operation, journal) = fixture.stage();
        let incoming = operation.child("incoming").unwrap();
        if unknown {
            fixture.put(&incoming, "unrecognized", b"keep");
        } else {
            // Ordinary editing is deliberate here; this invalidates the known
            // generated ID/hash without authorizing cleanup of the new bytes.
            let path = fixture
                .path
                .join(".pcl-linux")
                .join(STORE)
                .join(&journal.operation_id)
                .join("incoming")
                .join("fixture.json");
            fs::write(path, b"changed generated bytes").unwrap();
        }
        assert!(recover_pending(&fixture.path).is_err());
        assert!(ensure_ready(&fixture.path).is_err());
        fixture.originals(&journal.files);
        assert!(incoming
            .stat(if unknown {
                "unrecognized"
            } else {
                "fixture.json"
            })
            .unwrap()
            .is_some());
    }
}
#[cfg(target_os = "linux")]
#[test]
fn old_schema_one_and_snapshot_wire_round_trip_without_field_or_byte_changes() {
    let key = r#"{"dev":1,"ino":2}"#;
    let snapshot = format!(
        r#"{{"size":1,"hash":"{}","dev":1,"ino":3,"mtime":4,"mtime_nsec":5,"ctime":6,"ctime_nsec":7}}"#,
        "a".repeat(64)
    );
    let bytes = format!(
        r#"{{"schema":1,"operation_id":"r-1-2-3","root":"/old/root","root_key":{key},"versions_key":{key},"instance_key":{key},"operation_key":{key},"instance_id":"fixture","stage_key":null,"incoming_key":null,"backup_key":null,"state":"staging","files":[{{"name":"fixture.json","original":{snapshot},"replacement":null}},{{"name":"fixture.jar","original":{snapshot},"replacement":null}}]}}"#
    );
    let journal: Journal = serde_json::from_str(&bytes).unwrap();
    validate_journal(&journal, "r-1-2-3").unwrap();
    assert_eq!(serde_json::to_string(&journal).unwrap(), bytes);
    let fixture = Fixture::new();
    assert!(prepare(&fixture.path, "fixture", vec![])
        .unwrap()
        .revision
        .starts_with("reset-v1-"));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn native_journal_replacement_preserves_damaged_future_and_foreign_prior_authority() {
    use crate::launcher_local::filesystem::{documents::Document, Dir as LocalDir};
    for case in ["damaged", "future", "foreign-binding"] {
        let fixture = Fixture::new();
        let (_root, operation, journal) = fixture.stage();
        let foreign = Fixture::new();
        let prior = match case {
            "damaged" => b"{external evidence, not a reset journal".to_vec(),
            "future" => {
                let mut value = serde_json::to_value(&journal).unwrap();
                value["schema"] = 99.into();
                serde_json::to_vec(&value).unwrap()
            }
            _ => {
                let mut value = journal.clone();
                value.root_key = Dir::open(&foreign.path).unwrap().key().unwrap();
                serde_json::to_vec(&value).unwrap()
            }
        };
        let local = LocalDir(operation.bound_file().unwrap());
        let document = Document::new(&local, "journal.json", MAX_JOURNAL).unwrap();
        #[cfg(windows)]
        {
            let state = document.state().unwrap();
            document.compare_write(&state, Some(&prior)).unwrap();
        }
        #[cfg(unix)]
        {
            let state = document.snapshot().unwrap().unwrap();
            document.replace(Some(&state), &prior).unwrap();
        }
        #[cfg(windows)]
        let before = document.state().unwrap().fact_digest();
        #[cfg(unix)]
        let before = pcl_core::retained_fs::metadata(&operation.regular("journal.json").unwrap())
            .unwrap()
            .identity;
        let error = write_journal(&operation, &journal).unwrap_err();
        assert!(!error.is_empty(), "{case}");
        #[cfg(windows)]
        {
            let mut actual = document.state().unwrap();
            assert_eq!(
                actual.fact_digest(),
                before,
                "{case}: old logical facts were changed/GCed"
            );
            assert_eq!(
                document.read_state(&mut actual).unwrap().unwrap(),
                prior,
                "{case}"
            );
        }
        #[cfg(unix)]
        {
            let mut actual = document.snapshot().unwrap().unwrap();
            assert_eq!(document.bytes(&mut actual).unwrap(), prior, "{case}");
            assert_eq!(
                pcl_core::retained_fs::metadata(&operation.regular("journal.json").unwrap())
                    .unwrap()
                    .identity,
                before,
                "{case}: old physical journal was swapped out"
            );
            assert_eq!(
                fs::read(
                    fixture
                        .path
                        .join(".pcl-linux/instance-resets")
                        .join(&journal.operation_id)
                        .join("journal.json")
                )
                .unwrap(),
                prior,
                "{case}"
            );
        }
        fixture.originals(&journal.files);
    }
}
