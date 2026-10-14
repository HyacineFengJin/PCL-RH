//! Synthetic failed attempts exercise publication races without launching a
//! monitor, Java or Tauri, or reading a user account/log directory.
use super::*;
use crate::integration_tests::Fixture;
use std::{collections::BTreeMap, fs, path::PathBuf};

fn failed(shared: &Shared, name: Option<&str>) -> (GameRoot, RunStatus) {
    let root = shared.config.resolve(None).unwrap();
    let run = RunStatus {
        stage: "error".into(),
        message: "fixture launch failed".into(),
        version: Some("fixture".into()),
        exit_code: Some(1),
        root_id: Some(root.id.clone()),
        root_path: Some(root.path.clone()),
        ..Default::default()
    };
    *shared.status.lock().unwrap() = run.clone();
    *shared.log.lock().unwrap() = name.map(|name| log_path(&root, name));
    (root, run)
}

fn log_path(root: &GameRoot, name: &str) -> PathBuf {
    Path::new(&root.path).join(".pcl-linux/logs").join(name)
}

fn write_log(root: &GameRoot, name: &str, text: &str) {
    let path = log_path(root, name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn second_root(fixture: &Fixture, shared: &Shared) -> GameRoot {
    let path = fixture.0.join("second");
    fs::create_dir_all(&path).unwrap();
    shared
        .config
        .register(path.to_str().unwrap().into(), None)
        .unwrap()
}

fn tree(path: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(path: &Path, root: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        if path.is_dir() {
            entries.insert(path.strip_prefix(root).unwrap().to_owned(), None);
            for entry in fs::read_dir(path).unwrap() {
                visit(&entry.unwrap().path(), root, entries);
            }
        } else {
            entries.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                Some(fs::read(path).unwrap()),
            );
        }
    }
    let mut entries = BTreeMap::new();
    visit(path, path, &mut entries);
    entries
}

#[test]
fn report_is_read_only_and_releases_operation_and_configuration_locks_during_log_read() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, expected) = failed(&shared, Some("current.log"));
    write_log(&root, "current.log", "current failure evidence");
    fs::create_dir_all(fixture.0.join(".pcl-rust")).unwrap();
    fs::write(
        fixture.0.join(".pcl-rust/accounts.json"),
        b"synthetic account sentinel is not a diagnostic input",
    )
    .unwrap();
    let before = tree(&fixture.0);
    let report = report_with(
        &shared,
        &root.id,
        &expected,
        |log| {
            assert!(shared.operations.try_lock().is_ok());
            assert_eq!(shared.config.snapshot().root_id, root.id);
            log.read_tail()
        },
        str::to_owned,
    )
    .unwrap();
    assert_eq!(report.version.as_deref(), Some("fixture"));
    assert_eq!(report.exit_code, Some(1));
    assert_eq!(report.log_name.as_deref(), Some("current.log"));
    assert_eq!(report.log_tail.as_deref(), Some("current failure evidence"));
    assert!(report.log_error.is_none());
    assert!(!report.log_truncated);
    assert_eq!(tree(&fixture.0), before);
}

#[test]
fn missing_current_log_keeps_the_failure_message_without_guessing_latest() {
    for name in [None, Some("missing.log")] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, name);
        write_log(&root, "latest.log", "unrelated old failure evidence");
        let report = report(&shared, &root.id, &expected).unwrap();
        assert_eq!(report.message, expected.message);
        assert_eq!(report.log_name.as_deref(), name);
        assert!(report.log_tail.is_none());
        assert!(report.log_error.is_some());
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("unrelated old failure"));
    }
}

#[test]
fn submitted_status_and_root_must_describe_the_current_failure() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, mut expected) = failed(&shared, None);
    expected.message = "stale failure".into();
    assert!(report(&shared, &root.id, &expected).is_err());
    let second = second_root(&fixture, &shared);
    let actual = shared.status.lock().unwrap().clone();
    assert!(report(&shared, &second.id, &actual).is_err());
    shared.status.lock().unwrap().stage = "exited".into();
    assert!(report(&shared, &root.id, &actual).is_err());
}

#[test]
fn recovered_root_level_failure_does_not_infer_a_version_from_its_log_name() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, mut expected) = failed(&shared, Some("another-instance.log"));
    expected.version = None;
    *shared.status.lock().unwrap() = expected.clone();
    write_log(&root, "another-instance.log", "recovered failure evidence");
    let report = report(&shared, &root.id, &expected).unwrap();
    assert!(report.version.is_none());
    assert_eq!(report.log_name.as_deref(), Some("another-instance.log"));
    assert_eq!(report.exit_code, expected.exit_code);
    let mut wire = serde_json::to_value(&expected).unwrap();
    wire["unexpected_identity"] = serde_json::json!("rejected");
    assert!(serde_json::from_value::<RunStatus>(wire).is_err());
}

#[test]
fn switching_root_during_log_read_rejects_even_a_read_error() {
    for fail_read in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        write_log(&root, "current.log", "old evidence");
        let second = second_root(&fixture, &shared);
        let result = report_with(
            &shared,
            &root.id,
            &expected,
            |log| {
                shared.config.select(&second.id).unwrap();
                if fail_read {
                    Err("late read error".into())
                } else {
                    log.read_tail()
                }
            },
            str::to_owned,
        );
        assert!(matches!(result, Err(error) if error == CHANGED));
    }
}

#[test]
fn a_new_attempt_with_identical_status_and_log_cannot_publish_the_old_report() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, expected) = failed(&shared, Some("current.log"));
    write_log(&root, "current.log", "old evidence");
    let result = report_with(
        &shared,
        &root.id,
        &expected,
        |log| {
            let mut next = expected.clone();
            next.stage = "preparing".into();
            let generation = launcher_monitor_runtime::begin_preparation(&shared, next).unwrap();
            launcher_monitor_runtime::update_current(
                &shared,
                generation,
                "error",
                expected.message.clone(),
                expected.pid,
                expected.exit_code,
            );
            *shared.log.lock().unwrap() = Some(log_path(&root, "current.log"));
            assert!(*shared.status.lock().unwrap() == expected);
            log.read_tail()
        },
        str::to_owned,
    );
    assert!(matches!(result, Err(error) if error == CHANGED));
}

#[test]
fn replacing_any_bound_directory_or_the_log_rejects_the_report() {
    for component in ["root", "owner", "logs", "file"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        write_log(&root, "current.log", "original evidence");
        let path = match component {
            "root" => PathBuf::from(&root.path),
            "owner" => Path::new(&root.path).join(".pcl-linux"),
            "logs" => log_path(&root, "current.log").parent().unwrap().to_owned(),
            _ => log_path(&root, "current.log"),
        };
        let result = report_with(
            &shared,
            &root.id,
            &expected,
            |log| {
                fs::rename(&path, path.with_file_name("retained-carrier")).unwrap();
                if component == "file" {
                    fs::write(&path, b"replacement evidence").unwrap();
                } else {
                    fs::create_dir(&path).unwrap();
                }
                log.read_tail()
            },
            str::to_owned,
        );
        assert!(matches!(result, Err(error) if error == CHANGED));
    }
}

#[test]
fn a_log_appearing_or_changing_after_capture_rejects_the_report() {
    for existed in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        fs::create_dir_all(log_path(&root, "current.log").parent().unwrap()).unwrap();
        if existed {
            write_log(&root, "current.log", "original evidence");
        }
        let result = report_with(
            &shared,
            &root.id,
            &expected,
            |log| {
                write_log(&root, "current.log", "changed evidence");
                log.read_tail()
            },
            str::to_owned,
        );
        assert!(matches!(result, Err(error) if error == CHANGED));
    }
}

#[test]
fn foreign_log_binding_and_symbolic_links_never_read_another_file() {
    for symlink in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        let foreign = fixture.0.join("foreign.log");
        fs::write(&foreign, b"foreign secret sentinel").unwrap();
        if symlink {
            let current = log_path(&root, "current.log");
            fs::create_dir_all(current.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&foreign, current).unwrap();
        } else {
            *shared.log.lock().unwrap() = Some(foreign.clone());
        }
        let report = report(&shared, &root.id, &expected).unwrap();
        assert!(report.log_tail.is_none());
        assert!(report.log_error.is_some());
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("foreign secret"));
        assert_eq!(fs::read(foreign).unwrap(), b"foreign secret sentinel");
    }
}

#[test]
fn linked_log_directories_and_non_regular_or_shared_files_remain_read_errors() {
    for carrier in ["owner-link", "logs-link", "fifo", "hardlink"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        let foreign = fixture.0.join("foreign");
        fs::create_dir_all(foreign.join("logs")).unwrap();
        fs::write(foreign.join("logs/current.log"), b"foreign secret sentinel").unwrap();
        fs::write(foreign.join("current.log"), b"foreign secret sentinel").unwrap();
        let owner = Path::new(&root.path).join(".pcl-linux");
        let logs = owner.join("logs");
        if carrier == "owner-link" {
            std::os::unix::fs::symlink(&foreign, &owner).unwrap();
        } else if carrier == "logs-link" {
            fs::create_dir(&owner).unwrap();
            std::os::unix::fs::symlink(&foreign, &logs).unwrap();
        } else {
            fs::create_dir_all(&logs).unwrap();
            let current = log_path(&root, "current.log");
            if carrier == "fifo" {
                use std::{ffi::CString, os::unix::ffi::OsStrExt};
                let path = CString::new(current.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            } else {
                fs::hard_link(foreign.join("current.log"), current).unwrap();
            }
        }
        let report = report(&shared, &root.id, &expected).unwrap();
        assert!(report.log_tail.is_none());
        assert!(report.log_error.is_some());
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("foreign secret"));
    }
}

#[test]
fn sanitized_text_enters_the_classifier_and_report_evidence_and_log_fields() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, mut expected) = failed(&shared, Some("fixture-secret.log"));
    expected.message = "java.lang.OutOfMemoryError: Java heap space fixture-secret".into();
    *shared.status.lock().unwrap() = expected.clone();
    write_log(&root, "fixture-secret.log", &expected.message);
    let redact = |text: &str| text.replace("fixture-secret", "[REDACTED]");
    let report = report_with(
        &shared,
        &root.id,
        &expected,
        FailureLogCapture::read_tail,
        redact,
    )
    .unwrap();
    assert!(report.message.contains("[REDACTED]"));
    assert!(report.log_tail.as_ref().unwrap().contains("[REDACTED]"));
    assert!(report.log_name.is_none());
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("fixture-secret"));
    let report = report_with(
        &shared,
        &root.id,
        &expected,
        |_| Err("cannot read fixture-secret".into()),
        redact,
    )
    .unwrap();
    assert_eq!(report.log_error.as_deref(), Some("cannot read [REDACTED]"));
}

#[test]
fn closing_generation_or_log_binding_change_retires_the_report() {
    for closing in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        write_log(&root, "current.log", "original evidence");
        let result = report_with(
            &shared,
            &root.id,
            &expected,
            |log| {
                if closing {
                    shared.close_generation.fetch_add(1, Ordering::SeqCst);
                } else {
                    *shared.log.lock().unwrap() = Some(log_path(&root, "other.log"));
                }
                log.read_tail()
            },
            str::to_owned,
        );
        assert!(matches!(result, Err(error) if error == CHANGED));
    }
}
