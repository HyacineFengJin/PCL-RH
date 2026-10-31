//! Existing-instance component reset. Callers hold the shared writer admission.
//! Installation and processors run in an owned root; only the two same-id core
//! files are exchanged. All filesystem mutations are relative to no-follow FDs.
use pcl_install::{
    ComponentSelection, InstallRequest, Installer, Progress, ResolvedInstallRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const STORE: &str = "instance-resets";
const MAX_JSON: u64 = 8 * 1024 * 1024;
const MAX_JAR: u64 = 2 * 1024 * 1024 * 1024;
const MAX_INSTANCES: usize = 4096;
const MAX_FILES: usize = 100_000;
const MAX_DEPTH: usize = 64;
const MAX_JOURNAL: u64 = filesystem::JOURNAL_LIMIT;
#[cfg(target_os = "linux")]
#[path = "instance_reset/filesystem_linux.rs"]
mod filesystem;
#[cfg(not(target_os = "linux"))]
#[path = "instance_reset/filesystem_native.rs"]
mod filesystem;
#[cfg(all(test, target_os = "linux"))]
use filesystem::merge_cache;
use filesystem::{
    copy_core, lock, remove_owned_tree, rename_new, snapshot, Dir, DirKey, Snapshot,
    REVISION_PREFIX, SCHEMA,
};
#[cfg(test)]
use std::{fs, io::Write};
#[cfg(test)]
thread_local! {static COMMIT_JOURNAL_FAULT:std::cell::Cell<u8>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
thread_local! {static INITIAL_JOURNAL_FAULT:std::cell::Cell<u8>=const{std::cell::Cell::new(0)};}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize)]
pub struct ResetPlan {
    pub revision: String,
    pub minecraft: String,
    pub id: String,
    pub components: Vec<ComponentSelection>,
    pub current_components: Vec<ComponentSelection>,
    pub current_summary: String,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    root_key: DirKey,
    #[serde(skip)]
    versions_key: DirKey,
    #[serde(skip)]
    instance_key: DirKey,
    #[serde(skip)]
    originals: Vec<Snapshot>,
}

impl ResetPlan {
    pub(crate) fn install_request(&self) -> InstallRequest {
        InstallRequest {
            minecraft: self.minecraft.clone(),
            name: self.id.clone(),
            components: self.components.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    Staging,
    Prepared,
    Committed,
    Finished,
    RolledBack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreFile {
    name: String,
    original: Snapshot,
    replacement: Option<Snapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    operation_id: String,
    root: PathBuf,
    root_key: DirKey,
    versions_key: DirKey,
    instance_key: DirKey,
    operation_key: DirKey,
    instance_id: String,
    stage_key: Option<DirKey>,
    incoming_key: Option<DirKey>,
    backup_key: Option<DirKey>,
    state: State,
    files: Vec<CoreFile>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn name_ok(name: &str) -> Result<()> {
    pcl_core::identifier(name)?;
    if name.len() > 255 || name.chars().any(char::is_control) {
        return Err("文件名称无效".into());
    }
    Ok(())
}
fn operation_id() -> String {
    format!(
        "r-{:x}-{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}
fn operation_ok(name: &str) -> bool {
    name.starts_with("r-")
        && name.len() <= 80
        && name
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_hexdigit() || b == b'r')
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err("组件重置已取消".into())
    } else {
        Ok(())
    }
}
fn read_json(dir: &Dir, name: &str) -> Result<(Value, Snapshot)> {
    let mut file = dir.regular(name)?;
    if file.metadata().map_err(err)?.len() > MAX_JSON {
        return Err("版本 JSON 超过安全上限".into());
    }
    let snap = snapshot(dir.regular(name)?, MAX_JSON, None)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_JSON + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    let data: Value =
        serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))
            .map_err(err)?;
    if !data.is_object() || snap != snapshot(dir.regular(name)?, MAX_JSON, None)? {
        return Err("版本 JSON 无效或读取期间已变化".into());
    }
    Ok((data, snap))
}
fn game_arg(data: &Value, flag: &str) -> Option<String> {
    data["arguments"]["game"]
        .as_array()?
        .windows(2)
        .find(|pair| pair[0].as_str() == Some(flag))?
        .get(1)?
        .as_str()
        .map(str::to_owned)
}
fn coordinate(data: &Value, prefix: &str) -> Option<String> {
    data["libraries"].as_array()?.iter().find_map(|library| {
        library["name"]
            .as_str()
            .and_then(|name| name.strip_prefix(prefix))
            .and_then(|tail| tail.strip_prefix(':'))
            .and_then(|tail| tail.split(':').next())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    })
}
fn identity(data: &Value, id: &str) -> Result<(String, Vec<ComponentSelection>, String)> {
    if data["id"].as_str() != Some(id)
        || data.get("inheritsFrom").is_some()
        || data.get("jar").is_some_and(|jar| jar.as_str() != Some(id))
    {
        return Err("暂不支持继承版本、外部基础 JAR 或版本标识不一致的实例重置".into());
    }
    if ["patches", "patch", "mmc-pack", "minecraftArguments"]
        .iter()
        .any(|key| data.get(*key).is_some())
    {
        return Err("暂不支持旧版或自定义启动布局的实例重置".into());
    }
    let class = data["mainClass"]
        .as_str()
        .ok_or("版本缺少启动主类，无法确定重置范围")?;
    let libraries = data["libraries"].as_array().ok_or("版本依赖库信息无效")?;
    if libraries.iter().any(|library| {
        library["name"].as_str().is_some_and(|name| {
            [
                "optifine:",
                "com.mumfrey:liteloader:",
                "org.quiltmc:quilt-loader:",
                "net.labymod:",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        })
    }) {
        return Err("暂不支持此实例的加载器或混合组件，请保留原实例".into());
    }
    let fabric = coordinate(data, "net.fabricmc:fabric-loader");
    let neo = coordinate(data, "net.neoforged:neoforge")
        .or_else(|| coordinate(data, "net.neoforged:forge"))
        .or_else(|| game_arg(data, "--fml.neoForgeVersion"));
    let forge = game_arg(data, "--fml.forgeVersion")
        .or_else(|| coordinate(data, "net.minecraftforge:forge"))
        .or_else(|| coordinate(data, "net.minecraftforge:fmlloader"))
        .map(|v| v.split_once('-').map(|(_, v)| v.to_owned()).unwrap_or(v));
    if fabric.is_some() && (neo.is_some() || forge.is_some()) {
        return Err("实例包含混合加载器，无法安全重置".into());
    }
    let component = if let Some(version) = neo {
        Some(ComponentSelection {
            provider: "neoforge".into(),
            version,
        })
    } else if let Some(version) = forge {
        Some(ComponentSelection {
            provider: "forge".into(),
            version,
        })
    } else {
        fabric.map(|version| ComponentSelection {
            provider: "fabric".into(),
            version,
        })
    };
    let compatible = match component.as_ref().map(|c| c.provider.as_str()) {
        None => class == "net.minecraft.client.main.Main",
        Some("fabric") => matches!(
            class,
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
                | "net.fabricmc.loader.launch.knot.KnotClient"
        ),
        Some("forge" | "neoforge") => matches!(
            class,
            "cpw.mods.modlauncher.Launcher"
                | "cpw.mods.bootstraplauncher.BootstrapLauncher"
                | "net.minecraftforge.bootstrap.ForgeBootstrap"
        ),
        _ => false,
    };
    if !compatible {
        return Err("暂不支持此实例的旧版或自定义启动主类重置".into());
    }
    let mc_arg = game_arg(data, "--fml.mcVersion");
    let client = data["clientVersion"].as_str().map(str::to_owned);
    if mc_arg
        .as_ref()
        .zip(client.as_ref())
        .is_some_and(|(a, b)| a != b)
    {
        return Err("实例的 Minecraft 版本信息存在冲突".into());
    }
    let minecraft = mc_arg
        .or(client)
        .or_else(|| component.is_none().then(|| id.to_owned()))
        .ok_or("实例缺少明确的 Minecraft 原版版本号，无法安全重置")?;
    pcl_core::identifier(&minecraft)?;
    let components: Vec<_> = component.into_iter().collect();
    let summary = components
        .first()
        .map(|c| {
            format!(
                "{} {}",
                match c.provider.as_str() {
                    "fabric" => "Fabric",
                    "forge" => "Forge",
                    _ => "NeoForge",
                },
                c.version
            )
        })
        .unwrap_or_else(|| "原版".into());
    Ok((minecraft, components, summary))
}

/// Read-only, root-bound plan. The revision covers both core contents and all
/// other version JSONs so a newly introduced dependent invalidates submission.
pub fn prepare(root: &Path, id: &str, components: Vec<ComponentSelection>) -> Result<ResetPlan> {
    prepare_bound(root, id, components, None)
}
fn prepare_bound(
    root: &Path,
    id: &str,
    mut components: Vec<ComponentSelection>,
    active: Option<&str>,
) -> Result<ResetPlan> {
    name_ok(id)?;
    ensure_ready_except(root, active)?;
    for component in &mut components {
        component.provider = component.provider.to_ascii_lowercase();
    }
    let path = root.canonicalize().map_err(err)?;
    let root_dir = Dir::open(&path)?;
    let versions = root_dir.child("versions")?;
    let instance = versions.child(id)?;
    let (data, metadata) = read_json(&instance, &format!("{id}.json"))?;
    let jar = snapshot(instance.regular(&format!("{id}.jar"))?, MAX_JAR, None)?;
    let (minecraft, current_components, current_summary) = identity(&data, id)?;
    InstallRequest {
        minecraft: minecraft.clone(),
        name: id.into(),
        components: components.clone(),
    }
    .validate()?;
    let root_key = root_dir.key()?;
    let versions_key = versions.key()?;
    let instance_key = instance.key()?;
    let mut digest = Sha256::new();
    digest.update(
        serde_json::to_vec(&(
            &path,
            &root_key,
            &versions_key,
            &instance_key,
            &metadata,
            &jar,
            &minecraft,
            &components,
        ))
        .map_err(err)?,
    );
    let names = versions.names()?;
    if names.len() > MAX_INSTANCES {
        return Err("版本数量超过实例重置安全上限".into());
    }
    for name in names {
        let other = versions.child(&name)?;
        let other_key = other.key()?;
        let filename = format!("{name}.json");
        if other.stat(&filename)?.is_none() {
            digest.update(serde_json::to_vec(&(&name, other_key)).map_err(err)?);
            continue;
        }
        let (profile, snap) = read_json(&other, &filename)?;
        if name != id
            && (profile["inheritsFrom"].as_str() == Some(id) || profile["jar"].as_str() == Some(id))
        {
            return Err(format!(
                "实例 {name} 继承或复用此实例的核心文件，暂不能安全重置"
            ));
        }
        digest.update(serde_json::to_vec(&(&name, other_key, snap)).map_err(err)?);
    }
    if root_dir.key()? != Dir::open(&path)?.key()?
        || versions.key()? != root_dir.child("versions")?.key()?
        || instance.key()? != versions.child(id)?.key()?
        || metadata != snapshot(instance.regular(&format!("{id}.json"))?, MAX_JSON, None)?
        || jar != snapshot(instance.regular(&format!("{id}.jar"))?, MAX_JAR, None)?
    {
        return Err("实例目录读取期间发生变化，请刷新后重试".into());
    }
    Ok(ResetPlan {
        revision: format!("{REVISION_PREFIX}{:x}", digest.finalize()),
        minecraft,
        id: id.into(),
        components,
        current_components,
        current_summary,
        root: path,
        root_key,
        versions_key,
        instance_key,
        originals: vec![metadata, jar],
    })
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
fn validate_journal(j: &Journal, name: &str) -> Result<()> {
    let invalid = || "实例重置记录无效，已保留原文件与备份".to_owned();
    if j.schema != SCHEMA
        || j.operation_id != name
        || !operation_ok(name)
        || !j.root.is_absolute()
        || name_ok(&j.instance_id).is_err()
        || j.instance_id.len() > 120
        || j.files.len() != 2
    {
        return Err(invalid());
    }
    for key in [
        &j.root_key,
        &j.versions_key,
        &j.instance_key,
        &j.operation_key,
    ]
    .into_iter()
    .chain(j.stage_key.iter())
    .chain(j.incoming_key.iter())
    .chain(j.backup_key.iter())
    {
        filesystem::valid_key(key)?;
    }
    for (i, file) in j.files.iter().enumerate() {
        if file.name != format!("{}.{}", j.instance_id, if i == 0 { "json" } else { "jar" }) {
            return Err(invalid());
        }
        for snapshot in std::iter::once(&file.original).chain(file.replacement.as_ref()) {
            filesystem::valid_snapshot(snapshot)?;
            if snapshot.size > if i == 0 { MAX_JSON } else { MAX_JAR }
                || snapshot.hash.len() != 64
                || !snapshot.hash.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(invalid());
            }
        }
        if j.state != State::Staging && j.state != State::RolledBack && file.replacement.is_none() {
            return Err(invalid());
        }
    }
    if matches!(
        j.state,
        State::Prepared | State::Committed | State::Finished
    ) && (j.backup_key.is_none() || j.incoming_key.is_none() || j.stage_key.is_none())
    {
        return Err(invalid());
    }
    Ok(())
}
fn read_journal(operation: &Dir, name: &str) -> Result<Journal> {
    filesystem::operation_names(operation)?;
    let bytes = filesystem::journal_bytes(operation)?.ok_or("实例重置记录缺失")?;
    let journal = serde_json::from_slice(&bytes).map_err(|_| "实例重置记录损坏，已保留备份")?;
    validate_journal(&journal, name)?;
    Ok(journal)
}
fn write_journal(operation: &Dir, journal: &Journal) -> Result<()> {
    validate_journal(journal, &journal.operation_id)?;
    if operation.key()? != journal.operation_key {
        return Err("实例重置记录目录已变化".into());
    }
    filesystem::operation_names(operation)?;
    let bytes = serde_json::to_vec(journal).map_err(err)?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err("实例重置记录过大".into());
    }
    #[cfg(test)]
    let fault = if journal.state == State::Committed {
        COMMIT_JOURNAL_FAULT.with(|fault| fault.replace(0))
    } else {
        0
    };
    #[cfg(test)]
    let initial = if journal.state == State::Staging
        && journal.stage_key.is_none()
        && journal.incoming_key.is_none()
        && journal.backup_key.is_none()
    {
        INITIAL_JOURNAL_FAULT.with(|fault| fault.replace(0))
    } else {
        0
    };
    #[cfg(test)]
    if fault == 1 || initial == 1 {
        return Err("injected reset journal failure before publication".into());
    }
    filesystem::publish_journal(operation, &bytes)?;
    #[cfg(test)]
    if fault == 2 || initial == 2 {
        return Err("MayHaveCommitted: injected reset journal failure after publication".into());
    }
    Ok(())
}

fn bound_dirs(root: &Dir, journal: &Journal) -> Result<(Dir, Dir)> {
    if root.key()? != journal.root_key || Dir::open(&journal.root)?.key()? != journal.root_key {
        return Err("绑定的游戏目录已变化，已保留重置记录和备份".into());
    }
    let versions = root.child("versions")?;
    let instance = versions.child(&journal.instance_id)?;
    if versions.key()? != journal.versions_key || instance.key()? != journal.instance_key {
        return Err("绑定的实例目录已变化，已保留重置记录和备份".into());
    }
    Ok((versions, instance))
}
fn owned_child(operation: &Dir, name: &str, expected: &Option<DirKey>) -> Result<Option<Dir>> {
    let child = operation.optional(name)?;
    if let Some(child) = &child {
        if let Some(expected) = expected {
            if child.key()? != *expected {
                return Err(format!("重置暂存目录 {name} 已被替换，已保留文件"));
            }
        } else if !child.names()?.is_empty() {
            return Err(format!("重置暂存目录 {name} 含未知内容，已保留文件"));
        }
    }
    Ok(child)
}
fn cleanup_stage(operation: &Dir, journal: &Journal) -> Result<()> {
    if let Some(stage) = owned_child(operation, "stage", &journal.stage_key)? {
        remove_owned_tree(operation, "stage", &stage.key()?, &mut 0)?;
    }
    Ok(())
}
fn limit(index: usize) -> u64 {
    if index == 0 {
        MAX_JSON
    } else {
        MAX_JAR
    }
}
fn matches_file(dir: &Dir, name: &str, expected: &Snapshot, index: usize) -> Result<bool> {
    if dir.stat(name)?.is_none() {
        return Ok(false);
    }
    Ok(expected.stable_matches(&snapshot(dir.regular(name)?, limit(index), None)?))
}

/// Preflight the complete rollback before moving anything. Conflicting external
/// edits are retained and leave the launch guard active for explicit recovery.
fn rollback(root: &Dir, operation: &Dir, journal: &Journal) -> Result<()> {
    let (_, instance) = bound_dirs(root, journal)?;
    let backup =
        owned_child(operation, "backup", &journal.backup_key)?.ok_or("重置备份目录缺失")?;
    let incoming =
        owned_child(operation, "incoming", &journal.incoming_key)?.ok_or("重置替换目录缺失")?;
    let mut moved = Vec::new();
    for (index, file) in journal.files.iter().enumerate() {
        let replacement = file.replacement.as_ref().ok_or("重置替换文件记录缺失")?;
        let original_here = matches_file(&instance, &file.name, &file.original, index)?;
        let original_backup = matches_file(&backup, &file.name, &file.original, index)?;
        let replacement_here = matches_file(&instance, &file.name, replacement, index)?;
        let replacement_incoming = matches_file(&incoming, &file.name, replacement, index)?;
        if original_here == original_backup
            || replacement_here == replacement_incoming
            || (instance.stat(&file.name)?.is_some() && !original_here && !replacement_here)
            || (backup.stat(&file.name)?.is_some() && !original_backup)
            || (incoming.stat(&file.name)?.is_some() && !replacement_incoming)
        {
            return Err("核心文件或备份被外部修改，无法覆盖恢复；已保留原始备份".into());
        }
        moved.push((original_backup, replacement_here));
    }
    for (file, (original_backup, replacement_here)) in journal.files.iter().zip(moved).rev() {
        bound_dirs(root, journal)?;
        if replacement_here {
            rename_new(&instance, &file.name, &incoming, &file.name)?;
        }
        if original_backup {
            rename_new(&backup, &file.name, &instance, &file.name)?;
        }
    }
    for (index, file) in journal.files.iter().enumerate() {
        if !matches_file(&instance, &file.name, &file.original, index)? {
            return Err("原始核心文件恢复校验失败".into());
        }
    }
    Ok(())
}
fn finish_rollback(operation: &Dir, journal: &mut Journal) -> Result<()> {
    cleanup_stage(operation, journal)?;
    if let Some(incoming) = owned_child(operation, "incoming", &journal.incoming_key)? {
        for name in incoming.names()? {
            let (index, item) = journal
                .files
                .iter()
                .enumerate()
                .find(|(_, file)| file.name == name)
                .ok_or("替换目录含未知文件")?;
            let expected = item.replacement.as_ref().ok_or("替换目录含未登记文件")?;
            if !matches_file(&incoming, &name, expected, index)? {
                return Err("替换文件被外部修改，已保留文件".into());
            }
            filesystem::remove_registered(&incoming, &name, expected, limit(index))?;
        }
        filesystem::remove_directory(operation, "incoming", &incoming)?;
    }
    if let Some(backup) = owned_child(operation, "backup", &journal.backup_key)? {
        if !backup.names()?.is_empty() {
            return Err("回滚后的备份目录含未知文件".into());
        }
        filesystem::remove_directory(operation, "backup", &backup)?;
    }
    journal.state = State::RolledBack;
    write_journal(operation, journal)
}
fn finish_committed(root: &Dir, operation: &Dir, journal: &mut Journal) -> Result<()> {
    let (_, instance) = bound_dirs(root, journal)?;
    let backup =
        owned_child(operation, "backup", &journal.backup_key)?.ok_or("重置备份目录缺失")?;
    for (index, file) in journal.files.iter().enumerate() {
        if !matches_file(
            &instance,
            &file.name,
            file.replacement.as_ref().ok_or("替换记录缺失")?,
            index,
        )? || !matches_file(&backup, &file.name, &file.original, index)?
        {
            return Err("已提交的核心文件或原始备份发生变化，已保留重置记录".into());
        }
    }
    cleanup_stage(operation, journal)?;
    if let Some(incoming) = owned_child(operation, "incoming", &journal.incoming_key)? {
        if !incoming.names()?.is_empty() {
            return Err("已提交的替换目录含未知文件".into());
        }
        filesystem::remove_directory(operation, "incoming", &incoming)?;
    }
    journal.state = State::Finished;
    write_journal(operation, journal)
}
fn recover_one(root: &Dir, operation: &Dir, journal: &mut Journal) -> Result<()> {
    if operation.key()? != journal.operation_key {
        return Err("实例重置记录目录已变化".into());
    }
    match journal.state {
        State::Staging => finish_rollback(operation, journal),
        State::Prepared => {
            rollback(root, operation, journal)?;
            finish_rollback(operation, journal)
        }
        State::Committed => finish_committed(root, operation, journal),
        State::Finished | State::RolledBack => Ok(()),
    }
}

/// Read-only readiness guard used before launch and all mutations of this root.
pub fn ensure_ready(root: &Path) -> Result<()> {
    ensure_ready_except(root, None)
}
fn ensure_ready_except(root: &Path, active: Option<&str>) -> Result<()> {
    let path = root.canonicalize().map_err(err)?;
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
            return Err("实例重置目录含未知记录，已保留文件".into());
        }
        let operation = store.child(&name)?;
        if filesystem::journal_bytes(&operation)?.is_none() {
            return Err("存在未完成的实例重置，请先恢复后再操作或启动".into());
        }
        let journal = read_journal(&operation, &name)?;
        if journal.root != path
            || journal.root_key != dir.key()?
            || journal.operation_key != operation.key()?
        {
            return Err("实例重置记录与当前游戏目录不匹配，已保留文件".into());
        }
        if !matches!(journal.state, State::Finished | State::RolledBack)
            && !(active == Some(name.as_str()) && journal.state == State::Staging)
        {
            return Err("存在未完成的实例重置，请先恢复后再操作或启动".into());
        }
    }
    Ok(())
}

/// Requires the shared writer admission. Prepared operations restore originals;
/// committed operations retain the completed reset and finish owned cleanup.
pub fn recover_pending(root: &Path) -> Result<Value> {
    let path = root.canonicalize().map_err(err)?;
    let dir = Dir::open(&path)?;
    let Some(store) = storage(&dir, false)? else {
        return Ok(json!({"recovered": 0}));
    };
    let _lock = lock(&store)?;
    let mut recovered = 0usize;
    for name in store.names()? {
        if name == ".lock" {
            continue;
        }
        if !operation_ok(&name) {
            return Err("实例重置目录含未知记录，已保留文件".into());
        }
        let operation = store.child(&name)?;
        if filesystem::journal_bytes(&operation)?.is_none() {
            if operation.names()?.is_empty() {
                filesystem::remove_directory(&store, &name, &operation)?;
                recovered += 1;
                continue;
            }
            return Err("未登记的重置目录含有文件，已保留待人工检查".into());
        }
        let mut journal = read_journal(&operation, &name)?;
        if journal.root != path || journal.root_key != dir.key()? {
            return Err("重置记录与当前游戏目录不匹配".into());
        }
        if !matches!(journal.state, State::Finished | State::RolledBack) {
            recover_one(&dir, &operation, &mut journal)
                .map_err(|e| format!("取消清理失败：{e}"))?;
            recovered += 1;
        }
    }
    ensure_ready(&path)?;
    Ok(json!({"recovered": recovered, "message": "实例重置恢复完成"}))
}

fn setup(root: &Dir, store: &Dir, plan: &ResetPlan) -> Result<(Dir, Journal)> {
    let name = operation_id();
    let operation = store.create_dir(&name)?;
    let mut journal = Journal {
        schema: SCHEMA,
        operation_id: name,
        root: plan.root.clone(),
        root_key: root.key()?,
        versions_key: plan.versions_key.clone(),
        instance_key: plan.instance_key.clone(),
        operation_key: operation.key()?,
        instance_id: plan.id.clone(),
        stage_key: None,
        incoming_key: None,
        backup_key: None,
        state: State::Staging,
        files: plan
            .originals
            .iter()
            .enumerate()
            .map(|(i, original)| CoreFile {
                name: format!("{}.{}", plan.id, if i == 0 { "json" } else { "jar" }),
                original: original.clone(),
                replacement: None,
            })
            .collect(),
    };
    if let Err(error) = write_journal(&operation, &journal) {
        return Err(filesystem::failed_initial_journal(
            store, &operation, &journal, error,
        ));
    }
    let result = (|| {
        journal.backup_key = Some(operation.create_dir("backup")?.key()?);
        write_journal(&operation, &journal)?;
        journal.incoming_key = Some(operation.create_dir("incoming")?.key()?);
        write_journal(&operation, &journal)?;
        journal.stage_key = Some(operation.create_dir("stage")?.key()?);
        write_journal(&operation, &journal)
    })();
    if let Err(error) = result {
        return match finish_rollback(&operation, &mut journal) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
    Ok((operation, journal))
}
fn recheck_plan(root: &Path, plan: &ResetPlan) -> Result<()> {
    let fresh = prepare(root, &plan.id, plan.components.clone())?;
    if fresh.revision != plan.revision
        || fresh.root != plan.root
        || fresh.root_key != plan.root_key
        || fresh.minecraft != plan.minecraft
    {
        return Err("实例或关联版本已变化，请重新读取重置计划".into());
    }
    Ok(())
}
fn check_originals(root: &Dir, operation: &Dir, journal: &Journal) -> Result<Dir> {
    if operation.key()? != journal.operation_key {
        return Err("重置记录目录已变化".into());
    }
    let (_, instance) = bound_dirs(root, journal)?;
    for (index, file) in journal.files.iter().enumerate() {
        if snapshot(instance.regular(&file.name)?, limit(index), None)? != file.original {
            return Err("实例核心文件已变化，请重新读取重置计划".into());
        }
    }
    Ok(instance)
}

fn commit(
    root: &Dir,
    operation: &Dir,
    journal: &mut Journal,
    cancel: &AtomicBool,
    changed: &impl Fn(usize) -> Result<()>,
) -> Result<()> {
    check(cancel)?;
    let instance = check_originals(root, operation, journal)?;
    let backup = owned_child(operation, "backup", &journal.backup_key)?.ok_or("备份目录缺失")?;
    let incoming =
        owned_child(operation, "incoming", &journal.incoming_key)?.ok_or("替换目录缺失")?;
    let mut expected_names: Vec<_> = journal.files.iter().map(|f| f.name.clone()).collect();
    expected_names.sort();
    if !backup.names()?.is_empty() || incoming.names()? != expected_names {
        return Err("重置暂存文件与记录不一致".into());
    }
    for (index, file) in journal.files.iter().enumerate() {
        let expected = file.replacement.as_ref().ok_or("替换核心文件未登记")?;
        if !matches_file(&incoming, &file.name, expected, index)? {
            return Err("替换核心文件已变化".into());
        }
    }
    let result = (|| {
        journal.state = State::Prepared;
        write_journal(operation, journal)?;
        let mut moved = 0;
        for (index, file) in journal.files.iter().enumerate() {
            check(cancel)?;
            bound_dirs(root, journal)?;
            if !matches_file(&instance, &file.name, &file.original, index)? {
                return Err("原始核心文件已变化".into());
            }
            rename_new(&instance, &file.name, &backup, &file.name)?;
            moved += 1;
            changed(moved)?;
            check(cancel)?;
            bound_dirs(root, journal)?;
            rename_new(&incoming, &file.name, &instance, &file.name)?;
            moved += 1;
            changed(moved)?;
        }
        check(cancel)?;
        for (index, file) in journal.files.iter().enumerate() {
            if !matches_file(
                &instance,
                &file.name,
                file.replacement.as_ref().unwrap(),
                index,
            )? || !matches_file(&backup, &file.name, &file.original, index)?
            {
                return Err("核心替换结果校验失败".into());
            }
        }
        bound_dirs(root, journal)?;
        journal.state = State::Committed;
        if let Err(error) = write_journal(operation, journal) {
            #[cfg(not(target_os = "linux"))]
            return Err(format!(
                "{error}；提交结果未确认，已保留新核心与原始备份，请重新读取恢复记录"
            ));
            #[cfg(target_os = "linux")]
            {
                // A failed fsync may nevertheless have published the commit marker.
                // Re-publish Prepared before rollback; if that fails, recovery keeps
                // the durable record and the backup without guessing its outcome.
                journal.state = State::Prepared;
                if let Err(record) = write_journal(operation, journal) {
                    journal.state = State::Committed;
                    return Err(format!("取消清理失败：无法确认提交或回滚记录，已保留完整核心和原始备份：{record}；原错误：{error}"));
                }
                return Err(error);
            }
        }
        Ok(())
    })();
    drop(incoming);
    drop(backup);
    if let Err(error) = result {
        if journal.state == State::Committed {
            return Err(error);
        }
        return match rollback(root, operation, journal)
            .and_then(|_| finish_rollback(operation, journal))
        {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
    // Cancellation ends at the durable commit marker. A late cancel retains the
    // complete reset and its backup. Cleanup errors stay visible and recoverable.
    finish_committed(root, operation, journal)
        .map_err(|e| format!("取消清理失败：重置已完成，但暂存清理失败：{e}"))
}

#[cfg(test)]
pub fn execute(
    root: &Path,
    project: &Path,
    plan: ResetPlan,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
) -> Result<Value> {
    execute_with_policy(
        root,
        project,
        plan,
        pcl_network::download_snapshot(),
        cancel,
        callback,
    )
}
#[cfg(test)]
pub fn execute_with_policy(
    root: &Path,
    project: &Path,
    plan: ResetPlan,
    scheduler: std::sync::Arc<pcl_network::DownloadScheduler>,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
) -> Result<Value> {
    check(cancel)?;
    recheck_plan(root, &plan)?;
    let installer = Installer::new()?
        .with_download_policy(scheduler)
        .with_project(project)
        .with_cache_source(&plan.root)?;
    let resolved = installer.resolve_request(&plan.install_request(), cancel)?;
    execute_prepared(root, plan, installer, resolved, cancel, callback, || Ok(()))
}

/// Java acquisition precedes the durable reset journal. The same resolved
/// authority is then installed in its private stage; a failed Java download
/// therefore leaves neither a pending reset nor a partly replaced core.
pub(crate) fn execute_prepared(
    root: &Path,
    plan: ResetPlan,
    installer: Installer,
    resolved: ResolvedInstallRequest,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
    before_commit: impl Fn() -> Result<()>,
) -> Result<Value> {
    check(cancel)?;
    recheck_plan(root, &plan)?;
    if serde_json::to_value(resolved.request()).map_err(err)?
        != serde_json::to_value(plan.install_request()).map_err(err)?
    {
        return Err("安装元数据与重置计划不一致，请重新检查".into());
    }
    let root_dir = Dir::open(&plan.root)?;
    let store = storage(&root_dir, true)?.ok_or("重置记录目录缺失")?;
    let _lock = lock(&store)?;
    recheck_plan(root, &plan)?;
    let (operation, mut journal) = setup(&root_dir, &store, &plan)?;
    let last = Mutex::new(None::<Progress>);
    let result = (|| {
        let stage =
            owned_child(&operation, "stage", &journal.stage_key)?.ok_or("安装暂存目录缺失")?;
        installer.install_resolved_request_bound(
            stage.bound_file()?,
            &resolved,
            cancel,
            |mut p| {
                if p.stage == "complete" {
                    p.stage = "reset_commit".into();
                    p.message = "正在替换实例核心文件".into();
                }
                p.steps.push(pcl_install::InstallStep {
                    id: "reset_core".into(),
                    label: "重置实例组件".into(),
                    state: if p.stage == "reset_commit" {
                        "running"
                    } else {
                        "pending"
                    }
                    .into(),
                    progress: None,
                });
                *last.lock().unwrap() = Some(p.clone());
                callback(p);
            },
        )?;
        check(cancel)?;
        bound_dirs(&root_dir, &journal)?;
        if stage.key()? != operation.child("stage")?.key()? {
            return Err("安装暂存目录已变化".into());
        }
        let installed = stage.child("versions")?.child(&plan.id)?;
        let (metadata, installed_json) = read_json(&installed, &format!("{}.json", plan.id))?;
        let (mc, selections, _) = identity(&metadata, &plan.id)?;
        let mut requested = plan.components.clone();
        for component in &mut requested {
            if component.provider == "forge" {
                if let Some(version) = component
                    .version
                    .strip_prefix(&format!("{}-", plan.minecraft))
                {
                    component.version = version.to_owned();
                }
            }
        }
        if mc != plan.minecraft
            || serde_json::to_value(&selections).map_err(err)?
                != serde_json::to_value(&requested).map_err(err)?
        {
            return Err("生成的实例版本或组件与重置计划不一致".into());
        }
        let incoming =
            owned_child(&operation, "incoming", &journal.incoming_key)?.ok_or("替换目录缺失")?;
        for index in 0..journal.files.len() {
            let name = journal.files[index].name.clone();
            copy_core(
                installed.regular(&name)?,
                &incoming,
                &operation,
                &mut journal,
                index,
                cancel,
            )?;
            if index == 0 {
                let copied = journal.files[index]
                    .replacement
                    .as_ref()
                    .ok_or("生成的核心 JSON 尚未登记")?;
                if copied.size != installed_json.size || copied.hash != installed_json.hash {
                    return Err("生成的核心 JSON 在组件校验与复制之间已变化".into());
                }
            }
        }
        for cache in ["libraries", "assets"] {
            if let Some(source) = stage.optional(cache)? {
                filesystem::merge_cache_bound(
                    &source,
                    &root_dir.ensure(cache)?,
                    &root_dir,
                    cancel,
                    &mut 0,
                )?;
            }
        }
        // Native directory cleanup must not run while our own handles retain
        // descendants of that installer stage or the incoming directory.
        drop(installed);
        drop(incoming);
        drop(stage);
        // The Staging record intentionally blocks public prepare. Revalidate the
        // dependency set with this operation temporarily considered ready.
        let fresh = prepare_bound(
            &plan.root,
            &plan.id,
            plan.components.clone(),
            Some(&journal.operation_id),
        )?;
        if fresh.revision != plan.revision {
            return Err("安装期间实例或关联版本已变化，请重新读取重置计划".into());
        }
        before_commit()?;
        commit(&root_dir, &operation, &mut journal, cancel, &|_| Ok(()))?;
        Ok(())
    })();
    if let Err(error) = result {
        if journal.state == State::Staging {
            return match finish_rollback(&operation, &mut journal) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
            };
        }
        return Err(error);
    }
    let mut progress = last.into_inner().unwrap().unwrap_or(Progress {
        steps: vec![],
        stage: String::new(),
        message: String::new(),
        completed: 1,
        total: 1,
        bytes_done: 0,
        bytes_total: 0,
        network_bytes: 0,
    });
    progress.stage = "complete".into();
    progress.message = format!("{} 组件重置完成", plan.id);
    for step in &mut progress.steps {
        step.state = "complete".into();
        step.progress = Some(1.0);
    }
    callback(progress);
    Ok(
        json!({"id": plan.id, "minecraft": plan.minecraft, "components": plan.components,
        "backup_id": journal.operation_id, "message": "组件重置完成，实例内容已保留"}),
    )
}

#[cfg(test)]
#[path = "instance_reset/portable_tests.rs"]
mod portable_tests;
#[cfg(all(test, target_os = "linux"))]
#[path = "instance_reset/tests.rs"]
mod tests;
