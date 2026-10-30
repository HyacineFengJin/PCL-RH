//! Durable process identities use the portable core. The GUI's direct stop
//! remains limited to kernels providing an identity-bound process handle;
//! macOS requires the monitor-owned control path before it can be connected.
pub(super) use pcl_core::game_process::{alive, identity, Identity};
use sha2::{Digest, Sha256};
use std::process::Child;
/// Stable for one supervisor lifetime, including across marker phase changes.
/// The GUI can invalidate an older worker without exposing /proc identity data.
pub(super) fn session_id(identity: &Identity) -> String {
    let mut hash = Sha256::new();
    hash.update(identity.pid.to_be_bytes());
    hash.update(identity.start_time.to_be_bytes());
    hash.update(identity.boot_id.as_bytes());
    format!("{:x}", hash.finalize())
}
#[cfg(target_os = "linux")]
pub(super) fn signal(expected: &Identity, signal: i32) -> Result<(), String> {
    if signal != libc::SIGKILL {
        return Err("游戏停止信号不受支持".into());
    }
    pcl_core::game_process::terminate(expected)
}
/// Only an unreaped owned child is used here: its PID cannot be reused while
/// killing the process group. Normal completed children never enter this path.
pub(super) fn kill_owned(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}
