//! Recoverable resource replacement and undo, independent of Tauri/networking.
//!
//! The instance owns its permanent `.pcl-resource-updates` history; journal
//! bindings contain inodes and isolation flags, never a stale instance name.
//! A root marker protects active transactions even if an external process moves
//! their entire instance out of `versions`. Small status records let launch
//! guards inspect permanent history without hashing every manifest or archive.
//!
//! Staging -> Prepared -> Applying -> Committed. Only Applying touches targets;
//! recovery rolls it back. UndoPrepared -> UndoApplying -> Restored similarly
//! returns failed undo to Committed. Original and new inodes are moved, not
//! hardlinked, so provider checks keep seeing independent files (`nlink == 1`).
use super::verified_batch::{
    archive,
    filesystem::{self, Dir, Key, Owned},
};
use super::{MutationResult, VerifiedImport};
use pcl_core::content_scope::IsolationPolicy;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
mod journal;
mod recovery;
mod scope;
use scope::{Context, Inventory, Node, Scope};
type Result<T> = std::result::Result<T, String>;
const HISTORY: &str = ".pcl-resource-updates";
const MARKERS: &str = "resource-update-pending";
const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_BATCH_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HISTORIES: usize = 256;
const CANCELLED: &str = "资源下载已取消";
const KINDS: [&str; 3] = ["mods", "resourcepacks", "shaderpacks"];
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    pub kind: String,
    pub old_file_name: String,
    pub old_fingerprint: String,
    pub old_sha512: String,
    pub new_file_name: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct UpdateHistory {
    pub id: String,
    pub files: Vec<String>,
    pub created_at: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Staging,
    Prepared,
    Applying,
    Committed,
    UndoPrepared,
    UndoApplying,
    RolledBack,
    Restored,
}
impl State {
    fn pending(self) -> bool {
        self != Self::Committed
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Old {
    name: String,
    fingerprint: String,
    owned: Owned,
    backup: Option<Owned>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    kind: String,
    name: String,
    size: u64,
    sha512: String,
    new: Option<Owned>,
    old: Option<Old>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    id: String,
    created_at: u64,
    scope: Scope,
    marker: Option<Owned>,
    state: State,
    before: Inventory,
    after: Inventory,
    items: Vec<Item>,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn check(c: &AtomicBool) -> Result<()> {
    if c.load(Ordering::Acquire) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    id.strip_prefix("u-").is_some_and(super::valid_operation_id)
}
fn new_id() -> String {
    format!("u-{}", super::new_id())
}
fn valid_hash(s: &str) -> bool {
    s.len() == 128
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn name_ok(kind: &str, name: &str) -> Result<()> {
    super::safe_name(name)?;
    if kind != "mods" && name.ends_with(".disabled") {
        return Err("只有模组可以保持禁用状态".into());
    }
    let name = name.strip_suffix(".disabled").unwrap_or(name);
    if name.contains(':')
        || name.starts_with(".pcl-")
        || !match kind {
            "mods" => name.ends_with(".jar"),
            "resourcepacks" | "shaderpacks" => name.ends_with(".zip"),
            _ => false,
        }
    {
        return Err("资源更新类型或文件名无效".into());
    }
    Ok(())
}
fn counterpart(name: &str) -> String {
    match name.strip_suffix(".disabled") {
        Some(s) => s.into(),
        None => format!("{name}.disabled"),
    }
}
fn slot(prefix: &str, index: usize) -> String {
    format!("{prefix}-{index:04}")
}
fn verify_owned(dir: &Dir, name: &str, owned: &Owned) -> Result<()> {
    if dir.regular(name)?.metadata().map_err(err)?.nlink() != 1 {
        return Err("更新文件含额外硬链接，已保留".into());
    }
    filesystem::verify_named(dir, name, owned)
}
use std::os::unix::fs::MetadataExt;
fn validate_sources(
    files: &mut [VerifiedImport],
    replacements: &[Replacement],
    cancel: &AtomicBool,
) -> Result<Vec<String>> {
    if files.len() > MAX_FILES || replacements.len() > MAX_FILES {
        return Err("一次最多更新 512 个资源文件".into());
    }
    let mut total = 0u64;
    let mut names = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut tokens = Vec::new();
    for f in files.iter_mut() {
        check(cancel)?;
        name_ok(&f.kind, &f.file_name)?;
        if f.size == 0
            || f.size > MAX_FILE_BYTES
            || !valid_hash(&f.sha512)
            || names.contains(&(f.kind.clone(), counterpart(&f.file_name)))
            || !names.insert((f.kind.clone(), f.file_name.clone()))
            || !keys.insert(filesystem::key_file(&f.file)?)
        {
            return Err("更新下载文件重复或大小、SHA512 无效".into());
        }
        total = total
            .checked_add(f.size)
            .filter(|n| *n <= MAX_BATCH_BYTES)
            .ok_or("资源更新下载超过 8 GiB")?;
        filesystem::anonymous_source(&f.file)?;
        let token = super::token_for(&f.file)?;
        filesystem::verify_file(&mut f.file, None, f.size, &f.sha512, Some(cancel))?;
        archive::validate(&mut f.file, cancel)?;
        if super::token_for(&f.file)? != token {
            return Err("更新下载文件校验期间已经变化".into());
        }
        tokens.push(token);
    }
    let mut old = BTreeSet::new();
    let mut mapped = BTreeSet::new();
    for r in replacements {
        name_ok(&r.kind, &r.old_file_name)?;
        name_ok(&r.kind, &r.new_file_name)?;
        if !super::valid_token(&r.old_fingerprint)
            || !valid_hash(&r.old_sha512)
            || !old.insert((r.kind.clone(), r.old_file_name.clone()))
            || !mapped.insert((r.kind.clone(), r.new_file_name.clone()))
            || !names.contains(&(r.kind.clone(), r.new_file_name.clone()))
            || r.old_file_name.ends_with(".disabled") != r.new_file_name.ends_with(".disabled")
        {
            return Err("资源替换映射重复、缺少下载或禁用状态不匹配".into());
        }
    }
    for r in replacements {
        if r.old_file_name != r.new_file_name
            && old.contains(&(r.kind.clone(), r.new_file_name.clone()))
        {
            return Err("资源替换映射形成交叉覆盖".into());
        }
    }
    Ok(tokens)
}
fn sources_unchanged(files: &[VerifiedImport], tokens: &[String]) -> Result<()> {
    for (f, t) in files.iter().zip(tokens) {
        filesystem::anonymous_source(&f.file)?;
        if super::token_for(&f.file)? != *t {
            return Err("更新下载描述符已经变化".into());
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn update_verified_batch(
    root: &Path,
    id: &str,
    files: &mut [VerifiedImport],
    replacements: &[Replacement],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<()>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult> {
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
pub(super) fn update_verified_batch_with_isolation(
    root: &Path,
    id: &str,
    files: &mut [VerifiedImport],
    replacements: &[Replacement],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<()>,
    progress: &mut dyn FnMut(u64, u64),
    policy: IsolationPolicy,
) -> Result<MutationResult> {
    check(cancel)?;
    let tokens = validate_sources(files, replacements, cancel)?;
    let root = root.canonicalize().map_err(err)?;
    let _lock = crate::instance_rename_refs::root_history_lock(&root)?;
    super::ensure_local_resources_ready(&root)?;
    super::verified_batch::ensure_ready(&root)?;
    ensure_ready(&root)?;
    let (context, mut j) =
        scope::prepare_with_isolation(&root, id, files, replacements, cancel, policy)?;
    if files.is_empty() {
        commit()?;
        check(cancel)?;
        context.live(&j, true, cancel)?;
        return Ok(MutationResult {
            changed: 0,
            undo_id: None,
            message: "所选模组已经是当前版本".into(),
        });
    }
    let result = (|| {
        stage(&context, &mut j, files, &tokens, cancel, progress)?;
        context.live(&j, true, cancel)?;
        j.after = scope::expected_after(&j)?;
        j.state = State::Prepared;
        journal::write(&context, &j)?;
        check(cancel)?;
        commit()?;
        sources_unchanged(files, &tokens)?;
        check(cancel)?;
        context.live(&j, true, cancel)?;
        j.state = State::Applying;
        journal::write(&context, &j)?;
        apply(&context, &mut j, progress)?;
        j.state = State::Committed;
        journal::write(&context, &j)?;
        journal::remove_marker(&context, &j)?;
        Ok(MutationResult {
            changed: j.items.len(),
            undo_id: Some(j.id.clone()),
            message: format!("已更新 {} 个资源文件", j.items.len()),
        })
    })();
    finish_failure(&context, &j, result)
}
fn finish_failure(
    context: &Context,
    j: &Journal,
    result: Result<MutationResult>,
) -> Result<MutationResult> {
    match result {
        Ok(value) => Ok(value),
        Err(original) if matches!(j.state, State::Committed | State::Restored) => Err(original),
        Err(original) => match recovery::one(context, j) {
            Ok(()) => Err(original),
            Err(cleanup) => Err(format!("资源更新恢复失败：{cleanup}；原错误：{original}")),
        },
    }
}
struct StagedCopy {
    file: File,
    owned: Owned,
}
fn stage(
    context: &Context,
    j: &mut Journal,
    files: &mut [VerifiedImport],
    tokens: &[String],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let mut done = 0;
    let total = j
        .items
        .iter()
        .map(|i| i.size + i.old.as_ref().map_or(0, |o| o.owned.size))
        .sum();
    for index in 0..j.items.len() {
        context.check(j)?;
        check(cancel)?;
        if let Some(old) = j.items[index].old.clone() {
            let dir = context
                .resources(j, &j.items[index].kind, false)?
                .ok_or("旧资源目录缺失")?;
            let mut file = dir.regular(&old.name)?;
            if super::token_for(&file)? != old.fingerprint {
                return Err("旧资源文件已经变化".into());
            }
            let staged = copy(
                context,
                &mut file,
                old.owned.size,
                &old.owned.sha512,
                cancel,
                &mut done,
                total,
                progress,
            )?;
            if super::token_for(&file)? != old.fingerprint {
                return Err("旧资源在备份期间已经变化".into());
            }
            journal::register_copy(context, j, index, true, &staged.owned)?;
            j.items[index].old.as_mut().ok_or("旧资源登记缺失")?.backup = Some(staged.owned);
            context
                .operation
                .as_ref()
                .ok_or("更新目录缺失")?
                .link_anonymous(&staged.file, &slot("backup", index))?;
        }
        let f = &mut files[index];
        if super::token_for(&f.file)? != tokens[index] {
            return Err("更新下载描述符已经变化".into());
        }
        let staged = copy(
            context,
            &mut f.file,
            f.size,
            &f.sha512,
            cancel,
            &mut done,
            total,
            progress,
        )?;
        if super::token_for(&f.file)? != tokens[index] {
            return Err("更新下载文件复制期间已经变化".into());
        }
        journal::register_copy(context, j, index, false, &staged.owned)?;
        j.items[index].new = Some(staged.owned);
        check(cancel)?;
        context
            .operation
            .as_ref()
            .ok_or("更新目录缺失")?
            .link_anonymous(&staged.file, &slot("new", index))?;
    }
    sources_unchanged(files, tokens)
}
fn copy(
    context: &Context,
    source: &mut File,
    size: u64,
    hash: &str,
    cancel: &AtomicBool,
    done: &mut u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<StagedCopy> {
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    let mut target = operation.anonymous()?;
    source.seek(SeekFrom::Start(0)).map_err(err)?;
    let mut bytes = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        check(cancel)?;
        let n = source.read(&mut buffer).map_err(err)?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .filter(|n| *n <= size)
            .ok_or("资源备份复制超过声明大小")?;
        target.write_all(&buffer[..n]).map_err(err)?;
        *done += n as u64;
        progress(*done, total);
    }
    if bytes != size {
        return Err("资源备份复制大小不符".into());
    }
    target.sync_all().map_err(err)?;
    let owned = filesystem::verify_file(&mut target, None, size, hash, Some(cancel))?;
    Ok(StagedCopy {
        file: target,
        owned,
    })
}
fn apply(context: &Context, j: &mut Journal, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    scope::create_targets(context, j)?;
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    let total = j.items.iter().map(|i| i.size).sum();
    let mut done = 0u64;
    for index in 0..j.items.len() {
        context.check(j)?;
        let item = &j.items[index];
        let dir = context
            .resources(j, &item.kind, true)?
            .ok_or("资源目录缺失")?;
        if let Some(old) = &item.old {
            verify_owned(&dir, &old.name, &old.owned)?;
            operation.absent(&slot("old", index))?;
            dir.move_directory(&old.name, operation, &slot("old", index))?;
        }
        dir.absent(&item.name)?;
        dir.absent(&counterpart(&item.name))?;
        verify_owned(
            operation,
            &slot("new", index),
            item.new.as_ref().ok_or("新资源未登记")?,
        )?;
        operation.move_directory(&slot("new", index), &dir, &item.name)?;
        done += item.size;
        progress(done, total);
    }
    context.inventory_matches(j, false, None)?;
    Ok(())
}
pub(super) fn updates_history(root: &Path, id: &str) -> Result<Vec<UpdateHistory>> {
    scope::history(root, id)
}
pub(super) fn restore_update(
    root: &Path,
    id: &str,
    undo_id: &str,
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<()>,
) -> Result<MutationResult> {
    check(cancel)?;
    if !valid_id(undo_id) {
        return Err("资源更新撤销标识无效".into());
    }
    let root = root.canonicalize().map_err(err)?;
    let _lock = crate::instance_rename_refs::root_history_lock(&root)?;
    super::ensure_local_resources_ready(&root)?;
    super::verified_batch::ensure_ready(&root)?;
    ensure_ready(&root)?;
    let context = Context::existing(&root, id, undo_id)?;
    let mut j = journal::read(&context)?;
    if j.state != State::Committed {
        return Err("此更新历史当前不能撤销，请先恢复未完成操作".into());
    }
    context.validate_actual_scope(&j)?;
    context.inventory_matches(&j, false, Some(cancel))?;
    recovery::verify_committed_private(&context, &j)?;
    journal::create_marker(&context, &mut j)?;
    j.state = State::UndoPrepared;
    let result = (|| {
        journal::write(&context, &j)?;
        check(cancel)?;
        commit()?;
        check(cancel)?;
        context.validate_actual_scope(&j)?;
        context.inventory_matches(&j, false, None)?;
        j.state = State::UndoApplying;
        journal::write(&context, &j)?;
        undo(&context, &j)?;
        j.state = State::Restored;
        journal::write(&context, &j)?;
        recovery::cleanup_restored(&context, &j)?;
        Ok(MutationResult {
            changed: j.items.len(),
            undo_id: None,
            message: format!("已撤销 {} 个资源文件的更新", j.items.len()),
        })
    })();
    finish_failure(&context, &j, result)
}
fn undo(context: &Context, j: &Journal) -> Result<()> {
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    // Move every new output aside before restoring originals, keeping same-name
    // replacements and additions recoverable with no arbitrary overwrite.
    for (index, item) in j.items.iter().enumerate() {
        context.check(j)?;
        let dir = context
            .resources(j, &item.kind, true)?
            .ok_or("资源目录缺失")?;
        verify_owned(
            &dir,
            &item.name,
            item.new.as_ref().ok_or("新资源所有权缺失")?,
        )?;
        operation.absent(&slot("new", index))?;
        dir.move_directory(&item.name, operation, &slot("new", index))?;
    }
    for (index, item) in j.items.iter().enumerate() {
        if let Some(old) = &item.old {
            context.check(j)?;
            let dir = context
                .resources(j, &item.kind, true)?
                .ok_or("资源目录缺失")?;
            dir.absent(&old.name)?;
            dir.absent(&counterpart(&old.name))?;
            verify_owned(operation, &slot("old", index), &old.owned)?;
            operation.move_directory(&slot("old", index), &dir, &old.name)?;
        }
    }
    // Keep created directory inodes until Restored is durable. An interrupted
    // undo can then put the new files back into the same bound directories.
    if scope::inventory(context, &j.scope, None)? != j.before {
        return Err("撤销后的资源清单与原清单不一致".into());
    }
    Ok(())
}
pub(super) fn ensure_ready(root: &Path) -> Result<()> {
    scope::ensure_ready(root)
}
pub(super) fn recover_root(root: &Path) -> Result<MutationResult> {
    recovery::root(root)
}
#[cfg(test)]
mod tests;
