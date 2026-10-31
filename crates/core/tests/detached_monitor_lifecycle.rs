//! Production detached-monitor lifecycle using this executable as GUI/helper
//! and synthetic game. No shell, JVM, Tauri or real game is involved.
// Exercise real GUI PE inherited pipes even in the debug test profile.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor/control.rs"]
mod control_port;
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor.rs"]
mod launcher_game_monitor;
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local.rs"]
mod launcher_local;
#[cfg(unix)]
#[path = "detached_monitor_lifecycle/unix.rs"]
mod lifecycle;
#[cfg(windows)]
#[path = "detached_monitor_lifecycle/windows.rs"]
mod lifecycle;
#[cfg(windows)]
const REQUEST_LIMIT: usize = 2 * 1024 * 1024;
#[cfg(windows)]
const FAILURE: &str = "synthetic monitor transport failed";
#[cfg(windows)]
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor/protocol/windows.rs"]
mod transport_port;

fn main() {
    {
        // Exactly the production entry-point ordering: monitor dispatch precedes
        // all GUI/fixture setup. Its helper uses the real bounded pipe protocol.
        #[cfg(windows)]
        if let Some(code) = lifecycle::dispatch_transport_fixture() {
            std::process::exit(code);
        }
        if let Some(code) = launcher_game_monitor::dispatch_cli() {
            std::process::exit(code);
        }
        if let Some(code) = lifecycle::dispatch_fixture() {
            std::process::exit(code);
        }
        lifecycle::run();
    }
}
