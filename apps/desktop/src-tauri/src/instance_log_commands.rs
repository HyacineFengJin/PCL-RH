//! IPC accepts the explicit root/instance and the exact selected row revisions.
//! The service captures admission briefly and does bounded log I/O outside it.
use crate::{instance_logs as logs, Shared};
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
