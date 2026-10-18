//! Synthetic report/export gaps stand in for the native chooser. Sources,
//! destinations, accounts and journals are confined to disposable fixture trees.
use super::*;

fn output(fixture: &Fixture) -> PathBuf {
    fixture.0.join("launch-failure-report.txt")
}

fn prepared(shared: &Shared, root: &GameRoot, expected: &RunStatus) -> PreparedDiagnostic {
    let report = report(shared, &root.id, expected).unwrap();
    prepare(shared, &root.id, expected, &report.content_revision).unwrap()
}

#[test]
fn exported_report_contains_only_the_current_redacted_bounded_native_facts() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, mut expected) = failed(&shared, Some("current.log"));
    expected.message = "java.lang.OutOfMemoryError: fixture-secret".into();
    *shared.status.lock().unwrap() = expected.clone();
    let log = format!(
        "not-in-the-current-tail\n{}\njava.lang.OutOfMemoryError: fixture-secret\nlast evidence",
        "ordinary tail\n".repeat(30_000)
    );
    write_log(&root, "current.log", &log);
    write_log(&root, "unrelated.log", "unrelated raw secret sentinel");
    let crash = Path::new(&root.path).join("crash-reports");
    fs::create_dir(&crash).unwrap();
    fs::write(
        crash.join("crash-newest.txt"),
        b"unrelated crash secret sentinel",
    )
    .unwrap();
    let before = tree(Path::new(&root.path));
    let redact = |text: &str| text.replace("fixture-secret", "[REDACTED]");
    let report = report_with(
        &shared,
        &root.id,
        &expected,
        FailureLogCapture::read_tail_from,
        redact,
    )
    .unwrap();
    assert_eq!(report.content_revision.len(), 64);
    assert!(report.log_truncated);
    let prepared = prepare_with(
        &shared,
        &root.id,
        &expected,
        &report.content_revision,
        |text| {
            assert!(shared.operations.try_lock().is_ok());
            redact(text)
        },
    )
    .unwrap();
    assert!(shared.operations.try_lock().is_ok());
    let accepted = prepared.text().to_owned();
    assert!(accepted.contains("退出码：1"));
    assert!(accepted.contains("java_memory"));
    assert!(accepted.contains("证据：java.lang.OutOfMemoryError: [REDACTED]"));
    assert!(accepted.contains("日志已截断：是"));
    assert!(accepted.ends_with("last evidence\n"));
    for absent in [
        "fixture-secret",
        "not-in-the-current-tail",
        "unrelated raw",
        "unrelated crash",
    ] {
        assert!(!accepted.contains(absent));
    }
    let result = prepared.publish(&shared, &output(&fixture)).unwrap();
    assert_eq!(result.status, "complete");
    assert_eq!(result.count, 1);
    assert_eq!(result.path.as_deref(), output(&fixture).to_str());
    assert_eq!(fs::read_to_string(output(&fixture)).unwrap(), accepted);
    assert_eq!(tree(Path::new(&root.path)), before);
}

#[test]
fn unreadable_or_absent_current_log_exports_message_and_error_without_guessing_a_file() {
    for name in [None, Some("missing.log")] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, name);
        write_log(&root, "latest.log", "unrelated evidence");
        let prepared = prepared(&shared, &root, &expected);
        assert!(prepared.report().log_error.is_some());
        assert!(prepared.text().contains("日志读取错误："));
        assert!(prepared.text().contains(&expected.message));
        assert!(!prepared.text().contains("unrelated evidence"));
        prepared.publish(&shared, &output(&fixture)).unwrap();
    }
}

#[test]
fn malformed_forged_or_differently_redacted_receipts_refuse_export() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, expected) = failed(&shared, Some("current.log"));
    write_log(&root, "current.log", "visible evidence");
    let report = report(&shared, &root.id, &expected).unwrap();
    for revision in [String::new(), "0".repeat(64), "x".repeat(64)] {
        assert!(prepare(&shared, &root.id, &expected, &revision).is_err());
    }
    assert!(prepare_with(
        &shared,
        &root.id,
        &expected,
        &report.content_revision,
        |text| text.replace("visible", "[REDACTED]")
    )
    .is_err());
    assert!(!output(&fixture).exists());
}

#[test]
fn receipt_covers_exactly_presented_errors_findings_tail_and_truncation() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, expected) = failed(&shared, Some("current.log"));
    write_log(&root, "current.log", "java.lang.OutOfMemoryError: evidence");
    let original = report(&shared, &root.id, &expected)
        .unwrap()
        .content_revision;
    for field in [
        "version",
        "exit",
        "message",
        "findings",
        "evidence",
        "name",
        "tail",
        "error",
        "truncated",
        "scope",
        "offset",
        "text",
    ] {
        let mut prepared = prepare_read_with(
            &shared,
            &root.id,
            &expected,
            FailureLogCapture::read_tail_from,
            str::to_owned,
        )
        .unwrap();
        match field {
            "version" => prepared.report.version = None,
            "exit" => prepared.report.exit_code = None,
            "message" => prepared.report.message.push_str(" changed"),
            "findings" => prepared.report.findings[0].code = "unknown".into(),
            "evidence" => prepared.report.findings[0].evidence.clear(),
            "name" => prepared.report.log_name = None,
            "tail" => prepared.report.log_tail = None,
            "error" => prepared.report.log_error = Some("read error".into()),
            "truncated" => prepared.report.log_truncated = !prepared.report.log_truncated,
            "scope" => prepared.report.log_scope = FailureLogScope::History,
            "offset" => prepared.captured.failure.log_start = Some(1),
            _ => prepared.text.push_str(" changed"),
        }
        assert_ne!(
            content_revision(&prepared.captured, &prepared.report, &prepared.text).unwrap(),
            original,
            "{field}"
        );
    }
}

fn mutate(action: &str, shared: &Shared, root: &GameRoot, expected: &RunStatus, second: &GameRoot) {
    match action {
        "file" => write_log(root, "current.log", "modified evidence"),
        "same-bytes-replacement" => {
            let file = log_path(root, "current.log");
            fs::rename(&file, file.with_file_name("retained.log")).unwrap();
            write_log(root, "current.log", "original evidence");
        }
        "removed" => fs::remove_file(log_path(root, "current.log")).unwrap(),
        "log-binding" => *shared.log.lock().unwrap() = Some(log_path(root, "other.log")),
        "root" => {
            let path = Path::new(&root.path);
            fs::rename(path, path.with_file_name("retained-root")).unwrap();
            fs::create_dir(path).unwrap();
        }
        "settings" => {
            shared.config.select(&second.id).unwrap();
            shared.config.select(&root.id).unwrap();
        }
        "root-selection" => {
            shared.config.select(&second.id).unwrap();
        }
        "closing" => shared.closing.store(true, Ordering::SeqCst),
        "close-generation" => {
            shared.close_generation.fetch_add(1, Ordering::SeqCst);
        }
        "task-ended" => {
            let task = shared
                .tasks
                .admit(
                    crate::tasks::TaskTarget {
                        root_id: root.id.clone(),
                        root_path: root.path.clone(),
                        instance_id: Some("fixture".into()),
                    },
                    crate::tasks::TaskKind::ResourceOperation,
                )
                .unwrap();
            task.finish(crate::tasks::TaskOutcome::Complete {
                result: None,
                message: "fixture completed".into(),
                error: None,
            });
        }
        "recovery" => {
            fs::create_dir_all(Path::new(&root.path).join(".pcl-linux/instance-resets/unknown"))
                .unwrap()
        }
        "failure-generation" => {
            let mut next = expected.clone();
            next.stage = "preparing".into();
            let generation = launcher_monitor_runtime::begin_preparation(shared, next).unwrap();
            launcher_monitor_runtime::update_current(
                shared,
                generation,
                "error",
                expected.message.clone(),
                expected.pid,
                expected.exit_code,
            );
            *shared.log.lock().unwrap() = Some(log_path(root, "current.log"));
            assert!(*shared.status.lock().unwrap() == *expected);
        }
        _ => unreachable!(),
    }
}

#[test]
fn chooser_gap_and_final_publication_revalidate_every_bound_scope() {
    for at_final_gate in [false, true] {
        for action in [
            "file",
            "same-bytes-replacement",
            "removed",
            "log-binding",
            "root",
            "settings",
            "root-selection",
            "closing",
            "close-generation",
            "task-ended",
            "recovery",
            "failure-generation",
        ] {
            let fixture = Fixture::new();
            let shared = fixture.shared();
            let (root, expected) = failed(&shared, Some("current.log"));
            let second = second_root(&fixture, &shared);
            write_log(&root, "current.log", "original evidence");
            let prepared = prepared(&shared, &root, &expected);
            let result = if at_final_gate {
                prepared.publish_with(&shared, &output(&fixture), || {
                    assert!(shared.operations.try_lock().is_ok());
                    mutate(action, &shared, &root, &expected, &second);
                })
            } else {
                mutate(action, &shared, &root, &expected, &second);
                prepared.publish(&shared, &output(&fixture))
            };
            assert!(
                matches!(result, Err(error) if error == CHANGED),
                "{action} final={at_final_gate}"
            );
            assert!(!output(&fixture).exists(), "{action}");
        }
    }
}

#[test]
fn changed_scope_between_display_and_prepare_cannot_reuse_a_receipt() {
    for action in [
        "same-bytes-replacement",
        "settings",
        "task-ended",
        "failure-generation",
    ] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        let second = second_root(&fixture, &shared);
        write_log(&root, "current.log", "original evidence");
        let report = report(&shared, &root.id, &expected).unwrap();
        mutate(action, &shared, &root, &expected, &second);
        assert!(
            prepare(&shared, &root.id, &expected, &report.content_revision).is_err(),
            "{action}"
        );
    }
}

#[test]
fn existing_targets_symlinks_and_non_txt_outputs_are_preserved() {
    for action in ["existing", "symlink", "extension"] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, None);
        let prepared = prepared(&shared, &root, &expected);
        let kept = fixture.0.join("keep.txt");
        fs::write(&kept, b"preserved sentinel").unwrap();
        let mut target = output(&fixture);
        match action {
            "existing" => fs::write(&target, b"preserved sentinel").unwrap(),
            "symlink" => std::os::unix::fs::symlink(&kept, &target).unwrap(),
            _ => {
                target.set_extension("log");
            }
        }
        let before = tree(&fixture.0);
        assert!(prepared.publish(&shared, &target).is_err());
        assert_eq!(tree(&fixture.0), before);
    }
}

#[test]
fn output_under_game_root_or_a_symlink_parent_is_rejected() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, expected) = failed(&shared, Some("current.log"));
    write_log(&root, "current.log", "original evidence");
    let alias = fixture.0.join("alias");
    std::os::unix::fs::symlink(&root.path, &alias).unwrap();
    for parent in [
        PathBuf::from(&root.path),
        log_path(&root, "current.log").parent().unwrap().to_owned(),
        alias,
    ] {
        let prepared = prepared(&shared, &root, &expected);
        let before = tree(Path::new(&root.path));
        assert!(prepared
            .publish(&shared, &parent.join("report.txt"))
            .is_err());
        assert_eq!(tree(Path::new(&root.path)), before);
    }
}

#[test]
fn destination_replacement_or_moving_it_inside_game_root_cannot_publish() {
    for moved_inside in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, None);
        let prepared = prepared(&shared, &root, &expected);
        let selected = fixture.0.join("chosen");
        let retained = if moved_inside {
            Path::new(&root.path).join("moved-chosen")
        } else {
            fixture.0.join("retained-chosen")
        };
        fs::create_dir(&selected).unwrap();
        let result = prepared.publish_with(&shared, &selected.join("report.txt"), || {
            fs::rename(&selected, &retained).unwrap();
            if moved_inside {
                std::os::unix::fs::symlink(&retained, &selected).unwrap();
            } else {
                fs::create_dir(&selected).unwrap();
            }
        });
        assert!(result.is_err());
        assert!(fs::read_dir(retained).unwrap().next().is_none());
        assert!(fs::read_dir(selected).unwrap().next().is_none());
    }
}

#[test]
fn cancelled_chooser_leaves_no_output_and_returns_an_empty_outcome() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, expected) = failed(&shared, None);
    let before = tree(&fixture.0);
    let prepared = prepared(&shared, &root, &expected);
    assert!(shared.operations.try_lock().is_ok());
    drop(prepared);
    let result = crate::toolbox_images::publish::ExportOutcome::cancelled();
    assert_eq!(result.status, "cancelled");
    assert_eq!(result.count, 0);
    assert!(result.path.is_none() && result.warning.is_none());
    assert_eq!(tree(&fixture.0), before);
}

#[test]
fn native_ai_and_export_admission_can_retain_a_short_operation_guard() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, expected) = failed(&shared, None);
    let prepared = prepared(&shared, &root, &expected);
    prepared.verify(&shared).unwrap();
    assert!(shared.operations.try_lock().is_ok());
    let guard = prepared.authorize(&shared).unwrap();
    assert!(shared.operations.try_lock().is_err());
    drop(guard);
    assert!(shared.operations.try_lock().is_ok());
}

#[test]
fn active_tasks_and_uncertain_recovery_refuse_report_before_log_read() {
    for recovery in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        write_log(&root, "current.log", "should never read");
        let _task = if recovery {
            fs::create_dir_all(Path::new(&root.path).join(".pcl-linux/instance-resets/unknown"))
                .unwrap();
            None
        } else {
            Some(
                shared
                    .tasks
                    .admit(
                        crate::tasks::TaskTarget {
                            root_id: root.id.clone(),
                            root_path: root.path.clone(),
                            instance_id: None,
                        },
                        crate::tasks::TaskKind::ResourceOperation,
                    )
                    .unwrap(),
            )
        };
        let before = tree(&fixture.0);
        let result = report_with(
            &shared,
            &root.id,
            &expected,
            |_, _| panic!("unstable scope must refuse admission before reading logs"),
            str::to_owned,
        );
        assert!(result.is_err());
        assert_eq!(tree(&fixture.0), before);
    }
}
