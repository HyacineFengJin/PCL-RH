use super::*;
use sha2::{Digest, Sha512};
use std::{fs, io::Write, os::unix::fs::symlink};
struct Fixture {
    root: PathBuf,
    source: PathBuf,
}
impl Fixture {
    fn new(isolated: bool, dirs: bool) -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("work/autonomous-2026-10-04/modrinth/batch/fixtures")
            .join(op_id());
        let root = base.join("root");
        let source = base.join("source");
        fs::create_dir_all(root.join("versions/test")).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(
            root.join("versions/test/test.json"),
            br#"{"id":"test","libraries":[]}"#,
        )
        .unwrap();
        if isolated {
            fs::create_dir_all(root.join("versions/test/config")).unwrap();
        }
        if dirs {
            for k in ["mods", "resourcepacks", "shaderpacks"] {
                fs::create_dir_all(if isolated {
                    root.join("versions/test").join(k)
                } else {
                    root.join(k)
                })
                .unwrap();
            }
        }
        Self {
            root: root.canonicalize().unwrap(),
            source: source.canonicalize().unwrap(),
        }
    }
    fn files(&self) -> Vec<VerifiedImport> {
        ["mods", "resourcepacks", "shaderpacks"]
            .iter()
            .map(|kind| {
                self.file(
                    kind,
                    if *kind == "mods" {
                        "sample.jar"
                    } else {
                        "sample.zip"
                    },
                )
            })
            .collect()
    }
    fn file(&self, kind: &str, name: &str) -> VerifiedImport {
        let file = Dir::open(&self.source).unwrap().anonymous().unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("payload.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"generic resource fixture").unwrap();
        let mut file = writer.finish().unwrap();
        file.sync_all().unwrap();
        let size = file.metadata().unwrap().len();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        VerifiedImport {
            kind: kind.into(),
            file_name: name.into(),
            file,
            size,
            sha512: format!("{:x}", Sha512::digest(&bytes)),
        }
    }
    fn folder(&self, kind: &str) -> PathBuf {
        crate::ui_data::resource_dir(&self.root, "test", kind).unwrap()
    }
    fn clean(&self) {
        ensure_ready(&self.root).unwrap();
        if let Some(store) = storage(&Dir::open(&self.root).unwrap(), false).unwrap() {
            assert!(store.names().unwrap().is_empty());
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(self.root.parent().unwrap()).unwrap();
    }
}
fn run(
    f: &Fixture,
    files: &mut [VerifiedImport],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<()>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult> {
    import_verified_batch(&f.root, "test", files, cancel, commit, progress)
}
fn prepared(f: &Fixture, files: &mut [VerifiedImport]) -> (Dir, Journal) {
    let c = AtomicBool::new(false);
    let tokens = source_tokens(files, &c).unwrap();
    let (scope, root) = Scope::capture(
        &f.root,
        "test",
        &files.iter().map(|i| i.kind.clone()).collect(),
    )
    .unwrap();
    let (operation, mut j) = setup(&root, scope, files).unwrap();
    stage(&operation, &mut j, files, &tokens, &c, &mut |_, _| {}).unwrap();
    j.state = State::Prepared;
    write_journal(&operation, &j).unwrap();
    (operation, j)
}
fn first_publish(operation: &Dir, j: &mut Journal) {
    let root = j.scope.base().unwrap();
    prepare_directories(&root, operation, j).unwrap();
    let stage = operation.child("files").unwrap();
    let item = &j.items[0];
    let dir = target(&root, j, &item.kind).unwrap().unwrap();
    let name = item.file_name.clone();
    register_published(operation, j, 0).unwrap();
    j.items[0].published = true;
    stage.link(&slot(0), &dir, &name).unwrap();
}
#[test]
fn mixed_kind_shared_success() {
    mixed(false)
}
#[test]
fn mixed_kind_isolated_success() {
    mixed(true)
}
fn mixed(isolated: bool) {
    let f = Fixture::new(isolated, false);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    let mut calls = 0;
    let result = run(
        &f,
        &mut files,
        &c,
        &mut || {
            calls += 1;
            for kind in ["mods", "resourcepacks", "shaderpacks"] {
                assert!(!f.folder(kind).exists());
            }
            Ok(())
        },
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(result.changed, 3);
    for i in files {
        let mut output = File::open(f.folder(&i.kind).join(&i.file_name)).unwrap();
        filesystem::verify_file(&mut output, None, i.size, &i.sha512, None).unwrap();
        filesystem::anonymous_source(&i.file).unwrap();
    }
    f.clean();
}
#[test]
fn wrong_hash_and_named_source_reject_before_target_mutation() {
    let f = Fixture::new(false, false);
    let c = AtomicBool::new(false);
    let mut files = f.files();
    files[0].sha512 = "0".repeat(128);
    assert!(
        run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {})
            .unwrap_err()
            .contains("SHA512")
    );
    f.clean();
    let mut files = f.files();
    files[0].file = File::open(f.root.join("versions/test/test.json")).unwrap();
    assert!(
        run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {})
            .unwrap_err()
            .contains("匿名")
    );
    f.clean();
}
#[test]
fn invalid_archive_rejects_with_correct_hash() {
    let f = Fixture::new(false, false);
    let c = AtomicBool::new(false);
    let mut file = Dir::open(&f.source).unwrap().anonymous().unwrap();
    file.write_all(b"not an archive").unwrap();
    let mut files = vec![VerifiedImport {
        kind: "mods".into(),
        file_name: "test.jar".into(),
        file,
        size: 14,
        sha512: format!("{:x}", Sha512::digest(b"not an archive")),
    }];
    assert!(run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {}).is_err());
    f.clean();
}
#[test]
fn precommit_cancel_cleans_all_private_staging() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    assert_eq!(
        run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {
            c.store(true, Ordering::Release)
        })
        .unwrap_err(),
        CANCELLED
    );
    f.clean();
    assert!(!f.root.join("mods").exists());
}
#[test]
fn commit_reject_and_gate_cancel_preserve_error_and_cleanup() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    assert_eq!(
        run(
            &f,
            &mut files,
            &c,
            &mut || {
                c.store(true, Ordering::Release);
                Err("plan changed".into())
            },
            &mut |_, _| {}
        )
        .unwrap_err(),
        "plan changed"
    );
    f.clean();
    c.store(false, Ordering::Release);
    let mut files = f.files();
    assert_eq!(
        run(
            &f,
            &mut files,
            &c,
            &mut || {
                c.store(true, Ordering::Release);
                Ok(())
            },
            &mut |_, _| {}
        )
        .unwrap_err(),
        CANCELLED
    );
    f.clean();
}
#[test]
fn late_cancel_does_not_split_committed_batch() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    let mut admitted = false;
    run(
        &f,
        &mut files,
        &c,
        &mut || {
            admitted = true;
            Ok(())
        },
        &mut |_, _| {
            if f.root.join("mods/sample.jar").exists() {
                c.store(true, Ordering::Release)
            }
        },
    )
    .unwrap();
    assert!(admitted);
    assert!(c.load(Ordering::Acquire));
    f.clean();
}
#[test]
fn partial_publish_foreign_collision_rolls_back_owned_outputs() {
    let f = Fixture::new(false, true);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    let second = f.folder("resourcepacks").join("sample.zip");
    let e = run(&f, &mut files, &c, &mut || Ok(()), &mut |_, _| {
        if f.folder("mods").join("sample.jar").exists() && !second.exists() {
            fs::write(&second, b"external").unwrap();
        }
    })
    .unwrap_err();
    assert!(!e.starts_with("取消清理失败"), "{e}");
    assert!(!f.folder("mods").join("sample.jar").exists());
    assert_eq!(fs::read(second).unwrap(), b"external");
    f.clean();
}
#[test]
fn source_modified_by_commit_callback_preserves_ordinary_error() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let mut external = files[0].file.try_clone().unwrap();
    let c = AtomicBool::new(false);
    let e = run(
        &f,
        &mut files,
        &c,
        &mut || {
            external.seek(SeekFrom::Start(0)).unwrap();
            external.write_all(b"changed").unwrap();
            c.store(true, Ordering::Release);
            Ok(())
        },
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(e.contains("描述符已经变化"), "{e}");
    f.clean();
}
#[test]
fn prepared_crash_rolls_back_all_kinds_without_selected_profile() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let (op, mut j) = prepared(&f, &mut files);
    first_publish(&op, &mut j);
    fs::remove_file(f.root.join("versions/test/test.json")).unwrap();
    assert!(ensure_ready(&f.root).is_err());
    recover_root(&f.root).unwrap();
    assert!(!f.root.join("mods").exists());
    f.clean();
}
#[test]
fn repeated_recovery_after_first_output_and_directory_were_removed() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let (op, mut j) = prepared(&f, &mut files);
    first_publish(&op, &mut j);
    let root = j.scope.base().unwrap();
    let dir = target(&root, &j, "mods").unwrap().unwrap();
    dir.unlink("sample.jar", false).unwrap();
    root.unlink("mods", true).unwrap();
    // Receipt still says published, exactly as a crash during rollback leaves it.
    recover_root(&f.root).unwrap();
    f.clean();
    assert!(!f.root.join("shaderpacks").exists());
}
#[test]
fn committed_crash_keeps_outputs_and_finishes_private_cleanup() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let (op, mut j) = prepared(&f, &mut files);
    let root = j.scope.base().unwrap();
    publish(&root, &op, &mut j, &mut |_, _| {}).unwrap();
    recover_root(&f.root).unwrap();
    for i in &files {
        assert!(f.folder(&i.kind).join(&i.file_name).exists());
    }
    f.clean();
}
#[test]
fn unknown_stage_and_external_content_are_preserved_for_retry() {
    let f = Fixture::new(false, true);
    let mut files = f.files();
    let (op, mut j) = prepared(&f, &mut files);
    first_publish(&op, &mut j);
    let path = f.folder("mods").join("sample.jar");
    let original = fs::read(&path).unwrap();
    fs::write(&path, b"external edit").unwrap();
    assert!(recover_root(&f.root).unwrap_err().contains("取消清理失败"));
    assert_eq!(fs::read(&path).unwrap(), b"external edit");
    fs::write(&path, original).unwrap();
    recover_root(&f.root).unwrap();
    f.clean();
    let mut files = f.files();
    let (op, _) = prepared(&f, &mut files);
    let stage = op.child("files").unwrap();
    let foreign = stage.anonymous().unwrap();
    stage.link_anonymous(&foreign, "unknown").unwrap();
    assert!(recover_root(&f.root).is_err());
    stage.unlink("unknown", false).unwrap();
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn external_inode_and_directory_replacement_block_recovery() {
    let f = Fixture::new(false, true);
    let mut files = f.files();
    let (op, mut j) = prepared(&f, &mut files);
    first_publish(&op, &mut j);
    let path = f.folder("mods").join("sample.jar");
    let bytes = fs::read(&path).unwrap();
    fs::rename(&path, f.source.join("owned")).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(recover_root(&f.root).is_err());
    fs::remove_file(&path).unwrap();
    fs::rename(f.source.join("owned"), &path).unwrap();
    fs::rename(f.root.join("mods"), f.root.join("moved-mods")).unwrap();
    fs::create_dir(f.root.join("mods")).unwrap();
    assert!(recover_root(&f.root).is_err());
    fs::remove_dir(f.root.join("mods")).unwrap();
    fs::rename(f.root.join("moved-mods"), f.root.join("mods")).unwrap();
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn safe_names_duplicates_and_disabled_counterparts_are_rejected() {
    let f = Fixture::new(false, true);
    let c = AtomicBool::new(false);
    for name in [
        "../bad.jar",
        "bad:name.jar",
        ".pcl-owned.jar",
        "bad.jar.disabled",
        "bad.zip",
    ] {
        let mut files = vec![f.file("mods", name)];
        assert!(run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {}).is_err());
    }
    let mut files = vec![f.file("mods", "test.jar"), f.file("mods", "test.jar")];
    assert!(run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {}).is_err());
    fs::write(f.folder("mods").join("test.jar.disabled"), b"disabled").unwrap();
    let mut files = vec![f.file("mods", "test.jar")];
    assert!(run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {}).is_err());
    f.clean();
}
#[test]
fn linked_resource_directory_is_not_followed() {
    let f = Fixture::new(false, false);
    symlink(&f.source, f.root.join("mods")).unwrap();
    let c = AtomicBool::new(false);
    let mut files = f.files();
    assert!(run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {}).is_err());
    assert!(f.source.read_dir().unwrap().next().is_none());
    f.clean();
}
#[test]
fn empty_reuse_batch_has_one_gate_and_no_record() {
    let f = Fixture::new(false, false);
    let c = AtomicBool::new(false);
    let mut calls = 0;
    let result = run(
        &f,
        &mut [],
        &c,
        &mut || {
            calls += 1;
            Ok(())
        },
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(result.changed, 0);
    assert_eq!(calls, 1);
    f.clean();
}
#[test]
fn commit_record_sync_failure_keeps_disk_committed_outputs() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    journal::FAIL_COMMITTED_WRITE.with(|v| v.set(true));
    let error = run(&f, &mut files, &c, &mut || Ok(()), &mut |_, _| {}).unwrap_err();
    assert!(error.contains("提交日志同步失败"));
    assert!(ensure_ready(&f.root).is_err());
    for i in &files {
        assert!(f.folder(&i.kind).join(&i.file_name).exists());
    }
    recover_root(&f.root).unwrap();
    for i in &files {
        assert!(f.folder(&i.kind).join(&i.file_name).exists());
    }
    f.clean();
}
#[test]
fn ownership_receipt_before_first_path_and_staging_crash_are_recoverable() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    let (scope, root) = Scope::capture(
        &f.root,
        "test",
        &files.iter().map(|i| i.kind.clone()).collect(),
    )
    .unwrap();
    let (op, j) = setup(&root, scope, &files).unwrap();
    let stage = op.child("files").unwrap();
    let mut anonymous = stage.anonymous().unwrap();
    files[0].file.seek(SeekFrom::Start(0)).unwrap();
    std::io::copy(&mut files[0].file, &mut anonymous).unwrap();
    anonymous.sync_all().unwrap();
    let owned = filesystem::verify_file(
        &mut anonymous,
        None,
        files[0].size,
        &files[0].sha512,
        Some(&c),
    )
    .unwrap();
    register_owned(&op, &j, 0, &owned).unwrap();
    // Crash here: durable receipt exists but the anonymous inode has no name.
    drop(anonymous);
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn directory_move_has_a_durable_inode_before_target_exists() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let (op, j) = prepared(&f, &mut files);
    let private = op.mkdir("target-mods").unwrap();
    register_directory(&op, &j, "mods", &private.key().unwrap()).unwrap();
    assert!(!f.root.join("mods").exists());
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn limits_reject_without_allocating_large_files() {
    let f = Fixture::new(false, false);
    let c = AtomicBool::new(false);
    let mut files = vec![f.file("mods", "big.jar")];
    files[0].size = MAX_FILE_BYTES + 1;
    assert!(run(&f, &mut files, &c, &mut || panic!("commit"), &mut |_, _| {}).is_err());
    let file = f.file("mods", "test.jar");
    let mut many: Vec<_> = (0..MAX_FILES + 1)
        .map(|n| VerifiedImport {
            kind: "mods".into(),
            file_name: format!("test{n}.jar"),
            file: file.file.try_clone().unwrap(),
            size: file.size,
            sha512: file.sha512.clone(),
        })
        .collect();
    assert!(
        run(&f, &mut many, &c, &mut || panic!("commit"), &mut |_, _| {})
            .unwrap_err()
            .contains("512")
    );
    f.clean();
}
#[test]
fn commit_target_directory_replacement_is_preserved_until_restored() {
    let f = Fixture::new(false, true);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    let saved = f.root.join("saved-mods");
    let error = run(
        &f,
        &mut files,
        &c,
        &mut || {
            fs::rename(f.root.join("mods"), &saved).unwrap();
            fs::create_dir(f.root.join("mods")).unwrap();
            fs::write(f.root.join("mods/user.jar"), b"user file").unwrap();
            Ok(())
        },
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(error.starts_with("取消清理失败"), "{error}");
    assert_eq!(
        fs::read(f.root.join("mods/user.jar")).unwrap(),
        b"user file"
    );
    assert!(ensure_ready(&f.root).is_err());
    fs::remove_file(f.root.join("mods/user.jar")).unwrap();
    fs::remove_dir(f.root.join("mods")).unwrap();
    fs::rename(saved, f.root.join("mods")).unwrap();
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn prepared_publication_intent_catches_edit_before_postlink_receipt() {
    let f = Fixture::new(false, true);
    let mut files = f.files();
    let (op, j) = prepared(&f, &mut files);
    register_published(&op, &j, 0).unwrap();
    let root = j.scope.base().unwrap();
    let dir = target(&root, &j, "mods").unwrap().unwrap();
    op.child("files")
        .unwrap()
        .link(&slot(0), &dir, "sample.jar")
        .unwrap();
    let path = f.folder("mods").join("sample.jar");
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, b"edited before crash").unwrap();
    assert!(recover_root(&f.root).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"edited before crash");
    fs::write(path, bytes).unwrap();
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn late_disabled_counterpart_conflict_rolls_back_and_preserves_foreign_file() {
    let f = Fixture::new(false, true);
    let mut files = f.files();
    let c = AtomicBool::new(false);
    let disabled = f.folder("mods").join("sample.jar.disabled");
    let result = run(&f, &mut files, &c, &mut || Ok(()), &mut |_, _| {
        if f.folder("shaderpacks").join("sample.zip").exists() && !disabled.exists() {
            fs::write(&disabled, b"external disabled").unwrap();
        }
    });
    assert!(result.is_err());
    assert_eq!(fs::read(disabled).unwrap(), b"external disabled");
    for i in files {
        assert!(!f.folder(&i.kind).join(i.file_name).exists());
    }
    f.clean();
}
#[test]
fn pending_batch_blocks_legacy_write_before_missing_target_creation() {
    let f = Fixture::new(false, false);
    let mut files = f.files();
    let _prepared = prepared(&f, &mut files);
    let c = AtomicBool::new(false);
    let result = super::super::set_enabled(
        &f.root,
        "test",
        "mods",
        &[],
        true,
        &c,
        &mut || panic!("commit"),
        &mut |_, _| {},
    );
    assert!(result.unwrap_err().contains("未完成"));
    assert!(!f.root.join("mods").exists());
    super::super::recover_verified_batches(&f.root).unwrap();
    f.clean();
}

#[test]
fn explicit_isolation_publishes_all_resource_kinds_in_the_captured_base() {
    for policy in [IsolationPolicy::Shared, IsolationPolicy::Isolated] {
        let f = Fixture::new(policy == IsolationPolicy::Shared, false);
        let base = if policy == IsolationPolicy::Isolated {
            f.root.join("versions/test")
        } else {
            f.root.clone()
        };
        let other = if policy == IsolationPolicy::Isolated {
            f.root.clone()
        } else {
            f.root.join("versions/test")
        };
        let mut files = f.files();
        import_verified_batch_with_isolation(
            &f.root,
            "test",
            &mut files,
            &AtomicBool::new(false),
            &mut || Ok(()),
            &mut |_, _| {},
            policy,
        )
        .unwrap();
        for file in &files {
            assert!(base.join(&file.kind).join(&file.file_name).is_file());
            assert!(!other.join(&file.kind).join(&file.file_name).exists());
        }
        f.clean();
    }
}

#[test]
fn explicit_shared_interrupted_batch_recovers_its_recorded_paths_with_isolated_markers() {
    let f = Fixture::new(true, false);
    let mut files = f.files();
    let cancel = AtomicBool::new(false);
    let tokens = source_tokens(&mut files, &cancel).unwrap();
    let (scope, root) = Scope::capture_with_isolation(
        &f.root,
        "test",
        &files.iter().map(|f| f.kind.clone()).collect(),
        IsolationPolicy::Shared,
    )
    .unwrap();
    let (operation, mut journal) = setup(&root, scope, &files).unwrap();
    stage(
        &operation,
        &mut journal,
        &mut files,
        &tokens,
        &cancel,
        &mut |_, _| {},
    )
    .unwrap();
    journal.state = State::Prepared;
    write_journal(&operation, &journal).unwrap();
    first_publish(&operation, &mut journal);
    assert!(f.root.join("mods/sample.jar").exists());
    assert!(pcl_core::content_scope::inspect(&f.root, "test")
        .unwrap()
        .is_isolated());
    recover_root(&f.root).unwrap();
    assert!(!f.root.join("mods/sample.jar").exists());
    assert!(f.root.join("versions/test/config").is_dir());
    assert!(!f.root.join("versions/test/mods").exists());
    f.clean();
}
