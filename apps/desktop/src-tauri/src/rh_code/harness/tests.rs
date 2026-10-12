use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

struct Fixture {
    directory: std::path::PathBuf,
    store: std::path::PathBuf,
    project: Project,
}
impl Fixture {
    fn new() -> Self {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/harness-2026-10-12/native/fixtures")
            .join(id());
        let project_folder = directory.join("project");
        fs::create_dir_all(&project_folder).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        let project_folder = fs::canonicalize(project_folder).unwrap();
        Self {
            store: directory.join("store"),
            directory,
            project: Project {
                id: "a".repeat(24),
                name: "Fixture".into(),
                folder: project_folder.to_string_lossy().into(),
                updated_at: 0,
                targets: vec![],
                conversations: vec![],
                layout: json!({}),
            },
        }
    }
    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let path = Path::new(&self.project.folder).join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn read(&self, path: &str) -> Vec<u8> {
        fs::read(Path::new(&self.project.folder).join(path)).unwrap()
    }
    fn bound(&self) -> Value {
        let snapshot = capture(&self.project).unwrap();
        bind(
            &self.store,
            &self.project,
            "job-fixture",
            "conversation-fixture",
            &snapshot,
        )
        .unwrap();
        snapshot
    }
    fn proposal(&self, snapshot: &Value, files: Value) -> (Value, Value) {
        let args = json!({"jobId":"job-fixture","proposalId":"b".repeat(32),"expectedDigest":"c".repeat(64),"conversationId":"conversation-fixture","projectId":self.project.id});
        let export = json!({"jobId":args["jobId"],"proposalId":args["proposalId"],"digest":args["expectedDigest"],
            "conversationId":args["conversationId"],"projectId":self.project.id,"sourceFingerprint":snapshot["sourceFingerprint"],"files":files});
        (args, export)
    }
    /// Exercise a real durable journal and real kernel renames, then stop before
    /// commit to model a process disappearing at a publication boundary.
    fn interrupted(&self, export: &Value, published: usize) -> (Dir, Journal) {
        let store = store(&self.store).unwrap();
        let binding = load_binding(&store, "job-fixture").unwrap();
        let root = root(&self.project).unwrap();
        let changes = preflight(&root, &binding, export).unwrap();
        let name =
            hash(format!("job-fixture:{}", export["proposalId"].as_str().unwrap()).as_bytes());
        let transaction = store
            .ensure_child("harness-transactions")
            .unwrap()
            .ensure_child(&name)
            .unwrap();
        let mut journal = Journal {
            schema_version: 1,
            binding,
            proposal_id: export["proposalId"].as_str().unwrap().into(),
            digest: export["digest"].as_str().unwrap().into(),
            export_hash: hash(&serde_json::to_vec(export).unwrap()),
            phase: "preparing".into(),
            cleanup_complete: false,
            entries: changes.iter().map(|v| v.entry.clone()).collect(),
            created_directories: vec![],
        };
        write_journal(&transaction, &journal).unwrap();
        for (index, change) in changes.iter().enumerate() {
            if let Some(bytes) = &change.before_bytes {
                transaction
                    .immutable(&format!("before-{index}"), bytes)
                    .unwrap();
            }
            let parent = ensure_parent(
                &root,
                split_path(&change.entry.path).0,
                &transaction,
                &mut journal,
            )
            .unwrap();
            journal.entries[index].parent_identity = Some(parent.key().unwrap());
            if let Some(bytes) = &change.after_bytes {
                let mode = change.entry.before.as_ref().map_or(0o644, |v| v.mode);
                let file = parent.anonymous(bytes, mode).unwrap();
                journal.entries[index].staged = Some(FileState {
                    identity: identity(&file).unwrap(),
                    sha256: hash(bytes),
                    size: bytes.len(),
                    mode,
                });
                write_journal(&transaction, &journal).unwrap();
                parent
                    .link_anonymous(&file, &journal.entries[index].temporary)
                    .unwrap();
            }
        }
        journal.phase = "publishing".into();
        write_journal(&transaction, &journal).unwrap();
        for entry in journal.entries.iter().take(published) {
            publish(&self.project, &root, entry).unwrap();
        }
        (transaction, journal)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn replacement(path: &str, before: &str, after: &str) -> Value {
    json!({"path":path,"expectedSha256":hash(before.as_bytes()),"data":STANDARD.encode(after.as_bytes()),"delete":false})
}
fn creation(path: &str, after: &str) -> Value {
    json!({"path":path,"expectedSha256":null,"data":STANDARD.encode(after.as_bytes()),"delete":false})
}
fn deletion(path: &str, before: &str) -> Value {
    json!({"path":path,"expectedSha256":hash(before.as_bytes()),"data":null,"delete":true})
}

#[test]
fn captures_sorted_text_with_matching_host_canonical_fingerprint_and_omissions() {
    let f = Fixture::new();
    f.write("z.txt", "last");
    f.write("a/ü.rs", "first");
    f.write("nul.bin", b"a\0b");
    f.write("invalid.bin", [0xff]);
    f.write(".private/secret.txt", "secret");
    f.write("node_modules/library.js", "dependency");
    f.write("build/output.txt", "generated");
    symlink(
        Path::new(&f.project.folder).join("z.txt"),
        Path::new(&f.project.folder).join("link.txt"),
    )
    .unwrap();
    let captured = capture(&f.project).unwrap();
    assert_eq!(captured["sourceFiles"].as_array().unwrap().len(), 2);
    assert_eq!(captured["sourceFiles"][0]["path"], "a/ü.rs");
    assert_eq!(captured["sourceOmissions"].as_array().unwrap().len(), 3);
    let canonical = format!("[{{\"path\":\"a/ü.rs\",\"sha256\":\"{}\",\"size\":5}},{{\"path\":\"z.txt\",\"sha256\":\"{}\",\"size\":4}}]",hash(b"first"),hash(b"last"));
    assert_eq!(captured["sourceFingerprint"], hash(canonical.as_bytes()));
    assert!(!serde_json::to_string(&captured).unwrap().contains("secret"));
}
#[test]
fn capture_omits_oversized_files_and_rejects_total_text_budget() {
    let f = Fixture::new();
    f.write("oversized.txt", vec![b'x'; MAX_FILE + 1]);
    assert_eq!(
        capture(&f.project).unwrap()["sourceOmissions"][0]["reason"],
        "fileTooLarge"
    );
    for index in 0..17 {
        f.write(&format!("{index}.txt"), vec![b'x'; MAX_FILE]);
    }
    assert!(capture(&f.project).unwrap_err().contains("16 MiB"));
}
#[test]
fn capture_rejects_more_than_1000_text_files() {
    let f = Fixture::new();
    for index in 0..=MAX_FILES {
        f.write(&format!("{index}.txt"), "");
    }
    assert!(capture(&f.project).unwrap_err().contains("1000"));
}
#[test]
fn native_binding_is_immutable_for_the_same_job() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    bind(
        &f.store,
        &f.project,
        "job-fixture",
        "conversation-fixture",
        &captured,
    )
    .unwrap();
    f.write("a.txt", "new");
    let changed = capture(&f.project).unwrap();
    assert!(bind(
        &f.store,
        &f.project,
        "job-fixture",
        "conversation-fixture",
        &changed
    )
    .unwrap_err()
    .contains("Immutable"));
}
#[test]
fn applies_replace_create_delete_preserves_mode_and_replays_without_overwrite() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    f.write("removed.txt", "remove");
    fs::set_permissions(
        Path::new(&f.project.folder).join("a.txt"),
        fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    let captured = f.bound();
    let (args, export) = f.proposal(
        &captured,
        json!([
            replacement("a.txt", "old", "new"),
            creation("new/nested/file.txt", "created"),
            deletion("removed.txt", "remove")
        ]),
    );
    assert_eq!(
        apply(&f.store, &f.project, &args, &export).unwrap()["status"],
        "applied"
    );
    assert_eq!(f.read("a.txt"), b"new");
    assert_eq!(f.read("new/nested/file.txt"), b"created");
    assert!(!Path::new(&f.project.folder).join("removed.txt").exists());
    assert_eq!(
        fs::metadata(Path::new(&f.project.folder).join("a.txt"))
            .unwrap()
            .mode()
            & 0o777,
        0o751
    );
    let transaction_name =
        hash(format!("job-fixture:{}", args["proposalId"].as_str().unwrap()).as_bytes());
    assert_eq!(
        fs::read(
            f.store
                .join("harness-transactions")
                .join(transaction_name)
                .join("before-0")
        )
        .unwrap(),
        b"old"
    );
    f.write("a.txt", "handwritten after apply");
    assert_eq!(
        apply(&f.store, &f.project, &args, &export).unwrap()["status"],
        "alreadyApplied"
    );
    assert_eq!(f.read("a.txt"), b"handwritten after apply");
}
#[test]
fn stale_manifest_and_wrong_original_hash_reject_the_entire_batch() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    f.write("other.txt", "other");
    let captured = f.bound();
    let (args, mut export) = f.proposal(
        &captured,
        json!([
            replacement("a.txt", "old", "new"),
            creation("fresh/file.txt", "created")
        ]),
    );
    f.write("other.txt", "external change");
    assert!(apply(&f.store, &f.project, &args, &export)
        .unwrap_err()
        .contains("source changed"));
    assert_eq!(f.read("a.txt"), b"old");
    assert!(!Path::new(&f.project.folder).join("fresh").exists());
    f.write("other.txt", "other");
    export["files"][0]["expectedSha256"] = json!(hash(b"forged"));
    assert!(apply(&f.store, &f.project, &args, &export)
        .unwrap_err()
        .contains("frozen manifest"));
    assert_eq!(f.read("a.txt"), b"old");
}
#[test]
fn new_visible_source_after_binding_is_a_conflict() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (args, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    f.write("new-user-file.txt", "handwritten");
    assert!(apply(&f.store, &f.project, &args, &export).is_err());
    assert_eq!(f.read("a.txt"), b"old");
}
#[test]
fn host_export_identity_and_review_digest_are_both_required() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (args, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    for (field, value) in [
        ("jobId", "other-job"),
        ("proposalId", "d"),
        ("digest", "d"),
        ("projectId", "other-project"),
        ("conversationId", "other-conversation"),
        ("sourceFingerprint", "d"),
    ] {
        let mut forged = export.clone();
        forged[field] = json!(value);
        assert!(
            apply(&f.store, &f.project, &args, &forged).is_err(),
            "{field}"
        );
    }
    assert_eq!(f.read("a.txt"), b"old");
}
#[test]
fn traversal_hidden_dependencies_file_parent_conflicts_and_text_errors_are_rejected() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    for path in [
        "../outside.txt",
        "/absolute.txt",
        "a/../../outside.txt",
        ".hidden/file",
        "a/.hidden",
        "node_modules/pkg/index.js",
        "a\\file",
        "a:file",
    ] {
        let (args, export) = f.proposal(&captured, json!([creation(path, "new")]));
        assert!(
            apply(&f.store, &f.project, &args, &export).is_err(),
            "{path}"
        );
    }
    let (args, export) = f.proposal(
        &captured,
        json!([creation("new", "a"), creation("new/file", "b")]),
    );
    assert!(apply(&f.store, &f.project, &args, &export).is_err());
    let (args, mut export) = f.proposal(&captured, json!([creation("invalid.txt", "a")]));
    export["files"][0]["data"] = json!(STANDARD.encode([0xff]));
    assert!(apply(&f.store, &f.project, &args, &export).is_err());
    assert_eq!(f.read("a.txt"), b"old");
}
#[test]
fn proposal_cannot_write_through_a_symlink_or_replace_a_hardlink() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let outside = f.directory.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file.txt"), "outside").unwrap();
    symlink(&outside, Path::new(&f.project.folder).join("alias")).unwrap();
    let (args, export) = f.proposal(&captured, json!([creation("alias/file.txt", "new")]));
    assert!(apply(&f.store, &f.project, &args, &export).is_err());
    assert_eq!(fs::read(outside.join("file.txt")).unwrap(), b"outside");
    fs::hard_link(
        Path::new(&f.project.folder).join("a.txt"),
        f.directory.join("hardlink"),
    )
    .unwrap();
    let (args, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    assert!(apply(&f.store, &f.project, &args, &export).is_err());
    assert_eq!(f.read("a.txt"), b"old");
}
#[test]
fn replacing_project_directory_even_with_identical_files_invalidates_binding() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (args, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    fs::rename(&f.project.folder, f.directory.join("original-project")).unwrap();
    fs::create_dir(&f.project.folder).unwrap();
    f.write("a.txt", "old");
    assert!(apply(&f.store, &f.project, &args, &export)
        .unwrap_err()
        .contains("identity changed"));
    assert_eq!(f.read("a.txt"), b"old");
    assert_eq!(
        fs::read(f.directory.join("original-project/a.txt")).unwrap(),
        b"old"
    );
}
#[test]
fn recovers_partially_published_replace_delete_create_as_one_batch() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    f.write("remove.txt", "remove");
    f.write("last.txt", "last");
    let captured = f.bound();
    let (_, export) = f.proposal(
        &captured,
        json!([
            replacement("a.txt", "old", "new"),
            deletion("remove.txt", "remove"),
            creation("new/nested/file.txt", "created"),
            replacement("last.txt", "last", "changed")
        ]),
    );
    let (transaction, _) = f.interrupted(&export, 3);
    assert_eq!(f.read("a.txt"), b"new");
    assert!(Path::new(&f.project.folder)
        .join("new/nested/file.txt")
        .exists());
    assert_eq!(recover(&f.store, &f.project).unwrap()["recovered"], 1);
    assert_eq!(f.read("a.txt"), b"old");
    assert_eq!(f.read("remove.txt"), b"remove");
    assert_eq!(f.read("last.txt"), b"last");
    assert!(!Path::new(&f.project.folder).join("new").exists());
    let recovered = read_journal(&transaction).unwrap();
    assert_eq!(recovered.phase, "rolledBack");
    assert!(recovered.cleanup_complete);
    assert_eq!(recover(&f.store, &f.project).unwrap()["recovered"], 0);
}
#[test]
fn external_edit_blocks_recovery_before_any_file_is_restored_then_retry_recovers() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    f.write("b.txt", "before");
    let captured = f.bound();
    let (_, export) = f.proposal(
        &captured,
        json!([
            replacement("a.txt", "old", "new"),
            replacement("b.txt", "before", "after")
        ]),
    );
    let (transaction, journal) = f.interrupted(&export, 1);
    f.write("b.txt", "external edit");
    assert!(recover(&f.store, &f.project)
        .unwrap_err()
        .contains("external edit"));
    assert_eq!(f.read("a.txt"), b"new");
    assert_eq!(f.read("b.txt"), b"external edit");
    assert_eq!(transaction.read("before-0", MAX_FILE).unwrap().1, b"old");
    assert_eq!(read_journal(&transaction).unwrap().phase, "publishing");
    f.write("b.txt", "before");
    recover(&f.store, &f.project).unwrap();
    assert_eq!(f.read("a.txt"), b"old");
    assert!(!Path::new(&f.project.folder)
        .join(&journal.entries[0].temporary)
        .exists());
}
#[test]
fn edited_published_file_is_retained_with_original_backup() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (_, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    let (transaction, _) = f.interrupted(&export, 1);
    f.write("a.txt", "handwritten");
    assert!(recover(&f.store, &f.project).is_err());
    assert_eq!(f.read("a.txt"), b"handwritten");
    assert_eq!(transaction.read("before-0", MAX_FILE).unwrap().1, b"old");
}
#[test]
fn changed_backup_blocks_recovery_without_discarding_published_or_original_inodes() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (_, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    let (transaction, journal) = f.interrupted(&export, 1);
    fs::write(
        format!("/proc/self/fd/{}/before-0", transaction.0.as_raw_fd()),
        "tampered backup",
    )
    .unwrap();
    assert!(recover(&f.store, &f.project)
        .unwrap_err()
        .contains("backup changed"));
    assert_eq!(f.read("a.txt"), b"new");
    assert_eq!(f.read(&journal.entries[0].temporary), b"old");
}
#[test]
fn moved_parent_or_root_during_pending_transaction_keeps_both_generations() {
    let f = Fixture::new();
    f.write("dir/a.txt", "old");
    let captured = f.bound();
    let (_, export) = f.proposal(&captured, json!([replacement("dir/a.txt", "old", "new")]));
    let (_, journal) = f.interrupted(&export, 1);
    fs::rename(
        Path::new(&f.project.folder).join("dir"),
        Path::new(&f.project.folder).join("moved"),
    )
    .unwrap();
    f.write("dir/a.txt", "outside replacement");
    assert!(recover(&f.store, &f.project).is_err());
    assert_eq!(f.read("dir/a.txt"), b"outside replacement");
    assert_eq!(f.read("moved/a.txt"), b"new");
    assert_eq!(
        f.read(&format!("moved/{}", journal.entries[0].temporary)),
        b"old"
    );
    fs::rename(&f.project.folder, f.directory.join("original-project")).unwrap();
    fs::create_dir(&f.project.folder).unwrap();
    f.write("dir/a.txt", "new root");
    assert!(recover(&f.store, &f.project).is_err());
    assert_eq!(f.read("dir/a.txt"), b"new root");
}
#[test]
fn crash_after_anonymous_inode_registration_before_link_has_no_unowned_named_file() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (_, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    let (transaction, mut journal) = f.interrupted(&export, 0);
    let root = root(&f.project).unwrap();
    let parent = entry_parent(&root, &journal.entries[0]).unwrap();
    parent
        .unlink(
            &journal.entries[0].temporary,
            journal.entries[0].staged.as_ref().unwrap(),
        )
        .unwrap();
    journal.phase = "preparing".into();
    write_journal(&transaction, &journal).unwrap();
    recover(&f.store, &f.project).unwrap();
    assert_eq!(f.read("a.txt"), b"old");
    assert!(read_journal(&transaction).unwrap().cleanup_complete);
}
#[test]
fn crash_during_backup_preparation_can_recover_without_source_publication() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (_, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    let (transaction, mut journal) = f.interrupted(&export, 0);
    let parent = root(&f.project).unwrap();
    parent
        .unlink(
            &journal.entries[0].temporary,
            journal.entries[0].staged.as_ref().unwrap(),
        )
        .unwrap();
    journal.entries[0].staged = None;
    journal.phase = "preparing".into();
    write_journal(&transaction, &journal).unwrap();
    let backup = transaction.read("before-0", MAX_FILE).unwrap().0;
    transaction.unlink("before-0", &backup).unwrap();
    recover(&f.store, &f.project).unwrap();
    assert_eq!(f.read("a.txt"), b"old");
}
#[test]
fn idempotent_native_receipt_rejects_reused_digest_with_different_export_bytes() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (args, mut export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    apply(&f.store, &f.project, &args, &export).unwrap();
    export["files"][0]["data"] = json!(STANDARD.encode("different"));
    assert!(apply(&f.store, &f.project, &args, &export)
        .unwrap_err()
        .contains("different bytes"));
    assert_eq!(f.read("a.txt"), b"new");
}
#[test]
fn malformed_journal_and_symlinked_store_fail_closed() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (_, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    let (transaction, _) = f.interrupted(&export, 1);
    transaction
        .replace_record("journal.json", b"{\"schemaVersion\":999}")
        .unwrap();
    assert!(recover(&f.store, &f.project).is_err());
    assert_eq!(f.read("a.txt"), b"new");
    fs::rename(&f.store, f.directory.join("original-store")).unwrap();
    symlink(f.directory.join("original-store"), &f.store).unwrap();
    assert!(recover(&f.store, &f.project).is_err());
}
#[test]
fn independent_native_writer_lease_prevents_overlap() {
    let f = Fixture::new();
    let store = store(&f.store).unwrap();
    let first = lock(&store).unwrap();
    assert!(lock(&store).is_err());
    drop(first);
    lock(&store).unwrap();
}

#[test]
fn explicit_retry_after_a_clean_rollback_applies_the_same_reviewed_proposal() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (args, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    let (transaction, _) = f.interrupted(&export, 1);
    recover(&f.store, &f.project).unwrap();
    assert_eq!(f.read("a.txt"), b"old");
    assert_eq!(
        apply(&f.store, &f.project, &args, &export).unwrap()["status"],
        "applied"
    );
    assert_eq!(f.read("a.txt"), b"new");
    assert_eq!(read_journal(&transaction).unwrap().phase, "committed");
}

#[test]
fn final_source_inventory_stays_within_the_snapshot_budget() {
    let f = Fixture::new();
    for index in 0..MAX_FILES {
        f.write(&format!("{index}.txt"), "");
    }
    let captured = f.bound();
    let (args, export) = f.proposal(&captured, json!([creation("extra.txt", "extra")]));
    assert!(apply(&f.store, &f.project, &args, &export)
        .unwrap_err()
        .contains("snapshot budget"));
    assert!(!Path::new(&f.project.folder).join("extra.txt").exists());
    assert_eq!(
        capture(&f.project).unwrap()["sourceFiles"]
            .as_array()
            .unwrap()
            .len(),
        MAX_FILES
    );
}

#[test]
fn incomplete_host_file_records_fail_before_any_publication() {
    let f = Fixture::new();
    let captured = f.bound();
    let (args, export) = f.proposal(&captured, json!([creation("new.txt", "new")]));
    for field in ["expectedSha256", "data", "delete"] {
        let mut missing = export.clone();
        missing["files"][0].as_object_mut().unwrap().remove(field);
        assert!(
            apply(&f.store, &f.project, &args, &missing).is_err(),
            "{field}"
        );
    }
    assert!(!Path::new(&f.project.folder).join("new.txt").exists());
}

#[test]
fn recovery_does_not_recursively_remove_user_files_in_new_directories() {
    let f = Fixture::new();
    let captured = f.bound();
    let (_, export) = f.proposal(
        &captured,
        json!([creation("new/nested/proposed.txt", "new")]),
    );
    let (transaction, _) = f.interrupted(&export, 1);
    f.write("new/nested/user.txt", "handwritten");
    recover(&f.store, &f.project).unwrap();
    assert_eq!(f.read("new/nested/user.txt"), b"handwritten");
    assert!(!Path::new(&f.project.folder)
        .join("new/nested/proposed.txt")
        .exists());
    assert!(read_journal(&transaction).unwrap().cleanup_complete);
}

#[test]
fn journal_provenance_must_match_the_immutable_native_binding() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (_, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    let (transaction, mut journal) = f.interrupted(&export, 1);
    journal.binding.conversation_id = "forged-conversation".into();
    write_journal(&transaction, &journal).unwrap();
    assert!(recover(&f.store, &f.project)
        .unwrap_err()
        .contains("native job provenance"));
    assert_eq!(f.read("a.txt"), b"new");
    assert_eq!(transaction.read("before-0", MAX_FILE).unwrap().1, b"old");
}

#[test]
fn replacing_a_staging_slot_with_a_symlink_retains_source_and_link_target() {
    let f = Fixture::new();
    f.write("a.txt", "old");
    let captured = f.bound();
    let (_, export) = f.proposal(&captured, json!([replacement("a.txt", "old", "new")]));
    let (_, journal) = f.interrupted(&export, 0);
    let temporary = Path::new(&f.project.folder).join(&journal.entries[0].temporary);
    fs::remove_file(&temporary).unwrap();
    let outside = f.directory.join("outside.txt");
    fs::write(&outside, "outside").unwrap();
    symlink(&outside, &temporary).unwrap();
    assert!(recover(&f.store, &f.project).is_err());
    assert_eq!(f.read("a.txt"), b"old");
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
    assert!(fs::symlink_metadata(&temporary)
        .unwrap()
        .file_type()
        .is_symlink());
}
