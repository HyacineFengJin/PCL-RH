//! The repair command accepts only a registered root, instance and settings
//! revision. Download authority is derived by the engine from pinned metadata.

use crate::{launcher_damage, launcher_integrity, launcher_repair, Shared};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub fn instance_repair_start(
    id: String,
    root_id: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let id = launcher_repair::start(state.inner().clone(), &id, &root_id, &revision)?;
    Ok(serde_json::json!({ "id": id }))
}

#[tauri::command]
pub fn instance_verify_start(
    id: String,
    root_id: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let id = launcher_integrity::start(state.inner().clone(), &id, &root_id, &revision)?;
    Ok(serde_json::json!({ "id": id }))
}

#[tauri::command]
pub fn instance_damage_repair_start(
    id: String,
    root_id: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let id = launcher_damage::start(state.inner().clone(), &id, &root_id, &revision)?;
    Ok(serde_json::json!({ "id": id }))
}

#[tauri::command]
pub fn instance_damage_recover(
    root_id: String,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let id = launcher_damage::recover(state.inner().clone(), &root_id, &revision)?;
    Ok(serde_json::json!({ "id": id }))
}
