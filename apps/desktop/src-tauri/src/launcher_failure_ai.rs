//! Native ownership for explicit failure-report AI requests. Preview and start
//! rebuild the accepted receipt and the same bounded text; renderer arguments
//! never supply report bytes, provider URLs, credentials or tools. Operations is
//! held only by short verification calls, never while calling the Node host.
use crate::{
    experimental::Host,
    launcher_failure::{self, FailureLogScope, PreparedDiagnostic},
    RunStatus, Shared,
};
use serde_json::{json, Value};
use std::sync::{atomic::Ordering, Arc};
use tauri::State;

const REPORT_BYTES: usize = 48_000;
const INPUT_BYTES: usize = 64_000;
const HEAD_BYTES: usize = 16_000;
const EXCERPT_GAP: &str = "\n\n[AI 报告摘录：原生报告中段已省略，下面继续保留报告末尾。]\n\n";
const EXCERPT_END: &str = "\n[AI 摘录结束：仅包含有界开头与末尾，请勿视为完整原始日志。]\n";

struct AiPayload {
    report_id: String,
    text: String,
    truncated: bool,
}

fn prefix(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn suffix(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn input_size(report_id: &str, text: &str) -> usize {
    serde_json::to_vec(&json!({
        "diagnostic":{"schemaVersion":1,"reportId":report_id},"reportText":text
    }))
    .expect("Diagnostic payload contains only JSON strings")
    .len()
}

fn maximum(mut high: usize, mut fits: impl FnMut(usize) -> bool) -> usize {
    let mut low = 0;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if fits(middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

fn payload_text(
    report_id: &str,
    text: &str,
    scope: FailureLogScope,
    log_truncated: bool,
) -> AiPayload {
    if text.len() <= REPORT_BYTES && input_size(report_id, text) <= INPUT_BYTES {
        return AiPayload {
            report_id: report_id.into(),
            text: text.into(),
            truncated: false,
        };
    }
    // A large message can move the original scope warning into the omitted
    // middle. Repeat it outside the excerpts so legacy history never becomes
    // evidence for the current launch merely because the payload was bounded.
    let scope = match scope {
        FailureLogScope::Attempt => "本次启动",
        FailureLogScope::History => "历史日志；缺少本次启动边界，不属于本次失败证据",
        FailureLogScope::None => "没有可读取的日志文本",
    };
    let opening = format!(
        "[AI 报告摘录：发送上限要求省略原生报告中段，以下保留原生报告开头与末尾。完整报告可另行导出。]\n日志范围：{scope}\n原始日志已截断：{}\n\n",
        if log_truncated { "是" } else { "否" }
    );
    let assemble =
        |head: &str, tail: &str| format!("{opening}{head}{EXCERPT_GAP}{tail}{EXCERPT_END}");
    // Reserve half the encoded budget for the tail when unusual control bytes
    // expand to six JSON bytes. Ordinary logs retain approximately 16 KB ahead.
    let head_bytes = maximum(HEAD_BYTES.min(text.len()), |bytes| {
        input_size(report_id, &assemble(prefix(text, bytes), "")) <= INPUT_BYTES / 2
    });
    let head = prefix(text, head_bytes);
    let tail_bytes = maximum(
        text.len().saturating_sub(head.len()).min(REPORT_BYTES),
        |bytes| {
            let candidate = assemble(head, suffix(text, bytes));
            candidate.len() <= REPORT_BYTES && input_size(report_id, &candidate) <= INPUT_BYTES
        },
    );
    AiPayload {
        report_id: report_id.into(),
        text: assemble(head, suffix(text, tail_bytes)),
        truncated: true,
    }
}

fn payload(prepared: &PreparedDiagnostic) -> AiPayload {
    payload_text(
        prepared.receipt(),
        prepared.text(),
        prepared.report().log_scope,
        prepared.report().log_truncated,
    )
}

fn open(shared: &Shared) -> Result<(), String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，无法处理 AI 诊断".into());
    }
    Ok(())
}

fn preview_with(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
    revision: &str,
    call: impl FnOnce(&str, Value) -> Result<Value, String>,
) -> Result<Value, String> {
    let result = (|| {
        open(shared)?;
        let prepared = launcher_failure::prepare(shared, root_id, expected, revision)?;
        let payload = payload(&prepared);
        prepared.verify(shared)?;
        let configuration = call("diagnostic_preview", json!({"reportId":payload.report_id}))?;
        // Provider metadata may take time to load. Retire the complete preview
        // if the report changes before the exact text leaves native ownership.
        prepared.verify(shared)?;
        Ok(
            json!({"report_id":payload.report_id,"report_text":payload.text,
            "report_truncated":payload.truncated,"configuration":configuration}),
        )
    })();
    result.map_err(|error| shared.accounts.redact(error))
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn start_with(
    shared: &Shared,
    root_id: &str,
    expected: &RunStatus,
    revision: &str,
    confirmation_token: &str,
    call: impl FnOnce(&str, Value) -> Result<Value, String>,
) -> Result<Value, String> {
    let result = (|| {
        open(shared)?;
        if !uuid(confirmation_token) {
            return Err("AI 诊断确认已失效，请重新预览".into());
        }
        let prepared = launcher_failure::prepare(shared, root_id, expected, revision)?;
        let payload = payload(&prepared);
        // The lock drops before host I/O. The immutable report and one-use
        // confirmation remain bound to this explicit click if the UI changes.
        prepared.verify(shared)?;
        call(
            "diagnostic_analyze",
            json!({"reportId":payload.report_id,"reportText":payload.text,
            "confirmationToken":confirmation_token}),
        )
    })();
    result.map_err(|error| shared.accounts.redact(error))
}

fn task_with(
    shared: &Shared,
    operation: &str,
    report_id: &str,
    job_id: &str,
    call: impl FnOnce(&str, Value) -> Result<Value, String>,
) -> Result<Value, String> {
    let result = (|| {
        open(shared)?;
        if report_id.len() != 64
            || !report_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !uuid(job_id)
        {
            return Err("AI 诊断报告或任务标识无效".into());
        }
        // No new log read: old jobs remain inspectable and cancellable after a
        // root switch, new attempt or append. Node checks profile plus report ID.
        call(operation, json!({"reportId":report_id,"jobId":job_id}))
    })();
    result.map_err(|error| shared.accounts.redact(error))
}

#[tauri::command]
pub async fn launch_failure_ai_preview(
    root_id: String,
    expected: RunStatus,
    revision: String,
    state: State<'_, Arc<Shared>>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    let host = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        preview_with(
            &shared,
            &root_id,
            &expected,
            &revision,
            |operation, args| host.diagnostic_call(operation, args),
        )
    })
    .await
    .map_err(|_| "准备 AI 诊断预览的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn launch_failure_ai_start(
    root_id: String,
    expected: RunStatus,
    revision: String,
    confirmation_token: String,
    state: State<'_, Arc<Shared>>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    let host = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        start_with(
            &shared,
            &root_id,
            &expected,
            &revision,
            &confirmation_token,
            |operation, args| host.diagnostic_call(operation, args),
        )
    })
    .await
    .map_err(|_| "开始 AI 诊断的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn launch_failure_ai_status(
    report_id: String,
    job_id: String,
    state: State<'_, Arc<Shared>>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    task_command("diagnostic_status", report_id, job_id, state, host).await
}

#[tauri::command]
pub async fn launch_failure_ai_cancel(
    report_id: String,
    job_id: String,
    state: State<'_, Arc<Shared>>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    task_command("diagnostic_cancel", report_id, job_id, state, host).await
}

async fn task_command(
    operation: &'static str,
    report_id: String,
    job_id: String,
    state: State<'_, Arc<Shared>>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let shared = state.inner().clone();
    let host = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        task_with(
            &shared,
            operation,
            &report_id,
            &job_id,
            |operation, args| host.diagnostic_call(operation, args),
        )
    })
    .await
    .map_err(|_| "读取或取消 AI 诊断的任务意外退出".to_string())?
}

#[cfg(test)]
#[path = "launcher_failure_ai/tests.rs"]
mod tests;
