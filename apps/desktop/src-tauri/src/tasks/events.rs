//! Rate-limit observational progress events without delaying scheduler writes.
//! Boundary changes are immediate. No timer, snapshot payload or terminal
//! history is retained here; consumers still read the authoritative task list.
use super::{TaskSnapshot, TaskStage};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const MAX_TRACKED: usize = 64;

struct Sent {
    at: Instant,
    stage: TaskStage,
    phase: String,
    can_cancel: bool,
    display_name: Option<String>,
}

#[derive(Default)]
pub(crate) struct TaskEventGate {
    sent: HashMap<String, Sent>,
}
impl TaskEventGate {
    pub(crate) fn accept(&mut self, task: &TaskSnapshot, now: Instant) -> bool {
        if task.stage.is_terminal() {
            self.sent.remove(&task.id);
            return true;
        }
        if self.sent.get(&task.id).is_some_and(|last| {
            last.stage == task.stage
                && last.phase == task.phase
                && last.can_cancel == task.can_cancel
                && last.display_name == task.display_name
                && now.saturating_duration_since(last.at) < PROGRESS_INTERVAL
        }) {
            return false;
        }
        if self.sent.len() >= MAX_TRACKED && !self.sent.contains_key(&task.id) {
            // The scheduler currently admits fewer live tasks. If that changes,
            // forget rate-limit hints rather than dropping lifecycle events.
            self.sent.clear();
        }
        self.sent.insert(
            task.id.clone(),
            Sent {
                at: now,
                stage: task.stage,
                phase: task.phase.clone(),
                can_cancel: task.can_cancel,
                display_name: task.display_name.clone(),
            },
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskKind, TaskOutcome, TaskProgress, TaskTarget, Tasks};
    use std::sync::{Arc, Mutex};

    fn progress(count: u64) -> TaskProgress {
        TaskProgress {
            stage: TaskStage::Downloading,
            phase: "download".into(),
            message: "progress".into(),
            completed: count,
            total: 10_000,
            bytes_done: count,
            bytes_total: 10_000,
            network_bytes: count,
            steps: vec![],
        }
    }
    fn task() -> (Arc<Tasks>, super::super::TaskHandle) {
        let tasks = Arc::new(Tasks::new());
        let task = tasks
            .admit(
                TaskTarget {
                    root_id: "event-fixture".into(),
                    root_path: "event-fixture".into(),
                    instance_id: Some("fixture".into()),
                },
                TaskKind::ResourceDownload,
            )
            .unwrap();
        (tasks, task)
    }

    #[test]
    fn real_progress_keeps_every_scheduler_update_but_bounds_wire_serialization() {
        let (tasks, task) = task();
        let now = Instant::now();
        let gate = Arc::new(Mutex::new(TaskEventGate::default()));
        let observed = Arc::new(Mutex::new((0usize, 0usize, 0usize, 0usize)));
        let observed_copy = observed.clone();
        tasks.set_listener(move |snapshot| {
            let clock = now + Duration::from_micros(snapshot.completed * 100);
            let mut result = observed_copy.lock().unwrap();
            result.0 += 1;
            result.1 += serde_json::to_vec(&snapshot).unwrap().len();
            if gate.lock().unwrap().accept(&snapshot, clock) {
                result.2 += 1;
                result.3 += serde_json::to_vec(&snapshot).unwrap().len();
            }
        });
        for count in 0..10_000 {
            task.update(progress(count)).unwrap();
        }
        assert_eq!(tasks.snapshot(task.id()).unwrap().completed, 9_999);
        task.finish(TaskOutcome::Complete {
            result: None,
            message: "done".into(),
            error: None,
        });
        let result = *observed.lock().unwrap();
        assert_eq!(result.0, 10_001);
        assert_eq!(result.2, 11);
        assert!(result.3 < result.1 / 100);
        eprintln!(
            "synthetic task progress: events {} -> {}, serialized bytes {} -> {}",
            result.0, result.2, result.1, result.3
        );
        assert_eq!(
            tasks.snapshot(task.id()).unwrap().stage,
            TaskStage::Complete
        );
    }

    #[test]
    fn cancellation_titles_phases_and_terminal_boundaries_bypass_the_interval() {
        let (tasks, task) = task();
        let now = Instant::now();
        let mut gate = TaskEventGate::default();
        let mut view = tasks.snapshot(task.id()).unwrap();
        assert!(gate.accept(&view, now));
        assert!(!gate.accept(&view, now));
        for change in 0..4 {
            match change {
                0 => view.stage = TaskStage::Downloading,
                1 => view.phase = "install".into(),
                2 => view.can_cancel = false,
                _ => view.display_name = Some("actual resource".into()),
            }
            assert!(gate.accept(&view, now));
        }
        for terminal in [TaskStage::Complete, TaskStage::Cancelled, TaskStage::Error] {
            view.stage = terminal;
            assert!(gate.accept(&view, now));
            assert!(gate.sent.is_empty());
        }
        assert!(gate.accept(&tasks.snapshot(task.id()).unwrap(), now));
        assert!(gate.accept(&tasks.snapshot(task.id()).unwrap(), now + PROGRESS_INTERVAL));
    }

    #[test]
    fn hints_are_bounded_and_do_not_starve_another_task() {
        let (tasks, task) = task();
        let now = Instant::now();
        let mut gate = TaskEventGate::default();
        let mut view = tasks.snapshot(task.id()).unwrap();
        for id in 0..MAX_TRACKED * 2 {
            view.id = format!("fixture-{id}");
            assert!(gate.accept(&view, now));
            assert!(gate.sent.len() <= MAX_TRACKED);
        }
    }
}
