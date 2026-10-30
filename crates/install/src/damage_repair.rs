//! Explicit damaged dependency repair. Metadata is captured locally, then each
//! declared dependency is hashed again. Only a mismatching regular file with a
//! verified allowed download can enter the persistent replacement transaction.
//!
//! A synced independent byte backup precedes every replacement. Uncommitted
//! renames roll back on failure/cancellation; committed files and all backups
//! remain. An interrupted record blocks new writers until explicit recovery.
//! Missing files still belong to MissingRepair; mods and saves are excluded.
use super::*;
use std::{
    io::{Seek, SeekFrom},
    time::{SystemTime, UNIX_EPOCH},
};

#[path = "damage_store.rs"]
mod store;
use store::{fingerprint, Fingerprint, Operation, Store};

#[derive(Clone, Debug, Serialize)]
pub struct DamageRepairReport {
    pub id: String,
    pub operation_id: String,
    pub backup_directory: String,
    pub files_repaired: u64,
    pub files_already_valid: u64,
    pub files_missing: u64,
    pub unsupported_total: usize,
    pub unsupported: Vec<RepairUnsupported>,
    pub assets_enumerated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DamageRecoveryReport {
    pub recovered: usize,
}

pub struct DamageRepair {
    captured: MissingRepair,
    operation_id: String,
}
static NEXT_DAMAGE_ID: AtomicU64 = AtomicU64::new(0);

impl DamageRepair {
    pub fn capture_bound(root: fs::File, root_display: impl AsRef<Path>, id: &str) -> Result<Self> {
        Self::ensure_ready(root_display.as_ref())?;
        let captured =
            MissingRepair::capture(root, root_display.as_ref(), id, CaptureMode::Verify)?;
        let operation_id = format!(
            "d-{:x}-{:x}-{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(error)?
                .as_nanos(),
            std::process::id(),
            NEXT_DAMAGE_ID.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Self {
            captured,
            operation_id,
        })
    }
    pub fn id(&self) -> &str {
        self.captured.id()
    }
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Read-only guard for launch and every cooperating root writer. Only the
    /// current task's exact captured operation ID may be excluded internally.
    pub fn ensure_ready(root: &Path) -> Result<()> {
        Self::ensure_ready_except(root, None)
    }
    pub fn ensure_ready_except(root: &Path, operation_id: Option<&str>) -> Result<()> {
        if store::store_absent(root)? {
            return Ok(());
        }
        let Some(store) = Store::open(store::open_root(root)?, root, false, false)? else {
            return Ok(());
        };
        for operation in store.operations()? {
            if Some(operation.id()) != operation_id && !operation.done()? {
                return Err("依赖损坏修复存在未完成记录，请先恢复原文件后再继续".into());
            }
        }
        Ok(())
    }
    pub fn pending(root: &Path) -> Result<usize> {
        if store::store_absent(root)? {
            return Ok(0);
        }
        let Some(store) = Store::open(store::open_root(root)?, root, false, false)? else {
            return Ok(0);
        };
        store
            .operations()?
            .into_iter()
            .try_fold(0, |count, op| Ok(count + usize::from(!op.done()?)))
    }
    pub fn recover_pending_bound(
        root: fs::File,
        root_display: impl AsRef<Path>,
        cancel: &AtomicBool,
        fast_guard: &(impl Fn() -> Result<()> + Send + Sync),
        validate_binding: &(impl Fn() -> Result<()> + Send + Sync),
        on_progress: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<DamageRecoveryReport> {
        check(cancel)?;
        validate_binding()?;
        let Some(mut store) = Store::open(root, root_display.as_ref(), false, true)? else {
            return Ok(DamageRecoveryReport { recovered: 0 });
        };
        let operations = store.operations()?;
        let mut recovered = 0;
        for operation in operations {
            check(cancel)?;
            fast_guard()?;
            validate_binding()?;
            if operation.done()? {
                continue;
            }
            // Cancellation is observed between complete rollback boundaries.
            // Once a move starts, filesystem-bound restoration drains even if
            // application generations have retired the initiating request.
            store::recover(&mut store, &operation, &|| {
                check(cancel)?;
                fast_guard()?;
                validate_binding()
            })?;
            recovered += 1;
            on_progress(Progress {
                stage: "damage-recover".into(),
                message: "正在恢复未完成的依赖修复".into(),
                completed: recovered as u64,
                total: recovered as u64,
                bytes_done: 0,
                bytes_total: 0,
                network_bytes: 0,
                steps: Vec::new(),
            });
        }
        validate_binding()?;
        store.verify()?;
        Ok(DamageRecoveryReport { recovered })
    }
}

struct RepairRun<'a> {
    installer: &'a Installer,
    request: &'a DamageRepair,
    cancel: &'a AtomicBool,
    fast_guard: &'a (dyn Fn() -> Result<()> + Send + Sync),
    validate_binding: &'a (dyn Fn() -> Result<()> + Send + Sync),
    on_progress: &'a (dyn Fn(Progress) + Send + Sync),
    progress: RepairProgress,
    report: DamageRepairReport,
    store: Option<Store>,
    operation: Option<Operation>,
    backup_bytes: u64,
    last_verify: Mutex<Instant>,
}

impl Installer {
    /// Caller holds exclusive root writer admission and authorizes replacing
    /// damaged declared dependencies. No UI report is accepted as authority.
    pub fn repair_damaged(
        &self,
        request: &DamageRepair,
        cancel: &AtomicBool,
        fast_guard: &(impl Fn() -> Result<()> + Send + Sync),
        validate_binding: &(impl Fn() -> Result<()> + Send + Sync),
        on_progress: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<DamageRepairReport> {
        let mut state = request
            .captured
            .state
            .lock()
            .map_err(|_| "损坏修复捕获锁损坏")?;
        validate(&state, cancel, validate_binding, true)?;
        DamageRepair::ensure_ready(Path::new(&state.filesystem.display))?;
        let mut run = RepairRun {
            installer: self,
            request,
            cancel,
            fast_guard,
            validate_binding,
            on_progress,
            progress: RepairProgress::default(),
            report: DamageRepairReport {
                id: request.id().into(),
                operation_id: request.operation_id().into(),
                backup_directory: format!("{}/{}", store::STORE, request.operation_id()),
                files_repaired: 0,
                files_already_valid: 0,
                files_missing: 0,
                unsupported_total: 0,
                unsupported: Vec::new(),
                assets_enumerated: request.captured.index.is_none()
                    || request.captured.captured_index.is_some(),
            },
            store: None,
            operation: None,
            backup_bytes: 0,
            last_verify: Mutex::new(Instant::now() - Duration::from_secs(1)),
        };
        let result = run.execute(&mut state);
        // No external application guard is used for rollback: retiring the
        // request must not strand a gap that this exact filesystem capability
        // can safely restore. Conflicts preserve every file and stay pending.
        if let (Some(store), Some(operation)) = (&mut run.store, &run.operation) {
            if let Err(recovery) = store::recover(store, operation, &|| Ok(())) {
                return Err(format!(
                    "取消清理失败：{}；恢复未完成，原件与备份均已保留：{recovery}",
                    result
                        .as_ref()
                        .err()
                        .map(String::as_str)
                        .unwrap_or("损坏修复已停止")
                ));
            }
        }
        result?;
        Ok(run.report)
    }
}

impl RepairRun<'_> {
    fn poll(&self, state: &RepairState) -> Result<()> {
        check(self.cancel)?;
        (self.fast_guard)()?;
        let mut last = self.last_verify.lock().map_err(|_| "损坏修复检查锁损坏")?;
        if last.elapsed() >= Duration::from_millis(100) {
            state.verify_fast()?;
            if let Some(store) = &self.store {
                store.verify()?;
            }
            *last = Instant::now();
        }
        Ok(())
    }
    fn emit(&self, phase: &str, message: &str) {
        let mut progress = self.progress.event(phase, message);
        progress.steps = [
            ("damage-check", "检查损坏依赖"),
            ("damage-download", "下载并验证替换文件"),
            ("damage-commit", "备份并替换损坏文件"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (id, label))| {
            let current = match phase {
                "damage-check" => 0,
                "damage-download" => 1,
                _ => 2,
            };
            InstallStep {
                id: id.into(),
                label: label.into(),
                state: if phase == "complete" || index < current {
                    "complete"
                } else if index == current {
                    "running"
                } else {
                    "pending"
                }
                .into(),
                progress: None,
            }
        })
        .collect();
        (self.on_progress)(progress);
    }
    fn unsupported(&mut self, file: &RequiredFile, reason: &str) {
        self.report.unsupported_total += 1;
        if self.report.unsupported.len() < UNSUPPORTED_LIMIT {
            self.report.unsupported.push(RepairUnsupported {
                path: bounded(&file.label),
                reason: bounded(reason),
            });
        }
    }
    fn open(&self, state: &RepairState, path: &str) -> Result<fs::File> {
        let parent = state
            .filesystem
            .directories
            .get(split(path).0)
            .ok_or("损坏修复目录未绑定")?;
        store::open_regular(parent, split(path).1)
    }
    fn operation(&mut self, state: &RepairState) -> Result<()> {
        if self.store.is_none() {
            let mut store = Store::open(
                state.filesystem.directories[""]
                    .try_clone()
                    .map_err(error)?,
                &state.filesystem.display,
                true,
                true,
            )?
            .ok_or("无法创建损坏修复记录")?;
            for old in store.operations()? {
                if !old.done()? {
                    return Err("存在未完成的依赖修复，请先恢复".into());
                }
            }
            let operation = store.begin(self.request.operation_id(), self.request.id())?;
            self.store = Some(store);
            self.operation = Some(operation);
        }
        Ok(())
    }
    fn execute(&mut self, state: &mut RepairState) -> Result<()> {
        let mut files = Vec::new();
        files.extend(self.request.captured.index.iter().cloned());
        files.extend(self.request.captured.files.iter().cloned());
        let mut files = manifest::deduplicate(files)?;
        self.progress.total = files.len() as u64;
        self.emit("damage-check", "正在检查已有依赖的 SHA1 和大小");
        let mut index = 0;
        while index < files.len() {
            self.poll(state)?;
            let required = files[index].clone();
            index += 1;
            let Some(path) = &required.relative else {
                self.unsupported(
                    &required,
                    required
                        .unsupported
                        .as_deref()
                        .unwrap_or("无法安全定位依赖"),
                );
                continue;
            };
            if !state.filesystem.inspect(path)?.present() {
                self.report.files_missing += 1;
                self.progress.completed += 1;
                continue;
            }
            let Some(download) = &required.download else {
                self.unsupported(
                    &required,
                    required
                        .unsupported
                        .as_deref()
                        .unwrap_or("缺少可信校验与下载声明"),
                );
                self.progress.completed += 1;
                continue;
            };
            if let Err(reason) = self.installer.repair_download_eligibility(&required, state) {
                self.unsupported(&required, &reason);
                self.progress.completed += 1;
                continue;
            }
            let parent = state
                .filesystem
                .directories
                .get(split(path).0)
                .ok_or("损坏修复目录未绑定")?;
            let observed_metadata =
                entry_metadata(parent, split(path).1)?.ok_or("依赖在损坏检查前已移除")?;
            if observed_metadata.links != 1
                || observed_metadata.len > manifest::MAX_REPAIR_FILE_BYTES
            {
                self.unsupported(&required, "原文件有硬链接或超过 512 MiB 独立备份上限");
                self.progress.completed += 1;
                continue;
            }
            let mut original_file = self.open(state, path)?;
            let observed = fingerprint(&mut original_file, &|| self.poll(state))?;
            if state.filesystem.stamp(path)? != Some(observed.stamp) {
                return Err("依赖在损坏检查期间变化，请重新检查".into());
            }
            if observed.stamp.len == download.size && observed.sha1 == download.sha1 {
                self.report.files_already_valid += 1;
                self.progress.completed += 1;
                continue;
            }
            self.backup_bytes = self
                .backup_bytes
                .checked_add(observed.stamp.len)
                .ok_or("原件备份总大小溢出")?;
            if self.backup_bytes > MAX_MISSING_BYTES {
                return Err("单次损坏修复原件备份超过 8 GiB 上限".into());
            }
            self.operation(state)?;
            self.replace(state, &required, &mut original_file, observed)?;
            self.report.files_repaired += 1;
            self.progress.completed += 1;
            if self
                .request
                .captured
                .index
                .as_ref()
                .and_then(|index| index.relative.as_ref())
                == Some(path)
            {
                let snapshot = state
                    .filesystem
                    .snapshot(path, manifest::MAX_ASSET_INDEX_BYTES)?;
                let value = json(&snapshot.bytes)?;
                files.extend(manifest::asset_objects(&value)?);
                files = manifest::deduplicate(files)?;
                state.metadata.retain(|old| old.relative != *path);
                state.metadata.push(snapshot);
                for file in &files {
                    if let Some(path) = &file.relative {
                        state.filesystem.inspect(path)?;
                    }
                }
                self.progress.total = files.len() as u64;
                self.report.assets_enumerated = true;
            }
            validate(state, self.cancel, self.validate_binding, true)?;
            self.emit("damage-commit", "已备份并修复损坏依赖");
        }
        if !self.report.assets_enumerated {
            self.unsupported(
                &RequiredFile {
                    relative: None,
                    label: "assets/objects".into(),
                    download: None,
                    unsupported: None,
                },
                "资源索引未能可信枚举，需修复索引后重新检查",
            );
        }
        validate(state, self.cancel, self.validate_binding, true)?;
        self.emit("complete", "损坏依赖修复完成，原件备份已保留");
        Ok(())
    }
    fn replace(
        &mut self,
        state: &RepairState,
        required: &RequiredFile,
        original_file: &mut fs::File,
        original: Fingerprint,
    ) -> Result<()> {
        let path = required.relative.as_ref().ok_or("损坏修复路径缺失")?;
        let download = required.download.as_ref().ok_or("损坏修复下载声明缺失")?;
        let ordinal = self.report.files_repaired as usize;
        let operation = self.operation.as_ref().ok_or("损坏修复记录未创建")?;
        let directory = operation.directory();
        let mut backup = RepairStage::new(
            directory.clone(),
            &format!("backup-{ordinal}"),
            original.stamp.len,
            operation.staging_area(),
        )?;
        original_file.seek(SeekFrom::Start(0)).map_err(error)?;
        let mut received = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        while received < original.stamp.len {
            self.poll(state)?;
            let max = (original.stamp.len - received).min(buffer.len() as u64) as usize;
            let n = original_file.read(&mut buffer[..max]).map_err(error)?;
            if n == 0 {
                return Err("原件备份读取期间变化".into());
            }
            backup.file_mut().write_all(&buffer[..n]).map_err(error)?;
            received += n as u64;
        }
        let copied = fingerprint(backup.file_mut(), &|| self.poll(state))?;
        if copied.sha256 != original.sha256
            || Stamp::of(original_file).map_err(error)? != original.stamp
            || state.filesystem.stamp(path)? != Some(original.stamp)
        {
            return Err("原件备份期间源文件变化".into());
        }
        if !backup.publish()? {
            return Err("原件备份已存在，拒绝覆盖".into());
        }
        self.emit("damage-check", "原件备份已保存，正在复核备份内容");
        let mut backup_file = store::open_regular(&directory, &format!("backup-{ordinal}"))?;
        let backup = fingerprint(&mut backup_file, &|| self.poll(state))?;
        // Publishing names the inode in the root. Reopening must still match
        // the original bytes; a new fingerprint alone could bless an external
        // edit made between publication and this read as the durable backup.
        if backup.sha256 != original.sha256 || backup.stamp.len != original.stamp.len {
            return Err("原件备份发布后内容变化，拒绝替换源文件".into());
        }
        let mut incoming = RepairStage::new(
            directory.clone(),
            &format!("incoming-{ordinal}"),
            download.size,
            operation.staging_area(),
        )?;
        self.emit("damage-download", "正在下载并校验损坏依赖的替换文件");
        self.progress.total_bytes = self
            .progress
            .total_bytes
            .checked_add(download.size)
            .ok_or("修复下载总大小溢出")?;
        if self.progress.total_bytes > MAX_MISSING_BYTES {
            return Err("损坏修复下载超过 8 GiB 上限".into());
        }
        self.installer.repair_fetch(
            download,
            state,
            self.cancel,
            &|| (self.fast_guard)(),
            &mut self.progress,
            &|mut progress| {
                progress.stage = "damage-download".into();
                progress.message = "正在下载损坏依赖的替换文件".into();
                (self.on_progress)(progress);
            },
            |chunk| incoming.file_mut().write_all(chunk).map_err(error),
        )?;
        self.progress.bytes += download.size;
        let replacement = fingerprint(incoming.file_mut(), &|| self.poll(state))?;
        // A newly downloaded index must be structurally valid before it can
        // replace the old index or authorize any asset object destination.
        if self
            .request
            .captured
            .index
            .as_ref()
            .and_then(|index| index.relative.as_ref())
            == Some(path)
        {
            incoming
                .file_mut()
                .seek(SeekFrom::Start(0))
                .map_err(error)?;
            let mut bytes = Vec::new();
            incoming
                .file_mut()
                .take(manifest::MAX_ASSET_INDEX_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(error)?;
            manifest::asset_objects(&json(&bytes)?)?;
        }
        if !incoming.publish()? {
            return Err("替换暂存文件已存在，拒绝覆盖".into());
        }
        let mut incoming_file = store::open_regular(&directory, &format!("incoming-{ordinal}"))?;
        let incoming = fingerprint(&mut incoming_file, &|| self.poll(state))?;
        if incoming.sha256 != replacement.sha256 {
            return Err("替换文件发布后变化".into());
        }
        validate(state, self.cancel, self.validate_binding, true)?;
        if Stamp::of(original_file).map_err(error)? != original.stamp
            || state.filesystem.stamp(path)? != Some(original.stamp)
        {
            return Err("原文件在下载期间变化，拒绝替换".into());
        }
        let intent =
            operation.prepare(ordinal, path, &state.filesystem, original, incoming, backup)?;
        self.emit("damage-commit", "正在持久备份并替换损坏依赖");
        check(self.cancel)?;
        (self.fast_guard)()?;
        let store = self.store.as_mut().ok_or("损坏修复存储未创建")?;
        store::publish(store, operation, &intent, &|| {
            check(self.cancel)?;
            (self.fast_guard)()?;
            (self.validate_binding)()?;
            check(self.cancel)
        })
    }
}

#[cfg(test)]
#[path = "damage_repair_tests.rs"]
mod tests;
