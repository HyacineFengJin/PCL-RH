//! Anonymous-pipe handoff with a bounded deadline and explicit commit. Helper
//! stdout carries only fixed protocol words and numeric PIDs, never source text.
use super::{
    output::{self, Redactor},
    process,
    state::{self, Session},
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
const REQUEST_LIMIT: usize = 2 * 1024 * 1024;
const HANDSHAKE: Duration = Duration::from_secs(10);
const FAILURE: &str = "独立游戏监控未能完成启动确认，已停止未确认的启动；请刷新进程状态";

/// Deliberately has no Debug or Serialize implementation. This request is
/// internal launch data and must never become a Tauri response or diagnostic.
pub struct MonitorRequest {
    pub project: PathBuf,
    pub root_id: String,
    pub root_path: PathBuf,
    pub instance_id: String,
    pub java: PathBuf,
    pub args: Vec<String>,
    pub game_dir: PathBuf,
    pub log_path: PathBuf,
    pub redactions: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    schema_version: u32,
    project: PathBuf,
    root_id: String,
    root_path: PathBuf,
    instance_id: String,
    java: PathBuf,
    args: Vec<String>,
    game_dir: PathBuf,
    log_path: PathBuf,
    redactions: Vec<String>,
}
impl MonitorRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project: PathBuf,
        root_id: String,
        root_path: PathBuf,
        instance_id: String,
        java: PathBuf,
        args: Vec<String>,
        game_dir: PathBuf,
        log_path: PathBuf,
        redactions: Vec<String>,
    ) -> Result<Self, String> {
        let request = Self {
            project,
            root_id,
            root_path,
            instance_id,
            java,
            args,
            game_dir,
            log_path,
            redactions,
        };
        validate(&request)?;
        Ok(request)
    }
}
fn validate(request: &MonitorRequest) -> Result<(), String> {
    for path in [
        &request.project,
        &request.root_path,
        &request.game_dir,
        &request.java,
    ] {
        if !path.is_absolute()
            || path.to_str().is_none()
            || path.as_os_str().len() > 4096
            || path.components().any(|part| {
                !matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
        {
            return Err("游戏监控启动路径无效".into());
        }
    }
    if !request.game_dir.starts_with(&request.root_path)
        || request.root_id.is_empty()
        || request.root_id.len() > 128
        || request.root_id.chars().any(char::is_control)
        || request.args.len() > 4096
        || request.args.iter().any(|arg| arg.contains('\0'))
        || request.args.iter().map(String::len).sum::<usize>() > 1024 * 1024
    {
        return Err("游戏监控启动参数超出安全范围".into());
    }
    state::validate_log_path(&request.root_path, &request.log_path)?;
    state::validate_instance_id(&request.instance_id)?;
    let _ = Redactor::new(request.redactions.clone())?;
    Ok(())
}
impl From<MonitorRequest> for WireRequest {
    fn from(request: MonitorRequest) -> Self {
        Self {
            schema_version: 2,
            project: request.project,
            root_id: request.root_id,
            root_path: request.root_path,
            instance_id: request.instance_id,
            java: request.java,
            args: request.args,
            game_dir: request.game_dir,
            log_path: request.log_path,
            redactions: request.redactions,
        }
    }
}
impl TryFrom<WireRequest> for MonitorRequest {
    type Error = String;
    fn try_from(wire: WireRequest) -> Result<Self, String> {
        if wire.schema_version != 2 {
            return Err("不支持的游戏监控协议".into());
        }
        Self::new(
            wire.project,
            wire.root_id,
            wire.root_path,
            wire.instance_id,
            wire.java,
            wire.args,
            wire.game_dir,
            wire.log_path,
            wire.redactions,
        )
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorStarted {
    pub session_id: String,
    pub pid: u32,
    pub monitor_pid: u32,
    pub log_path: PathBuf,
}
pub(super) fn nonblocking(fd: i32) -> Result<(), String> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(FAILURE.into());
    }
    Ok(())
}
fn ready(fd: i32, events: i16, deadline: Instant) -> Result<(), String> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(FAILURE.into());
        }
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().min(100) as i32) };
        if result > 0 {
            return Ok(());
        }
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(FAILURE.into());
        }
    }
}
fn write_pipe(fd: i32, mut bytes: &[u8], deadline: Instant) -> Result<(), String> {
    while !bytes.is_empty() {
        ready(fd, libc::POLLOUT, deadline)?;
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &bytes[count as usize..];
        } else if count < 0
            && matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            )
        {
            continue;
        } else {
            return Err(FAILURE.into());
        }
    }
    Ok(())
}
fn read_exact(fd: i32, mut bytes: &mut [u8], deadline: Instant) -> Result<(), String> {
    while !bytes.is_empty() {
        ready(fd, libc::POLLIN, deadline)?;
        let count = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &mut bytes[count as usize..];
        } else if count < 0
            && matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            )
        {
            continue;
        } else {
            return Err(FAILURE.into());
        }
    }
    Ok(())
}
fn read_line(fd: i32, deadline: Instant) -> Result<String, String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        read_exact(fd, &mut byte, deadline)?;
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
        if bytes.len() > 128 {
            return Err(FAILURE.into());
        }
    }
    String::from_utf8(bytes).map_err(|_| FAILURE.into())
}
/// Production always launches the current binary; the caller never selects an
/// external supervisor executable. Only its single fixed mode flag is in argv.
pub fn spawn_monitor(request: MonitorRequest) -> Result<MonitorStarted, String> {
    let executable = std::env::current_exe().map_err(|_| FAILURE)?;
    spawn_with_helper(&executable, request)
}
pub(super) fn spawn_with_helper(
    executable: &Path,
    request: MonitorRequest,
) -> Result<MonitorStarted, String> {
    spawn_with_timeout(executable, request, HANDSHAKE)
}
#[cfg(test)]
pub(super) fn spawn_test_deadline(
    executable: &Path,
    request: MonitorRequest,
    timeout: Duration,
) -> Result<MonitorStarted, String> {
    spawn_with_timeout(executable, request, timeout)
}
fn spawn_with_timeout(
    executable: &Path,
    request: MonitorRequest,
    timeout: Duration,
) -> Result<MonitorStarted, String> {
    validate(&request)?;
    let project = request.project.clone();
    let log_path = request.log_path.clone();
    let bytes = serde_json::to_vec(&WireRequest::from(request)).map_err(|_| FAILURE)?;
    if bytes.len() > REQUEST_LIMIT {
        return Err("游戏监控请求超过安全上限".into());
    }
    let mut command = Command::new(executable);
    command
        .arg("--game-monitor")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().map_err(|_| FAILURE)?;
    let mut pending = Pending {
        child: Some(child),
        game: None,
        project,
    };
    let child = pending.child.as_mut().unwrap();
    let input = child.stdin.take().ok_or(FAILURE)?;
    let output = child.stdout.take().ok_or(FAILURE)?;
    nonblocking(input.as_raw_fd())?;
    nonblocking(output.as_raw_fd())?;
    let deadline = Instant::now() + timeout;
    write_pipe(
        input.as_raw_fd(),
        &(bytes.len() as u32).to_be_bytes(),
        deadline,
    )?;
    write_pipe(input.as_raw_fd(), &bytes, deadline)?;
    // Launch secrets are no longer needed in the parent-side protocol buffer.
    drop(bytes);
    let ready = read_line(output.as_raw_fd(), deadline)?;
    let words = ready.split_whitespace().collect::<Vec<_>>();
    if words.len() != 3 || words[0] != "READY" {
        return Err(FAILURE.into());
    }
    let pid = words[1].parse::<u32>().map_err(|_| FAILURE)?;
    let monitor_pid = words[2].parse::<u32>().map_err(|_| FAILURE)?;
    if monitor_pid != child.id() || pid == 0 || pid == monitor_pid {
        return Err(FAILURE.into());
    }
    pending.game = process::identity(pid)?;
    if pending.game.is_none() {
        return Err(FAILURE.into());
    }
    let supervisor = process::identity(monitor_pid)?.ok_or(FAILURE)?;
    write_pipe(input.as_raw_fd(), b"COMMIT\n", deadline)?;
    if read_line(output.as_raw_fd(), deadline)? != "ACK" {
        return Err(FAILURE.into());
    }
    // After ACK the helper holds the lifetime lock and durable process marker.
    // A reaper does not own cancellation and survives until the GUI exits.
    let mut child = pending.child.take().unwrap();
    drop(input);
    drop(output);
    let _ = std::thread::Builder::new()
        .name("pcl-monitor-reap".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(MonitorStarted {
        session_id: process::session_id(&supervisor),
        pid,
        monitor_pid,
        log_path,
    })
}
struct Pending {
    child: Option<Child>,
    game: Option<process::Identity>,
    project: PathBuf,
}
impl Drop for Pending {
    fn drop(&mut self) {
        let Some(child) = &mut self.child else {
            return;
        };
        // Try to stop the captured game first, then the still-unreaped helper.
        // Java also has PDEATHSIG as a last resort before the PID handshake.
        if let Some(game) = &self.game {
            let _ = process::signal(game, libc::SIGKILL);
        } else if let Ok(Some(status)) = state::read_status(&self.project) {
            if status.monitor_pid == child.id() {
                let _ = state::stop_monitor(&self.project, &status.revision);
            }
        }
        // Closing the pipe lets an uncommitted real helper run its own group
        // cleanup and durable failure update. SIGKILL is the bounded fallback.
        let cleanup = Instant::now() + Duration::from_millis(500);
        while Instant::now() < cleanup {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        process::kill_owned(child);
    }
}
/// Return None for a regular GUI invocation. Every monitor invocation exits
/// without constructing Tauri or reading settings/accounts.
pub fn dispatch_cli() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_none_or(|arg| arg != "--game-monitor") {
        return None;
    }
    if args.len() != 1 {
        return Some(2);
    }
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    Some(if helper().is_ok() { 0 } else { 2 })
}
fn helper() -> Result<(), String> {
    nonblocking(0)?;
    nonblocking(1)?;
    let deadline = Instant::now() + HANDSHAKE;
    let mut length = [0u8; 4];
    read_exact(0, &mut length, deadline)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > REQUEST_LIMIT {
        return Err(FAILURE.into());
    }
    let mut bytes = vec![0; length];
    read_exact(0, &mut bytes, deadline)?;
    let wire: WireRequest = serde_json::from_slice(&bytes).map_err(|_| FAILURE)?;
    bytes.fill(0);
    drop(bytes);
    let request = MonitorRequest::try_from(wire)?;
    // Only validated explicit launch requests can create marker/log directories.
    let mut session = Session::start(
        &request.project,
        request.root_id,
        request.root_path.clone(),
        request.instance_id,
        request.log_path.clone(),
    )?;
    let log = Arc::new(Mutex::new(state::open_launch_log(
        &request.root_path,
        &request.log_path,
    )?));
    let game_directory = crate::launcher_local::filesystem::Dir::absolute(&request.game_dir)?;
    let game_directory_fd = game_directory.0.as_raw_fd();
    let mut command = Command::new(&request.java);
    command
        .args(&request.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let supervisor = std::process::id();
    unsafe {
        command.pre_exec(move || {
            // A validated directory FD is inherited through fork. Reopening the
            // path with Command::current_dir would follow a late symlink swap.
            if libc::fchdir(game_directory_fd) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != supervisor as i32 {
                return Err(io::Error::from_raw_os_error(libc::ECANCELED));
            }
            Ok(())
        });
    }
    let mut game = OwnedGame::new(command.spawn().map_err(|_| FAILURE)?);
    drop(request.args);
    session.spawned(game.0.id())?;
    let stop = Arc::new(AtomicBool::new(false));
    let stdout = game.0.stdout.take().ok_or(FAILURE)?;
    let stderr = game.0.stderr.take().ok_or(FAILURE)?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let output_stop = stop.clone();
    let output_log = log.clone();
    let redactor = Redactor::new(request.redactions.clone())?;
    let stdout = std::thread::Builder::new()
        .name("pcl-game-stdout".into())
        .spawn(move || {
            let _ = output::stream(stdout, output_log, redactor, Some(output_stop));
        })
        .map_err(|_| FAILURE)?;
    let output_stop = stop.clone();
    let output_log = log.clone();
    let redactor = Redactor::new(request.redactions)?;
    let stderr = std::thread::Builder::new()
        .name("pcl-game-stderr".into())
        .spawn(move || {
            let _ = output::stream(stderr, output_log, redactor, Some(output_stop));
        })
        .map_err(|_| FAILURE)?;
    write_pipe(
        1,
        format!("READY {} {}\n", game.0.id(), supervisor).as_bytes(),
        deadline,
    )?;
    if read_line(0, deadline)? != "COMMIT" {
        return Err(FAILURE.into());
    }
    write_pipe(1, b"ACK\n", deadline)?;
    // Parent pipes are no longer used. Java/log ownership stays entirely here.
    let result = process::wait_owned(&mut game.0)?;
    game.1 = true;
    let drain = Instant::now() + Duration::from_secs(2);
    while !(stdout.is_finished() && stderr.is_finished()) && Instant::now() < drain {
        std::thread::sleep(Duration::from_millis(10));
    }
    stop.store(true, Ordering::Release);
    let _ = stdout.join();
    let _ = stderr.join();
    let _ = log.lock().map_err(|_| FAILURE)?.sync_all();
    session.finished(result.success(), result.code())
}
struct OwnedGame(Child, bool);
impl OwnedGame {
    fn new(child: Child) -> Self {
        Self(child, false)
    }
}
impl Drop for OwnedGame {
    fn drop(&mut self) {
        if !self.1 {
            process::kill_owned(&mut self.0);
        }
    }
}
