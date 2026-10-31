//! Instance publication for legacy `pcl-local-instance` ZIP and native packs.
//!
//! `prepare` owns a read-only source/target snapshot. Callers keep that typed
//! plan server-side and hold the common game-root writer admission for execute
//! and recovery. Archive paths never become OS paths: validated components are
//! opened relative to pinned no-follow directory FDs, with NO_XDEV inside root.
//!
//! Schema 1 preserves the legacy ZIP transaction; schema 2 first registers a
//! Building private root before the installer generates any output. Both share
//! Staging, which owns anonymous, registered extraction files;
//! Prepared authorizes no-overwrite publication; Committed retains the imported
//! instance and only finishes staging cleanup. Earlier states roll back only
//! outputs whose inode and bytes still match the journal. Cleanup conflicts
//! remain errors and keep the launch/write guard, including after cancellation.
use pcl_install::{InstallStep, Progress};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use zip::ZipArchive;
mod archive;
pub(crate) mod build;
mod filesystem;
mod journal;
pub(crate) mod local_snapshot;
pub(crate) mod mrpack;
pub(crate) mod publication;
use archive::{checked_zip, legacy_resources, resolve_version, scan_archive, ArchiveEntry};
use filesystem::{
    file_stamp, hash_file, is_anonymous, link_anonymous, link_file, lock, move_new, open_source,
    source_snapshot, Dir, ImportLock, Key, Snapshot, StageFile,
};
use journal::{BuildRegistration, Journal, State};

type Result<T> = std::result::Result<T, String>;
const STORE: &str = "instance-imports";
const MAX_FILES: usize = 100_000;
const MAX_DEPTH: usize = 96;
const MAX_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const MAX_FILE: u64 = 8 * 1024 * 1024 * 1024;
const MAX_JSON: u64 = 32 * 1024 * 1024;
const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_JOURNAL: u64 = 64 * 1024 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// JSON is a reviewable summary, never a deserializable authorization token.
#[derive(Clone, Debug, Serialize)]
pub struct ImportPlan {
    pub revision: String,
    pub name: String,
    pub pack_name: String,
    pub pack_version: String,
    pub minecraft: String,
    pub file_count: usize,
    pub bytes: u64,
    pub reused_files: usize,
    pub warnings: Vec<String>,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    source: PathBuf,
    #[serde(skip)]
    source_snapshot: Snapshot,
    #[serde(skip)]
    root_key: Key,
    #[serde(skip)]
    targets: BTreeMap<String, Target>,
    #[serde(skip)]
    files: Vec<PlannedFile>,
    #[serde(skip)]
    instance_dirs: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "snapshot",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Target {
    Absent,
    Directory(Key),
    File(Snapshot),
}
#[derive(Clone, Debug, Serialize)]
struct PlannedFile {
    target: String,
    size: u64,
    hash: String,
    reuse: bool,
    #[serde(skip)]
    source: FileSource,
}
#[derive(Clone, Debug)]
enum FileSource {
    Archive(ArchiveEntry),
    Generated(Vec<u8>),
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[cfg(target_os = "linux")]
fn os_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}
fn changed() -> String {
    "导入包或目标游戏目录已经变化，请重新检查导入计划".into()
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err("实例导入已取消".into())
    } else {
        Ok(())
    }
}
fn component(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', ':', '\0'])
        || name.chars().any(char::is_control)
    {
        Err("导入包包含危险或无效路径".into())
    } else {
        pcl_core::retained_fs::Leaf::new(name)
            .map(|_| ())
            .map_err(error)
    }
}
fn relative(path: &str) -> Result<Vec<&str>> {
    let parts: Vec<_> = path.split('/').collect();
    if path.is_empty() || path.len() > 4096 || parts.len() > MAX_DEPTH {
        return Err("导入包路径过长或目录层级过深".into());
    }
    for name in &parts {
        component(name)?;
    }
    Ok(parts)
}
fn name_ok(name: &str) -> Result<()> {
    component(name)?;
    if name.len() > 120
        || name.trim() != name
        || name.starts_with(".install-")
        || name.starts_with(".pcl-")
    {
        return Err("实例名称不能超过 120 字节、含前后空格或使用启动器保留前缀".into());
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn cstring(name: &str) -> Result<std::ffi::CString> {
    std::ffi::CString::new(name).map_err(error)
}
fn nonce() -> String {
    format!(
        "i-{:x}-{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}
fn operation_ok(name: &str) -> bool {
    name.starts_with("i-")
        && name.len() <= 80
        && name
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_hexdigit() || b == b'i')
}
fn forbidden(path: &str) -> bool {
    path.split('/').any(|n| {
        let n = n.to_ascii_lowercase();
        n.starts_with(".pcl-")
            || n.starts_with(".install-")
            || matches!(
                n.as_str(),
                "pcl"
                    | ".pcl"
                    | ".git"
                    | ".pcl-linux"
                    | "launcher_accounts.json"
                    | "launcher_profiles.json"
                    | "accounts.json"
                    | "account.json"
            )
    })
}

fn capture_target(root: &Dir, path: &str) -> Result<Target> {
    let parts = relative(path)?;
    let mut dir = root.duplicate()?;
    for (i, part) in parts.iter().enumerate() {
        let Some(stamp) = dir.stat(part)? else {
            return Ok(Target::Absent);
        };
        if i == parts.len() - 1 {
            return if stamp.directory() {
                Ok(Target::Directory(dir.child(part)?.key()?))
            } else if stamp.regular() {
                Ok(Target::File(hash_file(dir.regular(part)?, MAX_FILE, None)?))
            } else {
                Err("目标路径包含符号链接或特殊文件".into())
            };
        }
        dir = dir.child(part)?;
    }
    Err("目标路径无效".into())
}
fn remember_target(root: &Dir, targets: &mut BTreeMap<String, Target>, path: &str) -> Result<()> {
    if !targets.contains_key(path) {
        targets.insert(path.into(), capture_target(root, path)?);
        if targets.len() > MAX_FILES * 2 {
            return Err("导入共享资源文件与目录节点数量过多".into());
        }
    }
    Ok(())
}
fn add_parents(root: &Dir, targets: &mut BTreeMap<String, Target>, path: &str) -> Result<()> {
    let parts = relative(path)?;
    for depth in 1..parts.len() {
        let parent = parts[..depth].join("/");
        remember_target(root, targets, &parent)?;
        if matches!(targets[&parent], Target::File(_)) {
            return Err("目标共享资源的父路径是文件".into());
        }
    }
    Ok(())
}
fn guards(root: &Path) -> Result<()> {
    crate::resource_ops::ensure_ready(root)?;
    crate::instance_reset::ensure_ready(root)?;
    crate::instance_rename::ensure_ready(root)?;
    ensure_ready(root)
}

pub fn prepare(root: &Path, archive: &Path, name: &str) -> Result<ImportPlan> {
    name_ok(name)?;
    let root = root.canonicalize().map_err(error)?;
    guards(&root)?;
    crate::instance_delete::ensure_name_available(&root, name)?;
    let dir = Dir::open(&root)?;
    let root_key = dir.key()?;
    let source = archive.to_path_buf();
    let source_snapshot = source_snapshot(&source, None)?;
    let scan = scan_archive(open_source(&source)?, &AtomicBool::new(false))?;
    if source_snapshot != self::source_snapshot(&source, None)? {
        return Err(changed());
    }
    let target_instance = format!("versions/{name}");
    let mut targets = BTreeMap::new();
    remember_target(&dir, &mut targets, "versions")?;
    remember_target(&dir, &mut targets, &target_instance)?;
    if !matches!(targets[&target_instance], Target::Absent) {
        return Err("目标实例名称已存在，请使用其他名称".into());
    }
    if matches!(targets["versions"], Target::File(_)) {
        return Err("versions 路径不是目录".into());
    }
    let legacy_resources = legacy_resources(&scan)?;
    let mut used = BTreeSet::new();
    let (mut metadata, jar) = resolve_version(
        &scan,
        &scan.manifest.game.instance,
        &mut BTreeSet::new(),
        &mut used,
    )?;
    let object = metadata.as_object_mut().ok_or("版本 JSON 无效")?;
    object.remove("inheritsFrom");
    object.insert("id".into(), name.into());
    object.insert("jar".into(), name.into());
    // clientVersion carries the actual Minecraft identity after custom naming.
    object.insert(
        "clientVersion".into(),
        scan.manifest.game.minecraft.clone().into(),
    );
    let metadata = serde_json::to_vec_pretty(&metadata).map_err(error)?;
    if metadata.len() as u64 > MAX_JSON {
        return Err("合并后的版本 JSON 超过大小限制".into());
    }
    let mut outputs: BTreeMap<String, FileSource> = BTreeMap::new();
    outputs.insert(
        format!("{target_instance}/{name}.json"),
        FileSource::Generated(metadata),
    );
    outputs.insert(
        format!("{target_instance}/{name}.jar"),
        FileSource::Archive(scan.entries[&jar].clone()),
    );
    let mut instance_dirs = BTreeSet::from(["config".into()]);
    for (path, entry) in &scan.entries {
        if path == "pcl-export.json" || used.contains(path) {
            continue;
        }
        let relative = path.strip_prefix(".minecraft/").ok_or("导出包布局无效")?;
        let content = if scan.manifest.game.isolated {
            path.strip_prefix(&format!("{}/", scan.manifest.content_root))
        } else {
            Some(relative)
        };
        let shared = relative.starts_with("libraries/")
            || relative == "libraries"
            || relative.starts_with("assets/")
            || relative == "assets"
            || legacy_resources.contains(relative)
            || (scan.manifest.game.isolated
                && (relative == "resources" || relative.starts_with("resources/")));
        if shared {
            if !scan.manifest.bundled_assets {
                return Err("未声明打包资源的 ZIP 包含共享支持库或资源".into());
            }
            if !entry.directory {
                if outputs
                    .insert(relative.into(), FileSource::Archive(entry.clone()))
                    .is_some()
                {
                    return Err("ZIP 文件映射到重复的导入目标".into());
                }
                if !scan.manifest.game.isolated && legacy_resources.contains(relative) {
                    outputs.insert(
                        format!("{target_instance}/{relative}"),
                        FileSource::Archive(entry.clone()),
                    );
                }
            } else {
                add_parents(&dir, &mut targets, relative)?;
                remember_target(&dir, &mut targets, relative)?;
                if matches!(targets[relative], Target::File(_)) {
                    return Err("目标共享资源目录已被文件占用".into());
                }
            }
        } else if relative.starts_with("versions/") {
            let Some(content) = content else {
                return Err("ZIP 包含未声明的其他版本文件".into());
            };
            if entry.directory {
                instance_dirs.insert(content.into());
            } else if outputs
                .insert(
                    format!("{target_instance}/{content}"),
                    FileSource::Archive(entry.clone()),
                )
                .is_some()
            {
                return Err("游戏内容与新实例核心文件冲突".into());
            }
        } else if let Some(content) = content {
            if matches!(
                content.split('/').next(),
                Some(
                    "versions"
                        | "libraries"
                        | "assets"
                        | "runtime"
                        | "runtimes"
                        | "bin"
                        | "java"
                        | "jre"
                        | "jre8"
                        | "jre17"
                        | "jre21"
                )
            ) {
                return Err("ZIP 包含不支持的游戏内容布局".into());
            }
            if entry.directory {
                instance_dirs.insert(content.into());
            } else if outputs
                .insert(
                    format!("{target_instance}/{content}"),
                    FileSource::Archive(entry.clone()),
                )
                .is_some()
            {
                return Err("游戏内容与新实例核心文件冲突".into());
            }
        } else {
            return Err("ZIP 文件不属于清单声明的实例或共享资源".into());
        }
    }
    if outputs.len() > MAX_FILES {
        return Err("导入映射文件数量超过安全限制".into());
    }
    let mut files = Vec::new();
    let mut bytes = 0u64;
    let mut reused_files = 0;
    for (target, source) in outputs {
        relative(&target)?;
        let (size, hash) = match &source {
            FileSource::Archive(e) => (e.size, e.hash.clone()),
            FileSource::Generated(b) => (b.len() as u64, format!("{:x}", Sha256::digest(b))),
        };
        let inside = target.starts_with(&format!("{target_instance}/"));
        let reuse = if inside {
            let rel = target
                .strip_prefix(&format!("{target_instance}/"))
                .ok_or("目标路径无效")?;
            let p = relative(rel)?;
            for depth in 1..p.len() {
                instance_dirs.insert(p[..depth].join("/"));
                if instance_dirs.len() > MAX_FILES {
                    return Err("导入实例目录节点数量过多".into());
                }
            }
            false
        } else {
            add_parents(&dir, &mut targets, &target)?;
            remember_target(&dir, &mut targets, &target)?;
            match &targets[&target] {
                Target::Absent => false,
                Target::File(existing) if existing.stamp.size == size && existing.hash == hash => {
                    true
                }
                _ => {
                    return Err(format!(
                        "目标已有不同内容的共享资源，已保留原文件：{target}"
                    ))
                }
            }
        };
        if reuse {
            reused_files += 1;
        }
        bytes = bytes
            .checked_add(size)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or("导入内容超过大小限制")?;
        files.push(PlannedFile {
            target,
            size,
            hash,
            reuse,
            source,
        });
    }
    for path in instance_dirs.clone() {
        let p = relative(&path)?;
        for depth in 1..p.len() {
            instance_dirs.insert(p[..depth].join("/"));
            if instance_dirs.len() > MAX_FILES {
                return Err("导入实例目录节点数量过多".into());
            }
        }
    }
    // A malicious directory declaration must not become an empty directory
    // over a generated JSON/JAR or one of the imported game files.
    let file_names: BTreeSet<_> = files.iter().map(|f| f.target.as_str()).collect();
    for path in &instance_dirs {
        relative(path)?;
        if file_names.contains(format!("{target_instance}/{path}").as_str()) {
            return Err("游戏内容目录与核心文件冲突".into());
        }
    }
    let mut digest = Sha256::new();
    digest.update(b"pcl-local-import-plan-v1\0");
    digest.update(
        serde_json::to_vec(&(
            &root,
            &source,
            &source_snapshot,
            &root_key,
            &targets,
            &files,
            &instance_dirs,
            name,
        ))
        .map_err(error)?,
    );
    let mut warnings =
        vec!["此导入仅支持本启动器导出的本地 ZIP；所有游戏内容将放入新实例的独立目录。".into()];
    if !scan.manifest.bundled_assets {
        warnings.push(
            "导出包未包含支持库和游戏资源；导入不会联网下载，启动前可能仍需补齐依赖。".into(),
        );
    }
    if scan.manifest.game.isolated == false {
        warnings.push("来源使用共享游戏内容；本次导入将这些内容复制为独立实例。".into());
    }
    let plan = ImportPlan {
        revision: format!("{:x}", digest.finalize()),
        name: name.into(),
        pack_name: scan.manifest.name,
        pack_version: scan.manifest.version,
        minecraft: scan.manifest.game.minecraft,
        file_count: files.len(),
        bytes,
        reused_files,
        warnings,
        root,
        source,
        source_snapshot,
        root_key,
        targets,
        files,
        instance_dirs,
    };
    validate_targets(&dir, &plan.targets)?;
    if Dir::open(&plan.root)?.key()? != plan.root_key {
        return Err(changed());
    }
    Ok(plan)
}

fn validate_targets(root: &Dir, targets: &BTreeMap<String, Target>) -> Result<()> {
    for (path, expected) in targets {
        if capture_target(root, path)? != *expected {
            return Err(changed());
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    target: String,
    size: u64,
    hash: String,
    reuse: bool,
    staged: Option<Snapshot>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationDirectory {
    before: Option<Key>,
    created: Option<Key>,
}
/// The full plan changes only at durable state boundaries. Ownership is
/// registered in one small immutable sidecar per extracted file/directory;
/// serializing the complete plan per copy would make real packs quadratic.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    schema: u32,
    operation_key: Key,
    record: OwnershipRecord,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OwnershipRecord {
    File { index: usize, snapshot: Snapshot },
    InstanceDirectory { path: String, key: Key },
    DestinationDirectory { path: String, key: Key },
}
#[cfg(test)]
thread_local! { static FAIL_COMMITTED_JOURNAL_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
thread_local! { static FULL_JOURNAL_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn record_name(record: &OwnershipRecord, j: &Journal) -> Result<String> {
    match record {
        OwnershipRecord::File { index, .. } => Ok(format!("owned-f-{index}.json")),
        OwnershipRecord::InstanceDirectory { path, .. } => {
            if !j.instance_dirs.contains_key(path) {
                return Err("实例目录登记不属于导入计划".into());
            }
            Ok(format!(
                "owned-i-{:x}.json",
                Sha256::digest(path.as_bytes())
            ))
        }
        OwnershipRecord::DestinationDirectory { path, .. } => {
            if !j.destination_dirs.contains_key(path) {
                return Err("共享目录登记不属于导入计划".into());
            }
            Ok(format!(
                "owned-d-{:x}.json",
                Sha256::digest(path.as_bytes())
            ))
        }
    }
}
fn read_ownership(operation: &Dir, name: &str, j: &Journal) -> Result<Ownership> {
    let mut file = operation.regular(name)?;
    if file.metadata().map_err(error)?.len() > 16 * 1024 {
        return Err("导入所有权登记过大".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    let owned: Ownership =
        serde_json::from_slice(&bytes).map_err(|_| "导入所有权登记损坏，已保留文件")?;
    if bytes.len() > 16 * 1024
        || owned.schema != 1
        || owned.operation_key != j.operation_key
        || record_name(&owned.record, j)? != name
    {
        return Err("导入所有权登记与当前操作不匹配".into());
    }
    Ok(owned)
}
fn apply_ownership(j: &mut Journal, record: OwnershipRecord) -> Result<()> {
    match record {
        OwnershipRecord::File { index, snapshot } => {
            valid_snapshot(&snapshot)?;
            let f = j.files.get_mut(index).ok_or("导入所有权登记文件编号无效")?;
            if f.reuse
                || snapshot.stamp.size != f.size
                || snapshot.hash != f.hash
                || f.staged.as_ref().is_some_and(|s| s != &snapshot)
            {
                return Err("导入文件所有权登记冲突".into());
            }
            f.staged = Some(snapshot);
        }
        OwnershipRecord::InstanceDirectory { path, key } => {
            let existing = j
                .instance_dirs
                .get_mut(&path)
                .ok_or("实例目录所有权登记无效")?;
            if existing.as_ref().is_some_and(|k| k != &key) {
                return Err("实例目录所有权登记冲突".into());
            }
            *existing = Some(key);
        }
        OwnershipRecord::DestinationDirectory { path, key } => {
            let d = j
                .destination_dirs
                .get_mut(&path)
                .ok_or("共享目录所有权登记无效")?;
            if d.before.is_some() || d.created.as_ref().is_some_and(|k| k != &key) {
                return Err("共享目录所有权登记冲突".into());
            }
            d.created = Some(key);
        }
    }
    Ok(())
}
fn register_ownership(operation: &Dir, j: &Journal, record: OwnershipRecord) -> Result<()> {
    validate_binding(operation, j)?;
    let name = record_name(&record, j)?;
    let owned = Ownership {
        schema: 1,
        operation_key: j.operation_key.clone(),
        record,
    };
    let bytes = serde_json::to_vec(&owned).map_err(error)?;
    let mut file = operation.stage_for(&name, MAX_JOURNAL)?;
    file.write_all(&bytes).map_err(error)?;
    file.sync_all().map_err(error)?;
    link_anonymous(&mut file, operation, &name)
}
fn hydrate_ownership(operation: &Dir, j: &mut Journal) -> Result<()> {
    for name in operation.names()? {
        if name.starts_with("owned-") {
            let owned = read_ownership(operation, &name, j)?;
            apply_ownership(j, owned.record)?;
        }
    }
    Ok(())
}
fn storage(root: &Dir, create: bool) -> Result<Option<Dir>> {
    let pcl = if create {
        Some(root.ensure(".pcl-linux")?)
    } else {
        root.optional(".pcl-linux")?
    };
    match pcl {
        None => Ok(None),
        Some(pcl) => {
            if create {
                pcl.ensure(STORE).map(Some)
            } else {
                pcl.optional(STORE)
            }
        }
    }
}
fn valid_snapshot(snapshot: &Snapshot) -> Result<()> {
    if !snapshot.stamp.regular()
        || snapshot.stamp.size > MAX_FILE
        || snapshot.hash.len() != 64
        || !snapshot.hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("导入恢复记录中的文件校验无效".into());
    }
    Ok(())
}
fn shared(path: &str) -> bool {
    matches!(
        path.split('/').next(),
        Some("libraries" | "assets" | "resources")
    )
}
fn validate_journal(j: &Journal, name: &str) -> Result<()> {
    let invalid = || "导入恢复记录无效，已保留文件供检查".to_owned();
    if !journal::valid_format(j)
        || j.operation_id != name
        || !operation_ok(name)
        || !j.root.is_absolute()
        || name_ok(&j.name).is_err()
        || j.files.len() > MAX_FILES
        || j.targets.len() > MAX_FILES * MAX_DEPTH
        || j.instance_dirs.len() > MAX_FILES
        || j.destination_dirs.len() > MAX_FILES
    {
        return Err(invalid());
    }
    if j.state == State::Building
        || (j.schema == 2 && j.state == State::RolledBack && j.files.is_empty())
    {
        return journal::validate_building(j);
    }
    let prefix = format!("versions/{}/", j.name);
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for f in &j.files {
        relative(&f.target)?;
        if forbidden(&f.target)
            || (!f.target.starts_with(&prefix) && !shared(&f.target))
            || !names.insert(&f.target)
            || f.size > MAX_FILE
            || f.hash.len() != 64
            || !f.hash.bytes().all(|b| b.is_ascii_hexdigit())
            || (f.reuse && f.target.starts_with(&prefix))
        {
            return Err(invalid());
        }
        total = total
            .checked_add(f.size)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(invalid)?;
        if let Some(snapshot) = &f.staged {
            valid_snapshot(snapshot)?;
            if snapshot.stamp.size != f.size || snapshot.hash != f.hash || f.reuse {
                return Err(invalid());
            }
        }
        if matches!(
            j.state,
            State::Prepared | State::Committed | State::Finished
        ) && !f.reuse
            && f.staged.is_none()
        {
            return Err(invalid());
        }
    }
    for path in j.instance_dirs.keys() {
        relative(path)?;
        if forbidden(path) || names.contains(&format!("{prefix}{path}")) {
            return Err(invalid());
        }
    }
    for (path, d) in &j.destination_dirs {
        relative(path)?;
        if path != "versions" && !shared(path) || d.before.is_some() && d.created.is_some() {
            return Err(invalid());
        }
        match j.targets.get(path) {
            Some(Target::Directory(k)) if d.before.as_ref() == Some(k) => {}
            Some(Target::Absent) if d.before.is_none() => {}
            _ => return Err(invalid()),
        }
    }
    for (path, target) in &j.targets {
        relative(path)?;
        if path != "versions" && path != &format!("versions/{}", j.name) && !shared(path) {
            return Err(invalid());
        }
        if let Target::File(s) = target {
            valid_snapshot(s)?;
        }
    }
    if !j.instance_dirs.contains_key("config")
        || !names.contains(&format!("{prefix}{}.json", j.name))
        || !names.contains(&format!("{prefix}{}.jar", j.name))
    {
        return Err(invalid());
    }
    if matches!(
        j.state,
        State::Prepared | State::Committed | State::Finished
    ) && (j.files_key.is_none()
        || j.instance_key.is_none()
        || j.instance_dirs.values().any(Option::is_none))
    {
        return Err(invalid());
    }
    Ok(())
}
fn read_journal(operation: &Dir, name: &str, filename: &str) -> Result<Journal> {
    #[cfg(not(target_os = "linux"))]
    let bytes = operation.read_journal_bytes(filename)?;
    #[cfg(target_os = "linux")]
    let bytes = {
        let mut file = operation.regular(filename)?;
        if file.metadata().map_err(error)?.len() > MAX_JOURNAL {
            return Err("导入恢复记录过大".into());
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_JOURNAL + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() as u64 > MAX_JOURNAL {
            return Err("导入恢复记录过大".into());
        }
        bytes
    };
    let mut j = journal::decode(&bytes)?;
    validate_journal(&j, name)?;
    if operation.key() != Ok(j.operation_key.clone()) {
        return Err("导入记录目录已经变化".into());
    }
    hydrate_ownership(operation, &mut j)?;
    validate_journal(&j, name)?;
    Ok(j)
}
fn validate_next(operation: &Dir, j: &Journal) -> Result<bool> {
    if operation.stat("journal.next")?.is_some() {
        let next = read_journal(operation, &j.operation_id, "journal.next")?;
        if next.schema != j.schema
            || next
                .build
                .as_ref()
                .zip(j.build.as_ref())
                .is_some_and(|(a, b)| a.key.is_some() && b.key.is_some() && a.key != b.key)
            || next.root != j.root
            || next.root_key != j.root_key
            || next.operation_key != j.operation_key
            || next.name != j.name
        {
            return Err("未完成的导入记录与当前操作不一致".into());
        }
        return Ok(true);
    }
    Ok(false)
}
fn cleanup_next(operation: &Dir, j: &Journal) -> Result<()> {
    if operation.stat("journal.next")?.is_some() {
        let expected = hash_file(operation.regular("journal.next")?, MAX_JOURNAL, None)?;
        if validate_next(operation, j)? {
            operation.unlink_snapshot("journal.next", &expected)?;
        }
    }
    Ok(())
}
fn write_journal(operation: &Dir, j: &Journal) -> Result<()> {
    #[cfg(test)]
    if j.state == State::Committed && FAIL_COMMITTED_JOURNAL_WRITE.with(|fail| fail.replace(false))
    {
        return Err("fixture durable commit failure".into());
    }
    validate_journal(j, &j.operation_id)?;
    validate_binding(operation, j)?;
    let bytes = journal::encode(j)?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err("导入恢复记录超过大小限制".into());
    }
    #[cfg(not(target_os = "linux"))]
    {
        operation.write_journal_bytes(&bytes)?;
        #[cfg(test)]
        FULL_JOURNAL_WRITES.with(|n| n.set(n.get() + 1));
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        cleanup_next(operation, j)?;
        let initial = operation.stat("journal.json")?.is_none();
        let leaf = if initial {
            "journal.json"
        } else {
            "journal.next"
        };
        let mut file = operation.stage_for(leaf, MAX_JOURNAL)?;
        file.write_all(&bytes).map_err(error)?;
        file.sync_all().map_err(error)?;
        if initial {
            link_anonymous(&mut file, operation, "journal.json")?;
            #[cfg(test)]
            FULL_JOURNAL_WRITES.with(|n| n.set(n.get() + 1));
            return Ok(());
        }
        link_anonymous(&mut file, operation, "journal.next")?;
        // This replacement concerns only the validated launcher-owned journal.
        read_journal(operation, &j.operation_id, "journal.json")?;
        filesystem::replace_journal(operation)?;
        #[cfg(test)]
        FULL_JOURNAL_WRITES.with(|n| n.set(n.get() + 1));
        Ok(())
    }
}

fn bound_root(j: &Journal) -> Result<Dir> {
    let root = Dir::open(&j.root)?;
    if root.key() != Ok(j.root_key.clone()) {
        return Err(changed());
    }
    Ok(root)
}
fn validate_binding(operation: &Dir, j: &Journal) -> Result<()> {
    let root = bound_root(j)?;
    let store = storage(&root, false)?.ok_or("导入记录目录已经变化")?;
    if operation.key()? != j.operation_key
        || store.child(&j.operation_id)?.key()? != j.operation_key
    {
        return Err("导入记录目录已经变化".into());
    }
    Ok(())
}
fn owned_dir(operation: &Dir, name: &str, key: &Option<Key>) -> Result<Option<Dir>> {
    let Some(dir) = operation.optional(name)? else {
        return Ok(None);
    };
    match key {
        Some(key) if dir.key() == Ok(key.clone()) => Ok(Some(dir)),
        None if dir.names()?.is_empty() => Ok(Some(dir)),
        _ => Err("导入暂存目录已变化，已保留文件".into()),
    }
}
fn slot(index: usize) -> String {
    format!("f-{index}")
}
fn setup(root: &Dir, store: &Dir, plan: &ImportPlan) -> Result<(Dir, Journal)> {
    let operation_id = nonce();
    let operation = store.mkdir(&operation_id)?.with_journal()?;
    let file_targets: BTreeSet<_> = plan.files.iter().map(|f| f.target.as_str()).collect();
    let destination_dirs = plan
        .targets
        .iter()
        .filter(|(path, _)| {
            *path != &format!("versions/{}", plan.name) && !file_targets.contains(path.as_str())
        })
        .map(|(path, t)| {
            Ok((
                path.clone(),
                DestinationDirectory {
                    before: match t {
                        Target::Directory(k) => Some(k.clone()),
                        Target::Absent => None,
                        Target::File(_) => return Err("共享资源目录计划无效".to_owned()),
                    },
                    created: None,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut j = Journal {
        schema: 1,
        build: None,
        operation_id,
        root: plan.root.clone(),
        root_key: root.key()?,
        operation_key: operation.key()?,
        name: plan.name.clone(),
        state: State::Staging,
        files_key: None,
        instance_key: None,
        files: plan
            .files
            .iter()
            .map(|f| JournalFile {
                target: f.target.clone(),
                size: f.size,
                hash: f.hash.clone(),
                reuse: f.reuse,
                staged: None,
            })
            .collect(),
        targets: plan.targets.clone(),
        destination_dirs,
        instance_dirs: plan
            .instance_dirs
            .iter()
            .map(|p| (p.clone(), None))
            .collect(),
    };
    // Initial journal has no pathname until complete and durable. A crash in
    // mkdir -> journal publication leaves only an empty removable operation.
    if let Err(e) = write_journal(&operation, &j) {
        return match store.unlink_key(&j.operation_id, &j.operation_key) {
            Ok(()) => Err(e),
            Err(c) => Err(format!("取消清理失败：{c}；原错误：{e}")),
        };
    }
    let result = (|| {
        j.files_key = Some(operation.mkdir("files")?.key()?);
        write_journal(&operation, &j)?;
        j.instance_key = Some(operation.mkdir("instance")?.key()?);
        write_journal(&operation, &j)?;
        Ok(())
    })();
    if let Err(e) = result {
        return match recover_one(&operation, &mut j) {
            Ok(()) => Err(e),
            Err(c) => Err(format!("取消清理失败：{c}；原错误：{e}")),
        };
    }
    Ok((operation, j))
}
fn stage_file(
    operation: &Dir,
    j: &mut Journal,
    index: usize,
    source: &FileSource,
    zip: &mut ZipArchive<File>,
    cancel: &AtomicBool,
    bytes_done: &mut u64,
    notify: &impl Fn(u64),
) -> Result<()> {
    let stage = owned_dir(operation, "files", &j.files_key)?.ok_or("导入文件暂存目录缺失")?;
    let expected = &j.files[index];
    let mut file = stage.stage_for(&slot(index), expected.size)?;
    let mut written = 0u64;
    let mut hash = Sha256::new();
    let mut copy = |input: &mut dyn Read| -> Result<()> {
        let mut buffer = [0u8; 128 * 1024];
        loop {
            check(cancel)?;
            let n = input
                .read(&mut buffer)
                .map_err(|e| format!("导入解压或校验失败：{e}"))?;
            if n == 0 {
                break;
            }
            written = written
                .checked_add(n as u64)
                .filter(|n| *n <= expected.size)
                .ok_or("ZIP 内容大小超过计划声明")?;
            hash.update(&buffer[..n]);
            file.write_all(&buffer[..n]).map_err(error)?;
            *bytes_done = bytes_done.checked_add(n as u64).ok_or("导入内容过大")?;
            notify(*bytes_done);
        }
        Ok(())
    };
    match source {
        FileSource::Generated(bytes) => copy(&mut bytes.as_slice())?,
        FileSource::Archive(expected) => {
            let mut entry = zip.by_index(expected.index).map_err(error)?;
            if entry.name() != expected.path
                || entry.size() != expected.size
                || entry.is_dir()
                || entry.is_symlink()
            {
                return Err(changed());
            }
            copy(&mut entry)?;
        }
    }
    if written != expected.size || format!("{:x}", hash.finalize()) != expected.hash {
        return Err(changed());
    }
    file.sync_all().map_err(error)?;
    let snapshot = hash_file(file.try_clone().map_err(error)?, MAX_FILE, Some(cancel))?;
    register_ownership(
        operation,
        j,
        OwnershipRecord::File {
            index,
            snapshot: snapshot.clone(),
        },
    )?;
    j.files[index].staged = Some(snapshot);
    check(cancel)?;
    link_anonymous(&mut file, &stage, &slot(index))
}
fn ordered_paths<'a>(paths: impl Iterator<Item = &'a String>, reverse: bool) -> Vec<String> {
    let mut result: Vec<_> = paths.cloned().collect();
    result.sort_by(|a, b| {
        a.matches('/')
            .count()
            .cmp(&b.matches('/').count())
            .then(a.cmp(b))
    });
    if reverse {
        result.reverse();
    }
    result
}
fn assemble_instance(operation: &Dir, j: &mut Journal, cancel: &AtomicBool) -> Result<()> {
    let instance =
        owned_dir(operation, "instance", &j.instance_key)?.ok_or("导入实例暂存目录缺失")?;
    for path in ordered_paths(j.instance_dirs.keys(), false) {
        check(cancel)?;
        let (parent, name) = instance.parent(&path)?;
        let created = parent.mkdir(&name)?;
        let key = created.key()?;
        register_ownership(
            operation,
            j,
            OwnershipRecord::InstanceDirectory {
                path: path.clone(),
                key: key.clone(),
            },
        )?;
        j.instance_dirs.insert(path, Some(key));
    }
    let files = owned_dir(operation, "files", &j.files_key)?.ok_or("导入文件暂存目录缺失")?;
    let prefix = format!("versions/{}/", j.name);
    for (i, f) in j.files.iter().enumerate() {
        if let Some(path) = f.target.strip_prefix(&prefix) {
            check(cancel)?;
            let (parent, name) = instance.parent(path)?;
            link_file(&files, &slot(i), &parent, &name)?;
        }
    }
    verify_instance(&instance, j)
}
fn verify_instance(instance: &Dir, j: &Journal) -> Result<()> {
    if instance.key()? != j.instance_key.clone().ok_or("导入实例身份未登记")? {
        return Err("新实例暂存目录已变化".into());
    }
    let prefix = format!("versions/{}/", j.name);
    let files: BTreeMap<_, _> = j
        .files
        .iter()
        .filter_map(|f| f.target.strip_prefix(&prefix).map(|p| (p.to_owned(), f)))
        .collect();
    fn walk(
        dir: &Dir,
        path: &str,
        j: &Journal,
        files: &BTreeMap<String, &JournalFile>,
        nodes: &mut usize,
    ) -> Result<()> {
        for name in dir.names()? {
            *nodes += 1;
            if *nodes > MAX_FILES * 2 {
                return Err("导入暂存节点数量超过限制".into());
            }
            let path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            let stamp = dir.stat(&name)?.ok_or_else(changed)?;
            if stamp.directory() {
                let child = dir.child(&name)?;
                match j.instance_dirs.get(&path) {
                    Some(Some(key)) if child.key() == Ok(key.clone()) => {
                        walk(&child, &path, j, files, nodes)?
                    }
                    Some(None) if child.names()?.is_empty() => {}
                    _ => return Err("新实例暂存目录包含外部目录或目录已变化".into()),
                }
            } else {
                let f = files.get(&path).ok_or("新实例暂存目录包含未登记文件")?;
                let expected = f.staged.as_ref().ok_or("导入暂存文件身份未登记")?;
                if !expected.owned_matches(&hash_file(dir.regular(&name)?, MAX_FILE, None)?) {
                    return Err("新实例内容已变化，保留暂存记录".into());
                }
            }
        }
        Ok(())
    }
    walk(instance, "", j, &files, &mut 0)
}
fn complete_instance(instance: &Dir, j: &Journal) -> Result<()> {
    verify_instance(instance, j)?;
    for (path, key) in &j.instance_dirs {
        if instance.at(path)?.key()? != key.clone().ok_or("导入目录未登记")? {
            return Err(changed());
        }
    }
    let prefix = format!("versions/{}/", j.name);
    for f in &j.files {
        if let Some(path) = f.target.strip_prefix(&prefix) {
            if !f
                .staged
                .as_ref()
                .ok_or("导入文件未登记")?
                .owned_matches(&hash_file(instance.file(path)?, MAX_FILE, None)?)
            {
                return Err(changed());
            }
        }
    }
    Ok(())
}
fn remove_instance_tree(instance: &Dir, j: &Journal) -> Result<()> {
    // Validate the entire tree before the first unlink. A newly added user file
    // or modified byte prevents destructive cleanup, even in a cancelled task.
    verify_instance(instance, j)?;
    let prefix = format!("versions/{}/", j.name);
    for f in &j.files {
        if let Some(path) = f.target.strip_prefix(&prefix) {
            let Some((parent, name)) = instance.optional_parent(path)? else {
                continue;
            };
            if parent.stat(&name)?.is_some() {
                let snap = hash_file(parent.regular(&name)?, MAX_FILE, None)?;
                if !f
                    .staged
                    .as_ref()
                    .ok_or("导入文件未登记")?
                    .owned_matches(&snap)
                {
                    return Err("导入内容已变化，拒绝清理".into());
                }
                parent.unlink_snapshot(&name, &snap)?;
            }
        }
    }
    for path in ordered_paths(j.instance_dirs.keys(), true) {
        let Some((parent, name)) = instance.optional_parent(&path)? else {
            continue;
        };
        if let Some(child) = parent.optional(&name)? {
            if j.instance_dirs[&path]
                .as_ref()
                .is_some_and(|k| child.key() != Ok(k.clone()))
                || !child.names()?.is_empty()
            {
                return Err("导入内容目录已变化，拒绝清理".into());
            }
            parent.unlink_key(&name, &child.key()?)?;
        }
    }
    if !instance.names()?.is_empty() {
        return Err("导入实例目录包含未登记内容".into());
    }
    Ok(())
}
fn verify_shared(root: &Dir, j: &Journal, committed: bool) -> Result<()> {
    for f in &j.files {
        if f.target.starts_with(&format!("versions/{}/", j.name)) {
            continue;
        }
        let current = capture_target(root, &f.target)?;
        if f.reuse {
            if j.targets.get(&f.target) != Some(&current) {
                return Err("复用的共享资源已经变化，已保留导入记录".into());
            }
        } else {
            match current {
                Target::Absent if !committed => {}
                Target::File(s)
                    if f.staged
                        .as_ref()
                        .is_some_and(|expected| expected.owned_matches(&s)) => {}
                _ => return Err("已发布共享资源已经变化，拒绝覆盖或清理".into()),
            }
        }
    }
    Ok(())
}
fn ensure_destination_dirs(root: &Dir, operation: &Dir, j: &mut Journal) -> Result<()> {
    for path in ordered_paths(j.destination_dirs.keys(), false) {
        validate_binding(operation, j)?;
        let (parent, name) = destination_parent(root, &path, j)?;
        let record = &j.destination_dirs[&path];
        if let Some(key) = &record.before {
            if parent.child(&name)?.key() != Ok(key.clone()) {
                return Err(changed());
            }
        } else {
            if parent.stat(&name)?.is_some() {
                return Err(changed());
            }
            let child = parent.mkdir(&name)?;
            let key = child.key()?;
            register_ownership(
                operation,
                j,
                OwnershipRecord::DestinationDirectory {
                    path: path.clone(),
                    key: key.clone(),
                },
            )?;
            j.destination_dirs
                .get_mut(&path)
                .ok_or("目录计划缺失")?
                .created = Some(key);
        }
    }
    Ok(())
}
fn destination_parent(root: &Dir, path: &str, j: &Journal) -> Result<(Dir, String)> {
    let parts = relative(path)?;
    let mut dir = root.duplicate()?;
    for depth in 1..parts.len() {
        let prefix = parts[..depth].join("/");
        let record = j
            .destination_dirs
            .get(&prefix)
            .ok_or("共享资源父目录身份未登记")?;
        let expected = record
            .before
            .as_ref()
            .or(record.created.as_ref())
            .ok_or("共享资源父目录未创建")?;
        dir = dir.child(parts[depth - 1])?;
        if dir.key()? != *expected {
            return Err(changed());
        }
    }
    Ok((dir, parts[parts.len() - 1].into()))
}
fn verify_destination_dirs(root: &Dir, j: &Journal, committed: bool) -> Result<()> {
    for (path, d) in &j.destination_dirs {
        match (
            capture_target(root, path)?,
            d.before.as_ref().or(d.created.as_ref()),
        ) {
            (Target::Directory(actual), Some(expected)) if actual == *expected => {}
            (Target::Absent, _) if !committed && d.before.is_none() => {}
            // An interrupted mkdir can be unregistered, but only an empty
            // directory is eligible for rollback in that state.
            (Target::Directory(_), None) if !committed && root.at(path)?.names()?.is_empty() => {}
            _ => return Err("目标共享资源目录已经变化，已保留导入记录".into()),
        }
    }
    Ok(())
}
fn publish(
    root: &Dir,
    operation: &Dir,
    j: &mut Journal,
    source: &Path,
    source_expected: &Snapshot,
) -> Result<()> {
    publication::publish_prepared(root, operation, j, || {
        if source_snapshot(source, None)? != *source_expected {
            return Err(changed());
        }
        Ok(())
    })
}
fn cleanup_stage(operation: &Dir, j: &Journal) -> Result<()> {
    build::cleanup_build(operation, j)?;
    if let Some(instance) = owned_dir(operation, "instance", &j.instance_key)? {
        if j.instance_key.is_some() {
            remove_instance_tree(&instance, j)?;
        }
        operation.unlink_key("instance", &instance.key()?)?;
    }
    if let Some(files) = owned_dir(operation, "files", &j.files_key)? {
        // Unknown entries are preserved. Journal-before-link makes every
        // legitimate visible file identifiable, including a crash mid-copy.
        let names = files.names()?;
        let mut checked = Vec::with_capacity(names.len());
        for name in names {
            let index = name
                .strip_prefix("f-")
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|i| slot(*i) == name)
                .ok_or("导入暂存目录包含未登记文件")?;
            let expected = j
                .files
                .get(index)
                .and_then(|f| f.staged.as_ref())
                .ok_or("导入暂存文件缺少持久身份")?;
            if !expected.owned_matches(&hash_file(files.regular(&name)?, MAX_FILE, None)?) {
                return Err("导入暂存内容已变化，拒绝清理".into());
            }
            checked.push((name, expected));
        }
        // Use the checked list and recheck each inode/byte sequence. A new
        // foreign filename added between scans must survive and make rmdir
        // fail, instead of being deleted by a second unvalidated listing.
        for (name, expected) in checked {
            if !expected.owned_matches(&hash_file(files.regular(&name)?, MAX_FILE, None)?) {
                return Err("导入暂存内容在清理前已经变化".into());
            }
            files.unlink_snapshot(&name, expected)?;
        }
        operation.unlink_key("files", &files.key()?)?;
    }
    cleanup_next(operation, j)?;
    for name in operation.names()? {
        if name.starts_with("owned-") {
            let expected = hash_file(operation.regular(&name)?, MAX_JOURNAL, None)?;
            read_ownership(operation, &name, j)?;
            operation.unlink_snapshot(&name, &expected)?;
        }
    }
    let names = operation.names()?;
    if names.iter().any(|n| n != "journal.json") {
        return Err("导入记录目录包含未知内容，已保留".into());
    }
    Ok(())
}
fn rollback(root: &Dir, operation: &Dir, j: &Journal) -> Result<()> {
    validate_binding(operation, j)?;
    verify_destination_dirs(root, j, false)?;
    verify_shared(root, j, false)?;
    if let Some(versions) = root.optional("versions")? {
        if let Some(instance) = versions.optional(&j.name)? {
            if instance.key()? != j.instance_key.clone().ok_or("发布实例身份未登记")? {
                return Err("目标实例已经被其他内容占用，拒绝清理".into());
            }
            remove_instance_tree(&instance, j)?;
            versions.unlink_key(&j.name, &instance.key()?)?;
        }
    }
    for f in &j.files {
        if f.reuse || f.target.starts_with(&format!("versions/{}/", j.name)) {
            continue;
        }
        if let Target::File(current) = capture_target(root, &f.target)? {
            if !f
                .staged
                .as_ref()
                .ok_or("共享暂存身份缺失")?
                .owned_matches(&current)
            {
                return Err("目标共享资源已变化，拒绝清理".into());
            }
            let (parent, name) = root.parent(&f.target)?;
            parent.unlink_snapshot(&name, &current)?;
        }
    }
    for path in ordered_paths(j.destination_dirs.keys(), true) {
        let record = &j.destination_dirs[&path];
        if record.before.is_some() {
            continue;
        }
        let Some((parent, name)) = root.optional_parent(&path)? else {
            continue;
        };
        if let Some(child) = parent.optional(&name)? {
            // A crash between mkdir and key registration can leave an empty
            // directory. Empty removal loses no bytes; nonempty is a conflict.
            if record
                .created
                .as_ref()
                .is_some_and(|k| child.key() != Ok(k.clone()))
                || !child.names()?.is_empty()
            {
                return Err("导入创建的共享目录已变化，拒绝清理".into());
            }
            parent.unlink_key(&name, &child.key()?)?;
        }
    }
    cleanup_stage(operation, j)
}
fn recover_one(operation: &Dir, j: &mut Journal) -> Result<()> {
    let root = bound_root(j)?;
    if operation.key() != Ok(j.operation_key.clone()) {
        return Err("导入记录目录已变化".into());
    }
    match j.state {
        State::Committed => {
            verify_destination_dirs(&root, j, true)?;
            verify_shared(&root, j, true)?;
            let versions = root.child("versions")?;
            complete_instance(&versions.child(&j.name)?, j)?;
            cleanup_stage(operation, j)?;
            j.state = State::Finished;
        }
        State::Building => {
            cleanup_stage(operation, j)?;
            j.state = State::RolledBack;
        }
        State::Staging | State::Prepared => {
            rollback(&root, operation, j)?;
            j.state = State::RolledBack;
        }
        State::Finished | State::RolledBack => {
            cleanup_next(operation, j)?;
            if j.schema == 2 {
                build::verify_terminal_operation(operation, j)?;
            }
            return operation.check_source_cleanup();
        }
    }
    operation.check_source_cleanup()?;
    write_journal(operation, j)
}

/// Read-only guard. The common root admission must guard every writer and
/// launch; pending imports remain recoverable even without selected metadata.
pub fn ensure_ready(root: &Path) -> Result<()> {
    ensure_ready_except_build(root, None)
}

/// Bootstrap may suppress only its captured running pack operation. All other
/// pending operations, roots and malformed records keep the recovery guard.
pub(crate) fn ensure_ready_except_build(root: &Path, active_operation: Option<&str>) -> Result<()> {
    let path = root.canonicalize().map_err(error)?;
    let dir = Dir::open(&path)?;
    let Some(store) = storage(&dir, false)? else {
        return Ok(());
    };
    for name in store.names()? {
        if name == ".lock" {
            store.regular(&name)?;
            continue;
        }
        if !operation_ok(&name) {
            return Err("导入记录目录包含未知内容".into());
        }
        let operation = store.child(&name)?.with_journal()?;
        if operation.stat("journal.json")?.is_none() {
            return Err("存在未完成的实例导入，请先恢复再操作或启动".into());
        }
        let j = read_journal(&operation, &name, "journal.json")?;
        if j.root != path || j.root_key != dir.key()? {
            return Err("导入记录与当前游戏目录不匹配".into());
        }
        if j.schema == 2 && matches!(j.state, State::Finished | State::RolledBack) {
            build::verify_terminal_operation(&operation, &j)?;
        }
        if !matches!(j.state, State::Finished | State::RolledBack) {
            if j.schema == 2 && active_operation == Some(name.as_str()) {
                validate_binding(&operation, &j)?;
                if let Some(build) = &j.build {
                    let private = owned_dir(&operation, "build", &build.key)?;
                    if matches!(j.state, State::Building | State::Staging | State::Prepared)
                        && private.is_none()
                    {
                        return Err("运行中的私有构建目录缺失，已保留恢复记录".into());
                    }
                }
                continue;
            }
            return Err("存在未完成的实例导入，请先恢复再操作或启动".into());
        }
    }
    Ok(())
}

/// Requires the shared writer admission. No archive is needed: the journal
/// contains all ownership and hash information required after process death.
pub fn recover_pending(root: &Path) -> Result<Value> {
    let path = root.canonicalize().map_err(error)?;
    let dir = Dir::open(&path)?;
    if storage(&dir, false)?.is_none() {
        return Ok(json!({"recovered":0}));
    }
    filesystem::require_mutation_support()?;
    let store = storage(&dir, false)?.ok_or("导入记录目录缺失")?;
    let _lock = lock(&store)?;
    let mut recovered = 0usize;
    for name in store.names()? {
        if name == ".lock" {
            continue;
        }
        if !operation_ok(&name) {
            return Err("导入记录目录包含未知内容".into());
        }
        let operation = store.child(&name)?.with_journal()?;
        if operation.stat("journal.json")?.is_none() {
            if operation.names()?.is_empty() {
                store.unlink_key(&name, &operation.key()?)?;
                recovered += 1;
                continue;
            }
            return Err("未登记的导入目录包含内容，已保留供检查".into());
        }
        let mut j = read_journal(&operation, &name, "journal.json")?;
        if j.root != path || j.root_key != dir.key()? {
            return Err("导入记录与当前游戏目录不匹配".into());
        }
        if !matches!(j.state, State::Finished | State::RolledBack) {
            recover_one(&operation, &mut j).map_err(|e| format!("取消清理失败：{e}"))?;
            recovered += 1;
        } else {
            cleanup_next(&operation, &j)?;
        }
    }
    ensure_ready(&path)?;
    dir.reconcile_sources()?;
    Ok(json!({"recovered":recovered,"message":"实例导入恢复完成"}))
}
fn progress(
    stage: &str,
    message: &str,
    completed: u64,
    total: u64,
    bytes_done: u64,
    bytes_total: u64,
) -> Progress {
    let ids = [
        ("import-check", "检查导入包与目标目录"),
        ("import-extract", "解压独立实例与资源"),
        ("import-commit", "发布实例文件"),
        ("import-cleanup", "清理导入暂存文件"),
    ];
    let current = ids.iter().position(|(id, _)| *id == stage).unwrap_or(0);
    Progress {
        stage: stage.into(),
        message: message.into(),
        completed,
        total,
        bytes_done,
        bytes_total,
        network_bytes: 0,
        steps: ids
            .iter()
            .enumerate()
            .map(|(i, (id, label))| InstallStep {
                id: (*id).into(),
                label: (*label).into(),
                state: if i < current {
                    "complete"
                } else if i == current {
                    "running"
                } else {
                    "pending"
                }
                .into(),
                progress: if i < current {
                    Some(1.0)
                } else if i == current {
                    Some(if total == 0 {
                        0.0
                    } else {
                        completed as f64 / total as f64
                    })
                } else {
                    None
                },
            })
            .collect(),
    }
}

#[cfg(test)]
pub fn execute(
    plan: ImportPlan,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
) -> Result<Value> {
    execute_checked(plan, cancel, callback, || Ok(()))
}

/// Recheck launcher reference stores after long extraction, while cancellation
/// admission is closed and before publishing files into the captured root.
pub fn execute_checked(
    plan: ImportPlan,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
    commit_check: impl FnOnce() -> Result<()>,
) -> Result<Value> {
    filesystem::require_mutation_support()?;
    check(cancel)?;
    guards(&plan.root)?;
    crate::instance_delete::ensure_name_available(&plan.root, &plan.name)?;
    callback(progress(
        "import-check",
        "正在检查导入包与目标游戏目录",
        0,
        1,
        0,
        plan.bytes,
    ));
    let mut root = Dir::open(&plan.root)?;
    if root.key() != Ok(plan.root_key.clone())
        || source_snapshot(&plan.source, Some(cancel))? != plan.source_snapshot
    {
        return Err(changed());
    }
    validate_targets(&root, &plan.targets)?;
    root = root.with_sources()?;
    let store = storage(&root, true)?.ok_or("导入记录目录缺失")?;
    let _lock = lock(&store)?;
    let (operation, mut j) = setup(&root, &store, &plan)?;
    let result = (|| {
        let mut zip = checked_zip(open_source(&plan.source)?)?;
        let mut bytes_done = 0u64;
        for (index, f) in plan.files.iter().enumerate() {
            check(cancel)?;
            if f.reuse {
                continue;
            }
            stage_file(
                &operation,
                &mut j,
                index,
                &f.source,
                &mut zip,
                cancel,
                &mut bytes_done,
                &|done| {
                    callback(progress(
                        "import-extract",
                        "正在解压并校验导入内容",
                        index as u64,
                        plan.file_count as u64,
                        done,
                        plan.bytes,
                    ))
                },
            )?;
        }
        check(cancel)?;
        assemble_instance(&operation, &mut j, cancel)?;
        if source_snapshot(&plan.source, Some(cancel))? != plan.source_snapshot {
            return Err(changed());
        }
        validate_targets(&root, &plan.targets)?;
        if Dir::open(&plan.root)?.key() != Ok(plan.root_key.clone()) {
            return Err(changed());
        }
        j.state = State::Prepared;
        write_journal(&operation, &j)?;
        check(cancel)?;
        // The callback closes task cancellation admission. Recheck any cancel
        // accepted immediately before that gate; publication then runs to a
        // durable commit or owned rollback without later cancellation checks.
        callback(progress(
            "import-commit",
            "正在发布新实例与共享资源",
            0,
            1,
            bytes_done,
            plan.bytes,
        ));
        check(cancel)?;
        commit_check()?;
        publish(
            &root,
            &operation,
            &mut j,
            &plan.source,
            &plan.source_snapshot,
        )?;
        callback(progress(
            "import-cleanup",
            "正在清理导入暂存文件",
            0,
            1,
            bytes_done,
            plan.bytes,
        ));
        recover_one(&operation, &mut j).map_err(|e| format!("取消清理失败：{e}"))?;
        Ok(
            json!({"id":plan.name,"files":plan.file_count,"bytes":plan.bytes,"reused_files":plan.reused_files,"message":"实例导入完成"}),
        )
    })();
    match result {
        Ok(value) => Ok(value),
        Err(original) => {
            // A failed durable commit must roll back. A committed import keeps
            // its successful output; recovery retries only staging cleanup.
            match recover_one(&operation, &mut j) {
                Ok(()) => Err(original),
                Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{original}")),
            }
        }
    }
}

#[cfg(test)]
#[path = "instance_import/tests.rs"]
mod tests;
