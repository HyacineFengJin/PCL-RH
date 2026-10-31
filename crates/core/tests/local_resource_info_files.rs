//! Actual read-only resource bindings and the production panel token format.
#![allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
#[path = "../../../apps/desktop/src-tauri/src/local_resource_info/files.rs"]
mod local_resource_info_files;
#[path = "../../../apps/desktop/src-tauri/src/resource_ops/token.rs"]
mod resource_ops;
