//! Production detached-monitor lifecycle on native Unix, using this executable
//! as both GUI and synthetic game. No shell, JVM, Tauri or real game is involved.
//! Windows retains its separate GameChild fixtures; its desktop monitor still
//! lacks the pinned-cwd/backend integration exercised by this Unix executable.
#[cfg(unix)]
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor/control.rs"]
mod control_port;
#[cfg(unix)]
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor.rs"]
mod launcher_game_monitor;
#[cfg(unix)]
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local.rs"]
mod launcher_local;
#[cfg(unix)]
#[path = "detached_monitor_lifecycle/unix.rs"]
mod lifecycle;

fn main() {
    #[cfg(unix)]
    {
        // Exactly the production entry-point ordering: monitor dispatch precedes
        // all GUI/fixture setup. Its helper uses the real bounded pipe protocol.
        if let Some(code) = launcher_game_monitor::dispatch_cli() {
            std::process::exit(code);
        }
        if let Some(code) = lifecycle::dispatch_fixture() {
            std::process::exit(code);
        }
        lifecycle::run();
    }
    #[cfg(windows)]
    println!("detached_monitor_lifecycle: Unix-only production-monitor fixture; Windows monitor integration is not asserted (native GameChild fixtures remain separate)");
}
