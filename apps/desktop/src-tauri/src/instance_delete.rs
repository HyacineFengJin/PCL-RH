//! Recoverable instance deletion, with launcher references retained as tombstones.
//!
//! The only game-data mutation is one no-overwrite rename of `versions/id` into
//! a root-owned recovery slot. Shared libraries/assets/resources are never
//! visited or removed. Original settings, metadata, export presets and resource
//! undo histories stay scoped to the old ID; callers must enforce the name
//! reservation through `ensure_name_available` before installing/importing or
//! renaming another instance, and before addressing game data by a reserved ID.
//!
//! `prepare` binds root/project directory identities, dependency JSONs, settings
//! and the complete content tree. `execute` repeats those checks under writer
//! admission and a nonblocking process lock. The `committing` progress callback
//! is the task cancellation gate; cancellation is checked once more afterwards,
//! then no longer changes the outcome. Failures after that gate retain Error.
//!
//! Durable states define recovery, rather than the presence of a directory:
//! Prepared rolls a partial delete back; Deleted retains the recovery copy;
//! Restoring rolls a partial restore back into recovery; Restored/RolledBack
//! release the name. A project marker blocks cooperating writers across roots
//! until a terminal state is durable and its matching marker is cleared. Unknown
//! content, replaced inodes, mounts, changed journal files and destination
//! conflicts are retained and reported, never overwritten or recursively erased.
//!
//! This module owns plans, dependencies and durable state transitions.
//! `filesystem` owns descriptor access, snapshots and the recovery-store lock;
//! it never decides which journal state permits cleanup or recovery.

use crate::instance_rename_refs::{self, DiskSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

mod filesystem;
pub(crate) mod project_gate;
use filesystem::{
    absolute, anonymous_key, canonical, hash, lock, name_ok, read_bytes, rename_new, scan, Dir,
    FileToken, Key, Tree, MAX_DEPTH, MAX_FILES,
};
use project_gate::{app, operation_ok, read_marker, scope_ok, Marker, MARKER, READY};

const STORE: &str = "instance-deletions";
const MAX_INSTANCES: usize = 4096;
const MAX_JOURNAL: usize = 64 * 1024 * 1024;
const MAX_JSON: usize = 8 * 1024 * 1024;
const CANCELLED: &str = "实例删除已取消";
static NEXT: AtomicU64 = AtomicU64::new(0);
type Result<T> = std::result::Result<T, String>;

fn nonce() -> String {
    format!(
        "d-{:x}-{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
fn cancelled(cancel: Option<&AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Profile {
    directory: Key,
    json: Option<FileToken>,
}
fn dependencies(
    versions: &Dir,
    id: &str,
    cancel: Option<&AtomicBool>,
) -> Result<BTreeMap<String, Profile>> {
    let names = versions.names()?;
    if names.len() > MAX_INSTANCES {
        return Err("实例数量超过安全上限".into());
    }
    let mut profiles = BTreeMap::new();
    let mut bytes = 0u64;
    for name in names {
        cancelled(cancel)?;
        let directory = versions.child(&name)?;
        let filename = format!("{name}.json");
        let token = if directory.stat(&filename)?.is_some() {
            let (token, data) = read_bytes(&directory, &filename, MAX_JSON)?;
            bytes = bytes
                .checked_add(data.len() as u64)
                .ok_or("版本 JSON 总大小过大")?;
            if bytes > 64 * 1024 * 1024 {
                return Err("版本 JSON 总大小超过安全上限".into());
            }
            if name != id {
                let value: Value = serde_json::from_slice(&data)
                    .map_err(|_| format!("版本 {name} 的 JSON 损坏，无法确认实例引用"))?;
                if !value.is_object() || value["id"].as_str() != Some(name.as_str()) {
                    return Err(format!(
                        "版本 {name} 的 JSON 标识与目录不符，无法确认实例引用"
                    ));
                }
                for field in ["inheritsFrom", "jar"] {
                    if let Some(reference) = value.get(field) {
                        let reference = reference.as_str().ok_or("版本引用标识无效")?;
                        name_ok(reference)?;
                        if reference == id {
                            return Err(format!(
                                "实例 {name} 的 {field} 引用了待删除实例，请先处理依赖实例"
                            ));
                        }
                    }
                }
            }
            Some(token)
        } else {
            None
        };
        profiles.insert(
            name,
            Profile {
                directory: directory.key()?,
                json: token,
            },
        );
    }
    Ok(profiles)
}

/// Only display fields cross the IPC boundary. The server retains the bound
/// snapshot and prepares again on submission before comparing `revision`.
#[derive(Clone, Debug, Serialize)]
pub struct DeletePlan {
    pub id: String,
    pub root_id: String,
    pub revision: String,
    pub total_files: u64,
    pub total_bytes: u64,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    project: PathBuf,
    #[serde(skip)]
    root_key: Key,
    #[serde(skip)]
    project_key: Key,
    #[serde(skip)]
    versions_key: Key,
    #[serde(skip)]
    tree: Tree,
    #[serde(skip)]
    profiles: BTreeMap<String, Profile>,
    #[serde(skip)]
    settings: DiskSnapshot,
}
#[derive(Clone, Debug, Serialize)]
pub struct DeleteProgress {
    pub phase: String,
    pub message: String,
    pub completed: u64,
    pub total: u64,
}
fn progress(
    report: &impl Fn(DeleteProgress),
    phase: &str,
    message: &str,
    completed: u64,
    total: u64,
) {
    report(DeleteProgress {
        phase: phase.into(),
        message: message.into(),
        completed,
        total,
    });
}
fn ensure_rename_ready(project: &Path) -> Result<()> {
    // The combined reference-writer guard also calls our deletion guard. Read
    // the rename marker directly to avoid recursion and to keep recovery able
    // to service its own deletion marker under the settings lock.
    if instance_rename_refs::pending_root(project)?.is_some() {
        Err("存在未完成的实例重命名，请先恢复后删除或恢复实例".into())
    } else {
        Ok(())
    }
}

pub fn prepare(root: &Path, project: &Path, root_id: &str, id: &str) -> Result<DeletePlan> {
    prepare_bound(root, project, root_id, id, None)
}
fn prepare_bound(
    root: &Path,
    project: &Path,
    root_id: &str,
    id: &str,
    cancel: Option<&AtomicBool>,
) -> Result<DeletePlan> {
    scope_ok(root_id)?;
    name_ok(id)?;
    ensure_ready(root)?;
    ensure_name_available(root, id)?;
    ensure_project_ready(project)?;
    ensure_rename_ready(project)?;
    let root = canonical(root)?;
    let project = canonical(project)?;
    let root_dir = Dir::open(&root)?;
    let project_dir = Dir::open(&project)?;
    let versions = root_dir.child("versions")?;
    let instance = versions.child(id)?;
    let tree = scan(&instance, cancel)?;
    let profiles = dependencies(&versions, id, cancel)?;
    let (settings, bytes) =
        instance_rename_refs::store_snapshot(&project, "settings.json", 2 * 1024 * 1024)?;
    if let Some(bytes) = bytes {
        crate::config::check_delete_dependencies(&bytes, root_id, &root, id)?;
    }
    let root_key = root_dir.key()?;
    let project_key = project_dir.key()?;
    let versions_key = versions.key()?;
    let revision = hash(
        &serde_json::to_vec(&(
            root_id,
            &root,
            &project,
            &root_key,
            &project_key,
            &versions_key,
            &tree,
            &profiles,
            &settings,
        ))
        .map_err(|e| e.to_string())?,
    );
    // Pins alone survive an external rename of the root. Verify that the user
    // visible paths still resolve to those same directories before confirmation.
    if Dir::open(&root)?.key() != Ok(root_key.clone())
        || Dir::open(&project)?.key() != Ok(project_key.clone())
        || root_dir.child("versions")?.key() != Ok(versions_key.clone())
    {
        return Err("实例目录在检查期间已被替换".into());
    }
    Ok(DeletePlan {
        id: id.into(),
        root_id: root_id.into(),
        revision: format!("delete:{revision}"),
        total_files: tree.files,
        total_bytes: tree.bytes,
        root,
        project,
        root_key,
        project_key,
        versions_key,
        tree,
        profiles,
        settings,
    })
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    Prepared,
    Deleted,
    Restoring,
    Restored,
    RolledBack,
}
impl State {
    fn terminal(self) -> bool {
        matches!(self, Self::Deleted | Self::Restored | Self::RolledBack)
    }
    fn reserved(self) -> bool {
        matches!(self, Self::Prepared | Self::Deleted | Self::Restoring)
    }
    fn label(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Deleted => "deleted",
            Self::Restoring => "restoring",
            Self::Restored => "restored",
            Self::RolledBack => "rolled_back",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    operation_id: String,
    root_id: String,
    id: String,
    root: PathBuf,
    project: PathBuf,
    root_key: Key,
    project_key: Key,
    versions_key: Key,
    storage_key: Key,
    operation_key: Key,
    created_ms: u64,
    state: State,
    tree: Tree,
    marker_cleared: bool,
    generation: u64,
    previous_hash: Option<String>,
}
struct Record {
    journal: Journal,
    token: Option<FileToken>,
}
fn storage(root: &Dir, create: bool) -> Result<Option<Dir>> {
    let app = if create {
        Some(root.ensure(".pcl-rust")?)
    } else {
        root.optional(".pcl-rust")?
    };
    let Some(app) = app else { return Ok(None) };
    if create {
        app.ensure(STORE).map(Some)
    } else {
        app.optional(STORE)
    }
}
fn validate(j: &Journal, name: &str) -> Result<()> {
    name_ok(&j.id)?;
    scope_ok(&j.root_id)?;
    absolute(&j.root)?;
    absolute(&j.project)?;
    if j.schema != 1
        || j.operation_id != name
        || !operation_ok(name)
        || j.tree.entries.is_empty()
        || (j.generation == 0) != j.previous_hash.is_none()
        || j.previous_hash
            .as_ref()
            .is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
        || j.tree.entries.len() > MAX_FILES
        || !j.tree.entries[0].path.is_empty()
        || j.tree.entries[0].hash.is_some()
        || !j.tree.entries[0].stamp.key.same_volume(&j.root_key)
    {
        return Err("实例删除记录版本或范围无效，已保留".into());
    }
    let mut paths = std::collections::BTreeSet::new();
    let mut files = 0u64;
    let mut bytes = 0u64;
    for e in &j.tree.entries {
        if !paths.insert(&e.path)
            || e.path.len() > 4096
            || e.path.split('/').count() > MAX_DEPTH + 1
        {
            return Err("实例删除内容记录无效，已保留".into());
        }
        if !e.path.is_empty() {
            for p in e.path.split('/') {
                name_ok(p)?;
            }
        }
        if !e.stamp.key.same_volume(&j.root_key)
            || !e.stamp.key.valid()
            || e.stamp.links == 0
            || e.hash
                .as_ref()
                .is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("实例删除内容身份无效，已保留".into());
        }
        match (
            e.stamp.is_directory(),
            e.stamp.is_regular(),
            e.hash.is_some(),
        ) {
            (true, false, false) => {}
            (false, true, true) if e.stamp.links == 1 => {
                files += 1;
                bytes = bytes.checked_add(e.stamp.size).ok_or("实例记录过大")?;
            }
            _ => return Err("实例删除内容类型无效，已保留".into()),
        }
    }
    if files != j.tree.files || bytes != j.tree.bytes {
        return Err("实例删除内容统计无效，已保留".into());
    }
    Ok(())
}
fn read_record(operation: &Dir, name: &str) -> Result<Record> {
    let (token, bytes) = read_bytes(operation, "journal.json", MAX_JOURNAL)?;
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|_| "实例删除记录损坏，文件已保留")?;
    validate(&journal, name)?;
    if operation.key() != Ok(journal.operation_key.clone()) {
        return Err("实例恢复记录目录已被替换".into());
    }
    for name in operation.names()? {
        if !matches!(name.as_str(), "journal.json" | "instance") {
            return Err("实例恢复目录含未知文件，文件已保留".into());
        }
    }
    Ok(Record {
        journal,
        token: Some(token),
    })
}
fn write_record(operation: &Dir, record: &mut Record) -> Result<()> {
    if operation.key() != Ok(record.journal.operation_key.clone()) {
        return Err("实例恢复记录目录已被替换".into());
    }
    if let Some(expected) = &record.token {
        let (actual, bytes) = read_bytes(operation, "journal.json", MAX_JOURNAL)?;
        if expected != &actual {
            return Err("实例删除记录已被外部修改，已保留".into());
        }
        let previous: Journal =
            serde_json::from_slice(&bytes).map_err(|_| "实例删除记录损坏，已保留")?;
        record.journal.generation = previous
            .generation
            .checked_add(1)
            .ok_or("实例删除记录代次已耗尽")?;
        record.journal.previous_hash = Some(actual.hash);
    }
    validate(&record.journal, &record.journal.operation_id)?;
    let bytes = serde_json::to_vec(&record.journal).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_JOURNAL {
        return Err("实例删除记录超过安全上限".into());
    }
    let mut file = operation.anonymous()?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let current = operation.stat("journal.json")?;
    if record.token.is_none() {
        if current.is_some() {
            return Err("初始实例删除记录已被外部占用".into());
        }
        operation.link(&file, "journal.json")?;
    } else {
        let expected = record.token.as_ref().unwrap();
        if expected != &read_bytes(operation, "journal.json", MAX_JOURNAL)?.0 {
            return Err("实例删除记录已被外部修改，已保留".into());
        }
        let next = format!("journal-{}.next", nonce());
        operation.link(&file, &next)?;
        let temporary = read_bytes(operation, &next, MAX_JOURNAL)?.0;
        if let Err(error) = operation.exchange(&next, "journal.json", "无法保存实例删除状态")
        {
            operation.unlink_file(&next, &temporary)?;
            return Err(error);
        }
        let displaced = read_bytes(operation, &next, MAX_JOURNAL)?.0;
        if !expected.moved_matches(&displaced) {
            // Preserve external writes even if they raced the final exchange.
            if !temporary.moved_matches(&read_bytes(operation, "journal.json", MAX_JOURNAL)?.0) {
                return Err("实例删除状态交换期间发生冲突，两份记录均已保留".into());
            }
            operation.exchange(&next, "journal.json", "实例删除记录冲突，无法还原记录")?;
            operation.sync()?;
            operation.unlink_file(&next, &temporary)?;
            return Err("实例删除记录被外部修改，原记录已还原并保留".into());
        }
        operation.sync()?;
        operation.unlink_file(&next, &displaced)?;
    }
    record.token = Some(read_bytes(operation, "journal.json", MAX_JOURNAL)?.0);
    Ok(())
}
fn immutable_journal(j: &Journal) -> Result<Vec<u8>> {
    serde_json::to_vec(&(
        &j.operation_id,
        &j.root_id,
        &j.id,
        &j.root,
        &j.project,
        &j.root_key,
        &j.project_key,
        &j.versions_key,
        &j.storage_key,
        &j.operation_key,
        j.created_ms,
        &j.tree,
    ))
    .map_err(|e| e.to_string())
}
fn cleanup_journal_temps(
    root: &Dir,
    store: &Dir,
    operation: &Dir,
    name: &str,
    project: &Path,
) -> Result<()> {
    let (current_token, bytes) = read_bytes(operation, "journal.json", MAX_JOURNAL)?;
    let current: Journal =
        serde_json::from_slice(&bytes).map_err(|_| "实例删除记录损坏，已保留")?;
    validate(&current, name)?;
    // The generation chain proves publication order, not directory ownership.
    // Bind every recorded identity before deleting even a recognised .next file.
    bind(root, store, operation, &current, project)?;
    for temporary in operation.names()? {
        if matches!(temporary.as_str(), "journal.json" | "instance") {
            continue;
        }
        let Some(candidate) = temporary
            .strip_prefix("journal-")
            .and_then(|s| s.strip_suffix(".next"))
        else {
            return Err("实例恢复目录含未知文件，已保留".into());
        };
        if !operation_ok(candidate) {
            return Err("实例恢复目录含未知暂存文件，已保留".into());
        }
        let (token, bytes) = read_bytes(operation, &temporary, MAX_JOURNAL)?;
        let alternate: Journal =
            serde_json::from_slice(&bytes).map_err(|_| "实例删除暂存记录损坏，已保留")?;
        validate(&alternate, name)?;
        let before_exchange = alternate.generation == current.generation.saturating_add(1)
            && alternate.previous_hash.as_deref() == Some(current_token.hash.as_str());
        let after_exchange = current.generation == alternate.generation.saturating_add(1)
            && current.previous_hash.as_deref() == Some(token.hash.as_str());
        if immutable_journal(&current)? != immutable_journal(&alternate)?
            || !(before_exchange || after_exchange)
        {
            return Err("实例删除暂存记录内容发生冲突，原记录和暂存记录均已保留".into());
        }
        if !current_token.moved_matches(&read_bytes(operation, "journal.json", MAX_JOURNAL)?.0) {
            return Err("实例删除记录清理期间已变化，已保留".into());
        }
        operation.unlink_file(&temporary, &token)?;
    }
    Ok(())
}
fn marker_matches(marker: &Marker, j: &Journal) -> bool {
    marker.operation_id == j.operation_id
        && marker.root_id == j.root_id
        && marker.id == j.id
        && marker.root == j.root
        && marker.project == j.project
        && marker.root_key == j.root_key
        && marker.project_key == j.project_key
        && marker.storage_key == j.storage_key
        && marker.operation_key == j.operation_key
}
fn create_marker(j: &Journal) -> Result<()> {
    if let Some((marker, _)) = read_marker(&j.project)? {
        return if marker_matches(&marker, j) {
            Ok(())
        } else {
            Err(READY.into())
        };
    }
    let app = app(&j.project, true)?.ok_or("应用目录不可用")?;
    let mut file = app.anonymous()?;
    let marker = Marker {
        schema: 1,
        operation_id: j.operation_id.clone(),
        root_id: j.root_id.clone(),
        id: j.id.clone(),
        root: j.root.clone(),
        project: j.project.clone(),
        root_key: j.root_key.clone(),
        project_key: j.project_key.clone(),
        storage_key: j.storage_key.clone(),
        operation_key: j.operation_key.clone(),
        file_key: anonymous_key(&file)?,
    };
    file.write_all(&serde_json::to_vec(&marker).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    app.link(&file, MARKER)
}
fn clear_marker(operation: &Dir, record: &mut Record) -> Result<()> {
    let j = &record.journal;
    if let Some((marker, token)) = read_marker(&j.project)? {
        if !marker_matches(&marker, j) {
            return Err("实例删除锁定标记属于其他操作，已保留".into());
        }
        app(&j.project, false)?
            .ok_or("实例删除锁定目录消失")?
            .unlink_file(MARKER, &token)?;
    }
    record.journal.marker_cleared = true;
    write_record(operation, record)
}

pub fn pending_root(project: &Path) -> Result<Option<PathBuf>> {
    project_gate::pending_root(project)
}
pub fn ensure_project_ready(project: &Path) -> Result<()> {
    project_gate::ensure_project_ready(project)
}

fn bind(root: &Dir, store: &Dir, operation: &Dir, j: &Journal, project: &Path) -> Result<Dir> {
    if canonical(project)? != j.project
        || Dir::open(&j.project)?.key() != Ok(j.project_key.clone())
        || root.key() != Ok(j.root_key.clone())
        || Dir::open(&j.root)?.key() != Ok(j.root_key.clone())
        || storage(root, false)?.ok_or("实例恢复目录缺失")?.key() != Ok(j.storage_key.clone())
        || store.key() != Ok(j.storage_key.clone())
        || store.child(&j.operation_id)?.key() != Ok(j.operation_key.clone())
        || operation.key() != Ok(j.operation_key.clone())
    {
        return Err("实例删除记录与当前目录身份不符，文件已保留".into());
    }
    let versions = root.child("versions")?;
    if versions.key() != Ok(j.versions_key.clone()) {
        return Err("versions目录已被替换，文件已保留".into());
    }
    Ok(versions)
}
fn verify_tree(directory: &Dir, j: &Journal) -> Result<()> {
    if !j.tree.moved_matches(&scan(directory, None)?) {
        return Err("实例源目录或恢复副本已被外部修改，文件已保留".into());
    }
    Ok(())
}
fn rollback_delete(root: &Dir, store: &Dir, operation: &Dir, record: &mut Record) -> Result<()> {
    let versions = bind(
        root,
        store,
        operation,
        &record.journal,
        &record.journal.project,
    )?;
    match (
        versions.optional(&record.journal.id)?,
        operation.optional("instance")?,
    ) {
        (Some(source), None) => verify_tree(&source, &record.journal)?,
        (None, Some(recovery)) => {
            verify_tree(&recovery, &record.journal)?;
            rename_new(operation, "instance", &versions, &record.journal.id)?;
            verify_tree(&versions.child(&record.journal.id)?, &record.journal)?;
        }
        _ => return Err("删除恢复发生源目录或目标冲突，两处内容已保留".into()),
    }
    record.journal.state = State::RolledBack;
    write_record(operation, record)?;
    clear_marker(operation, record)
}
fn rollback_restore(root: &Dir, store: &Dir, operation: &Dir, record: &mut Record) -> Result<()> {
    let versions = bind(
        root,
        store,
        operation,
        &record.journal,
        &record.journal.project,
    )?;
    match (
        versions.optional(&record.journal.id)?,
        operation.optional("instance")?,
    ) {
        (None, Some(recovery)) => verify_tree(&recovery, &record.journal)?,
        (Some(source), None) => {
            verify_tree(&source, &record.journal)?;
            rename_new(&versions, &record.journal.id, operation, "instance")?;
            verify_tree(&operation.child("instance")?, &record.journal)?;
        }
        _ => return Err("实例恢复发生源目录或目标冲突，两处内容已保留".into()),
    }
    record.journal.state = State::Deleted;
    write_record(operation, record)?;
    clear_marker(operation, record)
}

fn visit_records(
    root: &Dir,
    store: &Dir,
    mut visit: impl FnMut(&Dir, Record) -> Result<()>,
) -> Result<()> {
    // Do not retain every directory FD or full content snapshot while scanning
    // history. Thousands of terminal journals must not exhaust descriptors or
    // multiply the per-journal memory bound in every bootstrap guard.
    for name in store.names()? {
        if name == ".lock" {
            store.regular(&name)?;
            continue;
        }
        if !operation_ok(&name) {
            return Err("实例恢复区含未知记录，已保留".into());
        }
        let operation = store.child(&name)?;
        if operation.stat("journal.json")?.is_none() {
            return Err(READY.into());
        }
        let record = read_record(&operation, &name)?;
        if record.journal.root_key != root.key()? || record.journal.storage_key != store.key()? {
            return Err("实例恢复区身份与记录不匹配".into());
        }
        visit(&operation, record)?;
    }
    Ok(())
}
pub fn ensure_ready(root: &Path) -> Result<()> {
    let path = canonical(root)?;
    let root = Dir::open(&path)?;
    let Some(store) = storage(&root, false)? else {
        return Ok(());
    };
    visit_records(&root, &store, |_, record| {
        if record.journal.root != path
            || !record.journal.state.terminal()
            || !record.journal.marker_cleared
        {
            return Err(READY.into());
        }
        Ok(())
    })
}
/// A Deleted tombstone reserves its original physical ID even if a user has
/// manually created another directory at that path. Never address that new
/// directory through the deleted instance's retained launcher references.
pub fn ensure_name_available(root: &Path, id: &str) -> Result<()> {
    name_ok(id)?;
    if reserved_names(root)?.contains(id) {
        return Err("此实例名称由可恢复删除记录保留，请先恢复实例或使用其他名称".into());
    }
    Ok(())
}
/// Batch read for scans: avoid parsing every journal once per visible instance.
pub fn reserved_names(root: &Path) -> Result<BTreeSet<String>> {
    let path = canonical(root)?;
    let root = Dir::open(&path)?;
    let Some(store) = storage(&root, false)? else {
        return Ok(BTreeSet::new());
    };
    let mut names = BTreeSet::new();
    visit_records(&root, &store, |_, record| {
        if record.journal.root != path {
            return Err("实例恢复区范围不匹配".into());
        }
        if record.journal.state.reserved() {
            names.insert(record.journal.id);
        }
        Ok(())
    })?;
    Ok(names)
}

pub fn execute(
    root: &Path,
    project: &Path,
    plan: DeletePlan,
    cancel: &AtomicBool,
    report: impl Fn(DeleteProgress),
) -> Result<Value> {
    filesystem::require_mutation_support()?;
    instance_rename_refs::with_settings_lock(project, || {
        execute_locked(root, project, plan, cancel, report)
    })
}
fn execute_locked(
    root: &Path,
    project: &Path,
    plan: DeletePlan,
    cancel: &AtomicBool,
    report: impl Fn(DeleteProgress),
) -> Result<Value> {
    cancelled(Some(cancel))?;
    let root_path = canonical(root)?;
    let project_path = canonical(project)?;
    if root_path != plan.root || project_path != plan.project {
        return Err("删除任务所属目录已变化".into());
    }
    progress(
        &report,
        "checking",
        "检查实例内容与依赖",
        0,
        plan.total_files,
    );
    let checked = prepare_bound(
        &root_path,
        &project_path,
        &plan.root_id,
        &plan.id,
        Some(cancel),
    )?;
    if checked.root_key != plan.root_key
        || checked.project_key != plan.project_key
        || checked.versions_key != plan.versions_key
        || checked.tree != plan.tree
        || checked.profiles != plan.profiles
        || !plan.settings.compatible(&checked.settings)
    {
        return Err("实例内容、依赖或设置已变化，请重新检查后删除".into());
    }
    let root_dir = Dir::open(&root_path)?;
    let store = storage(&root_dir, true)?.ok_or("实例恢复区不可用")?;
    let _lock = lock(&store)?;
    ensure_ready(&root_path)?;
    let name = nonce();
    let operation = store.create(&name)?;
    let journal = Journal {
        schema: 1,
        operation_id: name.clone(),
        root_id: plan.root_id.clone(),
        id: plan.id.clone(),
        root: root_path,
        project: project_path,
        root_key: plan.root_key,
        project_key: plan.project_key,
        versions_key: plan.versions_key,
        storage_key: store.key()?,
        operation_key: operation.key()?,
        created_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64,
        state: State::Prepared,
        tree: plan.tree,
        marker_cleared: false,
        generation: 0,
        previous_hash: None,
    };
    let mut record = Record {
        journal,
        token: None,
    };
    write_record(&operation, &mut record)?;
    let mut committing = false;
    let result: Result<Value> = (|| {
        create_marker(&record.journal)?;
        cancelled(Some(cancel))?;
        progress(
            &report,
            "committing",
            "将实例移入恢复区",
            0,
            plan.total_files,
        );
        // Callback synchronizes task.begin_finishing. Honour a token accepted
        // just before that gate; all subsequent errors remain visible as Error.
        cancelled(Some(cancel))?;
        committing = true;
        let versions = bind(&root_dir, &store, &operation, &record.journal, project)?;
        verify_tree(&versions.child(&record.journal.id)?, &record.journal)?;
        if dependencies(&versions, &record.journal.id, None)? != plan.profiles {
            return Err("实例依赖在提交前变化，原内容已保留".into());
        }
        let (settings, bytes) =
            instance_rename_refs::store_snapshot(project, "settings.json", 2 * 1024 * 1024)?;
        if !plan.settings.compatible(&settings) {
            return Err("Java或实例设置在提交前变化，原内容已保留".into());
        }
        if let Some(bytes) = bytes {
            crate::config::check_delete_dependencies(
                &bytes,
                &record.journal.root_id,
                &record.journal.root,
                &record.journal.id,
            )?;
        }
        rename_new(&versions, &record.journal.id, &operation, "instance")?;
        bind(&root_dir, &store, &operation, &record.journal, project)?;
        verify_tree(&operation.child("instance")?, &record.journal)?;
        record.journal.state = State::Deleted;
        write_record(&operation, &mut record)?;
        clear_marker(&operation, &mut record)?;
        progress(
            &report,
            "complete",
            "实例已移入恢复区，可恢复原目录与资料",
            plan.total_files,
            plan.total_files,
        );
        Ok(json!({"id":record.journal.id,"operation_id":name,"deleted":true,"recoverable":true}))
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            // A durable Deleted state is committed even when marker cleanup
            // fails. Never silently restore it under a success-looking error.
            if record.journal.state == State::Prepared {
                if let Err(cleanup) = rollback_delete(&root_dir, &store, &operation, &mut record) {
                    return Err(format!("取消清理失败：{cleanup}；原错误：{error}"));
                }
            }
            if committing && error != CANCELLED && !error.starts_with("取消清理失败：") {
                Err(format!("取消清理失败：实例删除未完成：{error}"))
            } else {
                Err(error)
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DeleteHistoryEntry {
    pub operation_id: String,
    pub id: String,
    pub root_id: String,
    pub original_root_id: String,
    pub created_ms: u64,
    pub state: String,
    pub total_files: u64,
    pub total_bytes: u64,
    pub can_restore: bool,
    pub revision: Option<String>,
    pub warning: Option<String>,
}
fn restore_revision(record: &Record) -> Result<String> {
    Ok(format!(
        "restore:{}",
        hash(&serde_json::to_vec(&record.journal).map_err(|e| e.to_string())?)
    ))
}
fn restore_checks(
    root: &Dir,
    store: &Dir,
    operation: &Dir,
    record: &Record,
    project: &Path,
    root_id: &str,
) -> Result<()> {
    if record.journal.root_id != root_id {
        return Err("此删除记录属于原游戏目录标识，请重新关联原游戏目录后恢复".into());
    }
    if record.journal.state != State::Deleted || !record.journal.marker_cleared {
        return Err(READY.into());
    }
    let versions = bind(root, store, operation, &record.journal, project)?;
    if versions.stat(&record.journal.id)?.is_some() {
        return Err("原实例目录已被占用，恢复副本已保留，请先移开冲突目录".into());
    }
    verify_tree(&operation.child("instance")?, &record.journal)
}
/// Malformed journals fail the whole read rather than presenting an incomplete
/// list as authoritative. Known records remain visible with precise warnings
/// when their source/recovery paths conflict or contents have changed.
pub fn history(root: &Path, project: &Path, root_id: &str) -> Result<Vec<DeleteHistoryEntry>> {
    scope_ok(root_id)?;
    let path = canonical(root)?;
    let project = canonical(project)?;
    let root = Dir::open(&path)?;
    let Some(store) = storage(&root, false)? else {
        return Ok(vec![]);
    };
    let _lock = lock(&store)?;
    let mut output = Vec::new();
    visit_records(&root, &store, |operation, record| {
        let j = &record.journal;
        if j.root != path {
            return Err("实例删除历史范围不匹配".into());
        }
        if matches!(j.state, State::Restored | State::RolledBack) {
            return Ok(());
        }
        let check = restore_checks(&root, &store, operation, &record, &project, root_id);
        let can_restore = check.is_ok();
        output.push(DeleteHistoryEntry {
            operation_id: j.operation_id.clone(),
            id: j.id.clone(),
            root_id: root_id.into(),
            original_root_id: j.root_id.clone(),
            created_ms: j.created_ms,
            state: j.state.label().into(),
            total_files: j.tree.files,
            total_bytes: j.tree.bytes,
            can_restore,
            revision: if can_restore {
                Some(restore_revision(&record)?)
            } else {
                None
            },
            warning: check.err(),
        });
        Ok(())
    })?;
    output.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.operation_id.cmp(&b.operation_id))
    });
    Ok(output)
}
pub fn undo(
    root: &Path,
    project: &Path,
    root_id: &str,
    operation_id: &str,
    expected_revision: &str,
    cancel: &AtomicBool,
    report: impl Fn(DeleteProgress),
) -> Result<Value> {
    filesystem::require_mutation_support()?;
    instance_rename_refs::with_settings_lock(project, || {
        undo_locked(
            root,
            project,
            root_id,
            operation_id,
            expected_revision,
            cancel,
            report,
        )
    })
}
fn undo_locked(
    root: &Path,
    project: &Path,
    root_id: &str,
    operation_id: &str,
    expected_revision: &str,
    cancel: &AtomicBool,
    report: impl Fn(DeleteProgress),
) -> Result<Value> {
    cancelled(Some(cancel))?;
    scope_ok(root_id)?;
    if !operation_ok(operation_id) {
        return Err("实例删除记录标识无效".into());
    }
    ensure_ready(root)?;
    ensure_project_ready(project)?;
    ensure_rename_ready(project)?;
    let path = canonical(root)?;
    let project = canonical(project)?;
    let root = Dir::open(&path)?;
    let store = storage(&root, false)?.ok_or("实例删除记录不存在")?;
    let _lock = lock(&store)?;
    let operation = store.child(operation_id)?;
    let mut record = read_record(&operation, operation_id)?;
    progress(
        &report,
        "checking",
        "检查恢复副本与原目录",
        0,
        record.journal.tree.files,
    );
    restore_checks(&root, &store, &operation, &record, &project, root_id)?;
    if restore_revision(&record)? != expected_revision {
        return Err("实例删除记录已变化，请重新检查后恢复".into());
    }
    cancelled(Some(cancel))?;
    record.journal.state = State::Restoring;
    record.journal.marker_cleared = false;
    write_record(&operation, &mut record)?;
    let mut committing = false;
    let result: Result<Value> = (|| {
        create_marker(&record.journal)?;
        cancelled(Some(cancel))?;
        progress(
            &report,
            "committing",
            "恢复原实例目录",
            0,
            record.journal.tree.files,
        );
        cancelled(Some(cancel))?;
        committing = true;
        let versions = bind(&root, &store, &operation, &record.journal, &project)?;
        verify_tree(&operation.child("instance")?, &record.journal)?;
        rename_new(&operation, "instance", &versions, &record.journal.id)?;
        bind(&root, &store, &operation, &record.journal, &project)?;
        verify_tree(&versions.child(&record.journal.id)?, &record.journal)?;
        record.journal.state = State::Restored;
        write_record(&operation, &mut record)?;
        clear_marker(&operation, &mut record)?;
        progress(
            &report,
            "complete",
            "实例目录与原资料已恢复",
            record.journal.tree.files,
            record.journal.tree.files,
        );
        Ok(json!({"id":record.journal.id,"operation_id":operation_id,"restored":true}))
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if record.journal.state == State::Restoring {
                if let Err(cleanup) = rollback_restore(&root, &store, &operation, &mut record) {
                    return Err(format!("取消清理失败：{cleanup}；原错误：{error}"));
                }
            }
            if committing && error != CANCELLED && !error.starts_with("取消清理失败：") {
                Err(format!("取消清理失败：实例恢复未完成：{error}"))
            } else {
                Err(error)
            }
        }
    }
}
pub fn recover_pending(root: &Path, project: &Path) -> Result<Value> {
    #[cfg(windows)]
    {
        // An empty read-only probe needs no journal update or settings lock.
        // Use the same literal-directory and validated marker readers as real
        // recovery; missing, malformed and inaccessible state stay distinct.
        let path = canonical(root)?;
        let project_path = canonical(project)?;
        ensure_rename_ready(&project_path)?;
        let directory = Dir::open(&path)?;
        let store = storage(&directory, false)?;
        let marker = pending_root(&project_path)?;
        if store.is_none() && marker.is_none() {
            return Ok(json!({"recovered": 0}));
        }
    }
    filesystem::require_mutation_support()?;
    instance_rename_refs::with_settings_lock(project, || recover_locked(root, project))
}
fn recover_locked(root: &Path, project: &Path) -> Result<Value> {
    ensure_rename_ready(project)?;
    let path = canonical(root)?;
    let project = canonical(project)?;
    let root = Dir::open(&path)?;
    let Some(store) = storage(&root, false)? else {
        if pending_root(&project)?.as_deref() == Some(path.as_path()) {
            return Err("实例删除待恢复标记缺少对应记录，标记已保留".into());
        }
        return Ok(json!({"recovered":0}));
    };
    let _lock = lock(&store)?;
    let mut recovered = 0usize;
    // Empty directories are the only possible unregistered setup artifact:
    // initial journal publication is anonymous+atomic, and no game move can
    // happen before that publication. Preserve every nonempty unknown slot.
    for name in store.names()? {
        if name == ".lock" {
            continue;
        }
        if !operation_ok(&name) {
            return Err("实例恢复区含未知记录，已保留".into());
        }
        let operation = store.child(&name)?;
        if operation.stat("journal.json")?.is_none() {
            if !operation.names()?.is_empty() {
                return Err("未登记的实例恢复目录含文件，已保留".into());
            }
            if store.child(&name)?.key() != operation.key()
                || Dir::open(&path)?.key() != root.key()
                || storage(&root, false)?.ok_or("实例恢复区消失")?.key() != store.key()
            {
                return Err("空实例恢复记录在清理前已被替换，已保留".into());
            }
            store.remove_empty(&name, &operation.key()?)?;
            recovered += 1;
            continue;
        }
        cleanup_journal_temps(&root, &store, &operation, &name, &project)?;
        let mut record = read_record(&operation, &name)?;
        bind(&root, &store, &operation, &record.journal, &project)?;
        let pending = !record.journal.state.terminal() || !record.journal.marker_cleared;
        match record.journal.state {
            State::Prepared => rollback_delete(&root, &store, &operation, &mut record)?,
            State::Restoring => rollback_restore(&root, &store, &operation, &mut record)?,
            State::Deleted => {
                verify_tree(&operation.child("instance")?, &record.journal)?;
                if root.child("versions")?.stat(&record.journal.id)?.is_some() {
                    return Err("原实例目录已被占用，恢复副本与冲突目录均已保留".into());
                }
                if !record.journal.marker_cleared {
                    clear_marker(&operation, &mut record)?
                }
            }
            State::Restored | State::RolledBack => {
                if !record.journal.marker_cleared {
                    if operation.stat("instance")?.is_some() {
                        return Err("终结实例恢复记录仍含恢复副本，已保留".into());
                    }
                    verify_tree(
                        &root.child("versions")?.child(&record.journal.id)?,
                        &record.journal,
                    )?;
                    clear_marker(&operation, &mut record)?
                }
            }
        }
        if pending {
            recovered += 1
        }
    }
    ensure_ready(&path)?;
    if pending_root(&project)?.as_deref() == Some(path.as_path()) {
        return Err("实例删除待恢复标记缺少对应可恢复记录，标记已保留".into());
    }
    Ok(json!({"recovered":recovered,"message":"未完成的实例删除或恢复已安全回滚"}))
}

#[cfg(test)]
mod tests;
