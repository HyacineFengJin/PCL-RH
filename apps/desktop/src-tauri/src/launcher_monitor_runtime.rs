//! Coordinate GUI state with the detached supervisor. Lock order is application
//! admission (`operations`), then this session mutex, then `status`/`log`. Marker
//! reads and adoption share the session lock with beginning a launch, so an old
//! terminal read cannot overwrite a later preparation. Workers carry a launch
//! generation; their status, stop and window effects expire with that generation.
//! Historical terminal recovery is allowed before a local attempt starts. After
//! admission, its old durable marker is explicitly retired. Even if preparation
//! fails before publishing a helper, the old marker cannot reclaim log/status
//! after another GUI restart.
use crate::{launcher_game_monitor, RunStatus, Shared};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
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
    preparation_cancel: Option<Arc<AtomicBool>>,
    supervisor: Option<String>,
    binding: Option<MonitorBinding>,
    log_start: Option<u64>,
    // A different GUI can observe the helper's initial claim before its output
    // FD exists. Only that Starting observation permits None→Some once; ACK
    // adoption already knows the offset and never grants this exception.
    boundary_pending: bool,
    terminal_revision: Option<String>,
    // Only unbound history is retired; a helper this GUI actually observes
    // running may always publish its own final result afterward.
    unattached_history_retired: bool,
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
    instance_id: Option<String>,
    log_path: String,
    log_start: Option<u64>,
    boundary_pending: bool,
    terminal: bool,
    retired: bool,
    exit_code: Option<i32>,
    revision: String,
}
/// These captured values are immutable after the initial boundary commitment.
/// The instance ID
/// is meaningful only with this exact root and supervisor; matching names in a
/// different root or a log filename never establish the identity of an attempt.
#[derive(Clone, PartialEq, Eq)]
struct MonitorBinding {
    root_id: String,
    root_path: String,
    instance_id: Option<String>,
    log_path: String,
    log_start: Option<u64>,
}
impl From<&Observation> for MonitorBinding {
    fn from(monitor: &Observation) -> Self {
        Self {
            root_id: monitor.root_id.clone(),
            root_path: monitor.root_path.clone(),
            instance_id: monitor.instance_id.clone(),
            log_path: monitor.log_path.clone(),
            log_start: monitor.log_start,
        }
    }
}
impl MonitorBinding {
    fn commits_initial_boundary(&self, next: &Self) -> bool {
        self.log_start.is_none()
            && next.log_start.is_some()
            && self.root_id == next.root_id
            && self.root_path == next.root_path
            && self.instance_id == next.instance_id
            && self.log_path == next.log_path
    }
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
            instance_id: monitor.instance_id,
            log_path: monitor.log_path,
            log_start: monitor.log_start,
            boundary_pending: monitor.phase == "starting" && monitor.log_start.is_none(),
            terminal: matches!(monitor.phase.as_str(), "exited" | "failed"),
            retired: monitor.phase == "retired",
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
        Ok(Some(monitor)) => {
            if monitor.retired && !monitor.busy {
                // Another GUI may retire a finished helper this GUI last saw
                // running. Retired history cannot become a new failure result.
                return if session.attached || session.supervisor.is_some() {
                    retire_history_locked(shared, session)
                } else {
                    Ok(())
                };
            }
            let replaced = session.supervisor.as_deref() != Some(monitor.session_id.as_str());
            let binding = MonitorBinding::from(&monitor);
            if !replaced {
                if session.binding.as_ref().is_some_and(|seen| {
                    *seen != binding
                        && !(session.boundary_pending && seen.commits_initial_boundary(&binding))
                }) || session
                    .terminal_revision
                    .as_deref()
                    .is_some_and(|seen| seen != monitor.revision)
                {
                    if !session.attached && session.terminal_revision.is_some() {
                        return retire_history_locked(shared, session);
                    }
                    return fail_locked(
                        shared,
                        session,
                        "独立游戏监控的已绑定记录被外部修改，已保留文件；请检查运行状态".into(),
                    );
                }
                if !session.attached
                    && session.terminal_revision.as_deref() == Some(monitor.revision.as_str())
                {
                    // A successful manual stop is a GUI intent. Reapplying the
                    // same signal-exit marker would erase its stopped outcome.
                    return Ok(());
                }
            }
            let recover_terminal = monitor.terminal
                && (!session.unattached_history_retired || session.supervisor.is_some());
            if !monitor.busy && !session.attached && !recover_terminal {
                return Ok(());
            }
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
            session.binding = Some(binding);
            session.log_start = monitor.log_start;
            session.boundary_pending = monitor.boundary_pending;
            session.terminal_revision = (!monitor.busy).then(|| monitor.revision.clone());
            let stopped = !monitor.busy && session.stop_requested;
            if !monitor.busy {
                session.stop_requested = false;
            }
            let mut run = shared.status.lock().unwrap();
            // Only explicit persisted identity may be recovered. A live legacy
            // helper can retain the instance from this GUI's own acknowledged
            // launch; reopening a version-1 marker always leaves it unknown.
            let version = monitor.instance_id.clone().or_else(|| {
                (!replaced
                    && run.root_id.as_deref() == Some(monitor.root_id.as_str())
                    && run.root_path.as_deref() == Some(monitor.root_path.as_str()))
                .then(|| run.version.clone())
                .flatten()
            });
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
        Ok(None) if session.terminal_revision.is_some() => retire_history_locked(shared, session),
        Ok(_) => Ok(()),
        Err(error) => fail_locked(shared, session, error),
    }
}
/// A completed process cannot become busy because its history file moved. Drop
/// that history's report/log binding and fence readers, while leaving file bytes
/// alone. Unknown formats still take fail_locked: they may hide a new live game.
fn retire_history_locked(shared: &Shared, session: &mut Session) -> Result<(), String> {
    session.generation = session
        .generation
        .checked_add(1)
        .ok_or("启动代次已达上限，请重新打开启动器")?;
    session.attached = false;
    session.supervisor = None;
    session.binding = None;
    session.log_start = None;
    session.boundary_pending = false;
    session.terminal_revision = None;
    session.unattached_history_retired = true;
    session.stop_requested = false;
    session.preparation_cancel = None;
    *shared.status.lock().unwrap() = RunStatus {
        stage: "idle".into(),
        message: "已完成的游戏监控记录已变化，先前失败上下文已失效".into(),
        ..Default::default()
    };
    *shared.log.lock().unwrap() = None;
    Ok(())
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

/// A diagnostic owns this exact failed attempt, including a generation even
/// when a later attempt happens to publish an identical error and log path.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct FailureSnapshot {
    pub run: RunStatus,
    pub log: Option<PathBuf>,
    pub log_start: Option<u64>,
    generation: Generation,
}
impl FailureSnapshot {
    pub(super) fn generation(&self) -> u64 {
        self.generation.0
    }
}

/// Caller holds operations. Refresh may itself describe a broken monitor; that
/// error remains diagnosable. Capture status and log while holding the session
/// lock so a new preparation cannot substitute either half of this binding.
pub(super) fn failure_snapshot(shared: &Shared) -> Result<FailureSnapshot, String> {
    let mut session = shared.monitor.lock();
    let _ = refresh_locked(shared, &mut session);
    failure_snapshot_locked(shared, &session)
}

fn failure_snapshot_locked(shared: &Shared, session: &Session) -> Result<FailureSnapshot, String> {
    let run = shared.status.lock().unwrap().clone();
    if run.stage != "error" {
        return Err("当前没有可诊断的启动失败，请刷新启动状态".into());
    }
    Ok(FailureSnapshot {
        run,
        log: shared.log.lock().unwrap().clone(),
        log_start: session.log_start,
        generation: Generation(session.generation),
    })
}

/// Keep monitor adoption and worker updates fenced through the final metadata
/// check. This callback only checks already-bound FDs; the bounded content read
/// and redaction must run before taking the session lock.
pub(super) fn verify_failure(
    shared: &Shared,
    expected: &FailureSnapshot,
    verify: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut session = shared.monitor.lock();
    let _ = refresh_locked(shared, &mut session);
    if failure_snapshot_locked(shared, &session)? != *expected {
        return Err("当前启动失败已变化，请重新诊断".into());
    }
    verify()
}
/// Caller holds operations. Account edits take status while starting auth, so
/// their admission check and publication of `preparing` share that same lock.
pub fn begin_preparation(shared: &Shared, next: RunStatus) -> Result<Generation, String> {
    begin_preparation_with(shared, next, |revision| {
        launcher_game_monitor::retire_completed(&shared.project, revision)
    })
}
fn begin_preparation_with(
    shared: &Shared,
    next: RunStatus,
    retire: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Generation, String> {
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
    let next_generation = session
        .generation
        .checked_add(1)
        .ok_or("启动代次已达上限，请重新打开启动器")?;
    if let Some(revision) = &session.terminal_revision {
        // Publication is the commit boundary: do not retire history before all
        // admission checks pass, or publish preparing if retirement fails. The
        // lifecycle lock/revision/process recheck belongs to the monitor service.
        retire(revision)?;
    }
    session.generation = next_generation;
    session.stop_requested = false;
    session.preparation_cancel = None;
    session.supervisor = None;
    session.binding = None;
    session.log_start = None;
    session.boundary_pending = false;
    session.terminal_revision = None;
    session.unattached_history_retired = true;
    shared.stop.store(false, Ordering::SeqCst);
    *run = next;
    // Preparing can fail before a monitor creates its log. The preceding
    // attempt's log must never become evidence for that new failure.
    *shared.log.lock().unwrap() = None;
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

/// A preparation worker may continue only in its own unacknowledged generation.
/// This check performs no marker or filesystem reads and never consumes stop.
pub(super) fn require_current_preparation(
    shared: &Shared,
    generation: Generation,
) -> Result<(), String> {
    let session = shared.monitor.lock();
    require_preparation_locked(shared, &session, generation)
}

/// Pins the final spawn admission against accepted stop requests. The caller
/// holds operations first and drops this guard before ACK adoption takes it.
pub(super) struct PreparationAdmission<'a> {
    _session: MutexGuard<'a, Session>,
}

pub(super) fn authorize_preparation(
    shared: &Shared,
    generation: Generation,
) -> Result<PreparationAdmission<'_>, String> {
    let session = shared.monitor.lock();
    require_preparation_locked(shared, &session, generation)?;
    Ok(PreparationAdmission { _session: session })
}

fn require_preparation_locked(
    shared: &Shared,
    session: &Session,
    generation: Generation,
) -> Result<(), String> {
    if session.generation != generation.0
        || session.attached
        || shared.status.lock().unwrap().stage != "preparing"
    {
        return Err("启动准备会话已变化，请重新启动".into());
    }
    if shared.stop.load(Ordering::SeqCst)
        || session.stop_requested
        || session
            .preparation_cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::SeqCst))
    {
        return Err(crate::tasks::CANCELLED.into());
    }
    Ok(())
}

/// Stop and download scheduling share this exact cancellation token. Binding
/// under the session lock closes the stop-before-registration race.
pub(super) fn bind_preparation_cancel(
    shared: &Shared,
    generation: Generation,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut session = shared.monitor.lock();
    if let Err(error) = require_preparation_locked(shared, &session, generation) {
        if error == crate::tasks::CANCELLED {
            cancel.store(true, Ordering::SeqCst);
        }
        return Err(error);
    }
    session.preparation_cancel = Some(cancel);
    Ok(())
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
        || session
            .binding
            .as_ref()
            .is_some_and(|seen| *seen != MonitorBinding::from(&monitor))
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
        if let Some(cancel) = &session.preparation_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
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
    log_start: Option<u64>,
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
    session.terminal_revision = None;
    session.log_start = log_start;
    session.boundary_pending = false; // ACK already carries the committed start.
    let log_path = log.display().to_string();
    *shared.log.lock().unwrap() = Some(log);
    {
        let mut run = shared.status.lock().unwrap();
        // Pin the acknowledged launch's captured identity before the first
        // refresh, including any external replacement in that narrow interval.
        session.binding =
            run.root_id
                .as_ref()
                .zip(run.root_path.as_ref())
                .map(|(root_id, root_path)| MonitorBinding {
                    root_id: root_id.clone(),
                    root_path: root_path.clone(),
                    instance_id: run.version.clone(),
                    log_path,
                    log_start,
                });
        run.stage = "running".into();
        run.message = "游戏进程已启动，正在加载".into();
        run.pid = Some(pid);
        run.exit_code = None;
    }
    let preparation_cancelled = session
        .preparation_cancel
        .take()
        .is_some_and(|cancel| cancel.load(Ordering::SeqCst));
    if shared.stop.swap(false, Ordering::SeqCst) || preparation_cancelled {
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
    log_start: u64,
) -> Result<Handoff, String> {
    adopt_with(
        shared,
        generation,
        pid,
        monitor_pid,
        supervisor,
        log,
        Some(log_start),
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
pub(super) fn set_failure_log_start(shared: &Shared, start: Option<u64>) {
    let mut session = shared.monitor.lock();
    session.log_start = start;
    if let Some(binding) = &mut session.binding {
        binding.log_start = start;
    }
}

#[cfg(test)]
#[path = "launcher_monitor_runtime/tests.rs"]
mod tests;
