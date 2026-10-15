//! Same-binary detached game supervision. The CLI dispatch must run before any
//! GUI/config/account initialization. Sensitive launch data travels only through
//! the bounded anonymous stdin pipe; persistent state contains process identities
//! and captured root/instance/log identity, never arguments, players or credentials.
//!
//! READY means Java spawned and its durable marker exists. COMMIT/ACK transfers
//! ownership to the helper; before that point every failure kills/reaps Java.
//! Reopening the launcher restores running or completed attempts from the marker;
//! legacy markers keep their root/log binding without inventing an instance ID.
#[path = "launcher_game_monitor/output.rs"]
mod output;
#[path = "launcher_game_monitor/process.rs"]
mod process;
#[path = "launcher_game_monitor/protocol.rs"]
mod protocol;
#[path = "launcher_game_monitor/state.rs"]
mod state;
pub use protocol::{dispatch_cli, spawn_monitor, MonitorRequest};
pub use state::{read_status, retire_completed, stop_monitor};

#[cfg(test)]
#[path = "launcher_game_monitor/tests.rs"]
mod tests;
