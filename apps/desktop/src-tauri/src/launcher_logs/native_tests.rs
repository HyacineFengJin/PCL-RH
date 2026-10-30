//! The actual log provider runs on synthetic roots on every native host.
use super::*;
use std::fs;

struct Fixture {
    temporary: tempfile::TempDir,
    root: PathBuf,
    exports: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let work = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|path| path.join("Cargo.lock").is_file() && path.join("apps/desktop").is_dir())
            .unwrap()
            .join("work/native-launcher-log-fixtures");
        fs::create_dir_all(&work).unwrap();
        let temporary = tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap();
        let base = Dir::absolute(temporary.path()).unwrap();
        let root = temporary.path().join("root");
        base.new_private_child("root")
            .unwrap()
            .private_child(".pcl-linux")
            .unwrap()
            .private_child("logs")
            .unwrap();
        let exports = temporary.path().join("exports");
        // Ordinary user-selected destinations need not already be private.
        fs::create_dir(&exports).unwrap();
        Self {
            temporary,
            root,
            exports,
        }
    }
    fn log(&self, name: &str) -> PathBuf {
        self.root.join(".pcl-linux/logs").join(name)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.log(name), bytes).unwrap();
    }
}
#[cfg(windows)]
#[path = "../../../../../crates/core/src/file_ops/junction_fixture.rs"]
mod junction_fixture;
fn directory_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    junction_fixture::create(target, link);
}

#[test]
fn native_list_read_tail_and_session_offset_use_actual_owned_file_streams() {
    let fixture = Fixture::new();
    let prefix = b"previous attempt\n";
    let bytes = b"previous attempt\nfirst secret\nlast line\n";
    fixture.write("启动.log", bytes);
    let rows = list(&fixture.root, Some(&fixture.log("启动.log"))).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "启动.log");
    assert_eq!(rows[0].bytes, bytes.len() as u64);
    assert!(rows[0].current);
    assert!(!read(&fixture.root, "启动.log", |text| text
        .replace("secret", "<redacted>"))
    .unwrap()
    .contains("secret"));
    assert!(read_tail(&fixture.root, "启动.log", 100, str::to_owned)
        .unwrap()
        .ends_with("last line"));
    let mut captured =
        FailureLogCapture::capture(&fixture.root, Some(&fixture.log("启动.log"))).unwrap();
    let tail = captured.read_tail_from(Some(prefix.len() as u64)).unwrap();
    assert_eq!(tail.text, "first secret\nlast line");
    assert!(!tail.truncated);
    captured.verify().unwrap();
    assert!(!fixture.root.join(".pcl-linux/log-trash").exists());
}

#[test]
fn native_session_capture_rejects_same_bytes_under_a_replaced_file_identity() {
    let fixture = Fixture::new();
    fixture.write("failed.log", b"same bytes\n");
    let captured =
        FailureLogCapture::capture(&fixture.root, Some(&fixture.log("failed.log"))).unwrap();
    let old_revision = captured.binding_revision().unwrap();
    let logs = Dir::absolute(&fixture.root.join(".pcl-linux/logs")).unwrap();
    let base = Dir::absolute(fixture.temporary.path()).unwrap();
    pcl_core::file_ops::rename_noreplace_at(
        &logs.0,
        std::ffi::OsStr::new("failed.log"),
        &base.0,
        std::ffi::OsStr::new("saved.log"),
    )
    .unwrap();
    fixture.write("failed.log", b"same bytes\n");
    assert!(captured.verify().is_err());
    let fresh =
        FailureLogCapture::capture(&fixture.root, Some(&fixture.log("failed.log"))).unwrap();
    assert_ne!(fresh.binding_revision().unwrap(), old_revision);
    assert_eq!(
        fs::read(fixture.temporary.path().join("saved.log")).unwrap(),
        b"same bytes\n"
    );
}

#[test]
fn native_exports_redact_and_publish_text_and_zip_into_ordinary_destination_directories() {
    let fixture = Fixture::new();
    fixture.write("one.log", b"secret one\n");
    fixture.write("two.log", b"secret two\n");
    for (selection, name) in [
        (ExportSelection::Named("one.log".into()), "one.txt"),
        (ExportSelection::All, "all.zip"),
    ] {
        let plan = prepare_export(&fixture.root, &selection, None).unwrap();
        let destination = fixture.exports.join(name);
        execute_export(
            &fixture.root,
            &selection,
            &plan.revision,
            None,
            &destination,
            &AtomicBool::new(false),
            |text| text.replace("secret", "redacted"),
        )
        .unwrap();
        let bytes = fs::read(&destination).unwrap();
        if name.ends_with("zip") {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
            assert_eq!(archive.len(), 2);
            let mut text = String::new();
            archive
                .by_name("two.log")
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            assert_eq!(text, "redacted two\n");
        } else {
            assert_eq!(bytes, b"redacted one\n");
        }
    }
    assert_eq!(fs::read_dir(&fixture.exports).unwrap().count(), 2);
    assert_eq!(fs::read(fixture.log("one.log")).unwrap(), b"secret one\n");
}

#[test]
fn native_export_conflicts_and_midstream_cancel_preserve_target_and_named_evidence() {
    let fixture = Fixture::new();
    fixture.write("one.log", b"secret source\n");
    let selection = ExportSelection::Named("one.log".into());
    let plan = prepare_export(&fixture.root, &selection, None).unwrap();
    let target = fixture.exports.join("existing.txt");
    fs::write(&target, b"user original").unwrap();
    let conflict = execute_export(
        &fixture.root,
        &selection,
        &plan.revision,
        None,
        &target,
        &AtomicBool::new(false),
        |text| text.replace("secret", "redacted"),
    )
    .unwrap_err();
    assert!(conflict.contains("已经存在"), "{conflict}");
    assert_eq!(fs::read(&target).unwrap(), b"user original");
    let cancel = AtomicBool::new(false);
    let cancelled = fixture.exports.join("cancelled.txt");
    let failure = execute_export(
        &fixture.root,
        &selection,
        &plan.revision,
        None,
        &cancelled,
        &cancel,
        |text| {
            cancel.store(true, Ordering::Relaxed);
            text.replace("secret", "redacted")
        },
    )
    .unwrap_err();
    assert!(failure.contains("取消"), "{failure}");
    assert!(!cancelled.exists());
    assert_eq!(
        fs::read(fixture.log("one.log")).unwrap(),
        b"secret source\n"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(fs::read_dir(&fixture.exports).unwrap().count(), 1);
    #[cfg(not(target_os = "linux"))]
    {
        assert!(conflict.contains("已保留"));
        assert!(failure.contains("已保留"));
        assert!(fs::read_dir(&fixture.exports).unwrap().count() >= 3);
    }
}

#[test]
fn native_clear_and_restore_keep_current_and_use_durable_no_replace_history() {
    let fixture = Fixture::new();
    fixture.write("history.log", b"restore these bytes\n");
    fixture.write("current.log", b"running session\n");
    let current = fixture.log("current.log");
    let plan = prepare_clear(&fixture.root, Some(&current)).unwrap();
    assert_eq!(plan.names, vec!["history.log"]);
    execute_clear(
        &fixture.root,
        &plan.revision,
        Some(&current),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!fixture.log("history.log").exists());
    assert_eq!(fs::read(&current).unwrap(), b"running session\n");
    let rows = list_recovery(&fixture.root).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].warnings.is_empty(), "{:?}", rows[0].warnings);
    restore(
        &fixture.root,
        &rows[0].operation_id,
        &rows[0].revision,
        Some(&current),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        fs::read(fixture.log("history.log")).unwrap(),
        b"restore these bytes\n"
    );
    assert!(list_recovery(&fixture.root).unwrap().is_empty());
}

#[test]
fn native_interrupted_named_stage_is_retained_and_visible_as_incomplete_recovery() {
    let fixture = Fixture::new();
    let owner = Dir::absolute(&fixture.root)
        .unwrap()
        .child(".pcl-linux")
        .unwrap();
    let store = owner.private_child("log-trash").unwrap();
    let operation = store.new_private_child("interrupted").unwrap();
    let mut output = operation.temporary().unwrap();
    output
        .file_mut()
        .write_all(b"incomplete private bytes")
        .unwrap();
    let error = output.failure("synthetic interruption".into());
    assert!(error.contains("synthetic interruption"));
    #[cfg(not(target_os = "linux"))]
    {
        assert!(error.contains("已保留"));
        let names = operation.names().unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(
            fs::read(
                fixture
                    .root
                    .join(".pcl-linux/log-trash/interrupted")
                    .join(&names[0])
            )
            .unwrap(),
            b"incomplete private bytes"
        );
    }
    let rows = list_recovery(&fixture.root).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].warnings.is_empty());
    assert!(fixture
        .root
        .join(".pcl-linux/log-trash/interrupted")
        .is_dir());
}

#[test]
fn native_reads_reject_links_and_multilink_files_without_touching_referents() {
    let fixture = Fixture::new();
    fixture.write("original.log", b"untouched");
    fs::hard_link(fixture.log("original.log"), fixture.log("alias.log")).unwrap();
    assert!(read(&fixture.root, "original.log", str::to_owned).is_err());
    assert!(read(&fixture.root, "alias.log", str::to_owned).is_err());
    let outside = fixture.temporary.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("retained"), b"outside").unwrap();
    directory_link(&outside, &fixture.log("linked.log"));
    let logs = Dir::absolute(&fixture.root.join(".pcl-linux/logs")).unwrap();
    assert!(logs.file("linked.log").is_err());
    assert!(logs.optional_child("linked.log").is_err());
    assert_eq!(fs::read(outside.join("retained")).unwrap(), b"outside");
    assert_eq!(fs::read(fixture.log("original.log")).unwrap(), b"untouched");
}

#[test]
fn native_directory_inventory_has_a_real_entry_limit() {
    let fixture = Fixture::new();
    let path = fixture.root.join(".pcl-linux/logs");
    for index in 0..=MAX_ENTRIES {
        fs::write(path.join(format!("entry-{index}")), b"").unwrap();
    }
    assert!(Dir::absolute(&path).unwrap().names().is_err());
    assert_eq!(fs::read_dir(&path).unwrap().count(), MAX_ENTRIES + 1);
}

#[test]
fn native_log_stamp_wire_retains_unix_shape_and_windows_full_identity() {
    #[cfg(unix)]
    {
        let old = r#"{"device":42,"inode":7,"mode":33152,"owner":1000,"links":1,"bytes":5,"modified":[1,2],"changed":[3,4]}"#;
        let stamp: Stamp = serde_json::from_str(old).unwrap();
        assert_eq!(serde_json::to_string(&stamp).unwrap(), old);
    }
    let fixture = Fixture::new();
    fixture.write("identity.log", b"bytes");
    let file = Dir::absolute(&fixture.root.join(".pcl-linux/logs"))
        .unwrap()
        .file("identity.log")
        .unwrap()
        .unwrap();
    let stamp = Stamp::from_file(&file).unwrap();
    assert!(stamp.owned() && stamp.regular());
    #[cfg(windows)]
    {
        let json = serde_json::to_value(&stamp).unwrap();
        assert_eq!(json["format"], "windows-file-id-128-v1");
        assert_eq!(json["file_id"].as_array().unwrap().len(), 16);
        assert!(json.get("device").is_none() && json.get("inode").is_none());
        assert!(serde_json::from_str::<Stamp>(r#"{"device":42,"inode":7,"mode":33152,"owner":1000,"links":1,"bytes":5,"modified":[1,2],"changed":[3,4]}"#).is_err());
    }
}

#[test]
fn native_oversized_recovery_record_cannot_create_an_unreadable_commit_or_move_originals() {
    let fixture = Fixture::new();
    fixture.write("original.log", b"must remain in original location");
    let owner = Dir::absolute(&fixture.root)
        .unwrap()
        .child(".pcl-linux")
        .unwrap();
    let operation = owner
        .private_child("log-trash")
        .unwrap()
        .new_private_child("oversized")
        .unwrap();
    assert!(operation
        .write_new("journal.json", &vec![b'x'; MAX_JOURNAL_BYTES as usize + 1])
        .is_err());
    assert!(operation.names().unwrap().is_empty());
    assert_eq!(
        fs::read(fixture.log("original.log")).unwrap(),
        b"must remain in original location"
    );
}
