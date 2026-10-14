//! Read-only analysis of one current failed launch.
//!
//! Commands submit the root and status the user actually saw. Admission binds
//! that attempt's monitor generation, log FD and directory identities; bounded
//! reads and account redaction happen outside operations. Publication verifies
//! the entire binding again, including an intervening launch that failed with
//! the same visible status. Account files, credentials and arbitrary game logs
//! are never diagnostic inputs.

use crate::{
    config::GameRoot,
    launcher_failure_analysis::{self, FailureFinding},
    launcher_logs::{FailureLogCapture, FailureLogTail},
    launcher_monitor_runtime::{self, FailureSnapshot},
    RunStatus, Shared,
};
use serde::Serialize;
use std::{path::Path, sync::atomic::Ordering};

const CHANGED: &str = "诊断期间启动会话或游戏目录已变化，请重新诊断启动失败";

#[derive(Serialize)]
pub(super) struct FailureReport {
    pub version: Option<String>,
    pub exit_code: Option<i32>,
    pub message: String,
    pub findings: Vec<FailureFinding>,
    pub log_name: Option<String>,
    pub log_tail: Option<String>,
    pub log_error: Option<String>,
    pub log_truncated: bool,
}

struct CapturedFailure {
    root: GameRoot,
    settings_revision: String,
    close_generation: u64,
    failure: FailureSnapshot,
    log: FailureLogCapture,
}

fn capture(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
) -> Result<CapturedFailure, String> {
    let _operation = shared.operations.lock().unwrap();
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请重新打开后诊断启动失败".into());
    }
    let settings = shared.config.checked_snapshot()?;
    let failure = launcher_monitor_runtime::failure_snapshot(shared)?;
    if failure.run != *expected || settings.root_id != root_id {
        return Err(CHANGED.into());
    }
    let root = shared.config.resolve(Some(root_id))?;
    if failure.run.root_id.as_deref() != Some(root.id.as_str())
        || failure.run.root_path.as_deref() != Some(root.path.as_str())
    {
        return Err("本次启动失败不属于所选游戏目录，请刷新启动状态".into());
    }
    let log = FailureLogCapture::capture(Path::new(&root.path), failure.log.as_deref())?;
    Ok(CapturedFailure {
        root,
        settings_revision: settings.revision,
        close_generation: shared.close_generation.load(Ordering::SeqCst),
        failure,
        log,
    })
}

fn verify(shared: &Shared, captured: &CapturedFailure) -> Result<(), String> {
    let _operation = shared.operations.lock().unwrap();
    if shared.closing.load(Ordering::SeqCst)
        || shared.close_generation.load(Ordering::SeqCst) != captured.close_generation
        || shared.config.checked_snapshot()?.revision != captured.settings_revision
        || shared.config.resolve(Some(&captured.root.id))? != captured.root
    {
        return Err(CHANGED.into());
    }
    launcher_monitor_runtime::verify_failure(shared, &captured.failure, || captured.log.verify())
        .map_err(|_| CHANGED.to_string())
}

fn report_with(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
    read: impl FnOnce(&mut FailureLogCapture) -> Result<FailureLogTail, String>,
    redact: impl Fn(&str) -> String,
) -> Result<FailureReport, String> {
    let mut captured = capture(shared, root_id, expected)?;
    let (log_tail, log_error, log_truncated) = match read(&mut captured.log) {
        Ok(tail) => (Some(redact(&tail.text)), None, tail.truncated),
        Err(error) => (None, Some(redact(&error)), false),
    };
    let message = redact(&captured.failure.run.message);
    // Only sanitized diagnostic text enters the classifier or the report.
    let findings = launcher_failure_analysis::analyze(&message, log_tail.as_deref());
    // A sanitized filename is no longer the exact file selection. Hide that
    // optional entry rather than disclosing a secret or naming a different log.
    let log_name = captured.log.name.as_deref().and_then(|name| {
        let safe = redact(name);
        (safe == name).then_some(safe)
    });
    let report = FailureReport {
        // Instance identity must still equal the submitted status so callers
        // can retire stale actions. It is not an input to text classification.
        version: captured.failure.run.version.clone(),
        exit_code: captured.failure.run.exit_code,
        message,
        findings,
        log_name,
        log_tail,
        log_error,
        log_truncated,
    };
    // Even an I/O failure can describe a superseded attempt. Verify before
    // publishing either its log text or its error, after all slow work finishes.
    verify(shared, &captured).map_err(|_| CHANGED.to_string())?;
    Ok(report)
}

pub(super) fn report(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
) -> Result<FailureReport, String> {
    report_with(
        shared,
        root_id,
        expected,
        FailureLogCapture::read_tail,
        |text| shared.accounts.redact(text.to_owned()),
    )
}

#[cfg(test)]
#[path = "launcher_failure/tests.rs"]
mod tests;
