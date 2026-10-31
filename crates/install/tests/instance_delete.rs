//! Actual recoverable Delete service, references and native persistence engines.
//! Run `cargo test -p pcl-install --test instance_delete instance_delete::`.
//! Dependencies below are production sources, never ready/transform stubs; their
//! own suites run in the primary rename bundle, avoiding duplicate MAX512 IO.
//! All business transformations and project admission gates are production code;
//! fixtures never require Tauri, a chooser, account or real game/JVM process.
#![allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/config.rs"]
mod config;
#[path = "../../../apps/desktop/src-tauri/src/export_presets.rs"]
mod export_presets;
#[path = "../../../apps/desktop/src-tauri/src/instance_delete.rs"]
mod instance_delete;
#[path = "../../../apps/desktop/src-tauri/src/instance_export.rs"]
mod instance_export;
#[path = "../../../apps/desktop/src-tauri/src/instance_meta.rs"]
mod instance_meta;
#[path = "../../../apps/desktop/src-tauri/src/instance_rename.rs"]
mod instance_rename;
#[path = "../../../apps/desktop/src-tauri/src/instance_rename_refs.rs"]
mod instance_rename_refs;
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
#[path = "../../../apps/desktop/src-tauri/src/modrinth_install.rs"]
#[allow(unused_imports)]
mod modrinth_install;
#[path = "../../../apps/desktop/src-tauri/src/resource_digest.rs"]
mod resource_digest;
#[path = "../../../apps/desktop/src-tauri/src/resource_ops.rs"]
mod resource_ops;
#[path = "../../../apps/desktop/src-tauri/src/tasks.rs"]
mod tasks;
#[path = "../../../apps/desktop/src-tauri/src/ui_data/resource_dir.rs"]
mod ui_data;
