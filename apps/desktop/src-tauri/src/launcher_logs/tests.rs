use super::history::{now, operation_id};
use super::*;

#[test]
fn live_log_tail_uses_line_limit_and_redacts_its_selected_suffix() {
    let f = Fixture::new();
    let content = (0..150)
        .map(|i| format!("line {i} token=fixture-secret\n"))
        .collect::<String>();
    f.write("live.log", &content);
    let tail = read_tail(&f.root(), "live.log", 100, |text| {
        text.replace("fixture-secret", "<redacted>")
    })
    .unwrap();
    assert_eq!(tail.lines().count(), 100);
    assert!(tail.starts_with("line 50 "));
    assert!(tail.contains("line 149 "));
    assert!(!tail.contains("fixture-secret"));
}

#[test]
fn live_log_tail_remains_bounded_after_log_exceeds_history_export_limit() {
    let f = Fixture::new();
    let mut file = fs::File::create(f.log("large.log")).unwrap();
    file.set_len(MAX_FILE_BYTES + 1024).unwrap();
    file.seek(SeekFrom::End(-10)).unwrap();
    file.write_all(b"\nlast row\n").unwrap();
    assert_eq!(
        read_tail(&f.root(), "large.log", 100, str::to_owned).unwrap(),
        "last row"
    );
    assert!(prepare_export(&f.root(), &ExportSelection::Named("large.log".into()), None).is_err());
}

#[test]
fn failure_log_tail_keeps_at_most_four_hundred_complete_lines() {
    let f = Fixture::new();
    let text = (0..450).map(|i| format!("line {i}\n")).collect::<String>();
    f.write("failed.log", &text);
    let mut captured = FailureLogCapture::capture(&f.root(), Some(&f.log("failed.log"))).unwrap();
    let tail = captured.read_tail().unwrap();
    assert!(tail.truncated);
    assert_eq!(tail.text.lines().count(), 400);
    assert!(tail.text.starts_with("line 50\n"));
    assert!(tail.text.ends_with("line 449"));
    captured.verify().unwrap();
}

#[test]
fn failure_log_tail_limits_bytes_and_discards_a_partial_or_giant_first_line() {
    let f = Fixture::new();
    for suffix in ["", "\nlast complete line\n"] {
        let mut file = fs::File::create(f.log("failed.log")).unwrap();
        file.write_all(&vec![b'x'; (FAILURE_TAIL_BYTES + 1024) as usize])
            .unwrap();
        file.write_all(suffix.as_bytes()).unwrap();
        drop(file);
        let mut captured =
            FailureLogCapture::capture(&f.root(), Some(&f.log("failed.log"))).unwrap();
        let tail = captured.read_tail().unwrap();
        assert!(tail.truncated);
        assert_eq!(
            tail.text,
            if suffix.is_empty() {
                ""
            } else {
                "last complete line"
            }
        );
        captured.verify().unwrap();
    }
}

#[test]
fn failure_log_tail_discards_split_utf8_and_marks_only_actual_truncation() {
    let f = Fixture::new();
    f.write("failed.log", &"完整行\n".repeat(30_000));
    let mut captured = FailureLogCapture::capture(&f.root(), Some(&f.log("failed.log"))).unwrap();
    let tail = captured.read_tail().unwrap();
    assert!(tail.truncated);
    assert_eq!(tail.text.lines().count(), 400);
    assert!(!tail.text.contains('\u{fffd}'));
    f.write("short.log", "one line\ntwo lines\n");
    let mut short = FailureLogCapture::capture(&f.root(), Some(&f.log("short.log"))).unwrap();
    let tail = short.read_tail().unwrap();
    assert!(!tail.truncated);
    assert_eq!(tail.text, "one line\ntwo lines");
}
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|path| {
                path.join("crates/core/Cargo.toml").is_file()
                    && path.join("apps/desktop/src-tauri/Cargo.toml").is_file()
            })
            .expect("test workspace root")
            .join("work/launcher-service-fixtures")
            .join(operation_id());
        fs::create_dir_all(path.join("root/.pcl-linux/logs")).unwrap();
        fs::create_dir_all(path.join("exports")).unwrap();
        Self(path)
    }
    fn root(&self) -> PathBuf {
        self.0.join("root")
    }
    fn log(&self, name: &str) -> PathBuf {
        self.root().join(".pcl-linux/logs").join(name)
    }
    fn out(&self, name: &str) -> PathBuf {
        self.0.join("exports").join(name)
    }
    fn write(&self, name: &str, data: &str) {
        fs::write(self.log(name), data).unwrap();
    }
    fn cleared(&self, current: Option<&Path>) -> ClearOutcome {
        let plan = prepare_clear(&self.root(), current).unwrap();
        execute_clear(
            &self.root(),
            &plan.revision,
            current,
            &AtomicBool::new(false),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn redact(text: &str) -> String {
    text.replace("fake-access-token", "[REDACTED]")
}

#[test]
fn previews_never_create_a_recovery_store_and_current_is_retained() {
    let f = Fixture::new();
    f.write("old.log", "old bytes");
    f.write("current.log", "current bytes");
    let current = f.log("current.log");
    let plan = prepare_clear(&f.root(), Some(&current)).unwrap();
    let listed = list(&f.root(), Some(&current)).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed.iter().filter(|row| row.current).count(), 1);
    assert_eq!(plan.names, vec!["old.log"]);
    assert_eq!(plan.retained_current.as_deref(), Some("current.log"));
    assert!(list_recovery(&f.root()).unwrap().is_empty());
    assert!(!f.root().join(".pcl-linux/log-trash").exists());
    assert_eq!(
        prepare_export(&f.root(), &ExportSelection::All, Some(&current))
            .unwrap()
            .file_count,
        2
    );
    assert!(!f.root().join(".pcl-linux/log-trash").exists());
    let outcome = execute_clear(
        &f.root(),
        &plan.revision,
        Some(&current),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(outcome.file_count, 1);
    assert_eq!(fs::read_to_string(current).unwrap(), "current bytes");
    assert!(!f.log("old.log").exists());
    let rows = list_recovery(&f.root()).unwrap();
    assert_eq!(rows[0].names, vec!["old.log"]);
    assert!(rows[0].warnings.is_empty());
    restore(
        &f.root(),
        &outcome.operation_id,
        &rows[0].revision,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(f.log("old.log")).unwrap(), "old bytes");
    assert!(list_recovery(&f.root()).unwrap().is_empty());
}

#[test]
fn stale_source_or_changed_current_refuses_before_mkdir() {
    let f = Fixture::new();
    f.write("one.log", "same");
    f.write("two.log", "other");
    let plan = prepare_clear(&f.root(), None).unwrap();
    assert!(execute_clear(
        &f.root(),
        &plan.revision,
        Some(&f.log("one.log")),
        &AtomicBool::new(false)
    )
    .is_err());
    f.write("one.log", "edit");
    assert!(execute_clear(&f.root(), &plan.revision, None, &AtomicBool::new(false)).is_err());
    assert!(!f.root().join(".pcl-linux/log-trash").exists());
    assert_eq!(fs::read_to_string(f.log("one.log")).unwrap(), "edit");
}

#[test]
fn text_and_zip_export_redact_real_source_bytes_and_never_overwrite() {
    let f = Fixture::new();
    f.write("one.log", "credential=fake-access-token\nmessage=ok\n");
    f.write("two.log", "another fake-access-token");
    let single = ExportSelection::Named("one.log".into());
    let plan = prepare_export(&f.root(), &single, None).unwrap();
    let out = f.out("one.txt");
    execute_export(
        &f.root(),
        &single,
        &plan.revision,
        None,
        &out,
        &AtomicBool::new(false),
        redact,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        "credential=[REDACTED]\nmessage=ok\n"
    );
    assert!(execute_export(
        &f.root(),
        &single,
        &plan.revision,
        None,
        &out,
        &AtomicBool::new(false),
        redact
    )
    .is_err());
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        "credential=[REDACTED]\nmessage=ok\n"
    );
    let plan = prepare_export(&f.root(), &ExportSelection::All, None).unwrap();
    let zip_path = f.out("all.zip");
    execute_export(
        &f.root(),
        &ExportSelection::All,
        &plan.revision,
        None,
        &zip_path,
        &AtomicBool::new(false),
        redact,
    )
    .unwrap();
    let mut archive = zip::ZipArchive::new(fs::File::open(zip_path).unwrap()).unwrap();
    assert_eq!(archive.len(), 2);
    use std::io::Read;
    for name in ["one.log", "two.log"] {
        let mut bytes = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut bytes)
            .unwrap();
        assert!(!bytes.contains("fake-access-token"));
        assert!(bytes.contains("[REDACTED]"));
    }
    assert!(fs::read_to_string(f.log("one.log"))
        .unwrap()
        .contains("fake-access-token"));
}

#[test]
fn export_detects_callback_time_source_mutation_and_removes_anonymous_stage() {
    let f = Fixture::new();
    f.write("one.log", "original");
    let selection = ExportSelection::Named("one.log".into());
    let plan = prepare_export(&f.root(), &selection, None).unwrap();
    let out = f.out("one.log");
    let result = execute_export(
        &f.root(),
        &selection,
        &plan.revision,
        None,
        &out,
        &AtomicBool::new(false),
        |text| {
            f.write("one.log", "changed by another writer");
            text.to_owned()
        },
    );
    assert!(result.is_err());
    assert!(!out.exists());
    assert_eq!(fs::read_dir(f.0.join("exports")).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(f.log("one.log")).unwrap(),
        "changed by another writer"
    );
}

#[test]
fn cancelled_export_never_publishes_and_cancelled_clear_is_read_only() {
    let f = Fixture::new();
    f.write("one.log", "bytes");
    let selection = ExportSelection::Named("one.log".into());
    let plan = prepare_export(&f.root(), &selection, None).unwrap();
    let cancel = AtomicBool::new(false);
    assert!(execute_export(
        &f.root(),
        &selection,
        &plan.revision,
        None,
        &f.out("one.log"),
        &cancel,
        |text| {
            cancel.store(true, Ordering::SeqCst);
            text.to_owned()
        }
    )
    .is_err());
    assert_eq!(fs::read_dir(f.0.join("exports")).unwrap().count(), 0);
    let clear = prepare_clear(&f.root(), None).unwrap();
    assert!(execute_clear(&f.root(), &clear.revision, None, &AtomicBool::new(true)).is_err());
    assert!(!f.root().join(".pcl-linux/log-trash").exists());
}

#[test]
fn restore_refuses_stale_revision_and_preserves_both_conflicting_files() {
    let f = Fixture::new();
    f.write("one.log", "archived");
    let cleared = f.cleared(None);
    let row = list_recovery(&f.root()).unwrap().remove(0);
    f.write("one.log", "replacement");
    assert!(restore(
        &f.root(),
        &cleared.operation_id,
        &row.revision,
        None,
        &AtomicBool::new(false)
    )
    .is_err());
    let fresh = list_recovery(&f.root()).unwrap().remove(0);
    assert!(!fresh.warnings.is_empty());
    assert!(restore(
        &f.root(),
        &cleared.operation_id,
        &fresh.revision,
        None,
        &AtomicBool::new(false)
    )
    .is_err());
    assert_eq!(fs::read_to_string(f.log("one.log")).unwrap(), "replacement");
    assert_eq!(
        fs::read_to_string(
            f.root()
                .join(".pcl-linux/log-trash")
                .join(&cleared.operation_id)
                .join("one.log")
        )
        .unwrap(),
        "archived"
    );
}

#[test]
fn immutable_journal_recovers_a_partial_clear_without_in_memory_state() {
    let f = Fixture::new();
    f.write("one.log", "first");
    f.write("two.log", "second");
    let scope = Scope::open(&f.root()).unwrap();
    let entries = scan(&scope, None, &AtomicBool::new(false)).unwrap();
    let store = scope
        .owner
        .as_ref()
        .unwrap()
        .private_child("log-trash")
        .unwrap();
    let id = operation_id();
    let operation = store.new_private_child(&id).unwrap();
    let logs = scope.logs.as_ref().unwrap();
    let journal = Journal {
        schema: 1,
        operation_id: id.clone(),
        created_at: now(),
        root: scope.root.stamp().unwrap().identity(),
        logs: logs.stamp().unwrap().identity(),
        entries,
    };
    operation
        .write_new("journal.json", &serde_json::to_vec(&journal).unwrap())
        .unwrap();
    logs.move_new("one.log", &operation).unwrap();
    drop(scope);
    let rows = list_recovery(&f.root()).unwrap();
    assert_eq!(rows[0].names, vec!["one.log"]);
    assert!(rows[0].warnings.is_empty());
    restore(
        &f.root(),
        &id,
        &rows[0].revision,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(f.log("one.log")).unwrap(), "first");
    assert_eq!(fs::read_to_string(f.log("two.log")).unwrap(), "second");
    assert!(list_recovery(&f.root()).unwrap().is_empty());
}

#[test]
fn restore_protects_current_and_keeps_mutated_archive() {
    let f = Fixture::new();
    f.write("one.log", "original");
    let cleared = f.cleared(None);
    let row = list_recovery(&f.root()).unwrap().remove(0);
    assert!(restore(
        &f.root(),
        &cleared.operation_id,
        &row.revision,
        Some(&f.log("one.log")),
        &AtomicBool::new(false)
    )
    .is_err());
    let archive = f
        .root()
        .join(".pcl-linux/log-trash")
        .join(&cleared.operation_id)
        .join("one.log");
    fs::write(&archive, "externally changed retained data").unwrap();
    let row = list_recovery(&f.root()).unwrap().remove(0);
    assert!(restore(
        &f.root(),
        &cleared.operation_id,
        &row.revision,
        None,
        &AtomicBool::new(false)
    )
    .is_err());
    assert_eq!(
        fs::read_to_string(archive).unwrap(),
        "externally changed retained data"
    );
    assert!(!f.log("one.log").exists());
}

#[test]
fn symlink_hardlink_and_fifo_sources_are_refused_without_writing() {
    for kind in ["symlink", "hardlink", "fifo"] {
        let f = Fixture::new();
        fs::write(f.out("secret.txt"), "must not export").unwrap();
        match kind {
            "symlink" => symlink(f.out("secret.txt"), f.log("one.log")).unwrap(),
            "hardlink" => fs::hard_link(f.out("secret.txt"), f.log("one.log")).unwrap(),
            _ => {
                use std::os::unix::ffi::OsStrExt;
                let name = std::ffi::CString::new(f.log("one.log").as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
        }
        assert!(prepare_clear(&f.root(), None).is_err());
        assert!(prepare_export(&f.root(), &ExportSelection::All, None).is_err());
        assert!(!f.root().join(".pcl-linux/log-trash").exists());
        assert_eq!(
            fs::read_to_string(f.out("secret.txt")).unwrap(),
            "must not export"
        );
    }
}

#[test]
fn bounds_refuse_large_source_and_read_is_redacted() {
    let f = Fixture::new();
    f.write("one.log", "credential=fake-access-token");
    assert_eq!(
        read(&f.root(), "one.log", redact).unwrap(),
        "credential=[REDACTED]"
    );
    fs::File::create(f.log("large.log"))
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    assert!(prepare_clear(&f.root(), None).is_err());
    assert!(prepare_export(&f.root(), &ExportSelection::All, None).is_err());
    assert!(!f.root().join(".pcl-linux/log-trash").exists());
}

#[test]
fn unsafe_recovery_permissions_are_refused_and_originals_stay_put() {
    let f = Fixture::new();
    f.write("one.log", "original");
    let store = f.root().join(".pcl-linux/log-trash");
    fs::create_dir(&store).unwrap();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o755)).unwrap();
    let plan = prepare_clear(&f.root(), None).unwrap();
    assert!(execute_clear(&f.root(), &plan.revision, None, &AtomicBool::new(false)).is_err());
    assert_eq!(fs::read_to_string(f.log("one.log")).unwrap(), "original");
}

#[test]
fn oversized_current_is_retained_without_blocking_historical_cleanup() {
    let f = Fixture::new();
    f.write("old.log", "historical");
    fs::File::create(f.log("current.log"))
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    let current = f.log("current.log");
    let plan = prepare_clear(&f.root(), Some(&current)).unwrap();
    assert_eq!(plan.names, vec!["old.log"]);
    f.cleared(Some(&current));
    assert_eq!(fs::metadata(current).unwrap().len(), MAX_FILE_BYTES + 1);
    assert!(!f.log("old.log").exists());
}

#[test]
fn incomplete_or_unknown_records_do_not_hide_other_recoverable_logs() {
    let f = Fixture::new();
    f.write("old.log", "historical");
    let cleared = f.cleared(None);
    let store = f.root().join(".pcl-linux/log-trash");
    fs::create_dir(store.join("logs-interrupted-before-journal")).unwrap();
    fs::set_permissions(
        store.join("logs-interrupted-before-journal"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::write(store.join("unknown-file"), "retain this").unwrap();
    let rows = list_recovery(&f.root()).unwrap();
    assert_eq!(rows.len(), 3);
    let known = rows
        .iter()
        .find(|row| row.operation_id == cleared.operation_id)
        .unwrap();
    assert!(known.warnings.is_empty());
    assert_eq!(known.file_count, 1);
    assert_eq!(rows.iter().filter(|row| row.revision.is_empty()).count(), 2);
    restore(
        &f.root(),
        &known.operation_id,
        &known.revision,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(store.join("unknown-file")).unwrap(),
        "retain this"
    );
    assert!(store.join("logs-interrupted-before-journal").is_dir());
}

#[test]
fn unknown_file_in_valid_archive_is_retained() {
    let f = Fixture::new();
    f.write("old.log", "historical");
    let cleared = f.cleared(None);
    let unknown = f
        .root()
        .join(".pcl-linux/log-trash")
        .join(&cleared.operation_id)
        .join("foreign-data");
    fs::write(&unknown, "must survive").unwrap();
    let rows = list_recovery(&f.root()).unwrap();
    assert_eq!(rows[0].file_count, 1);
    assert!(!rows[0].warnings.is_empty());
    assert!(restore(
        &f.root(),
        &rows[0].operation_id,
        &rows[0].revision,
        None,
        &AtomicBool::new(false)
    )
    .is_err());
    assert_eq!(fs::read_to_string(unknown).unwrap(), "must survive");
}

#[test]
fn source_directory_drift_during_export_keeps_output_unpublished() {
    let f = Fixture::new();
    f.write("one.log", "original");
    let selection = ExportSelection::Current;
    let current = f.log("one.log");
    let plan = prepare_export(&f.root(), &selection, Some(&current)).unwrap();
    let moved = f.root().join(".pcl-linux/original-logs");
    let output = f.out("one.log");
    assert!(execute_export(
        &f.root(),
        &selection,
        &plan.revision,
        Some(&current),
        &output,
        &AtomicBool::new(false),
        |text| {
            fs::rename(f.root().join(".pcl-linux/logs"), &moved).unwrap();
            fs::create_dir(f.root().join(".pcl-linux/logs")).unwrap();
            f.write("one.log", "replacement");
            text.to_owned()
        }
    )
    .is_err());
    assert!(!output.exists());
    assert_eq!(
        fs::read_to_string(moved.join("one.log")).unwrap(),
        "original"
    );
    assert_eq!(fs::read_to_string(f.log("one.log")).unwrap(), "replacement");
}

#[test]
fn symlink_target_parent_is_refused_and_redacted_output_is_bounded() {
    let f = Fixture::new();
    f.write("one.log", "original");
    let selection = ExportSelection::Named("one.log".into());
    let plan = prepare_export(&f.root(), &selection, None).unwrap();
    symlink(f.0.join("exports"), f.0.join("alias")).unwrap();
    assert!(execute_export(
        &f.root(),
        &selection,
        &plan.revision,
        None,
        &f.0.join("alias/one.log"),
        &AtomicBool::new(false),
        str::to_owned
    )
    .is_err());
    assert!(execute_export(
        &f.root(),
        &selection,
        &plan.revision,
        None,
        &f.out("one.log"),
        &AtomicBool::new(false),
        |_| "x".repeat(MAX_FILE_BYTES as usize + 1)
    )
    .is_err());
    assert_eq!(fs::read_dir(f.0.join("exports")).unwrap().count(), 0);
}

#[test]
fn recovery_lock_is_exclusive_and_explicitly_unlocked_despite_duplicate_fd() {
    let f = Fixture::new();
    let scope = Scope::open(&f.root()).unwrap();
    let store = scope
        .owner
        .as_ref()
        .unwrap()
        .private_child("log-trash")
        .unwrap();
    let lock = store.lock().unwrap();
    let duplicate = lock.duplicate_for_test();
    assert!(store.lock().is_err());
    drop(lock);
    assert!(store.lock().is_ok());
    drop(duplicate);
}

#[test]
fn completed_restore_relinquishes_logs_before_future_launch_overwrites_them() {
    let f = Fixture::new();
    f.write("one.log", "old launch");
    let cleared = f.cleared(None);
    let row = list_recovery(&f.root()).unwrap().remove(0);
    restore(
        &f.root(),
        &cleared.operation_id,
        &row.revision,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    f.write("one.log", "new launch");
    assert!(list_recovery(&f.root()).unwrap().is_empty());
    assert_eq!(fs::read_to_string(f.log("one.log")).unwrap(), "new launch");
    assert!(restore(
        &f.root(),
        &cleared.operation_id,
        &row.revision,
        None,
        &AtomicBool::new(false)
    )
    .is_err());
}

#[test]
fn interruption_after_last_restore_can_finish_receipt_without_touching_log() {
    let f = Fixture::new();
    f.write("one.log", "old launch");
    let cleared = f.cleared(None);
    let scope = Scope::open(&f.root()).unwrap();
    let store = scope.store().unwrap().unwrap();
    let operation = store.child(&cleared.operation_id).unwrap();
    operation
        .move_new("one.log", scope.logs.as_ref().unwrap())
        .unwrap();
    drop(scope);
    let row = list_recovery(&f.root()).unwrap().remove(0);
    assert_eq!(row.file_count, 0);
    assert!(row.needs_completion);
    assert!(row.warnings.is_empty());
    let original = fs::metadata(f.log("one.log")).unwrap();
    let outcome = restore(
        &f.root(),
        &cleared.operation_id,
        &row.revision,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(outcome.file_count, 0);
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        original.ino(),
        fs::metadata(f.log("one.log")).unwrap().ino()
    );
    assert!(list_recovery(&f.root()).unwrap().is_empty());
    f.write("one.log", "new launch");
    assert!(list_recovery(&f.root()).unwrap().is_empty());
}
