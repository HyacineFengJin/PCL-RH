//! One-use pack confirmation and scoped installation orchestration.
//!
//! Confirmation owns the source FD and provider-resolved core. Claim transfers
//! it to one queued worker; the cache TTL does not expire submitted intent.
//! Official archive inputs may be downloaded for confirmation. Game content
//! writes and pack dependency downloads wait for the installation worker's turn.
//! The transaction owns cleanup. Launcher references are checked at admission
//! and commit; long hashing/network work never holds the operations mutex.
use super::*;
use crate::{
    config::GameRoot,
    tasks::{TaskHandle, TaskKind, TaskOutcome, TaskScope, TaskTarget},
    Shared,
};
use pcl_install::{Installer, Progress, ResolvedInstallRequest};
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(600);
const MAX_ENTRIES: usize = 64;
const MAX_FDS: usize = 100;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ONE: usize = 4 * 1024 * 1024;
const MAX_PREPARING: usize = 4;
const MAX_INNER_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const INVALID: &str = "整合包确认已过期或已提交，请重新检查";
const CAPACITY: &str = "已提交的整合包任务占满确认缓存，请等待或取消任务";
#[derive(Default)]
pub(crate) struct ConfirmationCache {
    entries: Mutex<VecDeque<Record>>,
    budget: Arc<Mutex<ConfirmationUsage>>,
    active: Mutex<BTreeMap<PathBuf, String>>,
    pub(super) inputs: official::Inputs,
}
#[derive(Default)]
struct ConfirmationUsage {
    fds: usize,
    bytes: usize,
    inner_bytes: u64,
    preparing: usize,
}
struct Record {
    token: String,
    expires: Instant,
    authority: ConfirmedPack,
}
pub(super) struct Lease {
    budget: Arc<Mutex<ConfirmationUsage>>,
    bytes: usize,
    fds: usize,
    inner_bytes: u64,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut b = self.budget.lock().unwrap();
        b.fds -= self.fds;
        b.bytes -= self.bytes;
        b.inner_bytes -= self.inner_bytes;
    }
}
/// Admission starts before opening/classifying the outer source. The bounded
/// window covers short-lived clones and parsing buffers; the lease reserves
/// retained FDs/metadata and inner disk before any payload is decoded. On
/// success only the lease moves to the cache/worker. Claimed leases are never
/// visible to TTL/LRU eviction, and payload owners drop before their charge.
pub(crate) struct Preparation {
    lease: Option<Lease>,
    budget: Arc<Mutex<ConfirmationUsage>>,
}
impl Preparation {
    pub(super) fn into_lease(mut self) -> Lease {
        self.lease.take().unwrap()
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        self.budget.lock().unwrap().preparing -= 1;
    }
}
/// Native command gates run after the source scan. Until those gates accept
/// the returned token, a failure must close its unclaimed inputs immediately,
/// including an anonymous inner payload. A concurrent successful claim owns
/// its lease independently and cannot be discarded by this projection guard.
pub(crate) struct PendingProjection<'a> {
    cache: &'a ConfirmationCache,
    revision: Option<String>,
}
impl PendingProjection<'_> {
    pub(crate) fn expose(mut self) {
        self.revision = None;
    }
}
impl Drop for PendingProjection<'_> {
    fn drop(&mut self) {
        if let Some(revision) = &self.revision {
            self.cache.discard_unclaimed(revision);
        }
    }
}
struct ConfirmedPack {
    checked: CheckedPack,
    root_id: String,
    project: PathBuf,
    core: Option<ConfirmedCore>,
    factory: Arc<pcl_network::ClientFactory>,
    scheduler: Arc<pcl_network::DownloadScheduler>,
    curseforge: Option<Arc<crate::community_curseforge::CurseforgeClient>>,
    _lease: Lease,
    #[cfg(test)]
    transfer_route: Option<reqwest::Url>,
}
struct ConfirmedCore {
    resolved: ResolvedInstallRequest,
    installer: Installer,
}
impl ConfirmationCache {
    #[cfg(test)]
    pub(super) fn input_usage(&self) -> (usize, usize, u64, usize) {
        let usage = self.budget.lock().unwrap();
        (usage.fds, usage.bytes, usage.inner_bytes, usage.preparing)
    }
    pub(crate) fn release_source(&self, id: &str) -> Option<String> {
        if id.len() > 256 {
            return None;
        }
        let task = self.inputs.release(id);
        let source = Source::Official(id.into());
        self.entries
            .lock()
            .unwrap()
            .retain(|record| record.authority.checked.plan.binding.source != source);
        task
    }
    /// Source inputs share the same retained disk/FD budget as nested packs.
    /// Reservation precedes the HTTP body; the lease then follows every Arc
    /// owner through confirmation, queueing and publication.
    pub(super) fn reserve_input(&self, preparation: &mut Preparation, size: u64) -> Result<()> {
        let lease = preparation.lease.as_mut().unwrap();
        self.resize_lease(lease, 1, MAX_ONE, size)
    }
    pub(crate) fn pending_projection(&self, revision: &str) -> PendingProjection<'_> {
        PendingProjection {
            cache: self,
            revision: Some(revision.into()),
        }
    }
    pub(crate) fn discard_unclaimed(&self, revision: &str) {
        if revision.len() > 256 || !revision.starts_with("pack-confirm-v1:") {
            return;
        }
        let mut entries = self.entries.lock().unwrap();
        if let Some(at) = entries.iter().position(|record| record.token == revision) {
            entries.remove(at);
        }
    }
    pub(crate) fn preparation(&self) -> Result<Preparation> {
        {
            let mut usage = self.budget.lock().unwrap();
            if usage.preparing >= MAX_PREPARING {
                return Err("整合包检查正在忙碌，请等待当前检查完成后重试".into());
            }
            usage.preparing += 1;
        }
        let mut preparation = Preparation {
            lease: Some(Lease {
                budget: self.budget.clone(),
                bytes: 0,
                fds: 0,
                inner_bytes: 0,
            }),
            budget: self.budget.clone(),
        };
        self.resize_lease(preparation.lease.as_mut().unwrap(), 1, MAX_ONE, 0)?;
        Ok(preparation)
    }
    fn resize_lease(
        &self,
        lease: &mut Lease,
        fds: usize,
        bytes: usize,
        inner_bytes: u64,
    ) -> Result<()> {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|e| e.expires > Instant::now());
        loop {
            let mut usage = self.budget.lock().unwrap();
            let next_fds = usage.fds.saturating_sub(lease.fds).saturating_add(fds);
            let next_bytes = usage
                .bytes
                .saturating_sub(lease.bytes)
                .saturating_add(bytes);
            let next_inner = usage
                .inner_bytes
                .saturating_sub(lease.inner_bytes)
                .saturating_add(inner_bytes);
            if next_fds <= MAX_FDS && next_bytes <= MAX_BYTES && next_inner <= MAX_INNER_BYTES {
                usage.fds = next_fds;
                usage.bytes = next_bytes;
                usage.inner_bytes = next_inner;
                lease.fds = fds;
                lease.bytes = bytes;
                lease.inner_bytes = inner_bytes;
                return Ok(());
            }
            drop(usage);
            // Only records still owned by this deque can be evicted. Preparing,
            // claimed, queued and running jobs retain their independent leases.
            if entries.pop_front().is_none() {
                return Err(CAPACITY.into());
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn prepare(
        &self,
        root: &GameRoot,
        project: &Path,
        source: &Path,
        name: &str,
        optional: Option<&[String]>,
    ) -> Result<PackPlan> {
        self.prepare_admitted(
            self.preparation()?,
            root,
            project,
            source,
            name,
            optional,
            &AtomicBool::new(false),
        )
    }
    pub(crate) fn prepare_admitted(
        &self,
        mut preparation: Preparation,
        root: &GameRoot,
        project: &Path,
        source: &Path,
        name: &str,
        optional: Option<&[String]>,
        cancel: &AtomicBool,
    ) -> Result<PackPlan> {
        if !Arc::ptr_eq(&preparation.budget, &self.budget) {
            return Err("整合包检查 admission 与确认缓存不符".into());
        }
        super::super::check(cancel)?;
        let lease = preparation.lease.as_mut().unwrap();
        let checked = prepare_checked_with_stage(
            Path::new(&root.path),
            source,
            name,
            optional,
            cancel,
            |size| {
                let (fds, bytes) = (lease.fds + 1, lease.bytes);
                self.resize_lease(lease, fds, bytes, size)?;
                super::super::check(cancel)?;
                // Capture ancestors without following links. Inputs stay outside
                // the game root: anonymous on Linux, registry-owned on native
                // named hosts. No input pathname becomes installation authority.
                input_staging(project, Path::new(&root.path))?.anonymous()
            },
        )?;
        self.confirm_checked(preparation, root, project, checked, cancel)
    }
    pub(super) fn confirm_checked(
        &self,
        mut preparation: Preparation,
        root: &GameRoot,
        project: &Path,
        mut checked: CheckedPack,
        cancel: &AtomicBool,
    ) -> Result<PackPlan> {
        if !Arc::ptr_eq(&preparation.budget, &self.budget) {
            return Err("整合包检查 admission 与确认缓存不符".into());
        }
        let lease = preparation.lease.as_mut().unwrap();
        let factory = pcl_network::snapshot();
        let scheduler = pcl_network::download_snapshot();
        let curseforge = curseforge::configure(&mut checked, factory.clone(), cancel)?;
        if !checked.plan.preview.blockers.is_empty() {
            return Ok(checked.plan);
        }
        let core = if checked.bundled.is_some() {
            None
        } else {
            let (fds, bytes, inner_bytes) = (lease.fds + 1, lease.bytes, lease.inner_bytes);
            self.resize_lease(lease, fds, bytes, inner_bytes)?;
            let installer = Installer::from_factory(&factory)?
                .with_project(project)
                .with_cache_source(Path::new(&root.path))?
                .with_download_policy(scheduler.clone());
            let resolved = installer.resolve_request(&checked.request()?, cancel)?;
            Some(ConfirmedCore {
                installer,
                resolved,
            })
        };
        let fds = checked.retained_source_fds() + usize::from(core.is_some());
        checked.recheck_source(cancel)?;
        plan_target(&checked.plan)?;
        if let (Some(profile), Some(core)) = (&checked.rebuild_profile, &core) {
            // HMCL may describe extra runtime behavior. Compare those facts
            // with this exact captured rebuild, never silently discard them.
            checked
                .plan
                .preview
                .blockers
                .extend(formats::rebuild_blockers(
                    profile,
                    &core.resolved.launch_profile()?,
                )?);
            if !checked.plan.preview.blockers.is_empty() {
                return Ok(checked.plan);
            }
        }
        // Include native tree nodes, payload capacities and captured provider
        // metadata, rather than treating JSON size as the allocation budget.
        let bytes = footprint(&checked)
            + core.as_ref().map_or(0, |c| c.resolved.heap_bytes())
            + 4096
            + usize::from(curseforge.is_some()) * 8192;
        if bytes > MAX_ONE {
            return Err("整合包确认超过内存限额，请减少包内文件数量".into());
        }
        self.resize_lease(lease, fds, bytes, checked.retained_inner_bytes())?;
        super::super::check(cancel)?;
        let token = token()?;
        checked.plan.revision = token.clone();
        checked.plan.installable = true;
        checked
            .plan
            .warnings
            .retain(|s| !s.starts_with("此计划仅用于本地预览"));
        let view = checked.plan.clone();
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|e| e.expires > now);
        while entries.len() >= MAX_ENTRIES {
            entries.pop_front();
        }
        entries.push_back(Record {
            token,
            expires: now + TTL,
            authority: ConfirmedPack {
                checked,
                root_id: root.id.clone(),
                project: project.to_owned(),
                core,
                factory,
                scheduler,
                curseforge,
                _lease: preparation.lease.take().unwrap(),
                #[cfg(test)]
                transfer_route: None,
            },
        });
        Ok(view)
    }
    pub(crate) fn ensure_ready(&self, root: &Path) -> Result<()> {
        let active = self.active.lock().unwrap().get(root).cloned();
        super::super::ensure_ready_except_build(root, active.as_deref())
    }
    fn own_operation<'a>(&'a self, root: &Path, id: &str) -> Result<ActiveOperation<'a>> {
        let mut active = self.active.lock().unwrap();
        if active.contains_key(root) {
            return Err("此游戏目录已有私有实例构建任务".into());
        }
        active.insert(root.to_owned(), id.into());
        Ok(ActiveOperation {
            cache: self,
            root: root.to_owned(),
            id: id.into(),
        })
    }
    #[cfg(test)]
    fn claim(
        &self,
        root: &GameRoot,
        project: &Path,
        source: &Path,
        name: &str,
        token: &str,
    ) -> Result<ConfirmedPack> {
        self.claim_source(
            root,
            project,
            &Source::Local(source.to_owned()),
            name,
            token,
        )
    }
    fn claim_source(
        &self,
        root: &GameRoot,
        project: &Path,
        source: &Source,
        name: &str,
        token: &str,
    ) -> Result<ConfirmedPack> {
        if token.len() > 256 || !token.starts_with("pack-confirm-v1:") {
            return Err(INVALID.into());
        }
        let mut entries = self.entries.lock().unwrap();
        let now = Instant::now();
        entries.retain(|e| e.expires > now);
        let at = entries
            .iter()
            .position(|e| e.token == token)
            .ok_or(INVALID)?;
        let a = &entries[at].authority;
        if a.root_id != root.id
            || a.project != project
            || a.checked.plan.binding.root != Path::new(&root.path)
            || &a.checked.plan.binding.source != source
            || a.checked.plan.name != name
        {
            return Err("整合包确认与所选目录、文件或实例名称不符".into());
        }
        Ok(entries.remove(at).unwrap().authority)
    }
}
pub(super) fn input_staging(project: &Path, game_root: &Path) -> Result<Dir> {
    let project_dir = Dir::open(project)?;
    let namespace = project_dir.optional(".pcl-linux")?;
    let inputs = namespace
        .as_ref()
        .map(|dir| dir.optional("pack-inputs"))
        .transpose()?
        .flatten();
    let stage_path = project.join(".pcl-linux/pack-inputs");
    // Bind the deepest existing ancestor before mkdir. Missing directory leaves
    // use the scheduler's final-path footprint so the ordinary game root under
    // project/Minecraft remains distinct. Physical ancestors also catch bind
    // aliases, unlike a lexical starts_with check.
    let stage_scope = if inputs.is_some() {
        TaskScope::root(&stage_path)?
    } else if namespace.is_some() {
        TaskScope::files(&[stage_path])?
    } else {
        TaskScope::files(&[project.join(".pcl-linux")])?
    };
    if stage_scope.conflicts(&TaskScope::root(game_root)?) {
        return Err("整合包匿名暂存目录与游戏目录重叠，请选择项目之外的游戏目录".into());
    }
    match inputs {
        Some(inputs) => inputs.with_input_sources(),
        None => {
            let namespace = match namespace {
                Some(namespace) => namespace,
                None => project_dir.ensure(".pcl-linux")?,
            };
            namespace.ensure("pack-inputs")?.with_input_sources()
        }
    }
}
struct ActiveOperation<'a> {
    cache: &'a ConfirmationCache,
    root: PathBuf,
    id: String,
}
impl Drop for ActiveOperation<'_> {
    fn drop(&mut self) {
        let mut active = self.cache.active.lock().unwrap();
        if active.get(&self.root) == Some(&self.id) {
            active.remove(&self.root);
        }
    }
}
pub(super) fn token() -> Result<String> {
    let mut bytes = [0u8; 24];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| "无法生成整合包确认标识")?;
    Ok(format!(
        "pack-confirm-v1:{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
fn footprint(c: &CheckedPack) -> usize {
    let view = &c.plan;
    std::mem::size_of::<Record>()
        + c.inner.as_ref().map_or(0, |inner| {
            // The source binding also owns a cloned snapshot/entry for the
            // native revision. Charge both copies, never the disk payload.
            2 * (inner.snapshot.hash.capacity()
                + inner.outer_entry.path.capacity()
                + inner.outer_entry.archive_path.capacity()
                + inner.outer_entry.hash.capacity())
        })
        + c.rebuild_profile.as_ref().map_or(0, Vec::capacity)
        + c.curseforge_files.capacity() * std::mem::size_of::<(u32, u32)>()
        + view.name.capacity()
        + view.pack_name.capacity()
        + view.pack_version.capacity()
        + view.minecraft.capacity()
        + view.revision.capacity()
        + view
            .warnings
            .iter()
            .map(|s| s.capacity() + std::mem::size_of::<String>())
            .sum::<usize>()
        + serde_json::to_vec(&view.preview).map_or(MAX_ONE, |v| v.len().saturating_mul(3))
        + c.outputs
            .iter()
            .map(|(p, o)| {
                p.capacity()
                    + 256
                    + match o {
                        Output::Remote {
                            path,
                            sha1,
                            sha512,
                            downloads,
                            ..
                        } => {
                            path.capacity()
                                + sha1.capacity()
                                + sha512.capacity()
                                + downloads
                                    .iter()
                                    .map(|s| s.capacity() + std::mem::size_of::<String>())
                                    .sum::<usize>()
                        }
                        Output::CurseForge(f) => {
                            f.path.capacity() + f.sha1.capacity() + f.url.capacity()
                        }
                        Output::Override(f) => {
                            f.path.capacity() + f.hash.capacity() + f.archive_path.capacity()
                        }
                    }
            })
            .sum::<usize>()
        + c.directories
            .iter()
            .map(|s| s.capacity() + 128)
            .sum::<usize>()
        + c.plan.binding.root.as_os_str().len()
        + c.plan.binding.source.heap_bytes()
        + 2048
        + c.bundled.as_ref().map_or(0, |b| {
            b.metadata.capacity()
                + b.jar.path.capacity()
                + b.jar.archive_path.capacity()
                + b.shared
                    .iter()
                    .map(|(p, f)| {
                        256 + p.capacity()
                            + f.path.capacity()
                            + f.archive_path.capacity()
                            + f.hash.capacity()
                    })
                    .sum::<usize>()
        })
}

fn target(a: &ConfirmedPack) -> Result<()> {
    plan_target(&a.checked.plan)
}
fn plan_target(plan: &PackPlan) -> Result<()> {
    let (root, versions) = root_scope(&plan.binding.root, &plan.name)?;
    if root != plan.binding.root_key
        || plan
            .binding
            .versions_key
            .as_ref()
            .is_some_and(|old| Some(old) != versions.as_ref())
    {
        return Err(super::super::changed());
    }
    Ok(())
}
fn app_gate(shared: &Shared, root: &GameRoot, name: &str) -> Result<()> {
    let _operation = shared.operations.lock().unwrap();
    crate::require_network_submission(shared)?;
    if shared.config.resolve(Some(&root.id))?.path != root.path {
        return Err("整合包安装目录登记已变化，请重新检查".into());
    }
    crate::instance_commands::new_name(shared, root, name)
}
/// A commit already owns its root scope. Recheck external references and the
/// physical root after long hashes; closing the window must not revoke a commit
/// that already closed cancellation admission.
fn commit_binding(shared: &Shared, root: &GameRoot, checked: &CheckedPack) -> Result<()> {
    let _operation = shared.operations.lock().unwrap();
    checked.recheck_source_stamp()?;
    if shared.config.resolve(Some(&root.id))?.path != root.path
        || Dir::open(Path::new(&root.path))?.key()? != checked.plan.binding.root_key
    {
        return Err("整合包目标目录已变化，请重新检查".into());
    }
    crate::instance_commands::new_name(shared, root, &checked.plan.name)
}
/// Submission claims native authority before queue admission; an error never
/// restores a replayable confirmation. The writer holds its scope through cleanup.
pub(crate) fn start(
    shared: Arc<Shared>,
    root: GameRoot,
    source: PathBuf,
    name: String,
    revision: String,
) -> Result<serde_json::Value> {
    start_source(shared, root, Source::Local(source), name, revision)
}
pub(super) fn start_source(
    shared: Arc<Shared>,
    root: GameRoot,
    source: Source,
    name: String,
    revision: String,
) -> Result<serde_json::Value> {
    let authority = shared.pack_confirmations.claim_source(
        &root,
        &shared.project,
        &source,
        &name,
        &revision,
    )?;
    let mut scope = TaskScope::root(Path::new(&root.path))?;
    let java = if authority
        .core
        .as_ref()
        .map(|core| core.resolved.processor_java_requirement())
        .transpose()?
        .flatten()
        .is_some()
    {
        // Resolution already knows the actual client processor batch. Reserve
        // both destinations before queueing; the worker never upgrades its turn.
        scope = scope.union(TaskScope::managed_runtime_store(&shared.project)?)?;
        let preferences = shared.launcher_preferences.snapshot();
        if let Some(warning) = preferences.warning {
            return Err(warning);
        }
        Some(crate::installer_java::Context::capture(
            &shared,
            &root,
            &name,
            TaskKind::ModpackInstall,
            crate::installer_java::Inputs {
                settings: shared.config.checked_snapshot()?,
                preferences_revision: preferences.revision,
                factory: authority.factory.clone(),
                scheduler: authority.scheduler.clone(),
            },
        )?)
    } else {
        None
    };
    if shared.tasks.list().iter().any(|t| {
        !t.stage.is_terminal() && t.root_id == root.id && t.instance_id.as_deref() == Some(&name)
    }) {
        return Err("此实例名称已有安装任务，请使用其他名称或等待完成".into());
    }
    let task = shared.tasks.admit_queued(
        TaskTarget {
            root_id: root.id.clone(),
            root_path: root.path.clone(),
            instance_id: Some(name.clone()),
        },
        TaskKind::ModpackInstall,
        scope,
    )?;
    task.set_resource_name(&authority.checked.plan.pack_name);
    let id = task.id().to_owned();
    shared.downloads.track(&task);
    let auto_select = shared
        .launcher_preferences
        .snapshot()
        .preferences
        .auto_select_installed;
    std::thread::Builder::new()
        .name(format!("pcl-pack-{id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute(&shared, &root, &task, authority, java)
            }))
            .unwrap_or_else(|_| Err("整合包安装任务意外退出，请先恢复未完成操作".into()));
            match result {
                Ok(mut value) => {
                    task.begin_finishing();
                    if auto_select {
                        let _operation = shared.operations.lock().unwrap();
                        if let Err(e) = shared.config.select_installed(&root.id, &name) {
                            value["warning"] = format!("实例已安装，但选择状态未保存：{e}").into();
                        }
                    }
                    crate::finish_instance_task(task, Ok(value), "整合包安装完成");
                }
                Err(e)
                    if e == crate::tasks::CANCELLED
                        || e == "实例导入已取消"
                        || e == "导入已取消"
                        || e == "安装已取消"
                        || e == crate::java_download::CANCELLED =>
                {
                    task.finish(TaskOutcome::Failed(e));
                }
                Err(e) => {
                    task.finish(TaskOutcome::Error(e));
                }
            }
        })
        .map_err(|e| format!("无法启动整合包任务：{e}"))?;
    Ok(serde_json::json!({"id":id}))
}

fn execute(
    shared: &Shared,
    root: &GameRoot,
    task: &TaskHandle,
    mut authority: ConfirmedPack,
    mut java: Option<crate::installer_java::Context>,
) -> Result<serde_json::Value> {
    task.wait_turn()?;
    let cancel = task.cancellation_token();
    app_gate(shared, root, &authority.checked.plan.name)?;
    if let Some(java) = &java {
        java.verify(shared, task)?;
    }
    crate::ensure_instance_files_ready(shared, root)?;
    crate::instance_reset::ensure_ready(Path::new(&root.path))?;
    crate::resource_ops::ensure_verified_batches_ready(Path::new(&root.path))?;
    target(&authority)?;
    // Hashing can outlive an external versions-directory replacement.
    // Revalidate the captured directory identity immediately before beginning
    // the durable private build; root identity alone is insufficient.
    authority.checked.recheck_source(&cancel)?;
    if let Some(input) = &authority.checked.official {
        official::recheck_evidence(input, &cancel)?;
    }
    curseforge::recheck(&authority.checked, authority.curseforge.as_deref(), &cancel)?;
    target(&authority)?;
    let core = match (authority.core.take(), java.as_mut()) {
        (Some(core), Some(java)) => Some(ConfirmedCore {
            installer: java.ensure(shared, task, core.installer, &core.resolved)?,
            resolved: core.resolved,
        }),
        (core, _) => core,
    };
    let java_bytes = java.as_ref().map_or(0, |java| java.network_bytes());
    if let Some(java) = &java {
        java.verify(shared, task)?;
        // Managed Java publication can outlive the input and target bindings.
        // It stays installed on error; no private instance exists at this point.
        authority.checked.recheck_source(&cancel)?;
        target(&authority)?;
        app_gate(shared, root, &authority.checked.plan.name)?;
    }
    let mut operation = super::super::build::BuildOperation::begin_bound(
        Path::new(&root.path),
        &authority.checked.plan.name,
        &authority.checked.plan.binding.root_key,
        authority.checked.plan.binding.versions_key.as_ref(),
    )?;
    let _active = match shared
        .pack_confirmations
        .own_operation(Path::new(&root.path), operation.operation_id())
    {
        Ok(active) => active,
        Err(error) => {
            return match operation.abort() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("整合包任务登记失败：{error}；清理失败：{cleanup}")),
            }
        }
    };
    let core_bytes = AtomicU64::new(java_bytes);
    let pack_bytes = AtomicU64::new(0);
    let core_files = AtomicU64::new(0);
    let core_steps = Mutex::new(Vec::<pcl_install::InstallStep>::new());
    let pack_files = authority.checked.outputs.len() as u64;
    let outcome = (|| {
        if let Some(core) = &core {
            let result = core.installer.install_resolved_request_bound(
                operation.root_fd()?,
                &core.resolved,
                &cancel,
                |mut p| {
                    p.network_bytes = p.network_bytes.saturating_add(java_bytes);
                    core_bytes.fetch_max(p.network_bytes, Ordering::Relaxed);
                    core_files.fetch_max(p.completed, Ordering::Relaxed);
                    *core_steps.lock().unwrap() = p.steps.clone();
                    p.total = p.total.saturating_add(pack_files);
                    p.steps.extend(pack_steps("pending", "pending"));
                    shared.downloads.progress(task, p);
                },
            )?;
            if result.id != authority.checked.plan.name {
                return Err("构建结果与确认的实例名称不符".into());
            }
        } else {
            stage_bundled(&authority.checked, &operation, &cancel)?;
        }
        if let Some(java) = &java {
            java.verify(shared, task)?;
        }
        // Core resolution/processors may take minutes. Verify both held input
        // archives before consuming any pack body after this long phase.
        authority.checked.recheck_source(&cancel)?;
        let metadata = operation.open_file(&format!(
            "versions/{0}/{0}.json",
            authority.checked.plan.name
        ))?;
        let value: serde_json::Value =
            serde_json::from_reader(metadata.take(super::super::MAX_JSON + 1))
                .map_err(|_| "构建后的版本JSON无效")?;
        if value["id"] != authority.checked.plan.name
            || value["clientVersion"] != authority.checked.plan.minecraft
            || value.get("inheritsFrom").is_some()
        {
            return Err("构建后的游戏版本身份不符".into());
        }
        let client = transfer::Client::new(&authority.factory, authority.scheduler.clone())?
            .with_local_mirrors(matches!(
                authority.checked.plan.binding.source,
                Source::Local(_)
            ));
        #[cfg(test)]
        let client = match &authority.transfer_route {
            Some(route) => client.with_test_route(route.clone())?,
            None => client,
        };
        let store = operation.root_directory()?;
        let completed = AtomicU64::new(0);
        let total = authority.checked.outputs.len() as u64;
        let finish = |path: &str, file: StageFile| -> Result<()> {
            super::super::check(&cancel)?;
            if let Some(java) = &java {
                java.verify_fast(shared, task)?;
            }
            // Completion is consumed serially, so shared parent directories
            // cannot race even when their independent downloads finish together.
            stage_file(
                &store,
                &format!("versions/{}/{path}", authority.checked.plan.name),
                file,
                &cancel,
            )?;
            pack_bytes.store(client.network_bytes(), Ordering::Relaxed);
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            shared.downloads.progress(
                task,
                pack_progress(
                    &core_steps,
                    "pack-files",
                    path,
                    core_files.load(Ordering::Relaxed) + done,
                    core_files.load(Ordering::Relaxed) + total,
                    core_bytes
                        .load(Ordering::Relaxed)
                        .saturating_add(client.network_bytes()),
                ),
            );
            Ok(())
        };
        // Resolve shared build parents serially before independent downloads.
        // Each registered writer then captures its actual final parent/leaf.
        for path in authority.checked.outputs.keys() {
            let target = format!("versions/{}/{path}", authority.checked.plan.name);
            let parts = super::super::relative(&target)?;
            if parts.len() > 1 {
                ensure_directory(&store, &parts[..parts.len() - 1].join("/"))?;
            }
        }
        let instance_name = authority.checked.plan.name.as_str();
        let jobs = authority
            .checked
            .outputs
            .iter()
            .filter(|(_, output)| !matches!(output, Output::Override(_)))
            .map(|(path, output)| {
                let client = &client;
                let store = &store;
                let cancel = &cancel;
                let java = &java;
                let core_steps = &core_steps;
                let core_files = &core_files;
                let core_bytes = &core_bytes;
                let completed = &completed;
                async move {
                    super::super::check(cancel)?;
                    if let Some(java) = java {
                        java.verify_fast(shared, task)?;
                    }
                    let report = |p: transfer::TransferProgress| {
                        shared.downloads.progress(
                            task,
                            pack_progress(
                                core_steps,
                                "pack-download",
                                path,
                                core_files.load(Ordering::Relaxed)
                                    + completed.load(Ordering::Relaxed),
                                core_files.load(Ordering::Relaxed) + total,
                                core_bytes
                                    .load(Ordering::Relaxed)
                                    .saturating_add(p.network_bytes),
                            ),
                        );
                    };
                    // Each polled file owns one registered/anonymous writer.
                    // Drop all other borrowed futures before aborting the build.
                    let fd = output_stage(
                        store,
                        &format!("versions/{instance_name}/{path}"),
                        output.size(),
                    )?;
                    let verified = match output {
                        Output::Remote {
                            size,
                            sha1,
                            sha512,
                            downloads,
                            ..
                        } => {
                            let verified = client
                                .download(
                                    transfer::Remote {
                                        downloads,
                                        size: *size,
                                        sha1,
                                        sha512,
                                    },
                                    fd,
                                    cancel,
                                    report,
                                )
                                .await?;
                            if verified.size != *size
                                || !verified.sha1.eq_ignore_ascii_case(sha1)
                                || !verified.sha512.eq_ignore_ascii_case(sha512)
                            {
                                return Err("整合包下载结果与确认信息不符".into());
                            }
                            verified
                        }
                        Output::CurseForge(file) => {
                            let verified =
                                client.download_curseforge(file, fd, cancel, report).await?;
                            if verified.size != file.size
                                || !verified.sha1.eq_ignore_ascii_case(&file.sha1)
                            {
                                return Err("CurseForge 下载结果与确认信息不符".into());
                            }
                            verified
                        }
                        Output::Override(_) => unreachable!(),
                    };
                    Ok((path.as_str(), verified.file))
                }
            });
        tauri::async_runtime::block_on(pcl_network::for_each_bounded(
            jobs,
            usize::from(authority.scheduler.policy().max_concurrent_transfers),
            |(path, file)| finish(path, file),
        ))?;
        // ZIP clones share the held source cursor. Decode local overrides only
        // after all remote futures have released their responses and descriptors.
        for (path, output) in &authority.checked.outputs {
            let Output::Override(file) = output else {
                continue;
            };
            super::super::check(&cancel)?;
            if let Some(java) = &java {
                java.verify_fast(shared, task)?;
            }
            let file = extract_override(
                &authority.checked,
                file,
                output_stage(
                    &store,
                    &format!("versions/{}/{path}", authority.checked.plan.name),
                    file.size,
                )?,
                &cancel,
            )?;
            finish(path, file)?;
        }
        authority.checked.recheck_source(&cancel)?;
        curseforge::recheck(&authority.checked, authority.curseforge.as_deref(), &cancel)?;
        if let Some(java) = &java {
            java.verify(shared, task)?;
        }
        for path in &authority.checked.directories {
            ensure_directory(
                &store,
                &format!("versions/{}/{path}", authority.checked.plan.name),
            )?;
        }
        operation.seal(&cancel)
    })();
    let sealed = match outcome {
        Ok(value) => value,
        Err(error) => {
            return match operation.abort() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!(
                    "整合包安装失败：{error}；清理失败，请先恢复未完成操作：{cleanup}"
                )),
            }
        }
    };
    if let Some(input) = &authority.checked.official {
        // Metadata may change during a long core/dependency build. Recheck
        // outside the operations lock, before the irreversible publication.
        if let Err(error) = official::recheck_evidence(input, &cancel) {
            return match operation.abort() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}；清理失败：{cleanup}")),
            };
        }
    }
    let result = operation.publish_checked(
        sealed,
        &cancel,
        |mut p| {
            p.network_bytes = core_bytes
                .load(Ordering::Relaxed)
                .saturating_add(pack_bytes.load(Ordering::Relaxed));
            p.steps = combined_steps(&core_steps, "complete", "running");
            shared.downloads.progress(task, p);
        },
        || {
            authority.checked.recheck_source(&AtomicBool::new(false))?;
            commit_binding(shared, root, &authority.checked)
        },
        || {
            if let Some(java) = &java {
                java.verify(shared, task)?;
            }
            app_gate(shared, root, &authority.checked.plan.name)?;
            task.begin_finishing();
            Ok(())
        },
    )?;
    shared.downloads.progress(
        task,
        Progress {
            steps: combined_steps(&core_steps, "complete", "complete"),
            stage: "pack-files".into(),
            message: "整合包安装完成".into(),
            completed: 1,
            total: 1,
            bytes_done: 0,
            bytes_total: 0,
            network_bytes: core_bytes
                .load(Ordering::Relaxed)
                .saturating_add(pack_bytes.load(Ordering::Relaxed)),
        },
    );
    Ok(result)
}
fn pack_progress(
    core_steps: &Mutex<Vec<pcl_install::InstallStep>>,
    stage: &str,
    path: &str,
    completed: u64,
    total: u64,
    network: u64,
) -> Progress {
    Progress {
        stage: stage.into(),
        message: format!("整合包文件：{path}"),
        completed,
        total,
        bytes_done: 0,
        bytes_total: 0,
        network_bytes: network,
        steps: combined_steps(
            core_steps,
            if completed >= total {
                "complete"
            } else {
                "running"
            },
            "pending",
        ),
    }
}
fn extract_override(
    checked: &CheckedPack,
    f: &OverrideFile,
    mut destination: StageFile,
    cancel: &AtomicBool,
) -> Result<StageFile> {
    // try_clone shares the input offset. All readers of an official input,
    // including concurrent re-confirmations, participate in the same gate.
    let _reader = checked
        .official
        .as_ref()
        .map(|input| input.reader.lock().unwrap());
    let mut zip = super::super::archive::checked_zip(checked.content_file()?)?;
    let mut input = zip
        .by_name(&f.archive_path)
        .map_err(|_| "整合包覆盖文件已改变")?;
    let mut total = 0u64;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 128 * 1024];
    loop {
        super::super::check(cancel)?;
        let n = input
            .read(&mut bytes)
            .map_err(|_| "整合包覆盖文件解压或CRC校验失败")?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .filter(|v| *v <= f.size)
            .ok_or("整合包覆盖文件超过声明大小")?;
        hash.update(&bytes[..n]);
        destination
            .write_all(&bytes[..n])
            .map_err(super::super::error)?;
    }
    if total != f.size || format!("{:x}", hash.finalize()) != f.hash {
        return Err("整合包覆盖文件校验失败".into());
    }
    destination.sync_all().map_err(super::super::error)?;
    destination
        .seek(SeekFrom::Start(0))
        .map_err(super::super::error)?;
    Ok(destination)
}

/// Verified pack bodies become named files only inside the durably owned build
/// root. Closing each FD bounds descriptors independently of the pack size;
/// seal later pins the complete tree before any real-root publication.
fn ensure_directory(root: &Dir, path: &str) -> Result<Dir> {
    let mut parent = root.duplicate()?;
    for part in super::super::relative(path)? {
        parent = parent.ensure(&part)?;
    }
    Ok(parent)
}
fn output_stage(root: &Dir, path: &str, max: u64) -> Result<StageFile> {
    let parts = super::super::relative(path)?;
    let (name, parents) = parts.split_last().ok_or("构建输出路径无效")?;
    let parent = if parents.is_empty() {
        root.duplicate()?
    } else {
        ensure_directory(root, &parents.join("/"))?
    };
    parent.stage_for(name, max)
}
fn stage_file(root: &Dir, path: &str, mut file: StageFile, cancel: &AtomicBool) -> Result<()> {
    super::super::check(cancel)?;
    let parts = super::super::relative(path)?;
    let (leaf, parents) = parts.split_last().ok_or("构建输出路径无效")?;
    let parent = if parents.is_empty() {
        root.duplicate()?
    } else {
        ensure_directory(root, &parents.join("/"))?
    };
    file.sync_all().map_err(super::super::error)?;
    super::super::link_anonymous(&mut file, &parent, leaf)?;
    parent.sync()
}

/// Keep completed core/loader steps visible while the pack body downloads;
/// replacing that list would make the installation history disappear mid-task.
fn combined_steps(
    core: &Mutex<Vec<pcl_install::InstallStep>>,
    files: &str,
    content: &str,
) -> Vec<pcl_install::InstallStep> {
    let mut steps = core.lock().unwrap().clone();
    steps.extend(pack_steps(files, content));
    steps
}
fn pack_steps(files: &str, content: &str) -> Vec<pcl_install::InstallStep> {
    [
        ("pack-files", "下载并校验整合包文件", files),
        ("pack-content", "安装整合包内容", content),
    ]
    .into_iter()
    .map(|(id, label, state)| pcl_install::InstallStep {
        id: id.into(),
        label: label.into(),
        state: state.into(),
        progress: None,
    })
    .collect()
}

fn stage_bundled(
    checked: &CheckedPack,
    operation: &super::super::build::BuildOperation,
    cancel: &AtomicBool,
) -> Result<()> {
    let bundled = checked.bundled.as_ref().ok_or("整合包缺少游戏构建来源")?;
    let root = operation.root_directory()?;
    let prefix = format!("versions/{0}/{0}", checked.plan.name);
    let mut metadata = output_stage(
        &root,
        &format!("{prefix}.json"),
        bundled.metadata.len() as u64,
    )?;
    metadata
        .write_all(&bundled.metadata)
        .map_err(super::super::error)?;
    stage_file(&root, &format!("{prefix}.json"), metadata, cancel)?;
    let jar = extract_override(
        checked,
        &bundled.jar,
        output_stage(&root, &format!("{prefix}.jar"), bundled.jar.size)?,
        cancel,
    )?;
    stage_file(&root, &format!("{prefix}.jar"), jar, cancel)?;
    for (path, fact) in &bundled.shared {
        let file = extract_override(checked, fact, output_stage(&root, path, fact.size)?, cancel)?;
        stage_file(&root, path, file, cancel)?;
    }
    ensure_directory(&root, &format!("versions/{}/config", checked.plan.name))?;
    Ok(())
}

#[cfg(test)]
#[path = "nested_budget_review_tests.rs"]
mod nested_budget_review_tests;
#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
