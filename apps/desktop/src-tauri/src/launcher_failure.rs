//! Analysis of one current failed launch, with a native content receipt.
//!
//! Commands submit the root/status the user saw. Capture pins the attempt,
//! monitor log and directory identities; slow reads and redaction run outside
//! operations. Export and AI reuse PreparedDiagnostic, never renderer text.
//! Account files and unrelated game/crash logs are not diagnostic inputs.

#[path = "launcher_failure/export.rs"]
pub(super) mod export;

use crate::{
    config::GameRoot,
    launcher_failure_analysis::FailureFinding,
    launcher_local::filesystem::Dir,
    launcher_logs::{FailureLogCapture, FailureLogTail},
    launcher_monitor_runtime::{self, FailureSnapshot},
    RunStatus, Shared,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{atomic::Ordering, MutexGuard},
};

const CHANGED: &str = "诊断期间启动会话或游戏目录已变化，请重新诊断启动失败";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum FailureLogScope {
    Attempt,
    History,
    None,
}

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
    pub log_scope: FailureLogScope,
    pub content_revision: String,
}

struct CapturedFailure {
    root: GameRoot,
    root_directory: Dir,
    settings_revision: String,
    close_generation: u64,
    tasks_revision: u64,
    failure: FailureSnapshot,
    log: FailureLogCapture,
}

fn ready(shared: &Shared, root: &GameRoot) -> Result<(), String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请重新打开后诊断启动失败".into());
    }
    if shared.tasks.active().is_some() {
        return Err("请等待当前文件任务结束后诊断启动失败".into());
    }
    crate::ensure_instance_files_ready(shared, root)?;
    crate::instance_reset::ensure_ready(Path::new(&root.path))?;
    crate::resource_ops::ensure_ready(Path::new(&root.path))
}

fn capture(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
) -> Result<CapturedFailure, String> {
    let _operation = shared.operations.lock().unwrap();
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
    ready(shared, &root)?;
    let root_directory = Dir::absolute(Path::new(&root.path))?;
    let log = FailureLogCapture::capture(Path::new(&root.path), failure.log.as_deref())?;
    Ok(CapturedFailure {
        root,
        root_directory,
        settings_revision: settings.revision,
        close_generation: shared.close_generation.load(Ordering::SeqCst),
        tasks_revision: shared.tasks.revision(),
        failure,
        log,
    })
}

/// Caller owns operations, held through the short export publication or AI
/// admission gate. Neither log reads nor the native chooser run under this lock.
fn verify_locked(shared: &Shared, captured: &CapturedFailure) -> Result<(), String> {
    if shared.closing.load(Ordering::SeqCst)
        || shared.close_generation.load(Ordering::SeqCst) != captured.close_generation
        || shared.tasks.revision() != captured.tasks_revision
        || shared.config.checked_snapshot()?.revision != captured.settings_revision
        || shared.config.resolve(Some(&captured.root.id))? != captured.root
        || Dir::absolute(Path::new(&captured.root.path))?.identity()?
            != captured.root_directory.identity()?
    {
        return Err(CHANGED.into());
    }
    ready(shared, &captured.root)?;
    launcher_monitor_runtime::verify_failure(shared, &captured.failure, || captured.log.verify())
}

/// Native payload shared by saving and AI. Private fields prevent callers from
/// substituting renderer-supplied content after a successful receipt check.
pub(super) struct PreparedDiagnostic {
    captured: CapturedFailure,
    report: FailureReport,
    text: String,
}
impl PreparedDiagnostic {
    pub(super) fn report(&self) -> &FailureReport {
        &self.report
    }
    pub(super) fn text(&self) -> &str {
        &self.text
    }
    pub(super) fn receipt(&self) -> &str {
        &self.report.content_revision
    }

    pub(super) fn authorize<'a>(&self, shared: &'a Shared) -> Result<MutexGuard<'a, ()>, String> {
        let guard = shared.operations.lock().unwrap();
        verify_locked(shared, &self.captured).map_err(|_| CHANGED.to_string())?;
        // A new account may identify a previously unknown secret. Refresh the
        // displayed report instead of silently changing the accepted payload.
        if shared.accounts.redact(self.text.clone()) != self.text {
            return Err(CHANGED.into());
        }
        Ok(guard)
    }

    pub(super) fn verify(&self, shared: &Shared) -> Result<(), String> {
        self.authorize(shared).map(|_| ())
    }
}

fn digest(value: &impl Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|_| "无法绑定诊断内容")?)
    ))
}

fn content_revision(
    captured: &CapturedFailure,
    report: &FailureReport,
    text: &str,
) -> Result<String, String> {
    // The receipt field starts empty and is excluded explicitly. Every visible
    // fact, including read failure and truncation, participates in this digest.
    digest(&(
        "launch-failure-content-v2",
        &captured.root,
        &captured.settings_revision,
        captured.close_generation,
        captured.tasks_revision,
        captured.failure.generation(),
        captured.failure.log_start,
        &captured.failure.run,
        captured.log.binding_revision()?,
        (
            &report.version,
            report.exit_code,
            &report.message,
            &report.findings,
            &report.log_name,
            &report.log_tail,
            &report.log_error,
            report.log_truncated,
            report.log_scope,
        ),
        text,
    ))
}

fn diagnostic_text(report: &FailureReport, redact: impl Fn(&str) -> String) -> String {
    let mut text = String::from("PCL RH 启动失败诊断报告\n来源：本次启动状态与绑定的监控日志尾部\n预分析只提供线索，不证明具体模组、驱动或修复方案。\n未自动关联其他日志或崩溃报告；脱敏只覆盖启动器已知账号字段，分享前请检查内容。\n\n");
    if let Some(version) = &report.version {
        text.push_str(&format!("实例：{}\n", redact(version)));
    }
    text.push_str(&format!(
        "退出码：{}\n",
        report
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "未提供".into())
    ));
    text.push_str(&format!("启动错误：\n{}\n\n预分析线索：\n", report.message));
    for finding in &report.findings {
        text.push_str(&format!("- {}\n", finding.code));
        for evidence in &finding.evidence {
            text.push_str(&format!("  证据：{evidence}\n"));
        }
    }
    if let Some(name) = &report.log_name {
        text.push_str(&format!("\n监控日志：{name}\n"));
    }
    text.push_str(match report.log_scope {
        FailureLogScope::Attempt => "日志范围：本次启动\n",
        FailureLogScope::History => {
            "日志范围：历史日志；缺少本次启动边界，以下日志未用于本次预分析，不证明本次失败原因。\n"
        }
        FailureLogScope::None => "日志范围：没有可读取的日志文本\n",
    });
    text.push_str(if report.log_truncated {
        "日志已截断：是（仅包含有界尾部）\n"
    } else {
        "日志已截断：否\n"
    });
    if let Some(error) = &report.log_error {
        text.push_str(&format!("日志读取错误：{error}\n"));
    }
    if let Some(tail) = &report.log_tail {
        let heading = if report.log_scope == FailureLogScope::History {
            "历史日志尾部（仅供查看，不属于本次失败证据）"
        } else {
            "监控日志尾部"
        };
        text.push_str(&format!("\n{heading}：\n{tail}\n"));
    }
    text
}

fn prepare_read_with(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
    read: impl FnOnce(&mut FailureLogCapture, Option<u64>) -> Result<FailureLogTail, String>,
    redact: impl Fn(&str) -> String,
) -> Result<PreparedDiagnostic, String> {
    let analyzer = shared
        .product_plugins
        .optional_service(crate::product_plugins::contracts::FAILURE_ANALYSIS)?;
    let mut captured = capture(shared, root_id, expected)?;
    let (log_tail, log_error, log_truncated) =
        match read(&mut captured.log, captured.failure.log_start) {
            Ok(tail) => (Some(redact(&tail.text)), None, tail.truncated),
            Err(error) => (None, Some(redact(&error)), false),
        };
    let log_scope = match log_tail.as_deref() {
        None | Some("") => FailureLogScope::None,
        Some(_) if captured.failure.log_start.is_some() => FailureLogScope::Attempt,
        Some(_) => FailureLogScope::History,
    };
    let message = redact(&captured.failure.run.message);
    // A legacy file can contain earlier attempts. Keep it available for the
    // user to inspect, but never elevate its old errors into current findings.
    // The report and its log/export receipt remain usable without the optional
    // analysis provider. A failed provider still propagates an explicit error.
    let findings = analyzer
        .as_ref()
        .map(|analyzer| {
            analyzer.0.analyze(
                &message,
                (log_scope == FailureLogScope::Attempt)
                    .then_some(log_tail.as_deref())
                    .flatten(),
            )
        })
        .unwrap_or_default();
    let log_name = captured.log.name.as_deref().and_then(|name| {
        let safe = redact(name);
        (safe == name).then_some(safe)
    });
    let mut report = FailureReport {
        version: captured.failure.run.version.clone(),
        exit_code: captured.failure.run.exit_code,
        message,
        findings,
        log_name,
        log_tail,
        log_error,
        log_truncated,
        log_scope,
        content_revision: String::new(),
    };
    let text = diagnostic_text(&report, redact);
    report.content_revision = content_revision(&captured, &report, &text)?;
    let prepared = PreparedDiagnostic {
        captured,
        report,
        text,
    };
    // Even read errors can belong to a superseded launch. Verify before either
    // a report or an export preparation may leave the service.
    prepared.verify(shared)?;
    Ok(prepared)
}

fn report_with(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
    read: impl FnOnce(&mut FailureLogCapture, Option<u64>) -> Result<FailureLogTail, String>,
    redact: impl Fn(&str) -> String,
) -> Result<FailureReport, String> {
    prepare_read_with(shared, root_id, expected, read, redact).map(|prepared| prepared.report)
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
        FailureLogCapture::read_tail_from,
        |text| shared.accounts.redact(text.to_owned()),
    )
}

/// Rebuild exactly the native facts the user accepted. This consumes only a
/// receipt from the renderer; diagnostic bytes are always read natively.
pub(super) fn prepare(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
    revision: &str,
) -> Result<PreparedDiagnostic, String> {
    prepare_with(shared, root_id, expected, revision, |text| {
        shared.accounts.redact(text.to_owned())
    })
}

fn prepare_with(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
    revision: &str,
    redact: impl Fn(&str) -> String,
) -> Result<PreparedDiagnostic, String> {
    if revision.len() != 64 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CHANGED.into());
    }
    let prepared = prepare_read_with(
        shared,
        root_id,
        expected,
        FailureLogCapture::read_tail_from,
        redact,
    )?;
    if prepared.receipt() != revision {
        return Err(CHANGED.into());
    }
    Ok(prepared)
}

#[cfg(test)]
#[path = "launcher_failure/tests.rs"]
mod tests;
