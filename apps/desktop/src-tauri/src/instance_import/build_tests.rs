use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt},
};

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("work/modpack-install-2026-10-06/publication/fixtures")
            .join(nonce());
        fs::create_dir_all(base.join("game")).unwrap();
        let base = base.canonicalize().unwrap();
        Self {
            root: base.join("game"),
            base,
        }
    }
    fn write(&self, path: &Path, bytes: impl AsRef<[u8]>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn core(&self) -> BuildOperation {
        let operation = BuildOperation::begin(&self.root, "Named").unwrap();
        self.write(&operation.root_path().join("versions/Named/Named.json"),
            json!({"id":"Named","jar":"Named","clientVersion":"1.20.1","mainClass":"net.minecraft.client.main.Main","libraries":[]}).to_string());
        self.write(
            &operation.root_path().join("versions/Named/Named.jar"),
            b"client jar",
        );
        self.write(
            &operation
                .root_path()
                .join("libraries/generated/runtime.jar"),
            b"runtime absent from JSON",
        );
        self.write(
            &operation.root_path().join("assets/objects/ab/object"),
            b"asset",
        );
        operation
    }
    fn external_path(&self, target: &str, bytes: &[u8]) -> (VerifiedFile, PathBuf) {
        let path = self.base.join(format!(
            "input-{:x}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        self.write(&path, bytes);
        let input = VerifiedFile::from_fd(
            target.into(),
            File::open(&path).unwrap(),
            bytes.len() as u64,
            &format!("{:x}", Sha256::digest(bytes)),
            &AtomicBool::new(false),
        )
        .unwrap();
        (input, path)
    }
    fn external(&self, target: &str, bytes: &[u8]) -> VerifiedFile {
        self.external_path(target, bytes).0
    }
    fn prepared(&self) -> (BuildOperation, VerifiedOutputs) {
        let mut operation = self.core();
        let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
        publication::plan_native(&operation.root, &mut operation.journal, &outputs).unwrap();
        write_journal(&operation.operation, &operation.journal).unwrap();
        operation.journal.files_key =
            Some(operation.operation.mkdir("files").unwrap().key().unwrap());
        write_journal(&operation.operation, &operation.journal).unwrap();
        operation.journal.instance_key = Some(
            operation
                .operation
                .mkdir("instance")
                .unwrap()
                .key()
                .unwrap(),
        );
        write_journal(&operation.operation, &operation.journal).unwrap();
        publication::stage_native(
            &operation.operation,
            &mut operation.journal,
            &outputs,
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap();
        assemble_instance(
            &operation.operation,
            &mut operation.journal,
            &AtomicBool::new(false),
        )
        .unwrap();
        operation.journal.state = State::Prepared;
        write_journal(&operation.operation, &operation.journal).unwrap();
        (operation, outputs)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fn remove_fixture(path: &Path) {
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return;
            };
            if metadata.is_dir() {
                for child in fs::read_dir(path).unwrap() {
                    remove_fixture(&child.unwrap().path());
                }
                fs::remove_dir(path).unwrap();
            } else {
                fs::remove_file(path).unwrap();
            }
        }
        remove_fixture(&self.base);
    }
}
fn commit(operation: BuildOperation, outputs: VerifiedOutputs) -> Result<Value> {
    operation.publish_checked(
        outputs,
        &AtomicBool::new(false),
        |_| {},
        || Ok(()),
        || Ok(()),
    )
}

fn indexed_core(f: &Fixture, index: &[u8]) -> BuildOperation {
    use sha1::{Digest, Sha1};
    let operation = f.core();
    let metadata = json!({"id":"Named","jar":"Named","clientVersion":"1.20.1",
        "mainClass":"net.minecraft.client.main.Main","libraries":[],"assets":"5",
        "assetIndex":{"id":"5","sha1":format!("{:x}",Sha1::digest(index)),"size":index.len()}});
    f.write(
        &operation.root_path().join("versions/Named/Named.json"),
        metadata.to_string(),
    );
    f.write(&operation.root_path().join("assets/indexes/5.json"), index);
    operation
}

#[test]
fn different_index_revisions_install_without_replacing_existing_games_index() {
    use sha1::{Digest, Sha1};
    let f = Fixture::new();
    let original = f.root.join("assets/indexes/5.json");
    f.write(&original, b"existing index revision");
    let inode = fs::metadata(&original).unwrap().ino();
    let index = br#"{"objects":{}}"#;
    let alias = format!("pcl-{:x}", Sha1::digest(index));
    let mut operation = indexed_core(&f, index);
    let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    assert!(!operation.root_path().join("assets/indexes/5.json").exists());
    commit(operation, outputs).unwrap();
    assert_eq!(fs::read(&original).unwrap(), b"existing index revision");
    assert_eq!(fs::metadata(&original).unwrap().ino(), inode);
    let metadata: Value =
        serde_json::from_slice(&fs::read(f.root.join("versions/Named/Named.json")).unwrap())
            .unwrap();
    assert_eq!(metadata["assetIndex"]["id"], alias);
    assert_eq!(metadata["assets"], alias);
    assert_eq!(
        fs::read(f.root.join(format!("assets/indexes/{alias}.json"))).unwrap(),
        index
    );
    let readiness = pcl_core::inspection::inspect_files_with_isolation(
        &f.root,
        "Named",
        pcl_core::content_scope::IsolationPolicy::Automatic,
    )
    .unwrap();
    assert_eq!(readiness.missing_total, 0);
}

#[test]
fn index_alias_conflicts_tampering_and_cancellation_preserve_the_target() {
    use sha1::{Digest, Sha1};
    let index = br#"{"objects":{}}"#;
    let alias = format!("pcl-{:x}.json", Sha1::digest(index));
    for mode in [
        "target-conflict",
        "private-conflict",
        "hash",
        "cancel",
        "sealed-change",
    ] {
        let f = Fixture::new();
        f.write(&f.root.join("keep.txt"), b"keep");
        let mut operation = indexed_core(&f, index);
        if mode == "target-conflict" {
            f.write(
                &f.root.join("assets/indexes").join(&alias),
                b"foreign alias",
            );
        } else if mode == "private-conflict" {
            f.write(
                &operation.root_path().join("assets/indexes").join(&alias),
                b"foreign alias",
            );
        } else if mode == "hash" {
            f.write(
                &operation.root_path().join("assets/indexes/5.json"),
                b"changed bytes",
            );
        }
        let sealed = operation.seal(&AtomicBool::new(mode == "cancel"));
        if matches!(mode, "private-conflict" | "hash" | "cancel") {
            assert!(sealed.is_err(), "{mode}");
            operation.abort().unwrap();
        } else {
            let outputs = sealed.unwrap();
            if mode == "sealed-change" {
                f.write(
                    &operation.root_path().join("assets/indexes").join(&alias),
                    b"changed bytes",
                );
            }
            assert!(commit(operation, outputs).is_err(), "{mode}");
        }
        assert!(!f.root.join("versions/Named").exists());
        assert_eq!(fs::read(f.root.join("keep.txt")).unwrap(), b"keep");
        if mode == "target-conflict" {
            assert_eq!(
                fs::read(f.root.join("assets/indexes").join(&alias)).unwrap(),
                b"foreign alias"
            );
        }
    }
}

#[test]
fn building_is_durable_before_installer_bytes_and_restart_cleans_only_private_tree() {
    let f = Fixture::new();
    f.write(&f.root.join("libraries/existing.jar"), b"keep");
    let operation = BuildOperation::begin(&f.root, "Named").unwrap();
    let wire: Value = serde_json::from_slice(
        &fs::read(operation.root_path().parent().unwrap().join("journal.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(wire["schema"], 2);
    assert_eq!(wire["origin"], "modpack");
    assert_eq!(wire["state"], "building");
    assert!(wire["build_key"]["ino"].as_u64().is_some());
    assert_eq!(
        operation.root_fd().unwrap().metadata().unwrap().ino(),
        wire["build_key"]["ino"].as_u64().unwrap()
    );
    f.write(
        &operation
            .root_path()
            .join("versions/.install-incomplete/component-work/generated.jar"),
        b"partial",
    );
    f.write(&operation.root_path().join("libraries/new.jar"), b"private");
    assert!(ensure_ready(&f.root).is_err());
    assert!(ensure_ready_except_build(&f.root, Some(operation.operation_id())).is_ok());
    assert!(ensure_ready_except_build(&f.root, Some("i-other")).is_err());
    let private = operation.root_path().to_owned();
    drop(operation);
    // Building has never owned this external instance or real cache path.
    f.write(&f.root.join("versions/Named/user.txt"), b"external");
    assert_eq!(recover_pending(&f.root).unwrap()["recovered"], 1);
    assert!(!private.exists());
    assert_eq!(
        fs::read(f.root.join("versions/Named/user.txt")).unwrap(),
        b"external"
    );
    assert_eq!(
        fs::read(f.root.join("libraries/existing.jar")).unwrap(),
        b"keep"
    );
    assert_eq!(recover_pending(&f.root).unwrap()["recovered"], 0);
}

#[test]
fn cancelled_build_cleans_registered_regular_intermediates_and_releases_guard() {
    let f = Fixture::new();
    let mut operation = f.core();
    // Subtree ownership deliberately includes regular processor intermediates
    // even when they are not final outputs or individually journaled.
    f.write(
        &operation.root_path().join("processor-intermediate"),
        b"owned root subtree",
    );
    assert!(operation.seal(&AtomicBool::new(true)).is_err());
    operation.abort().unwrap();
    assert!(ensure_ready(&f.root).is_ok());
    assert!(!f.root.join("versions/Named").exists());
    assert!(!operation.root_path().exists());
}

#[test]
fn replacement_build_root_is_preserved_and_remains_recoverable_error() {
    let f = Fixture::new();
    let mut operation = f.core();
    let old = operation.root_path().with_file_name("detached-build");
    fs::rename(operation.root_path(), &old).unwrap();
    f.write(&operation.root_path().join("foreign.txt"), b"foreign");
    assert!(operation.verify_binding().is_err());
    assert!(operation.abort().is_err());
    assert_eq!(
        fs::read(operation.root_path().join("foreign.txt")).unwrap(),
        b"foreign"
    );
    drop(operation);
    assert!(recover_pending(&f.root).is_err());
    assert!(old.join("versions/Named/Named.jar").exists());
    assert!(ensure_ready(&f.root).is_err());
}

#[test]
fn special_private_node_is_preflighted_before_any_unlink_and_retry_recovers() {
    let f = Fixture::new();
    let mut operation = f.core();
    let untouched = operation.root_path().join("versions/Named/Named.jar");
    let link = operation.root_path().join("libraries/linked");
    symlink(&f.root, &link).unwrap();
    assert!(operation.abort().is_err());
    assert!(untouched.exists());
    let private = operation.root_path().to_owned();
    drop(operation);
    assert!(recover_pending(&f.root).is_err());
    assert!(untouched.exists());
    fs::remove_file(link).unwrap();
    recover_pending(&f.root).unwrap();
    assert!(!private.exists());
    assert!(ensure_ready(&f.root).is_ok());
}

#[test]
fn unknown_operation_content_is_retained_after_known_private_cleanup() {
    let f = Fixture::new();
    let mut operation = f.core();
    let unknown = operation.root_path().parent().unwrap().join("foreign.txt");
    f.write(&unknown, b"foreign");
    assert!(operation.abort().is_err());
    assert_eq!(fs::read(&unknown).unwrap(), b"foreign");
    assert!(!operation.root_path().exists());
    drop(operation);
    assert!(recover_pending(&f.root).is_err());
    assert!(ensure_ready(&f.root).is_err());
    fs::remove_file(unknown).unwrap();
    recover_pending(&f.root).unwrap();
}

#[test]
fn only_empty_unregistered_build_can_be_cleaned_after_key_registration_crash() {
    let f = Fixture::new();
    let mut operation = BuildOperation::begin(&f.root, "Named").unwrap();
    operation.journal.build.as_mut().unwrap().key = None;
    write_journal(&operation.operation, &operation.journal).unwrap();
    f.write(&operation.root_path().join("unregistered.txt"), b"unknown");
    drop(operation);
    assert!(recover_pending(&f.root).is_err());
    let store = f.root.join(".pcl-linux/instance-imports");
    let op = fs::read_dir(store)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    assert_eq!(
        fs::read(op.join("build/unregistered.txt")).unwrap(),
        b"unknown"
    );
    fs::remove_file(op.join("build/unregistered.txt")).unwrap();
    recover_pending(&f.root).unwrap();
}

#[test]
fn seal_rejects_unknown_layout_other_versions_and_linked_outputs() {
    for bad in ["root", "version", "link"] {
        let f = Fixture::new();
        let mut operation = f.core();
        match bad {
            "root" => f.write(&operation.root_path().join("runtime/file"), b"bad"),
            "version" => f.write(
                &operation.root_path().join("versions/Other/Other.jar"),
                b"bad",
            ),
            _ => {
                symlink(&f.root, operation.root_path().join("versions/Named/link")).unwrap();
            }
        }
        assert!(operation.seal(&AtomicBool::new(false)).is_err());
        if bad == "link" {
            assert!(operation.abort().is_err());
        } else {
            operation.abort().unwrap();
            assert!(ensure_ready(&f.root).is_ok());
        }
    }
}

#[test]
fn full_commit_retains_generated_runtime_and_empty_pack_is_isolated() {
    let f = Fixture::new();
    let mut operation = f.core();
    let core_inode = operation
        .open_file("versions/Named/Named.jar")
        .unwrap()
        .metadata()
        .unwrap()
        .ino();
    let private = operation.root_path().to_owned();
    let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    let result = commit(operation, outputs).unwrap();
    assert_eq!(result["files"], 4);
    assert_eq!(
        fs::read(f.root.join("libraries/generated/runtime.jar")).unwrap(),
        b"runtime absent from JSON"
    );
    assert_eq!(
        fs::read(f.root.join("assets/objects/ab/object")).unwrap(),
        b"asset"
    );
    assert!(f.root.join("versions/Named/config").is_dir());
    assert_ne!(
        fs::metadata(f.root.join("versions/Named/Named.jar"))
            .unwrap()
            .ino(),
        core_inode
    );
    assert!(!private.exists());
    assert!(ensure_ready(&f.root).is_ok());
}

#[test]
fn pack_inputs_are_copied_from_mutable_fds_and_core_collision_is_rejected() {
    let f = Fixture::new();
    let mut operation = f.core();
    let mut outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    let (input, input_path) = f.external_path("versions/Named/mods/example.jar", b"pack");
    let input_inode = fs::metadata(input_path).unwrap().ino();
    outputs.files.push(input);
    commit(operation, outputs).unwrap();
    assert_eq!(
        fs::read(f.root.join("versions/Named/mods/example.jar")).unwrap(),
        b"pack"
    );
    assert_ne!(
        fs::metadata(f.root.join("versions/Named/mods/example.jar"))
            .unwrap()
            .ino(),
        input_inode
    );
    let f = Fixture::new();
    let mut operation = f.core();
    let mut outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    outputs
        .files
        .push(f.external("versions/Named/Named.json", b"overwrite"));
    assert!(commit(operation, outputs).unwrap_err().contains("冲突"));
    assert!(ensure_ready(&f.root).is_ok());
    assert!(!f.root.join("versions/Named").exists());
}

#[test]
fn merged_file_directory_ancestors_fail_before_destination_writes() {
    for shared in [false, true] {
        let f = Fixture::new();
        let mut operation = f.core();
        let mut outputs = operation.seal(&AtomicBool::new(false)).unwrap();
        let path = if shared {
            "libraries/parent"
        } else {
            "versions/Named/config"
        };
        outputs.files.push(f.external(path, b"file"));
        outputs.directories.insert(format!("{path}/child"));
        assert!(commit(operation, outputs).is_err());
        assert!(ensure_ready(&f.root).is_ok());
        assert!(!f.root.join("libraries").exists());
    }
}

#[test]
fn fd_content_or_build_tree_changes_after_seal_fail_and_preserve_external_input() {
    let f = Fixture::new();
    let mut operation = f.core();
    let mut outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    let (input, actual) = f.external_path("versions/Named/mods/example.jar", b"same");
    let alias = f.base.join("alias");
    fs::hard_link(&actual, &alias).unwrap();
    f.write(&alias, b"edit");
    outputs.files.push(input);
    assert!(commit(operation, outputs).is_err());
    assert_eq!(fs::read(actual).unwrap(), b"edit");
    assert!(ensure_ready(&f.root).is_ok());
    for replacement in [false, true] {
        let f = Fixture::new();
        let mut operation = f.core();
        let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
        if replacement {
            let jar = operation.root_path().join("versions/Named/Named.jar");
            fs::remove_file(&jar).unwrap();
            f.write(&jar, b"client jar");
        } else {
            f.write(
                &operation.root_path().join("versions/Named/new.txt"),
                b"new",
            );
        }
        assert!(commit(operation, outputs).is_err());
        assert!(ensure_ready(&f.root).is_ok());
    }
}

#[test]
fn identical_shared_cache_is_reused_and_different_cache_is_retained() {
    let f = Fixture::new();
    f.write(
        &f.root.join("libraries/generated/runtime.jar"),
        b"runtime absent from JSON",
    );
    let old = fs::metadata(f.root.join("libraries/generated/runtime.jar")).unwrap();
    let mut operation = f.core();
    let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    assert_eq!(commit(operation, outputs).unwrap()["reused_files"], 1);
    let new = fs::metadata(f.root.join("libraries/generated/runtime.jar")).unwrap();
    assert_eq!((old.ino(), old.mtime_nsec()), (new.ino(), new.mtime_nsec()));
    let f = Fixture::new();
    f.write(
        &f.root.join("libraries/generated/runtime.jar"),
        b"different",
    );
    let mut operation = f.core();
    let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    assert!(commit(operation, outputs)
        .unwrap_err()
        .contains("已保留原文件"));
    assert_eq!(
        fs::read(f.root.join("libraries/generated/runtime.jar")).unwrap(),
        b"different"
    );
    assert!(ensure_ready(&f.root).is_ok());
}

#[test]
fn prepared_shared_half_publication_recovery_removes_only_owned_outputs() {
    let f = Fixture::new();
    f.write(
        &f.root.join("libraries/generated/runtime.jar"),
        b"runtime absent from JSON",
    );
    let (mut operation, _outputs) = f.prepared();
    ensure_destination_dirs(
        &operation.root,
        &operation.operation,
        &mut operation.journal,
    )
    .unwrap();
    let files = operation.operation.child("files").unwrap();
    let index = operation
        .journal
        .files
        .iter()
        .position(|f| f.target.starts_with("assets/"))
        .unwrap();
    let (parent, name) = destination_parent(
        &operation.root,
        &operation.journal.files[index].target,
        &operation.journal,
    )
    .unwrap();
    link_file(&files, &slot(index), &parent, &name).unwrap();
    drop(operation);
    recover_pending(&f.root).unwrap();
    assert!(!f.root.join("versions/Named").exists());
    assert!(!f.root.join("assets").exists());
    assert_eq!(
        fs::read(f.root.join("libraries/generated/runtime.jar")).unwrap(),
        b"runtime absent from JSON"
    );
    assert!(ensure_ready(&f.root).is_ok());
    assert_eq!(recover_pending(&f.root).unwrap()["recovered"], 0);
}

#[test]
fn moved_instance_with_failed_durable_commit_rolls_back_and_keeps_reuse() {
    let f = Fixture::new();
    f.write(
        &f.root.join("libraries/generated/runtime.jar"),
        b"runtime absent from JSON",
    );
    let mut operation = f.core();
    let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    FAIL_COMMITTED_JOURNAL_WRITE.with(|fail| fail.set(true));
    assert!(commit(operation, outputs)
        .unwrap_err()
        .contains("fixture durable commit failure"));
    assert!(!f.root.join("versions/Named").exists());
    assert!(!f.root.join("assets").exists());
    assert_eq!(
        fs::read(f.root.join("libraries/generated/runtime.jar")).unwrap(),
        b"runtime absent from JSON"
    );
    assert!(ensure_ready(&f.root).is_ok());
}

#[test]
fn committed_cleanup_conflict_retains_installation_then_retries_without_source() {
    let f = Fixture::new();
    let (mut operation, _outputs) = f.prepared();
    publication::publish_prepared(
        &operation.root,
        &operation.operation,
        &mut operation.journal,
        || Ok(()),
    )
    .unwrap();
    let private = operation.root_path().to_owned();
    let link = private.join("special");
    symlink(&f.base, &link).unwrap();
    drop(operation);
    assert!(recover_pending(&f.root).is_err());
    assert_eq!(
        fs::read(f.root.join("versions/Named/Named.jar")).unwrap(),
        b"client jar"
    );
    assert_eq!(
        fs::read(f.root.join("libraries/generated/runtime.jar")).unwrap(),
        b"runtime absent from JSON"
    );
    fs::remove_file(link).unwrap();
    recover_pending(&f.root).unwrap();
    assert!(!private.exists());
    assert!(f.root.join("versions/Named/config").is_dir());
}

#[test]
fn cancellation_during_copy_or_at_gate_cleans_and_late_cancel_retains_commit() {
    for at_gate in [false, true] {
        let f = Fixture::new();
        let mut operation = f.core();
        let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
        let cancel = AtomicBool::new(false);
        let result = operation.publish_checked(
            outputs,
            &cancel,
            |p| {
                if !at_gate && p.stage == "import-extract" && p.bytes_done > 0 {
                    cancel.store(true, Ordering::SeqCst);
                }
            },
            || Ok(()),
            || {
                if at_gate {
                    cancel.store(true, Ordering::SeqCst);
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(ensure_ready(&f.root).is_ok());
        assert!(!f.root.join("versions/Named").exists());
    }
    let f = Fixture::new();
    let mut operation = f.core();
    let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    let cancel = AtomicBool::new(false);
    let bindings = std::cell::Cell::new(0);
    let result = operation.publish_checked(
        outputs,
        &cancel,
        |_| {},
        || {
            bindings.set(bindings.get() + 1);
            if bindings.get() == 2 {
                cancel.store(true, Ordering::SeqCst);
            }
            Ok(())
        },
        || Ok(()),
    );
    result.unwrap();
    assert_eq!(bindings.get(), 2);
    assert!(cancel.load(Ordering::SeqCst));
    assert!(f.root.join("versions/Named/Named.jar").exists());
    assert!(ensure_ready(&f.root).is_ok());
}

#[test]
fn source_binding_and_precommit_reference_failures_rollback_prepared_private_outputs() {
    for source_failure in [false, true] {
        let f = Fixture::new();
        let mut operation = f.core();
        let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
        let result = operation.publish_checked(
            outputs,
            &AtomicBool::new(false),
            |_| {},
            || {
                if source_failure {
                    Err("source changed".into())
                } else {
                    Ok(())
                }
            },
            || Err("reference changed".into()),
        );
        assert!(result.is_err());
        assert!(ensure_ready(&f.root).is_ok());
        assert!(!f.root.join("versions/Named").exists());
        assert!(!f.root.join("libraries").exists());
    }
}

#[test]
fn schema_dispatch_keeps_legacy_strict_and_unknown_versions_preserved() {
    let f = Fixture::new();
    let operation = BuildOperation::begin(&f.root, "Named").unwrap();
    let encoded = journal::encode(&operation.journal).unwrap();
    let mut wire: Value = serde_json::from_slice(&encoded).unwrap();
    wire["schema"] = 1.into();
    assert!(journal::decode(&serde_json::to_vec(&wire).unwrap()).is_err());
    wire["schema"] = 3.into();
    assert!(journal::decode(&serde_json::to_vec(&wire).unwrap()).is_err());
    wire["schema"] = 2.into();
    wire["origin"] = "future".into();
    assert!(journal::decode(&serde_json::to_vec(&wire).unwrap()).is_err());
}

#[test]
fn native_output_scale_keeps_full_journal_writes_bounded() {
    let f = Fixture::new();
    let mut operation = f.core();
    for i in 0..1200 {
        f.write(
            &operation
                .root_path()
                .join(format!("libraries/runtime/lib-{i}.jar")),
            b"runtime",
        );
    }
    let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    FULL_JOURNAL_WRITES.with(|writes| writes.set(0));
    assert_eq!(commit(operation, outputs).unwrap()["files"], 1204);
    assert!(FULL_JOURNAL_WRITES.with(|writes| writes.get()) <= 6);
    assert!(f.root.join("libraries/runtime/lib-1199.jar").exists());
}

#[test]
fn dropping_generated_runtime_from_publication_plan_is_rejected() {
    let f = Fixture::new();
    let mut operation = f.core();
    let mut outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    outputs
        .files
        .retain(|file| file.target() != "libraries/generated/runtime.jar");
    assert!(commit(operation, outputs)
        .unwrap_err()
        .contains("完整构建文件"));
    assert!(ensure_ready(&f.root).is_ok());
    assert!(!f.root.join("versions/Named").exists());
}

#[test]
fn complete_native_publication_works_with_a_small_process_fd_limit() {
    const FLAG: &str = "PCL_TEST_PRIVATE_BUILD_FD_LIMIT";
    if std::env::var_os(FLAG).is_some() {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        limit.rlim_cur = 96;
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        let f = Fixture::new();
        let mut operation = f.core();
        for i in 0..240 {
            f.write(
                &operation
                    .root_path()
                    .join(format!("libraries/runtime/part-{i}.jar")),
                b"runtime",
            );
        }
        let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
        assert_eq!(commit(operation, outputs).unwrap()["files"], 244);
        assert!(ensure_ready(&f.root).is_ok());
        return;
    }
    let output=std::process::Command::new(std::env::current_exe().unwrap())
        .args(["instance_import::build::tests::complete_native_publication_works_with_a_small_process_fd_limit","--exact","--nocapture","--test-threads=1"])
        .env(FLAG,"1").output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn replaced_captured_root_is_rejected_before_transaction_store_creation() {
    let f = Fixture::new();
    let root = Dir::open(&f.root).unwrap();
    let expected = root.key().unwrap();
    let detached = f.base.join("detached-game");
    fs::rename(&f.root, &detached).unwrap();
    fs::create_dir(&f.root).unwrap();
    f.write(&f.root.join("user.txt"), b"replacement");
    assert!(BuildOperation::begin_bound(&f.root, "Named", &expected, None).is_err());
    assert!(!f.root.join(".pcl-linux").exists());
    assert!(!detached.join(".pcl-linux").exists());
    assert_eq!(fs::read(f.root.join("user.txt")).unwrap(), b"replacement");
}

#[test]
fn foreign_content_added_to_finished_private_operation_is_not_hidden_by_terminal_state() {
    let f = Fixture::new();
    let mut operation = f.core();
    let operation_path = operation.root_path().parent().unwrap().to_owned();
    let outputs = operation.seal(&AtomicBool::new(false)).unwrap();
    commit(operation, outputs).unwrap();
    let foreign = operation_path.join("foreign.txt");
    f.write(&foreign, b"keep");
    assert!(ensure_ready(&f.root).is_err());
    assert!(recover_pending(&f.root).is_err());
    assert_eq!(fs::read(&foreign).unwrap(), b"keep");
    assert_eq!(
        fs::read(f.root.join("versions/Named/Named.jar")).unwrap(),
        b"client jar"
    );
    fs::remove_file(foreign).unwrap();
    recover_pending(&f.root).unwrap();
}

#[test]
fn replaced_captured_versions_tree_is_rejected_before_store_creation() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("versions")).unwrap();
    let root = Dir::open(&f.root).unwrap();
    let expected_root = root.key().unwrap();
    let expected_versions = root.child("versions").unwrap().key().unwrap();
    fs::rename(f.root.join("versions"), f.base.join("old-versions")).unwrap();
    fs::create_dir(f.root.join("versions")).unwrap();
    f.write(&f.root.join("versions/user.txt"), b"keep replacement");
    assert!(BuildOperation::begin_bound(
        &f.root,
        "Named",
        &expected_root,
        Some(&expected_versions),
    )
    .is_err());
    assert!(!f.root.join(".pcl-linux").exists());
    assert_eq!(
        fs::read(f.root.join("versions/user.txt")).unwrap(),
        b"keep replacement"
    );
}
