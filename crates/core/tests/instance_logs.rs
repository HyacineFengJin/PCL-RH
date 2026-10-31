//! Real desktop log binding, bounded reader and named export backend, exercised
//! without Tauri, accounts, a JVM, user roots or a native file chooser.
#![allow(dead_code)]
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod filesystem_impl;
mod launcher_local {
    pub(crate) use crate::filesystem_impl as filesystem;
}
#[path = "../../../apps/desktop/src-tauri/src/instance_logs/model.rs"]
mod model;
use model::{digest, LogKind};
const CHANGED: &str = "游戏日志或实例目录已变化，请刷新列表后重新选择日志";
#[path = "../../../apps/desktop/src-tauri/src/instance_logs/binding.rs"]
mod binding;
#[path = "../../../apps/desktop/src-tauri/src/instance_logs/native_export.rs"]
mod native_export;
#[path = "../../../apps/desktop/src-tauri/src/instance_logs/native_tests.rs"]
mod native_tests;
#[path = "../../../apps/desktop/src-tauri/src/instance_logs/reader.rs"]
mod reader;

#[path = "../../../apps/desktop/src-tauri/src/instance_logs/destination.rs"]
mod destination;
