//! The actual resource-operation/token/publisher implementations without Tauri.
//! The two UI-free boundaries below retain production core scope resolution
//! and the exact production reference-history lock; no readiness stub is used.
#![allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
#[path = "../../../apps/desktop/src-tauri/src/resource_ops.rs"]
mod resource_ops;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
#[path = "../../../apps/desktop/src-tauri/src/instance_rename_refs/filesystem.rs"]
mod reference_filesystem;
mod instance_rename_refs {
    pub(crate) use crate::reference_filesystem::{root_history_lock, ReferenceLock};
}

// Desktop API reexports are intentionally retained at this headless boundary.
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/modrinth_install.rs"]
mod modrinth_install;
#[path = "../../../apps/desktop/src-tauri/src/resource_digest.rs"]
mod resource_digest;
#[path = "../../../apps/desktop/src-tauri/src/tasks.rs"]
mod tasks;
#[path = "../../../apps/desktop/src-tauri/src/ui_data/resource_dir.rs"]
mod ui_data;
