//! Checked client overlays for local pack formats and native installation authority.
//!
//! Shared ZIP/FD primitives retain the PCL ZIP boundary. This separate parser
//! validates all archive bodies and builds a bounded effective client overlay;
//! parsing itself downloads nothing and creates no target directories. The
//! service may resolve official core metadata before issuing a one-use native
//! confirmation. The public DTO is never Deserialize/install authority; its
//! token refers to captured source, target, choices and provider evidence.
#[cfg(all(test, unix))]
use super::filesystem::Stamp;
use super::{
    file_stamp, hash_file, is_anonymous, name_ok, open_source, Dir, Key, Result, Snapshot,
    StageFile,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
#[path = "mrpack/archive.rs"]
mod archive;
#[path = "mrpack/curseforge.rs"]
mod curseforge;
#[path = "mrpack/foreign.rs"]
mod foreign;
#[path = "mrpack/formats.rs"]
mod formats;
#[path = "mrpack/manifest.rs"]
mod manifest;
#[path = "mrpack/mirror.rs"]
pub(crate) mod mirror;
#[path = "mrpack/official.rs"]
pub(crate) mod official;
#[path = "mrpack/service.rs"]
pub(crate) mod service;
#[path = "mrpack/transfer.rs"]
mod transfer;
use archive::{ArchiveScan, OverrideFile};
use manifest::{Client, Manifest};
pub(crate) use service::ConfirmationCache;

const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_DECODED: u64 = 1024 * 1024 * 1024;
const MAX_ENTRY: u64 = 256 * 1024 * 1024;
const MAX_INDEX: u64 = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_OUTPUT: u64 = 8 * 1024 * 1024 * 1024;
const INDEX: &str = "modrinth.index.json";

#[derive(Clone, Serialize)]
pub struct PackPlan {
    pub revision: String,
    pub name: String,
    pub pack_name: String,
    pub pack_version: String,
    pub minecraft: String,
    pub file_count: usize,
    pub bytes: u64,
    pub reused_files: usize,
    pub warnings: Vec<String>,
    pub format: &'static str,
    pub installable: bool,
    pub preview: Preview,
    #[serde(skip)]
    binding: Binding,
}
#[derive(Clone, Serialize)]
pub struct Preview {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub dependencies: Vec<PreviewDependency>,
    pub files: Vec<PreviewFile>,
    pub required_files: usize,
    pub optional_files: usize,
    pub excluded_files: usize,
    pub download_bytes: u64,
    pub override_files: usize,
    pub override_bytes: u64,
    pub client_overrides: usize,
    pub shadowed_files: usize,
    pub blockers: Vec<String>,
}
#[derive(Clone, Serialize)]
pub struct PreviewDependency {
    pub id: String,
    pub version: String,
    pub supported: bool,
}
#[derive(Clone, Serialize)]
pub struct PreviewFile {
    pub path: String,
    pub size: u64,
    pub client: Client,
    pub selected: bool,
    pub overridden: bool,
}
#[derive(Clone, Serialize)]
struct Binding {
    root: PathBuf,
    root_key: Key,
    versions_key: Option<Key>,
    source: Source,
    source_snapshot: Snapshot,
    archive_limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    inner: Option<InnerBinding>,
}
/// Local paths are re-opened to detect replacement. Official inputs have no
/// pathname authority: their opaque ID refers to a held, verified owned input.
#[derive(Clone, Serialize, PartialEq, Eq)]
enum Source {
    Local(PathBuf),
    Official(String),
}
impl Source {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Local(path) => path.as_os_str().len(),
            Self::Official(id) => id.len(),
        }
    }
}
#[derive(Clone, Serialize)]
struct InnerBinding {
    snapshot: Snapshot,
    outer_entry: OverrideFile,
}
#[derive(Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
enum Output {
    Remote {
        path: String,
        size: u64,
        sha1: String,
        sha512: String,
        downloads: Vec<String>,
    },
    CurseForge(curseforge::ResolvedFile),
    Override(OverrideFile),
}
impl Output {
    fn size(&self) -> u64 {
        match self {
            Self::Remote { size, .. } => *size,
            Self::CurseForge(f) => f.size,
            Self::Override(f) => f.size,
        }
    }
}

fn source_file(source: &Path, limit: u64) -> Result<File> {
    let file = open_source(source)?;
    let stamp = file_stamp(&file)?;
    if !stamp.regular() || stamp.size > limit {
        return Err(format!(
            "整合包源必须为普通文件且不超过 {} MiB",
            limit / (1024 * 1024)
        ));
    }
    Ok(file)
}
fn root_scope(root: &Path, name: &str) -> Result<(Key, Option<Key>)> {
    name_ok(name)?;
    let root = Dir::open(root)?;
    let versions = match root.stat("versions")? {
        None => None,
        Some(stamp) if stamp.directory() => Some(root.child("versions")?),
        Some(_) => return Err("versions 路径不是普通目录".into()),
    };
    if let Some(versions) = &versions {
        if versions.stat(name)?.is_some() {
            return Err("目标实例名称已存在，请使用其他名称".into());
        }
    }
    Ok((root.key()?, versions.as_ref().map(Dir::key).transpose()?))
}

/// Recognition chooses a parser, not authority. It checks the bounded central
/// directory and stable held/path identities without trusting an extension.
pub fn recognizes(source: &Path) -> Result<bool> {
    recognizes_cancellable(source, &AtomicBool::new(false))
}
fn recognizes_cancellable(source: &Path, cancel: &AtomicBool) -> Result<bool> {
    super::check(cancel)?;
    // Dispatch retains the original PCL ZIP source bounds. The smaller mrpack
    // budget applies only after its root marker selects this parser.
    let file = open_source(source)?;
    let before = file_stamp(&file)?;
    if !before.regular() || before.size > super::MAX_BYTES {
        return Err("导入文件类型无效或超过大小限制".into());
    }
    let mut zip = super::archive::checked_zip(file.try_clone().map_err(super::error)?)?;
    let mut index = false;
    let mut pcl = false;
    for i in 0..zip.len() {
        super::check(cancel)?;
        let entry = zip.by_index(i).map_err(|_| "ZIP 目录内容无效")?;
        if entry.name() == INDEX {
            if entry.is_dir() {
                return Err("mrpack 根清单不是普通文件".into());
            }
            index = true;
        }
        if entry.name() == "pcl-export.json" {
            pcl = true;
        }
    }
    if file_stamp(&file)? != before || file_stamp(&open_source(source)?)? != before {
        return Err(super::changed());
    }
    if index && pcl {
        return Err("ZIP 同时包含 PCL 与 Modrinth 根清单，格式有歧义".into());
    }
    super::check(cancel)?;
    Ok(index)
}

pub fn inspect(
    root: &Path,
    source: &Path,
    name: &str,
    optional_paths: Option<&[String]>,
) -> Result<PackPlan> {
    Ok(prepare_checked(root, source, name, optional_paths)?.plan)
}

/// The held descriptor, parsed outputs and root binding move together into the
/// one-use confirmation cache. A serialized preview cannot recreate authority.
struct CheckedPack {
    plan: PackPlan,
    file: File,
    outputs: BTreeMap<String, Output>,
    directories: BTreeSet<String>,
    bundled: Option<formats::BundledCore>,
    rebuild_profile: Option<Vec<u8>>,
    curseforge_files: Vec<(u32, u32)>,
    inner: Option<HeldInnerArchive>,
    official: Option<std::sync::Arc<official::Input>>,
}
/// The original selected source is always `CheckedPack.file`. The inner input
/// is process-owned anonymous content authority; no pathname can recreate it.
struct HeldInnerArchive {
    file: StageFile,
    snapshot: Snapshot,
    outer_entry: OverrideFile,
}
/// Parsed facts stay named so format adapters cannot confuse bundled cores,
/// private runtime declarations or the effective client output collections.
struct PreparedPack {
    plan: PackPlan,
    outputs: BTreeMap<String, Output>,
    directories: BTreeSet<String>,
    bundled: Option<formats::BundledCore>,
    rebuild_profile: Option<Vec<u8>>,
    curseforge_files: Vec<(u32, u32)>,
}
fn prepare_checked(
    root: &Path,
    source: &Path,
    name: &str,
    optional_paths: Option<&[String]>,
) -> Result<CheckedPack> {
    prepare_checked_with_stage(
        root,
        source,
        name,
        optional_paths,
        &AtomicBool::new(false),
        |_| Err("带启动器整合包需要原生确认服务分配匿名内层归档".into()),
    )
}
fn prepare_checked_with_stage(
    root: &Path,
    source: &Path,
    name: &str,
    optional_paths: Option<&[String]>,
    cancel: &AtomicBool,
    allocate_inner: impl FnOnce(u64) -> Result<StageFile>,
) -> Result<CheckedPack> {
    super::check(cancel)?;
    let format = formats::classify_cancellable(source, cancel)?;
    if format == formats::Format::Pcl {
        return Err("PCL 本地导出 ZIP 应使用原生导入流程".into());
    }
    let limit = if format == formats::Format::Modrinth {
        MAX_ARCHIVE
    } else {
        8 * 1024 * 1024 * 1024
    };
    let (root_key, versions_key) = root_scope(root, name)?;
    let file = source_file(source, limit)?;
    // Admission uses only central-directory facts. Reserve the anonymous
    // payload before even the first full source hash reads the outer body.
    let staged_inner = if format == formats::Format::Launcher {
        let entry = formats::nested_entry(&file, cancel)?;
        let mut destination = allocate_inner(entry.size)?;
        let metadata = destination.metadata().map_err(super::error)?;
        if !metadata.is_file() || metadata.len() != 0 || !is_anonymous(&destination)? {
            return Err("内层整合包目标必须是空的匿名普通文件".into());
        }
        destination.seek(SeekFrom::Start(0)).map_err(super::error)?;
        Some((entry, destination))
    } else {
        None
    };
    let before = hash_file(file.try_clone().map_err(super::error)?, limit, Some(cancel))?;
    let mut binding = Binding {
        root: root.to_owned(),
        root_key,
        versions_key,
        source: Source::Local(source.to_owned()),
        source_snapshot: before.clone(),
        archive_limit: limit,
        inner: None,
    };
    let (inner, content_format, decoded_limit) =
        if let Some((entry, mut destination)) = staged_inner {
            let scan = formats::scan_outer(
                file.try_clone().map_err(super::error)?,
                &entry,
                &mut destination,
                cancel,
            )?;
            destination.sync_all().map_err(super::error)?;
            destination.seek(SeekFrom::Start(0)).map_err(super::error)?;
            let snapshot = hash_file(
                destination.try_clone().map_err(super::error)?,
                2 * 1024 * 1024 * 1024,
                Some(cancel),
            )?;
            if snapshot.hash != scan.entry.hash || snapshot.stamp.size != scan.entry.size {
                return Err(super::changed());
            }
            let content_format = formats::classify_held(&destination, cancel)?;
            if content_format == formats::Format::Launcher {
                return Err("仅支持一层内嵌整合包；内层不能再次包含带启动器归档".into());
            }
            if content_format == formats::Format::Pcl {
                return Err("内嵌 PCL 本地导出包请直接选择内层文件，使用原生导入流程".into());
            }
            if content_format == formats::Format::Modrinth && snapshot.stamp.size > MAX_ARCHIVE {
                return Err("内嵌 mrpack 压缩文件超过 512 MiB 限制".into());
            }
            binding.inner = Some(InnerBinding {
                snapshot: snapshot.clone(),
                outer_entry: scan.entry.clone(),
            });
            (
                Some(HeldInnerArchive {
                    file: destination,
                    snapshot,
                    outer_entry: scan.entry,
                }),
                content_format,
                formats::DECODED_BYTES
                    .checked_sub(scan.decoded_bytes)
                    .ok_or("两层整合包解压内容超过 16 GiB 限制")?,
            )
        } else {
            (None, format, formats::DECODED_BYTES)
        };
    let content_file = inner.as_ref().map_or(&file, |inner| &inner.file);
    let PreparedPack {
        mut plan,
        outputs,
        directories,
        bundled,
        rebuild_profile,
        curseforge_files,
    } = if inner.is_none()
        && format == formats::Format::Modrinth
        && recognizes_cancellable(source, cancel)?
    {
        let scan = archive::scan_cancellable(file.try_clone().map_err(super::error)?, cancel)?;
        let manifest = manifest::parse(&scan.index)?;
        let optional = selection(&manifest, optional_paths)?;
        let (plan, outputs, dirs) = build(name, manifest, scan, optional, binding)?;
        PreparedPack {
            plan,
            outputs,
            directories: dirs,
            bundled: None,
            rebuild_profile: None,
            curseforge_files: Vec::new(),
        }
    } else {
        from_archive(
            name,
            content_format,
            formats::prepare_with_budget(
                content_file.try_clone().map_err(super::error)?,
                name,
                content_format,
                optional_paths,
                cancel,
                decoded_limit,
            )?,
            binding,
        )?
    };
    if inner.is_some() {
        plan.warnings
            .push("已解析带启动器归档中的唯一内层整合包；外层启动器和配置不会安装".into());
    }
    let ignored_mirrors = outputs
        .values()
        .filter_map(|output| match output {
            Output::Remote { downloads, .. } => Some(
                downloads
                    .iter()
                    .filter(|url| matches!(mirror::local_declaration(url), Ok(None)))
                    .count(),
            ),
            Output::Override(_) | Output::CurseForge(_) => None,
        })
        .sum::<usize>();
    if ignored_mirrors > 0 {
        // Preserve an excluded declaration as a fact without echoing URLs or
        // query credentials into a warning, log or task projection.
        plan.warnings
            .push(format!("忽略 {ignored_mirrors} 个不受支持的下载镜像声明"));
    }
    // The held descriptor and the current pathname must both retain the full
    // original stamp/hash. This permits read-only hard links while detecting
    // replacement or edits through any alias during decompression/planning.
    if hash_file(file.try_clone().map_err(super::error)?, limit, Some(cancel))? != before {
        return Err(super::changed());
    }
    super::check(cancel)?;
    plan.recheck_cancellable(cancel)?;
    if let Some(inner) = &inner {
        if !is_anonymous(&inner.file)?
            || hash_file(
                inner.file.try_clone().map_err(super::error)?,
                2 * 1024 * 1024 * 1024,
                Some(cancel),
            )? != inner.snapshot
        {
            return Err(super::changed());
        }
    }
    plan.revision = format!(
        "mrpack-v1:{:x}",
        Sha256::digest(serde_json::to_vec(&plan.revision_facts()?).map_err(super::error)?)
    );
    Ok(CheckedPack {
        plan,
        file,
        outputs,
        directories,
        bundled,
        rebuild_profile,
        curseforge_files,
        inner,
        official: None,
    })
}

fn selection(manifest: &Manifest, paths: Option<&[String]>) -> Result<BTreeSet<String>> {
    let paths = paths.unwrap_or(&[]);
    if paths.len() > MAX_ENTRIES {
        return Err("可选文件选择超过 10000 项限制".into());
    }
    let mut selected = BTreeSet::new();
    for path in paths {
        manifest::output_path(path)?;
        if !manifest
            .files
            .iter()
            .any(|f| &f.path == path && f.env.client == Client::Optional)
        {
            return Err("只能选择清单中声明为客户端可选的文件".into());
        }
        if !selected.insert(path.clone()) {
            return Err("可选文件选择包含重复路径".into());
        }
    }
    Ok(selected)
}

fn build(
    name: &str,
    manifest: Manifest,
    scan: ArchiveScan,
    optional: BTreeSet<String>,
    binding: Binding,
) -> Result<(PackPlan, BTreeMap<String, Output>, BTreeSet<String>)> {
    let (dependencies, mut blockers) = manifest::dependencies(&manifest.dependencies);
    let mut files = Vec::new();
    let mut outputs = BTreeMap::new();
    let mut required_files = 0;
    let mut optional_files = 0;
    let mut excluded_files = 0;
    let mut mirrors = BTreeMap::new();
    for file in &manifest.files {
        let selected = match file.env.client {
            Client::Required => {
                required_files += 1;
                true
            }
            Client::Optional => {
                optional_files += 1;
                optional.contains(&file.path)
            }
            Client::Unsupported => false,
        };
        if !selected {
            excluded_files += 1;
        } else {
            outputs.insert(
                file.path.clone(),
                Output::Remote {
                    path: file.path.clone(),
                    size: file.file_size,
                    sha1: file.hashes["sha1"].clone(),
                    sha512: file.hashes["sha512"].clone(),
                    downloads: file.downloads.clone(),
                },
            );
            mirrors.insert(
                file.path.clone(),
                file.has_supported_mirror(matches!(binding.source, Source::Local(_))),
            );
        }
        files.push(PreviewFile {
            path: file.path.clone(),
            size: file.file_size,
            client: file.env.client,
            selected,
            overridden: false,
        });
    }
    let mut shadowed_files = 0;
    for override_file in scan.common.into_values().chain(scan.client.into_values()) {
        if outputs
            .insert(override_file.path.clone(), Output::Override(override_file))
            .is_some()
        {
            shadowed_files += 1;
        }
    }
    archive::validate_outputs(&outputs.keys().cloned().collect(), &scan.directories)?;
    let mut download_bytes = 0u64;
    let mut override_bytes = 0u64;
    let mut override_files = 0;
    let mut client_overrides = 0;
    let mut bytes = 0u64;
    for (path, output) in &outputs {
        if path == &format!("{name}.jar") || path == &format!("{name}.json") {
            return Err("mrpack 输出与目标实例核心文件名冲突".into());
        }
        bytes = bytes
            .checked_add(output.size())
            .filter(|n| *n <= MAX_OUTPUT)
            .ok_or("mrpack 有效客户端输出超过 8 GiB 限制")?;
        match output {
            Output::Remote { size, .. } => {
                download_bytes += size;
                if !mirrors[path] {
                    blockers.push(format!("文件 {path} 没有受支持的 HTTPS 下载镜像"));
                }
            }
            Output::CurseForge(_) => return Err("Modrinth 清单不能声明 CurseForge 文件".into()),
            Output::Override(file) => {
                override_files += 1;
                override_bytes += file.size;
                if file.client {
                    client_overrides += 1;
                }
            }
        }
    }
    for file in &mut files {
        file.overridden = matches!(outputs.get(&file.path), Some(Output::Override(_)));
    }
    let mut warnings = vec!["此计划仅用于本地预览；未连接网络，尚未核实游戏和加载器版本".into()];
    if scan.foreign_files > 0 {
        warnings.push(foreign::warning(scan.foreign_files));
    }
    if scan.server_files > 0 {
        warnings.push(format!(
            "忽略 {} 个 server-overrides 文件；客户端不应用该层",
            scan.server_files
        ));
    }
    if scan.ignored_files > 0 {
        warnings.push(format!(
            "忽略 {} 个覆盖层以外的归档文件",
            scan.ignored_files
        ));
    }
    let minecraft = manifest.dependencies["minecraft"].clone();
    let mut plan = PackPlan {
        revision: String::new(),
        name: name.into(),
        pack_name: manifest.name,
        pack_version: manifest.version_id,
        minecraft,
        file_count: outputs.len(),
        bytes,
        reused_files: 0,
        warnings,
        format: "modrinth",
        installable: false,
        preview: Preview {
            summary: manifest.summary,
            dependencies,
            files,
            required_files,
            optional_files,
            excluded_files,
            download_bytes,
            override_files,
            override_bytes,
            client_overrides,
            shadowed_files,
            blockers,
        },
        binding,
    };
    // Source hash already binds all declarations/overlays, including excluded
    // server data. The derived output/selection digest makes precedence and
    // optional authorization explicit without retaining decompressed bodies.
    plan.revision = format!(
        "overlay:{:x}",
        Sha256::digest(serde_json::to_vec(&(optional, &outputs)).map_err(super::error)?)
    );
    Ok((plan, outputs, scan.directories))
}
impl PackPlan {
    #[cfg(test)]
    pub fn recheck(&self) -> Result<()> {
        self.recheck_cancellable(&AtomicBool::new(false))
    }
    pub(crate) fn recheck_cancellable(&self, cancel: &AtomicBool) -> Result<()> {
        super::check(cancel)?;
        let (root_key, versions_key) = root_scope(&self.binding.root, &self.name)?;
        if root_key != self.binding.root_key
            || versions_key != self.binding.versions_key
            || match &self.binding.source {
                Source::Local(path) => {
                    hash_file(
                        source_file(path, self.binding.archive_limit)?,
                        self.binding.archive_limit,
                        Some(cancel),
                    )? != self.binding.source_snapshot
                }
                Source::Official(_) => false,
            }
        {
            return Err(super::changed());
        }
        Ok(())
    }
    fn revision_facts(&self) -> Result<impl Serialize + '_> {
        Ok((&self.binding, &self.revision, &self.name, &self.preview))
    }
}

#[cfg(test)]
#[path = "mrpack/nested_review_tests.rs"]
mod nested_review_tests;
#[cfg(test)]
#[path = "mrpack/nested_tests.rs"]
mod nested_tests;
#[cfg(test)]
#[path = "mrpack/tests.rs"]
mod tests;

impl CheckedPack {
    fn content_file(&self) -> Result<File> {
        self.inner
            .as_ref()
            .map_or(&self.file, |inner| &inner.file)
            .try_clone()
            .map_err(super::error)
    }
    fn retained_source_fds(&self) -> usize {
        1 + usize::from(self.inner.is_some())
    }
    fn retained_inner_bytes(&self) -> u64 {
        self.inner
            .as_ref()
            .map_or(0, |inner| inner.outer_entry.size)
    }
    fn recheck_source(&self, cancel: &std::sync::atomic::AtomicBool) -> Result<()> {
        super::check(cancel)?;
        let _reader = self
            .official
            .as_ref()
            .map(|input| input.reader.lock().unwrap());
        let before = &self.plan.binding.source_snapshot;
        if hash_file(
            self.file.try_clone().map_err(super::error)?,
            self.plan.binding.archive_limit,
            Some(cancel),
        )? != *before
            || match &self.plan.binding.source {
                Source::Local(path) => {
                    hash_file(
                        source_file(path, self.plan.binding.archive_limit)?,
                        self.plan.binding.archive_limit,
                        Some(cancel),
                    )? != *before
                }
                Source::Official(_) => !self
                    .official
                    .as_ref()
                    .ok_or("官方整合包缺少来源所有权")?
                    .owns(&self.file)?,
            }
        {
            return Err(super::changed());
        }
        if let Some(inner) = &self.inner {
            if !is_anonymous(&inner.file)?
                || hash_file(
                    inner.file.try_clone().map_err(super::error)?,
                    2 * 1024 * 1024 * 1024,
                    Some(cancel),
                )? != inner.snapshot
            {
                return Err(super::changed());
            }
        }
        Ok(())
    }
    fn recheck_source_stamp(&self) -> Result<()> {
        let expected = &self.plan.binding.source_snapshot.stamp;
        if file_stamp(&self.file)? != *expected
            || match &self.plan.binding.source {
                Source::Local(path) => file_stamp(&open_source(path)?)? != *expected,
                Source::Official(_) => !self
                    .official
                    .as_ref()
                    .ok_or("官方整合包缺少来源所有权")?
                    .owns(&self.file)?,
            }
        {
            return Err(super::changed());
        }
        if let Some(inner) = &self.inner {
            if file_stamp(&inner.file)? != inner.snapshot.stamp || !is_anonymous(&inner.file)? {
                return Err(super::changed());
            }
        }
        Ok(())
    }
    fn request(&self) -> Result<pcl_install::InstallRequest> {
        let mut components = Vec::new();
        for dependency in &self.plan.preview.dependencies {
            let provider = match dependency.id.as_str() {
                "minecraft" => continue,
                "fabric-loader" => "fabric",
                "forge" => "forge",
                "neoforge" => "neoforge",
                _ => return Err("整合包声明了尚不支持的游戏组件".into()),
            };
            components.push(pcl_install::ComponentSelection {
                provider: provider.into(),
                version: dependency.version.clone(),
            });
        }
        let request = pcl_install::InstallRequest {
            minecraft: self.plan.minecraft.clone(),
            name: self.plan.name.clone(),
            components,
        };
        request.validate()?;
        Ok(request)
    }
}

/// The marker route is separate from execution authority. All non-PCL formats
/// use a native pack confirmation; the original exported ZIP keeps its DTO.
pub(crate) fn is_local_export(source: &Path) -> Result<bool> {
    is_local_export_cancellable(source, &AtomicBool::new(false))
}
pub(crate) fn is_local_export_cancellable(source: &Path, cancel: &AtomicBool) -> Result<bool> {
    Ok(formats::classify_cancellable(source, cancel)? == formats::Format::Pcl)
}
fn from_archive(
    name: &str,
    format: formats::Format,
    prepared: formats::PreparedArchive,
    binding: Binding,
) -> Result<PreparedPack> {
    let formats::PreparedArchive {
        pack_name,
        version,
        minecraft,
        dependencies,
        outputs,
        directories,
        warnings,
        blockers,
        summary,
        preview_files: files,
        optional,
        bundled,
        shadowed_files,
        rebuild_profile,
        curseforge_files,
    } = prepared;
    let (mut dependencies, _) = manifest::dependencies(&dependencies);
    if bundled.is_some() {
        for d in &mut dependencies {
            d.supported = true;
        }
    }
    let required_files = files
        .iter()
        .filter(|f| f.client == Client::Required)
        .count();
    let optional_files = files
        .iter()
        .filter(|f| f.client == Client::Optional)
        .count();
    let excluded_files = files.iter().filter(|f| !f.selected).count();
    let download_bytes = outputs
        .values()
        .filter_map(|o| {
            if let Output::Remote { size, .. } = o {
                Some(*size)
            } else {
                None
            }
        })
        .sum();
    let override_bytes = outputs
        .values()
        .filter_map(|o| {
            if let Output::Override(f) = o {
                Some(f.size)
            } else {
                None
            }
        })
        .sum();
    let override_files = outputs
        .values()
        .filter(|o| matches!(o, Output::Override(_)))
        .count();
    let client_overrides = outputs
        .values()
        .filter(|o| matches!(o,Output::Override(f) if f.client))
        .count();
    let mut bytes = outputs.values().map(Output::size).sum::<u64>();
    let mut file_count = outputs.len();
    if let Some(core) = &bundled {
        bytes = bytes
            .checked_add(core.jar.size)
            .and_then(|n| n.checked_add(core.metadata.len() as u64))
            .and_then(|n| n.checked_add(core.shared.values().map(|f| f.size).sum::<u64>()))
            .ok_or("整合包有效输出大小超过限制")?;
        file_count += 2 + core.shared.len();
    }
    let overlay = format!(
        "overlay:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&outputs, &directories, optional)).map_err(super::error)?
        )
    );
    let plan = PackPlan {
        revision: overlay,
        name: name.into(),
        pack_name,
        pack_version: version,
        minecraft,
        file_count,
        bytes,
        reused_files: 0,
        warnings,
        format: format.as_str(),
        installable: false,
        preview: Preview {
            summary,
            dependencies,
            files,
            required_files,
            optional_files,
            excluded_files,
            download_bytes,
            override_files,
            override_bytes,
            client_overrides,
            shadowed_files,
            blockers,
        },
        binding,
    };
    Ok(PreparedPack {
        plan,
        outputs,
        directories,
        bundled,
        rebuild_profile,
        curseforge_files,
    })
}
