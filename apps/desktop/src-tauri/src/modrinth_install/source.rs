//! Retained generated network/copy sources. Linux preserves its anonymous File
//! contract; native named leases retain their registry Ticket until publish or
//! durable discard. A raw named File cannot construct this capability.
#[cfg(target_os = "linux")]
pub(crate) type Input = std::fs::File;

/// Holds every writable source until transfers finish. Futures only borrow these
/// inputs; cancellation drops their borrows before this batch retires ownership.
pub(crate) struct InputBatch {
    inputs: Vec<Input>,
}
impl InputBatch {
    pub(crate) fn from_inputs(inputs: Vec<Input>) -> Self {
        Self { inputs }
    }
    pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, Input> {
        self.inputs.iter_mut()
    }
    pub(crate) fn into_inputs(mut self) -> Vec<Input> {
        std::mem::take(&mut self.inputs)
    }
    pub(crate) fn retire(&mut self) -> std::result::Result<(), String> {
        #[cfg(not(target_os = "linux"))]
        return retire_batch(self.inputs.iter_mut());
        #[cfg(target_os = "linux")]
        {
            self.inputs.clear();
            Ok(())
        }
    }
}
impl Drop for InputBatch {
    fn drop(&mut self) {
        let _ = self.retire();
    }
}

#[cfg(not(target_os = "linux"))]
mod native {
    use pcl_core::retained_fs::{self, Directory, Kind, Leaf};
    use pcl_install::staging::{
        AllocationBatchError, BatchOwner, DiscardBatchError, DiscardOutcome, Limits, Outcome,
        Ticket,
    };
    use std::{
        collections::BTreeMap,
        fs::File,
        io::{Read, Seek, SeekFrom, Write},
        ops::{Deref, DerefMut},
        sync::{atomic::AtomicBool, Arc, Mutex, OnceLock, Weak},
    };
    type Result<T> = std::result::Result<T, String>;
    pub(crate) struct Area {
        registry: pcl_install::staging::Area,
        budget: u64,
        failures: Mutex<Vec<String>>,
        // Scoped ownership only: no global strong Arc, and a failed Area admits
        // no new allocations. These actual owners close when this scope ends.
        retained: Mutex<Vec<Retained>>,
    }
    #[allow(dead_code)] // Fields retain native capabilities until Area scope ends.
    enum Retained {
        Allocation(AllocationBatchError),
        Discard(DiscardBatchError),
        Outcome(DiscardOutcome),
        Publication(Outcome),
        Owners(Vec<BatchOwner>),
    }
    impl Area {
        fn retain(&self, owner: Retained) {
            self.retained
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(owner);
        }
        fn fail(&self, error: String) {
            if let Ok(mut failures) = self.failures.lock() {
                if failures.len() < 128 {
                    failures.push(error);
                }
            }
        }
        pub(crate) fn check_cleanup(&self) -> Result<()> {
            let failures = self.failures.lock().map_err(|_| "资源来源清理状态不可用")?;
            if failures.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "登记资源来源清理未确认，原件与证据已保留：{}",
                    failures.join("；")
                ))
            }
        }
        #[cfg(test)]
        pub(crate) fn pending(&self) -> Result<usize> {
            self.registry
                .report()
                .map(|report| report.pending_tickets)
                .map_err(|error| error.to_string())
        }
    }
    fn directory(file: &File) -> Result<Directory> {
        Directory::from_file(file.try_clone().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }
    pub(crate) fn open_area(parent: &File, name: &str, byte_budget: u64) -> Result<Arc<Area>> {
        if byte_budget == 0 {
            return Err("资源来源配额无效".into());
        }
        let parent = directory(parent)?;
        parent
            .metadata()
            .and_then(|metadata| metadata.require_private())
            .map_err(|error| error.to_string())?;
        let leaf = Leaf::new(name).map_err(|error| error.to_string())?;
        let key =
            serde_json::to_string(&(parent.identity().map_err(|error| error.to_string())?, name))
                .map_err(|error| error.to_string())?;
        type Pool = Mutex<BTreeMap<String, Weak<Area>>>;
        static POOL: OnceLock<Pool> = OnceLock::new();
        let mut pool = POOL
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .map_err(|_| "资源来源登记池不可用")?;
        pool.retain(|_, weak| weak.strong_count() != 0);
        if let Some(area) = pool.get(&key).and_then(Weak::upgrade) {
            if area.budget != byte_budget {
                return Err("资源来源登记区配额与当前能力不一致".into());
            }
            area.check_cleanup()?;
            return Ok(area);
        }
        let mut limits = Limits::default();
        limits.area_bytes = byte_budget
            .checked_add(limits.log_bytes)
            .ok_or("资源来源配额溢出")?;
        let registry = pcl_install::staging::Area::open_at_private(&parent, &leaf, true, limits)
            .map_err(|error| error.to_string())?;
        registry
            .reconcile_discarded()
            .map_err(|error| error.to_string())?;
        let report = registry.report().map_err(|error| error.to_string())?;
        if report.pending_tickets != 0 || !report.writable() {
            return Err(format!("资源来源登记区 {name} 含 {} 个未退役 ticket、{} 项保留证据；来源文件未删除，请先核对登记区", report.pending_tickets, report.issue_count));
        }
        let area = Arc::new(Area {
            registry,
            budget: byte_budget,
            failures: Mutex::new(Vec::new()),
            retained: Mutex::new(Vec::new()),
        });
        pool.insert(key, Arc::downgrade(&area));
        Ok(area)
    }
    pub struct Input {
        ticket: Option<Ticket>,
        area: Arc<Area>,
    }
    pub(crate) fn allocate(area: &Arc<Area>, target: &File, name: &str, max: u64) -> Result<Input> {
        if max > 2 * 1024 * 1024 * 1024 {
            return Err("资源暂存声明超过 2 GiB 限制".into());
        }
        area.check_cleanup()?;
        let ticket = area
            .registry
            .allocate(
                &directory(target)?,
                &Leaf::new(name).map_err(|error| error.to_string())?,
                max,
            )
            .map_err(|error| error.to_string())?;
        Ok(Input {
            ticket: Some(ticket),
            area: area.clone(),
        })
    }
    pub(crate) fn allocate_batch(
        area: &Arc<Area>,
        target: &File,
        requests: &[(String, u64)],
        cancel: &AtomicBool,
    ) -> Result<super::InputBatch> {
        area.check_cleanup()?;
        if requests.len() > 512 {
            return Err("资源批次来源数量超过512项".into());
        }
        let mut total = 0u64;
        for (_, max) in requests {
            total = total.checked_add(*max).ok_or("资源来源批次大小溢出")?;
            if total > 8 * 1024 * 1024 * 1024 {
                return Err("资源来源批次超过8GiB限制".into());
            }
        }
        let target = directory(target)?;
        let requests = requests
            .iter()
            .map(|(name, max)| {
                if *max == 0 || *max > 2 * 1024 * 1024 * 1024 {
                    return Err("资源暂存声明超过 2 GiB 限制".into());
                }
                Ok((Leaf::new(name).map_err(|e| e.to_string())?, *max))
            })
            .collect::<Result<Vec<_>>>()?;
        match area.registry.allocate_batch(&target, &requests, cancel) {
            Ok(tickets) => Ok(super::InputBatch::from_inputs(
                tickets
                    .into_iter()
                    .map(|ticket| Input {
                        ticket: Some(ticket),
                        area: area.clone(),
                    })
                    .collect(),
            )),
            Err(failure)
                if failure.not_eligible
                    && failure.allocated.is_empty()
                    && failure.retained_stage.is_none()
                    && failure.uncertain_allocation.is_none() =>
            {
                // Only this zero-effect admission failure can use old singles.
                let mut batch = super::InputBatch::from_inputs(Vec::new());
                for (name, max) in requests {
                    if cancel.load(std::sync::atomic::Ordering::Acquire) {
                        return Err("资源下载已取消".into());
                    }
                    let ticket = area
                        .registry
                        .allocate(&target, &name, max)
                        .map_err(|e| e.to_string())?;
                    batch.inputs.push(Input {
                        ticket: Some(ticket),
                        area: area.clone(),
                    });
                }
                Ok(batch)
            }
            Err(failure) => {
                let message = format!("资源批次分配未确认，登记与来源已保留：{failure}");
                area.fail(message.clone());
                area.retain(Retained::Allocation(failure));
                Err(message)
            }
        }
    }
    pub(crate) fn retire_batch<'a>(inputs: impl IntoIterator<Item = &'a mut Input>) -> Result<()> {
        let mut groups: BTreeMap<usize, (Arc<Area>, Vec<Ticket>)> = BTreeMap::new();
        for input in inputs {
            let group = groups
                .entry(Arc::as_ptr(&input.area) as usize)
                .or_insert_with(|| (input.area.clone(), Vec::new()));
            if let Some(ticket) = input.ticket.take() {
                group.1.push(ticket);
            }
        }
        let mut errors = Vec::new();
        for (_, (area, tickets)) in groups {
            let count = tickets.len();
            match area
                .registry
                .discard_batch(tickets, &AtomicBool::new(false))
            {
                Ok(_) => {}
                Err(failure)
                    if failure.not_eligible
                        && failure.discarded.is_empty()
                        && failure.uncertain.is_none()
                        && failure.remaining.len() == count
                        && failure
                            .remaining
                            .iter()
                            .all(|owner| matches!(owner, BatchOwner::Ticket(_))) =>
                {
                    // Admission returned every original writable owner unchanged.
                    let mut remaining = failure.remaining.into_iter();
                    while let Some(owner) = remaining.next() {
                        let BatchOwner::Ticket(ticket) = owner else {
                            unreachable!()
                        };
                        match ticket.discard() {
                            Ok(DiscardOutcome::Discarded { .. }) => {}
                            Ok(outcome @ DiscardOutcome::MayHaveDiscarded { .. }) => {
                                let message =
                                    "MayHaveDiscarded：资源来源退役未确认，登记证据已保留"
                                        .to_string();
                                area.fail(message.clone());
                                area.retain(Retained::Outcome(outcome));
                                errors.push(message);
                                area.retain(Retained::Owners(remaining.collect()));
                                break;
                            }
                            Err(error) => {
                                area.fail(error.to_string());
                                errors.push(error.to_string());
                                area.retain(Retained::Owners(remaining.collect()));
                                break;
                            }
                        }
                    }
                }
                Err(failure) => {
                    let message =
                        format!("资源批次退役未确认，剩余原生所有权与登记证据已保留：{failure}");
                    area.fail(message.clone());
                    area.retain(Retained::Discard(failure));
                    errors.push(message);
                }
            }
            if let Err(error) = area.check_cleanup() {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("；"))
        }
    }
    impl Input {
        pub(crate) fn verify_owned(&self) -> Result<()> {
            let ticket = self.ticket.as_ref().ok_or("资源暂存已尝试发布或退役")?;
            let metadata =
                retained_fs::metadata(ticket.file()).map_err(|error| error.to_string())?;
            metadata
                .require_private()
                .map_err(|error| error.to_string())?;
            let allocation = ticket.allocation();
            if metadata.identity != allocation.source_identity
                || metadata.kind != Kind::Regular
                || metadata.reparse_tag.is_some()
                || metadata.len > allocation.max_bytes
                || metadata.links
                    != if allocation.source_name.is_some() {
                        1
                    } else {
                        0
                    }
            {
                return Err("资源来源不是已登记的独占生成文件".into());
            }
            if let Some(name) = &allocation.source_name {
                self.area
                    .registry
                    .directory()
                    .recheck(
                        &Leaf::new(name).map_err(|error| error.to_string())?,
                        &metadata.identity,
                    )
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        }
        pub(crate) fn publish(&mut self, target: &File, name: &str) -> Result<()> {
            self.verify_owned()?;
            let target = directory(target)?;
            let leaf = Leaf::new(name).map_err(|error| error.to_string())?;
            let allocation = self.ticket.as_ref().ok_or("资源暂存已退役")?.allocation();
            if allocation.target_parent != target.identity().map_err(|error| error.to_string())?
                || allocation.target_leaf.as_os_str() != leaf.as_os_str()
            {
                return Err("资源发布目标与原生登记的父目录或叶名不同".into());
            }
            let ticket = self.ticket.take().ok_or("资源暂存已退役")?;
            match ticket.publish(&target, &leaf).map_err(|error| {
                self.area.fail(error.to_string());
                error.to_string()
            })? {
                Outcome::Published { .. } => Ok(()),
                Outcome::Exists { ticket } => {
                    match ticket.discard() {
                        Ok(DiscardOutcome::Discarded { .. }) => {}
                        Ok(outcome @ DiscardOutcome::MayHaveDiscarded { .. }) => {
                            self.area.fail(
                                "MayHaveDiscarded：资源来源退役未确认，登记证据已保留".into(),
                            );
                            self.area.retain(Retained::Outcome(outcome));
                        }
                        Err(error) => self.area.fail(error.to_string()),
                    }
                    Err("资源目标已存在，原文件已保留".into())
                }
                outcome @ Outcome::MayHavePublished { .. } => {
                    let Outcome::MayHavePublished { ref error, .. } = outcome else {
                        unreachable!()
                    };
                    let message =
                        format!("MayHavePublished：资源可能已发布，登记与现存文件已保留：{error}");
                    self.area.fail(message.clone());
                    self.area.retain(Retained::Publication(outcome));
                    Err(message)
                }
            }
        }
        pub(crate) fn retire(&mut self) -> Result<()> {
            let Some(ticket) = self.ticket.take() else {
                return Ok(());
            };
            let result = match ticket.discard().map_err(|error| error.to_string()) {
                Ok(DiscardOutcome::Discarded { .. }) => Ok(()),
                Ok(outcome @ DiscardOutcome::MayHaveDiscarded { .. }) => {
                    self.area.retain(Retained::Outcome(outcome));
                    Err("MayHaveDiscarded：资源来源退役未确认，登记证据已保留".into())
                }
                Err(error) => Err(error),
            };
            if let Err(error) = &result {
                self.area.fail(error.clone());
            }
            result
        }
    }
    impl Drop for Input {
        fn drop(&mut self) {
            let _ = self.retire();
        }
    }
    impl Deref for Input {
        type Target = File;
        fn deref(&self) -> &File {
            self.ticket
                .as_ref()
                .expect("live registered resource source")
                .file()
        }
    }
    impl DerefMut for Input {
        fn deref_mut(&mut self) -> &mut File {
            self.ticket
                .as_mut()
                .expect("live registered resource source")
                .file_mut()
        }
    }
    impl Read for Input {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.deref_mut().read(bytes)
        }
    }
    impl Write for Input {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.deref_mut().write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.deref_mut().flush()
        }
    }
    impl Seek for Input {
        fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
            self.deref_mut().seek(from)
        }
    }
}
#[cfg(not(target_os = "linux"))]
pub(crate) use native::*;

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;
    use pcl_core::retained_fs::{Directory, Leaf, Traversal};
    use std::io::Write;
    #[test]
    fn batch_sources_keep_writable_ownership_until_explicit_handoff_or_drop() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let private = root
            .create_private(&Leaf::new("private").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let file = private.try_clone_file().unwrap();
        let area = open_area(&file, "sources", 1024).unwrap();
        let requests = vec![("first".into(), 64), ("second".into(), 64)];
        let mut batch = allocate_batch(
            &area,
            &file,
            &requests,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        for source in batch.iter_mut() {
            source.write_all(b"actual batch source").unwrap();
            source.verify_owned().unwrap();
        }
        assert_eq!(area.pending().unwrap(), 2);
        let mut inputs = batch.into_inputs();
        assert_eq!(area.pending().unwrap(), 2);
        retire_batch(inputs.iter_mut()).unwrap();
        assert_eq!(area.pending().unwrap(), 0);
        area.check_cleanup().unwrap();
        drop(inputs);
        drop(
            allocate_batch(
                &area,
                &file,
                &requests,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap(),
        );
        assert_eq!(area.pending().unwrap(), 0);
        assert!(private
            .entry(&Leaf::new("first").unwrap())
            .unwrap()
            .is_none());
        assert!(private
            .entry(&Leaf::new("second").unwrap())
            .unwrap()
            .is_none());
        drop(area);
        assert_eq!(
            open_area(&file, "sources", 1024)
                .unwrap()
                .pending()
                .unwrap(),
            0
        );
    }
    #[test]
    fn batch_retirement_groups_real_areas_and_preserves_other_active_owner() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let private = root
            .create_private(&Leaf::new("private").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let file = private.try_clone_file().unwrap();
        let first = open_area(&file, "first-area", 1024).unwrap();
        let second = open_area(&file, "second-area", 1024).unwrap();
        let mut untouched = allocate(&first, &file, "untouched", 64).unwrap();
        untouched.write_all(b"retained other owner").unwrap();
        let mut selected = vec![
            allocate(&first, &file, "one", 64).unwrap(),
            allocate(&second, &file, "two", 64).unwrap(),
        ];
        retire_batch(selected.iter_mut()).unwrap();
        assert_eq!(first.pending().unwrap(), 1);
        assert_eq!(second.pending().unwrap(), 0);
        untouched.verify_owned().unwrap();
        untouched.retire().unwrap();
        first.check_cleanup().unwrap();
        second.check_cleanup().unwrap();
    }
    #[test]
    fn typed_ticket_refuses_retarget_and_publishes_only_its_registered_leaf() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let area_parent = root
            .create_private(
                &Leaf::new("area-parent").unwrap(),
                Traversal::SameFilesystem,
            )
            .unwrap();
        let target = root
            .create_private(&Leaf::new("target").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let area = open_area(&area_parent.try_clone_file().unwrap(), "sources", 1024).unwrap();
        let target_file = target.try_clone_file().unwrap();
        let mut input = allocate(&area, &target_file, "right", 64).unwrap();
        input.write_all(b"verified bytes").unwrap();
        input.sync_all().unwrap();
        assert_eq!(area.pending().unwrap(), 1);
        assert!(input.publish(&target_file, "wrong").is_err());
        input.verify_owned().unwrap();
        assert!(target
            .entry(&Leaf::new("wrong").unwrap())
            .unwrap()
            .is_none());
        input.publish(&target_file, "right").unwrap();
        assert_eq!(area.pending().unwrap(), 0);
        area.check_cleanup().unwrap();
        assert_eq!(
            std::fs::read(temporary.path().join("target/right")).unwrap(),
            b"verified bytes"
        );
    }
    #[test]
    fn dropping_the_last_input_durably_discards_before_area_release() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let parent = root
            .create_private(&Leaf::new("private").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let file = parent.try_clone_file().unwrap();
        let area = open_area(&file, "sources", 1024).unwrap();
        let mut input = allocate(&area, &file, "never-published", 64).unwrap();
        input.write_all(b"partial").unwrap();
        drop(input);
        area.check_cleanup().unwrap();
        assert_eq!(area.pending().unwrap(), 0);
        drop(area);
        let reopened = open_area(&file, "sources", 1024).unwrap();
        assert_eq!(reopened.pending().unwrap(), 0);
        assert!(parent
            .entry(&Leaf::new("never-published").unwrap())
            .unwrap()
            .is_none());
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod abandoned_tests {
    use super::*;
    use pcl_core::retained_fs::{Directory, Leaf, Traversal};
    use std::io::Write;
    #[test]
    fn reopening_unretired_registry_preserves_original_source_and_refuses_new_writer() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let private = root
            .create_private(&Leaf::new("private").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let mut limits = pcl_install::staging::Limits::default();
        limits.area_bytes = 1024 + limits.log_bytes;
        let registry = pcl_install::staging::Area::open_at_private(
            &private,
            &Leaf::new("sources").unwrap(),
            true,
            limits,
        )
        .unwrap();
        let mut ticket = registry
            .allocate(&private, &Leaf::new("never-published").unwrap(), 64)
            .unwrap();
        ticket
            .file_mut()
            .write_all(b"retained interrupted bytes")
            .unwrap();
        ticket.file_mut().sync_all().unwrap();
        let name = ticket.allocation().source_name.clone().unwrap();
        drop(ticket);
        assert_eq!(registry.report().unwrap().pending_tickets, 1);
        drop(registry);
        let error = match open_area(&private.try_clone_file().unwrap(), "sources", 1024) {
            Ok(_) => panic!("unretired source accepted"),
            Err(error) => error,
        };
        assert!(error.contains("sources"), "{error}");
        assert!(error.contains("1 个未退役"), "{error}");
        assert_eq!(
            std::fs::read(temporary.path().join("private/sources").join(name)).unwrap(),
            b"retained interrupted bytes"
        );
        assert!(private
            .entry(&Leaf::new("never-published").unwrap())
            .unwrap()
            .is_none());
    }
}
