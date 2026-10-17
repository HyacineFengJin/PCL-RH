//! Synthetic directories only. These tests never launch Java/a game, read an
//! actual account, run recovery, or invoke a desktop operation. Export tests
//! publish only to disposable synthetic destination directories.
#[path = "export_tests.rs"]
mod export_tests;
use super::*;
use crate::integration_tests::Fixture;
use flate2::{write::GzEncoder, Compression};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::PathBuf,
};

struct LogsFixture {
    fixture: Fixture,
    shared: Shared,
    root: GameRoot,
}
impl LogsFixture {
    fn new() -> Self {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        let version = Path::new(&root.path).join("versions/Example");
        fs::create_dir_all(&version).unwrap();
        fs::write(version.join("Example.json"), br#"{"id":"Example"}"#).unwrap();
        Self {
            fixture,
            shared,
            root,
        }
    }
    fn instance(&self) -> PathBuf {
        Path::new(&self.root.path).join("versions/Example")
    }
    fn base(&self) -> PathBuf {
        PathBuf::from(&self.root.path)
    }
    fn write(&self, kind: LogKind, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        self.write_at(&self.base(), kind, name, bytes)
    }
    fn write_at(&self, base: &Path, kind: LogKind, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = base.join(kind.directory()).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
    fn list(&self) -> LogListing {
        list(&self.shared, &self.root.id, "Example").unwrap()
    }
    fn read(&self, listing: &LogListing, row: &LogRow) -> Result<LogContent, String> {
        read(
            &self.shared,
            &self.root.id,
            "Example",
            row.kind,
            &row.name,
            &row.revision,
            &listing.revision,
        )
    }
    fn second(&self) -> GameRoot {
        let path = self.fixture.0.join("second");
        fs::create_dir_all(&path).unwrap();
        self.shared
            .config
            .register(path.to_str().unwrap().into(), None)
            .unwrap()
    }
}
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
fn fifo(path: &Path) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}
fn tree(path: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, found: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        if path.is_dir() {
            found.insert(path.strip_prefix(root).unwrap().into(), None);
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), found);
            }
        } else {
            found.insert(
                path.strip_prefix(root).unwrap().into(),
                Some(fs::read(path).unwrap()),
            );
        }
    }
    let mut found = BTreeMap::new();
    visit(path, path, &mut found);
    found
}

#[test]
fn missing_log_directories_are_empty_and_never_created() {
    let f = LogsFixture::new();
    let before = tree(&f.fixture.0);
    let listing = f.list();
    assert!(listing.entries.is_empty());
    assert!(!listing.truncated);
    assert!(listing.shared);
    assert_eq!(listing.game_dir, f.root.path);
    assert_eq!(before, tree(&f.fixture.0));
}
#[test]
fn shared_and_each_automatic_isolation_marker_match_core_and_select_only_that_scope() {
    for marker in [
        None,
        Some("mods"),
        Some("saves"),
        Some("config"),
        Some("options.txt"),
    ] {
        let f = LogsFixture::new();
        if let Some(marker) = marker {
            if marker == "options.txt" {
                fs::write(f.instance().join(marker), b"music:0.5").unwrap();
            } else {
                fs::create_dir(f.instance().join(marker)).unwrap();
            }
        }
        f.write(LogKind::Game, "latest.log", b"shared log");
        f.write_at(&f.instance(), LogKind::Game, "latest.log", b"isolated log");
        let listing = f.list();
        let scope = pcl_core::content_scope::inspect(&f.base(), "Example").unwrap();
        assert_eq!(listing.shared, !scope.is_isolated());
        assert_eq!(
            listing.game_dir,
            if marker.is_some() {
                f.instance()
            } else {
                f.base()
            }
            .display()
            .to_string()
        );
        assert_eq!(
            f.read(&listing, &listing.entries[0]).unwrap().text,
            if marker.is_some() {
                "isolated log"
            } else {
                "shared log"
            }
        );
    }
}
#[test]
fn lists_only_supported_direct_children_and_reads_the_explicit_crash_report() {
    let f = LogsFixture::new();
    for name in ["latest.log", "debug.txt", "older.log.gz"] {
        f.write(LogKind::Game, name, b"game evidence");
    }
    for name in ["latest.LOG", "bad.gz", "config.json"] {
        f.write(LogKind::Game, name, b"excluded");
    }
    f.write(
        LogKind::Crash,
        "crash-one.txt",
        b"explicitly selected report",
    );
    f.write(LogKind::Crash, "excluded.log", b"excluded");
    fs::create_dir_all(f.base().join("logs/nested")).unwrap();
    fs::write(f.base().join("logs/nested/private.log"), b"excluded").unwrap();
    let listing = f.list();
    assert_eq!(listing.entries.len(), 4);
    assert_eq!(listing.id, "Example");
    assert!(listing
        .entries
        .iter()
        .all(|row| row.modified.is_some() && row.revision.len() == 64));
    let crash = listing
        .entries
        .iter()
        .find(|row| row.kind == LogKind::Crash)
        .unwrap();
    let content = f.read(&listing, crash).unwrap();
    assert_eq!(content.text, "explicitly selected report");
    assert_eq!(content.kind, LogKind::Crash);
    assert_eq!(content.name, "crash-one.txt");
    assert!(!content.truncated);
}
#[test]
fn no_other_instance_or_unknown_root_or_traversal_can_be_substituted() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "latest.log", b"Example evidence");
    let listing = f.list();
    let row = &listing.entries[0];
    for root in ["unknown".to_owned(), f.second().id] {
        assert!(list(&f.shared, &root, "Example").is_err());
        assert!(read(
            &f.shared,
            &root,
            "Example",
            row.kind,
            &row.name,
            &row.revision,
            &listing.revision
        )
        .is_err());
    }
    for id in ["Missing", "../Example", "Example/..", ""] {
        assert!(list(&f.shared, &f.root.id, id).is_err());
    }
    for name in [
        "../latest.log",
        "nested/latest.log",
        "..\\latest.log",
        "latest.log\n",
        "bad.gz",
        "",
    ] {
        assert!(read(
            &f.shared,
            &f.root.id,
            "Example",
            LogKind::Game,
            name,
            &row.revision,
            &listing.revision
        )
        .is_err());
    }
    assert!(read(
        &f.shared,
        &f.root.id,
        "Example",
        LogKind::Crash,
        "latest.log",
        &row.revision,
        &listing.revision
    )
    .is_err());
}
#[test]
fn a_row_from_one_instance_cannot_be_reused_for_another_shared_instance() {
    let f = LogsFixture::new();
    fs::create_dir(f.base().join("versions/Another")).unwrap();
    fs::write(f.base().join("versions/Another/Another.json"), b"{}").unwrap();
    f.write(LogKind::Game, "latest.log", b"shared evidence");
    let listing = f.list();
    let row = &listing.entries[0];
    assert!(read(
        &f.shared,
        &f.root.id,
        "Another",
        row.kind,
        &row.name,
        &row.revision,
        &listing.revision
    )
    .is_err());
}
#[test]
fn unrelated_appends_and_new_files_keep_existing_row_and_scope_revisions_valid() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "selected.log", b"selected evidence");
    let other = f.write(LogKind::Game, "latest.log", b"first");
    let listing = f.list();
    let selected = listing
        .entries
        .iter()
        .find(|row| row.name == "selected.log")
        .unwrap();
    OpenOptions::new()
        .append(true)
        .open(other)
        .unwrap()
        .write_all(b" append")
        .unwrap();
    f.write(LogKind::Game, "new.log", b"new");
    let fresh = f.list();
    assert_eq!(listing.revision, fresh.revision);
    assert_eq!(
        selected.revision,
        fresh
            .entries
            .iter()
            .find(|row| row.name == selected.name)
            .unwrap()
            .revision
    );
    assert_eq!(
        f.read(&listing, selected).unwrap().text,
        "selected evidence"
    );
}
#[test]
fn selected_append_replace_remove_or_forged_revision_requires_refresh() {
    for action in [
        "append",
        "replace",
        "remove",
        "row-revision",
        "listing-revision",
    ] {
        let f = LogsFixture::new();
        let path = f.write(LogKind::Game, "latest.log", b"original");
        let mut listing = f.list();
        match action {
            "append" => {
                OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(b" new")
                    .unwrap();
            }
            "replace" => {
                fs::rename(&path, path.with_file_name("retained.log")).unwrap();
                fs::write(&path, b"replacement").unwrap();
            }
            "remove" => {
                fs::remove_file(&path).unwrap();
            }
            "row-revision" => {
                listing.entries[0].revision = "0".repeat(64);
            }
            _ => {
                listing.revision = "0".repeat(64);
            }
        }
        assert!(f.read(&listing, &listing.entries[0]).is_err(), "{action}");
    }
}
#[test]
fn file_change_during_read_cannot_publish_old_bytes_or_a_late_io_error() {
    for fail in [false, true] {
        let f = LogsFixture::new();
        let path = f.write(LogKind::Game, "latest.log", b"original evidence");
        let listing = f.list();
        let row = &listing.entries[0];
        let result = read_with(
            &f.shared,
            &f.root.id,
            "Example",
            row.kind,
            &row.name,
            &row.revision,
            &listing.revision,
            |selected| {
                let original = selected.read(&f.shared.closing);
                fs::write(&path, b"changed evidence").unwrap();
                if fail {
                    Err("late error".into())
                } else {
                    original
                }
            },
            str::to_owned,
        );
        assert!(matches!(result, Err(error) if error == CHANGED));
    }
}
#[test]
fn replacing_each_bound_directory_retires_listing_and_read_publication() {
    for component in ["root", "versions", "instance", "logs", "crash"] {
        for during_list in [false, true] {
            let f = LogsFixture::new();
            f.write(LogKind::Game, "latest.log", b"original evidence");
            f.write(LogKind::Crash, "report.txt", b"report evidence");
            let listing = f.list();
            let row = listing
                .entries
                .iter()
                .find(|row| row.kind == LogKind::Game)
                .unwrap();
            let path = match component {
                "root" => f.base(),
                "versions" => f.base().join("versions"),
                "instance" => f.instance(),
                "logs" => f.base().join("logs"),
                _ => f.base().join("crash-reports"),
            };
            let replace = || {
                fs::rename(&path, path.with_file_name(format!("retained-{component}"))).unwrap();
                fs::create_dir(&path).unwrap();
            };
            let result = if during_list {
                list_with(&f.shared, &f.root.id, "Example", |_| replace()).map(|_| ())
            } else {
                read_with(
                    &f.shared,
                    &f.root.id,
                    "Example",
                    row.kind,
                    &row.name,
                    &row.revision,
                    &listing.revision,
                    |selected| {
                        let tail = selected.read(&f.shared.closing);
                        replace();
                        tail
                    },
                    str::to_owned,
                )
                .map(|_| ())
            };
            assert!(
                matches!(result, Err(error) if error == CHANGED),
                "{component}"
            );
        }
    }
}
#[test]
fn missing_directory_appearing_during_listing_is_a_scope_change() {
    let f = LogsFixture::new();
    let result = list_with(&f.shared, &f.root.id, "Example", |_| {
        f.write(LogKind::Game, "latest.log", b"new evidence");
    });
    assert!(matches!(result, Err(error) if error == CHANGED));
}
#[test]
fn replacing_instance_json_or_adding_isolation_marker_retires_old_selection() {
    for change in ["json", "marker"] {
        let f = LogsFixture::new();
        f.write(LogKind::Game, "latest.log", b"old shared evidence");
        let listing = f.list();
        if change == "json" {
            fs::write(f.instance().join("Example.json"), b"{\"changed\":true}").unwrap();
        } else {
            fs::write(f.instance().join("options.txt"), b"music:0.5").unwrap();
        }
        assert!(f.read(&listing, &listing.entries[0]).is_err());
    }
}
#[test]
fn settings_change_root_switch_and_shutdown_generation_retire_late_reads() {
    for action in [
        "settings",
        "root",
        "closing",
        "generation",
        "preparing",
        "task",
    ] {
        let f = LogsFixture::new();
        let second = f.second();
        f.write(LogKind::Game, "latest.log", b"original evidence");
        let listing = f.list();
        let row = &listing.entries[0];
        let mut admitted_task = None;
        let result = read_with(
            &f.shared,
            &f.root.id,
            "Example",
            row.kind,
            &row.name,
            &row.revision,
            &listing.revision,
            |selected| {
                let result = selected.read(&f.shared.closing);
                match action {
                    "settings" => {
                        f.shared
                            .config
                            .select_installed(&f.root.id, "Example")
                            .unwrap();
                    }
                    "root" => {
                        f.shared.config.select(&second.id).unwrap();
                    }
                    "closing" => {
                        f.shared.closing.store(true, Ordering::SeqCst);
                    }
                    "generation" => {
                        f.shared.close_generation.fetch_add(1, Ordering::SeqCst);
                    }
                    "preparing" => {
                        f.shared.status.lock().unwrap().stage = "preparing".into();
                    }
                    _ => {
                        admitted_task = Some(
                            f.shared
                                .tasks
                                .admit(
                                    crate::tasks::TaskTarget {
                                        root_id: f.root.id.clone(),
                                        root_path: f.root.path.clone(),
                                        instance_id: Some("Example".into()),
                                    },
                                    crate::tasks::TaskKind::ResourceOperation,
                                )
                                .unwrap(),
                        );
                    }
                }
                result
            },
            str::to_owned,
        );
        assert!(matches!(result, Err(error) if error == CHANGED), "{action}");
    }
}
#[test]
fn running_games_allow_reading_but_preparing_closing_and_file_tasks_refuse_admission() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "latest.log", b"running evidence");
    f.shared.status.lock().unwrap().stage = "running".into();
    let listing = f.list();
    assert_eq!(
        f.read(&listing, &listing.entries[0]).unwrap().text,
        "running evidence"
    );
    f.shared.status.lock().unwrap().stage = "preparing".into();
    assert!(list(&f.shared, &f.root.id, "Example").is_err());
    f.shared.status.lock().unwrap().stage = "running".into();
    f.shared.closing.store(true, Ordering::SeqCst);
    assert!(list(&f.shared, &f.root.id, "Example").is_err());
    f.shared.closing.store(false, Ordering::SeqCst);
    let _task = f
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
    assert!(list(&f.shared, &f.root.id, "Example").is_err());
}
#[test]
fn log_reads_and_enumeration_do_not_hold_operations_or_config_locks() {
    let f = LogsFixture::new();
    f.write(LogKind::Game, "latest.log", b"readonly evidence");
    let before = tree(&f.fixture.0);
    let listing = list_with(&f.shared, &f.root.id, "Example", |_| {
        assert!(f.shared.operations.try_lock().is_ok());
        assert_eq!(f.shared.config.snapshot().root_id, f.root.id);
    })
    .unwrap();
    let row = &listing.entries[0];
    read_with(
        &f.shared,
        &f.root.id,
        "Example",
        row.kind,
        &row.name,
        &row.revision,
        &listing.revision,
        |selected| {
            assert!(f.shared.operations.try_lock().is_ok());
            assert_eq!(f.shared.config.snapshot().root_id, f.root.id);
            selected.read(&f.shared.closing)
        },
        str::to_owned,
    )
    .unwrap();
    assert_eq!(tree(&f.fixture.0), before);
}
#[test]
fn symbolic_link_and_special_isolation_markers_refuse_shared_fallback() {
    for linked in [false, true] {
        let f = LogsFixture::new();
        f.write(LogKind::Game, "latest.log", b"must not fall back");
        let marker = f.instance().join("config");
        if linked {
            std::os::unix::fs::symlink("missing", marker).unwrap();
        } else {
            fifo(&marker);
        }
        let error = list(&f.shared, &f.root.id, "Example").err().unwrap();
        assert!(error.contains("符号链接或特殊文件"));
    }
}
#[test]
fn symbolic_log_and_instance_directories_are_never_followed() {
    for component in ["versions", "instance", "logs", "crash"] {
        let f = LogsFixture::new();
        let foreign = f.fixture.0.join("foreign");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("latest.log"), b"foreign secret").unwrap();
        let path = match component {
            "versions" => f.base().join("versions"),
            "instance" => f.instance(),
            "logs" => f.base().join("logs"),
            _ => f.base().join("crash-reports"),
        };
        if path.exists() {
            fs::rename(&path, path.with_file_name("retained-directory")).unwrap();
        }
        std::os::unix::fs::symlink(&foreign, &path).unwrap();
        assert!(list(&f.shared, &f.root.id, "Example").is_err());
        assert_eq!(
            fs::read(foreign.join("latest.log")).unwrap(),
            b"foreign secret"
        );
    }
}
#[test]
fn canonical_root_resolution_does_not_authorize_a_substituted_registered_root_symlink() {
    for ancestor in [false, true] {
        let f = LogsFixture::new();
        f.write(LogKind::Game, "latest.log", b"original evidence");
        let listing = f.list();
        let foreign = f.fixture.0.join("foreign");
        let foreign_root = if ancestor {
            foreign.join(".minecraft")
        } else {
            foreign.clone()
        };
        fs::create_dir_all(foreign_root.join("versions/Example")).unwrap();
        fs::write(foreign_root.join("versions/Example/Example.json"), b"{}").unwrap();
        f.write_at(
            &foreign_root,
            LogKind::Game,
            "latest.log",
            b"foreign secret",
        );
        let target = if ancestor {
            f.base().parent().unwrap().to_owned()
        } else {
            f.base()
        };
        fs::rename(&target, target.with_file_name("retained-root-carrier")).unwrap();
        std::os::unix::fs::symlink(&foreign, &target).unwrap();
        assert!(list(&f.shared, &f.root.id, "Example").is_err());
        assert!(f.read(&listing, &listing.entries[0]).is_err());
        assert_eq!(
            fs::read(foreign_root.join("logs/latest.log")).unwrap(),
            b"foreign secret"
        );
    }
}
#[test]
fn supported_symlink_fifo_hardlink_and_directory_entries_are_refused_without_reading() {
    for carrier in ["symlink", "fifo", "hardlink", "directory"] {
        let f = LogsFixture::new();
        let path = f.write(LogKind::Game, "latest.log", b"original evidence");
        let listing = f.list();
        fs::remove_file(&path).unwrap();
        let foreign = f.fixture.0.join("foreign-secret");
        fs::write(&foreign, b"foreign secret").unwrap();
        match carrier {
            "symlink" => std::os::unix::fs::symlink(&foreign, &path).unwrap(),
            "fifo" => fifo(&path),
            "hardlink" => fs::hard_link(&foreign, &path).unwrap(),
            _ => fs::create_dir(&path).unwrap(),
        }
        assert!(list(&f.shared, &f.root.id, "Example").is_err(), "{carrier}");
        assert!(f.read(&listing, &listing.entries[0]).is_err(), "{carrier}");
        assert_eq!(fs::read(foreign).unwrap(), b"foreign secret");
    }
}
#[test]
fn unreadable_log_file_and_directory_are_errors_and_permissions_are_preserved() {
    let f = LogsFixture::new();
    let path = f.write(LogKind::Game, "latest.log", b"unreadable evidence");
    let listing = f.list();
    for target in [path.clone(), path.parent().unwrap().to_owned()] {
        let mode = fs::metadata(&target).unwrap().permissions().mode();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();
        let listed = list(&f.shared, &f.root.id, "Example");
        let read = f.read(&listing, &listing.entries[0]);
        let retained = fs::symlink_metadata(&target).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        assert!(listed.is_err());
        assert!(read.is_err());
        assert_eq!(retained, 0);
    }
}
#[test]
fn invalid_missing_oversized_or_linked_instance_json_never_becomes_an_empty_log_scope() {
    for action in ["invalid", "array", "missing", "huge", "link"] {
        let f = LogsFixture::new();
        let json = f.instance().join("Example.json");
        match action {
            "invalid" => fs::write(&json, b"invalid").unwrap(),
            "array" => fs::write(&json, b"[]").unwrap(),
            "missing" => fs::remove_file(&json).unwrap(),
            "huge" => {
                OpenOptions::new()
                    .write(true)
                    .open(&json)
                    .unwrap()
                    .set_len(1024 * 1024 + 1)
                    .unwrap();
            }
            _ => {
                fs::remove_file(&json).unwrap();
                std::os::unix::fs::symlink("missing", &json).unwrap();
            }
        }
        assert!(list(&f.shared, &f.root.id, "Example").is_err(), "{action}");
    }
}
#[test]
fn row_limit_is_stably_sorted_and_reports_truncation_after_filtering() {
    let f = LogsFixture::new();
    for index in 0..530 {
        f.write(LogKind::Game, &format!("log-{index:04}.log"), b"entry");
    }
    let listing = f.list();
    assert_eq!(listing.entries.len(), MAX_ROWS);
    assert!(listing.truncated);
    assert!(listing
        .entries
        .windows(2)
        .all(|pair| pair[0].modified > pair[1].modified
            || (pair[0].modified == pair[1].modified && pair[0].name < pair[1].name)));
    assert_eq!(
        f.list()
            .entries
            .iter()
            .map(|row| &row.name)
            .collect::<Vec<_>>(),
        listing
            .entries
            .iter()
            .map(|row| &row.name)
            .collect::<Vec<_>>()
    );
}
#[test]
fn enumeration_limit_includes_unsupported_entries_and_returns_an_explicit_error() {
    let f = LogsFixture::new();
    fs::create_dir(f.base().join("logs")).unwrap();
    for index in 0..4097 {
        fs::write(
            f.base().join("logs").join(format!("ignored-{index}.bin")),
            b"",
        )
        .unwrap();
    }
    assert!(list(&f.shared, &f.root.id, "Example")
        .err()
        .unwrap()
        .contains("4096"));
}
#[test]
fn huge_sparse_plain_log_reads_a_bounded_tail_and_discards_the_partial_first_line() {
    let f = LogsFixture::new();
    let path = f.write(LogKind::Game, "latest.log", b"");
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(8 * 1024 * 1024 * 1024).unwrap();
    file.seek(SeekFrom::End(-50)).unwrap();
    file.write_all(b"\ncomplete final evidence\n").unwrap();
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    assert!(content.truncated);
    assert!(content.text.contains("complete final evidence"));
    assert!(content.text.len() <= reader::MAX_TEXT_BYTES);
}
#[test]
fn unicode_tail_has_no_truncation_replacement_and_a_single_partial_line_is_empty() {
    let f = LogsFixture::new();
    let path = f.write(
        LogKind::Game,
        "latest.log",
        format!("{}\n保留的完整末行 🐈\n", "多字节🐈".repeat(60000)),
    );
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    assert_eq!(content.text, "保留的完整末行 🐈\n");
    assert!(content.truncated);
    assert!(!content.text.contains('\u{fffd}'));
    fs::write(path, "密钥片段🐈".repeat(60000)).unwrap();
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    assert_eq!(content.text, "");
    assert!(content.truncated);
}
#[test]
fn invalid_utf8_is_lossy_but_output_is_still_bounded() {
    let f = LogsFixture::new();
    f.write(
        LogKind::Game,
        "latest.log",
        vec![0xff; reader::MAX_TEXT_BYTES],
    );
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    assert!(content.truncated);
    assert!(content.text.len() <= reader::MAX_TEXT_BYTES);
    assert!(content.text.chars().all(|c| c == '\u{fffd}'));
}
#[test]
fn invalid_utf8_expansion_is_bounded_only_after_redacting_the_complete_secret() {
    let f = LogsFixture::new();
    let secret = "synthetic-full-credential";
    let invalid_bytes = reader::MAX_TEXT_BYTES / 4;
    // With the old premature output cap, lossy expansion put the suffix start
    // in the middle of this otherwise complete credential-bearing line.
    let position = 2 * invalid_bytes - secret.len() / 2;
    let mut bytes = vec![b'a'; position];
    bytes.extend(secret.as_bytes());
    bytes.resize(reader::MAX_TEXT_BYTES - invalid_bytes, b'b');
    bytes.extend(vec![0xff; invalid_bytes]);
    f.write(LogKind::Game, "latest.log", bytes);
    let listing = f.list();
    let row = &listing.entries[0];
    let content = read_with(
        &f.shared,
        &f.root.id,
        "Example",
        row.kind,
        &row.name,
        &row.revision,
        &listing.revision,
        |selected| selected.read(&f.shared.closing),
        |text| {
            assert!(text.contains(secret));
            text.replace(secret, "[REDACTED]")
        },
    )
    .unwrap();
    assert!(!content.text.contains("credential"));
    assert!(!content.text.contains("synthetic"));
    assert!(content.text.len() <= reader::MAX_TEXT_BYTES);
    assert!(content.truncated);
}
#[test]
fn selected_text_is_redacted_once_after_loading_and_expansion_keeps_unicode_boundaries() {
    let f = LogsFixture::new();
    f.write(
        LogKind::Game,
        "latest.log",
        b"token=synthetic-token uuid=synthetic-uuid",
    );
    f.write(
        LogKind::Game,
        "other.log",
        b"other log must never be loaded",
    );
    let listing = f.list();
    let row = listing
        .entries
        .iter()
        .find(|row| row.name == "latest.log")
        .unwrap();
    let content = read_with(
        &f.shared,
        &f.root.id,
        "Example",
        row.kind,
        &row.name,
        &row.revision,
        &listing.revision,
        |selected| selected.read(&f.shared.closing),
        |text| {
            assert_eq!(text, "token=synthetic-token uuid=synthetic-uuid");
            text.replace("synthetic-token", "[REDACTED]")
                .replace("synthetic-uuid", "[REDACTED]")
        },
    )
    .unwrap();
    assert_eq!(content.text, "token=[REDACTED] uuid=[REDACTED]");
    let tail = reader::bound_redacted("猫".repeat(reader::MAX_TEXT_BYTES), false);
    assert!(tail.truncated);
    assert!(tail.text.len() <= reader::MAX_TEXT_BYTES);
    assert!(tail.text.chars().all(|c| c == '猫'));
}
#[test]
fn gzip_single_and_multiple_members_read_in_order_including_an_empty_member() {
    for multiple in [false, true] {
        let f = LogsFixture::new();
        let mut bytes = gzip(b"first line\n");
        if multiple {
            bytes.extend(gzip(b""));
            bytes.extend(gzip("第二行 🐈\n".as_bytes()));
        }
        f.write(LogKind::Game, "older.log.gz", bytes);
        let listing = f.list();
        let content = f.read(&listing, &listing.entries[0]).unwrap();
        assert_eq!(
            content.text,
            if multiple {
                "first line\n第二行 🐈\n"
            } else {
                "first line\n"
            }
        );
        assert!(!content.truncated);
    }
}
#[test]
fn gzip_large_output_keeps_only_a_unicode_safe_complete_tail() {
    let f = LogsFixture::new();
    let data = format!(
        "{}\ncomplete last line 🐈\n",
        "huge 🐈 first line".repeat(40000)
    );
    f.write(LogKind::Game, "older.log.gz", gzip(data.as_bytes()));
    let listing = f.list();
    let content = f.read(&listing, &listing.entries[0]).unwrap();
    assert_eq!(content.text, "complete last line 🐈\n");
    assert!(content.truncated);
}
#[test]
fn truncated_crc_invalid_trailing_junk_and_truncated_second_gzip_member_are_errors() {
    for action in ["truncated", "crc", "junk", "second"] {
        let f = LogsFixture::new();
        let mut bytes = gzip(b"valid first line\n");
        match action {
            "truncated" => {
                bytes.truncate(bytes.len() - 1);
            }
            "crc" => {
                let position = bytes.len() - 8;
                bytes[position] ^= 0xff;
            }
            "junk" => bytes.extend(b"unrelated junk"),
            _ => {
                let mut second = gzip(b"second member");
                second.truncate(second.len() - 4);
                bytes.extend(second);
            }
        }
        f.write(LogKind::Game, "older.log.gz", bytes);
        let listing = f.list();
        assert!(
            f.read(&listing, &listing.entries[0])
                .err()
                .unwrap()
                .contains("损坏或不完整"),
            "{action}"
        );
    }
}
#[test]
fn oversized_gzip_input_and_decompression_bomb_are_explicit_errors() {
    for bomb in [false, true] {
        let f = LogsFixture::new();
        if bomb {
            f.write(
                LogKind::Game,
                "huge.log.gz",
                gzip(&vec![b'x'; reader::MAX_INFLATED_BYTES as usize + 1]),
            );
        } else {
            let path = f.write(LogKind::Game, "huge.log.gz", b"");
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_len(reader::MAX_GZIP_BYTES + 1)
                .unwrap();
        }
        let listing = f.list();
        let error = f.read(&listing, &listing.entries[0]).err().unwrap();
        assert!(error.contains(if bomb { "32 MiB" } else { "8 MiB" }));
    }
}
#[test]
fn gzip_output_budget_applies_across_members_not_separately() {
    let f = LogsFixture::new();
    let half = vec![b'x'; reader::MAX_INFLATED_BYTES as usize / 2 + 1];
    let mut bytes = gzip(&half);
    bytes.extend(gzip(&half));
    f.write(LogKind::Game, "huge.log.gz", bytes);
    let listing = f.list();
    assert!(f
        .read(&listing, &listing.entries[0])
        .err()
        .unwrap()
        .contains("32 MiB"));
}
#[test]
fn pending_recovery_carrier_is_refused_without_repair_or_log_reads() {
    let f = LogsFixture::new();
    f.write(
        LogKind::Game,
        "latest.log",
        b"must not read under uncertain scope",
    );
    fs::create_dir_all(f.base().join(".pcl-linux/instance-resets/unknown")).unwrap();
    let before = tree(&f.fixture.0);
    let error = list(&f.shared, &f.root.id, "Example").err().unwrap();
    assert!(error.contains("实例重置目录含未知记录"));
    assert_eq!(tree(&f.fixture.0), before);
}

#[test]
fn explicit_isolation_controls_log_sources_and_policy_roundtrip_retires_old_listing() {
    use pcl_core::content_scope::IsolationPolicy;
    let f = LogsFixture::new();
    fs::create_dir(f.instance().join("config")).unwrap();
    f.write(LogKind::Game, "latest.log", b"shared log");
    f.write_at(&f.instance(), LogKind::Game, "latest.log", b"isolated log");
    let set_policy = |policy| {
        let mut settings = f.shared.config.snapshot();
        settings.isolation = policy;
        f.shared.config.save(settings).unwrap();
    };
    set_policy(IsolationPolicy::Shared);
    let before = f.list();
    assert!(before.shared);
    assert_eq!(
        f.read(&before, &before.entries[0]).unwrap().text,
        "shared log"
    );
    set_policy(IsolationPolicy::Isolated);
    let isolated = f.list();
    assert!(!isolated.shared);
    assert_eq!(
        f.read(&isolated, &isolated.entries[0]).unwrap().text,
        "isolated log"
    );
    assert!(f.read(&before, &before.entries[0]).is_err());
    set_policy(IsolationPolicy::Shared);
    let current = f.list();
    assert!(current.shared);
    assert_ne!(before.revision, current.revision);
    assert!(f.read(&before, &before.entries[0]).is_err());
    assert_eq!(
        f.read(&current, &current.entries[0]).unwrap().text,
        "shared log"
    );
}
