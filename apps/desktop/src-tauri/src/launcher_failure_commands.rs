//! Explicit frontend failure binding and scheduling. The read-only service owns
//! generation/FD checks and bounded, sanitized analysis outside admission.
use crate::{launcher_failure, toolbox_images::publish::ExportOutcome, RunStatus, Shared};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn launch_failure_report(
    root_id: String,
    expected: RunStatus,
    state: State<'_, Arc<Shared>>,
) -> Result<launcher_failure::FailureReport, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        launcher_failure::report(&shared, &root_id, &expected)
    })
    .await
    .map_err(|_| "诊断启动失败的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn launch_failure_export(
    root_id: String,
    expected: RunStatus,
    revision: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<ExportOutcome, String> {
    let shared = state.inner().clone();
    let worker = shared.clone();
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        launcher_failure::prepare(&worker, &root_id, &expected, &revision)
    })
    .await
    .map_err(|_| "准备启动失败报告导出的任务意外退出".to_string())??;
    let choice = shared
        .desktop
        .choose_named_launcher_file(
            window,
            shared.project.clone(),
            "export_launch_failure_report",
            Some(prepared.suggested_name().into()),
        )
        .await?;
    if choice.status == "cancelled" {
        return Ok(ExportOutcome::cancelled());
    }
    let target = choice.paths.into_iter().next().ok_or_else(|| {
        choice
            .message
            .unwrap_or_else(|| "未选择有效的文本保存位置".into())
    })?;
    tauri::async_runtime::spawn_blocking(move || prepared.publish(&shared, &target))
        .await
        .map_err(|_| "启动失败报告导出任务意外退出".to_string())?
}
