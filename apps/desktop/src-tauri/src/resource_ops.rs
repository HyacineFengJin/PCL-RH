//! Local resource mutations. The caller serializes writers and closes its
//! cancellation window in `commit`; this module does not depend on Tauri.
use pcl_core::content_scope::IsolationPolicy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::fs;

#[path = "resource_ops/update_batch.rs"]
pub(crate) mod update_batch;
#[path = "resource_ops/verified_batch.rs"]
mod verified_batch;
pub use update_batch::{Replacement, UpdateHistory};
pub(crate) use verified_batch::preflight;
pub(crate) use verified_batch::retire_sources;
pub use verified_batch::VerifiedImport;

/// One recoverable transaction for all downloaded resource kinds. Inputs remain
/// anonymous; the caller closes cancellation and rechecks its captured plan in commit.
#[cfg(test)]
pub fn import_verified_batch(
    root: &Path,
    id: &str,
    files: &mut [VerifiedImport],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult, String> {
    import_verified_batch_with_isolation(
        root,
        id,
        files,
        cancel,
        commit,
        progress,
        IsolationPolicy::Automatic,
    )
}

pub fn import_verified_batch_with_isolation(
    root: &Path,
    id: &str,
    files: &mut [VerifiedImport],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
    policy: IsolationPolicy,
) -> Result<MutationResult, String> {
    verified_batch::import_verified_batch_with_isolation(
        root, id, files, cancel, commit, progress, policy,
    )
}
pub fn ensure_verified_batches_ready(root: &Path) -> Result<(), String> {
    verified_batch::ensure_ready(root)?;
    ensure_updates_ready(root)
}
pub fn recover_verified_batches(root: &Path) -> Result<MutationResult, String> {
    // Recover older import batches first; updates then recheck their own root
    // marker and physical instance bindings under the same writer admission.
    let imported = verified_batch::recover_root(root)?;
    let updated = recover_updates(root)?;
    Ok(MutationResult {
        changed: imported.changed + updated.changed,
        undo_id: None,
        message: format!(
            "已恢复 {} 个资源下载、更新或撤销事务",
            imported.changed + updated.changed
        ),
    })
}

/// Replace verified anonymous downloads and preserve their original inodes for undo.
#[cfg(test)]
pub fn update_verified_batch(
    root: &Path,
    id: &str,
    files: &mut [VerifiedImport],
    replacements: &[Replacement],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult, String> {
    update_verified_batch_with_isolation(
        root,
        id,
        files,
        replacements,
        cancel,
        commit,
        progress,
        IsolationPolicy::Automatic,
    )
}

pub fn update_verified_batch_with_isolation(
    root: &Path,
    id: &str,
    files: &mut [VerifiedImport],
    replacements: &[Replacement],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
    policy: IsolationPolicy,
) -> Result<MutationResult, String> {
    update_batch::update_verified_batch_with_isolation(
        root,
        id,
        files,
        replacements,
        cancel,
        commit,
        progress,
        policy,
    )
}
pub fn updates_history(root: &Path, id: &str) -> Result<Vec<UpdateHistory>, String> {
    update_batch::updates_history(root, id)
}
pub fn restore_update(
    root: &Path,
    id: &str,
    undo_id: &str,
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
) -> Result<MutationResult, String> {
    update_batch::restore_update(root, id, undo_id, cancel, commit)
}
pub fn ensure_updates_ready(root: &Path) -> Result<(), String> {
    update_batch::ensure_ready(root)
}
pub fn recover_updates(root: &Path) -> Result<MutationResult, String> {
    update_batch::recover_root(root)
}

const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_BATCH_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 100_000;
// Includes JSON escaping of all permitted 255-byte names at MAX_FILES. The
// writer and reader share this limit so every successful mutation stays usable.
const MAX_JOURNAL_BYTES: usize = 2 * 1024 * 1024;
const JOURNAL_VERSION: u32 = if cfg!(windows) { 2 } else { 1 };
const STAGE_PREFIX: &str = ".pcl-resource-stage-";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
thread_local! {static JOURNAL_WRITE_FAILURES: std::cell::Cell<usize> = const {std::cell::Cell::new(0)};}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceFile {
    pub file_name: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MutationResult {
    pub changed: usize,
    pub undo_id: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RemovedOperation {
    pub id: String,
    pub files: Vec<String>,
    pub created_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Enable,
    Disable,
    Import,
    Remove,
    Restore,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Prepared,
    Committed,
    Finished,
    Restored,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    source_name: String,
    target_name: String,
    stage_name: String,
    fingerprint: String,
    content_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    operation_id: String,
    instance_id: String,
    kind: String,
    resource_relative: String,
    action: Action,
    undo_id: Option<String>,
    created_at: u64,
    state: JournalState,
    items: Vec<Item>,
}

#[cfg(target_os = "linux")]
#[path = "resource_ops/filesystem_linux.rs"]
mod filesystem;
#[cfg(not(target_os = "linux"))]
#[path = "resource_ops/filesystem_native.rs"]
mod filesystem;
#[path = "resource_ops/token.rs"]
pub(crate) mod token;
use filesystem::{
    check_copy_cleanup, complete_copy, copy_guard, create_copy, file_evidence, journal_bytes,
    open_source_parent, publish_journal, rename_new, source_identity, Dir, FileEvidence,
};
pub(crate) use token::{same_identity, token_for, valid_token};

pub fn fingerprint(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("文件名无效")?;
    let parent = path.parent().ok_or("文件路径无效")?;
    let dir = Dir::open(parent)?;
    dir.token(name)
}

fn safe_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        return Err("文件名必须是单个有效文件名".into());
    }
    #[cfg(windows)]
    pcl_core::retained_fs::Leaf::new(name).map_err(|e| e.to_string())?;
    Ok(())
}

fn valid_id(value: &str) -> bool {
    value.starts_with('r')
        && value.len() >= 12
        && value.len() <= 80
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn valid_operation_id(value: &str) -> bool {
    valid_id(value)
}

pub(crate) fn rename_journal_bytes(
    bytes: &[u8],
    operation_id: &str,
    old: &str,
    new: &str,
) -> Result<Option<Vec<u8>>, String> {
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err("资源恢复记录过大".into());
    }
    let mut journal: Journal = serde_json::from_slice(bytes)
        .map_err(|_| "资源恢复记录无法读取，已保留原文件".to_string())?;
    validate_journal(&journal, operation_id)?;
    if journal.state == JournalState::Prepared
        || (journal.state == JournalState::Committed && journal.action != Action::Remove)
    {
        return Err("存在未完成的资源操作，请先在资源管理中恢复后再重命名".into());
    }
    if journal.instance_id == new && journal.resource_relative.starts_with("versions/") {
        return Err("目标名称已有历史资源记录，请选择其他名称".into());
    }
    if journal.instance_id != old {
        return Ok(None);
    }
    journal.instance_id = new.into();
    if journal.resource_relative != journal.kind {
        journal.resource_relative = format!("versions/{new}/{}", journal.kind);
    }
    validate_journal(&journal, operation_id)?;
    let result = serde_json::to_vec(&journal).map_err(|error| error.to_string())?;
    if result.len() > MAX_JOURNAL_BYTES {
        return Err("资源恢复记录过大".into());
    }
    Ok(Some(result))
}

pub(crate) fn is_stage_entry(name: &str) -> bool {
    name.strip_prefix(STAGE_PREFIX).is_some_and(valid_id)
}

fn kind_ok(kind: &str) -> Result<(), String> {
    match kind {
        "mods" | "resourcepacks" | "shaderpacks" => Ok(()),
        _ => Err("该资源类型暂不支持本地文件操作".into()),
    }
}

fn resource_name(kind: &str, name: &str) -> Result<(), String> {
    safe_name(name)?;
    let valid = match kind {
        "mods" => name.ends_with(".jar") || name.ends_with(".jar.disabled"),
        "resourcepacks" | "shaderpacks" => name.ends_with(".zip"),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(format!("不支持此资源文件：{name}"))
    }
}

fn cancellation(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("操作已取消".into())
    } else {
        Ok(())
    }
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "r{nanos:032x}{:08x}{:016x}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn relative_string(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::Normal(part) => {
                let part = part.to_str().ok_or("目录名无效")?;
                safe_name(part)?;
                parts.push(part);
            }
            _ => return Err("资源目录范围无效".into()),
        }
    }
    if parts.is_empty() {
        return Err("资源目录范围无效".into());
    }
    Ok(parts.join("/"))
}

fn descend(root: &Dir, relative: &str, create_last: bool) -> Result<Dir, String> {
    let parts: Vec<_> = relative.split('/').collect();
    let mut current = None;
    for (index, part) in parts.iter().enumerate() {
        safe_name(part)?;
        let parent = current.as_ref().unwrap_or(root);
        current = Some(if create_last && index + 1 == parts.len() {
            parent.ensure_child(part)?
        } else {
            parent.child(part)?
        });
    }
    current.ok_or("资源目录范围无效".into())
}

struct Context {
    _history_lock: Option<crate::instance_rename_refs::ReferenceLock>,
    root_path: PathBuf,
    root: Dir,
    resources: Dir,
    storage: Option<Dir>,
    relative: String,
    id: String,
    kind: String,
    policy: Option<IsolationPolicy>,
}

impl Context {
    #[cfg(test)]
    fn new(root: &Path, id: &str, kind: &str, write: bool) -> Result<Self, String> {
        Self::new_with_isolation(root, id, kind, write, IsolationPolicy::Automatic)
    }
    fn new_with_isolation(
        root: &Path,
        id: &str,
        kind: &str,
        write: bool,
        policy: IsolationPolicy,
    ) -> Result<Self, String> {
        kind_ok(kind)?;
        safe_name(id)?;
        let root_path = root.canonicalize().map_err(|e| e.to_string())?;
        let history_lock = write
            .then(|| crate::instance_rename_refs::root_history_lock(&root_path))
            .transpose()?;
        if write {
            ensure_verified_batches_ready(&root_path)?;
        }
        let path = crate::ui_data::resource_dir_with_isolation(&root_path, id, kind, policy)?;
        let relative = relative_string(
            path.strip_prefix(&root_path)
                .map_err(|_| "目录超出游戏目录".to_string())?,
        )?;
        let root = Dir::open(&root_path)?;
        let resources = descend(&root, &relative, write)?;
        let storage = if write {
            Some(
                root.ensure_child(".pcl-linux")?
                    .ensure_child("resource-operations")?,
            )
        } else {
            match root.optional_child(".pcl-linux")? {
                Some(parent) => parent.optional_child("resource-operations")?,
                None => None,
            }
        };
        Ok(Self {
            _history_lock: history_lock,
            root_path,
            root,
            resources,
            storage,
            relative,
            id: id.to_owned(),
            kind: kind.to_owned(),
            policy: Some(policy),
        })
    }

    fn check_scope(&self) -> Result<(), String> {
        if let Some(policy) = self.policy {
            let scope =
                pcl_core::content_scope::inspect_with_isolation(&self.root_path, &self.id, policy)?;
            let path = self
                .root_path
                .join(scope.relative_base(&self.id)?)
                .join(&self.kind);
            if path != self.root_path.join(&self.relative) {
                return Err("实例隔离目录已变化，请刷新后重试".into());
            }
        }
        self.check()
    }

    // Rollback and recovery use only captured physical directory evidence;
    // selection changes must never block restoration into the recorded base.
    fn check(&self) -> Result<(), String> {
        if !self.root.same(&Dir::open(&self.root_path)?)?
            || !self
                .resources
                .same(&descend(&self.root, &self.relative, false)?)?
        {
            return Err("资源目录已变化，请重新读取列表".into());
        }
        if let Some(storage) = &self.storage {
            let current = self
                .root
                .child(".pcl-linux")?
                .child("resource-operations")?;
            if !storage.same(&current)? {
                return Err("资源操作目录已变化".into());
            }
        }
        Ok(())
    }

    fn storage(&self) -> Result<&Dir, String> {
        self.storage.as_ref().ok_or("没有可恢复的文件记录".into())
    }

    fn operation(&self, id: &str) -> Result<Dir, String> {
        if !valid_id(id) {
            return Err("恢复记录无效".into());
        }
        self.storage()?.child(id)
    }

    fn new_journal(&self, action: Action, undo_id: Option<String>, items: Vec<Item>) -> Journal {
        Journal {
            version: JOURNAL_VERSION,
            operation_id: new_id(),
            instance_id: self.id.clone(),
            kind: self.kind.clone(),
            resource_relative: self.relative.clone(),
            action,
            undo_id,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            state: JournalState::Prepared,
            items,
        }
    }
}

fn stage_name(journal: &Journal) -> String {
    format!("{STAGE_PREFIX}{}", journal.operation_id)
}

fn check_count(count: usize) -> Result<(), String> {
    if count == 0 || count > MAX_FILES {
        return Err(format!("一次请选择 1 至 {MAX_FILES} 个文件"));
    }
    Ok(())
}

fn verify(dir: &Dir, name: &str, expected: &str) -> Result<(), String> {
    if dir.token(name)? != expected {
        Err(format!("文件已变化，请重新读取列表：{name}"))
    } else {
        Ok(())
    }
}

fn verify_owned(dir: &Dir, name: &str, expected: &str) -> Result<(), String> {
    if same_identity(&dir.token(name)?, expected) {
        Ok(())
    } else {
        Err(format!("待恢复文件已变化，已保留文件：{name}"))
    }
}

fn content_hash(file: &mut File) -> Result<String, String> {
    content_hash_cancellable(file, None)
}

fn content_hash_cancellable(
    file: &mut File,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE_BYTES {
        return Err("文件超过操作大小限制".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    let mut total = 0u64;
    loop {
        if let Some(cancel) = cancel {
            cancellation(cancel)?;
        }
        let length = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if length == 0 {
            break;
        }
        total = total.checked_add(length as u64).ok_or("文件过大")?;
        if total > MAX_FILE_BYTES {
            return Err("文件超过操作大小限制".into());
        }
        hasher.update(&buffer[..length]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn checked_owned_file(dir: &Dir, name: &str, item: &Item) -> Result<(File, FileEvidence), String> {
    let mut file = dir.regular_file(name)?;
    let before = token_for(&file)?;
    let evidence = file_evidence(&file)?;
    if !same_identity(&before, &item.fingerprint) {
        return Err(format!("待恢复文件已变化，已保留文件：{name}"));
    }
    if let Some(expected) = &item.content_hash {
        if content_hash(&mut file)? != *expected || token_for(&file)? != before {
            return Err(format!("待恢复文件内容已变化，已保留文件：{name}"));
        }
        // A path replacement during hashing must never cause unlink of that
        // replacement. The open descriptor and directory entry must still agree.
        verify(dir, name, &before)?;
    }
    Ok((file, evidence))
}

fn verify_item_owned(dir: &Dir, name: &str, item: &Item) -> Result<(), String> {
    checked_owned_file(dir, name, item).map(drop)
}
fn remove_item_owned(dir: &Dir, name: &str, item: &Item) -> Result<(), String> {
    let (file, evidence) = checked_owned_file(dir, name, item)?;
    dir.remove_file(name, &file, &evidence)
}

fn validate_journal(journal: &Journal, name: &str) -> Result<(), String> {
    let invalid = || "资源恢复记录无效，已保留原文件".to_string();
    if journal.version != JOURNAL_VERSION
        || journal.operation_id != name
        || !valid_id(name)
        || safe_name(&journal.instance_id).is_err()
        || kind_ok(&journal.kind).is_err()
        || check_count(journal.items.len()).is_err()
        || journal.created_at > 253_402_300_799
    {
        return Err(invalid());
    }
    let shared = journal.kind.clone();
    let isolated = format!("versions/{}/{}", journal.instance_id, journal.kind);
    if journal.resource_relative != shared && journal.resource_relative != isolated {
        return Err(invalid());
    }
    if (journal.action == Action::Restore) != journal.undo_id.is_some()
        || journal
            .undo_id
            .as_deref()
            .is_some_and(|id| !valid_id(id) || id == name)
        || (journal.state == JournalState::Restored && journal.action != Action::Remove)
    {
        return Err(invalid());
    }
    let mut sources = BTreeSet::new();
    let mut targets = BTreeSet::new();
    let mut total_bytes = 0u64;
    for (index, item) in journal.items.iter().enumerate() {
        resource_name(&journal.kind, &item.source_name).map_err(|_| invalid())?;
        resource_name(&journal.kind, &item.target_name).map_err(|_| invalid())?;
        if !sources.insert(&item.source_name)
            || !targets.insert(&item.target_name)
            || item.stage_name != format!("item-{index:04}")
            || !valid_token(&item.fingerprint)
            || matches!(
                journal.action,
                Action::Import | Action::Remove | Action::Restore
            ) != item.content_hash.is_some()
            || item.content_hash.as_ref().is_some_and(|hash| {
                hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err(invalid());
        }
        let size = item
            .fingerprint
            .split(':')
            .nth(1)
            .and_then(|size| size.parse::<u64>().ok())
            .ok_or_else(invalid)?;
        total_bytes = total_bytes.checked_add(size).ok_or_else(invalid)?;
        if size > MAX_FILE_BYTES || total_bytes > MAX_BATCH_BYTES {
            return Err(invalid());
        }
        let target = match journal.action {
            Action::Enable => item.source_name.strip_suffix(".disabled"),
            Action::Disable => item
                .source_name
                .ends_with(".jar")
                .then_some(item.target_name.strip_suffix(".disabled"))
                .flatten()
                .filter(|name| *name == item.source_name)
                .map(|_| item.target_name.as_str()),
            _ => Some(item.source_name.as_str()),
        };
        if target != Some(item.target_name.as_str())
            || (matches!(journal.action, Action::Enable | Action::Disable)
                && journal.kind != "mods")
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn read_journal_optional(dir: &Dir, name: &str) -> Result<Option<Journal>, String> {
    let Some(data) = journal_bytes(dir)? else {
        return Ok(None);
    };
    let journal: Journal = serde_json::from_slice(&data)
        .map_err(|_| "资源恢复记录无法读取，已保留原文件".to_string())?;
    validate_journal(&journal, name)?;
    Ok(Some(journal))
}
fn read_journal(dir: &Dir, name: &str) -> Result<Journal, String> {
    read_journal_optional(dir, name)?.ok_or("资源恢复记录缺失".into())
}

fn write_journal(dir: &Dir, journal: &Journal) -> Result<(), String> {
    validate_journal(journal, &journal.operation_id)?;
    #[cfg(test)]
    if JOURNAL_WRITE_FAILURES.with(|remaining| {
        let count = remaining.get();
        if count == 0 {
            false
        } else {
            remaining.set(count - 1);
            true
        }
    }) {
        return Err("injected resource journal write failure".into());
    }
    let data = serde_json::to_vec(journal).map_err(|e| e.to_string())?;
    publish_journal(dir, &data)
}

fn operation_dirs(context: &Context, journal: &Journal) -> Result<(Dir, Dir), String> {
    let source = match journal.action {
        Action::Restore => context
            .operation(journal.undo_id.as_deref().ok_or("恢复记录无效")?)?
            .child("files")?,
        _ => descend(&context.root, &context.relative, false)?,
    };
    let target = match journal.action {
        Action::Remove => context.operation(&journal.operation_id)?.child("files")?,
        _ => descend(&context.root, &context.relative, false)?,
    };
    Ok((source, target))
}

fn check_operation_dirs(
    context: &Context,
    journal: &Journal,
    stage: Option<&Dir>,
    source: &Dir,
    target: &Dir,
) -> Result<(), String> {
    context.check()?;
    let (current_source, current_target) = operation_dirs(context, journal)?;
    if !source.same(&current_source)? || !target.same(&current_target)? {
        return Err("操作目录已变化，已保留文件".into());
    }
    if let Some(stage) = stage {
        if !stage.same(&context.resources.child(&stage_name(journal))?)? {
            return Err("暂存目录已变化，已保留文件".into());
        }
    }
    Ok(())
}

fn ensure_stage_contents(stage: &Dir, journal: &Journal) -> Result<(), String> {
    let allowed: BTreeSet<_> = journal.items.iter().map(|item| &item.stage_name).collect();
    if stage.names()?.iter().any(|name| !allowed.contains(name)) {
        return Err("暂存目录含有其他文件，已保留全部文件".into());
    }
    Ok(())
}

fn rollback(context: &Context, journal: &Journal, stage: Option<&Dir>) -> Result<(), String> {
    context.check()?;
    let (source, target) = operation_dirs(context, journal)?;
    check_operation_dirs(context, journal, stage, &source, &target)?;
    if let Some(stage) = stage {
        ensure_stage_contents(stage, journal)?;
    }
    // Inspect the whole batch before recovering any file. Any ambiguous slot is
    // left untouched, including an unrelated same-name replacement.
    enum Slot {
        Original,
        Stage,
        Target,
        MissingImport,
    }
    let mut slots = Vec::new();
    for item in &journal.items {
        let original =
            journal.action != Action::Import && source.metadata(&item.source_name)?.is_some();
        let original_owned = original
            && source
                .token(&item.source_name)
                .is_ok_and(|token| same_identity(&token, &item.fingerprint));
        let staged = stage
            .map(|dir| dir.metadata(&item.stage_name))
            .transpose()?
            .flatten()
            .is_some();
        let final_file = target.metadata(&item.target_name)?.is_some();
        let final_owned = final_file
            && target
                .token(&item.target_name)
                .is_ok_and(|token| same_identity(&token, &item.fingerprint));
        // For non-import operations an existing target can be unrelated even
        // when the selected source still exists; preserve it and only move our
        // file if its original slot is free.
        if original_owned {
            if staged || final_owned {
                return Err("恢复位置存在多个文件，已保留全部文件".into());
            }
            slots.push(Slot::Original);
        } else if original {
            if staged || final_owned {
                return Err("原文件位置已被其他文件占用，已保留待恢复文件".into());
            }
            // A source changed before we moved it. Nothing owned by this
            // transaction exists at the staging/target slots; keep that edit.
            slots.push(Slot::Original);
        } else if staged {
            verify_item_owned(stage.ok_or("暂存目录缺失")?, &item.stage_name, item)?;
            if final_owned {
                return Err("恢复位置存在多个文件，已保留全部文件".into());
            }
            if journal.action != Action::Import {
                source.absent(&item.source_name)?;
            }
            slots.push(Slot::Stage);
        } else if final_owned {
            verify_item_owned(&target, &item.target_name, item)?;
            if journal.action != Action::Import {
                source.absent(&item.source_name)?;
            }
            slots.push(Slot::Target);
        } else if journal.action == Action::Import && !final_file {
            slots.push(Slot::MissingImport);
        } else {
            return Err("待恢复文件缺失，已保留其余文件".into());
        }
    }
    for (item, slot) in journal.items.iter().zip(slots).rev() {
        check_operation_dirs(context, journal, stage, &source, &target)?;
        match slot {
            Slot::Stage if journal.action == Action::Import => {
                let stage = stage.ok_or("暂存目录缺失")?;
                remove_item_owned(stage, &item.stage_name, item)?;
            }
            Slot::Target if journal.action == Action::Import => {
                let stage = stage.ok_or("暂存目录缺失")?;
                verify_item_owned(&target, &item.target_name, item)?;
                rename_new(&target, &item.target_name, stage, &item.stage_name)?;
                remove_item_owned(stage, &item.stage_name, item)?;
            }
            Slot::Stage => {
                let stage = stage.ok_or("暂存目录缺失")?;
                verify_owned(stage, &item.stage_name, &item.fingerprint)?;
                rename_new(stage, &item.stage_name, &source, &item.source_name)?;
            }
            Slot::Target => {
                verify_owned(&target, &item.target_name, &item.fingerprint)?;
                rename_new(&target, &item.target_name, &source, &item.source_name)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn remove_stage(context: &Context, journal: &Journal, stage: Option<&Dir>) -> Result<(), String> {
    if let Some(stage) = stage {
        if !stage.names()?.is_empty() {
            return Err("暂存目录仍含有文件，已保留恢复记录".into());
        }
        context.check()?;
        let current = context.resources.child(&stage_name(journal))?;
        if !stage.same(&current)? {
            return Err("暂存目录已变化，已保留文件".into());
        }
        context
            .resources
            .remove_empty_dir(&stage_name(journal), stage)?;
    }
    Ok(())
}

fn finish_committed(
    context: &Context,
    journal: &mut Journal,
    operation: &Dir,
) -> Result<(), String> {
    context.check()?;
    if !operation.same(&context.operation(&journal.operation_id)?)? {
        return Err("资源恢复记录目录已变化".into());
    }
    if journal.action == Action::Restore {
        let undo_id = journal.undo_id.as_deref().ok_or("恢复记录无效")?;
        let undo_dir = context.operation(undo_id)?;
        let mut undo = read_journal(&undo_dir, undo_id)?;
        if undo.action != Action::Remove
            || undo.resource_relative != journal.resource_relative
            || undo.kind != journal.kind
            || undo.items.len() != journal.items.len()
            || undo.items.iter().zip(&journal.items).any(|(a, b)| {
                a.target_name != b.source_name
                    || !same_identity(&a.fingerprint, &b.fingerprint)
                    || a.content_hash != b.content_hash
            })
        {
            return Err("恢复记录内容不匹配，已保留文件".into());
        }
        if undo.state == JournalState::Committed {
            undo.state = JournalState::Restored;
            write_journal(&undo_dir, &undo)?;
        } else if undo.state != JournalState::Restored {
            return Err("恢复记录状态不匹配".into());
        }
    }
    let stage = context.resources.optional_child(&stage_name(journal))?;
    remove_stage(context, journal, stage.as_ref())?;
    if journal.action != Action::Remove {
        journal.state = JournalState::Finished;
        write_journal(operation, journal)?;
    }
    Ok(())
}

fn recover(context: &Context) -> Result<(), String> {
    context.check()?;
    let storage = context.storage()?;
    let mut journals = Vec::new();
    for name in storage.names()? {
        if !valid_id(&name) {
            return Err("资源操作目录含有未知记录，已保留文件".into());
        }
        let operation = storage.child(&name)?;
        // An interruption before the first journal publication can only leave
        // service-owned empty directories/copies, never moved user resources.
        let Some(journal) = read_journal_optional(&operation, &name)? else {
            continue;
        };
        if journal.resource_relative == context.relative && journal.kind == context.kind {
            journals.push((operation, journal));
        }
    }
    // A committed restore must mark its removal record before a later restore
    // can consume that same undo ID. Pending records always roll back.
    journals.sort_by_key(|(_, journal)| {
        (
            journal.state != JournalState::Committed || journal.action != Action::Restore,
            journal.created_at,
            journal.operation_id.clone(),
        )
    });
    for (operation, mut journal) in journals {
        match journal.state {
            JournalState::Prepared => {
                let stage = context.resources.optional_child(&stage_name(&journal))?;
                rollback(context, &journal, stage.as_ref())?;
                remove_stage(context, &journal, stage.as_ref())?;
                journal.state = JournalState::Finished;
                write_journal(&operation, &journal)?;
            }
            JournalState::Committed => {
                finish_committed(context, &mut journal, &operation)?;
            }
            JournalState::Finished | JournalState::Restored => {}
        }
    }
    Ok(())
}

fn checked_selection(context: &Context, files: &[ResourceFile]) -> Result<Vec<Item>, String> {
    check_count(files.len())?;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    let mut total = 0u64;
    for file in files {
        resource_name(&context.kind, &file.file_name)?;
        if !seen.insert(&file.file_name) || !valid_token(&file.fingerprint) {
            return Err("所选文件重复或文件标识无效".into());
        }
        verify(&context.resources, &file.file_name, &file.fingerprint)?;
        let size = context
            .resources
            .regular_file(&file.file_name)?
            .metadata()
            .map_err(|e| e.to_string())?
            .len();
        total = total.checked_add(size).ok_or("文件总大小过大")?;
        if size > MAX_FILE_BYTES || total > MAX_BATCH_BYTES {
            return Err("所选文件超过操作大小限制".into());
        }
        items.push(Item {
            source_name: file.file_name.clone(),
            target_name: file.file_name.clone(),
            stage_name: format!("item-{:04}", items.len()),
            fingerprint: file.fingerprint.clone(),
            content_hash: None,
        });
    }
    Ok(items)
}

fn transaction(
    context: &Context,
    mut journal: Journal,
    operation: Dir,
    stage: Dir,
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult, String> {
    let (source, target) = operation_dirs(context, &journal)?;
    let outcome = (|| {
        context.check_scope()?;
        cancellation(cancel)?;
        let verify_batch = || -> Result<(), String> {
            context.check_scope()?;
            check_operation_dirs(context, &journal, Some(&stage), &source, &target)?;
            if !operation.same(&context.operation(&journal.operation_id)?)? {
                return Err("资源恢复记录目录已变化".into());
            }
            for item in &journal.items {
                if journal.action == Action::Import {
                    verify(&stage, &item.stage_name, &item.fingerprint)?;
                } else {
                    verify(&source, &item.source_name, &item.fingerprint)?;
                    stage.absent(&item.stage_name)?;
                }
                target.absent(&item.target_name)?;
            }
            Ok(())
        };
        verify_batch()?;
        commit()?;
        // The final admission check may run user code. Check every token and
        // destination again before moving the first selected resource.
        verify_batch()?;
        if journal.action != Action::Import {
            for item in &journal.items {
                check_operation_dirs(context, &journal, Some(&stage), &source, &target)?;
                verify(&source, &item.source_name, &item.fingerprint)?;
                rename_new(&source, &item.source_name, &stage, &item.stage_name)?;
            }
        }
        for (index, item) in journal.items.iter().enumerate() {
            check_operation_dirs(context, &journal, Some(&stage), &source, &target)?;
            verify_owned(&stage, &item.stage_name, &item.fingerprint)?;
            rename_new(&stage, &item.stage_name, &target, &item.target_name)?;
            progress((index + 1) as u64, journal.items.len() as u64);
        }
        context.check_scope()?;
        if !operation.same(&context.operation(&journal.operation_id)?)? {
            return Err("资源恢复记录目录已变化".into());
        }
        for item in &mut journal.items {
            verify_owned(&target, &item.target_name, &item.fingerprint)?;
            item.fingerprint = target.token(&item.target_name)?;
        }
        journal.state = JournalState::Committed;
        if let Err(error) = write_journal(&operation, &journal) {
            // If publication happened but fsync failed, explicitly return to
            // Prepared before attempting rollback; recovery must see our intent.
            journal.state = JournalState::Prepared;
            if let Err(repair) = write_journal(&operation, &journal) {
                // Neither error proves the durable logical state. Keep every
                // moved file and let reopening select Prepared rollback or
                // Committed completion from the actual journal, rather than
                // rolling back beneath a possibly durable commit.
                journal.state = JournalState::Committed;
                return Err(format!("{error}；无法保存回滚记录：{repair}"));
            }
            return Err(error);
        }
        Ok(())
    })();
    if let Err(error) = outcome {
        if journal.state == JournalState::Committed {
            return Err(format!("{error}；文件与恢复记录已保留"));
        }
        if let Err(recovery) = rollback(context, &journal, Some(&stage)) {
            return Err(format!("{error}；尚有文件待恢复：{recovery}"));
        }
        remove_stage(context, &journal, Some(&stage))?;
        journal.state = JournalState::Finished;
        write_journal(&operation, &journal)?;
        return Err(error);
    }
    // A published commit is already successful. A cleanup error retains its
    // durable record for the next admitted writer and does not misreport a
    // successful user mutation as a failed/rolled-back operation.
    let _ = finish_committed(context, &mut journal, &operation);
    Ok(MutationResult {
        changed: journal.items.len(),
        undo_id: (journal.action == Action::Remove).then(|| journal.operation_id.clone()),
        message: match journal.action {
            Action::Enable => format!("已启用 {} 个模组", journal.items.len()),
            Action::Disable => format!("已禁用 {} 个模组", journal.items.len()),
            Action::Import => format!("已导入 {} 个文件", journal.items.len()),
            Action::Remove => format!("已移除 {} 个文件，可恢复", journal.items.len()),
            Action::Restore => format!("已恢复 {} 个文件", journal.items.len()),
        },
    })
}

fn prepare(context: &Context, journal: &Journal) -> Result<(Dir, Dir), String> {
    context.check()?;
    let operation = context.storage()?.create_child(&journal.operation_id)?;
    operation.create_child("files")?;
    let stage = context.resources.create_child(&stage_name(journal))?;
    write_journal(&operation, journal)?;
    Ok((operation, stage))
}

#[cfg(test)]
pub fn set_enabled(
    root: &Path,
    id: &str,
    kind: &str,
    files: &[ResourceFile],
    enabled: bool,
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult, String> {
    set_enabled_with_isolation(
        root,
        id,
        kind,
        files,
        enabled,
        cancel,
        commit,
        progress,
        IsolationPolicy::Automatic,
    )
}

pub fn set_enabled_with_isolation(
    root: &Path,
    id: &str,
    kind: &str,
    files: &[ResourceFile],
    enabled: bool,
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
    policy: IsolationPolicy,
) -> Result<MutationResult, String> {
    if kind != "mods" {
        return Err("只有模组可以启用或禁用".into());
    }
    cancellation(cancel)?;
    let context = Context::new_with_isolation(root, id, kind, true, policy)?;
    recover(&context)?;
    let mut items = checked_selection(&context, files)?;
    items.retain(|item| item.source_name.ends_with(".disabled") == enabled);
    if items.is_empty() {
        return Ok(MutationResult {
            changed: 0,
            undo_id: None,
            message: "所选模组状态无需更改".into(),
        });
    }
    let mut targets = BTreeSet::new();
    for (index, item) in items.iter_mut().enumerate() {
        item.stage_name = format!("item-{index:04}");
        item.target_name = if enabled {
            item.source_name
                .strip_suffix(".disabled")
                .ok_or("模组文件名无效")?
                .to_owned()
        } else {
            format!("{}.disabled", item.source_name)
        };
        resource_name(kind, &item.target_name)?;
        if !targets.insert(item.target_name.clone()) {
            return Err("所选文件的目标文件名重复".into());
        }
        context.resources.absent(&item.target_name)?;
    }
    let journal = context.new_journal(
        if enabled {
            Action::Enable
        } else {
            Action::Disable
        },
        None,
        items,
    );
    let (operation, stage) = prepare(&context, &journal)?;
    transaction(
        &context, journal, operation, stage, cancel, commit, progress,
    )
}

#[cfg(test)]
pub fn remove(
    root: &Path,
    id: &str,
    kind: &str,
    files: &[ResourceFile],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult, String> {
    remove_with_isolation(
        root,
        id,
        kind,
        files,
        cancel,
        commit,
        progress,
        IsolationPolicy::Automatic,
    )
}

pub fn remove_with_isolation(
    root: &Path,
    id: &str,
    kind: &str,
    files: &[ResourceFile],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
    policy: IsolationPolicy,
) -> Result<MutationResult, String> {
    cancellation(cancel)?;
    let context = Context::new_with_isolation(root, id, kind, true, policy)?;
    recover(&context)?;
    let mut items = checked_selection(&context, files)?;
    for item in &mut items {
        cancellation(cancel)?;
        let mut file = context.resources.regular_file(&item.source_name)?;
        item.content_hash = Some(content_hash_cancellable(&mut file, Some(cancel))?);
        if token_for(&file)? != item.fingerprint {
            return Err(format!("文件已变化，请重新读取列表：{}", item.source_name));
        }
        verify(&context.resources, &item.source_name, &item.fingerprint)?;
    }
    let journal = context.new_journal(Action::Remove, None, items);
    let (operation, stage) = prepare(&context, &journal)?;
    // Moving retained originals is deliberately never replaced by copy+unlink.
    // On a different mount renameat2 returns EXDEV and the batch rolls back.
    transaction(
        &context, journal, operation, stage, cancel, commit, progress,
    )
}

fn validate_zip(file: &mut File, cancel: &AtomicBool) -> Result<(), String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let mut archive =
        zip::ZipArchive::new(file).map_err(|_| "文件不是有效的 ZIP/JAR 归档".to_string())?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err("归档中的文件数量过多".into());
    }
    for index in 0..archive.len() {
        cancellation(cancel)?;
        // Validate local headers and compressed bounds without extracting or
        // expanding zip bombs. Paths inside the archive are never used on disk.
        let entry = archive
            .by_index_raw(index)
            .map_err(|_| "归档文件结构损坏".to_string())?;
        if entry
            .data_start()
            .checked_add(entry.compressed_size())
            .is_none_or(|end| end > length)
        {
            return Err("归档文件内容不完整".into());
        }
    }
    Ok(())
}

struct ImportSource {
    file: File,
    parent: Dir,
    name: String,
    token: String,
    size: u64,
}

#[cfg(test)]
pub fn import_files(
    root: &Path,
    id: &str,
    kind: &str,
    sources: &[PathBuf],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult, String> {
    import_files_with_isolation(
        root,
        id,
        kind,
        sources,
        cancel,
        commit,
        progress,
        IsolationPolicy::Automatic,
    )
}

pub fn import_files_with_isolation(
    root: &Path,
    id: &str,
    kind: &str,
    sources: &[PathBuf],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
    policy: IsolationPolicy,
) -> Result<MutationResult, String> {
    check_count(sources.len())?;
    cancellation(cancel)?;
    let context = Context::new_with_isolation(root, id, kind, true, policy)?;
    recover(&context)?;
    let mut imports = Vec::new();
    let mut names = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut total = 0u64;
    for path in sources {
        cancellation(cancel)?;
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("导入来源必须是完整的本地文件路径".into());
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("导入文件名无效")?
            .to_owned();
        resource_name(kind, &name)?;
        if !names.insert(name.clone()) {
            return Err("导入文件名重复".into());
        }
        context.resources.absent(&name)?;
        // The platform backend opens the absolute ancestor graph without links.
        let parent = open_source_parent(path.parent().ok_or("导入路径无效")?)?;
        let mut file = parent.regular_file(&name)?;
        let token = token_for(&file)?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !identities.insert(source_identity(&file)?) {
            return Err("导入来源重复".into());
        }
        let size = metadata.len();
        total = total.checked_add(size).ok_or("导入文件总大小过大")?;
        if size > MAX_FILE_BYTES || total > MAX_BATCH_BYTES {
            return Err("导入文件超过大小限制".into());
        }
        validate_zip(&mut file, cancel)?;
        if token_for(&file)? != token {
            return Err(format!("导入来源已变化：{name}"));
        }
        verify(&parent, &name, &token)?;
        imports.push(ImportSource {
            file,
            parent,
            name,
            token,
            size,
        });
    }
    // Staging copies have no authority to change resource files until every
    // copy is complete and its Prepared journal is durably published.
    let copy_guard = copy_guard(&context.root)?;
    let mut journal = context.new_journal(Action::Import, None, Vec::new());
    let operation = context.storage()?.create_child(&journal.operation_id)?;
    operation.create_child("files")?;
    let stage = context.resources.create_child(&stage_name(&journal))?;
    let copies = (|| {
        let mut done = 0u64;
        let mut buffer = [0u8; 128 * 1024];
        for (index, source) in imports.iter_mut().enumerate() {
            cancellation(cancel)?;
            context.check()?;
            let item_name = format!("item-{index:04}");
            let mut destination = create_copy(&copy_guard, &stage, &item_name, source.size)?;
            source
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|e| e.to_string())?;
            let mut copied = 0u64;
            let mut hasher = Sha256::new();
            loop {
                cancellation(cancel)?;
                let length = source.file.read(&mut buffer).map_err(|e| e.to_string())?;
                if length == 0 {
                    break;
                }
                copied = copied.checked_add(length as u64).ok_or("导入文件过大")?;
                if copied > source.size {
                    return Err(format!("导入来源已变化：{}", source.name));
                }
                destination
                    .write_all(&buffer[..length])
                    .map_err(|e| e.to_string())?;
                hasher.update(&buffer[..length]);
                done += length as u64;
                progress(done, total);
            }
            if copied != source.size || token_for(&source.file)? != source.token {
                return Err(format!("导入来源已变化：{}", source.name));
            }
            verify(&source.parent, &source.name, &source.token)?;
            destination.sync_all().map_err(|e| e.to_string())?;
            validate_zip(&mut destination, cancel)?;
            let fingerprint = complete_copy(destination, &stage, &item_name)?;
            journal.items.push(Item {
                source_name: source.name.clone(),
                target_name: source.name.clone(),
                stage_name: item_name,
                fingerprint,
                content_hash: Some(format!("{:x}", hasher.finalize())),
            });
        }
        stage.sync()?;
        for source in &imports {
            if token_for(&source.file)? != source.token {
                return Err(format!("导入来源已变化：{}", source.name));
            }
            verify(&source.parent, &source.name, &source.token)?;
        }
        write_journal(&operation, &journal)?;
        Ok(())
    })();
    if let Err(error) = copies {
        // No resource has moved. Incomplete service-owned copies remain in a
        // private stage if cleanup cannot be proven safe; sources are untouched.
        for item in &journal.items {
            let _ = remove_item_owned(&stage, &item.stage_name, item);
        }
        if stage.names().is_ok_and(|names| names.is_empty()) {
            let _ = context
                .resources
                .remove_empty_dir(&stage_name(&journal), &stage);
        }
        return Err(match check_copy_cleanup(&copy_guard) {
            Ok(()) => error,
            Err(cleanup) => format!("{error}；来源退役失败，已保留证据：{cleanup}"),
        });
    }
    check_copy_cleanup(&copy_guard)?;
    let mut checked_commit = || {
        for source in &imports {
            if token_for(&source.file)? != source.token {
                return Err(format!("导入来源已变化：{}", source.name));
            }
            verify(&source.parent, &source.name, &source.token)?;
        }
        commit()?;
        for source in &imports {
            if token_for(&source.file)? != source.token {
                return Err(format!("导入来源已变化：{}", source.name));
            }
            verify(&source.parent, &source.name, &source.token)?;
        }
        Ok(())
    };
    // Byte progress was reported while streaming; keep its unit stable during
    // the short rename commit rather than changing total to a file count.
    transaction(
        &context,
        journal,
        operation,
        stage,
        cancel,
        &mut checked_commit,
        &mut |_, _| {},
    )
}

#[cfg(test)]
pub fn restore(
    root: &Path,
    id: &str,
    kind: &str,
    operation_id: &str,
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult, String> {
    restore_with_isolation(
        root,
        id,
        kind,
        operation_id,
        cancel,
        commit,
        progress,
        IsolationPolicy::Automatic,
    )
}

pub fn restore_with_isolation(
    root: &Path,
    id: &str,
    kind: &str,
    operation_id: &str,
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<(), String>,
    progress: &mut dyn FnMut(u64, u64),
    policy: IsolationPolicy,
) -> Result<MutationResult, String> {
    cancellation(cancel)?;
    let context = Context::new_with_isolation(root, id, kind, true, policy)?;
    recover(&context)?;
    let undo_dir = context.operation(operation_id)?;
    let undo = read_journal(&undo_dir, operation_id)?;
    if undo.action != Action::Remove
        || undo.state != JournalState::Committed
        || undo.resource_relative != context.relative
        || undo.kind != kind
    {
        return Err("该移除记录无法在当前资源目录恢复".into());
    }
    let retained = undo_dir.child("files")?;
    let mut items = Vec::new();
    for (index, old) in undo.items.iter().enumerate() {
        verify_item_owned(&retained, &old.target_name, old)?;
        context.resources.absent(&old.source_name)?;
        items.push(Item {
            source_name: old.target_name.clone(),
            target_name: old.source_name.clone(),
            stage_name: format!("item-{index:04}"),
            fingerprint: retained.token(&old.target_name)?,
            content_hash: old.content_hash.clone(),
        });
    }
    let journal = context.new_journal(Action::Restore, Some(operation_id.to_owned()), items);
    let (operation, stage) = prepare(&context, &journal)?;
    transaction(
        &context, journal, operation, stage, cancel, commit, progress,
    )
}

#[cfg(test)]
pub fn removed(root: &Path, id: &str, kind: &str) -> Result<Vec<RemovedOperation>, String> {
    removed_with_isolation(root, id, kind, IsolationPolicy::Automatic)
}
pub fn removed_with_isolation(
    root: &Path,
    id: &str,
    kind: &str,
    policy: IsolationPolicy,
) -> Result<Vec<RemovedOperation>, String> {
    kind_ok(kind)?;
    let resource_path = crate::ui_data::resource_dir_with_isolation(root, id, kind, policy)?;
    if !resource_path.exists() {
        return Ok(Vec::new());
    }
    let context = Context::new_with_isolation(root, id, kind, false, policy)?;
    context.check_scope()?;
    let Some(storage) = &context.storage else {
        return Ok(Vec::new());
    };
    let mut journals = Vec::new();
    for name in storage.names()? {
        if !valid_id(&name) {
            return Err("资源操作目录含有未知记录".into());
        }
        let operation = storage.child(&name)?;
        let Some(journal) = read_journal_optional(&operation, &name)? else {
            continue;
        };
        if journal.kind == kind && journal.resource_relative == context.relative {
            if journal.state == JournalState::Prepared {
                return Err("存在未完成的资源操作，请重试恢复后再操作".into());
            }
            journals.push(journal);
        }
    }
    // A read does not repair pending work. Hide records already consumed by a
    // durably committed restore even if its metadata cleanup was interrupted.
    let restored: BTreeSet<_> = journals
        .iter()
        .filter(|journal| {
            journal.action == Action::Restore && journal.state == JournalState::Committed
        })
        .filter_map(|journal| journal.undo_id.as_deref())
        .collect();
    let mut records = Vec::new();
    for journal in &journals {
        if journal.action != Action::Remove
            || journal.state != JournalState::Committed
            || restored.contains(journal.operation_id.as_str())
        {
            continue;
        }
        let files = context.operation(&journal.operation_id)?.child("files")?;
        for item in &journal.items {
            verify_item_owned(&files, &item.target_name, item)?;
        }
        records.push(RemovedOperation {
            id: journal.operation_id.clone(),
            files: journal
                .items
                .iter()
                .map(|item| item.source_name.clone())
                .collect(),
            created_at: journal.created_at,
        });
    }
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    context.check_scope()?;
    Ok(records)
}

/// Read-only launch guard. No game files or journals are changed here.
pub fn ensure_ready(root: &Path) -> Result<(), String> {
    ensure_local_resources_ready(root)?;
    ensure_verified_batches_ready(root)
}

/// Legacy-only guard for an active batch's commit callback. The batch already
/// holds the history lock and owns a pending record, so checking itself would fail.
pub fn ensure_local_resources_ready(root: &Path) -> Result<(), String> {
    let root_path = root.canonicalize().map_err(|e| e.to_string())?;
    let root = Dir::open(&root_path)?;
    let Some(pcl) = root.optional_child(".pcl-linux")? else {
        return Ok(());
    };
    let Some(storage) = pcl.optional_child("resource-operations")? else {
        return Ok(());
    };
    for name in storage.names()? {
        if !valid_id(&name) {
            return Err("资源操作目录含有未知记录".into());
        }
        let operation = storage.child(&name)?;
        let Some(journal) = read_journal_optional(&operation, &name)? else {
            continue;
        };
        if journal.state == JournalState::Prepared {
            return Err("存在未完成的资源操作，请在资源管理中重试恢复后再启动".into());
        }
    }
    Ok(())
}

pub fn ensure_new_instance_name(root: &Path, id: &str) -> Result<(), String> {
    ensure_verified_batches_ready(root)?;
    safe_name(id)?;
    let directory = Dir::open(root)?;
    let Some(pcl) = directory.optional_child(".pcl-linux")? else {
        return Ok(());
    };
    let Some(storage) = pcl.optional_child("resource-operations")? else {
        return Ok(());
    };
    for name in storage.names()? {
        if !valid_id(&name) {
            return Err("资源操作目录含有未知记录".into());
        }
        let operation = storage.child(&name)?;
        let Some(journal) = read_journal_optional(&operation, &name)? else {
            continue;
        };
        if journal.instance_id == id {
            return Err("此名称仍有资源操作记录，请使用新的实例名称".into());
        }
    }
    Ok(())
}

/// The caller must hold the same writer admission used for other mutations.
pub fn recover_pending(root: &Path, id: &str, kind: &str) -> Result<(), String> {
    verified_batch::recover_pending(root, id)?;
    kind_ok(kind)?;
    safe_name(id)?;
    let root_path = root.canonicalize().map_err(|e| e.to_string())?;
    let _history_lock = crate::instance_rename_refs::root_history_lock(&root_path)?;
    let root = Dir::open(&root_path)?;
    let Some(pcl) = root.optional_child(".pcl-linux")? else {
        return Ok(());
    };
    let Some(storage) = pcl.optional_child("resource-operations")? else {
        return Ok(());
    };
    let mut recorded = BTreeSet::new();
    for name in storage.names()? {
        if !valid_id(&name) {
            return Err("资源操作目录含有未知记录".into());
        }
        let operation = storage.child(&name)?;
        let Some(journal) = read_journal_optional(&operation, &name)? else {
            continue;
        };
        if journal.kind == kind
            && (journal.instance_id == id || journal.resource_relative == kind)
            && matches!(
                journal.state,
                JournalState::Prepared | JournalState::Committed
            )
        {
            recorded.insert(journal.resource_relative);
        }
    }
    // Recovery follows durable recorded paths. A later global or instance
    // choice cannot send an old rollback into the newly selected directory.
    for relative in recorded {
        let context = Context {
            _history_lock: None,
            root_path: root_path.clone(),
            root: Dir::open(&root_path)?,
            resources: descend(&root, &relative, false)?,
            storage: Some(root.child(".pcl-linux")?.child("resource-operations")?),
            relative,
            id: id.into(),
            kind: kind.into(),
            policy: None,
        };
        recover(&context)?;
    }
    Ok(())
}

#[cfg(all(test, windows))]
#[path = "../../../../crates/core/src/file_ops/junction_fixture.rs"]
mod junction_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[cfg(unix)]
    use std::{
        ffi::CString,
        os::unix::{
            ffi::OsStrExt,
            fs::{symlink, MetadataExt},
        },
    };

    struct Fixture {
        root: PathBuf,
        _temporary: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let fixture = Self {
                root: temporary.path().canonicalize().unwrap(),
                _temporary: temporary,
            };
            fixture.instance("shared", false);
            fixture.instance("shared-two", false);
            fixture.instance("isolated", true);
            for kind in ["mods", "resourcepacks", "shaderpacks"] {
                fs::create_dir_all(fixture.root().join(kind)).unwrap();
                fs::create_dir_all(fixture.root().join("versions/isolated").join(kind)).unwrap();
            }
            fixture
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn instance(&self, id: &str, isolated: bool) {
            let version = self.root().join("versions").join(id);
            fs::create_dir_all(&version).unwrap();
            fs::write(
                version.join(format!("{id}.json")),
                format!(r#"{{"id":"{id}","libraries":[]}}"#),
            )
            .unwrap();
            if isolated {
                fs::create_dir_all(version.join("mods")).unwrap();
            }
        }

        fn folder(&self, id: &str, kind: &str) -> PathBuf {
            crate::ui_data::resource_dir(self.root(), id, kind).unwrap()
        }

        fn put(&self, id: &str, kind: &str, name: &str, data: &[u8]) -> ResourceFile {
            let path = self.folder(id, kind).join(name);
            fs::write(&path, data).unwrap();
            ResourceFile {
                file_name: name.into(),
                fingerprint: fingerprint(&path).unwrap(),
            }
        }

        fn archive(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let folder = self.root().join("sources");
            fs::create_dir_all(&folder).unwrap();
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            writer
                .start_file("fixture.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
            let data = writer.finish().unwrap().into_inner();
            let path = folder.join(name);
            fs::write(&path, data).unwrap();
            path
        }
    }

    fn flag() -> AtomicBool {
        AtomicBool::new(false)
    }
    fn commit() -> Result<(), String> {
        Ok(())
    }
    fn progress(_: u64, _: u64) {}

    #[test]
    fn enable_disable_single_batch_and_noop_use_current_tokens() {
        let fixture = Fixture::new();
        let a = fixture.put("shared", "mods", "a.jar", b"a");
        let b = fixture.put("shared", "mods", "b.jar", b"b");
        let result = set_enabled(
            fixture.root(),
            "shared",
            "mods",
            &[a, b],
            false,
            &flag(),
            &mut commit,
            &mut progress,
        )
        .unwrap();
        assert_eq!(result.changed, 2);
        let folder = fixture.folder("shared", "mods");
        assert!(!folder.join("a.jar").exists());
        let selected = ResourceFile {
            file_name: "a.jar.disabled".into(),
            fingerprint: fingerprint(&folder.join("a.jar.disabled")).unwrap(),
        };
        assert_eq!(
            set_enabled(
                fixture.root(),
                "shared",
                "mods",
                &[selected.clone()],
                false,
                &flag(),
                &mut commit,
                &mut progress
            )
            .unwrap()
            .changed,
            0
        );
        assert_eq!(
            set_enabled(
                fixture.root(),
                "shared",
                "mods",
                &[selected],
                true,
                &flag(),
                &mut commit,
                &mut progress
            )
            .unwrap()
            .changed,
            1
        );
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"a");
        ensure_ready(fixture.root()).unwrap();
    }

    #[test]
    fn duplicate_selection_and_same_name_conflicts_write_nothing() {
        let fixture = Fixture::new();
        let a = fixture.put("shared", "mods", "a.jar", b"original");
        let folder = fixture.folder("shared", "mods");
        assert!(set_enabled(
            fixture.root(),
            "shared",
            "mods",
            &[a.clone(), a.clone()],
            false,
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        fs::write(folder.join("a.jar.disabled"), b"existing").unwrap();
        assert!(set_enabled(
            fixture.root(),
            "shared",
            "mods",
            &[a],
            false,
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"original");
        assert_eq!(
            fs::read(folder.join("a.jar.disabled")).unwrap(),
            b"existing"
        );
        assert!(removed(fixture.root(), "shared", "mods")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn failed_commit_and_failed_rollback_record_keep_files_for_durable_reconciliation() {
        let fixture = Fixture::new();
        let selected = fixture.put("shared", "mods", "a.jar", b"original");
        let result = set_enabled(
            fixture.root(),
            "shared",
            "mods",
            &[selected],
            false,
            &flag(),
            &mut || {
                JOURNAL_WRITE_FAILURES.with(|remaining| remaining.set(2));
                Ok(())
            },
            &mut progress,
        );
        JOURNAL_WRITE_FAILURES.with(|remaining| remaining.set(0));
        assert!(result.unwrap_err().contains("无法保存回滚记录"));
        let folder = fixture.folder("shared", "mods");
        // The physical commit was attempted; no rollback is authorized until
        // the durable logical journal is read again.
        assert!(!folder.join("a.jar").exists());
        assert_eq!(
            fs::read(folder.join("a.jar.disabled")).unwrap(),
            b"original"
        );
        assert!(ensure_ready(fixture.root()).is_err());
        recover_pending(fixture.root(), "shared", "mods").unwrap();
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"original");
        assert!(!folder.join("a.jar.disabled").exists());
        ensure_ready(fixture.root()).unwrap();
    }

    #[test]
    fn middle_of_enable_batch_rolls_back_and_preserves_new_conflict() {
        let fixture = Fixture::new();
        let a = fixture.put("shared", "mods", "a.jar", b"original a");
        let b = fixture.put("shared", "mods", "b.jar", b"original b");
        let folder = fixture.folder("shared", "mods");
        let result = set_enabled(
            fixture.root(),
            "shared",
            "mods",
            &[a, b],
            false,
            &flag(),
            &mut commit,
            &mut |done, _| {
                if done == 1 {
                    fs::write(folder.join("b.jar.disabled"), b"external blocker").unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"original a");
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"original b");
        assert!(!folder.join("a.jar.disabled").exists());
        assert_eq!(
            fs::read(folder.join("b.jar.disabled")).unwrap(),
            b"external blocker"
        );
        ensure_ready(fixture.root()).unwrap();
    }

    #[test]
    fn remove_and_restore_preserve_bytes_and_reject_whole_batch_conflict() {
        let fixture = Fixture::new();
        let a = fixture.put("shared", "mods", "a.jar", b"original a");
        let b = fixture.put("shared", "mods", "b.jar.disabled", b"original b");
        let result = remove(
            fixture.root(),
            "shared",
            "mods",
            &[a, b],
            &flag(),
            &mut commit,
            &mut progress,
        )
        .unwrap();
        let undo_id = result.undo_id.unwrap();
        let records = removed(fixture.root(), "shared-two", "mods").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].files, ["a.jar", "b.jar.disabled"]);
        let folder = fixture.folder("shared", "mods");
        fs::write(folder.join("b.jar.disabled"), b"existing b").unwrap();
        assert!(restore(
            fixture.root(),
            "shared",
            "mods",
            &undo_id,
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert!(!folder.join("a.jar").exists());
        assert_eq!(
            fs::read(folder.join("b.jar.disabled")).unwrap(),
            b"existing b"
        );
        fs::remove_file(folder.join("b.jar.disabled")).unwrap();
        assert_eq!(
            restore(
                fixture.root(),
                "shared-two",
                "mods",
                &undo_id,
                &flag(),
                &mut commit,
                &mut progress
            )
            .unwrap()
            .changed,
            2
        );
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"original a");
        assert_eq!(
            fs::read(folder.join("b.jar.disabled")).unwrap(),
            b"original b"
        );
        assert!(removed(fixture.root(), "shared", "mods")
            .unwrap()
            .is_empty());
        assert!(restore(
            fixture.root(),
            "shared",
            "mods",
            &undo_id,
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
    }

    #[test]
    fn middle_of_remove_batch_restores_originals_without_overwrite() {
        let fixture = Fixture::new();
        let a = fixture.put("shared", "mods", "a.jar", b"original a");
        let b = fixture.put("shared", "mods", "b.jar", b"original b");
        let root = fixture.root();
        let result = remove(
            root,
            "shared",
            "mods",
            &[a, b],
            &flag(),
            &mut commit,
            &mut |done, _| {
                if done == 1 {
                    let operations = root.join(".pcl-linux/resource-operations");
                    let operation = fs::read_dir(operations)
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path();
                    fs::write(operation.join("files/b.jar"), b"external blocker").unwrap();
                }
            },
        );
        assert!(result.is_err());
        let folder = fixture.folder("shared", "mods");
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"original a");
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"original b");
        let operation = fs::read_dir(root.join(".pcl-linux/resource-operations"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read(operation.join("files/b.jar")).unwrap(),
            b"external blocker"
        );
        ensure_ready(root).unwrap();
    }

    #[test]
    fn middle_of_restore_batch_keeps_undo_visible_and_retryable() {
        let fixture = Fixture::new();
        let a = fixture.put("shared", "mods", "a.jar", b"original a");
        let b = fixture.put("shared", "mods", "b.jar", b"original b");
        let removed_result = remove(
            fixture.root(),
            "shared",
            "mods",
            &[a, b],
            &flag(),
            &mut commit,
            &mut progress,
        )
        .unwrap();
        let undo = removed_result.undo_id.unwrap();
        let folder = fixture.folder("shared", "mods");
        assert!(restore(
            fixture.root(),
            "shared",
            "mods",
            &undo,
            &flag(),
            &mut commit,
            &mut |done, _| {
                if done == 1 {
                    fs::write(folder.join("b.jar"), b"external blocker").unwrap();
                }
            }
        )
        .is_err());
        assert!(!folder.join("a.jar").exists());
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"external blocker");
        let records = removed(fixture.root(), "shared", "mods").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, undo);
        fs::remove_file(folder.join("b.jar")).unwrap();
        assert_eq!(
            restore(
                fixture.root(),
                "shared",
                "mods",
                &undo,
                &flag(),
                &mut commit,
                &mut progress
            )
            .unwrap()
            .changed,
            2
        );
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"original a");
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"original b");
        assert!(removed(fixture.root(), "shared", "mods")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn restart_partial_restore_retains_a_working_undo_record() {
        let fixture = Fixture::new();
        let a = fixture.put("shared", "mods", "a.jar", b"original a");
        let b = fixture.put("shared", "mods", "b.jar", b"original b");
        let result = remove(
            fixture.root(),
            "shared",
            "mods",
            &[a, b],
            &flag(),
            &mut commit,
            &mut progress,
        )
        .unwrap();
        let undo_id = result.undo_id.unwrap();
        let context = Context::new(fixture.root(), "shared", "mods", true).unwrap();
        let undo_dir = context.operation(&undo_id).unwrap();
        let undo = read_journal(&undo_dir, &undo_id).unwrap();
        let journal =
            context.new_journal(Action::Restore, Some(undo_id.clone()), undo.items.clone());
        let (_, stage) = prepare(&context, &journal).unwrap();
        let retained = undo_dir.child("files").unwrap();
        for item in &journal.items {
            rename_new(&retained, &item.source_name, &stage, &item.stage_name).unwrap();
        }
        let first = &journal.items[0];
        rename_new(
            &stage,
            &first.stage_name,
            &context.resources,
            &first.target_name,
        )
        .unwrap();
        assert!(ensure_ready(fixture.root()).is_err());
        // A restarted process has released the previous root writer lock.
        drop(context);
        recover_pending(fixture.root(), "shared", "mods").unwrap();
        assert_eq!(
            removed(fixture.root(), "shared", "mods").unwrap()[0].id,
            undo_id
        );
        restore(
            fixture.root(),
            "shared",
            "mods",
            &undo_id,
            &flag(),
            &mut commit,
            &mut progress,
        )
        .unwrap();
        assert_eq!(
            fs::read(fixture.folder("shared", "mods").join("a.jar")).unwrap(),
            b"original a"
        );
        assert_eq!(
            fs::read(fixture.folder("shared", "mods").join("b.jar")).unwrap(),
            b"original b"
        );
    }

    #[test]
    fn retained_content_edit_with_preserved_mtime_is_not_restored_or_deleted() {
        let fixture = Fixture::new();
        let selected = fixture.put("shared", "mods", "a.jar", b"before");
        let result = remove(
            fixture.root(),
            "shared",
            "mods",
            &[selected],
            &flag(),
            &mut commit,
            &mut progress,
        )
        .unwrap();
        let undo = result.undo_id.unwrap();
        let retained = fixture
            .root()
            .join(".pcl-linux/resource-operations")
            .join(&undo)
            .join("files/a.jar");
        let metadata = fs::metadata(&retained).unwrap();
        fs::write(&retained, b"after!").unwrap();
        preserve_mtime(&retained, &metadata);
        assert!(removed(fixture.root(), "shared", "mods").is_err());
        assert!(restore(
            fixture.root(),
            "shared",
            "mods",
            &undo,
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert_eq!(fs::read(retained).unwrap(), b"after!");
        assert!(!fixture.folder("shared", "mods").join("a.jar").exists());
    }

    #[test]
    fn shared_and_isolated_scopes_are_distinct_for_all_resource_kinds() {
        let fixture = Fixture::new();
        for kind in ["mods", "resourcepacks", "shaderpacks"] {
            let name = if kind == "mods" {
                "fixture.jar"
            } else {
                "fixture.zip"
            };
            let shared = fixture.put("shared", kind, name, b"shared resource");
            let isolated = fixture.put("isolated", kind, name, b"isolated resource");
            let result = remove(
                fixture.root(),
                "isolated",
                kind,
                &[isolated],
                &flag(),
                &mut commit,
                &mut progress,
            )
            .unwrap();
            assert_eq!(
                fs::read(fixture.folder("shared", kind).join(name)).unwrap(),
                b"shared resource"
            );
            assert!(removed(fixture.root(), "shared", kind).unwrap().is_empty());
            assert!(restore(
                fixture.root(),
                "shared",
                kind,
                result.undo_id.as_deref().unwrap(),
                &flag(),
                &mut commit,
                &mut progress
            )
            .is_err());
            restore(
                fixture.root(),
                "isolated",
                kind,
                result.undo_id.as_deref().unwrap(),
                &flag(),
                &mut commit,
                &mut progress,
            )
            .unwrap();
            assert_eq!(
                fingerprint(&fixture.folder("shared", kind).join(name)).unwrap(),
                shared.fingerprint
            );
        }
    }

    #[test]
    fn stale_selection_and_commit_time_external_edit_fail_before_moves() {
        let fixture = Fixture::new();
        let a = fixture.put("shared", "mods", "a.jar", b"original");
        let b = fixture.put("shared", "mods", "b.jar", b"other original");
        let folder = fixture.folder("shared", "mods");
        fs::write(folder.join("a.jar"), b"changed").unwrap();
        assert!(remove(
            fixture.root(),
            "shared",
            "mods",
            &[a],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        let a = ResourceFile {
            file_name: "a.jar".into(),
            fingerprint: fingerprint(&folder.join("a.jar")).unwrap(),
        };
        assert!(set_enabled(
            fixture.root(),
            "shared",
            "mods",
            &[a, b],
            false,
            &flag(),
            &mut || {
                fs::write(folder.join("a.jar"), b"new edit").unwrap();
                Ok(())
            },
            &mut progress
        )
        .is_err());
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"new edit");
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"other original");
        assert!(!folder.join("b.jar.disabled").exists());
        ensure_ready(fixture.root()).unwrap();
    }

    #[test]
    fn selection_ctime_detects_same_size_edit_with_preserved_mtime() {
        let fixture = Fixture::new();
        let selected = fixture.put("shared", "mods", "a.jar", b"before");
        let path = fixture.folder("shared", "mods").join("a.jar");
        let metadata = fs::metadata(&path).unwrap();
        fs::write(&path, b"after!").unwrap();
        preserve_mtime(&path, &metadata);
        assert_ne!(fingerprint(&path).unwrap(), selected.fingerprint);
        assert!(remove(
            fixture.root(),
            "shared",
            "mods",
            &[selected],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert_eq!(fs::read(path).unwrap(), b"after!");
    }

    #[cfg(unix)]
    fn preserve_mtime(path: &Path, metadata: &fs::Metadata) {
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            },
            libc::timespec {
                tv_sec: metadata.mtime(),
                tv_nsec: metadata.mtime_nsec(),
            },
        ];
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::utimensat(
                    libc::AT_FDCWD,
                    name.as_ptr(),
                    times.as_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            },
            0
        );
    }

    #[cfg(windows)]
    fn preserve_mtime(path: &Path, metadata: &fs::Metadata) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
            .unwrap();
    }
    fn directory_alias(source: &Path, destination: &Path) {
        #[cfg(unix)]
        symlink(source, destination).unwrap();
        #[cfg(windows)]
        {
            fs::create_dir(destination).unwrap();
            junction_fixture::set_existing(source, destination).unwrap();
        }
    }
    fn remove_directory_alias(path: &Path) {
        #[cfg(unix)]
        fs::remove_file(path).unwrap();
        #[cfg(windows)]
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn import_accepts_valid_archives_and_never_overwrites_or_duplicates() {
        let fixture = Fixture::new();
        for (kind, name) in [
            ("mods", "fixture.jar.disabled"),
            ("resourcepacks", "fixture.zip"),
            ("shaderpacks", "shader.zip"),
        ] {
            let source = fixture.archive(name, b"archive content");
            let original = fs::read(&source).unwrap();
            let result = import_files(
                fixture.root(),
                "isolated",
                kind,
                &[source.clone()],
                &flag(),
                &mut commit,
                &mut progress,
            )
            .unwrap();
            assert_eq!(result.changed, 1);
            assert_eq!(
                fs::read(fixture.folder("isolated", kind).join(name)).unwrap(),
                original
            );
            assert_eq!(fs::read(&source).unwrap(), original);
            assert!(import_files(
                fixture.root(),
                "isolated",
                kind,
                &[source.clone()],
                &flag(),
                &mut commit,
                &mut progress
            )
            .is_err());
            assert!(import_files(
                fixture.root(),
                "shared",
                kind,
                &[source.clone(), source],
                &flag(),
                &mut commit,
                &mut progress
            )
            .is_err());
            assert!(!fixture.folder("shared", kind).join(name).exists());
        }
    }

    #[test]
    fn invalid_zip_and_wrong_extensions_fail_whole_import_batch() {
        let fixture = Fixture::new();
        let good = fixture.archive("good.jar", b"valid");
        let bad = fixture.root().join("sources/bad.jar");
        fs::write(&bad, b"PK\x03\x04incomplete ZIP").unwrap();
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[good, bad],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert!(!fixture.folder("shared", "mods").join("good.jar").exists());
        let wrong = fixture.archive("wrong.zip", b"valid");
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[wrong],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
    }

    #[test]
    fn copy_source_edit_and_cancellation_leave_visible_resources_unchanged() {
        let fixture = Fixture::new();
        let source = fixture.archive("copy.jar", &[b'x'; 200_000]);
        let mut changed = false;
        let result = import_files(
            fixture.root(),
            "shared",
            "mods",
            &[source.clone()],
            &flag(),
            &mut commit,
            &mut |_, _| {
                if !changed {
                    changed = true;
                    fs::write(&source, b"external source edit").unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&source).unwrap(), b"external source edit");
        assert!(!fixture.folder("shared", "mods").join("copy.jar").exists());
        let cancelled = AtomicBool::new(false);
        let source = fixture.archive("cancel.jar", &[b'y'; 200_000]);
        let mut committed = false;
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[source],
            &cancelled,
            &mut || {
                committed = true;
                Ok(())
            },
            &mut |_, _| {
                cancelled.store(true, Ordering::Release);
            }
        )
        .is_err());
        assert!(!committed);
        assert!(!fixture.folder("shared", "mods").join("cancel.jar").exists());
        ensure_ready(fixture.root()).unwrap();
    }

    #[test]
    fn commit_rejection_and_new_import_conflict_keep_all_original_files() {
        let fixture = Fixture::new();
        let source = fixture.archive("a.jar", b"source a");
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[source.clone()],
            &flag(),
            &mut || Err("cancelled at commit".into()),
            &mut progress
        )
        .is_err());
        let folder = fixture.folder("shared", "mods");
        assert!(!folder.join("a.jar").exists());
        let result = import_files(
            fixture.root(),
            "shared",
            "mods",
            &[source.clone()],
            &flag(),
            &mut || {
                fs::write(folder.join("a.jar"), b"external existing").unwrap();
                Ok(())
            },
            &mut progress,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(folder.join("a.jar")).unwrap(),
            b"external existing"
        );
        assert!(zip::ZipArchive::new(File::open(source).unwrap()).is_ok());
        ensure_ready(fixture.root()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn resource_links_source_links_and_operation_parent_links_cannot_cross_boundary() {
        let fixture = Fixture::new();
        let outside = fixture.root().join("outside");
        fs::create_dir(&outside).unwrap();
        let external = outside.join("external.jar");
        fs::write(&external, b"protected").unwrap();
        let folder = fixture.folder("shared", "mods");
        symlink(&external, folder.join("link.jar")).unwrap();
        assert!(fingerprint(&folder.join("link.jar")).is_err());
        let selected = ResourceFile {
            file_name: "link.jar".into(),
            fingerprint: fingerprint(&external).unwrap(),
        };
        assert!(remove(
            fixture.root(),
            "shared",
            "mods",
            &[selected],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        let good = fixture.archive("good.jar", b"valid");
        let link = fixture.root().join("sources/link.jar");
        symlink(&good, &link).unwrap();
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[link],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        fs::remove_file(folder.join("link.jar")).unwrap();
        fs::remove_dir(&folder).unwrap();
        symlink(&outside, &folder).unwrap();
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[good.clone()],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        fs::remove_file(&folder).unwrap();
        fs::create_dir(&folder).unwrap();
        let pcl = fixture.root().join(".pcl-linux");
        if pcl.exists() {
            fs::remove_dir_all(&pcl).unwrap();
        }
        symlink(&outside, &pcl).unwrap();
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[good],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert_eq!(fs::read(external).unwrap(), b"protected");
        assert!(!outside.join("resource-operations").exists());
    }

    #[cfg(windows)]
    #[test]
    fn native_junction_resource_source_and_history_ancestors_are_rejected() {
        let fixture = Fixture::new();
        let good = fixture.archive("good.jar", b"valid source");
        let outside = fixture.root().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"unchanged").unwrap();
        let resources = fixture.folder("shared", "mods");
        fs::remove_dir(&resources).unwrap();
        directory_alias(&outside, &resources);
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[good.clone()],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"unchanged");
        assert!(!outside.join("good.jar").exists());
        remove_directory_alias(&resources);
        fs::create_dir(&resources).unwrap();
        let sources = fixture.root().join("sources");
        let original = fixture.root().join("original-sources");
        fs::rename(&sources, &original).unwrap();
        directory_alias(&original, &sources);
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[good.clone()],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert!(zip::ZipArchive::new(File::open(original.join("good.jar")).unwrap()).is_ok());
        remove_directory_alias(&sources);
        fs::rename(&original, &sources).unwrap();
        let private = fixture.root().join(".pcl-linux");
        fs::remove_dir_all(&private).unwrap();
        directory_alias(&outside, &private);
        assert!(import_files(
            fixture.root(),
            "shared",
            "mods",
            &[good],
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"unchanged");
        assert!(!outside.join("resource-operations").exists());
        remove_directory_alias(&private);
    }

    #[test]
    fn directory_swap_during_commit_is_detected_before_any_resource_move() {
        let fixture = Fixture::new();
        let selected = fixture.put("shared", "mods", "a.jar", b"protected");
        let folder = fixture.folder("shared", "mods");
        let moved = fixture.root().join("moved-mods");
        let outside = fixture.root().join("outside");
        fs::create_dir(&outside).unwrap();
        let did_rename = std::cell::Cell::new(false);
        assert!(set_enabled(
            fixture.root(),
            "shared",
            "mods",
            &[selected],
            false,
            &flag(),
            &mut || {
                match fs::rename(&folder, &moved) {
                    Ok(()) => did_rename.set(true),
                    #[cfg(windows)]
                    Err(error) if matches!(error.raw_os_error(), Some(5 | 32)) => {
                        // The transaction retains its generated stage child;
                        // NTFS blocks renaming this ancestor while it is open.
                        assert!(folder.is_dir() && !moved.exists());
                        return Err("held native descendant blocked ancestor replacement".into());
                    }
                    Err(error) => panic!("unexpected rename result: {error}"),
                }
                directory_alias(&outside, &folder);
                Ok(())
            },
            &mut progress
        )
        .is_err());
        if !did_rename.get() {
            assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"protected");
            assert!(!folder.join("a.jar.disabled").exists());
            assert!(fs::read_dir(outside).unwrap().next().is_none());
            ensure_ready(fixture.root()).unwrap();
            return;
        }
        assert_eq!(fs::read(moved.join("a.jar")).unwrap(), b"protected");
        assert!(!moved.join("a.jar.disabled").exists());
        assert!(fs::read_dir(outside).unwrap().next().is_none());
        remove_directory_alias(&folder);
        fs::rename(moved, folder).unwrap();
        recover_pending(fixture.root(), "shared", "mods").unwrap();
        ensure_ready(fixture.root()).unwrap();
    }

    fn interrupted_disable(fixture: &Fixture) -> (Context, Journal, Dir, Dir) {
        let a = fixture.put("shared", "mods", "a.jar", b"original a");
        let b = fixture.put("shared", "mods", "b.jar", b"original b");
        let context = Context::new(fixture.root(), "shared", "mods", true).unwrap();
        let mut items = checked_selection(&context, &[a, b]).unwrap();
        for item in &mut items {
            item.target_name = format!("{}.disabled", item.source_name);
        }
        let journal = context.new_journal(Action::Disable, None, items);
        let (operation, stage) = prepare(&context, &journal).unwrap();
        for item in &journal.items {
            rename_new(
                &context.resources,
                &item.source_name,
                &stage,
                &item.stage_name,
            )
            .unwrap();
        }
        let item = &journal.items[0];
        rename_new(
            &stage,
            &item.stage_name,
            &context.resources,
            &item.target_name,
        )
        .unwrap();
        (context, journal, operation, stage)
    }

    #[test]
    fn restart_prepared_journal_blocks_launch_and_read_until_batch_recovered() {
        let fixture = Fixture::new();
        let (_, _, _, _) = interrupted_disable(&fixture);
        assert!(ensure_ready(fixture.root()).is_err());
        assert!(removed(fixture.root(), "shared", "mods").is_err());
        recover_pending(fixture.root(), "shared-two", "mods").unwrap();
        let folder = fixture.folder("shared", "mods");
        assert_eq!(fs::read(folder.join("a.jar")).unwrap(), b"original a");
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"original b");
        assert!(!folder.join("a.jar.disabled").exists());
        ensure_ready(fixture.root()).unwrap();
    }

    #[test]
    fn restart_conflicting_original_is_preserved_and_no_partial_recovery_runs() {
        let fixture = Fixture::new();
        let (_, _, _, _) = interrupted_disable(&fixture);
        let folder = fixture.folder("shared", "mods");
        fs::write(folder.join("b.jar"), b"new unrelated b").unwrap();
        assert!(recover_pending(fixture.root(), "shared", "mods").is_err());
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"new unrelated b");
        assert_eq!(
            fs::read(folder.join("a.jar.disabled")).unwrap(),
            b"original a"
        );
        assert!(!folder.join("a.jar").exists());
        assert!(ensure_ready(fixture.root()).is_err());
        fs::remove_file(folder.join("b.jar")).unwrap();
        recover_pending(fixture.root(), "shared", "mods").unwrap();
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"original b");
    }

    #[test]
    fn import_partial_commit_rolls_back_and_keeps_existing_blocker() {
        let fixture = Fixture::new();
        let source_a = fixture.archive("a.jar", b"a");
        let source_b = fixture.archive("b.jar", b"b");
        let context = Context::new(fixture.root(), "shared", "mods", true).unwrap();
        let mut journal = context.new_journal(Action::Import, None, Vec::new());
        let operation = context
            .storage()
            .unwrap()
            .create_child(&journal.operation_id)
            .unwrap();
        operation.create_child("files").unwrap();
        let stage = context
            .resources
            .create_child(&stage_name(&journal))
            .unwrap();
        for (index, path) in [source_a, source_b].iter().enumerate() {
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            let mut file = stage.create_file(&format!("item-{index:04}")).unwrap();
            file.write_all(&fs::read(path).unwrap()).unwrap();
            file.sync_all().unwrap();
            let hash = content_hash(&mut file).unwrap();
            drop(file);
            journal.items.push(Item {
                source_name: name.clone(),
                target_name: name,
                stage_name: format!("item-{index:04}"),
                fingerprint: stage.token(&format!("item-{index:04}")).unwrap(),
                content_hash: Some(hash),
            });
        }
        write_journal(&operation, &journal).unwrap();
        let folder = fixture.folder("shared", "mods");
        assert!(transaction(
            &context,
            journal,
            operation,
            stage,
            &flag(),
            &mut commit,
            &mut |done, _| {
                if done == 1 {
                    fs::write(folder.join("b.jar"), b"existing blocker").unwrap();
                }
            }
        )
        .is_err());
        assert!(!folder.join("a.jar").exists());
        assert_eq!(fs::read(folder.join("b.jar")).unwrap(), b"existing blocker");
        ensure_ready(fixture.root()).unwrap();
    }

    #[test]
    fn import_recovery_does_not_delete_same_inode_changed_content_with_old_mtime() {
        let fixture = Fixture::new();
        let context = Context::new(fixture.root(), "shared", "mods", true).unwrap();
        let mut journal = context.new_journal(Action::Import, None, Vec::new());
        let operation = context
            .storage()
            .unwrap()
            .create_child(&journal.operation_id)
            .unwrap();
        operation.create_child("files").unwrap();
        let stage = context
            .resources
            .create_child(&stage_name(&journal))
            .unwrap();
        let mut file = stage.create_file("item-0000").unwrap();
        file.write_all(b"before").unwrap();
        file.sync_all().unwrap();
        let hash = content_hash(&mut file).unwrap();
        drop(file);
        journal.items.push(Item {
            source_name: "a.jar".into(),
            target_name: "a.jar".into(),
            stage_name: "item-0000".into(),
            fingerprint: stage.token("item-0000").unwrap(),
            content_hash: Some(hash),
        });
        write_journal(&operation, &journal).unwrap();
        rename_new(&stage, "item-0000", &context.resources, "a.jar").unwrap();
        let path = fixture.folder("shared", "mods").join("a.jar");
        let metadata = fs::metadata(&path).unwrap();
        fs::write(&path, b"after!").unwrap();
        preserve_mtime(&path, &metadata);
        assert!(recover_pending(fixture.root(), "shared", "mods").is_err());
        assert_eq!(fs::read(path).unwrap(), b"after!");
        assert!(ensure_ready(fixture.root()).is_err());
    }

    #[test]
    fn malformed_journal_paths_versions_and_fingerprints_never_move_files() {
        let fixture = Fixture::new();
        let (_, journal, operation, _) = interrupted_disable(&fixture);
        for field in [
            "resource_relative",
            "source_name",
            "version",
            "fingerprint",
            "content_hash",
        ] {
            let mut value = serde_json::to_value(&journal).unwrap();
            match field {
                "resource_relative" => value[field] = "../outside/mods".into(),
                "source_name" => value["items"][0][field] = "../a.jar".into(),
                "version" => value[field] = 999.into(),
                "fingerprint" => value["items"][0][field] = "untrusted".into(),
                "content_hash" => value["items"][0][field] = "1234".into(),
                _ => unreachable!(),
            }
            publish_journal(&operation, &serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(recover_pending(fixture.root(), "shared", "mods").is_err());
            assert_eq!(
                fs::read(fixture.folder("shared", "mods").join("a.jar.disabled")).unwrap(),
                b"original a"
            );
        }
        write_journal(&operation, &journal).unwrap();
        recover_pending(fixture.root(), "shared", "mods").unwrap();
    }

    #[test]
    fn maximum_batch_with_long_escaped_names_has_readable_journal() {
        let fixture = Fixture::new();
        let context = Context::new(fixture.root(), "shared", "mods", true).unwrap();
        let sample_token = fixture
            .put("shared", "mods", "token-fixture.jar", b"a")
            .fingerprint;
        let items = (0..MAX_FILES)
            .map(|index| {
                // Legal names with the largest JSON expansion: ASCII quotes take
                // two bytes each in JSON while occupying only one on the filesystem.
                let fill = if cfg!(windows) { "a" } else { "\"" };
                let name = format!("{}{index:04}.jar", fill.repeat(247));
                assert_eq!(name.len(), 255);
                Item {
                    source_name: name.clone(),
                    target_name: name,
                    stage_name: format!("item-{index:04}"),
                    fingerprint: sample_token.clone(),
                    content_hash: Some("a".repeat(64)),
                }
            })
            .collect();
        let journal = context.new_journal(Action::Remove, None, items);
        let operation = context
            .storage()
            .unwrap()
            .create_child(&journal.operation_id)
            .unwrap();
        write_journal(&operation, &journal).unwrap();
        let length = journal_bytes(&operation).unwrap().unwrap().len() as u64;
        assert!(
            length
                > if cfg!(windows) {
                    128 * 1024
                } else {
                    512 * 1024
                }
        );
        assert!(length <= MAX_JOURNAL_BYTES as u64);
        let loaded = read_journal(&operation, &journal.operation_id).unwrap();
        assert_eq!(loaded.items.len(), MAX_FILES);
        assert_eq!(
            loaded.items[511].source_name,
            journal.items[511].source_name
        );
    }

    #[test]
    fn oversized_journal_publication_preserves_existing_readable_record() {
        let fixture = Fixture::new();
        let (_, journal, operation, _) = interrupted_disable(&fixture);
        let before =
            serde_json::to_vec(&read_journal(&operation, &journal.operation_id).unwrap()).unwrap();
        let names_before = operation.names().unwrap();
        assert!(publish_journal(&operation, &vec![b' '; MAX_JOURNAL_BYTES + 1]).is_err());
        assert_eq!(operation.names().unwrap(), names_before);
        let after =
            serde_json::to_vec(&read_journal(&operation, &journal.operation_id).unwrap()).unwrap();
        assert_eq!(after, before);
        recover_pending(fixture.root(), "shared", "mods").unwrap();
        assert_eq!(
            fs::read(fixture.folder("shared", "mods").join("a.jar")).unwrap(),
            b"original a"
        );
    }

    #[test]
    fn nonregular_files_and_unsafe_names_are_rejected_before_commit() {
        let fixture = Fixture::new();
        let folder = fixture.folder("shared", "mods");
        fs::create_dir(folder.join("directory.jar")).unwrap();
        assert!(fingerprint(&folder.join("directory.jar")).is_err());
        for name in ["../outside.jar", "a/b.jar", "a\\b.jar", ".", "..", ""] {
            let file = ResourceFile {
                file_name: name.into(),
                fingerprint: "v2:1:1:1:1:1:1:1".into(),
            };
            assert!(remove(
                fixture.root(),
                "shared",
                "mods",
                &[file],
                &flag(),
                &mut commit,
                &mut progress
            )
            .is_err());
        }
        assert!(set_enabled(
            fixture.root(),
            "shared",
            "resourcepacks",
            &[],
            false,
            &flag(),
            &mut commit,
            &mut progress
        )
        .is_err());
    }
    #[test]
    fn interrupted_legacy_operation_recovers_recorded_shared_path_after_marker_change() {
        let fixture = Fixture::new();
        let interrupted = interrupted_disable(&fixture);
        drop(interrupted);
        let version = fixture.root().join("versions/shared");
        fs::create_dir(version.join("mods")).unwrap();
        fs::write(version.join("mods/a.jar"), b"isolated untouched").unwrap();
        recover_pending(fixture.root(), "shared", "mods").unwrap();
        assert_eq!(
            fs::read(fixture.root().join("mods/a.jar")).unwrap(),
            b"original a"
        );
        assert_eq!(
            fs::read(fixture.root().join("mods/b.jar")).unwrap(),
            b"original b"
        );
        assert!(!fixture.root().join("mods/a.jar.disabled").exists());
        assert_eq!(
            fs::read(version.join("mods/a.jar")).unwrap(),
            b"isolated untouched"
        );
        ensure_ready(fixture.root()).unwrap();
    }
}
