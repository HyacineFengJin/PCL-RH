//! Task record ownership and bounded writer scheduling. Legacy admission is
//! globally exclusive; only explicitly migrated download workers may queue and
//! must obtain their turn before network or filesystem work. A cancellation
//! request never releases a scope: the worker first acknowledges cleanup.

#[path = "tasks/schedule.rs"]
mod schedule;
#[path = "tasks/scope.rs"]
mod scope;
pub use scope::TaskScope;
#[path = "tasks/events.rs"]
pub(crate) mod events;

use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const HISTORY_LIMIT: usize = 64;
const RUNNING_LIMIT: usize = 4;
const PENDING_LIMIT: usize = 32;
pub const CANCELLED: &str = "任务已取消";
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
type Listener = Arc<dyn Fn(TaskSnapshot) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct TaskTarget {
    pub root_id: String,
    pub root_path: String,
    pub instance_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Install,
    ModpackInstall,
    ModpackPrepare,
    JavaInstall,
    InstanceRepair,
    InstanceVerify,
    InstanceDamageRepair,
    InstanceDamageRecover,
    InstanceReset,
    InstanceExport,
    InstanceRename,
    InstanceImport,
    InstanceCopy,
    InstanceDelete,
    InstanceRestore,
    ResourceDownload,
    ResourceSave,
    ToolboxDownload,
    ResourceUpdate,
    ResourceUpdateRestore,
    ResourceOperation,
    LauncherLogs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStage {
    Queued,
    Preparing,
    Downloading,
    Processing,
    Complete,
    Error,
    Cancelled,
}

impl TaskStage {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Error | Self::Cancelled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Downloading => "downloading",
            Self::Processing => "processing",
            Self::Complete => "complete",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskSnapshot {
    pub id: String,
    pub root_id: String,
    pub root_path: String,
    pub instance_id: Option<String>,
    /// Provider-verified resource title, separate from the captured instance ID.
    pub display_name: Option<String>,
    pub kind: TaskKind,
    pub stage: TaskStage,
    pub phase: String,
    pub message: String,
    pub progress: f64,
    pub completed: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub network_bytes: u64,
    pub steps: Vec<pcl_install::InstallStep>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub can_cancel: bool,
    pub created_at: u64,
    pub finished_at: Option<u64>,
}

pub struct TaskProgress {
    pub stage: TaskStage,
    pub phase: String,
    pub message: String,
    pub completed: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub network_bytes: u64,
    pub steps: Vec<pcl_install::InstallStep>,
}

pub enum TaskOutcome {
    Complete {
        result: Option<Value>,
        message: String,
        // A post-install persistence warning does not invalidate the installed result.
        error: Option<String>,
    },
    /// All eligible writes finished, but required files remain unavailable.
    /// Retain the result and error card so a partially repaired instance cannot
    /// disappear as an ordinary successful download from the task page.
    Incomplete {
        result: Option<Value>,
        message: String,
        error: String,
    },
    Failed(String),
    CleanupFailed(String),
    // A cancellation request is not proof that it caused a failure. Services
    // with an explicit cancellation result use Error for every other failure.
    Error(String),
}

struct TaskRecord {
    snapshot: TaskSnapshot,
    cancel: Arc<AtomicBool>,
    turn: schedule::Turn,
    scope: TaskScope,
    initial_phase: String,
    initial_message: String,
    waiters: usize,
}

struct Inner {
    history: VecDeque<TaskRecord>,
    listener: Option<Listener>,
    revision: u64,
}

/// Every queued/running record remains owned by its worker until cleanup. The
/// history budget can trim only terminal records without a waiting observer.
pub struct Tasks {
    inner: Mutex<Inner>,
    finished: Condvar,
    history_limit: usize,
    running_limit: usize,
    pending_limit: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskListSnapshot {
    pub revision: String,
    pub tasks: Vec<TaskSnapshot>,
    pub running_limit: usize,
    pub pending_limit: usize,
}

impl Inner {
    fn changed(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("task revision exhausted");
    }
}

pub struct TaskHandle {
    tasks: Arc<Tasks>,
    id: String,
    cancel: Arc<AtomicBool>,
}

struct HistoryPin<'a> {
    tasks: &'a Tasks,
    id: String,
}

impl Drop for HistoryPin<'_> {
    fn drop(&mut self) {
        let notice = {
            let mut inner = self.tasks.inner.lock().unwrap();
            if let Some(record) = inner
                .history
                .iter_mut()
                .find(|record| record.snapshot.id == self.id)
            {
                record.waiters -= 1;
            }
            let revision = inner.revision;
            self.tasks.trim(&mut inner);
            if inner.revision != revision {
                inner
                    .history
                    .back()
                    .map(|record| (inner.listener.clone(), record.snapshot.clone()))
            } else {
                None
            }
        };
        if let Some((listener, snapshot)) = notice {
            publish(listener, snapshot);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn publish(listener: Option<Listener>, snapshot: TaskSnapshot) {
    if let Some(listener) = listener {
        // UI notification failures must not strand an admitted worker.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(snapshot)));
    }
}

impl Default for Tasks {
    fn default() -> Self {
        Self::new()
    }
}

impl Tasks {
    pub fn new() -> Self {
        Self::with_history_limit(HISTORY_LIMIT)
    }

    fn with_history_limit(history_limit: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                history: VecDeque::new(),
                listener: None,
                revision: 0,
            }),
            finished: Condvar::new(),
            history_limit: history_limit.max(1),
            running_limit: RUNNING_LIMIT,
            pending_limit: PENDING_LIMIT,
        }
    }

    pub fn set_listener(&self, listener: impl Fn(TaskSnapshot) + Send + Sync + 'static) {
        self.inner.lock().unwrap().listener = Some(Arc::new(listener));
    }

    pub fn list(&self) -> Vec<TaskSnapshot> {
        self.inner
            .lock()
            .unwrap()
            .history
            .iter()
            .rev()
            .map(|record| record.snapshot.clone())
            .collect()
    }

    /// One mutex read binds the complete task set to its monotonic revision.
    #[cfg(test)]
    pub fn list_snapshot(&self) -> TaskListSnapshot {
        let inner = self.inner.lock().unwrap();
        self.project_list(&inner)
    }

    /// Diagnostics need to notice even a task that started and finished during
    /// their read. Capture the generation without cloning the task history.
    pub(crate) fn revision(&self) -> u64 {
        self.inner.lock().unwrap().revision
    }

    fn project_list(&self, inner: &Inner) -> TaskListSnapshot {
        TaskListSnapshot {
            revision: inner.revision.to_string(),
            tasks: inner
                .history
                .iter()
                .rev()
                .map(|record| record.snapshot.clone())
                .collect(),
            running_limit: self.running_limit,
            pending_limit: self.pending_limit,
        }
    }

    /// Resolve registry paths before locking the scheduler, then bind task and
    /// blocked-root projections to one revision. No filesystem IO runs while
    /// task progress, cancellation or turn reservation waits for this mutex.
    /// Offline registry entries retain exact root-ID ownership; legacy global
    /// leases block them conservatively without failing the complete UI view.
    pub fn list_snapshot_with_roots(
        &self,
        roots: &[(String, PathBuf)],
    ) -> Result<(TaskListSnapshot, Vec<String>), String> {
        let scopes: Vec<_> = roots
            .iter()
            .map(|(id, path)| (id, TaskScope::root(path).ok()))
            .collect();
        let inner = self.inner.lock().unwrap();
        let blocked = scopes
            .into_iter()
            .filter(|(id, scope)| {
                inner.history.iter().any(|record| {
                    !record.snapshot.stage.is_terminal()
                        && (record.snapshot.root_id == **id
                            || scope
                                .as_ref()
                                .is_some_and(|scope| record.scope.conflicts(scope))
                            || (scope.is_none() && record.scope.is_global()))
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        Ok((self.project_list(&inner), blocked))
    }

    pub fn snapshot(&self, id: &str) -> Option<TaskSnapshot> {
        self.inner
            .lock()
            .unwrap()
            .history
            .iter()
            .find(|record| record.snapshot.id == id)
            .map(|record| record.snapshot.clone())
    }

    pub fn active(&self) -> Option<TaskSnapshot> {
        self.inner
            .lock()
            .unwrap()
            .history
            .iter()
            .find(|record| !record.snapshot.stage.is_terminal())
            .map(|record| record.snapshot.clone())
    }

    pub fn active_all(&self) -> Vec<TaskSnapshot> {
        self.inner
            .lock()
            .unwrap()
            .history
            .iter()
            .filter(|record| !record.snapshot.stage.is_terminal())
            .map(|record| record.snapshot.clone())
            .collect()
    }

    pub fn running_all(&self) -> Vec<TaskSnapshot> {
        self.inner
            .lock()
            .unwrap()
            .history
            .iter()
            .filter(|record| {
                record.turn == schedule::Turn::Running && !record.snapshot.stage.is_terminal()
            })
            .map(|record| record.snapshot.clone())
            .collect()
    }

    /// Includes queued ownership and final files physically inside this root.
    /// Root IDs are presentation identities, not filesystem conflict keys.
    pub fn blocks_path(&self, root: &Path) -> Result<bool, String> {
        let root = TaskScope::root(root)?;
        Ok(self
            .inner
            .lock()
            .unwrap()
            .history
            .iter()
            .any(|record| !record.snapshot.stage.is_terminal() && record.scope.conflicts(&root)))
    }

    pub fn uses_root(&self, root_id: &str) -> bool {
        self.inner.lock().unwrap().history.iter().any(|record| {
            !record.snapshot.stage.is_terminal() && record.snapshot.root_id == root_id
        })
    }

    pub fn admit(
        self: &Arc<Self>,
        target: TaskTarget,
        kind: TaskKind,
    ) -> Result<TaskHandle, String> {
        self.admit_inner(target, kind, TaskScope::global(), false)
    }

    /// Only workers migrated to wait_turn may enter the concurrent scheduler.
    /// Keeping legacy admission globally exclusive prevents an old synchronous
    /// transaction from writing without a gate while another scope is active.
    pub fn admit_queued(
        self: &Arc<Self>,
        target: TaskTarget,
        kind: TaskKind,
        scope: TaskScope,
    ) -> Result<TaskHandle, String> {
        if !matches!(
            kind,
            TaskKind::Install
                | TaskKind::ModpackInstall
                | TaskKind::ModpackPrepare
                | TaskKind::JavaInstall
                | TaskKind::ResourceDownload
                | TaskKind::ResourceSave
                | TaskKind::ToolboxDownload
        ) {
            return Err("此任务尚未支持排队，请使用独占操作".into());
        }
        self.admit_inner(target, kind, scope, true)
    }

    fn admit_inner(
        self: &Arc<Self>,
        target: TaskTarget,
        kind: TaskKind,
        scope: TaskScope,
        queued: bool,
    ) -> Result<TaskHandle, String> {
        let (snapshot, cancel, listener) = {
            let mut inner = self.inner.lock().unwrap();
            if !queued
                && inner
                    .history
                    .iter()
                    .any(|record| !record.snapshot.stage.is_terminal())
            {
                return Err("已有文件写入任务，请等待完成或取消".into());
            }
            let runs_now = !queued || schedule::can_start(&inner, &scope, self.running_limit);
            if !runs_now
                && inner
                    .history
                    .iter()
                    .filter(|record| {
                        record.turn == schedule::Turn::Queued
                            && !record.snapshot.stage.is_terminal()
                    })
                    .count()
                    >= self.pending_limit
            {
                return Err("排队任务已达32项，请等待完成或取消".into());
            }
            let created_at = now_ms();
            let id = format!(
                "task-{created_at}-{}",
                NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed)
            );
            let mut snapshot = TaskSnapshot {
                id: id.clone(),
                root_id: target.root_id,
                root_path: target.root_path,
                instance_id: target.instance_id,
                display_name: None,
                kind,
                stage: TaskStage::Preparing,
                phase: match kind {
                    TaskKind::Install => "metadata",
                    TaskKind::ModpackInstall => "pack-prepare",
                    TaskKind::ModpackPrepare => "pack-input-metadata",
                    TaskKind::JavaInstall => "java-metadata",
                    TaskKind::InstanceRepair => "repair-check",
                    TaskKind::InstanceVerify => "integrity-verify",
                    TaskKind::InstanceDamageRepair => "damage-check",
                    TaskKind::InstanceDamageRecover => "damage-recover",
                    TaskKind::InstanceReset => "reset_prepare",
                    TaskKind::InstanceExport => "export-scan",
                    TaskKind::InstanceRename => "rename_prepare",
                    TaskKind::InstanceImport => "import-check",
                    TaskKind::InstanceCopy => "import-check",
                    TaskKind::InstanceDelete => "delete-check",
                    TaskKind::InstanceRestore => "restore-check",
                    TaskKind::ResourceDownload => "resource-check",
                    TaskKind::ResourceSave => "resource-save-check",
                    TaskKind::ToolboxDownload => "toolbox-download-check",
                    TaskKind::ResourceUpdate => "resource-update-check",
                    TaskKind::ResourceUpdateRestore => "resource-update-restore-check",
                    TaskKind::ResourceOperation => "resources",
                    TaskKind::LauncherLogs => "launcher-logs",
                }
                .into(),
                message: match kind {
                    TaskKind::Install => "正在获取版本信息…",
                    TaskKind::ModpackInstall => "正在准备整合包安装…",
                    TaskKind::ModpackPrepare => "正在获取整合包文件信息…",
                    TaskKind::JavaInstall => "正在获取 Java 版本信息…",
                    TaskKind::InstanceRepair => "正在检查可补全的缺失文件…",
                    TaskKind::InstanceVerify => "正在检查依赖完整性…",
                    TaskKind::InstanceDamageRepair => "正在检查可修复的损坏文件…",
                    TaskKind::InstanceDamageRecover => "正在恢复未完成的文件修复…",
                    TaskKind::InstanceReset => "正在检查重置方案…",
                    TaskKind::InstanceExport => "正在检查导出文件…",
                    TaskKind::InstanceRename => "正在检查实例名称与引用…",
                    TaskKind::InstanceImport => "正在检查本地 ZIP…",
                    TaskKind::InstanceCopy => "正在检查实例复制…",
                    TaskKind::InstanceDelete => "正在检查实例删除范围…",
                    TaskKind::InstanceRestore => "正在检查实例恢复记录…",
                    TaskKind::ResourceDownload => "正在获取资源与必需前置信息…",
                    TaskKind::ResourceSave => "正在检查文件保存信息…",
                    TaskKind::ToolboxDownload => "正在检查自定义下载…",
                    TaskKind::ResourceUpdate => "正在检查模组更新与必需前置…",
                    TaskKind::ResourceUpdateRestore => "正在检查模组更新恢复记录…",
                    TaskKind::ResourceOperation => "正在检查资源文件…",
                    TaskKind::LauncherLogs => "正在检查日志文件…",
                }
                .into(),
                progress: 0.0,
                completed: 0,
                total: 0,
                bytes_done: 0,
                bytes_total: 0,
                network_bytes: 0,
                steps: Vec::new(),
                result: None,
                error: None,
                can_cancel: true,
                created_at,
                finished_at: None,
            };
            let initial_phase = snapshot.phase.clone();
            let initial_message = snapshot.message.clone();
            if !runs_now {
                snapshot.stage = TaskStage::Queued;
                snapshot.phase = "queued".into();
                snapshot.message = "等待其他任务完成后开始…".into();
            }
            let cancel = Arc::new(AtomicBool::new(false));
            inner.history.push_back(TaskRecord {
                snapshot: snapshot.clone(),
                cancel: cancel.clone(),
                turn: if runs_now {
                    schedule::Turn::Running
                } else {
                    schedule::Turn::Queued
                },
                scope,
                initial_phase,
                initial_message,
                waiters: 0,
            });
            inner.changed();
            self.trim(&mut inner);
            (snapshot, cancel, inner.listener.clone())
        };
        self.finished.notify_all();
        let handle = TaskHandle {
            tasks: self.clone(),
            id: snapshot.id.clone(),
            cancel,
        };
        publish(listener, snapshot);
        Ok(handle)
    }

    pub fn cancel(&self, id: &str) -> Result<TaskSnapshot, String> {
        let (snapshot, listener, changed) = {
            let mut inner = self.inner.lock().unwrap();
            let record = inner
                .history
                .iter_mut()
                .find(|record| record.snapshot.id == id)
                .ok_or("找不到此任务")?;
            let changed = record.snapshot.can_cancel && !record.snapshot.stage.is_terminal();
            if changed {
                record.cancel.store(true, Ordering::SeqCst);
                record.snapshot.can_cancel = false;
                record.snapshot.message = match record.snapshot.kind {
                    TaskKind::Install => "正在取消安装…",
                    TaskKind::ModpackInstall => "正在取消整合包安装并清理未完成文件…",
                    TaskKind::ModpackPrepare => "正在取消整合包下载并清理暂存文件…",
                    TaskKind::JavaInstall => "正在取消 Java 下载并清理未完成文件…",
                    TaskKind::InstanceRepair => "正在取消文件补全并清理未完成文件…",
                    TaskKind::InstanceVerify => "正在取消依赖完整性检查…",
                    TaskKind::InstanceDamageRepair => "正在取消修复并恢复未提交文件…",
                    TaskKind::InstanceDamageRecover => "正在取消恢复，恢复记录将保留…",
                    TaskKind::InstanceReset => "正在取消重置并恢复原核心文件…",
                    TaskKind::InstanceExport => "正在取消导出并清理临时文件…",
                    TaskKind::InstanceRename => "正在取消改名并清理临时文件…",
                    TaskKind::InstanceImport => "正在取消导入并清理未完成文件…",
                    TaskKind::InstanceCopy => "正在取消复制并清理未完成文件…",
                    TaskKind::InstanceDelete => "正在取消删除…",
                    TaskKind::InstanceRestore => "正在取消恢复…",
                    TaskKind::ResourceDownload => "正在取消资源下载并清理未完成文件…",
                    TaskKind::ResourceSave => "正在取消文件下载并清理未完成文件…",
                    TaskKind::ToolboxDownload => "正在取消自定义下载并清理未完成文件…",
                    TaskKind::ResourceUpdate => "正在取消模组更新并清理未完成文件…",
                    TaskKind::ResourceUpdateRestore => "正在取消模组更新恢复…",
                    TaskKind::ResourceOperation => "正在取消资源操作…",
                    TaskKind::LauncherLogs => "正在取消日志操作…",
                }
                .into();
                if record.turn == schedule::Turn::Queued {
                    record.snapshot.message = "正在取消排队任务，请等待清理完成…".into();
                }
            }
            let snapshot = record.snapshot.clone();
            if changed {
                inner.changed();
            }
            (snapshot, inner.listener.clone(), changed)
        };
        if changed {
            self.finished.notify_all();
            publish(listener, snapshot.clone());
        }
        Ok(snapshot)
    }

    /// The worker publishes terminal state only after dropping its staging files
    /// and stopping child processes. A cancellation caller waits for that point.
    #[cfg(test)]
    pub fn wait_terminal(&self, id: &str) -> Result<TaskSnapshot, String> {
        let _pin = self.pin_history(id)?;
        self.wait_terminal_pinned(id)
    }

    /// Pin before issuing cancellation, so history trimming cannot remove the
    /// exact requested terminal record between cancel and its response.
    pub fn cancel_and_wait(&self, id: &str) -> Result<TaskSnapshot, String> {
        let _pin = self.pin_history(id)?;
        self.cancel(id)?;
        self.wait_terminal_pinned(id)
    }

    fn pin_history(&self, id: &str) -> Result<HistoryPin<'_>, String> {
        let mut inner = self.inner.lock().unwrap();
        let record = inner
            .history
            .iter_mut()
            .find(|record| record.snapshot.id == id)
            .ok_or("找不到此任务")?;
        record.waiters = record.waiters.checked_add(1).ok_or("等待任务数量过多")?;
        Ok(HistoryPin {
            tasks: self,
            id: id.into(),
        })
    }

    fn wait_terminal_pinned(&self, id: &str) -> Result<TaskSnapshot, String> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            let record = inner
                .history
                .iter()
                .find(|record| record.snapshot.id == id)
                .ok_or("找不到此任务")?;
            if record.snapshot.stage.is_terminal() {
                return Ok(record.snapshot.clone());
            }
            inner = self.finished.wait(inner).map_err(|_| "等待任务清理失败")?;
        }
    }

    fn wait_turn(&self, id: &str, cancel: &AtomicBool) -> Result<(), String> {
        let scope = {
            let mut inner = self.inner.lock().unwrap();
            loop {
                let record = inner
                    .history
                    .iter()
                    .find(|record| record.snapshot.id == id)
                    .ok_or("找不到此任务")?;
                if cancel.load(Ordering::SeqCst) {
                    return Err(CANCELLED.into());
                }
                match record.turn {
                    schedule::Turn::Running => break record.scope.clone(),
                    schedule::Turn::Terminal => return Err("此任务已结束".into()),
                    schedule::Turn::Queued => {
                        inner = self.finished.wait(inner).map_err(|_| "等待任务运行失败")?;
                    }
                }
            }
        };
        scope.recheck()?;
        if cancel.load(Ordering::SeqCst) {
            Err(CANCELLED.into())
        } else {
            Ok(())
        }
    }

    fn trim(&self, inner: &mut Inner) {
        // Preserve the legacy total-history budget while live jobs and pinned
        // cancellation replies are protected. When live jobs exceed that budget,
        // retain at least the newest terminal result so completion remains visible.
        let live = inner
            .history
            .iter()
            .filter(|record| !record.snapshot.stage.is_terminal())
            .count();
        let terminal_budget = self.history_limit.saturating_sub(live).max(1);
        while inner
            .history
            .iter()
            .filter(|record| record.snapshot.stage.is_terminal() && record.waiters == 0)
            .count()
            > terminal_budget
        {
            let Some(index) = inner
                .history
                .iter()
                .position(|record| record.snapshot.stage.is_terminal() && record.waiters == 0)
            else {
                break;
            };
            inner.history.remove(index);
            inner.changed();
        }
    }

    fn update(&self, id: &str, progress: TaskProgress) -> Option<TaskSnapshot> {
        if progress.stage.is_terminal() || progress.stage == TaskStage::Queued {
            return None;
        }
        let (snapshot, listener) = {
            let mut inner = self.inner.lock().unwrap();
            let record = inner
                .history
                .iter_mut()
                .find(|record| record.snapshot.id == id)?;
            if record.turn != schedule::Turn::Running {
                return None;
            }
            let snapshot = &mut record.snapshot;
            snapshot.stage = progress.stage;
            snapshot.phase = progress.phase;
            if !record.cancel.load(Ordering::SeqCst) {
                snapshot.message = progress.message;
            }
            // Parallel file callbacks can arrive out of order. File completion and
            // real network transfer totals are cumulative; retry bytes may fall.
            snapshot.completed = snapshot.completed.max(progress.completed);
            snapshot.total = snapshot.total.max(progress.total);
            snapshot.bytes_done = progress.bytes_done;
            snapshot.bytes_total = snapshot.bytes_total.max(progress.bytes_total);
            snapshot.network_bytes = snapshot.network_bytes.max(progress.network_bytes);
            if !progress.steps.is_empty() {
                // Concurrent transfer callbacks may arrive in reverse order. Keep
                // each completed phase complete without inventing later progress.
                for mut step in progress.steps {
                    if let Some(old) = snapshot.steps.iter_mut().find(|old| old.id == step.id) {
                        if old.state == "complete"
                            || (old.state == "running" && step.state == "pending")
                        {
                            continue;
                        }
                        if let (Some(previous), Some(next)) = (old.progress, step.progress) {
                            step.progress = Some(previous.max(next).clamp(0.0, 1.0));
                        }
                        *old = step;
                    } else {
                        snapshot.steps.push(step);
                    }
                }
            }
            let ratio = if snapshot.bytes_total > 0 {
                snapshot.bytes_done as f64 / snapshot.bytes_total as f64
            } else if snapshot.total > 0 {
                snapshot.completed as f64 / snapshot.total as f64
            } else {
                0.0
            };
            snapshot.progress = if snapshot.steps.is_empty() {
                snapshot.progress.max(ratio.clamp(0.0, 1.0))
            } else {
                snapshot
                    .steps
                    .iter()
                    .map(|step| {
                        if step.state == "complete" {
                            1.0
                        } else {
                            step.progress.unwrap_or(0.0).clamp(0.0, 1.0)
                        }
                    })
                    .sum::<f64>()
                    / snapshot.steps.len() as f64
            };
            let snapshot = snapshot.clone();
            inner.changed();
            (snapshot, inner.listener.clone())
        };
        publish(listener, snapshot.clone());
        Some(snapshot)
    }

    fn begin_finishing(&self, id: &str) {
        let change = {
            let mut inner = self.inner.lock().unwrap();
            let Some(record) = inner
                .history
                .iter_mut()
                .find(|record| record.snapshot.id == id)
            else {
                return;
            };
            if record.turn != schedule::Turn::Running {
                return;
            }
            record.snapshot.stage = TaskStage::Processing;
            record.snapshot.can_cancel = false;
            let snapshot = record.snapshot.clone();
            inner.changed();
            (snapshot, inner.listener.clone())
        };
        publish(change.1, change.0);
    }

    fn finish(&self, id: &str, outcome: TaskOutcome) -> Option<TaskSnapshot> {
        let (snapshot, listener, promoted) = {
            let mut inner = self.inner.lock().unwrap();
            let record = inner
                .history
                .iter_mut()
                .find(|record| record.snapshot.id == id)?;
            if record.turn == schedule::Turn::Terminal
                || (record.turn == schedule::Turn::Queued
                    && matches!(outcome, TaskOutcome::Complete { .. }))
            {
                return None;
            }
            let snapshot = &mut record.snapshot;
            match outcome {
                TaskOutcome::Complete {
                    result,
                    message,
                    error,
                } => {
                    snapshot.stage = TaskStage::Complete;
                    snapshot.phase = "complete".into();
                    snapshot.message = message;
                    snapshot.progress = 1.0;
                    snapshot.result = result;
                    snapshot.error = error;
                }
                TaskOutcome::Failed(error) => {
                    let cancelled = record.cancel.load(Ordering::SeqCst);
                    snapshot.stage = if cancelled {
                        TaskStage::Cancelled
                    } else {
                        TaskStage::Error
                    };
                    snapshot.phase = snapshot.stage.as_str().into();
                    snapshot.message = if cancelled {
                        match snapshot.kind {
                            TaskKind::Install => "安装已取消，未完成文件已清理",
                            TaskKind::ModpackInstall => "整合包安装已取消，未完成文件已清理",
                            TaskKind::ModpackPrepare => "整合包下载已取消，暂存文件已清理",
                            TaskKind::JavaInstall => "Java 下载已取消，未完成文件已清理",
                            TaskKind::InstanceRepair => {
                                "文件补全已取消，已完成文件保留，未完成文件已清理"
                            }
                            TaskKind::InstanceVerify => "依赖完整性检查已取消",
                            TaskKind::InstanceDamageRepair => "损坏文件修复已取消，原件备份已保留",
                            TaskKind::InstanceDamageRecover => "文件修复恢复已取消，请继续恢复",
                            TaskKind::InstanceReset => "重置已取消，原实例已保留",
                            TaskKind::InstanceExport => "导出已取消，未完成 ZIP 已清理",
                            TaskKind::InstanceRename => "改名已取消，原实例已保留",
                            TaskKind::InstanceImport => "导入已取消，未完成文件已清理",
                            TaskKind::InstanceCopy => "复制已取消，未完成文件已清理",
                            TaskKind::InstanceDelete => "删除已取消，原实例已保留",
                            TaskKind::InstanceRestore => "恢复已取消，可恢复文件已保留",
                            TaskKind::ResourceDownload => "资源下载已取消，未完成文件已清理",
                            TaskKind::ResourceSave => "文件下载已取消，未完成文件已清理",
                            TaskKind::ToolboxDownload => "自定义下载已取消，未完成文件已清理",
                            TaskKind::ResourceUpdate => "模组更新已取消，未完成文件已清理",
                            TaskKind::ResourceUpdateRestore => {
                                "模组恢复已取消，更新结果与恢复记录已保留"
                            }
                            TaskKind::ResourceOperation => "资源操作已取消",
                            TaskKind::LauncherLogs => "日志操作已取消",
                        }
                        .into()
                    } else {
                        error.clone()
                    };
                    snapshot.error = if cancelled { None } else { Some(error) };
                }
                TaskOutcome::Incomplete {
                    result,
                    message,
                    error,
                } => {
                    snapshot.stage = TaskStage::Error;
                    snapshot.phase = "error".into();
                    snapshot.message = message;
                    snapshot.progress = 1.0;
                    snapshot.result = result;
                    snapshot.error = Some(error);
                }
                TaskOutcome::CleanupFailed(error) | TaskOutcome::Error(error) => {
                    snapshot.stage = TaskStage::Error;
                    snapshot.phase = "error".into();
                    snapshot.message = error.clone();
                    snapshot.error = Some(error);
                }
            }
            snapshot.can_cancel = false;
            snapshot.finished_at = Some(now_ms().max(snapshot.created_at));
            let snapshot = snapshot.clone();
            record.turn = schedule::Turn::Terminal;
            inner.changed();
            let promoted = schedule::promote(&mut inner, self.running_limit);
            self.trim(&mut inner);
            (snapshot, inner.listener.clone(), promoted)
        };
        self.finished.notify_all();
        publish(listener.clone(), snapshot.clone());
        for change in promoted {
            publish(listener.clone(), change);
        }
        Some(snapshot)
    }
}

impl TaskHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cancellation_token(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Call outside operations and before any worker network/filesystem side
    /// effect. An error does not release this handle: drop captured artifacts,
    /// then finish Failed(CANCELLED) or Error before allowing another writer.
    pub fn wait_turn(&self) -> Result<(), String> {
        self.tasks.wait_turn(&self.id, &self.cancel)
    }

    pub fn update(&self, progress: TaskProgress) -> Option<TaskSnapshot> {
        self.tasks.update(&self.id, progress)
    }

    /// Resource and runtime workers supply a title only after resolving an
    /// official identity. A finished worker cannot rename a newer task, and a
    /// dependency cannot replace this one-time name.
    pub fn set_resource_name(&self, name: &str) {
        if name.is_empty() || name.len() > 2048 {
            return;
        }
        let change = {
            let mut inner = self.tasks.inner.lock().unwrap();
            let Some(record) = inner
                .history
                .iter_mut()
                .find(|record| record.snapshot.id == self.id)
            else {
                return;
            };
            if !matches!(
                record.snapshot.kind,
                TaskKind::ResourceDownload
                    | TaskKind::ModpackInstall
                    | TaskKind::ModpackPrepare
                    | TaskKind::JavaInstall
            ) || record.snapshot.display_name.is_some()
                || record.snapshot.stage.is_terminal()
            {
                return;
            }
            record.snapshot.display_name = Some(name.into());
            let snapshot = record.snapshot.clone();
            inner.changed();
            (snapshot, inner.listener.clone())
        };
        publish(change.1, change.0);
    }

    pub fn begin_finishing(&self) {
        self.tasks.begin_finishing(&self.id);
    }

    pub fn finish(&self, outcome: TaskOutcome) -> Option<TaskSnapshot> {
        self.tasks.finish(&self.id, outcome)
    }

    pub fn publish(&self) {
        let state = {
            let inner = self.tasks.inner.lock().unwrap();
            inner
                .history
                .iter()
                .find(|record| record.snapshot.id == self.id)
                .map(|record| (record.snapshot.clone(), inner.listener.clone()))
        };
        if let Some((snapshot, listener)) = state {
            publish(listener, snapshot);
        }
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        // Dropping the worker lease happens only after its work has stopped.
        self.tasks
            .finish(&self.id, TaskOutcome::Failed("任务意外退出，请重试".into()));
    }
}

#[cfg(test)]
#[path = "tasks/scheduling_tests.rs"]
mod scheduling_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Barrier};

    fn target(root: &str) -> TaskTarget {
        TaskTarget {
            root_id: root.into(),
            root_path: format!("{}/work/task-tests/{root}", env!("CARGO_MANIFEST_DIR")),
            instance_id: Some("fixture-version".into()),
        }
    }

    fn complete(task: &TaskHandle) {
        task.finish(TaskOutcome::Complete {
            result: Some(serde_json::json!({ "id": "fixture-version" })),
            message: "完成".into(),
            error: None,
        });
    }

    #[test]
    fn explicit_error_is_not_masked_by_a_late_cancellation() {
        let tasks = Arc::new(Tasks::new());
        let task = tasks
            .admit(target("root-a"), TaskKind::InstanceImport)
            .unwrap();
        let id = task.id().to_owned();
        tasks.cancel(&id).unwrap();
        task.finish(TaskOutcome::Error("Source hash changed".into()));
        let result = tasks.snapshot(&id).unwrap();
        assert_eq!(result.stage, TaskStage::Error);
        assert_eq!(result.error.as_deref(), Some("Source hash changed"));
        assert!(tasks.active().is_none());
    }

    #[test]
    fn concurrent_admission_has_one_writer() {
        let tasks = Arc::new(Tasks::new());
        let barrier = Arc::new(Barrier::new(3));
        let (tx, rx) = mpsc::channel();
        let mut workers = Vec::new();
        for root in ["root-a", "root-b"] {
            let tasks = tasks.clone();
            let barrier = barrier.clone();
            let tx = tx.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                tx.send(tasks.admit(target(root), TaskKind::Install))
                    .unwrap();
            }));
        }
        barrier.wait();
        let outcomes = [rx.recv().unwrap(), rx.recv().unwrap()];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(tasks.active().is_some());
        drop(outcomes);
        assert!(tasks.active().is_none());
    }

    #[test]
    fn cancellation_keeps_writer_and_captured_scope_until_worker_finishes() {
        let tasks = Arc::new(Tasks::new());
        let task = tasks.admit(target("root-a"), TaskKind::Install).unwrap();
        let cancelled = tasks.cancel(task.id()).unwrap();
        assert!(!cancelled.can_cancel);
        assert!(task.cancellation_token().load(Ordering::SeqCst));
        assert!(tasks.uses_root("root-a"));
        assert!(!tasks.uses_root("root-b"));
        assert!(tasks.admit(target("root-b"), TaskKind::Install).is_err());
        assert_eq!(
            tasks.snapshot(task.id()).unwrap().root_path,
            target("root-a").root_path
        );
        task.finish(TaskOutcome::Failed("取消".into()));
        assert!(tasks.active().is_none());
        assert_eq!(
            tasks.snapshot(task.id()).unwrap().stage,
            TaskStage::Cancelled
        );
        assert!(tasks.admit(target("root-b"), TaskKind::Install).is_ok());
    }

    #[test]
    fn instance_jobs_keep_writer_until_cleanup_and_keep_completed_results() {
        for kind in [
            TaskKind::InstanceReset,
            TaskKind::InstanceExport,
            TaskKind::InstanceRename,
        ] {
            let tasks = Arc::new(Tasks::new());
            let task = tasks.admit(target("root-a"), kind).unwrap();
            tasks.cancel(task.id()).unwrap();
            assert!(tasks.active().is_some());
            assert!(tasks.admit(target("root-b"), TaskKind::Install).is_err());
            task.finish(TaskOutcome::Failed("cancelled".into()));
            assert_eq!(tasks.list()[0].stage, TaskStage::Cancelled);
            assert!(tasks.active().is_none());
            let task = tasks.admit(target("root-a"), kind).unwrap();
            tasks.cancel(task.id()).unwrap();
            task.finish(TaskOutcome::Complete {
                result: None,
                message: "committed".into(),
                error: None,
            });
            assert_eq!(tasks.list()[0].stage, TaskStage::Complete);
        }
    }

    #[test]
    fn cancellation_waits_for_cleanup_and_returns_the_requested_task() {
        let tasks = Arc::new(Tasks::new());
        let first = tasks.admit(target("root-a"), TaskKind::Install).unwrap();
        let id = first.id().to_owned();
        tasks.cancel(&id).unwrap();
        let (tx, rx) = mpsc::channel();
        let waiting = tasks.clone();
        let submitted = id.clone();
        let waiter = std::thread::spawn(move || {
            tx.send(waiting.wait_terminal(&submitted).unwrap()).unwrap();
        });
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());
        assert!(tasks.admit(target("root-b"), TaskKind::Install).is_err());
        first.finish(TaskOutcome::Failed("取消".into()));
        let second = tasks.admit(target("root-b"), TaskKind::Install).unwrap();
        let finished = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(finished.id, id);
        assert_eq!(finished.stage, TaskStage::Cancelled);
        assert!(!second.cancellation_token().load(Ordering::SeqCst));
        waiter.join().unwrap();
    }

    #[test]
    fn cancelled_cleanup_failure_stays_visible_as_error() {
        let tasks = Arc::new(Tasks::new());
        let task = tasks.admit(target("root-a"), TaskKind::Install).unwrap();
        tasks.cancel(task.id()).unwrap();
        task.finish(TaskOutcome::CleanupFailed(
            "取消清理失败：文件正被使用".into(),
        ));
        let snapshot = tasks.wait_terminal(task.id()).unwrap();
        assert_eq!(snapshot.stage, TaskStage::Error);
        assert!(snapshot.error.unwrap().contains("清理失败"));
        assert!(tasks.active().is_none());
    }

    #[test]
    fn detailed_steps_preserve_completed_phases_on_late_callbacks() {
        let tasks = Arc::new(Tasks::new());
        let task = tasks.admit(target("root-a"), TaskKind::Install).unwrap();
        let step = |id: &str, state: &str, progress| pcl_install::InstallStep {
            id: id.into(),
            label: id.into(),
            state: state.into(),
            progress,
        };
        let progress = |steps| TaskProgress {
            stage: TaskStage::Downloading,
            phase: "downloading".into(),
            message: "下载".into(),
            completed: 10,
            total: 10,
            bytes_done: 100,
            bytes_total: 100,
            network_bytes: 100,
            steps,
        };
        task.update(progress(vec![
            step("metadata", "complete", Some(1.0)),
            step("files", "running", Some(0.6)),
            step("install", "pending", None),
        ]));
        // Fully transferred files are still only part of installation progress.
        assert!(tasks.snapshot(task.id()).unwrap().progress < 1.0);
        task.update(progress(vec![
            step("metadata", "pending", None),
            step("files", "running", Some(0.2)),
            step("install", "pending", None),
        ]));
        let snapshot = tasks.snapshot(task.id()).unwrap();
        assert_eq!(snapshot.steps[0].state, "complete");
        assert_eq!(snapshot.steps[1].progress, Some(0.6));
        tasks.cancel(task.id()).unwrap();
        task.finish(TaskOutcome::Failed("取消".into()));
        let snapshot = tasks.wait_terminal(task.id()).unwrap();
        assert_eq!(snapshot.steps[2].state, "pending");
        assert!(snapshot.progress < 1.0);
    }

    #[test]
    fn history_is_bounded_and_late_cancel_cannot_touch_new_task() {
        let tasks = Arc::new(Tasks::with_history_limit(2));
        let first = tasks.admit(target("root-a"), TaskKind::Install).unwrap();
        let first_id = first.id().to_owned();
        complete(&first);
        assert!(tasks.active().is_none());
        let second = tasks.admit(target("root-b"), TaskKind::Install).unwrap();
        assert_ne!(first.id(), second.id());
        let before = serde_json::to_value(tasks.snapshot(&first_id).unwrap()).unwrap();
        tasks.cancel(&first_id).unwrap();
        assert_eq!(
            before,
            serde_json::to_value(tasks.snapshot(&first_id).unwrap()).unwrap()
        );
        assert!(!second.cancellation_token().load(Ordering::SeqCst));
        complete(&second);
        let third = tasks.admit(target("root-c"), TaskKind::Install).unwrap();
        assert_eq!(tasks.list().len(), 2);
        assert!(tasks.snapshot(&first_id).is_none());
        assert_eq!(tasks.list()[0].id, third.id());
    }

    #[test]
    fn progress_and_network_bytes_are_monotonic_with_retry_counters() {
        let tasks = Arc::new(Tasks::new());
        let task = tasks.admit(target("root-a"), TaskKind::Install).unwrap();
        for (completed, bytes_done, network_bytes) in [(4, 80, 60), (3, 40, 50)] {
            task.update(TaskProgress {
                stage: TaskStage::Downloading,
                phase: "downloading".into(),
                message: "下载".into(),
                completed,
                total: 10,
                bytes_done,
                bytes_total: 100,
                network_bytes,
                steps: Vec::new(),
            });
        }
        let snapshot = tasks.snapshot(task.id()).unwrap();
        assert_eq!(snapshot.completed, 4);
        assert_eq!(snapshot.bytes_done, 40);
        assert_eq!(snapshot.network_bytes, 60);
        assert_eq!(snapshot.progress, 0.8);
        tasks.cancel(task.id()).unwrap();
        task.update(TaskProgress {
            stage: TaskStage::Processing,
            phase: "installing".into(),
            message: "安装".into(),
            completed: 10,
            total: 10,
            bytes_done: 100,
            bytes_total: 100,
            network_bytes: 70,
            steps: Vec::new(),
        });
        let snapshot = tasks.snapshot(task.id()).unwrap();
        assert_eq!(snapshot.stage, TaskStage::Processing);
        assert_eq!(snapshot.phase, "installing");
        assert_eq!(snapshot.message, "正在取消安装…");
        assert_eq!(snapshot.progress, 1.0);
    }

    #[test]
    fn completion_warning_keeps_result_and_publishes_without_locks() {
        let tasks = Arc::new(Tasks::new());
        let weak = Arc::downgrade(&tasks);
        let (tx, rx) = mpsc::channel();
        tasks.set_listener(move |snapshot| {
            let tasks = weak.upgrade().unwrap();
            assert!(tasks.snapshot(&snapshot.id).is_some());
            let _ = tasks.list();
            tx.send(snapshot.stage).unwrap();
        });
        let task = tasks.admit(target("root-a"), TaskKind::Install).unwrap();
        task.begin_finishing();
        tasks.cancel(task.id()).unwrap();
        assert!(!task.cancellation_token().load(Ordering::SeqCst));
        task.finish(TaskOutcome::Complete {
            result: Some(serde_json::json!({ "id": "fixture-version" })),
            message: "安装完成，但选择失败".into(),
            error: Some("保存失败".into()),
        });
        let snapshot = tasks.snapshot(task.id()).unwrap();
        assert_eq!(snapshot.stage, TaskStage::Complete);
        assert_eq!(snapshot.result.unwrap()["id"], "fixture-version");
        assert_eq!(snapshot.error.as_deref(), Some("保存失败"));
        assert!(snapshot.finished_at.unwrap() >= snapshot.created_at);
        assert!(tasks.active().is_none());
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                TaskStage::Preparing,
                TaskStage::Processing,
                TaskStage::Complete
            ]
        );
    }
}
