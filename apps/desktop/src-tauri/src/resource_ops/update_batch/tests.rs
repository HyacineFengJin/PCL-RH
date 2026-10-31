use super::*;
use sha2::{Digest, Sha512};
use std::fs;

#[test]
fn final_update_source_recheck_maps_only_an_open_download_cancellation_window() {
    let fixture = Fixture::new(false, true);
    let mut files = vec![fixture.file("mods", "fixture.jar", b"actual archive")];
    let tokens = validate_sources(&mut files, &[], &AtomicBool::new(false)).unwrap();
    let cancel = AtomicBool::new(true);
    assert_eq!(
        sources_unchanged(&files, &tokens, Some(&cancel)).unwrap_err(),
        CANCELLED
    );
    sources_unchanged(&files, &tokens, None).unwrap();
    files[0].file.set_len(files[0].size - 1).unwrap();
    let conflict = sources_unchanged(&files, &tokens, None).unwrap_err();
    assert!(conflict.contains("描述符已经变化"), "{conflict}");
    assert_ne!(conflict, CANCELLED);
}

#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(windows)]
use std::os::windows::fs::symlink_dir as symlink;

pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    pub(crate) source: PathBuf,
    _temporary: tempfile::TempDir,
}
impl Fixture {
    pub(crate) fn new(isolated: bool, dirs: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path();
        let root = base.join("root");
        let source = base.join("source");
        fs::create_dir_all(root.join("versions/test")).unwrap();
        pcl_core::retained_fs::Directory::open_absolute(&base.canonicalize().unwrap())
            .unwrap()
            .create_private(
                &pcl_core::retained_fs::Leaf::new("source").unwrap(),
                pcl_core::retained_fs::Traversal::SameFilesystem,
            )
            .unwrap();
        fs::write(
            root.join("versions/test/test.json"),
            br#"{"id":"test","libraries":[]}"#,
        )
        .unwrap();
        if isolated {
            fs::create_dir_all(root.join("versions/test/config")).unwrap();
        }
        if dirs {
            for k in KINDS {
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
            _temporary: temporary,
        }
    }
    fn folder(&self, kind: &str) -> PathBuf {
        crate::ui_data::resource_dir(&self.root, "test", kind).unwrap()
    }
    fn archive(payload: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("payload.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(payload).unwrap();
        writer.finish().unwrap().into_inner()
    }
    pub(crate) fn file(&self, kind: &str, name: &str, payload: &[u8]) -> VerifiedImport {
        let bytes = Self::archive(payload);
        let size = bytes.len() as u64;
        let mut file = Dir::open(&self.source)
            .unwrap()
            .stage_for("verified-download-input", size)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        VerifiedImport {
            kind: kind.into(),
            file_name: name.into(),
            size: bytes.len() as u64,
            sha512: format!("{:x}", Sha512::digest(&bytes)),
            file,
        }
    }
    pub(crate) fn old(&self, kind: &str, name: &str, payload: &[u8]) -> Replacement {
        fs::create_dir_all(self.folder(kind)).unwrap();
        // Existing user resources are not download sources. Build their exact
        // ZIP bytes directly; only real incoming files allocate source Tickets.
        let bytes = Self::archive(payload);
        let old_sha512 = format!("{:x}", Sha512::digest(&bytes));
        let path = self.folder(kind).join(name);
        fs::write(&path, bytes).unwrap();
        Replacement {
            kind: kind.into(),
            old_file_name: name.into(),
            old_fingerprint: super::super::fingerprint(&path).unwrap(),
            old_sha512,
            new_file_name: format!("next-{name}"),
        }
    }
    fn replacement(&self, name: &str) -> Replacement {
        let path = self.folder("mods").join(name);
        Replacement {
            kind: "mods".into(),
            old_file_name: name.into(),
            old_fingerprint: super::super::fingerprint(&path).unwrap(),
            old_sha512: format!("{:x}", Sha512::digest(fs::read(path).unwrap())),
            new_file_name: format!("next-{name}"),
        }
    }
    pub(crate) fn run(
        &self,
        files: &mut [VerifiedImport],
        replacements: &[Replacement],
    ) -> MutationResult {
        update_verified_batch(
            &self.root,
            "test",
            files,
            replacements,
            &AtomicBool::new(false),
            &mut || Ok(()),
            &mut |_, _| {},
        )
        .unwrap()
    }
    pub(crate) fn clean(&self) {
        super::super::ensure_ready(&self.root).unwrap();
        assert!(scope::marker_store(&self.root)
            .unwrap()
            .unwrap()
            .names()
            .unwrap()
            .is_empty());
    }
}

fn prepared(
    f: &Fixture,
    files: &mut [VerifiedImport],
    replacements: &[Replacement],
) -> (Context, Journal) {
    let cancel = AtomicBool::new(false);
    let tokens = validate_sources(files, replacements, &cancel).unwrap();
    let (context, mut j) = scope::prepare(&f.root, "test", files, replacements, &cancel).unwrap();
    stage(&context, &mut j, files, &tokens, &cancel, &mut |_, _| {}).unwrap();
    j.after = scope::expected_after(&j).unwrap();
    j.state = State::Prepared;
    journal::write(&context, &j).unwrap();
    (context, j)
}
fn applying(context: &Context, j: &mut Journal) {
    j.state = State::Applying;
    journal::write(context, j).unwrap();
    scope::create_targets(context, j).unwrap();
}
fn move_one(context: &Context, j: &Journal, index: usize) {
    let item = &j.items[index];
    let dir = context.resources(j, &item.kind, true).unwrap().unwrap();
    let op = context.operation.as_ref().unwrap();
    if let Some(old) = &item.old {
        dir.move_directory(&old.name, op, &slot("old", index))
            .unwrap();
    }
    op.move_directory(&slot("new", index), &dir, &item.name)
        .unwrap();
}
fn undo_partial(context: &Context, j: &mut Journal, count: usize) {
    journal::create_marker(context, j).unwrap();
    j.state = State::UndoApplying;
    journal::write(context, j).unwrap();
    for index in 0..count {
        let item = &j.items[index];
        let dir = context.resources(j, &item.kind, true).unwrap().unwrap();
        dir.move_directory(
            &item.name,
            context.operation.as_ref().unwrap(),
            &slot("new", index),
        )
        .unwrap();
    }
}

#[test]
fn mixed_updates_keep_disabled_and_original_inodes() {
    for isolated in [false, true] {
        let f = Fixture::new(isolated, true);
        let mut r = f.old("mods", "old.jar.disabled", b"old");
        r.new_file_name = "new.jar.disabled".into();
        let old_inode = identity(f.folder("mods").join("old.jar.disabled"));
        let mut files = vec![
            f.file("mods", "new.jar.disabled", b"new"),
            f.file("resourcepacks", "new.zip", b"pack"),
            f.file("shaderpacks", "new.zip", b"shader"),
        ];
        let admitted = std::cell::Cell::new(false);
        let mut published = Vec::new();
        let result = update_verified_batch(
            &f.root,
            "test",
            &mut files,
            &[r],
            &AtomicBool::new(false),
            &mut || {
                admitted.set(true);
                Ok(())
            },
            &mut |done, total| {
                if admitted.get() {
                    published.push((done, total));
                }
            },
        )
        .unwrap();
        assert_eq!(published.len(), files.len());
        assert!(published.windows(2).all(|pair| pair[0].0 < pair[1].0));
        let last = published.last().unwrap();
        assert_eq!(last.0, last.1);
        assert!(published.first().unwrap().0 < last.1);
        let id = result.undo_id.unwrap();
        assert_eq!(result.changed, 3);
        for input in &files {
            assert_eq!(links(f.folder(&input.kind).join(&input.file_name)), 1);
        }
        let history = updates_history(&f.root, "test").unwrap();
        assert_eq!(history[0].id, id);
        assert!(history[0].files[0].contains("old.jar.disabled"));
        restore_update(
            &f.root,
            "test",
            &id,
            &AtomicBool::new(false),
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(
            identity(f.folder("mods").join("old.jar.disabled")),
            old_inode
        );
        assert!(updates_history(&f.root, "test").unwrap().is_empty());
        f.clean();
    }
}
#[test]
fn preparation_copies_without_fingerprint_or_directory_changes() {
    let f = Fixture::new(true, false);
    let r = f.old("mods", "old.jar", b"old");
    let old_path = f.folder("mods").join("old.jar");
    let fingerprint = super::super::fingerprint(&old_path).unwrap();
    let mut files = vec![
        f.file("mods", &r.new_file_name, b"new"),
        f.file("resourcepacks", "added.zip", b"added"),
    ];
    let (c, j) = prepared(&f, &mut files, &[r]);
    assert_eq!(super::super::fingerprint(&old_path).unwrap(), fingerprint);
    assert!(!f.folder("resourcepacks").exists());
    assert!(!f.folder("shaderpacks").exists());
    recovery::one(&c, &j).unwrap();
    f.clean();
}
#[test]
fn cancel_before_commit_cleans_without_touching_original() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let cancel = AtomicBool::new(false);
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let token = r.old_fingerprint.clone();
    let mut commits = 0;
    let result = update_verified_batch(
        &f.root,
        "test",
        &mut files,
        &[r],
        &cancel,
        &mut || {
            commits += 1;
            Ok(())
        },
        &mut |_, _| cancel.store(true, Ordering::Release),
    );
    assert_eq!(result.unwrap_err(), CANCELLED);
    assert_eq!(commits, 0);
    assert_eq!(
        super::super::fingerprint(&f.folder("mods").join("old.jar")).unwrap(),
        token
    );
    f.clean();
}
#[test]
fn commit_rejection_keeps_its_error_after_late_cancel() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let cancel = AtomicBool::new(false);
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let result = update_verified_batch(
        &f.root,
        "test",
        &mut files,
        &[r],
        &cancel,
        &mut || {
            cancel.store(true, Ordering::Release);
            Err("plan changed".into())
        },
        &mut |_, _| {},
    );
    assert_eq!(result.unwrap_err(), "plan changed");
    f.clean();
}
#[test]
fn applying_crash_rolls_back_original_inode() {
    let f = Fixture::new(false, true);
    let a = f.old("mods", "a.jar", b"a");
    let b = f.old("mods", "b.jar", b"b");
    let inode = identity(f.folder("mods").join("a.jar"));
    let mut files = vec![
        f.file("mods", &a.new_file_name, b"a2"),
        f.file("mods", &b.new_file_name, b"b2"),
    ];
    let (c, mut j) = prepared(&f, &mut files, &[a, b]);
    applying(&c, &mut j);
    move_one(&c, &j, 0);
    assert!(ensure_ready(&f.root).is_err());
    recover_root(&f.root).unwrap();
    assert_eq!(identity(f.folder("mods").join("a.jar")), inode);
    assert!(f.folder("mods").join("b.jar").exists());
    f.clean();
}
#[test]
fn rollback_conflict_retains_record_and_retries() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let (c, mut j) = prepared(&f, &mut files, &[r]);
    applying(&c, &mut j);
    move_one(&c, &j, 0);
    fs::write(f.folder("mods").join("old.jar"), b"external").unwrap();
    assert!(recover_root(&f.root).is_err());
    assert!(c
        .operation
        .as_ref()
        .unwrap()
        .stat("old-0000")
        .unwrap()
        .is_some());
    assert_eq!(
        fs::read(f.folder("mods").join("old.jar")).unwrap(),
        b"external"
    );
    fs::remove_file(f.folder("mods").join("old.jar")).unwrap();
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn committed_write_error_keeps_new_and_old_for_recovery() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    journal::FAIL_COMMITTED_WRITE.with(|v| v.set(true));
    let result = update_verified_batch(
        &f.root,
        "test",
        &mut files,
        &[r],
        &AtomicBool::new(false),
        &mut || Ok(()),
        &mut |_, _| {},
    );
    assert!(result.unwrap_err().contains("同步失败"));
    assert!(f.folder("mods").join("next-old.jar").exists());
    assert!(!f.folder("mods").join("old.jar").exists());
    assert!(ensure_ready(&f.root).is_err());
    recover_root(&f.root).unwrap();
    let id = updates_history(&f.root, "test").unwrap()[0].id.clone();
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    f.clean();
}
#[test]
fn undo_applying_partial_rolls_forward_to_committed() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![
        f.file("mods", &r.new_file_name, b"new"),
        f.file("resourcepacks", "added.zip", b"pack"),
    ];
    let id = f.run(&mut files, &[r]).undo_id.unwrap();
    let c = Context::existing(&f.root, "test", &id).unwrap();
    let mut j = journal::read(&c).unwrap();
    undo_partial(&c, &mut j, 1);
    recover_root(&f.root).unwrap();
    assert!(f.folder("mods").join("next-old.jar").exists());
    assert!(!f.folder("mods").join("old.jar").exists());
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    f.clean();
}
#[test]
fn undo_conflict_preserves_external_and_retry_succeeds() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let id = f.run(&mut files, &[r]).undo_id.unwrap();
    let c = Context::existing(&f.root, "test", &id).unwrap();
    let mut j = journal::read(&c).unwrap();
    undo_partial(&c, &mut j, 1);
    fs::write(f.folder("mods").join("next-old.jar"), b"external").unwrap();
    assert!(recover_root(&f.root).is_err());
    assert_eq!(
        fs::read(f.folder("mods").join("next-old.jar")).unwrap(),
        b"external"
    );
    fs::remove_file(f.folder("mods").join("next-old.jar")).unwrap();
    recover_root(&f.root).unwrap();
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    f.clean();
}
#[test]
fn undo_missing_target_dirs_keeps_binding_until_durable_restored() {
    let f = Fixture::new(true, false);
    let mut files = vec![f.file("resourcepacks", "added.zip", b"pack")];
    let id = f.run(&mut files, &[]).undo_id.unwrap();
    let c = Context::existing(&f.root, "test", &id).unwrap();
    let mut j = journal::read(&c).unwrap();
    undo_partial(&c, &mut j, 1);
    assert!(f.folder("resourcepacks").exists());
    recover_root(&f.root).unwrap();
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    assert!(!f.folder("resourcepacks").exists());
    f.clean();
}
#[test]
fn root_marker_detects_external_instance_move_even_missing_json() {
    let f = Fixture::new(true, true);
    let mut files = vec![f.file("mods", "new.jar", b"new")];
    let (c, j) = prepared(&f, &mut files, &[]);
    let instance = f.root.join("versions/test");
    let moved = f.source.join("moved");
    let marker = f
        .root
        .join(".pcl-linux")
        .join(MARKERS)
        .join(format!("{}.json", j.id));
    let marker_bytes = fs::read(&marker).unwrap();
    fs::remove_file(f.root.join("versions/test/test.json")).unwrap();
    #[cfg(windows)]
    {
        // A live Context retains descendant directory capabilities. Windows
        // refuses moving their ancestor; this is not a crashed-owner fixture.
        let blocked = fs::rename(&instance, &moved)
            .expect_err("live update capabilities must block the ancestor move");
        assert!(
            matches!(blocked.raw_os_error(), Some(5 | 32)),
            "unexpected live-capability move failure: {blocked}"
        );
        assert!(instance.is_dir());
        assert!(!moved.exists());
        c.check(&j).unwrap();
        assert_eq!(c.instance.key().unwrap(), j.scope.instance);
        assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
    }
    // Simulate losing the old owner before an external move. Recovery below
    // must reopen the durable marker/history, with no old Context helping it.
    drop(c);
    fs::rename(&instance, &moved).unwrap();
    assert!(super::super::ensure_ready(&f.root).is_err());
    assert!(recover_root(&f.root).is_err());
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
    assert!(moved.is_dir());
    assert!(!instance.exists());
    fs::rename(&moved, &instance).unwrap();
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn namespace_replacement_is_retained() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let (c, j) = prepared(&f, &mut files, &[r]);
    fs::rename(f.folder("mods"), f.source.join("moved-mods")).unwrap();
    fs::create_dir(f.folder("mods")).unwrap();
    assert!(recovery::one(&c, &j).is_err());
    assert!(c
        .operation
        .as_ref()
        .unwrap()
        .stat("new-0000")
        .unwrap()
        .is_some());
    fs::remove_dir(f.folder("mods")).unwrap();
    fs::rename(f.source.join("moved-mods"), f.folder("mods")).unwrap();
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn shared_inventory_change_at_commit_is_rejected() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let result = update_verified_batch(
        &f.root,
        "test",
        &mut files,
        &[r],
        &AtomicBool::new(false),
        &mut || {
            fs::write(f.folder("shaderpacks").join("external.zip"), b"external").unwrap();
            Ok(())
        },
        &mut |_, _| {},
    );
    assert!(result.unwrap_err().contains("清单"));
    assert!(f.folder("mods").join("old.jar").exists());
    assert_eq!(
        fs::read(f.folder("shaderpacks").join("external.zip")).unwrap(),
        b"external"
    );
    f.clean();
}
#[test]
fn unknown_private_file_blocks_guard_and_safe_recovery() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let id = f.run(&mut files, &[r]).undo_id.unwrap();
    let path = f
        .root
        .join("versions/test")
        .join(HISTORY)
        .join(&id)
        .join("retain.txt");
    fs::write(&path, b"retain").unwrap();
    assert!(super::super::ensure_ready(&f.root).is_err());
    assert!(restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(())
    )
    .is_err());
    assert!(recover_root(&f.root).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"retain");
    fs::remove_file(path).unwrap();
    f.clean();
}
#[test]
fn consecutive_updates_restore_original_inode_chain() {
    let f = Fixture::new(true, true);
    let a = f.old("mods", "old.jar", b"old");
    let inode = identity(f.folder("mods").join("old.jar"));
    let mut first = vec![f.file("mods", &a.new_file_name, b"first")];
    let first_id = f.run(&mut first, &[a]).undo_id.unwrap();
    let b = f.replacement("next-old.jar");
    let mut second = vec![f.file("mods", &b.new_file_name, b"second")];
    let second_id = f.run(&mut second, &[b]).undo_id.unwrap();
    assert!(restore_update(
        &f.root,
        "test",
        &first_id,
        &AtomicBool::new(false),
        &mut || Ok(())
    )
    .is_err());
    restore_update(
        &f.root,
        "test",
        &second_id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    restore_update(
        &f.root,
        "test",
        &first_id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(identity(f.folder("mods").join("old.jar")), inode);
    f.clean();
}
#[test]
fn rename_and_delete_restore_carry_history_with_instance_inode() {
    let f = Fixture::new(true, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let id = f.run(&mut files, &[r]).undo_id.unwrap();
    fs::rename(
        f.root.join("versions/test"),
        f.root.join("versions/renamed"),
    )
    .unwrap();
    fs::rename(
        f.root.join("versions/renamed/test.json"),
        f.root.join("versions/renamed/renamed.json"),
    )
    .unwrap();
    assert_eq!(updates_history(&f.root, "renamed").unwrap()[0].id, id);
    fs::rename(f.root.join("versions/renamed"), f.source.join("deleted")).unwrap();
    super::super::ensure_ready(&f.root).unwrap();
    fs::rename(f.source.join("deleted"), f.root.join("versions/renamed")).unwrap();
    restore_update(
        &f.root,
        "renamed",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    assert!(f.root.join("versions/renamed/mods/old.jar").exists());
    f.clean();
}
#[test]
fn symlinked_resource_directory_is_rejected_without_following() {
    let f = Fixture::new(false, false);
    symlink(&f.source, f.root.join("mods")).unwrap();
    let mut files = vec![f.file("mods", "new.jar", b"new")];
    assert!(update_verified_batch(
        &f.root,
        "test",
        &mut files,
        &[],
        &AtomicBool::new(false),
        &mut || Ok(()),
        &mut |_, _| {}
    )
    .is_err());
    #[cfg(target_os = "linux")]
    assert!(fs::read_dir(&f.source).unwrap().next().is_none());
    #[cfg(not(target_os = "linux"))]
    {
        // The caller still owns its registered Input after rejected admission.
        // No file was published through the alias; only the owned registry and
        // its source exist. Retire that source and prove no pending payloads.
        files[0].file.verify_owned().unwrap();
        assert!(!f.source.join("new.jar").exists());
        super::super::retire_sources(&mut files).unwrap();
        let private =
            pcl_core::retained_fs::Directory::open_absolute(&f.source.join(".pcl-linux")).unwrap();
        let area = crate::modrinth_install::source::open_area(
            private.file(),
            "resource-batch-sources",
            MAX_BATCH_BYTES * 2,
        )
        .unwrap();
        assert_eq!(area.pending().unwrap(), 0);
        let names = pcl_core::retained_fs::Directory::open_absolute(&f.source)
            .unwrap()
            .names(4)
            .unwrap();
        assert_eq!(names, [std::ffi::OsString::from(".pcl-linux")]);
    }
}
#[test]
fn ownership_receipts_recover_staging_before_next_full_manifest() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let cancel = AtomicBool::new(false);
    let tokens = validate_sources(&mut files, &[r.clone()], &cancel).unwrap();
    let (c, mut j) = scope::prepare(&f.root, "test", &files, &[r], &cancel).unwrap();
    stage(&c, &mut j, &mut files, &tokens, &cancel, &mut |_, _| {}).unwrap();
    let disk = journal::read(&c).unwrap();
    assert_eq!(disk.state, State::Staging);
    assert!(disk.items[0].new.is_some());
    assert!(disk.items[0].old.as_ref().unwrap().backup.is_some());
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn root_marker_receipt_recovers_undo_before_full_manifest() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let id = f.run(&mut files, &[r]).undo_id.unwrap();
    let c = Context::existing(&f.root, "test", &id).unwrap();
    let mut j = journal::read(&c).unwrap();
    journal::create_marker(&c, &mut j).unwrap();
    let disk = journal::read(&c).unwrap();
    assert_eq!(disk.state, State::Committed);
    assert_eq!(
        disk.marker.as_ref().unwrap().key,
        j.marker.as_ref().unwrap().key
    );
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn partially_completed_apply_rollback_retries() {
    let f = Fixture::new(false, true);
    let a = f.old("mods", "a.jar", b"a");
    let b = f.old("mods", "b.jar", b"b");
    let mut files = vec![
        f.file("mods", &a.new_file_name, b"a2"),
        f.file("mods", &b.new_file_name, b"b2"),
    ];
    let (c, mut j) = prepared(&f, &mut files, &[a, b]);
    applying(&c, &mut j);
    move_one(&c, &j, 0);
    move_one(&c, &j, 1);
    let dir = c.resources(&j, "mods", true).unwrap().unwrap();
    let op = c.operation.as_ref().unwrap();
    dir.move_directory(&j.items[0].name, op, "new-0000")
        .unwrap();
    op.move_directory("old-0000", &dir, &j.items[0].old.as_ref().unwrap().name)
        .unwrap();
    recover_root(&f.root).unwrap();
    assert!(f.folder("mods").join("a.jar").exists());
    assert!(f.folder("mods").join("b.jar").exists());
    f.clean();
}
#[test]
fn partially_completed_undo_rollback_retries() {
    let f = Fixture::new(false, true);
    let a = f.old("mods", "a.jar", b"a");
    let b = f.old("mods", "b.jar", b"b");
    let mut files = vec![
        f.file("mods", &a.new_file_name, b"a2"),
        f.file("mods", &b.new_file_name, b"b2"),
    ];
    let id = f.run(&mut files, &[a, b]).undo_id.unwrap();
    let c = Context::existing(&f.root, "test", &id).unwrap();
    let mut j = journal::read(&c).unwrap();
    undo_partial(&c, &mut j, 2);
    let op = c.operation.as_ref().unwrap();
    let dir = c.resources(&j, "mods", true).unwrap().unwrap();
    op.move_directory("old-0000", &dir, &j.items[0].old.as_ref().unwrap().name)
        .unwrap();
    op.move_directory("old-0001", &dir, &j.items[1].old.as_ref().unwrap().name)
        .unwrap();
    dir.move_directory(&j.items[0].old.as_ref().unwrap().name, op, "old-0000")
        .unwrap();
    op.move_directory("new-0000", &dir, &j.items[0].name)
        .unwrap();
    recover_root(&f.root).unwrap();
    assert!(f.folder("mods").join("next-a.jar").exists());
    assert!(f.folder("mods").join("next-b.jar").exists());
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    f.clean();
}
#[test]
fn interrupted_journal_temp_retries_without_overwrite() {
    let f = Fixture::new(false, true);
    let mut files = vec![f.file("mods", "new.jar", b"new")];
    let (c, j) = prepared(&f, &mut files, &[]);
    let op = c.operation.as_ref().unwrap();
    let bytes = serde_json::to_vec(&j).unwrap();
    {
        // Model an interrupted private writer, not CopyFile's inherited owner
        // or the Windows logical document's physical base fence.
        let directory = pcl_core::retained_fs::Directory::open_absolute(
            &f.root.join("versions/test").join(HISTORY).join(&j.id),
        )
        .unwrap();
        assert_eq!(
            filesystem::key_file(directory.file()).unwrap(),
            op.key().unwrap()
        );
        let mut next = directory
            .create_file(&pcl_core::retained_fs::Leaf::new("journal.next").unwrap())
            .unwrap();
        next.write_all(&bytes).unwrap();
        next.sync_all().unwrap();
        pcl_core::retained_fs::metadata(&next)
            .unwrap()
            .require_private()
            .unwrap();
        directory.sync().unwrap();
    }
    assert!(op.stat("journal.next").unwrap().is_some());
    assert_eq!(
        fs::read(
            f.root
                .join("versions/test")
                .join(HISTORY)
                .join(&j.id)
                .join("journal.next")
        )
        .unwrap(),
        bytes
    );
    journal::write(&c, &j).unwrap();
    assert_eq!(journal::read(&c).unwrap().id, j.id);
    recover_root(&f.root).unwrap();
    f.clean();
}
#[test]
fn undo_external_original_slot_conflict_retains_history() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let id = f.run(&mut files, &[r]).undo_id.unwrap();
    let c = Context::existing(&f.root, "test", &id).unwrap();
    let mut j = journal::read(&c).unwrap();
    undo_partial(&c, &mut j, 1);
    fs::write(f.folder("mods").join("old.jar"), b"external").unwrap();
    assert!(recover_root(&f.root).is_err());
    assert_eq!(
        fs::read(f.folder("mods").join("old.jar")).unwrap(),
        b"external"
    );
    assert!(c
        .operation
        .as_ref()
        .unwrap()
        .stat("old-0000")
        .unwrap()
        .is_some());
    fs::remove_file(f.folder("mods").join("old.jar")).unwrap();
    recover_root(&f.root).unwrap();
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    f.clean();
}

#[test]
fn maximum_replacements_have_bounded_phase_writes_and_readable_inventory() {
    let started = std::time::Instant::now();
    let f = Fixture::new(false, true);
    let mut replacements = Vec::new();
    for index in 0..MAX_FILES {
        replacements.push(f.old("mods", &format!("old-{index:04}.jar"), b"old"));
    }
    let bytes = Fixture::archive(b"new");
    let size = bytes.len() as u64;
    let sha512 = format!("{:x}", Sha512::digest(&bytes));
    let prepared = super::super::verified_batch::prepare_sources(
        replacements.iter().map(|r| preflight::SourceSpec {
            kind: "mods",
            name: &r.new_file_name,
            size,
            sha512: &sha512,
        }),
        &AtomicBool::new(false),
    )
    .unwrap();
    let source_dir = Dir::open(&f.source).unwrap();
    let mut inputs = preflight::AllocationPermit::allocate_batch_with(
        prepared.into_permits().collect(),
        |admitted| {
            let requests = admitted
                .iter()
                .map(|(_, source)| ("verified-download-input".to_string(), source.size))
                .collect::<Vec<_>>();
            source_dir.stage_batch_for(&requests, &AtomicBool::new(false))
        },
    )
    .unwrap();
    for file in inputs.iter_mut() {
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
    }
    let mut files: Vec<_> = replacements
        .iter()
        .zip(inputs.into_inputs())
        .map(|(r, file)| VerifiedImport {
            kind: "mods".into(),
            file_name: r.new_file_name.clone(),
            size,
            sha512: sha512.clone(),
            file,
        })
        .collect();
    assert_eq!(files.len(), MAX_FILES);
    eprintln!(
        "max512 phase=setup count={} elapsed_ms={}",
        files.len(),
        started.elapsed().as_millis()
    );
    let report = |phase: &str, event: &str, phase_started: std::time::Instant| {
        eprintln!(
            "max512 phase={phase} event={event} count={MAX_FILES} elapsed_ms={} phase_ms={}",
            started.elapsed().as_millis(),
            phase_started.elapsed().as_millis()
        );
    };
    journal::FULL_WRITES.with(|n| n.set(0));
    let publishing = std::time::Instant::now();
    report("publish", "begin", publishing);
    let id = f.run(&mut files, &replacements).undo_id.unwrap();
    report("publish", "end", publishing);
    // The UI retires downloaded sources immediately after publication. Keep
    // the same real ownership order instead of deferring 512 Tickets to Drop.
    let retiring = std::time::Instant::now();
    report("retire_sources", "begin", retiring);
    super::super::retire_sources(&mut files).unwrap();
    report("retire_sources", "end", retiring);
    assert_eq!(journal::FULL_WRITES.with(|n| n.get()), 4);
    assert_eq!(
        updates_history(&f.root, "test").unwrap()[0].files.len(),
        MAX_FILES
    );
    f.clean();
    let restoring = std::time::Instant::now();
    report("restore", "begin", restoring);
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    report("restore", "end", restoring);
    f.clean();
}
#[test]
fn size_and_history_limits_refuse_without_dropping_backups() {
    let f = Fixture::new(false, true);
    let mut huge = f.file("mods", "huge.jar", b"new");
    huge.size = MAX_FILE_BYTES + 1;
    assert!(update_verified_batch(
        &f.root,
        "test",
        &mut [huge],
        &[],
        &AtomicBool::new(false),
        &mut || Ok(()),
        &mut |_, _| {}
    )
    .unwrap_err()
    .contains("大小"));
    let history = f.root.join("versions/test").join(HISTORY);
    fs::create_dir(&history).unwrap();
    for _ in 0..MAX_HISTORIES {
        fs::create_dir(history.join(new_id())).unwrap();
    }
    let input = f.file("mods", "new.jar", b"new");
    let result = scope::prepare(&f.root, "test", &[input], &[], &AtomicBool::new(false));
    assert!(result.err().unwrap().contains("256"));
    assert_eq!(fs::read_dir(history).unwrap().count(), MAX_HISTORIES);
    assert!(!f.folder("mods").join("new.jar").exists());
}
#[test]
fn late_cancel_after_first_move_keeps_the_committed_batch() {
    let f = Fixture::new(false, true);
    let a = f.old("mods", "a.jar", b"a");
    let b = f.old("mods", "b.jar", b"b");
    let mut files = vec![
        f.file("mods", &a.new_file_name, b"a2"),
        f.file("mods", &b.new_file_name, b"b2"),
    ];
    let cancel = AtomicBool::new(false);
    let admitted = std::cell::Cell::new(false);
    let result = update_verified_batch(
        &f.root,
        "test",
        &mut files,
        &[a, b],
        &cancel,
        &mut || {
            admitted.set(true);
            Ok(())
        },
        &mut |_, _| {
            if admitted.get() {
                cancel.store(true, Ordering::Release);
            }
        },
    )
    .unwrap();
    assert_eq!(result.changed, 2);
    assert!(f.folder("mods").join("next-a.jar").exists());
    assert!(f.folder("mods").join("next-b.jar").exists());
    f.clean();
}
#[test]
fn cancellation_winning_at_the_commit_gate_preserves_old_inode() {
    let f = Fixture::new(false, true);
    let r = f.old("mods", "old.jar", b"old");
    let inode = identity(f.folder("mods").join("old.jar"));
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let cancel = AtomicBool::new(false);
    let result = update_verified_batch(
        &f.root,
        "test",
        &mut files,
        &[r],
        &cancel,
        &mut || {
            cancel.store(true, Ordering::Release);
            Ok(())
        },
        &mut |_, _| {},
    );
    assert_eq!(result.unwrap_err(), CANCELLED);
    assert_eq!(identity(f.folder("mods").join("old.jar")), inode);
    f.clean();
}
#[test]
fn update_pending_blocks_legacy_import_before_creating_missing_resource_dirs() {
    let f = Fixture::new(true, false);
    let mut files = vec![f.file("mods", "new.jar", b"new")];
    let (_c, _j) = prepared(&f, &mut files, &[]);
    let result = super::super::set_enabled(
        &f.root,
        "test",
        "mods",
        &[super::super::ResourceFile {
            file_name: "old.jar".into(),
            fingerprint: "invalid".into(),
        }],
        false,
        &AtomicBool::new(false),
        &mut || Ok(()),
        &mut |_, _| {},
    );
    assert!(result.is_err());
    assert!(!f.folder("mods").exists());
    assert!(super::super::ensure_verified_batches_ready(&f.root).is_err());
    super::super::recover_verified_batches(&f.root).unwrap();
    f.clean();
}

#[test]
fn explicit_shared_update_and_undo_keep_journal_paths_despite_automatic_isolated_markers() {
    let f = Fixture::new(true, false);
    let input = f.file("mods", "added.jar", b"shared explicit content");
    let result = update_verified_batch_with_isolation(
        &f.root,
        "test",
        &mut [input],
        &[],
        &AtomicBool::new(false),
        &mut || Ok(()),
        &mut |_, _| {},
        IsolationPolicy::Shared,
    )
    .unwrap();
    assert!(f.root.join("mods/added.jar").is_file());
    assert!(!f.root.join("versions/test/mods/added.jar").exists());
    let undo = result.undo_id.unwrap();
    restore_update(&f.root, "test", &undo, &AtomicBool::new(false), &mut || {
        Ok(())
    })
    .unwrap();
    assert!(!f.root.join("mods/added.jar").exists());
    assert!(!f.root.join("versions/test/mods").exists());
    assert!(f.root.join("versions/test/config").is_dir());
    f.clean();
}

fn identity(path: impl AsRef<Path>) -> Key {
    filesystem::key_file(&File::open(path).unwrap()).unwrap()
}
fn links(path: impl AsRef<Path>) -> u64 {
    pcl_core::retained_fs::metadata(&File::open(path).unwrap())
        .unwrap()
        .links
}

#[cfg(windows)]
#[test]
fn restored_native_history_releases_current_files_and_retains_unexpected_private_names() {
    let f = Fixture::new(false, true);
    let replacement = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &replacement.new_file_name, b"new")];
    let id = f.run(&mut files, &[replacement]).undo_id.unwrap();
    super::super::retire_sources(&mut files).unwrap();
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    let context = Context::existing(&f.root, "test", &id).unwrap();
    let journal = journal::read(&context).unwrap();
    assert_eq!(journal.schema, 2);
    assert_eq!(journal.state, State::FinishedRestored);
    fs::write(f.folder("mods").join("old.jar"), b"later user edit").unwrap();
    ensure_ready(&f.root).unwrap();
    assert_eq!(recover_root(&f.root).unwrap().changed, 0);
    let unknown = f
        .root
        .join("versions/test")
        .join(HISTORY)
        .join(&id)
        .join("old-0000");
    fs::write(&unknown, b"unowned evidence").unwrap();
    assert!(ensure_ready(&f.root).is_err());
    assert!(recover_root(&f.root).is_err());
    assert_eq!(fs::read(&unknown).unwrap(), b"unowned evidence");
    fs::remove_file(unknown).unwrap();
    ensure_ready(&f.root).unwrap();
}

#[cfg(not(target_os = "linux"))]
#[test]
fn malformed_native_logical_documents_are_preserved_before_business_replacement() {
    for name in ["journal.json", "status.json", "owned-marker.json"] {
        let f = Fixture::new(false, true);
        let replacement = f.old("mods", "old.jar", b"old");
        let original = fs::read(f.folder("mods").join("old.jar")).unwrap();
        let mut files = vec![f.file("mods", &replacement.new_file_name, b"new")];
        let (context, journal) = prepared(&f, &mut files, &[replacement]);
        let operation = context.operation.as_ref().unwrap();
        let limit = if name == "journal.json" {
            MAX_JOURNAL_BYTES
        } else {
            16 * 1024
        };
        let state = operation.document_state(name, limit).unwrap();
        operation
            .compare_document(name, limit, &state, Some(b"{retained unknown bytes"))
            .unwrap();
        let result = match name {
            "journal.json" => journal::write(&context, &journal),
            "status.json" => journal::status(&context, &journal),
            _ => {
                let mut journal = journal.clone();
                journal::create_marker(&context, &mut journal)
            }
        };
        assert!(result.is_err(), "{name} unexpectedly overwritten");
        let mut state = operation.document_state(name, limit).unwrap();
        assert_eq!(
            operation
                .read_document(name, limit, &mut state)
                .unwrap()
                .unwrap(),
            b"{retained unknown bytes"
        );
        assert_eq!(
            fs::read(f.folder("mods").join("old.jar")).unwrap(),
            original
        );
    }
}
