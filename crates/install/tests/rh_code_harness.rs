//! Actual Harness capture, immutable provenance and Linux transaction sources.
//! The bridge supplies data models only; it has no desktop, provider or game.
#![allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
#[path = "../../../apps/desktop/src-tauri/src/rh_code/harness_test_host.rs"]
mod rh_code;
