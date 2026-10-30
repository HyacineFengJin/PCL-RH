//! Process identities and game ownership, independent of the GUI lifetime.
//!
//! The detached monitor, not its GUI parent, must own `GameChild`. Dropping an
//! uncommitted monitor cleans its child; a committed monitor keeps ownership
//! until the game exits. This is lifecycle management, not a filesystem or
//! executable sandbox. Callers retain responsibility for launch admission and
//! directory capabilities. No argument, environment or executable path is put
//! in an error or durable identity.
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, io, path::Path, process::ExitStatus};

#[cfg(unix)]
#[path = "game_process/unix.rs"]
mod owned;
#[cfg(target_os = "linux")]
#[path = "game_process/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "game_process/macos.rs"]
mod platform;
#[cfg(windows)]
#[path = "game_process/windows.rs"]
mod platform;
#[cfg(windows)]
use platform as owned;

/// Compatible with the original Linux monitor marker. The boot UUID prevents
/// a marker from an earlier boot naming a new process with a repeated PID/time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub pid: u32,
    pub start_time: u64,
    pub boot_id: String,
}
impl Identity {
    pub fn valid(&self) -> bool {
        self.pid > 0
            && self.pid <= i32::MAX as u32
            && self.start_time > 0
            && self.boot_id.len() == 36
            && self.boot_id.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    }
}
pub fn identity(pid: u32) -> Result<Option<Identity>, String> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err("监控进程身份无效".into());
    }
    platform::identity(pid)
}
pub fn alive(expected: &Identity) -> Result<bool, String> {
    if !expected.valid() {
        return Err("监控进程身份无效".into());
    }
    Ok(identity(expected.pid)?.as_ref() == Some(expected))
}
/// Stop through a kernel object retained before checking the recorded identity.
/// macOS intentionally has no such API: its GUI must ask the owning monitor to
/// stop its unreaped child, rather than checking a PID then racing `kill(pid)`.
#[cfg(any(target_os = "linux", windows))]
pub fn terminate(expected: &Identity) -> Result<(), String> {
    if !expected.valid() {
        return Err("游戏进程身份无效，未发送停止信号".into());
    }
    platform::terminate(expected)
}

pub struct GameChild(owned::Owner);
pub struct GameOutput(owned::Output);
impl io::Read for GameOutput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.0, bytes)
    }
}
impl GameChild {
    /// Execute the exact image without an implicit command interpreter. The
    /// cwd here is a conventional path, not a directory capability; privileged
    /// callers requiring a pinned cwd must use `spawn_in_directory` on Unix.
    pub fn spawn(program: &Path, args: &[OsString], cwd: &Path) -> Result<Self, String> {
        owned::spawn(program, args, cwd).map(Self)
    }
    #[cfg(unix)]
    pub fn spawn_in_directory(
        program: &Path,
        args: &[OsString],
        cwd: &std::fs::File,
    ) -> Result<Self, String> {
        owned::spawn_in_directory(program, args, cwd).map(Self)
    }
    pub fn id(&self) -> u32 {
        self.0.id()
    }
    pub fn take_stdout(&mut self) -> Option<GameOutput> {
        self.0.take_stdout().map(GameOutput)
    }
    pub fn take_stderr(&mut self) -> Option<GameOutput> {
        self.0.take_stderr().map(GameOutput)
    }
    /// Wait without exposing or reaping the leader before group cleanup. On
    /// Windows the monitor's private Job owns descendants, including children
    /// that detach from the initial process. Unix groups cannot contain a child
    /// that deliberately creates another session; this is not a sandbox.
    pub fn wait(&mut self) -> Result<ExitStatus, String> {
        self.0.wait()
    }
    /// Observe without blocking the monitor's private stop-control loop. This
    /// never reaps a Unix group leader before its descendant cleanup.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, String> {
        self.0.try_wait()
    }
    pub fn terminate(&mut self) -> Result<(), String> {
        self.0.terminate()
    }
}
