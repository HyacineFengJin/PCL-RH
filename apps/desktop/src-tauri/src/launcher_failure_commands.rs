//! Explicit frontend failure binding and scheduling. The read-only service owns
//! generation/FD checks and bounded, sanitized analysis outside admission.
use crate::{launcher_failure, RunStatus, Shared};
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
