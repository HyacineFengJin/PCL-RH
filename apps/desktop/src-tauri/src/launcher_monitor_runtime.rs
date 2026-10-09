//! Coordinate GUI state with the detached supervisor. Lock order is application
//! admission (`operations`), then this session mutex, then `status`/`log`. Marker
//! reads and adoption share the session lock with beginning a launch, so an old
//! terminal read cannot overwrite a later preparation. Workers carry a launch
//! generation; their status, stop and window effects expire with that generation.
use crate::{launcher_game_monitor, RunStatus, Shared};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Mutex, MutexGuard},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation(u64);
#[derive(Default)]
pub struct MonitorRuntime {
    sync: Mutex<Session>,
}
#[derive(Default)]
struct Session {
    generation: u64,
    attached: bool,
    stop_requested: bool,
    supervisor: Option<String>,
}
impl MonitorRuntime {
    fn lock(&self) -> MutexGuard<'_, Session> {
        self.sync.lock().unwrap_or_else(|error| error.into_inner())
    }
}
// The coordinator owns no process identities itself. Every stop rereads the
// durable marker and delegates PID/starttime/boot identity validation to service.
struct Observation {
    session_id: String,
    busy: bool,
    game_running: bool,
    pid: Option<u32>,
    monitor_pid: u32,
    root_id: String,
    root_path: String,
    log_path: String,
    exit_code: Option<i32>,
    revision: String,
}
fn observe(shared: &Shared) -> Result<Option<Observation>, String> {
    Ok(
        launcher_game_monitor::read_status(&shared.project)?.map(|monitor| Observation {
            session_id: monitor.session_id,
            busy: monitor.busy,
            game_running: monitor.game_running,
            pid: monitor.pid,
            monitor_pid: monitor.monitor_pid,
            root_id: monitor.root_id,
            root_path: monitor.root_path,
            log_path: monitor.log_path,
            exit_code: monitor.exit_code,
            revision: monitor.revision,
        }),
    )
}
fn refresh_locked(shared: &Shared, session: &mut Session) -> Result<(), String> {
    refresh_from(shared, session, || observe(shared))
}
fn refresh_from(
    shared: &Shared,
    session: &mut Session,
    read: impl FnOnce() -> Result<Option<Observation>, String>,
) -> Result<(), String> {
    if !session.attached
        && matches!(
            shared.status.lock().unwrap().stage.as_str(),
            "preparing" | "running"
        )
    {
        return Ok(()); // This generation has not published its helper handshake.
    }
    match read() {
        Ok(Some(monitor)) if monitor.busy || session.attached => {
            let replaced = session.supervisor.as_deref() != Some(monitor.session_id.as_str());
            if replaced {
                // Another GUI may launch the next helper before this GUI reads
                // the preceding terminal marker. Its identity is a new generation.
                session.generation = session
                    .generation
                    .checked_add(1)
                    .ok_or("启动代次已达上限，请重新打开启动器")?;
                session.stop_requested = false;
                shared.stop.store(false, Ordering::SeqCst);
            }
            session.supervisor = Some(monitor.session_id.clone());
            let stopped = !monitor.busy && session.stop_requested;
            if !monitor.busy {
                session.stop_requested = false;
            }
            let mut run = shared.status.lock().unwrap();
            let version = if !replaced && run.root_id.as_deref() == Some(&monitor.root_id) {
                run.version.clone()
            } else {
                None
            };
            *run = RunStatus {
                stage: if monitor.busy {
                    "running"
                } else if stopped || monitor.exit_code == Some(0) {
                    "exited"
                } else {
                    "error"
                }
                .into(),
                message: if monitor.busy {
                    "游戏由独立监控进程运行".into()
                } else if stopped {
                    "游戏已停止".into()
                } else {
                    format!(
                        "独立监控的游戏已退出（{}）",
                        monitor
                            .exit_code
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "信号或监控中断".into())
                    )
                },
                version,
                pid: if monitor.game_running {
                    monitor.pid
                } else {
                    None
                },
                exit_code: monitor.exit_code,
                root_id: Some(monitor.root_id),
                root_path: Some(monitor.root_path),
            };
            drop(run);
            *shared.log.lock().unwrap() = Some(PathBuf::from(monitor.log_path));
            session.attached = monitor.busy;
            Ok(())
        }
        Ok(None) if session.attached => fail_locked(
            shared,
            session,
            "独立监控记录消失，请检查运行中的游戏；暂不允许启动或修改实例".into(),
        ),
        Ok(_) => Ok(()),
        Err(error) => fail_locked(shared, session, error),
    }
}
fn fail_locked(shared: &Shared, session: &mut Session, error: String) -> Result<(), String> {
    let mut run = shared.status.lock().unwrap();
    run.stage = "error".into();
    run.message = error.clone();
    run.pid = None;
    run.exit_code = None;
    session.attached = true; // Missing/corrupt state is not evidence Java exited.
    Err(error)
}
pub fn refresh(shared: &Shared) -> Result<(), String> {
    refresh_locked(shared, &mut shared.monitor.lock())
}
pub fn require_idle(shared: &Shared) -> Result<(), String> {
    let mut session = shared.monitor.lock();
    refresh_locked(shared, &mut session)?;
    if session.attached {
        return Err("独立监控的游戏仍在运行，请结束游戏后操作".into());
    }
    Ok(())
}
pub fn require_root_unused(shared: &Shared, root_id: &str) -> Result<(), String> {
    let mut session = shared.monitor.lock();
    refresh_locked(shared, &mut session)?;
    if session.attached && shared.status.lock().unwrap().root_id.as_deref() == Some(root_id) {
        return Err("该目录正由独立游戏监控使用，请结束游戏后移除".into());
    }
    Ok(())
}
/// Caller holds operations. Account edits take status while starting auth, so
/// their admission check and publication of `preparing` share that same lock.
pub fn begin_preparation(shared: &Shared, next: RunStatus) -> Result<Generation, String> {
    let mut session = shared.monitor.lock();
    refresh_locked(shared, &mut session)?;
    if session.attached {
        return Err("已有独立监控的游戏正在运行".into());
    }
    let mut run = shared.status.lock().unwrap();
    if shared.tasks.active().is_some() {
        return Err("请在文件操作结束后启动游戏".into());
    }
    if matches!(run.stage.as_str(), "preparing" | "running") {
        return Err("已有启动任务或游戏正在运行".into());
    }
    if shared.accounts.busy() {
        return Err("请先完成或取消微软登录".into());
    }
    session.generation = session
        .generation
        .checked_add(1)
        .ok_or("启动代次已达上限，请重新打开启动器")?;
    session.stop_requested = false;
    session.supervisor = None;
    shared.stop.store(false, Ordering::SeqCst);
    *run = next;
    Ok(Generation(session.generation))
}
pub fn update_current(
    shared: &Shared,
    generation: Generation,
    stage: &str,
    message: String,
    pid: Option<u32>,
    exit_code: Option<i32>,
) {
    with_current(shared, generation, || {
        let mut run = shared.status.lock().unwrap();
        run.stage = stage.into();
        run.message = message;
        run.pid = pid;
        run.exit_code = exit_code;
    });
}
pub fn with_current<T>(
    shared: &Shared,
    generation: Generation,
    action: impl FnOnce() -> T,
) -> Option<T> {
    let session = shared.monitor.lock();
    (session.generation == generation.0).then(action)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    Ready,
    Cancelled,
    Superseded,
}
fn stop_locked(
    session: &mut Session,
    signal: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    // Refresh cannot observe the terminal marker between signal and intent.
    // A failed retry preserves an earlier successful stop's intent.
    signal()?;
    session.stop_requested = true;
    Ok(())
}
fn stop_running(shared: &Shared, session: &mut Session) -> Result<(), String> {
    let monitor = observe(shared)?.ok_or("独立监控状态不存在，请刷新后检查")?;
    if session
        .supervisor
        .as_deref()
        .is_some_and(|id| id != monitor.session_id)
    {
        refresh_from(shared, session, || Ok(Some(monitor)))?;
        return Err("运行会话已变化，请刷新后停止".into());
    }
    if !monitor.game_running {
        return Ok(());
    }
    stop_locked(session, || {
        launcher_game_monitor::stop_monitor(&shared.project, &monitor.revision)
    })
}
pub fn request_stop(shared: &Shared) -> Result<(), String> {
    let mut session = shared.monitor.lock();
    refresh_locked(shared, &mut session)?;
    if session.attached {
        stop_running(shared, &mut session)
    } else {
        shared.stop.store(true, Ordering::SeqCst);
        Ok(())
    }
}
fn adopt_with(
    shared: &Shared,
    generation: Generation,
    pid: u32,
    monitor_pid: u32,
    supervisor: String,
    log: PathBuf,
    stop: impl FnOnce(&mut Session) -> Result<(), String>,
) -> Result<Handoff, String> {
    let mut session = shared.monitor.lock();
    if session.generation != generation.0 {
        // A superseded worker may stop only its own freshly acknowledged helper.
        // A newer marker's identity is never used to cancel the older launch.
        if let Some(monitor) = observe(shared)? {
            if monitor.session_id == supervisor
                && monitor.pid == Some(pid)
                && monitor.monitor_pid == monitor_pid
                && monitor.game_running
            {
                launcher_game_monitor::stop_monitor(&shared.project, &monitor.revision)?;
            }
        }
        return Ok(Handoff::Superseded);
    }
    session.attached = true;
    session.supervisor = Some(supervisor);
    *shared.log.lock().unwrap() = Some(log);
    {
        let mut run = shared.status.lock().unwrap();
        run.stage = "running".into();
        run.message = "游戏进程已启动，正在加载".into();
        run.pid = Some(pid);
        run.exit_code = None;
    }
    if shared.stop.swap(false, Ordering::SeqCst) {
        stop(&mut session)?;
        return Ok(Handoff::Cancelled);
    }
    Ok(Handoff::Ready)
}
/// Called immediately after successful ACK, before any window hide/close.
pub fn adopt_spawned(
    shared: &Shared,
    generation: Generation,
    pid: u32,
    monitor_pid: u32,
    supervisor: String,
    log: PathBuf,
) -> Result<Handoff, String> {
    adopt_with(
        shared,
        generation,
        pid,
        monitor_pid,
        supervisor,
        log,
        |session| stop_running(shared, session),
    )
}
/// Window effects run while generation and stop intent are pinned. A stop that
/// arrives between ACK adoption and this call suppresses automatic hide/close.
pub fn with_spawn_visibility<T>(
    shared: &Shared,
    generation: Generation,
    action: impl FnOnce() -> T,
) -> Result<Option<T>, String> {
    let mut session = shared.monitor.lock();
    if session.generation != generation.0 || session.stop_requested {
        return Ok(None);
    }
    if shared.stop.swap(false, Ordering::SeqCst) {
        stop_running(shared, &mut session)?;
        return Ok(None);
    }
    Ok(Some(action()))
}
pub fn poll_generation(shared: &Shared, generation: Generation) -> Result<bool, String> {
    let mut session = shared.monitor.lock();
    if session.generation != generation.0 {
        return Ok(false);
    }
    if shared.stop.swap(false, Ordering::SeqCst) && session.attached {
        stop_running(shared, &mut session)?;
    }
    refresh_locked(shared, &mut session)?;
    Ok(session.generation == generation.0 && session.attached)
}
#[cfg(test)]
pub fn test_attach(shared: &Shared) {
    shared.monitor.lock().attached = true;
}
#[cfg(test)]
pub fn test_attached(shared: &Shared) -> bool {
    shared.monitor.lock().attached
}

#[cfg(test)]
#[path = "launcher_monitor_runtime/tests.rs"]
mod tests;
