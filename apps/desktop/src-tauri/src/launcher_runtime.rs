//! Best-effort launcher lifecycle effects. A failed counter or diagnostic write
//! is a visible warning, never a reason to turn a successful Java spawn into a
//! failed launch. Task listeners enqueue closed records; disk work cannot stall
//! an installer or acquire its operation lock in the reverse order.
use crate::{launcher_local::*, launcher_prefs::LauncherPreferences, tasks::*};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc, Arc, Mutex,
    },
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WarningKind {
    Network,
    Window,
    Visibility,
    Statistics,
    Diagnostics,
}

#[derive(Default)]
pub struct RuntimeWarnings(Mutex<BTreeMap<WarningKind, String>>);
impl RuntimeWarnings {
    pub fn set(&self, kind: WarningKind, message: Option<String>) {
        let mut warnings = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(message) = message {
            warnings.insert(kind, message);
        } else {
            warnings.remove(&kind);
        }
    }
    pub fn message(&self) -> Option<String> {
        let warnings = self.0.lock().unwrap_or_else(|e| e.into_inner());
        (!warnings.is_empty()).then(|| warnings.values().cloned().collect::<Vec<_>>().join("\n"))
    }
}

pub struct LocalRuntime {
    pub stats: StatsStore,
    pub shortcuts: ShortcutStore,
    pub warnings: Arc<RuntimeWarnings>,
    /// Window hints have an independent order: never hold the game operation
    /// lock while calling a compositor API that may dispatch to the UI thread.
    pub window_effects: Mutex<()>,
    policy: AtomicU8,
    diagnostics: mpsc::SyncSender<(DiagnosticPolicy, DiagnosticRecord)>,
    observed: Mutex<VecDeque<(String, TaskStage)>>,
}
impl LocalRuntime {
    #[cfg(test)]
    pub fn new(project: PathBuf, paths: XdgPaths, prefs: &LauncherPreferences) -> Self {
        let executable = project.join("start-native.sh");
        Self::with_resources(project.clone(), project, executable, paths, prefs)
    }
    pub fn with_resources(
        project: PathBuf,
        resources: PathBuf,
        executable: PathBuf,
        paths: XdgPaths,
        prefs: &LauncherPreferences,
    ) -> Self {
        let warnings = Arc::new(RuntimeWarnings::default());
        let (sender, receiver) = mpsc::sync_channel::<(DiagnosticPolicy, DiagnosticRecord)>(64);
        let diagnostics = Diagnostics::new(project.clone());
        let worker_warnings = warnings.clone();
        std::thread::spawn(move || {
            while let Ok((policy, record)) = receiver.recv() {
                let warning = diagnostics
                    .record(policy, record)
                    .err()
                    .map(|e| format!("本地诊断未能记录：{e}"));
                worker_warnings.set(WarningKind::Diagnostics, warning);
            }
        });
        let runtime = Self {
            stats: StatsStore::new(project.clone()),
            shortcuts: ShortcutStore::with_resources(project, resources, executable, paths),
            warnings,
            window_effects: Mutex::new(()),
            policy: AtomicU8::new(0),
            diagnostics: sender,
            observed: Mutex::new(VecDeque::new()),
        };
        runtime.apply_preferences(prefs);
        runtime
    }
    pub fn apply_preferences(&self, prefs: &LauncherPreferences) {
        let enabled = prefs.local_diagnostics_enabled || prefs.advanced.debug_mode;
        self.policy.store(
            u8::from(enabled) | (u8::from(prefs.advanced.debug_mode) << 1),
            Ordering::Release,
        );
    }
    pub fn opened(&self) {
        self.lifecycle(StatEvent::LauncherOpened, DiagnosticEvent::LauncherOpened);
    }
    pub fn game_spawned(&self) {
        self.lifecycle(StatEvent::GameSpawned, DiagnosticEvent::GameSpawned);
    }
    fn lifecycle(&self, stat: StatEvent, event: DiagnosticEvent) {
        self.warnings.set(
            WarningKind::Statistics,
            self.stats
                .increment(stat)
                .err()
                .map(|e| format!("启动统计未能保存：{e}")),
        );
        self.enqueue(DiagnosticRecord {
            event,
            stage: DiagnosticStage::Ready,
            duration_ms: None,
            code: Some(DiagnosticCode::Success),
        });
    }
    fn enqueue(&self, record: DiagnosticRecord) {
        let bits = self.policy.load(Ordering::Acquire);
        if bits & 1 == 0 {
            return;
        }
        let policy = DiagnosticPolicy {
            enabled: true,
            debug: bits & 2 != 0,
        };
        if !policy.debug
            && matches!(
                record.event,
                DiagnosticEvent::Stage | DiagnosticEvent::TaskStarted
            )
        {
            return;
        }
        if self.diagnostics.try_send((policy, record)).is_err() {
            self.warnings.set(
                WarningKind::Diagnostics,
                Some("本地诊断队列已满或记录服务不可用，部分记录未保存".into()),
            );
        }
    }
    pub fn task_changed(&self, task: &TaskSnapshot) {
        let mut observed = self.observed.lock().unwrap_or_else(|e| e.into_inner());
        let previous = observed.iter().position(|(id, _)| id == &task.id);
        let is_new = previous.is_none();
        if let Some(index) = previous {
            if observed[index].1 == task.stage {
                return;
            }
            observed.remove(index);
        }
        observed.push_back((task.id.clone(), task.stage));
        while observed.len() > 64 {
            observed.pop_front();
        }
        drop(observed);
        let terminal = task.stage.is_terminal();
        self.enqueue(DiagnosticRecord {
            event: if terminal {
                DiagnosticEvent::TaskFinished
            } else if is_new {
                DiagnosticEvent::TaskStarted
            } else {
                DiagnosticEvent::Stage
            },
            stage: match task.stage {
                TaskStage::Queued | TaskStage::Preparing => DiagnosticStage::Plan,
                TaskStage::Downloading => DiagnosticStage::Download,
                TaskStage::Processing => DiagnosticStage::Publish,
                _ => DiagnosticStage::Finished,
            },
            duration_ms: task
                .finished_at
                .map(|end| end.saturating_sub(task.created_at).min(86_400_000)),
            code: match task.stage {
                TaskStage::Complete => Some(DiagnosticCode::Success),
                TaskStage::Cancelled => Some(DiagnosticCode::Cancelled),
                TaskStage::Error => Some(DiagnosticCode::Failed),
                _ => None,
            },
        });
    }
}
