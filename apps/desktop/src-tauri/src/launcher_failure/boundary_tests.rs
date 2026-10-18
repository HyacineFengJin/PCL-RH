//! Attempt offsets come from native monitor state, never from the renderer.
//! These fixtures exercise appended history without a process or real marker.
use super::*;

#[test]
fn legacy_history_stays_visible_but_never_supplies_current_failure_evidence() {
    for message in [
        "fixture launch failed",
        "Failed to initialize graphics window",
    ] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, mut expected) = failed(&shared, Some("current.log"));
        expected.message = message.into();
        *shared.status.lock().unwrap() = expected.clone();
        launcher_monitor_runtime::set_failure_log_start(&shared, None);
        write_log(
            &root,
            "current.log",
            "java.lang.OutOfMemoryError: old run\njava.lang.UnsupportedClassVersionError: old run",
        );
        let report = report(&shared, &root.id, &expected).unwrap();
        assert_eq!(report.log_scope, FailureLogScope::History);
        assert!(report.log_tail.as_ref().unwrap().contains("old run"));
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].code,
            if message.contains("graphics") {
                "graphics"
            } else {
                "unknown"
            }
        );
        assert!(report.findings[0]
            .evidence
            .iter()
            .all(|line| !line.contains("old run")));
        let prepared = prepare(&shared, &root.id, &expected, &report.content_revision).unwrap();
        assert!(prepared.text().contains("日志范围：历史日志"));
        assert!(prepared.text().contains("以下日志未用于本次预分析"));
        assert!(prepared
            .text()
            .contains("历史日志尾部（仅供查看，不属于本次失败证据）"));
    }
}

#[test]
fn a_known_offset_preserves_the_first_attempt_line_and_excludes_every_history_byte() {
    // A prior run need not end with a newline; the native byte boundary still
    // preserves the first complete line emitted by the new attempt.
    for prefix in [
        "java.lang.OutOfMemoryError: old run".to_owned(),
        "历史内容".repeat(60_000),
    ] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        let current = "Unrecognized VM option 'CurrentAttempt'\ncurrent last line";
        write_log(&root, "current.log", &format!("{prefix}{current}"));
        launcher_monitor_runtime::set_failure_log_start(&shared, Some(prefix.len() as u64));
        let report = report(&shared, &root.id, &expected).unwrap();
        assert_eq!(report.log_scope, FailureLogScope::Attempt);
        assert_eq!(report.log_tail.as_deref(), Some(current));
        assert!(!report.log_truncated);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "java_option");
        assert!(report.findings[0].evidence[0].starts_with("Unrecognized VM option"));
        let prepared = prepare(&shared, &root.id, &expected, &report.content_revision).unwrap();
        assert!(prepared.text().contains("日志范围：本次启动"));
        assert!(!prepared.text().contains(&prefix));
    }
}

#[test]
fn attempt_suffix_drops_only_a_byte_limit_fragment_and_stays_bounded() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, expected) = failed(&shared, Some("current.log"));
    let prefix = "legacy run\n";
    let current = format!(
        "{}\nUnrecognized VM option 'CurrentAttempt'",
        "界".repeat(100_000)
    );
    write_log(&root, "current.log", &format!("{prefix}{current}"));
    launcher_monitor_runtime::set_failure_log_start(&shared, Some(prefix.len() as u64));
    let report = report(&shared, &root.id, &expected).unwrap();
    assert_eq!(report.log_scope, FailureLogScope::Attempt);
    assert_eq!(
        report.log_tail.as_deref(),
        Some("Unrecognized VM option 'CurrentAttempt'")
    );
    assert!(report.log_truncated);
    assert!(!report.log_tail.as_ref().unwrap().contains('\u{fffd}'));
}

#[test]
fn offsets_past_eof_are_read_errors_and_an_empty_attempt_has_no_log_scope() {
    for past_eof in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        let original = "java.lang.OutOfMemoryError: previous run";
        write_log(&root, "current.log", original);
        let offset = original.len() as u64 + u64::from(past_eof);
        launcher_monitor_runtime::set_failure_log_start(&shared, Some(offset));
        let mut log = FailureLogCapture::capture(
            Path::new(&root.path),
            Some(&log_path(&root, "current.log")),
        )
        .unwrap();
        let read = log.read_tail_from(Some(offset));
        if past_eof {
            assert!(read.is_err());
        } else {
            assert_eq!(read.unwrap().text, "");
        }
        let report = report(&shared, &root.id, &expected).unwrap();
        assert_eq!(report.log_scope, FailureLogScope::None);
        assert_eq!(report.findings[0].code, "unknown");
        assert_eq!(report.log_error.is_some(), past_eof);
        assert!(!report.log_truncated);
        assert!(!report
            .log_tail
            .as_deref()
            .unwrap_or_default()
            .contains("previous run"));
    }
}

#[test]
fn a_scope_or_offset_change_cannot_reuse_the_displayed_receipt_even_for_equal_text() {
    for initial in [None, Some(0)] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, expected) = failed(&shared, Some("current.log"));
        write_log(&root, "current.log", "\ncurrent text");
        launcher_monitor_runtime::set_failure_log_start(&shared, initial);
        let report = report(&shared, &root.id, &expected).unwrap();
        launcher_monitor_runtime::set_failure_log_start(
            &shared,
            Some(if initial.is_none() { 0 } else { 1 }),
        );
        assert!(prepare(&shared, &root.id, &expected, &report.content_revision).is_err());
    }
}
