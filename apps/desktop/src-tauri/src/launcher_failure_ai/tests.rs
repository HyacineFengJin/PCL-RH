//! Synthetic native receipts and an injected bridge: no Node model request,
//! account lookup, desktop operation or user configuration is involved.
use super::*;
use crate::{config::GameRoot, integration_tests::Fixture, launcher_monitor_runtime};
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
};

const TOKEN: &str = "11111111-1111-4111-8111-111111111111";
const JOB: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn diagnostic_host_bridge_refuses_unrelated_operations_without_starting_a_child() {
    let fixture = Fixture::new();
    let host = Host::new(fixture.0.clone());
    assert!(host
        .diagnostic_call("provider_preset_save", json!({}))
        .is_err());
    assert!(host.diagnostic_call("job_create", json!({})).is_err());
    assert!(!fixture.0.join(".pcl-rust/experimental").exists());
    for name in [
        "diagnostic_preview",
        "diagnostic_analyze",
        "diagnostic_status",
        "diagnostic_cancel",
    ] {
        assert!(serde_json::from_value::<crate::experimental::Operation>(json!(name)).is_err());
    }
}

fn failed(shared: &Shared) -> (GameRoot, RunStatus, PathBuf, String) {
    let root = shared.config.resolve(None).unwrap();
    let run = RunStatus {
        stage: "error".into(),
        message: "synthetic launch failure".into(),
        version: Some("fixture".into()),
        exit_code: Some(1),
        root_id: Some(root.id.clone()),
        root_path: Some(root.path.clone()),
        ..Default::default()
    };
    let log = Path::new(&root.path).join(".pcl-linux/logs/current.log");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(
        &log,
        "java.lang.OutOfMemoryError: synthetic evidence\nfinal native evidence\n",
    )
    .unwrap();
    *shared.status.lock().unwrap() = run.clone();
    *shared.log.lock().unwrap() = Some(log.clone());
    launcher_monitor_runtime::set_failure_log_start(shared, Some(0));
    let revision = launcher_failure::report(shared, &root.id, &run)
        .unwrap()
        .content_revision;
    (root, run, log, revision)
}

#[test]
fn complete_native_text_is_preserved_when_both_transport_budgets_fit() {
    let id = "a".repeat(64);
    let text = "完整的原生报告\n不可信日志资料\n";
    let payload = payload_text(&id, text, FailureLogScope::Attempt, false);
    assert_eq!(payload.report_id, id);
    assert_eq!(payload.text, text);
    assert!(!payload.truncated);
    assert!(input_size(&payload.report_id, &payload.text) <= INPUT_BYTES);
}

#[test]
fn bounded_ai_excerpt_keeps_utf8_head_tail_and_explicit_end_markers() {
    let id = "a".repeat(64);
    let text = format!(
        "native report opening\n{}\nlast native evidence终",
        "中文🙂evidence\n".repeat(12_000)
    );
    let payload = payload_text(&id, &text, FailureLogScope::Attempt, true);
    assert!(payload.truncated);
    assert!(payload.text.len() <= REPORT_BYTES);
    assert!(input_size(&id, &payload.text) <= INPUT_BYTES);
    assert!(payload.text.contains("native report opening"));
    assert!(payload.text.contains("last native evidence终"));
    assert!(payload.text.contains("原生报告中段已省略"));
    assert!(payload.text.ends_with(EXCERPT_END));
    assert!(payload.text.contains("原始日志已截断：是"));
    let repeated = payload_text(&id, &text, FailureLogScope::Attempt, true);
    assert_eq!(
        payload.text, repeated.text,
        "Preview and start use exactly the same deterministic payload"
    );
}

#[test]
fn json_escaped_byte_limit_can_require_excerpt_even_below_the_utf8_limit() {
    let id = "a".repeat(64);
    for text in [
        format!("opening\n{}\nlast\n", "\0".repeat(12_000)),
        format!("opening\n{}\nlast\n", "\\\"".repeat(18_000)),
    ] {
        assert!(text.len() < REPORT_BYTES);
        assert!(input_size(&id, &text) > INPUT_BYTES);
        let payload = payload_text(&id, &text, FailureLogScope::History, false);
        assert!(payload.truncated);
        assert!(payload.text.len() <= REPORT_BYTES);
        assert!(input_size(&id, &payload.text) <= INPUT_BYTES);
        assert!(payload.text.contains("不属于本次失败证据"));
        assert!(payload.text.contains("opening"));
        assert!(payload.text.contains("last"));
        assert!(payload.text.ends_with(EXCERPT_END));
    }
}

#[test]
fn preview_and_start_only_send_the_same_native_text_without_provider_configuration() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, run, _, revision) = failed(&shared);
    // A synthetic provider file must never be read or copied into the report.
    let presets = fixture.0.join(".pcl-rust/experimental/ai-presets.json");
    fs::create_dir_all(presets.parent().unwrap()).unwrap();
    fs::write(&presets, "synthetic-config-secret-sentinel").unwrap();
    let expected_text = launcher_failure::prepare(&shared, &root.id, &run, &revision)
        .unwrap()
        .text()
        .to_owned();
    let preview = preview_with(&shared, &root.id, &run, &revision, |operation, args| {
        assert!(shared.operations.try_lock().is_ok());
        assert_eq!(operation, "diagnostic_preview");
        assert_eq!(args, json!({"reportId":revision}));
        Ok(json!({"ready":true,"confirmationToken":TOKEN,"selectedPreset":{"name":"Synthetic"}}))
    })
    .unwrap();
    assert_eq!(preview["report_id"], revision);
    assert_eq!(preview["report_text"], expected_text);
    assert_eq!(preview["report_truncated"], false);
    assert_eq!(preview["configuration"]["confirmationToken"], TOKEN);
    assert!(!preview
        .to_string()
        .contains("synthetic-config-secret-sentinel"));
    let started = start_with(
        &shared,
        &root.id,
        &run,
        &revision,
        TOKEN,
        |operation, args| {
            assert!(shared.operations.try_lock().is_ok());
            assert_eq!(operation, "diagnostic_analyze");
            assert_eq!(
                args,
                json!({"reportId":revision,"reportText":expected_text,"confirmationToken":TOKEN})
            );
            Ok(json!({"jobId":JOB,"reportId":revision}))
        },
    )
    .unwrap();
    assert_eq!(started, json!({"jobId":JOB,"reportId":revision}));
    assert_eq!(
        fs::read_to_string(presets).unwrap(),
        "synthetic-config-secret-sentinel"
    );
}

#[test]
fn unavailable_configuration_still_previews_native_text_without_starting_analysis() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, run, _, revision) = failed(&shared);
    let calls = Cell::new(0);
    let preview = preview_with(&shared, &root.id, &run, &revision, |operation, _| {
        calls.set(calls.get() + 1);
        assert_eq!(operation, "diagnostic_preview");
        Ok(json!({"ready":false,"confirmationToken":null,"selectedPreset":null,"warning":null,"maxReportBytes":48000}))
    }).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(preview["configuration"]["ready"], false);
    assert!(preview["report_text"]
        .as_str()
        .unwrap()
        .contains("final native evidence"));
}

#[test]
fn invalid_or_stale_receipts_never_enter_the_bridge() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, run, log, revision) = failed(&shared);
    for invalid in ["".into(), "0".repeat(64), "x".repeat(64)] {
        assert!(
            preview_with(&shared, &root.id, &run, &invalid, |_, _| panic!(
                "Invalid receipt must not call host"
            ))
            .is_err()
        );
        assert!(
            start_with(&shared, &root.id, &run, &invalid, TOKEN, |_, _| panic!(
                "Invalid receipt must not call host"
            ))
            .is_err()
        );
    }
    fs::write(&log, "changed evidence\n").unwrap();
    assert!(
        preview_with(&shared, &root.id, &run, &revision, |_, _| panic!(
            "Stale log must not call host"
        ))
        .is_err()
    );
    assert!(
        start_with(&shared, &root.id, &run, &revision, TOKEN, |_, _| panic!(
            "Stale log must not call host"
        ))
        .is_err()
    );
}

#[test]
fn preview_cannot_publish_after_scope_changes_during_provider_metadata_lookup() {
    for change_root in [false, true] {
        let fixture = Fixture::new();
        let shared = fixture.shared();
        let (root, run, log, _) = failed(&shared);
        let second_path = fixture.0.join("second-root");
        fs::create_dir_all(&second_path).unwrap();
        let second = shared
            .config
            .register(second_path.to_str().unwrap().into(), None)
            .unwrap();
        let revision = launcher_failure::report(&shared, &root.id, &run)
            .unwrap()
            .content_revision;
        let calls = Cell::new(0);
        let result = preview_with(&shared, &root.id, &run, &revision, |_, _| {
            calls.set(calls.get() + 1);
            assert!(shared.operations.try_lock().is_ok());
            if change_root {
                shared.config.select(&second.id).unwrap();
            } else {
                fs::write(&log, "changed during metadata lookup\n").unwrap();
            }
            Ok(json!({"ready":true,"confirmationToken":TOKEN}))
        });
        assert_eq!(calls.get(), 1);
        assert!(result.is_err());
    }
}

#[test]
fn old_jobs_can_be_queried_or_cancelled_after_current_log_and_run_disappear() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (_, _, log, revision) = failed(&shared);
    fs::remove_file(log).unwrap();
    *shared.status.lock().unwrap() = RunStatus::default();
    *shared.log.lock().unwrap() = None;
    for operation in ["diagnostic_status", "diagnostic_cancel"] {
        let result = task_with(&shared, operation, &revision, JOB, |actual, args| {
            assert!(shared.operations.try_lock().is_ok());
            assert_eq!(actual, operation);
            assert_eq!(args, json!({"reportId":revision,"jobId":JOB}));
            Ok(json!({"fixture":true}))
        })
        .unwrap();
        assert_eq!(result, json!({"fixture":true}));
    }
}

#[test]
fn closing_and_malformed_task_or_confirmation_ids_refuse_before_bridge_io() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let (root, run, _, revision) = failed(&shared);
    assert!(start_with(
        &shared,
        &root.id,
        &run,
        &revision,
        "not-a-token",
        |_, _| panic!("Malformed token")
    )
    .is_err());
    for (report_id, job_id) in [("bad", JOB), (&revision, "bad")] {
        assert!(task_with(
            &shared,
            "diagnostic_status",
            report_id,
            job_id,
            |_, _| panic!("Malformed task identity")
        )
        .is_err());
    }
    shared.closing.store(true, Ordering::SeqCst);
    assert!(preview_with(&shared, &root.id, &run, &revision, |_, _| panic!("Closing")).is_err());
    assert!(
        start_with(&shared, &root.id, &run, &revision, TOKEN, |_, _| panic!(
            "Closing"
        ))
        .is_err()
    );
    for operation in ["diagnostic_status", "diagnostic_cancel"] {
        assert!(task_with(&shared, operation, &revision, JOB, |_, _| panic!("Closing")).is_err());
    }
}
