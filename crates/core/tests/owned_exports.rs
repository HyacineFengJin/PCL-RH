//! Actual memory exporter for images, resource lists and diagnostic text.
//! No real account, chooser or game runs; output stays in synthetic TempDirs.
#![allow(dead_code)]
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
#[path = "../../../apps/desktop/src-tauri/src/toolbox_images/publish.rs"]
mod publish;
