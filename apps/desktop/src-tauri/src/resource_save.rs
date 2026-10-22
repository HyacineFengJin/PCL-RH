//! Standalone official resource Save. Preparation binds one explicitly selected
//! file; transfer and final authority checks use that opaque revision. Anonymous
//! staging belongs to the chosen filesystem, then a task-owned cancellation
//! gate precedes no-replacement publication. No instance or cache is touched.
#[path = "resource_save/authority.rs"]
pub(crate) mod authority;
#[path = "resource_save/files.rs"]
mod files;
#[path = "resource_save/model.rs"]
mod model;
#[cfg(test)]
#[path = "resource_save/tests.rs"]
mod tests;
use crate::modrinth_install::{
    provider::{ApiFile, HttpProvider},
    transfer,
};
pub(crate) use files::SaveTarget as CapturedSaveTarget;
pub use model::{CommitGate, SavePlan, SaveProgress, SaveRequest, SaveResult, SaveSource};
use pcl_network::DownloadScheduler;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
type Result<T> = std::result::Result<T, String>;
pub const CANCELLED: &str = "资源文件下载已取消";
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
pub async fn prepare(request: SaveRequest, cancel: &AtomicBool) -> Result<SavePlan> {
    authority::prepare(request, cancel).await
}
pub fn suggested_filename(plan: &SavePlan, project_version: bool) -> String {
    authority::suggested_filename(plan, project_version)
}
fn progress(
    report: &impl Fn(SaveProgress),
    phase: &str,
    message: &str,
    plan: &SavePlan,
    bytes_done: u64,
    network_bytes: u64,
) {
    report(SaveProgress {
        phase: phase.into(),
        message: message.into(),
        bytes_done,
        bytes_total: plan.size,
        network_bytes,
    });
}
/// The command captures and holds the native chooser's target before admission.
/// Keeping that descriptor through a queued wait prevents a replacement parent
/// pathname from retargeting this write. Request/revision supply only official
/// network authority; the target supplies the chosen local basename/location.
pub(crate) async fn save_captured(
    target: &CapturedSaveTarget,
    request: SaveRequest,
    revision: &str,
    scheduler: Arc<DownloadScheduler>,
    cancel: &AtomicBool,
    report: impl Fn(SaveProgress),
    commit_gate: CommitGate<'_>,
) -> Result<SaveResult> {
    cancelled(cancel)?;
    authority::validate_request(&request)?;
    target.recheck()?;
    let provider = HttpProvider::new(cancel)?.with_download_policy(scheduler);
    let source = request.source;
    let curseforge = (source == SaveSource::CurseForge)
        .then(|| crate::community_curseforge::CurseforgeClient::new(pcl_network::snapshot()))
        .transpose()?;
    let metadata_bytes = AtomicU64::new(0);
    let resolve = |request| {
        let provider = &provider;
        let metadata_bytes = &metadata_bytes;
        let curseforge = &curseforge;
        Box::pin(async move {
            if let Some(curseforge) = curseforge {
                // Cancellation owns the entire metadata future, including its
                // final recheck. A stopped task cannot continue API work.
                return crate::modrinth_install::provider::cancellable(
                    cancel,
                    std::time::Instant::now() + std::time::Duration::from_secs(45),
                    authority::resolve_curseforge(curseforge, request),
                )
                .await?;
            }
            let session = provider.renewed_metadata_session();
            let result = authority::resolve(&session, request).await;
            metadata_bytes.fetch_add(
                session.network_bytes.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            result
        }) as FutureAuthority<'_>
    };
    let request_file = |file: &ApiFile| {
        let url = match source {
            SaveSource::Modrinth => crate::modrinth_install::provider::cdn_url(&file.url)?,
            SaveSource::CurseForge => crate::community_curseforge::download_url(&file.url)?,
        };
        let request = if let Some(curseforge) = &curseforge {
            curseforge.download_request(&url)?
        } else {
            provider.client.get(url)
        };
        Ok(request.header(reqwest::header::ACCEPT_ENCODING, "identity"))
    };
    let session = SaveSession {
        http: &provider,
        resolve: &resolve,
        request_file: &request_file,
        metadata_bytes: &metadata_bytes,
    };
    save_with(
        &session,
        target,
        request,
        revision,
        cancel,
        report,
        commit_gate,
    )
    .await
}
type FutureAuthority<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<authority::Authority>> + 'a>>;
/// Internal dependency boundary lets fixtures supply fake metadata and loopback
/// payloads without broadening production CDN authority or changing globals.
struct SaveSession<'a> {
    http: &'a HttpProvider<'a>,
    resolve: &'a dyn Fn(SaveRequest) -> FutureAuthority<'a>,
    request_file: &'a dyn Fn(&ApiFile) -> Result<reqwest::RequestBuilder>,
    metadata_bytes: &'a AtomicU64,
}
impl SaveSession<'_> {
    fn network_bytes(&self) -> u64 {
        self.http
            .network_bytes
            .load(Ordering::Relaxed)
            .saturating_add(self.metadata_bytes.load(Ordering::Relaxed))
    }
}
async fn save_with(
    session: &SaveSession<'_>,
    target: &files::SaveTarget,
    request: SaveRequest,
    revision: &str,
    cancel: &AtomicBool,
    report: impl Fn(SaveProgress),
    commit_gate: CommitGate<'_>,
) -> Result<SaveResult> {
    if revision.len() != 136 || !revision.starts_with("save-v1:") {
        return Err("保存确认标识无效，请重新选择文件".into());
    }
    let authority = (session.resolve)(request.clone())
        .await
        .map_err(normalize_cancel)?;
    if authority.plan.revision != revision {
        return Err("官方文件信息已变化，请重新确认保存".into());
    }
    let plan = &authority.plan;
    progress(
        &report,
        "metadata",
        "官方文件已核对",
        plan,
        0,
        session.network_bytes(),
    );
    target.recheck()?;
    cancelled(cancel)?;
    let mut file = target.anonymous()?;
    let mut bytes_done = 0u64;
    transfer::response_into_file_digest(
        session.http,
        (session.request_file)(&authority.file)?,
        plan.size,
        authority::expected_digest(plan)?,
        &mut file,
        cancel,
        |amount| {
            bytes_done = bytes_done.saturating_add(amount);
            progress(
                &report,
                "downloading",
                "正在下载并校验保存文件",
                plan,
                bytes_done,
                session.network_bytes(),
            );
        },
    )
    .await
    .map_err(normalize_cancel)?;
    progress(
        &report,
        "verifying",
        "正在复核已下载文件",
        plan,
        bytes_done,
        session.network_bytes(),
    );
    let file = files::verify_anonymous(file, plan, cancel)?;
    let final_authority = (session.resolve)(request).await.map_err(normalize_cancel)?;
    if final_authority.plan.revision != revision {
        return Err("下载期间官方文件信息已变化；暂存文件已丢弃，请重新确认".into());
    }
    let network_bytes = session.network_bytes();
    target.recheck()?;
    cancelled(cancel)?;
    progress(
        &report,
        "committing",
        "文件校验完成，准备保存",
        plan,
        bytes_done,
        network_bytes,
    );
    commit_gate()?;
    cancelled(cancel)?;
    let warning = target.publish(&file)?;
    // Publication succeeded: later cancellation cannot turn the outcome into
    // a false failure. The caller retains task admission until this return.
    progress(
        &report,
        "complete",
        "资源文件已保存",
        plan,
        bytes_done,
        network_bytes,
    );
    Ok(SaveResult {
        path: target.path.display().to_string(),
        file_name: target.file_name.clone(),
        size: plan.size,
        sha512: file.sha512.clone(),
        network_bytes,
        warning,
    })
}

fn normalize_cancel(error: String) -> String {
    if error == crate::modrinth_install::CANCELLED {
        CANCELLED.into()
    } else {
        error
    }
}
