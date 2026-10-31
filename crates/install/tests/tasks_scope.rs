//! Actual desktop task record ownership, scheduling and platform footprints,
//! compiled without Tauri or an attached user project. All existing scheduler
//! cases run on each host; Windows uses private unprivileged directory aliases.
#![allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/tasks.rs"]
mod tasks;
