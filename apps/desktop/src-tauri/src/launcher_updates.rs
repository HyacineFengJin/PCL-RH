//! Launcher-owned update checks, verified staging and portable replacement.
//!
//! Tauri owns scheduling/admission and injects running build identity. This
//! service owns the official repository boundary, candidate token, cancellation
//! and staged descriptor. Check/download never replace an installed executable.
//! Portable apply owns an immutable journal and append-only phase markers.
//! Startup recovery and rollback preserve both executable inodes; uncertain
//! external edits block recovery while retaining every file.
#[path = "launcher_updates/binary.rs"]
mod binary;
#[path = "launcher_updates/files.rs"]
mod files;
#[path = "launcher_updates/manifest.rs"]
mod manifest;
#[path = "launcher_updates/model.rs"]
mod model;
#[path = "launcher_updates/provider.rs"]
mod provider;
#[cfg(test)]
#[path = "launcher_updates/test_support.rs"]
mod test_support;
#[cfg(test)]
#[path = "launcher_updates/tests.rs"]
mod tests;
#[path = "launcher_updates/transaction.rs"]
mod transaction;

use files::{Stage, Staging};
use manifest::{parse_manifest, unique_asset};
pub use model::*;
use provider::{hex, safe_tag, GitHubProvider, ReleaseProvider, MANIFEST_NAME, MAX_MANIFEST_BYTES};
use semver::Version;
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc, Mutex,
    },
    time::Duration,
};

#[derive(Clone)]
struct Candidate {
    view: ReleaseView,
    asset_id: u64,
}
#[derive(Clone, Copy)]
enum OperationKind {
    Check,
    Download,
    Apply,
    Recovery,
    Discard,
}
struct Active {
    id: String,
    kind: OperationKind,
    cancelled: Arc<AtomicBool>,
    cancel_allowed: bool,
}
struct State {
    view: UpdateView,
    candidate: Option<Candidate>,
    staged: Option<Stage>,
    active: Option<Active>,
}

pub struct UpdateService {
    project: PathBuf,
    current_version: Version,
    state: Mutex<State>,
    cleanup: Mutex<Option<files::Cleanup>>,
    #[cfg(test)]
    test_provider: Option<Arc<dyn ReleaseProvider>>,
}
impl UpdateService {
    pub fn new(project: &Path, current: CurrentBuild) -> Result<Self, String> {
        let valid_path = |path: &Path| pcl_core::retained_fs::validate_absolute_path(path).is_ok();
        if !valid_path(project)
            || current.executable.len() > 4096
            || !valid_path(Path::new(&current.executable))
            || current.executable.contains('\0')
            || current.version.len() > 128
            || current
                .commit
                .as_ref()
                .is_some_and(|commit| !hex(commit, 40))
        {
            return Err("运行版本信息或项目路径无效".into());
        }
        let current_version =
            Version::parse(&current.version).map_err(|_| "运行版本不是有效的语义版本")?;
        Ok(Self {
            project: project.into(),
            current_version,
            cleanup: Mutex::new(None),
            state: Mutex::new(State {
                view: UpdateView {
                    current,
                    state: CheckState::Idle,
                    message: None,
                    release: None,
                    download: DownloadView::default(),
                    plan: None,
                    installation: InstallationView::default(),
                },
                candidate: None,
                staged: None,
                active: None,
            }),
            #[cfg(test)]
            test_provider: None,
        })
    }
    pub fn snapshot(&self) -> UpdateView {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .view
            .clone()
    }
    pub fn is_busy(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active
            .is_some()
    }
    /// Cancellation remains admitted until the owned operation has dropped its
    /// response and anonymous file. Callers must await that operation on close.
    pub fn cancel(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(active) = &mut state.active {
            if !active.cancel_allowed {
                return false;
            }
            active.cancelled.store(true, AtomicOrdering::Release);
            true
        } else {
            false
        }
    }
    pub fn discard_staged(&self) -> Result<UpdateView, String> {
        let mut operation = self.start(OperationKind::Discard, None)?;
        let stage = {
            let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
            state.view.plan = None;
            state.staged.take()
        };
        let result = stage
            .map_or(Ok(()), |stage| stage.retire())
            .and_then(|_| self.cleanup_check());
        let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
        match result {
            Ok(()) => state.view.download = DownloadView::default(),
            Err(error) => {
                state.view.download.state = DownloadState::Error;
                state.view.download.message = Some(error.clone());
                state.active = None;
                operation.finished = true;
                return Err(error);
            }
        }
        state.active = None;
        operation.finished = true;
        Ok(state.view.clone())
    }
    fn cleanup_check(&self) -> Result<(), String> {
        self.cleanup
            .lock()
            .map_err(|_| "更新来源状态锁异常")?
            .as_ref()
            .map_or(Ok(()), files::Cleanup::check)
    }
    /// Run on startup before other writers. A malformed/external-edited journal
    /// is retained and exposed as recovery_required, never silently discarded.
    pub fn recover(&self) -> Result<UpdateView, String> {
        self.installation_operation(OperationKind::Recovery, |current, _| {
            transaction::recover(&self.project, current)
        })
    }
    /// Parent schedules this on a blocking worker and holds the shared idle
    /// admission until return. Running build identity never changes in place.
    pub fn apply(&self, token: &str) -> Result<UpdateView, String> {
        let mut operation = self.start(OperationKind::Apply, Some(token))?;
        let (current, candidate, stage) = {
            let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
            (
                state.view.current.clone(),
                state.candidate.clone().ok_or("更新选择已过期")?,
                state.staged.take().ok_or("更新暂存已释放，请重新下载")?,
            )
        };
        let installation = transaction::apply(
            &self.project,
            &current,
            &candidate.view,
            stage,
            &operation.cancelled,
            &|| {
                // This lock also guards cancel(). It makes cancellation and
                // the first filesystem exchange a single admission decision.
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let Some(active) = state.active.as_mut() else {
                    return false;
                };
                if active.id != operation.id {
                    return false;
                }
                active.cancel_allowed = false;
                !active.cancelled.load(AtomicOrdering::Acquire)
            },
        );
        let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
        state.view.installation = installation;
        state.view.plan = None;
        state.active = None;
        operation.finished = true;
        Ok(state.view.clone())
    }
    pub fn rollback(&self, token: &str) -> Result<UpdateView, String> {
        self.installation_operation(OperationKind::Recovery, |current, _| {
            transaction::rollback(&self.project, current, token)
        })
    }
    /// Only a process running the verified disk build may acknowledge it.
    /// Backups are retained; acknowledgement archives the pending journal.
    pub fn acknowledge_update(&self, token: &str) -> Result<UpdateView, String> {
        self.installation_operation(OperationKind::Recovery, |current, _| {
            transaction::acknowledge(&self.project, current, token)
        })
    }
    fn installation_operation(
        &self,
        kind: OperationKind,
        run: impl FnOnce(&CurrentBuild, &AtomicBool) -> InstallationView,
    ) -> Result<UpdateView, String> {
        let mut operation = self.start(kind, None)?;
        let current = self.snapshot().current;
        let installation = run(&current, &operation.cancelled);
        let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
        state.view.installation = installation;
        state.active = None;
        operation.finished = true;
        Ok(state.view.clone())
    }
    pub async fn check(&self, channel: UpdateChannel) -> Result<UpdateView, String> {
        let mut operation = self.start(OperationKind::Check, None)?;
        let mut result = cancellable(
            &operation.cancelled,
            Duration::from_secs(120),
            self.check_inner(channel, &operation.cancelled),
        )
        .await;
        let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
        if operation.cancelled.load(AtomicOrdering::Acquire) {
            result = Err("更新检查已取消".into());
        }
        match result {
            Ok(outcome) => {
                state.view.state = outcome.state;
                state.view.message = Some(outcome.message);
                state.view.release = outcome
                    .candidate
                    .as_ref()
                    .map(|candidate| candidate.view.clone());
                state.candidate = outcome.candidate;
            }
            Err(error) => {
                state.view.state = CheckState::Error;
                state.view.message = Some(error);
            }
        }
        state.active = None;
        operation.finished = true;
        Ok(state.view.clone())
    }
    pub async fn download(&self, token: &str) -> Result<UpdateView, String> {
        let mut operation = self.start(OperationKind::Download, Some(token))?;
        let candidate = {
            let state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
            state.candidate.clone().ok_or("请先重新检查更新")?
        };
        let mut result = cancellable(
            &operation.cancelled,
            Duration::from_secs(600),
            self.download_inner(&candidate, &operation.cancelled),
        )
        .await;
        let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
        if operation.cancelled.load(AtomicOrdering::Acquire) {
            drop(state);
            let cleanup = match result {
                Ok((stage, _)) => stage.retire(),
                Err(_) => Ok(()),
            }
            .and_then(|_| self.cleanup_check());
            result = Err(match cleanup {
                Ok(()) => "更新下载已取消，生成来源已退役".into(),
                Err(error) => format!("更新下载已取消；{error}"),
            });
            state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
        }
        match result {
            Ok((stage, plan)) => {
                state.staged = Some(stage);
                state.view.plan = Some(plan);
                state.view.download.state = DownloadState::Staged;
                state.view.download.message = Some(if cfg!(target_os = "linux") {
                    "已校验 SHA256 和 Linux ELF；暂存仅保留在本进程，尚未替换启动器".into()
                } else {
                    "已校验SHA256与原生程序格式；已登记来源尚未替换启动器".into()
                });
            }
            Err(error) => {
                state.staged = None;
                state.view.plan = None;
                state.view.download.state = if operation.cancelled.load(AtomicOrdering::Acquire) {
                    DownloadState::Cancelled
                } else {
                    DownloadState::Error
                };
                state.view.download.message = Some(error);
            }
        }
        state.active = None;
        operation.finished = true;
        Ok(state.view.clone())
    }

    fn start(&self, kind: OperationKind, token: Option<&str>) -> Result<Operation<'_>, String> {
        let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
        if state.active.is_some() {
            return Err("已有启动器更新操作正在进行".into());
        }
        if matches!(kind, OperationKind::Download | OperationKind::Apply)
            && !state
                .candidate
                .as_ref()
                .is_some_and(|candidate| token == Some(candidate.view.token.as_str()))
        {
            return Err("更新选择已过期，请重新检查并确认发布信息".into());
        }
        if matches!(kind, OperationKind::Apply)
            && (!state.view.plan.as_ref().is_some_and(|plan| plan.can_apply)
                || state.staged.is_none())
        {
            return Err("当前更新计划不能应用，请检查暂存、安装位置和恢复状态".into());
        }
        let id = opaque_token()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        state.active = Some(Active {
            id: id.clone(),
            kind,
            cancelled: cancelled.clone(),
            cancel_allowed: !matches!(kind, OperationKind::Recovery),
        });
        let obsolete = if matches!(kind, OperationKind::Check | OperationKind::Download) {
            state.view.plan = None;
            state.staged.take()
        } else {
            None
        };
        match kind {
            OperationKind::Check => {
                state.candidate = None;
                state.view.release = None;
                state.view.state = CheckState::Checking;
                state.view.message = Some(
                    platform_text(
                        "正在读取本项目的 GitHub 发布与 Linux 校验清单",
                        "正在读取本项目的GitHub发布与原生程序校验清单",
                    )
                    .into(),
                );
                state.view.download = DownloadView::default();
            }
            OperationKind::Download => {
                let total = state.candidate.as_ref().unwrap().view.bytes;
                state.view.download = DownloadView {
                    state: DownloadState::Downloading,
                    received_bytes: 0,
                    total_bytes: total,
                    message: None,
                };
            }
            OperationKind::Apply => state.view.installation.state = InstallationState::Applying,
            OperationKind::Recovery => {
                state.view.installation.state = InstallationState::RollingBack
            }
            OperationKind::Discard => {}
        }
        drop(state);
        let operation = Operation {
            service: self,
            id,
            cancelled,
            finished: false,
        };
        if let Some(stage) = obsolete {
            stage.retire()?;
            self.cleanup_check()?;
        }
        Ok(operation)
    }
    fn provider(&self) -> Result<Arc<dyn ReleaseProvider>, String> {
        #[cfg(test)]
        if let Some(provider) = &self.test_provider {
            return Ok(provider.clone());
        }
        Ok(Arc::new(GitHubProvider::new()?))
    }
    async fn check_inner(
        &self,
        channel: UpdateChannel,
        cancelled: &AtomicBool,
    ) -> Result<CheckOutcome, String> {
        let current = self.snapshot().current;
        if current.architecture == Architecture::Unsupported {
            return Ok(CheckOutcome::none(platform_text(
                "当前 Linux 架构尚无受支持的启动器更新格式",
                "当前系统架构尚无受支持的启动器更新格式",
            )));
        }
        let provider = self.provider()?;
        let releases = provider.releases().await?;
        if releases.len() > 300 {
            return Err("GitHub 发布列表超过数量限制".into());
        }
        let mut parsed = Vec::new();
        for release in releases {
            if release.draft
                || release.id == 0
                || release.assets.len() > 64
                || !safe_tag(&release.tag_name)
            {
                continue;
            }
            let tag = release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name);
            let Ok(version) = Version::parse(tag) else {
                continue;
            };
            if channel == UpdateChannel::Stable && (release.prerelease || !version.pre.is_empty()) {
                continue;
            }
            parsed.push((version, release));
        }
        parsed.sort_by(|left, right| {
            right
                .0
                .cmp_precedence(&left.0)
                .then_with(|| right.1.id.cmp(&left.1.id))
        });
        let mut compatible_seen = false;
        let mut uncertain_equal = false;
        let mut incompatible_newer = false;
        for (version, release) in parsed {
            ensure_not_cancelled(cancelled)?;
            let precedence = version.cmp_precedence(&self.current_version);
            let manifest_asset = match unique_asset(&release, MANIFEST_NAME) {
                Ok(asset) if asset.size > 0 && asset.size <= MAX_MANIFEST_BYTES as u64 => asset,
                _ => {
                    incompatible_newer |= precedence == Ordering::Greater;
                    continue;
                }
            };
            let mut stream = provider.asset(manifest_asset.id, false).await?;
            let bytes = read_bounded(&mut *stream, MAX_MANIFEST_BYTES, cancelled).await?;
            if bytes.len() as u64 != manifest_asset.size
                || manifest_asset
                    .digest
                    .as_ref()
                    .is_some_and(|digest| digest != &format!("sha256:{:x}", Sha256::digest(&bytes)))
            {
                incompatible_newer |= precedence == Ordering::Greater;
                continue;
            }
            let Ok((manifest, artifact, asset)) =
                parse_manifest(&bytes, &release, &version, &current.architecture)
            else {
                incompatible_newer |= precedence == Ordering::Greater;
                continue;
            };
            let commit = provider.tag_commit(&release.tag_name).await?;
            if !commit.eq_ignore_ascii_case(&manifest.commit) {
                incompatible_newer |= precedence == Ordering::Greater;
                continue;
            }
            let mut stream = provider.asset(asset.id, true).await?;
            let header = read_prefix(&mut *stream, cancelled).await?;
            if binary::prefix(&header, &current.architecture).is_err() {
                incompatible_newer |= precedence == Ordering::Greater;
                continue;
            }
            compatible_seen = true;
            let is_update = match precedence {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => match &current.commit {
                    Some(base) if base.eq_ignore_ascii_case(&commit) => false,
                    Some(base) => {
                        let comparison = provider.compare(base, &commit).await?;
                        match (
                            comparison.status.as_str(),
                            comparison.ahead_by,
                            comparison.behind_by,
                        ) {
                            ("ahead", ahead, 0) if ahead > 0 => true,
                            ("behind", 0, behind) if behind > 0 => false,
                            ("identical", 0, 0) => false,
                            _ => {
                                uncertain_equal = true;
                                false
                            }
                        }
                    }
                    None => {
                        uncertain_equal = true;
                        false
                    }
                },
            };
            if is_update {
                let candidate = Candidate {
                    asset_id: asset.id,
                    view: ReleaseView {
                        token: opaque_token()?,
                        tag: release.tag_name,
                        version: version.to_string(),
                        commit,
                        prerelease: release.prerelease || !version.pre.is_empty(),
                        artifact_name: artifact.name,
                        architecture: artifact.architecture,
                        bytes: artifact.size,
                        sha256: artifact.sha256.to_ascii_lowercase(),
                        elf_header_verified: cfg!(target_os = "linux"),
                    },
                };
                return Ok(CheckOutcome {
                    state: CheckState::Available,
                    message: platform_text(
                        "发现本项目的兼容 Linux 发布；下载前请确认资产、版本和大小",
                        "发现本项目的兼容发布；下载前请确认资产、版本和大小",
                    )
                    .into(),
                    candidate: Some(candidate),
                });
            }
        }
        if uncertain_equal {
            return Ok(CheckOutcome::none(
                "同版本发布与运行构建的提交关系无法证明，未将其视为可更新版本",
            ));
        }
        if incompatible_newer {
            return Ok(CheckOutcome::none(platform_text(
                "较新发布缺少匹配的 Linux ELF、校验清单或标签提交，无法安全更新",
                "较新发布缺少匹配的原生程序、校验清单或标签提交，无法安全更新",
            )));
        }
        if compatible_seen {
            Ok(CheckOutcome {
                state: CheckState::Latest,
                message: "未发现比当前运行构建更新的兼容发布".into(),
                candidate: None,
            })
        } else {
            Ok(CheckOutcome::none(platform_text(
                "本项目当前没有包含兼容 Linux ELF 和 SHA256 清单的发布",
                "本项目当前没有包含兼容原生程序与SHA256清单的发布",
            )))
        }
    }

    async fn download_inner(
        &self,
        candidate: &Candidate,
        cancelled: &AtomicBool,
    ) -> Result<(Stage, ApplyPlan), String> {
        let staging = Staging::prepare(&self.project)?;
        *self.cleanup.lock().map_err(|_| "更新来源状态锁异常")? = Some(staging.cleanup());
        let mut stage = staging.anonymous()?;
        let prepared = async {
        let provider = self.provider()?;
        let mut stream = provider.asset(candidate.asset_id, false).await?;
        loop {
            ensure_not_cancelled(cancelled)?;
            let Some(chunk) = stream.next_chunk().await? else {
                break;
            };
            let received = stage.append(&chunk, candidate.view.bytes)?;
            let mut state = self.state.lock().map_err(|_| "启动器更新服务锁异常")?;
            state.view.download.received_bytes = received;
        }
        ensure_not_cancelled(cancelled)?;
        stage.finish(
            candidate.view.bytes,
            &candidate.view.sha256,
            &candidate.view.architecture,
        )?;
        staging.check_visible()?;
        let current = self.snapshot().current;
        let expected = self.project.join(".pcl-rust/bin/pcl-desktop");
        let portable = !cfg!(windows) && current.install_channel == InstallChannel::Portable
            && Path::new(&current.executable) == expected;
        let pending = transaction::has_pending(&self.project)?;
        let (destination, original_sha256, reason) = if portable {
            let token = staging.original_token()?;
            let digest = token.sha256.clone();
            stage.original = Some(token);
            (
                Some(expected.to_string_lossy().into_owned()),
                Some(digest),
                if pending {
                    "校验暂存已准备；请先恢复或重启确认上次更新后再应用".into()
                } else {
                    "校验暂存已准备；应用会原子替换便携启动器并保留原 inode 供回滚，完成后需要重启"
                        .into()
                },
            )
        } else if current.install_channel == InstallChannel::PackageManaged {
            (
                None,
                None,
                "启动器由系统包管理安装，请使用对应包管理器升级；此暂存不会替换系统文件".into(),
            )
        } else if cfg!(windows) {
            (None,None,"已校验Windows程序；运行中EXE替换需要独立应用器，当前不会改动安装文件".into())
        } else {
            (
                None,
                None,
                "当前位置不属于本项目管理的便携安装目录，当前不支持应用内替换".into(),
            )
        };
        ensure_not_cancelled(cancelled)?;
        let view = &candidate.view;
        Ok(ApplyPlan {
                version: view.version.clone(),
                commit: view.commit.clone(),
                artifact_name: view.artifact_name.clone(),
                architecture: view.architecture.clone(),
                bytes: view.bytes,
                sha256: view.sha256.clone(),
                destination,
                install_channel: current.install_channel,
                original_sha256,
                can_apply: portable && !pending,
                restart_required: true,
                reason,
            })
        }.await;
        match prepared {
            Ok(plan) => Ok((stage, plan)),
            Err(error) => {
                let cleanup = stage.retire().and_then(|_| self.cleanup_check());
                Err(match cleanup {
                    Ok(()) => error,
                    Err(cleanup) => format!("{error}；{cleanup}"),
                })
            }
        }
    }
}

struct CheckOutcome {
    state: CheckState,
    message: String,
    candidate: Option<Candidate>,
}
impl CheckOutcome {
    fn none(message: &str) -> Self {
        Self {
            state: CheckState::NoCompatibleRelease,
            message: message.into(),
            candidate: None,
        }
    }
}
struct Operation<'a> {
    service: &'a UpdateService,
    id: String,
    cancelled: Arc<AtomicBool>,
    finished: bool,
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let cleanup_error = self.service.cleanup_check().err();
        let mut state = self
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.id == self.id)
        {
            let kind = state.active.as_ref().unwrap().kind;
            match kind {
                OperationKind::Check => {
                    state.view.state = CheckState::Error;
                    state.view.message = Some("更新检查已中断".into());
                }
                OperationKind::Download => {
                    state.view.download.state = DownloadState::Cancelled;
                    state.view.download.message = Some(if cfg!(target_os = "linux") {
                        "更新下载已中断，匿名暂存已释放".into()
                    } else {
                        "更新下载已中断，已登记生成来源已退役".into()
                    });
                }
                OperationKind::Discard => {
                    state.view.download.state = DownloadState::Error;
                    state.view.download.message = Some("更新来源清理中断，请核对登记证据".into());
                }
                OperationKind::Apply | OperationKind::Recovery => {
                    state.view.installation.state = InstallationState::RecoveryRequired;
                    state.view.installation.warning =
                        Some("更新安装操作中断，请运行启动恢复后继续".into());
                }
            }
            if let Some(error) = cleanup_error {
                state.view.download.state = DownloadState::Error;
                state.view.download.message = Some(error);
            }
            state.active = None;
        }
    }
}
fn ensure_not_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(AtomicOrdering::Acquire) {
        Err("操作已取消，未替换启动器文件".into())
    } else {
        Ok(())
    }
}
async fn cancellable<T>(
    cancelled: &AtomicBool,
    limit: Duration,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let cancellation = async {
        loop {
            ensure_not_cancelled(cancelled)?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        #[allow(unreachable_code)]
        Ok::<T, String>(unreachable!())
    };
    tokio::select! {
        result = future => result,
        result = cancellation => result,
        _ = tokio::time::sleep(limit) => Err("启动器更新操作超过时间限制，未替换启动器文件".into()),
    }
}
async fn read_bounded(
    stream: &mut dyn provider::AssetStream,
    max: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        ensure_not_cancelled(cancelled)?;
        let Some(chunk) = stream.next_chunk().await? else {
            break;
        };
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err("更新清单超过大小限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn read_prefix(
    stream: &mut dyn provider::AssetStream,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while bytes.len() < binary::PREFIX {
        ensure_not_cancelled(cancelled)?;
        let Some(chunk) = stream.next_chunk().await? else {
            if bytes.len() < 64 {
                return Err("更新程序头不完整".into());
            }
            break;
        };
        if chunk.len() > 1024 * 1024 {
            return Err(
                platform_text("更新 ELF 文件头响应块超过限制", "更新程序头响应块超过限制").into(),
            );
        }
        bytes.extend_from_slice(&chunk[..chunk.len().min(binary::PREFIX - bytes.len())]);
    }
    Ok(bytes)
}
fn opaque_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    #[cfg(target_os = "linux")]
    let count = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    #[cfg(target_os = "macos")]
    let count = if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } == 0 {
        bytes.len() as isize
    } else {
        -1
    };
    #[cfg(windows)]
    let count = {
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(
                provider: *mut std::ffi::c_void,
                bytes: *mut u8,
                len: u32,
                flags: u32,
            ) -> i32;
        }
        if unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                2,
            )
        } == 0
        {
            bytes.len() as isize
        } else {
            -1
        }
    };
    if count != bytes.len() as isize {
        return Err("无法生成更新选择标识".into());
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn platform_text(linux: &'static str, native: &'static str) -> &'static str {
    if cfg!(target_os = "linux") {
        linux
    } else {
        native
    }
}
