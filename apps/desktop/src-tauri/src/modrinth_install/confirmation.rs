//! Native, one-use confirmation authority for queued ordinary installations.
//!
//! Only a provider-prepared plan can enter this cache. The public revision is a
//! random lookup token, not a plan uploaded by the UI. Unclaimed entries expire
//! and are evicted within fixed byte/count bounds. Claim transfers the native
//! record to one admitted job: queue time never revokes that submitted intent.
//! Fresh planning still runs every conflict rule; this module permits only a
//! retained baseline plus verified additions and download-to-identical reuse.
use super::{
    plan::{InstallFile, Installed},
    provider::{Provider, Version},
    *,
};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use std::io::Read;

const TTL: Duration = Duration::from_secs(600);
const MAX_ENTRIES: usize = 64;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
const MAX_CONFIRMATION_BYTES: usize = 4 * 1024 * 1024;
// Reserve the deque's maximum backing allocation as well as per-record heaps.
const CACHE_STORAGE: usize = MAX_ENTRIES * std::mem::size_of::<Record>() + 512;
const INVALID: &str = "资源确认不存在、已领取或已过期，请重新检查方案";
const CHANGED: &str = "资源目标、来源或授权依赖已变化，请重新检查方案";

#[derive(Default)]
pub struct ConfirmationCache(Mutex<VecDeque<Record>>);
struct Record {
    token: String,
    expires: Instant,
    bytes: usize,
    authority: ConfirmedInstall,
}
/// Not Deserialize/Debug: native authority is moved from the confirmation cache
/// to one worker. Dropping the job releases it; it has no persistence or FDs.
pub struct ConfirmedInstall {
    pub(super) request: InstallRequest,
    pub(super) original: InstallPlan,
}

impl ConfirmationCache {
    pub fn remember(&self, request: InstallRequest, plan: InstallPlan) -> Result<InstallPlan> {
        request.validate()?;
        let bytes = footprint(&request, &plan)?;
        let mut random = [0; 24];
        #[cfg(target_os = "linux")]
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|_| "无法生成资源方案确认标识")?;
        #[cfg(target_os = "macos")]
        if unsafe { libc::getentropy(random.as_mut_ptr().cast(), random.len()) } != 0 {
            return Err("无法生成资源方案确认标识".into());
        }
        #[cfg(windows)]
        {
            use std::{ffi::c_void, ptr};
            #[link(name = "bcrypt")]
            extern "system" {
                fn BCryptGenRandom(
                    provider: *mut c_void,
                    bytes: *mut u8,
                    length: u32,
                    flags: u32,
                ) -> i32;
            }
            if unsafe {
                BCryptGenRandom(ptr::null_mut(), random.as_mut_ptr(), random.len() as u32, 2)
            } != 0
            {
                return Err("无法生成资源方案确认标识".into());
            }
        }
        let token = format!(
            "modrinth-confirm-v1:{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let mut view = plan.clone();
        view.revision = token.clone();
        let now = Instant::now();
        let mut cache = self.0.lock().unwrap();
        cache.retain(|record| record.expires > now);
        while cache.len() >= MAX_ENTRIES
            || cache.iter().map(|record| record.bytes).sum::<usize>()
                > MAX_CACHE_BYTES.saturating_sub(CACHE_STORAGE + bytes)
        {
            cache.pop_front();
        }
        cache.push_back(Record {
            token,
            expires: now + TTL,
            bytes,
            authority: ConfirmedInstall {
                request,
                original: plan,
            },
        });
        Ok(view)
    }

    /// Consume before admission. Queue-full/thread-start errors intentionally
    /// require another preparation instead of restoring a replayable token.
    pub fn claim(
        &self,
        root: &Path,
        project: &Path,
        root_id: &str,
        id: &str,
        request: &InstallRequest,
        token: &str,
    ) -> Result<ConfirmedInstall> {
        request.validate()?;
        if token.is_empty() || token.len() > 256 {
            return Err(INVALID.into());
        }
        let now = Instant::now();
        let mut cache = self.0.lock().unwrap();
        cache.retain(|record| record.expires > now);
        let index = cache
            .iter()
            .position(|record| record.token == token)
            .ok_or(INVALID)?;
        let original = &cache[index].authority.original;
        if &cache[index].authority.request != request
            || original.root_id != root_id
            || original.instance_id != id
            || root != original.target.root
            || project != original.target.project
        {
            return Err("资源确认与所选目录、实例或请求不符，请重新检查方案".into());
        }
        Ok(cache.remove(index).unwrap().authority)
    }
}

/// Count native capacities, including every field omitted from the public DTO.
/// JSON length cannot bound vectors of short strings; each String/Vec structure
/// and payload is included, with conservative B-tree node capacity overhead.
fn footprint(request: &InstallRequest, plan: &InstallPlan) -> Result<usize> {
    use allocation::*;
    let bytes = std::mem::size_of::<Record>()
        + 256
        + text(&request.project_id)
        + text(&request.version_id)
        + optional(&request.file_name)
        + plan.target.heap_bytes()
        + text(&plan.root_id)
        + text(&plan.instance_id)
        + text(&plan.minecraft_version)
        + text(&plan.loader)
        + text(&plan.revision)
        + vector(&plan.files, |file| {
            text(&file.project_id)
                + text(&file.version_id)
                + text(&file.title)
                + text(&file.kind)
                + text(&file.file_name)
                + text(&file.sha512)
                + text(&file.url)
                + optional(&file.existing_file_name)
        })
        + vector(&plan.dependencies, |edge| {
            text(&edge.from_project_id)
                + text(&edge.from_version_id)
                + optional(&edge.project_id)
                + optional(&edge.version_id)
                + optional(&edge.file_name)
                + text(&edge.dependency_type)
        })
        + vector(&plan.warnings, text)
        + optional(&plan.unidentified_warning)
        + map(&plan.authority_versions, text, version)
        + vector(&plan.installed, |installed| {
            local(&installed.file) + version(&installed.version)
        });
    if bytes > MAX_CONFIRMATION_BYTES {
        Err("资源确认方案超过内存缓存限额，请减少资源依赖或本地库存后重新检查".into())
    } else {
        Ok(bytes)
    }
}

pub(super) mod allocation {
    pub use super::super::allocation::{local, map, optional, text, vector};
    use super::*;
    pub fn version(version: &Version) -> usize {
        text(&version.id)
            + text(&version.project_id)
            + text(&version.name)
            + text(&version.version_number)
            + text(&version.date_published)
            + text(&version.version_type)
            + vector(&version.game_versions, text)
            + vector(&version.loaders, text)
            + vector(&version.files, |file| {
                text(&file.filename)
                    + text(&file.url)
                    + text(&file.hashes.sha512)
                    + optional(&file.hashes.sha1)
                    + optional(&file.file_type)
            })
            + vector(&version.dependencies, |edge| {
                optional(&edge.project_id)
                    + optional(&edge.version_id)
                    + optional(&edge.file_name)
                    + text(&edge.dependency_type)
            })
    }
}

fn same_artifact(old: &InstallFile, new: &InstallFile) -> bool {
    old.project_id == new.project_id
        && old.version_id == new.version_id
        && old.kind == new.kind
        && old.file_name == new.file_name
        && old.size == new.size
        && old.sha512 == new.sha512
        && old.required == new.required
        && old.url == new.url
        && (!old.reused || (new.reused && old.existing_file_name == new.existing_file_name))
}
fn known(plan: &InstallPlan, file: &LocalFile) -> bool {
    plan.installed.iter().any(|item| &item.file == file)
}

impl ConfirmedInstall {
    pub(crate) fn require_isolation(
        &self,
        isolation: pcl_core::content_scope::IsolationPolicy,
        revision: &str,
    ) -> Result<()> {
        // Empty revision belongs only to synthetic/legacy API fixtures. Native
        // preparation always captures the complete configuration generation.
        if self.original.target.isolation != isolation
            || (!self.original.target.settings_revision.is_empty()
                && self.original.target.settings_revision != revision)
        {
            return Err("实例隔离设置已变化，请重新检查资源方案".into());
        }
        Ok(())
    }
    pub fn request(&self) -> &InstallRequest {
        &self.request
    }
    pub fn resource_name(&self) -> Option<&str> {
        self.original
            .files
            .iter()
            .find(|file| file.project_id == self.request.project_id)
            .map(|file| file.title.as_str())
    }
    pub(super) fn capture_target(&self, cancel: &AtomicBool) -> Result<TargetSnapshot> {
        let old = &self.original;
        let current = target::capture_with_isolation(
            &old.target.root,
            &old.target.project,
            &old.root_id,
            &old.instance_id,
            cancel,
            old.target.isolation,
            &old.target.settings_revision,
        )?;
        if !old.target.permits_inventory_extension(&current) {
            return Err("排队期间目标实例或原库存已变化，请重新检查方案".into());
        }
        Ok(current)
    }
    pub(super) async fn replan<P: Provider>(
        &self,
        provider: &P,
        target: TargetSnapshot,
        cancel: &AtomicBool,
    ) -> Result<InstallPlan> {
        let current = plan::prepare(provider, target, self.request.clone(), cancel).await?;
        self.validate(provider, &current).await?;
        Ok(current)
    }
    pub(super) async fn validate<P: Provider>(
        &self,
        provider: &P,
        current: &InstallPlan,
    ) -> Result<()> {
        let old = &self.original;
        if !old.target.permits_inventory_extension(&current.target)
            || old.authority_versions != current.authority_versions
            || old.dependencies != current.dependencies
            || old.files.len() != current.files.len()
            || !old
                .files
                .iter()
                .zip(&current.files)
                .all(|(a, b)| same_artifact(a, b))
        {
            return Err(CHANGED.into());
        }
        // Every previously identified source fact remains identified and exact.
        // Previously unknown bytes may become identified, reducing risk, but
        // unknown additions or newly lost identification cannot prove safety.
        for installed in &old.installed {
            if !current
                .installed
                .iter()
                .any(|new| new.file == installed.file && new.version == installed.version)
            {
                return Err(CHANGED.into());
            }
        }
        for file in &current.target.local_files {
            if !known(current, file)
                && !(old.target.local_files.contains(file) && !known(old, file))
            {
                return Err("新增本地资源未被官方识别，请重新检查方案".into());
            }
        }
        let risks: BTreeSet<_> = old
            .warnings
            .iter()
            .filter(|warning| Some(*warning) != old.unidentified_warning.as_ref())
            .collect();
        if current
            .warnings
            .iter()
            .filter(|warning| Some(*warning) != current.unidentified_warning.as_ref())
            .any(|warning| !risks.contains(warning))
        {
            return Err("资源方案出现新的风险提示，请重新检查方案".into());
        }
        for installed in &current.installed {
            if old
                .installed
                .iter()
                .any(|before| before.file == installed.file && before.version == installed.version)
            {
                continue;
            }
            self.check_added_consumer(provider, current, installed)
                .await?;
        }
        Ok(())
    }

    async fn check_added_consumer<P: Provider>(
        &self,
        provider: &P,
        plan: &InstallPlan,
        installed: &Installed,
    ) -> Result<()> {
        let project = provider.project(&installed.version.project_id).await?;
        if project.id != installed.version.project_id {
            return Err("新增资源的官方项目身份不符".into());
        }
        let matching = installed.version.files.iter().filter(|file| {
            file.hashes.sha512 == installed.file.sha512 && file.size == installed.file.size
        });
        let mut correct_kind = false;
        for file in matching {
            let kind = plan::file_kind(&project, file)?;
            plan::extension(kind, file)?;
            correct_kind |= kind == installed.file.kind;
        }
        if !correct_kind {
            return Err("新增资源所在类型与官方文件类型不符，请重新检查方案".into());
        }
        if !installed.file.enabled {
            return Ok(());
        }
        // A new companion resource pack belongs to its publisher's mod
        // version, so compatibility uses project kind rather than local folder.
        if !plan::compatible(
            &installed.version,
            plan::kind(&project)?,
            &plan.target.compatibility,
        ) {
            return Err("新增资源的游戏版本或加载器不兼容，请重新检查方案".into());
        }
        // Ordinary fresh preparation already checked every enabled consumer's
        // reverse pins. The delta gate adds compatibility/type proof only.
        Ok(())
    }
}

/// Ordinary preparation and queued reuse share this reverse constraint gate.
/// It only constrains artifacts already selected by the request; it grants no
/// replacement, auto-enable or missing-dependency installation authority.
pub(super) async fn check_required_consumers<P: Provider>(
    provider: &P,
    plan: &InstallPlan,
) -> Result<()> {
    // Common libraries may have hundreds of consumers. Resolve each reverse
    // ID/project once; the provider's total HTTP/byte/cancellation limits still
    // bound unrelated version-only references whose project must be resolved.
    let mut versions = std::collections::BTreeMap::<String, Version>::new();
    let mut projects = std::collections::BTreeMap::new();
    for dependency in plan
        .installed
        .iter()
        .filter(|i| i.file.enabled)
        .flat_map(|i| &i.version.dependencies)
        .filter(|d| d.dependency_type == "required")
    {
        let referenced: Option<Version> = if let Some(id) = &dependency.version_id {
            if dependency
                .project_id
                .as_ref()
                .is_some_and(|project| !plan.authority_versions.contains_key(project))
            {
                continue;
            }
            let version =
                if let Some(version) = plan.authority_versions.values().find(|v| &v.id == id) {
                    version.clone()
                } else if let Some(version) = versions.get(id) {
                    version.clone()
                } else {
                    let version = provider.version(id).await?;
                    versions.insert(id.clone(), version.clone());
                    version
                };
            provider::validate_version(&version)?;
            if version.id != *id
                || dependency
                    .project_id
                    .as_ref()
                    .is_some_and(|project| project != &version.project_id)
            {
                return Err("新增资源的必需依赖项目与版本不符".into());
            }
            Some(version)
        } else {
            None
        };
        let project_id = referenced
            .as_ref()
            .map(|version| &version.project_id)
            .or(dependency.project_id.as_ref());
        if let Some(project_id) = project_id {
            let Some(selected) = plan.authority_versions.get(project_id) else {
                continue;
            };
            if referenced
                .as_ref()
                .is_some_and(|version| version.id != selected.id)
            {
                return Err("新增资源仍要求其他精确前置版本，请重新检查方案".into());
            }
            // No filename means the same primary runtime file selected by the
            // forward planner. Selecting a mod's companion pack alone cannot
            // satisfy another mod's required project/version declaration.
            let file = plan::select_file(selected, dependency.file_name.as_deref())?;
            let dependency_project = if let Some(project) = projects.get(project_id) {
                project
            } else {
                let project = provider.project(project_id).await?;
                projects.insert(project_id.clone(), project);
                projects.get(project_id).unwrap()
            };
            if dependency_project.id != *project_id {
                return Err("资源的前置项目身份不符".into());
            }
            let kind = plan::file_kind(&dependency_project, file)?;
            plan::extension(kind, file)?;
            let planned = plan.files.iter().any(|planned| {
                &planned.project_id == project_id
                    && planned.version_id == selected.id
                    && planned.kind == kind
                    && planned.file_name == file.filename
                    && planned.sha512 == file.hashes.sha512
                    && planned.size == file.size
            });
            // A recognized enabled alias is valid reuse, just as in forward
            // planning. Its official project/version and file role are needed;
            // a matching hash in a different resource folder is insufficient.
            let installed = plan.installed.iter().any(|installed| {
                installed.file.enabled
                    && installed.file.kind == kind
                    && installed.file.sha512 == file.hashes.sha512
                    && installed.file.size == file.size
                    && &installed.version.project_id == project_id
                    && installed.version.id == selected.id
                    && installed.version.files.iter().any(|authority| {
                        authority.filename == file.filename
                            && authority.hashes.sha512 == file.hashes.sha512
                            && authority.size == file.size
                    })
            });
            if !planned && !installed {
                return Err("资源要求的前置文件未在本次授权或已启用库存中，请重新检查方案".into());
            }
        } else if let Some(name) = &dependency.file_name {
            // A file-only requirement can constrain an affected basename,
            // but grants no authority to install an unrelated missing file.
            if plan.files.iter().any(|file| &file.file_name == name)
                && !plan.files.iter().any(|file| {
                    &file.file_name == name
                        && (!file.reused || file.existing_file_name.as_ref() == Some(name))
                })
            {
                return Err("新增资源的前置文件名与复用位置冲突，请重新检查方案".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "confirmation/tests.rs"]
mod tests;
