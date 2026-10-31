//! No-Tauri source host for the actual deletion project gate. No deletion
//! service, mutation entrypoint or replacement readiness implementation exists
//! here. These closed host utilities are required by its existing filesystem.
type Result<T> = std::result::Result<T, String>;
const MAX_JOURNAL: usize = 64 * 1024 * 1024;
fn cancelled(cancel: Option<&std::sync::atomic::AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
        Err("实例删除已取消".into())
    } else {
        Ok(())
    }
}
#[allow(dead_code, unused_imports)]
#[path = "filesystem.rs"]
mod filesystem;
#[path = "project_gate.rs"]
mod project_gate;
pub(crate) use project_gate::{ensure_project_ready, pending_root};
