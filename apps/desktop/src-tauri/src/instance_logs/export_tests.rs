//! The chooser is represented by the gap between prepare and publish. All
//! sources and destinations belong to this test's disposable synthetic tree.
use super::*;
use crate::instance_logs::export;

fn prepare(
    f: &LogsFixture,
    listing: &LogListing,
    content: &LogContent,
) -> Result<export::PreparedExport, String> {
    let row = listing
        .entries
        .iter()
        .find(|row| row.kind == content.kind && row.name == content.name)
        .unwrap();
    export::prepare(
        &f.shared,
        &f.root.id,
        "Example",
        row.kind,
        &row.name,
        &row.revision,
        &listing.revision,
        &content.content_revision,
    )
}
fn output(f: &LogsFixture) -> PathBuf {
    f.fixture.0.join("exported.excerpt.txt")
}

#[test]
fn export_plain_and_gzip_are_exactly_the_loaded_redacted_bounded_text() {
    for compressed in [false, true] {
        let f = LogsFixture::new();
        let name = if compressed {
            "old.log.gz"
        } else {
            "latest.log"
        };
        let mut raw = b"raw-only-prefix\n".to_vec();
        raw.extend_from_slice("tail fixture\n".repeat(30_000).as_bytes());
        raw.extend_from_slice(b"secret-account-value\nlast line\n");
        let source = f.write(
            LogKind::Game,
            name,
            if compressed { gzip(&raw) } else { raw.clone() },
        );
        let original = fs::read(&source).unwrap();
        let listing = f.list();
        let row = &listing.entries[0];
        let redact = |text: &str| text.replace("secret-account-value", "[REDACTED]");
        let content = read_with(
            &f.shared,
            &f.root.id,
            "Example",
            row.kind,
            &row.name,
            &row.revision,
            &listing.revision,
            |selected| selected.read(&f.shared.closing),
            redact,
        )
        .unwrap();
        assert!(content.truncated);
        assert!(content.text.len() <= reader::MAX_TEXT_BYTES);
        assert!(content.text.ends_with("[REDACTED]\nlast line\n"));
        assert!(!content.text.contains("raw-only-prefix"));
        assert!(!content.text.contains("secret-account-value"));
        assert_eq!(content.content_revision.len(), 64);
        let prepared = export::prepare_with(
            &f.shared,
            &f.root.id,
            "Example",
            row.kind,
            &row.name,
            &row.revision,
            &listing.revision,
            &content.content_revision,
            |text| {
                assert!(f.shared.operations.try_lock().is_ok());
                redact(text)
            },
        )
        .unwrap();
        assert_eq!(
            prepared.suggested_name(),
            if compressed {
                "old.excerpt.txt"
            } else {
                "latest.excerpt.txt"
            }
        );
        let outcome = prepared.publish(&f.shared, &output(&f)).unwrap();
        assert_eq!(outcome.status, "complete");
        assert_eq!(outcome.count, 1);
        assert_eq!(fs::read(output(&f)).unwrap(), content.text.as_bytes());
        assert_eq!(fs::read(source).unwrap(), original);
    }
}

#[test]
fn content_receipt_binds_the_exact_text_truncation_listing_and_row() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "latest.log", b"evidence\n");
    let mut listing = f.list();
    let mut content = f.read(&listing, &listing.entries[0]).unwrap();
    let valid = content.content_revision.clone();
    for forged in [String::new(), "0".repeat(64), "x".repeat(64)] {
        content.content_revision = forged;
        assert!(prepare(&f, &listing, &content).is_err());
    }
    content.content_revision = valid;
    let row = &listing.entries[0];
    for receipt in [
        digest(&(
            "instance-log-content-v1",
            &listing.revision,
            &row.revision,
            "Example",
            row.kind,
            &row.name,
            "other evidence\n",
            content.truncated,
        ))
        .unwrap(),
        digest(&(
            "instance-log-content-v1",
            &listing.revision,
            &row.revision,
            "Example",
            row.kind,
            &row.name,
            &content.text,
            !content.truncated,
        ))
        .unwrap(),
    ] {
        assert!(export::prepare(
            &f.shared,
            &f.root.id,
            "Example",
            row.kind,
            &row.name,
            &row.revision,
            &listing.revision,
            &receipt,
        )
        .is_err());
    }
    assert!(export::prepare_with(
        &f.shared,
        &f.root.id,
        "Example",
        row.kind,
        &row.name,
        &row.revision,
        &listing.revision,
        &content.content_revision,
        |_| "now redacted differently".into(),
    )
    .is_err());
    listing.entries[0].revision = "0".repeat(64);
    assert!(prepare(&f, &listing, &content).is_err());
    listing.revision = "0".repeat(64);
    assert!(prepare(&f, &listing, &content).is_err());
    assert!(!output(&f).exists());
}

#[test]
fn source_or_admission_changes_after_prepare_refuse_anonymous_publication() {
    for action in [
        "file",
        "removed",
        "json",
        "scope",
        "root",
        "settings",
        "root-selection",
        "closing",
        "generation",
        "task-ended",
        "recovery",
    ] {
        let f = LogsFixture::new();
        let second = f.second();
        let source = f.write(LogKind::Game, "latest.log", b"loaded evidence\n");
        let listing = f.list();
        let content = f.read(&listing, &listing.entries[0]).unwrap();
        let prepared = prepare(&f, &listing, &content).unwrap();
        let target = output(&f);
        let result = prepared.publish_with(&f.shared, &target, || {
            assert!(f.shared.operations.try_lock().is_ok());
            match action {
                "file" => fs::write(&source, b"modified evidence\n").unwrap(),
                "removed" => fs::remove_file(&source).unwrap(),
                "json" => fs::write(f.instance().join("Example.json"), b"{\"changed\":1}").unwrap(),
                "scope" => fs::write(f.instance().join("options.txt"), b"music:0.5").unwrap(),
                "root" => {
                    fs::rename(f.base(), f.base().with_file_name("retained-root")).unwrap();
                    fs::create_dir(f.base()).unwrap();
                }
                "settings" => {
                    f.shared
                        .config
                        .select_installed(&f.root.id, "Example")
                        .unwrap();
                }
                "root-selection" => {
                    f.shared.config.select(&second.id).unwrap();
                }
                "closing" => f.shared.closing.store(true, Ordering::SeqCst),
                "generation" => {
                    f.shared.close_generation.fetch_add(1, Ordering::SeqCst);
                }
                "task-ended" => {
                    let task = f
                        .shared
                        .tasks
                        .admit(
                            crate::tasks::TaskTarget {
                                root_id: f.root.id.clone(),
                                root_path: f.root.path.clone(),
                                instance_id: Some("Example".into()),
                            },
                            crate::tasks::TaskKind::ResourceOperation,
                        )
                        .unwrap();
                    task.finish(crate::tasks::TaskOutcome::Complete {
                        result: None,
                        message: "fixture complete".into(),
                        error: None,
                    });
                }
                _ => {
                    fs::create_dir_all(f.base().join(".pcl-linux/instance-resets/unknown")).unwrap()
                }
            }
        });
        assert!(matches!(result, Err(error) if error == CHANGED), "{action}");
        assert!(!target.exists(), "{action}");
    }
}

#[test]
fn replacing_a_source_root_ancestor_with_a_symlink_cannot_authorize_export() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "latest.log", b"original evidence");
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    let prepared = prepare(&f, &listing, &content).unwrap();
    let ancestor = f.base().parent().unwrap().to_owned();
    let retained = ancestor.with_file_name("retained-Minecraft");
    let result = prepared.publish_with(&f.shared, &output(&f), || {
        fs::rename(&ancestor, &retained).unwrap();
        std::os::unix::fs::symlink(&retained, &ancestor).unwrap();
    });
    fs::remove_file(&ancestor).unwrap();
    fs::rename(&retained, &ancestor).unwrap();
    assert!(matches!(result, Err(error) if error == CHANGED));
    assert!(!output(&f).exists());
}

#[test]
fn existing_targets_symlinks_and_non_text_extensions_never_change() {
    for action in ["existing", "symlink", "extension"] {
        let f = LogsFixture::new();
        f.write(LogKind::Game, "latest.log", b"evidence");
        let listing = f.list();
        let content = f.read(&listing, &listing.entries[0]).unwrap();
        let prepared = prepare(&f, &listing, &content).unwrap();
        let mut target = output(&f);
        let kept = f.fixture.0.join("keep.txt");
        fs::write(&kept, b"preserved").unwrap();
        match action {
            "existing" => fs::write(&target, b"preserved").unwrap(),
            "symlink" => std::os::unix::fs::symlink(&kept, &target).unwrap(),
            _ => {
                target.set_extension("log");
            }
        }
        let before = tree(&f.fixture.0);
        assert!(prepared.publish(&f.shared, &target).is_err());
        assert_eq!(tree(&f.fixture.0), before);
        assert_eq!(fs::read(kept).unwrap(), b"preserved");
    }
}

#[test]
fn replaced_destination_parent_cannot_receive_the_export() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "latest.log", b"evidence");
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    let prepared = prepare(&f, &listing, &content).unwrap();
    let parent = f.fixture.0.join("chosen");
    let retained = f.fixture.0.join("retained-chosen");
    fs::create_dir(&parent).unwrap();
    let result = prepared.publish_with(&f.shared, &parent.join("export.txt"), || {
        fs::rename(&parent, &retained).unwrap();
        fs::create_dir(&parent).unwrap();
    });
    assert!(result.is_err());
    assert!(fs::read_dir(parent).unwrap().next().is_none());
    assert!(fs::read_dir(retained).unwrap().next().is_none());
}

#[test]
fn destinations_under_root_or_through_symlink_parents_are_rejected_before_staging() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "latest.log", b"evidence");
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    let alias = f.fixture.0.join("root-alias");
    std::os::unix::fs::symlink(f.base(), &alias).unwrap();
    for parent in [f.base(), f.instance(), f.base().join("logs"), alias] {
        let prepared = prepare(&f, &listing, &content).unwrap();
        let before = tree(&f.base());
        assert!(prepared
            .publish(&f.shared, &parent.join("export.txt"))
            .is_err());
        assert_eq!(tree(&f.base()), before);
    }
}

#[test]
fn moving_the_pinned_destination_inside_root_before_publication_is_rejected() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "latest.log", b"evidence");
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    let prepared = prepare(&f, &listing, &content).unwrap();
    let parent = f.fixture.0.join("chosen");
    let inside = f.base().join("moved-chosen");
    fs::create_dir(&parent).unwrap();
    let result = prepared.publish_with(&f.shared, &parent.join("export.txt"), || {
        fs::rename(&parent, &inside).unwrap();
        std::os::unix::fs::symlink(&inside, &parent).unwrap();
    });
    assert!(result.is_err());
    assert!(fs::read_dir(inside).unwrap().next().is_none());
}

#[test]
fn empty_text_refuses_export_and_cancelled_preparation_leaves_no_output() {
    for bytes in [b"".as_slice(), b"readable evidence"] {
        let f = LogsFixture::new();
        f.write(LogKind::Crash, "crash-report.txt", bytes);
        let listing = f.list();
        let content = f.read(&listing, &listing.entries[0]).unwrap();
        let before = tree(&f.fixture.0);
        let prepared = prepare(&f, &listing, &content);
        if bytes.is_empty() {
            assert!(prepared.err().unwrap().contains("为空"));
        } else {
            drop(prepared.unwrap());
            let outcome = crate::toolbox_images::publish::ExportOutcome::cancelled();
            assert_eq!(outcome.status, "cancelled");
            assert_eq!(outcome.count, 0);
            assert!(outcome.path.is_none() && outcome.warning.is_none());
        }
        assert_eq!(tree(&f.fixture.0), before);
    }
}
