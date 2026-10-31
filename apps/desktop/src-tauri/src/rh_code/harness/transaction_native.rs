//! Source capture/binding are available independently of native application.
//! No two-rename sequence is represented as Linux's per-file atomic exchange.
use super::*;
use crate::rh_code::assets::AssetWrite;
fn unavailable() -> String {
    "HARNESS_APPLY_UNAVAILABLE: Native source publication and recovery are not implemented on this platform; project files are unchanged".into()
}
pub(in crate::rh_code) fn apply(
    _store_root: &Path,
    _project: &Project,
    _args: &Value,
    _export: &Value,
) -> Result<Value, String> {
    // This admission precedes store, source, backup or journal creation.
    Err(unavailable())
}
pub(in crate::rh_code) fn save_assets(
    _store_root: &Path,
    _project: &Project,
    _files: &[AssetWrite],
) -> Result<Value, String> {
    Err(unavailable())
}
pub(in crate::rh_code) fn recover(store_root: &Path, _project: &Project) -> Result<Value, String> {
    let storage = store(store_root)?;
    // Empty state is inspected, not inferred from a missing backend. Any
    // existing transaction, including an orphan, remains available to recovery.
    if let Some(transactions) = storage.optional_child("harness-transactions")? {
        if !transactions.names()?.is_empty() {
            return Err(unavailable());
        }
    }
    Ok(
        json!({"status":"ready","recovered":0,"pending":0,"interrupted":0,"retainedOrphans":0,"backupAvailable":false}),
    )
}
