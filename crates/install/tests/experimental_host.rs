//! Actual trusted host lease, session and pipe sources, without Tauri or AI.
//! Run `experimental_host::tests` to avoid repeating Code runner's own suite.
#![allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/code_runner.rs"]
mod code_runner;
#[path = "../../../apps/desktop/src-tauri/src/experimental/host_test_host.rs"]
mod experimental_host;
