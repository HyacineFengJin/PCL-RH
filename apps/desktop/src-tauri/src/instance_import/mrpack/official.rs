//! Official Modrinth archive inputs, separate from local pathname authority.
//!
//! A cancellable preparation job downloads one explicitly selected file into
//! an anonymous FD. The short-lived source cache supports name/optional edits
//! without fetching the archive again. Confirmations retain an Arc and their
//! own descriptor; eviction/closing a view cannot revoke a claimed worker.
use super::*;
use crate::{
    config::GameRoot,
    modrinth_install::{
        provider::{self, HttpProvider, Provider},
        transfer as resource_transfer,
    },
    resource_save::{authority::resolve_pack, SavePlan, SaveRequest},
    tasks, Shared,
};
use std::{
    collections::VecDeque,
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(600);
const MAX_SOURCES: usize = 64;
const EXPIRED: &str = "整合包下载来源已过期或已关闭，请重新选择";

pub(crate) struct Input {
    id: String,
    root_id: String,
    root: PathBuf,
    root_key: Key,
    project: PathBuf,
    evidence: SavePlan,
    file: File,
    snapshot: Snapshot,
    // dup/try_clone shares the file position. Serializing readers prevents a
    // re-confirmation from moving an installation worker's ZIP/hash cursor.
    pub(super) reader: Mutex<()>,
    #[cfg(test)]
    evidence_provider: Option<Arc<dyn Provider + Send>>,
    _lease: service::Lease,
}
struct Record {
    id: String,
    task_id: String,
    expires: Instant,
    input: Option<Arc<Input>>,
}
#[derive(Default)]
pub(super) struct Inputs(Mutex<VecDeque<Record>>);
impl Inputs {
    fn register(&self, id: String, task_id: String) -> Result<()> {
        let mut records = self.0.lock().unwrap();
        let now = Instant::now();
        records.retain(|r| r.input.is_none() || r.expires > now);
        if records.len() >= MAX_SOURCES {
            return Err("整合包下载来源过多，请关闭未使用的确认页".into());
        }
        records.push_back(Record {
            id,
            task_id,
            expires: now + TTL,
            input: None,
        });
        Ok(())
    }
    fn publish(&self, input: Arc<Input>) -> Result<()> {
        let mut records = self.0.lock().unwrap();
        let record = records
            .iter_mut()
            .find(|r| r.id == input.id)
            .ok_or(EXPIRED)?;
        record.expires = Instant::now() + TTL;
        record.input = Some(input);
        Ok(())
    }
    fn get(&self, id: &str, root: &GameRoot, project: &Path) -> Result<Arc<Input>> {
        let mut records = self.0.lock().unwrap();
        let now = Instant::now();
        records.retain(|r| r.input.is_none() || r.expires > now);
        let input = records
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.input.clone())
            .ok_or(EXPIRED)?;
        if input.root_id != root.id
            || input.root != Path::new(&root.path)
            || input.project != project
            || Dir::open(&input.root)?.key()? != input.root_key
        {
            return Err("整合包来源与当前游戏目录不符，请重新下载".into());
        }
        Ok(input)
    }
    /// Removing pending ownership is also a tombstone: a late worker cannot
    /// reinsert its result after the view closed, even at the completion boundary.
    pub(crate) fn release(&self, id: &str) -> Option<String> {
        if id.len() > 256 {
            return None;
        }
        let mut records = self.0.lock().unwrap();
        records
            .iter()
            .position(|r| r.id == id)
            .and_then(|at| records.remove(at))
            .map(|r| r.task_id)
    }
}

pub(crate) fn begin(
    shared: Arc<Shared>,
    root: GameRoot,
    request: SaveRequest,
) -> Result<serde_json::Value> {
    provider::id(&request.project_id)?;
    provider::id(&request.version_id)?;
    provider::file_name(&request.file_name)?;
    if !request.file_name.ends_with(".mrpack") {
        return Err("请选择 .mrpack 文件".into());
    }
    let id = service::token()?.replacen("pack-confirm-v1:", "pack-source-v1:", 1);
    let task = shared.tasks.admit_queued(
        tasks::TaskTarget {
            root_id: root.id.clone(),
            root_path: root.path.clone(),
            instance_id: None,
        },
        tasks::TaskKind::ModpackPrepare,
        tasks::TaskScope::files(&[shared
            .project
            .join(".pcl-linux/pack-inputs")
            .join(id.replace(':', "-"))])?,
    )?;
    shared
        .pack_confirmations
        .inputs
        .register(id.clone(), task.id().into())?;
    shared.downloads.track(&task);
    let task_id = task.id().to_string();
    let worker_id = id.clone();
    let scheduler = pcl_network::download_snapshot();
    let worker = shared.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("pcl-pack-input-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> Result<serde_json::Value> {
                    task.wait_turn()?;
                    let mut preparation = worker.pack_confirmations.preparation()?;
                    let cancel = task.cancellation_token();
                    super::super::check(&cancel)?;
                    let root_key = Dir::open(Path::new(&root.path))?.key()?;
                    let http = HttpProvider::new(&cancel)?.with_download_policy(scheduler);
                    let authority =
                        tauri::async_runtime::block_on(resolve_pack(&http, request.clone()))?;
                    task.set_resource_name(&authority.plan.project_title);
                    worker
                        .pack_confirmations
                        .reserve_input(&mut preparation, authority.file.size)?;
                    let size = authority.file.size;
                    let file = service::input_staging(&worker.project, Path::new(&root.path))?
                        .anonymous()?;
                    let pending = PendingDownload {
                        capture: Capture {
                            id: worker_id.clone(),
                            root_id: root.id.clone(),
                            root: PathBuf::from(&root.path),
                            project: worker.project.clone(),
                            root_key,
                        },
                        authority,
                        file,
                        preparation,
                    };
                    let input = tauri::async_runtime::block_on(download(
                        &http,
                        pending,
                        &cancel,
                        |amount| {
                            task.update(tasks::TaskProgress {
                                stage: tasks::TaskStage::Downloading,
                                phase: "pack-input-download".into(),
                                message: format!("正在下载整合包：{}", request.file_name),
                                completed: 0,
                                total: 1,
                                bytes_done: amount,
                                bytes_total: size,
                                network_bytes: http.network_bytes.load(Ordering::Relaxed),
                                steps: vec![],
                            });
                        },
                    ))?;
                    // No game writes occurred. Revalidate root ownership after transfer
                    // and serialize completion against shutdown/cancellation admission.
                    let _operation = worker.operations.lock().unwrap();
                    crate::require_network_submission(&worker)?;
                    if worker.config.resolve(Some(&root.id))?.path != root.path
                        || Dir::open(Path::new(&root.path))?.key()? != input.root_key
                    {
                        return Err("游戏目录已变化，请重新下载整合包".into());
                    }
                    super::super::check(&cancel)?;
                    task.begin_finishing();
                    super::super::check(&cancel)?;
                    let view =
                        serde_json::json!({"source_id": input.id, "evidence": input.evidence});
                    worker.pack_confirmations.inputs.publish(Arc::new(input))?;
                    Ok(view)
                },
            ))
            .unwrap_or_else(|_| Err("整合包下载任务意外退出".into()));
            match result {
                Ok(view) => {
                    task.finish(tasks::TaskOutcome::Complete {
                        result: Some(view),
                        message: "整合包已下载，请确认安装内容".into(),
                        error: None,
                    });
                }
                Err(error) => {
                    worker.pack_confirmations.inputs.release(&worker_id);
                    task.finish(if cancel_error(&error) {
                        tasks::TaskOutcome::Failed(tasks::CANCELLED.into())
                    } else {
                        tasks::TaskOutcome::Error(error)
                    });
                }
            }
        });
    if let Err(error) = spawned {
        shared.pack_confirmations.inputs.release(&id);
        return Err(format!("无法启动整合包下载：{error}"));
    }
    Ok(serde_json::json!({"id":task_id,"sourceId":id}))
}
fn cancel_error(error: &str) -> bool {
    matches!(
        error,
        tasks::CANCELLED | "实例导入已取消" | crate::modrinth_install::CANCELLED | EXPIRED
    )
}

/// These facts travel together from reservation through verified input. A
/// failure drops the anonymous body before returning its disk/FD reservation.
struct Capture {
    id: String,
    root_id: String,
    root: PathBuf,
    root_key: Key,
    project: PathBuf,
}
struct PendingDownload {
    capture: Capture,
    authority: crate::resource_save::authority::Authority,
    file: File,
    preparation: service::Preparation,
}
async fn download(
    http: &HttpProvider<'_>,
    pending: PendingDownload,
    cancel: &AtomicBool,
    received: impl FnMut(u64),
) -> Result<Input> {
    let url = provider::cdn_url(&pending.authority.file.url)?;
    let request = http
        .client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
    download_checked(
        http,
        &http.renewed_metadata_session(),
        pending,
        request,
        cancel,
        received,
    )
    .await
}
/// Production supplies the validated CDN request and official provider. Tests
/// inject loopback transport here, without widening either production policy.
async fn download_checked(
    http: &HttpProvider<'_>,
    metadata: &(impl Provider + ?Sized),
    mut pending: PendingDownload,
    request: reqwest::RequestBuilder,
    cancel: &AtomicBool,
    received: impl FnMut(u64),
) -> Result<Input> {
    resource_transfer::response_into_file(
        http,
        request,
        pending.authority.file.size,
        &pending.authority.file.hashes.sha512,
        &mut pending.file,
        cancel,
        received,
    )
    .await?;
    pending.file.sync_all().map_err(super::super::error)?;
    let snapshot = hash_file(
        pending.file.try_clone().map_err(super::super::error)?,
        MAX_ARCHIVE,
        Some(cancel),
    )?;
    if snapshot.stamp.size != pending.authority.file.size
        || pending
            .file
            .metadata()
            .map_err(super::super::error)?
            .nlink()
            != 0
    {
        return Err("整合包匿名输入身份无效".into());
    }
    let current = resolve_pack(metadata, pending.authority.plan.request.clone()).await?;
    if current.plan.revision != pending.authority.plan.revision {
        return Err("下载期间官方整合包文件已变化，请重新选择".into());
    }
    let capture = pending.capture;
    Ok(Input {
        id: capture.id,
        root_id: capture.root_id,
        root: capture.root,
        root_key: capture.root_key,
        project: capture.project,
        evidence: pending.authority.plan,
        file: pending.file,
        snapshot,
        reader: Mutex::new(()),
        #[cfg(test)]
        evidence_provider: None,
        _lease: pending.preparation.into_lease(),
    })
}
pub(crate) fn prepare(
    cache: &ConfirmationCache,
    root: &GameRoot,
    project: &Path,
    id: &str,
    name: &str,
    optional: Option<&[String]>,
    cancel: &AtomicBool,
) -> Result<PackPlan> {
    let preparation = cache.preparation()?;
    let input = cache.inputs.get(id, root, project)?;
    recheck_evidence(&input, cancel)?;
    let checked = prepare_checked(input, name, optional, cancel)?;
    let plan = cache.confirm_checked(preparation, root, project, checked, cancel)?;
    let projection = cache.pending_projection(&plan.revision);
    // Closing during a long parse/core resolve revokes only unclaimed output.
    // A submitted worker has already moved its authority out of this cache.
    cache.inputs.get(id, root, project)?;
    projection.expose();
    Ok(plan)
}
fn prepare_checked(
    input: Arc<Input>,
    name: &str,
    optional: Option<&[String]>,
    cancel: &AtomicBool,
) -> Result<CheckedPack> {
    let reader = input.reader.lock().unwrap();
    super::super::check(cancel)?;
    let (root_key, versions_key) = root_scope(&input.root, name)?;
    if root_key != input.root_key {
        return Err(super::super::changed());
    }
    let file = input.file.try_clone().map_err(super::super::error)?;
    if hash_file(
        file.try_clone().map_err(super::super::error)?,
        MAX_ARCHIVE,
        Some(cancel),
    )? != input.snapshot
        || file.metadata().map_err(super::super::error)?.nlink() != 0
    {
        return Err(super::super::changed());
    }
    let binding = Binding {
        root: input.root.clone(),
        root_key,
        versions_key,
        source: Source::Official(input.id.clone()),
        source_snapshot: input.snapshot.clone(),
        archive_limit: MAX_ARCHIVE,
        inner: None,
    };
    if formats::classify_held(&file, cancel)? != formats::Format::Modrinth {
        return Err("官方文件内容不是有效的 Modrinth 整合包".into());
    }
    let scan = archive::scan_cancellable(file.try_clone().map_err(super::super::error)?, cancel)?;
    let manifest = manifest::parse(&scan.index)?;
    let optional = selection(&manifest, optional)?;
    let (mut plan, outputs, directories) = build(name, manifest, scan, optional, binding)?;
    plan.recheck_cancellable(cancel)?;
    plan.revision = format!(
        "mrpack-v1:{:x}",
        Sha256::digest(serde_json::to_vec(&plan.revision_facts()?).map_err(super::super::error)?)
    );
    drop(reader);
    Ok(CheckedPack {
        plan,
        file,
        outputs,
        directories,
        bundled: None,
        rebuild_profile: None,
        curseforge_files: Vec::new(),
        inner: None,
        official: Some(input),
    })
}
pub(super) fn recheck_evidence(input: &Input, cancel: &AtomicBool) -> Result<()> {
    #[cfg(test)]
    if let Some(metadata) = &input.evidence_provider {
        let current = tauri::async_runtime::block_on(resolve_pack(
            metadata.as_ref(),
            input.evidence.request.clone(),
        ))?;
        return if current.plan.revision == input.evidence.revision {
            Ok(())
        } else {
            Err("官方整合包文件已变化，请重新下载".into())
        };
    }
    let http = HttpProvider::new(cancel)?;
    let current =
        tauri::async_runtime::block_on(resolve_pack(&http, input.evidence.request.clone()))?;
    if current.plan.revision != input.evidence.revision {
        return Err("官方整合包文件已变化，请重新下载".into());
    }
    Ok(())
}
pub(crate) fn start(
    shared: Arc<Shared>,
    root: GameRoot,
    id: String,
    name: String,
    revision: String,
) -> Result<serde_json::Value> {
    let result = service::start_source(
        shared.clone(),
        root,
        Source::Official(id.clone()),
        name,
        revision,
    );
    if result.is_ok() {
        shared.pack_confirmations.release_source(&id);
    }
    result
}

#[cfg(test)]
#[path = "official_tests.rs"]
mod tests;
