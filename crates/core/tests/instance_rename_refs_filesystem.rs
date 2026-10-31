//! Actual reference filesystem/optional document/marker implementation. The
//! wrapper supplies no reference business or persistence implementation.
#![allow(dead_code)]
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/instance_rename_refs/filesystem.rs"]
mod filesystem;
