//! Actual reset state machine, native document engine and registered sources.
//! Only private TempDirs and generated fixture bytes; no GUI/JVM/downloads.
#![allow(dead_code)]
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    #[allow(unused_imports)]
    pub(crate) use crate::local_filesystem as filesystem;
}
#[path = "../../../apps/desktop/src-tauri/src/instance_reset.rs"]
mod instance_reset;
