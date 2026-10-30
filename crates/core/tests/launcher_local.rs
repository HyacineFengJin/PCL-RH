//! Compile the actual desktop persistence port on every native runner, without
//! initializing Tauri, touching real launcher state or launching applications.
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local.rs"]
mod launcher_local;
