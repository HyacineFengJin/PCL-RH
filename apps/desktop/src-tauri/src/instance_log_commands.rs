//! IPC accepts the explicit root/instance and the exact selected row revisions.
//! The service captures admission briefly and does bounded log I/O outside it.
use crate::{instance_logs as logs, toolbox_images::publish::ExportOutcome, Shared};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn instance_logs(
    root_id: String,
    id: String,
    state: State<'_, Arc<Shared>>,
) -> Result<logs::LogListing, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || logs::list(&shared, &root_id, &id))
        .await
        .map_err(|_| "读取游戏日志列表的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn instance_read_log(
    root_id: String,
    id: String,
    kind: logs::LogKind,
    name: String,
    revision: String,
    listing_revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<logs::LogContent, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        logs::read(
            &shared,
            &root_id,
            &id,
            kind,
            &name,
            &revision,
            &listing_revision,
        )
    })
    .await
    .map_err(|_| "读取所选游戏日志的任务意外退出".to_string())?
}

#[tauri::command]
pub async fn instance_export_log(
    root_id: String,
    id: String,
    kind: logs::LogKind,
    name: String,
    revision: String,
    listing_revision: String,
    content_revision: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<ExportOutcome, String> {
    let shared = state.inner().clone();
    let worker = shared.clone();
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        logs::export::prepare(
            &worker,
            &root_id,
            &id,
            kind,
            &name,
            &revision,
            &listing_revision,
            &content_revision,
        )
    })
    .await
    .map_err(|_| "准备游戏日志导出的任务意外退出".to_string())??;
    let choice = shared
        .desktop
        .choose_named_launcher_file(
            window,
            shared.project.clone(),
            "export_instance_log_text",
            Some(prepared.suggested_name()),
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
        .map_err(|_| "游戏日志导出任务意外退出".to_string())?
}
