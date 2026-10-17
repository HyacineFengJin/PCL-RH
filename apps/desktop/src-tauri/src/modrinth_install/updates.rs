//! Local mod updates have two separate authorities: hashes identify installed
//! releases, and an unchanged resource-panel token authorizes a selected file.
//! Check is read-only. Prepare resolves one shared dependency graph while its
//! target snapshot keeps every original file; replacement permission never
//! removes inventory from the commit-time identity/content check.
//!
//! Only later compatible releases are chosen. Existing disabled mods remain
//! disabled, and a disabled mandatory dependency is never enabled implicitly.
//! All additional replacements/additions are exposed before confirmation.
use super::{
    plan::{InstallFile, PlannedDependency},
    provider::HttpProvider,
    *,
};
use crate::resource_ops::ResourceFile;
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;

#[path = "updates/check.rs"]
mod check;
#[cfg(test)]
#[path = "updates/integration_tests.rs"]
mod integration_tests;
#[path = "updates/policy.rs"]
pub(super) mod policy;
#[cfg(test)]
#[path = "updates/tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use integration_tests::test_update_batch;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    UpdateAvailable,
    UpToDate,
    Unknown,
    Blocked,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateEntry {
    pub file_name: String,
    pub fingerprint: String,
    pub enabled: bool,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub old_version_id: Option<String>,
    pub old_version: Option<String>,
    pub new_version_id: Option<String>,
    pub new_version: Option<String>,
    pub status: UpdateStatus,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct UpdateCheck {
    pub root_id: String,
    pub instance_id: String,
    pub minecraft_version: String,
    pub loader: String,
    pub entries: Vec<UpdateEntry>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct UpdateReplacement {
    pub kind: String,
    pub old_file_name: String,
    pub old_fingerprint: String,
    pub old_sha512: String,
    pub new_file_name: String,
    pub enabled: bool,
    pub project_id: String,
    pub title: String,
    pub old_version_id: String,
    pub old_version: String,
    pub new_version_id: String,
    pub new_version: String,
    pub size: u64,
    pub sha512: String,
    pub required: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct UpdatePlan {
    pub root_id: String,
    pub instance_id: String,
    pub minecraft_version: String,
    pub loader: String,
    pub revision: String,
    pub replacements: Vec<UpdateReplacement>,
    pub adds: Vec<InstallFile>,
    pub reuse: Vec<InstallFile>,
    pub dependencies: Vec<PlannedDependency>,
    pub warnings: Vec<String>,
    pub total_bytes: u64,
    pub download_bytes: u64,
    #[serde(skip)]
    transfer: InstallPlan,
}
pub struct VerifiedUpdateBatch {
    pub plan: UpdatePlan,
    pub files: Vec<transfer::VerifiedFile>,
    pub network_bytes: u64,
}

pub async fn check_updates_with_isolation(
    root: &Path,
    project: &Path,
    root_id: &str,
    id: &str,
    cancel: &AtomicBool,
    isolation: pcl_core::content_scope::IsolationPolicy,
    settings_revision: &str,
) -> Result<UpdateCheck> {
    let target = target::capture_with_isolation(
        root,
        project,
        root_id,
        id,
        cancel,
        isolation,
        settings_revision,
    )?;
    let provider = HttpProvider::new(cancel)?;
    let result = check::check(&provider, &target, cancel).await?;
    target::check(&target, cancel)?;
    Ok(result)
}
pub async fn prepare_update_with_isolation(
    root: &Path,
    project: &Path,
    root_id: &str,
    id: &str,
    selected: Vec<ResourceFile>,
    cancel: &AtomicBool,
    isolation: pcl_core::content_scope::IsolationPolicy,
    settings_revision: &str,
) -> Result<UpdatePlan> {
    let target = target::capture_with_isolation(
        root,
        project,
        root_id,
        id,
        cancel,
        isolation,
        settings_revision,
    )?;
    let provider = HttpProvider::new(cancel)?;
    prepare_with(&provider, target, selected, cancel).await
}
pub async fn download_update_request_with_policy_and_isolation(
    root: &Path,
    project: &Path,
    root_id: &str,
    id: &str,
    selected: Vec<ResourceFile>,
    revision: &str,
    scheduler: std::sync::Arc<pcl_network::DownloadScheduler>,
    cancel: &AtomicBool,
    report: impl Fn(DownloadProgress),
    isolation: pcl_core::content_scope::IsolationPolicy,
    settings_revision: &str,
) -> Result<VerifiedUpdateBatch> {
    let target = target::capture_with_isolation(
        root,
        project,
        root_id,
        id,
        cancel,
        isolation,
        settings_revision,
    )?;
    let provider = HttpProvider::new(cancel)?.with_download_policy(scheduler);
    let prepared = prepare_with(&provider, target, selected, cancel).await;
    report(DownloadProgress {
        phase: "metadata".into(),
        message: "官方模组更新与必需依赖检查结束".into(),
        completed: 0,
        total: 0,
        bytes_done: 0,
        bytes_total: 0,
        network_bytes: provider.network_bytes.load(Ordering::Relaxed),
    });
    let plan = prepared?;
    if plan.revision != revision {
        return Err("模组更新文件、目标或必需依赖已变化，请重新检查更新方案".into());
    }
    let batch = transfer::download(&provider, plan.transfer.clone(), cancel, report).await?;
    Ok(VerifiedUpdateBatch {
        plan,
        files: batch.files,
        network_bytes: batch.network_bytes,
    })
}
pub(crate) fn update_isolation(
    plan: &UpdatePlan,
) -> (pcl_core::content_scope::IsolationPolicy, &str) {
    (
        plan.transfer.target.isolation,
        &plan.transfer.target.settings_revision,
    )
}
pub fn recheck_update_target(plan: &UpdatePlan, cancel: &AtomicBool) -> Result<()> {
    target::check(&plan.transfer.target, cancel)
}

pub(super) async fn prepare_with<P: provider::Provider>(
    provider: &P,
    target: TargetSnapshot,
    mut selected: Vec<ResourceFile>,
    cancel: &AtomicBool,
) -> Result<UpdatePlan> {
    if selected.is_empty() || selected.len() > MAX_PROJECTS {
        return Err("请选择1至64个可识别的模组更新".into());
    }
    selected.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    let mut names = std::collections::BTreeSet::new();
    for file in &selected {
        provider::file_name(&file.file_name)?;
        if !names.insert(file.file_name.clone())
            || !target.local_files.iter().any(|local| {
                local.kind == "mods"
                    && local.file_name == file.file_name
                    && local.resource_fingerprint == file.fingerprint
            })
        {
            return Err("所选模组重复、不可写或文件已变化，请重新读取模组列表".into());
        }
    }
    let installed = plan::identify(provider, &target, cancel).await?;
    let mut catalog = check::Catalog::new(provider, &target, &installed);
    let mut roots = Vec::new();
    for file in &selected {
        let old = installed
            .iter()
            .find(|old| old.file.kind == "mods" && old.file.file_name == file.file_name)
            .ok_or("所选模组未被Modrinth识别，不能按名称推断更新")?;
        let candidate = catalog.candidate(old).await?;
        roots.push((
            old.clone(),
            candidate.ok_or("所选模组没有可安装的较新兼容正式版本")?,
        ));
    }
    let (transfer, replacements) =
        plan::prepare_updates(provider, target, installed, roots, cancel).await?;
    let adds = transfer
        .files
        .iter()
        .filter(|f| {
            !f.reused
                && !replacements
                    .iter()
                    .any(|r| r.kind == f.kind && r.new_file_name == f.file_name)
        })
        .cloned()
        .collect();
    let reuse = transfer
        .files
        .iter()
        .filter(|f| f.reused)
        .cloned()
        .collect();
    let mut result = UpdatePlan {
        root_id: transfer.root_id.clone(),
        instance_id: transfer.instance_id.clone(),
        minecraft_version: transfer.minecraft_version.clone(),
        loader: transfer.loader.clone(),
        revision: String::new(),
        replacements,
        adds,
        reuse,
        dependencies: transfer.dependencies.clone(),
        warnings: transfer.warnings.clone(),
        total_bytes: transfer.total_bytes,
        download_bytes: transfer.download_bytes,
        transfer,
    };
    result.revision = format!(
        "mod-update:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&result.transfer.target, &selected, &result))
                .map_err(|e| e.to_string())?
        )
    );
    recheck_update_target(&result, cancel)?;
    Ok(result)
}
