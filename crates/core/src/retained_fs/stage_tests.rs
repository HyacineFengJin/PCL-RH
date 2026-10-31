use super::*;
use crate::retained_fs::Traversal;
use std::{fs, io::Write, path::Path};

fn fixture() -> (tempfile::TempDir, Directory) {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/retained-stage-tests");
    fs::create_dir_all(&work).unwrap();
    let temporary = tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap();
    let root = Directory::open_absolute(temporary.path()).unwrap();
    let private = root
        .create_private(&Leaf::new("private").unwrap(), Traversal::SameFilesystem)
        .unwrap();
    (temporary, private)
}
fn leaf(name: &str) -> Leaf {
    Leaf::new(name).unwrap()
}
fn sealed(parent: &Directory, bytes: &[u8]) -> Sealed {
    let mut stage = Stage::new(parent).unwrap();
    stage.file_mut().write_all(bytes).unwrap();
    stage.seal(1024 * 1024).unwrap()
}
fn published(outcome: Result<PublishOutcome, PublishError>) -> Receipt {
    match outcome.unwrap() {
        PublishOutcome::Published { receipt } => receipt,
        PublishOutcome::Exists { .. } => panic!("Unexpected existing target"),
        PublishOutcome::MayHavePublished { error, receipt, .. } => {
            panic!("Publication requires reconciliation: {error}; {receipt:?}")
        }
    }
}

#[test]
fn explicit_discard_confirms_only_its_new_source_and_preserves_other_files() {
    let (_fixture, parent) = fixture();
    let mut original = parent.create_file(&leaf("business-original")).unwrap();
    original.write_all(b"original must remain").unwrap();
    original.sync_all().unwrap();
    drop(original);
    let stage = sealed(&parent, b"discarded input bytes");
    let expected = stage.evidence().clone();
    let expected_metadata = stage.metadata().clone();
    match stage.discard().unwrap() {
        DiscardOutcome::Discarded { receipt } => {
            assert_eq!(receipt.source, expected);
            assert_eq!(receipt.metadata, expected_metadata);
            assert!(receipt.discard_confirmed && receipt.directory_synced);
            assert_eq!(receipt.source_residue, SourceResidue::None);
        }
        DiscardOutcome::MayHaveDiscarded { error, receipt, .. } => {
            panic!("unexpected discard uncertainty: {error}; {receipt:?}")
        }
    }
    if let Some(name) = expected.source_name {
        assert!(parent.entry(&Leaf::new(name).unwrap()).unwrap().is_none());
    }
    let mut original = parent
        .open_regular(&leaf("business-original"), Links::Single)
        .unwrap();
    let mut bytes = Vec::new();
    original.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original must remain");
}

#[test]
fn discard_rechecks_dirty_original_and_returns_its_owned_stage() {
    let (_fixture, parent) = fixture();
    let mut stage = Stage::new(&parent).unwrap();
    stage
        .file_mut()
        .write_all(b"sealed original bytes")
        .unwrap();
    let mut outsider = stage.file().try_clone().unwrap();
    let stage = stage.seal(4096).unwrap();
    outsider.seek(SeekFrom::Start(0)).unwrap();
    outsider.write_all(b"changed sealed bytes!").unwrap();
    outsider.sync_all().unwrap();
    drop(outsider);
    let error = match stage.discard() {
        Err(error) => error,
        Ok(_) => panic!("dirty sealed bytes were discarded"),
    };
    assert!(error.error.to_string().contains("changed"));
    let name = error.stage.stage.source_name.clone();
    assert_eq!(
        metadata(&error.stage.stage.file).unwrap().identity,
        error.stage.evidence.identity
    );
    if let Some(name) = name {
        assert!(parent.entry(&name).unwrap().is_some());
    }
}

#[test]
fn failed_native_discard_retains_original_and_reports_uncertainty() {
    let (_fixture, parent) = fixture();
    let stage = sealed(&parent, b"retained after native refusal");
    let expected = stage.evidence().clone();
    match stage
        .discard_using(native::sync_parent, |_, _| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected native delete refusal",
            ))
        })
        .unwrap()
    {
        DiscardOutcome::MayHaveDiscarded {
            stage: Some(stage),
            receipt,
            error,
        } => {
            assert!(!receipt.discard_confirmed);
            assert!(!receipt.directory_synced);
            assert_eq!(receipt.source, expected);
            assert_eq!(metadata(&stage.stage.file).unwrap(), *stage.metadata());
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        _ => panic!("native delete refusal lost its owned source or claimed completion"),
    }
}

#[test]
fn failed_post_discard_sync_is_may_have_discarded_without_recreating_source() {
    let (_fixture, parent) = fixture();
    let stage = sealed(&parent, b"removed before injected sync failure");
    let evidence = stage.evidence().clone();
    match stage
        .discard_with(|_| Err(io::Error::other("injected discard sync failure")))
        .unwrap()
    {
        DiscardOutcome::MayHaveDiscarded {
            stage: None,
            receipt,
            error,
        } => {
            assert_eq!(receipt.source, evidence);
            assert!(!receipt.discard_confirmed);
            assert!(!receipt.directory_synced);
            assert!(error.to_string().contains("sync failure"));
        }
        _ => panic!("post-delete sync failure was reported as certain or source recreated"),
    }
    if let Some(name) = evidence.source_name {
        assert!(parent.entry(&Leaf::new(name).unwrap()).unwrap().is_none());
    }
}

#[test]
fn new_stage_is_private_and_platform_anonymity_is_truthful() {
    let (_fixture, parent) = fixture();
    let mut stage = Stage::new(&parent).unwrap();
    metadata(stage.file_mut())
        .unwrap()
        .require_private()
        .unwrap();
    #[cfg(target_os = "linux")]
    {
        assert!(stage.source_name().is_none());
        assert!(parent.names(10).unwrap().is_empty());
        assert_eq!(metadata(stage.file_mut()).unwrap().links, 0);
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        let name = stage.source_name().unwrap().as_os_str().to_string_lossy();
        assert!(name.starts_with(".pcl-stage-") && name.ends_with(".lease"));
        assert_eq!(name.len(), ".pcl-stage-".len() + 32 + ".lease".len());
        assert_eq!(parent.names(10).unwrap().len(), 1);
    }
    let name = stage.source_name().cloned();
    drop(stage);
    // Named leases deliberately remain recoverable on Drop, with no blind
    // unlink. Anonymous Linux staging is reclaimed by the kernel on close.
    if let Some(name) = name {
        assert!(parent.entry(&name).unwrap().is_some());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn anonymous_staging_preserves_repair_in_ordinary_0755_dependency_directories() {
    use std::os::unix::fs::PermissionsExt;
    let (fixture, private) = fixture();
    let parent_path = fixture.path().join("libraries");
    fs::create_dir(&parent_path).unwrap();
    fs::set_permissions(&parent_path, fs::Permissions::from_mode(0o755)).unwrap();
    let parent = Directory::open_absolute(&parent_path).unwrap();
    assert!(parent.metadata().unwrap().require_private().is_err());
    let mut stage = Stage::new(&parent).unwrap();
    assert!(stage.source_name().is_none());
    let file_metadata = metadata(stage.file_mut()).unwrap();
    file_metadata.require_private().unwrap();
    assert_eq!(file_metadata.links, 0);
    stage.file_mut().write_all(b"new dependency bytes").unwrap();
    let sealed = stage.seal(1024).unwrap();
    let receipt = published(sealed.publish_new(&parent, &leaf("library.jar")));
    assert_eq!(receipt.source.source_kind, SourceKind::AnonymousFd);
    assert_eq!(
        fs::read(parent_path.join("library.jar")).unwrap(),
        b"new dependency bytes"
    );
    assert_eq!(
        fs::metadata(&parent_path).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(private.names(10).unwrap().is_empty());
}

#[test]
fn sealing_enforces_size_bound_and_preserves_failed_stage_ownership() {
    let (_fixture, parent) = fixture();
    let mut stage = Stage::new(&parent).unwrap();
    stage.file_mut().write_all(b"too many bytes").unwrap();
    let error = match stage.seal(3) {
        Ok(_) => panic!("Oversize accepted"),
        Err(error) => error,
    };
    let sealed = error.stage.seal(32).unwrap();
    assert_eq!(sealed.evidence().len, 14);
    assert_eq!(
        sealed.evidence().sha256,
        <[u8; 32]>::from(Sha256::digest(b"too many bytes"))
    );
    assert!(parent.entry(&leaf("target")).unwrap().is_none());
}

#[test]
fn publish_new_verifies_bytes_and_complete_receipt_without_named_residue() {
    let (fixture, parent) = fixture();
    let stage = sealed(&parent, b"verified new bytes");
    let evidence = stage.evidence().clone();
    let receipt = published(stage.publish_new(&parent, &leaf("published 实例")));
    assert_eq!(receipt.source, evidence);
    assert_eq!(receipt.target_parent, parent.identity().unwrap());
    assert_eq!(receipt.observed_target, Some(evidence.identity));
    assert!(receipt.publication_confirmed && receipt.directories_synced);
    assert_eq!(receipt.source_residue, SourceResidue::None);
    assert_eq!(
        fs::read(fixture.path().join("private/published 实例")).unwrap(),
        b"verified new bytes"
    );
    assert_eq!(
        parent.names(10).unwrap(),
        vec![OsString::from("published 实例")]
    );
    assert_eq!(
        serde_json::from_slice::<Receipt>(&serde_json::to_vec(&receipt).unwrap()).unwrap(),
        receipt
    );
}

#[test]
fn existing_files_directories_and_links_are_preserved_and_stage_can_choose_new_target() {
    let (fixture, parent) = fixture();
    let path = fixture.path().join("private");
    fs::write(path.join("file"), b"original").unwrap();
    fs::create_dir(path.join("directory")).unwrap();
    for name in ["file", "directory"] {
        let stage = sealed(&parent, b"new");
        let stage = match stage.publish_new(&parent, &leaf(name)).unwrap() {
            PublishOutcome::Exists { stage } => stage,
            _ => panic!("Existing target was not reported"),
        };
        assert_eq!(fs::read(path.join("file")).unwrap(), b"original");
        published(stage.publish_new(&parent, &leaf(&format!("new-{name}"))));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("missing", path.join("link")).unwrap();
        assert!(matches!(
            sealed(&parent, b"new")
                .publish_new(&parent, &leaf("link"))
                .unwrap(),
            PublishOutcome::Exists { .. }
        ));
        assert_eq!(
            fs::read_link(path.join("link")).unwrap(),
            Path::new("missing")
        );
    }
}

#[test]
fn mutation_through_a_retained_writer_is_rejected_before_commit() {
    let (_fixture, parent) = fixture();
    let mut stage = Stage::new(&parent).unwrap();
    stage.file_mut().write_all(b"first").unwrap();
    let mut duplicate = stage.file_mut().try_clone().unwrap();
    let stage = stage.seal(1024).unwrap();
    duplicate.seek(SeekFrom::Start(0)).unwrap();
    duplicate.write_all(b"other").unwrap();
    let result = stage.publish_new(&parent, &leaf("target"));
    assert!(result.is_err());
    assert!(parent.entry(&leaf("target")).unwrap().is_none());
}

#[test]
fn post_commit_flush_failure_is_uncertain_and_does_not_delete_published_bytes() {
    let (fixture, parent) = fixture();
    let stage = sealed(&parent, b"published before flush error");
    let outcome = stage
        .publish_with(&parent, &leaf("target"), |_| {
            Err(io::Error::other("fixture flush failure"))
        })
        .unwrap();
    let (stage, receipt) = match outcome {
        PublishOutcome::MayHavePublished {
            stage,
            receipt,
            error,
        } => {
            assert_eq!(error.to_string(), "fixture flush failure");
            (stage, receipt)
        }
        _ => panic!("A post-commit failure was incorrectly reported as definite success/failure"),
    };
    assert!(receipt.publication_confirmed && !receipt.directories_synced);
    assert_eq!(
        fs::read(fixture.path().join("private/target")).unwrap(),
        b"published before flush error"
    );
    assert!(stage.publish_new(&parent, &leaf("another-target")).is_err());
    assert!(parent.entry(&leaf("another-target")).unwrap().is_none());
}

#[test]
fn post_commit_content_change_is_detected_after_directory_flush() {
    let (_fixture, parent) = fixture();
    let mut stage = Stage::new(&parent).unwrap();
    stage.file_mut().write_all(b"first").unwrap();
    let mut duplicate = stage.file_mut().try_clone().unwrap();
    let stage = stage.seal(1024).unwrap();
    let outcome = stage
        .publish_with(&parent, &leaf("target"), |_| {
            duplicate.seek(SeekFrom::Start(0))?;
            duplicate.write_all(b"other")?;
            native::sync_parent(&parent)
        })
        .unwrap();
    match outcome {
        PublishOutcome::MayHavePublished { receipt, .. } => assert!(!receipt.publication_confirmed),
        _ => panic!("Post-commit mutation escaped the final check"),
    }
    assert!(parent.entry(&leaf("target")).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn post_commit_ctime_change_is_not_authorized_by_restored_bytes_or_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (_fixture, parent) = fixture();
    let mut stage = Stage::new(&parent).unwrap();
    stage.file_mut().write_all(b"unchanged bytes").unwrap();
    let duplicate = stage.file_mut().try_clone().unwrap();
    let stage = stage.seal(1024).unwrap();
    let outcome = stage
        .publish_with(&parent, &leaf("target"), |_| {
            // Same mode and identical bytes: only native ctime changes. A fresh
            // metadata revision after the commit check still invalidates the CAS.
            duplicate.set_permissions(fs::Permissions::from_mode(0o600))?;
            native::sync_parent(&parent)
        })
        .unwrap();
    match outcome {
        PublishOutcome::MayHavePublished { receipt, .. } => assert!(!receipt.publication_confirmed),
        _ => panic!("Post-commit metadata revision escaped the final check"),
    }
}

#[test]
fn concurrent_no_replace_publication_has_one_winner() {
    use std::sync::{Arc, Barrier};
    let (fixture, parent) = fixture();
    let stages = [sealed(&parent, b"first"), sealed(&parent, b"other")];
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = stages
        .into_iter()
        .map(|stage| {
            let parent = parent.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                stage.publish_new(&parent, &leaf("winner"))
            })
        })
        .collect();
    let mut wins = 0;
    let mut collisions = 0;
    for worker in workers {
        match worker.join().unwrap().unwrap() {
            PublishOutcome::Published { .. } => wins += 1,
            PublishOutcome::Exists { .. } => collisions += 1,
            PublishOutcome::MayHavePublished { error, .. } => {
                panic!("Unexpected uncertainty: {error}")
            }
        }
    }
    assert_eq!((wins, collisions), (1, 1));
    assert!([b"first".as_slice(), b"other".as_slice()].contains(
        &fs::read(fixture.path().join("private/winner"))
            .unwrap()
            .as_slice()
    ));
}

#[test]
fn retained_source_and_target_parents_do_not_follow_replacement_paths() {
    let (fixture, root) = fixture();
    let source = root
        .create_private(&leaf("source"), Traversal::SameFilesystem)
        .unwrap();
    let target = root
        .create_private(&leaf("target"), Traversal::SameFilesystem)
        .unwrap();
    #[cfg(not(windows))]
    let stage = sealed(&source, b"held directories");
    let base = fixture.path().join("private");
    fs::rename(base.join("source"), base.join("source-held")).unwrap();
    fs::create_dir(base.join("source")).unwrap();
    // Windows correctly refuses to rename a directory containing a child that
    // denies delete sharing. Capture/rename the source first, then create the
    // stage through its held directory; never weaken its exclusive file lease.
    #[cfg(windows)]
    let stage = sealed(&source, b"held directories");
    fs::rename(base.join("target"), base.join("target-held")).unwrap();
    fs::create_dir(base.join("target")).unwrap();
    published(stage.publish_new(&target, &leaf("published")));
    assert_eq!(
        fs::read(base.join("target-held/published")).unwrap(),
        b"held directories"
    );
    assert!(!base.join("target/published").exists());
    assert!(fs::read_dir(base.join("source")).unwrap().next().is_none());
}

#[cfg(windows)]
#[test]
fn exclusive_stage_retains_windows_source_parent_until_publication() {
    let (fixture, parent) = fixture();
    let source = parent
        .create_private(&leaf("source"), Traversal::SameFilesystem)
        .unwrap();
    let target = parent
        .create_private(&leaf("target"), Traversal::SameFilesystem)
        .unwrap();
    let stage = sealed(&source, b"held child denies delete sharing");
    let error = fs::rename(
        fixture.path().join("private/source"),
        fixture.path().join("private/moved"),
    )
    .unwrap_err();
    assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
    published(stage.publish_new(&target, &leaf("published")));
    fs::rename(
        fixture.path().join("private/source"),
        fixture.path().join("private/moved"),
    )
    .unwrap();
    assert_eq!(
        fs::read(fixture.path().join("private/target/published")).unwrap(),
        b"held child denies delete sharing"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn substituted_named_source_is_preserved_and_rejected_before_commit() {
    let (fixture, parent) = fixture();
    let stage = sealed(&parent, b"original sealed bytes");
    let name = stage.source_name().unwrap().as_os_str().to_owned();
    let base = fixture.path().join("private");
    fs::rename(base.join(&name), base.join("retained-source")).unwrap();
    fs::write(base.join(&name), b"replacement must survive").unwrap();
    assert!(stage.publish_new(&parent, &leaf("target")).is_err());
    assert_eq!(
        fs::read(base.join(&name)).unwrap(),
        b"replacement must survive"
    );
    assert_eq!(
        fs::read(base.join("retained-source")).unwrap(),
        b"original sealed bytes"
    );
    assert!(!base.join("target").exists());
}

#[cfg(windows)]
#[test]
fn windows_named_stage_denies_other_writers_and_delete_renames() {
    let (fixture, parent) = fixture();
    let stage = sealed(&parent, b"exclusive held stage");
    let path = fixture
        .path()
        .join("private")
        .join(stage.source_name().unwrap().as_os_str());
    assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
    assert!(fs::rename(&path, path.with_extension("replacement")).is_err());
    assert!(fs::remove_file(&path).is_err());
    published(stage.publish_new(&parent, &leaf("target")));
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn publication_and_receipts_preserve_native_non_unicode_leaf_names() {
    let (_fixture, parent) = fixture();
    #[cfg(target_os = "linux")]
    let name = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(b"native-\xff".to_vec())
    };
    #[cfg(windows)]
    let name = {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[110, 45, 0xd800])
    };
    let leaf = Leaf::new(&name).unwrap();
    let receipt = published(sealed(&parent, b"native name").publish_new(&parent, &leaf));
    assert_eq!(receipt.target_name, name);
    assert!(parent.names(10).unwrap().contains(&name));
    assert_eq!(
        serde_json::from_slice::<Receipt>(&serde_json::to_vec(&receipt).unwrap()).unwrap(),
        receipt
    );
}
