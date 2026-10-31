//! One recoverable import for downloaded mods, resource packs and shader packs.
//!
//! Inputs are verified generated-source capabilities, never source paths. A single
//! root history lock covers all kinds. Preparation writes only the root's
//! private batch store; target directories stay unchanged until `commit` has
//! closed task cancellation admission and rechecked the application plan.
//!
//! Staging -> Prepared -> Committed. Before commit, recovery removes only
//! registered inodes whose complete bytes still match. After a durable commit
//! it keeps outputs and finishes owned staging cleanup. Any conflict retains
//! the journal and reports Error, even when the cancellation flag is set.
use super::{download_token, safe_name, token_for, MutationResult};
use pcl_core::content_scope::IsolationPolicy;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
#[path = "verified_batch/archive.rs"]
pub(super) mod archive;
#[path = "verified_batch/filesystem.rs"]
pub(super) mod filesystem;
#[path = "verified_batch/preflight.rs"]
pub(crate) mod preflight;
use filesystem::{Dir, Key, Owned};
type Result<T> = std::result::Result<T, String>;
const STORE: &str = "resource-batches";
const SCHEMA: u32 = if cfg!(windows) { 2 } else { 1 };
const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_BATCH_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
const CANCELLED: &str = "资源下载已取消";

/// Kept public for the parent module's re-export; this child module is private.
/// Generated-source ownership transfers from the download service.
pub struct VerifiedImport {
    pub kind: String,
    pub file_name: String,
    pub file: crate::modrinth_install::source::Input,
    pub size: u64,
    pub sha512: String,
}
pub(crate) fn retire_sources(files: &mut [VerifiedImport]) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    crate::modrinth_install::source::retire_batch(files.iter_mut().map(|input| &mut input.file))?;
    #[cfg(target_os = "linux")]
    let _ = files;
    Ok(())
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
fn name_ok(kind: &str, name: &str) -> Result<()> {
    safe_name(name)?;
    if name.contains(':')
        || name.starts_with(".pcl-")
        || name.ends_with(".disabled")
        || !match kind {
            "mods" => name.ends_with(".jar"),
            "resourcepacks" | "shaderpacks" => name.ends_with(".zip"),
            _ => false,
        }
    {
        return Err("资源类型或下载文件名无效".into());
    }
    Ok(())
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 128
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn op_id() -> String {
    format!("b-{}", super::new_id())
}
fn valid_op(id: &str) -> bool {
    id.strip_prefix("b-").is_some_and(super::valid_operation_id)
}
fn slot(index: usize) -> String {
    format!("item-{index:04}")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Destination {
    relative: String,
    before: Option<Key>,
    created: Option<Key>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    root: PathBuf,
    root_key: Key,
    versions_key: Key,
    instance_key: Key,
    instance_id: String,
    profile_token: String,
    destinations: BTreeMap<String, Destination>,
    #[serde(default)]
    policy: IsolationPolicy,
}
impl Scope {
    #[cfg(test)]
    fn capture(root: &Path, id: &str, kinds: &BTreeSet<String>) -> Result<(Self, Dir)> {
        Self::capture_with_isolation(root, id, kinds, IsolationPolicy::Automatic)
    }
    fn capture_with_isolation(
        root: &Path,
        id: &str,
        kinds: &BTreeSet<String>,
        policy: IsolationPolicy,
    ) -> Result<(Self, Dir)> {
        pcl_core::identifier(id)?;
        safe_name(id)?;
        let root = root.canonicalize().map_err(error)?;
        let root_dir = Dir::open(&root)?;
        let versions = root_dir.child("versions")?;
        let instance = versions.child(id)?;
        let profile_token = token_for(&instance.regular(&format!("{id}.json"))?)?;
        let mut destinations = BTreeMap::new();
        for kind in kinds {
            let target = crate::ui_data::resource_dir_with_isolation(&root, id, kind, policy)?;
            // Journals and retained traversal use slash-separated leaves on
            // every host; native Windows path separators are not wire bytes.
            let relative = target
                .strip_prefix(&root)
                .map_err(|_| "资源目录超出游戏目录")?
                .components()
                .map(|component| match component {
                    Component::Normal(name) => name.to_str().ok_or("资源目录不是 UTF-8"),
                    _ => Err("资源目录隔离规则无效"),
                })
                .collect::<std::result::Result<Vec<_>, _>>()?
                .join("/");
            if relative != *kind && relative != format!("versions/{id}/{kind}") {
                return Err("资源目录隔离规则无效".into());
            }
            let before = root_dir
                .optional_at(&relative)?
                .map(|d| d.key())
                .transpose()?;
            destinations.insert(
                kind.clone(),
                Destination {
                    relative,
                    before,
                    created: None,
                },
            );
        }
        let scope = Self {
            root,
            root_key: root_dir.key()?,
            versions_key: versions.key()?,
            instance_key: instance.key()?,
            instance_id: id.into(),
            profile_token,
            destinations,
            policy,
        };
        scope.live(&root_dir, false)?;
        Ok((scope, root_dir))
    }
    fn base(&self) -> Result<Dir> {
        let root = Dir::open(&self.root)?;
        if root.key()? != self.root_key
            || root.child("versions")?.key()? != self.versions_key
            || root.child("versions")?.child(&self.instance_id)?.key()? != self.instance_key
        {
            return Err("绑定的实例或游戏目录已经变化".into());
        }
        Ok(root)
    }
    fn live(&self, root: &Dir, created: bool) -> Result<()> {
        if root.key()? != self.root_key || self.base()?.key()? != self.root_key {
            return Err("绑定的游戏目录已经变化".into());
        }
        let instance = root.child("versions")?.child(&self.instance_id)?;
        if token_for(&instance.regular(&format!("{}.json", self.instance_id))?)?
            != self.profile_token
        {
            return Err("实例版本文件已经变化，请重新检查资源安装计划".into());
        }
        for (kind, d) in &self.destinations {
            let expected = self.root.join(&d.relative);
            if crate::ui_data::resource_dir_with_isolation(
                &self.root,
                &self.instance_id,
                kind,
                self.policy,
            )? != expected
            {
                return Err("实例资源隔离目录已经变化".into());
            }
            let actual = root
                .optional_at(&d.relative)?
                .map(|d| d.key())
                .transpose()?;
            let key = d
                .before
                .as_ref()
                .or(if created { d.created.as_ref() } else { None });
            if actual.as_ref() != key {
                return Err("资源目录已经被创建、移走或替换".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Staging,
    Prepared,
    Committed,
    #[cfg(windows)]
    Finished,
    #[cfg(windows)]
    RolledBack,
}
impl State {
    #[cfg(windows)]
    fn terminal(&self) -> bool {
        matches!(self, Self::Finished | Self::RolledBack)
    }
    fn partial(&self) -> bool {
        if *self == Self::Staging {
            return true;
        }
        #[cfg(windows)]
        if *self == Self::RolledBack {
            return true;
        }
        false
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    kind: String,
    file_name: String,
    size: u64,
    sha512: String,
    owned: Option<Owned>,
    // Durable publication intent, written before linkat. Prepared recovery
    // accepts missing files but refuses a foreign inode at an attempted path.
    published: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    operation_id: String,
    operation_key: Key,
    pcl_key: Key,
    store_key: Key,
    stage_key: Option<Key>,
    scope: Scope,
    state: State,
    items: Vec<Item>,
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
fn operation_binding(operation: &Dir, j: &Journal) -> Result<Dir> {
    let root = j.scope.base()?;
    let store = storage(&root, false)?.ok_or("资源批次记录目录已经变化")?;
    if root.child(".pcl-linux")?.key()? != j.pcl_key
        || store.key()? != j.store_key
        || operation.key()? != j.operation_key
        || store.child(&j.operation_id)?.key()? != j.operation_key
    {
        return Err("资源批次记录目录已经变化".into());
    }
    Ok(root)
}
fn target(root: &Dir, j: &Journal, kind: &str) -> Result<Option<Dir>> {
    let d = j.scope.destinations.get(kind).ok_or("资源批次类型未登记")?;
    let current = root.optional_at(&d.relative)?;
    if let Some(dir) = &current {
        if Some(dir.key()?).as_ref() != d.before.as_ref().or(d.created.as_ref()) {
            return Err("资源批次目标目录身份冲突，已保留文件".into());
        }
    } else if d.before.is_some() {
        return Err("原资源目录已被移走，已保留记录".into());
    }
    Ok(current)
}
fn absent_targets(root: &Dir, j: &Journal, created: bool) -> Result<()> {
    j.scope.live(root, created)?;
    for item in &j.items {
        if let Some(dir) = target(root, j, &item.kind)? {
            dir.absent(&item.file_name)?;
            dir.absent(&format!("{}.disabled", item.file_name))?;
        }
    }
    Ok(())
}
pub(in crate::resource_ops) fn prepare_sources<'a>(
    sources: impl IntoIterator<Item = preflight::SourceSpec<'a>>,
    cancel: &AtomicBool,
) -> Result<preflight::PreparedSources<'a>> {
    let mut names = BTreeSet::new();
    preflight::prepare(
        sources,
        preflight::Rules {
            max_files: MAX_FILES,
            max_file_bytes: MAX_FILE_BYTES,
            max_batch_bytes: MAX_BATCH_BYTES,
            count_error: "一次最多下载 512 个资源文件",
            file_error: "下载资源重复或大小、SHA512 声明无效",
            overflow_error: "资源下载批次超过 8 GiB 限制",
            total_error: "资源下载批次超过 8 GiB 限制",
        },
        |source| {
            check(cancel)?;
            name_ok(source.kind, source.name)?;
            if !valid_hash(source.sha512) || !names.insert((source.kind, source.name)) {
                return Err("下载资源重复或大小、SHA512 声明无效".into());
            }
            Ok(())
        },
    )
}
fn source_tokens(files: &mut [VerifiedImport], cancel: &AtomicBool) -> Result<Vec<String>> {
    prepare_sources(
        files.iter().map(|f| preflight::SourceSpec {
            kind: &f.kind,
            name: &f.file_name,
            size: f.size,
            sha512: &f.sha512,
        }),
        cancel,
    )?;
    let mut keys = BTreeSet::new();
    let mut tokens = Vec::new();
    for f in files {
        check(cancel)?;
        filesystem::anonymous_source(&f.file)?;
        if !keys.insert(filesystem::key_file(&f.file)?) {
            return Err("下载资源描述符重复".into());
        }
        let (_, token) =
            filesystem::verify_file_snapshot(&mut f.file, None, f.size, &f.sha512, Some(cancel))?;
        archive::validate(&mut f.file, cancel)?;
        if download_token(&f.file, Some(cancel))? != token {
            return Err("下载资源在验证期间已经变化".into());
        }
        tokens.push(token);
    }
    Ok(tokens)
}
pub(crate) fn recheck_sources(
    files: &[VerifiedImport],
    tokens: &[String],
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    for (f, token) in files.iter().zip(tokens) {
        filesystem::anonymous_source(&f.file)?;
        if download_token(&f.file, cancel)? != *token {
            return Err("已校验的下载资源描述符已经变化".into());
        }
    }
    Ok(())
}

#[path = "verified_batch/journal.rs"]
mod journal;
use journal::{
    read_journal, register_directory, register_owned, register_published, write_journal,
};

fn setup(root: &Dir, scope: Scope, files: &[VerifiedImport]) -> Result<(Dir, Journal)> {
    let store = storage(root, true)?.ok_or("资源批次记录目录缺失")?;
    let operation_id = op_id();
    let operation = store.mkdir(&operation_id)?.with_documents()?;
    let mut j = Journal {
        schema: SCHEMA,
        operation_id,
        operation_key: operation.key()?,
        pcl_key: root.child(".pcl-linux")?.key()?,
        store_key: store.key()?,
        stage_key: None,
        scope,
        state: State::Staging,
        items: files
            .iter()
            .map(|f| Item {
                kind: f.kind.clone(),
                file_name: f.file_name.clone(),
                size: f.size,
                sha512: f.sha512.clone(),
                owned: None,
                published: false,
            })
            .collect(),
    };
    if let Err(e) = write_journal(&operation, &j) {
        return match store.unlink(&j.operation_id, true) {
            Ok(()) => Err(e),
            Err(c) => Err(format!("取消清理失败：{c}；原错误：{e}")),
        };
    }
    let result = (|| {
        let stage = operation.mkdir("files")?;
        j.stage_key = Some(stage.key()?);
        write_journal(&operation, &j)?;
        Ok(())
    })();
    if let Err(e) = result {
        return match recover_one(&operation, &j) {
            Ok(()) => Err(e),
            Err(c) => Err(format!("取消清理失败：{c}；原错误：{e}")),
        };
    }
    Ok((operation, j))
}
fn stage(
    operation: &Dir,
    j: &mut Journal,
    files: &mut [VerifiedImport],
    tokens: &[String],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let destination = operation.child("files")?;
    if Some(destination.key()?) != j.stage_key {
        return Err("资源批次暂存目录已变化".into());
    }
    let total = j.items.iter().map(|i| i.size).sum();
    let mut done = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    let prepared = prepare_sources(
        files.iter().map(|f| preflight::SourceSpec {
            kind: &f.kind,
            name: &f.file_name,
            size: f.size,
            sha512: &f.sha512,
        }),
        cancel,
    )?;
    let mut copies = preflight::AllocationPermit::allocate_batch_with(
        prepared.into_permits().collect(),
        |admitted| {
            let requests = admitted
                .iter()
                .map(|(index, source)| (slot(*index), source.size))
                .collect::<Vec<_>>();
            destination.stage_batch_for(&requests, cancel)
        },
    )?;
    for (index, (f, copy)) in files.iter_mut().zip(copies.iter_mut()).enumerate() {
        check(cancel)?;
        operation_binding(operation, j)?;
        f.file.seek(SeekFrom::Start(0)).map_err(error)?;
        let before = download_token(&f.file, Some(cancel))?;
        let mut size = 0u64;
        loop {
            check(cancel)?;
            let n = f.file.read(&mut buffer).map_err(error)?;
            if n == 0 {
                break;
            }
            size = size
                .checked_add(n as u64)
                .filter(|n| *n <= f.size)
                .ok_or("下载资源复制时超过声明大小")?;
            copy.write_all(&buffer[..n]).map_err(error)?;
            done += n as u64;
            progress(done, total);
        }
        if size != f.size
            || before != tokens[index]
            || download_token(&f.file, Some(cancel))? != before
        {
            return Err("下载资源在复制期间已经变化".into());
        }
        copy.sync_all().map_err(error)?;
        let owned = filesystem::verify_file(copy, None, f.size, &f.sha512, Some(cancel))?;
        archive::validate(copy, cancel)?;
        register_owned(operation, j, index, &owned)?;
        j.items[index].owned = Some(owned);
        check(cancel)?;
        destination.link_anonymous(copy, &slot(index))?;
    }
    recheck_sources(files, tokens, Some(cancel))?;
    Ok(())
}
fn verify_stage(operation: &Dir, j: &Journal, complete: bool) -> Result<Option<Dir>> {
    let Some(stage) = operation.optional("files")? else {
        if complete {
            return Err("资源批次暂存目录缺失".into());
        }
        return Ok(None);
    };
    if let Some(key) = &j.stage_key {
        if stage.key()? != *key {
            return Err("资源批次暂存目录身份冲突".into());
        }
    } else if !stage.names()?.is_empty() {
        return Err("未登记资源暂存目录包含文件".into());
    }
    for name in stage.names()? {
        let index = name
            .strip_prefix("item-")
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|i| slot(*i) == name)
            .ok_or("资源暂存目录包含未登记文件")?;
        let item = j.items.get(index).ok_or("资源暂存文件编号无效")?;
        let owned = item
            .owned
            .as_ref()
            .ok_or("资源暂存文件缺少持久所有权登记")?;
        filesystem::verify_named(&stage, &name, owned)?;
    }
    if complete {
        for (index, item) in j.items.iter().enumerate() {
            filesystem::verify_named(
                &stage,
                &slot(index),
                item.owned.as_ref().ok_or("资源文件未完成登记")?,
            )?;
        }
    }
    Ok(Some(stage))
}
fn prepare_directories(root: &Dir, operation: &Dir, j: &mut Journal) -> Result<()> {
    let kinds: Vec<_> = j.scope.destinations.keys().cloned().collect();
    for kind in kinds {
        operation_binding(operation, j)?;
        let d = &j.scope.destinations[&kind];
        if d.before.is_some() {
            target(root, j, &kind)?;
            continue;
        }
        let (parent, name) = root.parent(&d.relative)?;
        parent.absent(&name)?;
        // Prepare the empty directory in the owned operation, then register
        // its inode before moving it into the previously absent target path.
        let private = format!("target-{kind}");
        let created = operation.mkdir(&private)?;
        let key = created.key()?;
        register_directory(operation, j, &kind, &key)?;
        j.scope
            .destinations
            .get_mut(&kind)
            .ok_or("资源目录登记缺失")?
            .created = Some(key.clone());
        operation_binding(operation, j)?;
        if operation.child(&private)?.key()? != key {
            return Err("资源批次新目录已被替换".into());
        }
        operation.move_directory(&private, &parent, &name)?;
    }
    Ok(())
}
fn verify_outputs(root: &Dir, j: &Journal, committed: bool) -> Result<()> {
    for item in &j.items {
        let Some(dir) = target(root, j, &item.kind)? else {
            if committed {
                return Err("已发布资源目录已移走".into());
            }
            continue;
        };
        if committed {
            dir.absent(&format!("{}.disabled", item.file_name))?;
        }
        match dir.stat(&item.file_name)? {
            None if !committed => {}
            Some(_)
                if item.owned.as_ref().is_some_and(|owned| {
                    filesystem::verify_named(&dir, &item.file_name, owned).is_ok()
                }) => {}
            Some(_) if !committed && !item.published => {} // foreign collision: preserve it; only our published output can be removed
            _ => return Err("已发布资源的内容或 inode 已变化，已保留恢复记录".into()),
        }
    }
    Ok(())
}
fn publish(
    root: &Dir,
    operation: &Dir,
    j: &mut Journal,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    absent_targets(root, j, false)?;
    let stage = verify_stage(operation, j, true)?.ok_or("资源暂存目录缺失")?;
    prepare_directories(root, operation, j)?;
    let total = j.items.iter().map(|i| i.size).sum();
    for index in 0..j.items.len() {
        operation_binding(operation, j)?;
        j.scope.live(root, true)?;
        let item = &j.items[index];
        let dir = target(root, j, &item.kind)?.ok_or("资源目录未创建")?;
        dir.absent(&item.file_name)?;
        dir.absent(&format!("{}.disabled", item.file_name))?;
        filesystem::verify_named(
            &stage,
            &slot(index),
            item.owned.as_ref().ok_or("资源所有权未登记")?,
        )?;
        let file_name = item.file_name.clone();
        register_published(operation, j, index)?;
        j.items[index].published = true;
        stage.link(&slot(index), &dir, &file_name)?;
        progress(total, total);
    }
    verify_outputs(root, j, true)?;
    j.state = State::Committed;
    // A rename may succeed even if its following fsync fails. Preserve this
    // uncertain commit for recovery instead of rolling back a disk Committed.
    write_journal(operation, j)
}
fn cleanup_stage(operation: &Dir, j: &Journal) -> Result<()> {
    let _root = operation_binding(operation, j)?;
    if let Some(stage) = verify_stage(operation, j, false)? {
        let names = stage.names()?;
        for name in names {
            let index = name
                .strip_prefix("item-")
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or("暂存文件编号无效")?;
            filesystem::verify_named(
                &stage,
                &name,
                j.items[index].owned.as_ref().ok_or("暂存所有权缺失")?,
            )?;
            stage.unlink(&name, false)?;
        }
        operation_binding(operation, j)?;
        if operation.child("files")?.key()? != stage.key()? {
            return Err("资源批次暂存目录已被替换，拒绝清理".into());
        }
        operation.unlink("files", true)?;
    }
    for (kind, d) in &j.scope.destinations {
        let name = format!("target-{kind}");
        if let Some(dir) = operation.optional(&name)? {
            if d.created
                .as_ref()
                .is_some_and(|key| dir.key().as_ref() != Ok(key))
                || !dir.names()?.is_empty()
            {
                return Err("私有资源目录身份冲突，已保留".into());
            }
            operation_binding(operation, j)?;
            if operation.child(&name)?.key()? != dir.key()? {
                return Err("私有资源目录已替换".into());
            }
            operation.unlink(&name, true)?;
        }
    }
    journal::remove_registrations(operation, j)?;
    let names = operation.names()?;
    if names.iter().any(|name| name != "journal.json") {
        return Err("资源批次记录目录包含未知文件，已保留".into());
    }
    operation_binding(operation, j)?;
    let on_disk = read_journal(operation, &j.operation_id)?;
    if on_disk.scope.root != j.scope.root
        || on_disk.scope.root_key != j.scope.root_key
        || on_disk.scope.instance_key != j.scope.instance_key
        || on_disk.items.len() != j.items.len()
        || on_disk.items.iter().zip(&j.items).any(|(a, b)| {
            a.kind != b.kind
                || a.file_name != b.file_name
                || a.size != b.size
                || a.sha512 != b.sha512
        })
    {
        return Err("资源批次日志绑定已变化，拒绝清理".into());
    }
    #[cfg(windows)]
    {
        operation.check_cleanup()?;
        let mut terminal = j.clone();
        terminal.state = if j.state == State::Committed {
            State::Finished
        } else {
            State::RolledBack
        };
        write_journal(operation, &terminal)?;
        return verify_terminal(operation, &terminal);
    }
    #[cfg(not(windows))]
    {
        operation.unlink("journal.json", false)?;
        let store = storage(&_root, false)?.ok_or("资源批次记录目录缺失")?;
        if store.child(&j.operation_id)?.key()? != j.operation_key || !operation.names()?.is_empty()
        {
            return Err("资源批次目录已替换，拒绝清理".into());
        }
        store.unlink(&j.operation_id, true)
    }
}
#[cfg(windows)]
fn verify_terminal(operation: &Dir, j: &Journal) -> Result<()> {
    if !j.state.terminal() {
        return Err("资源批次不是已退役终态".into());
    }
    operation_binding(operation, j)?;
    if operation.names()? != ["journal.json"] {
        return Err("资源批次终态包含未知内容或未清理的文件，已保留".into());
    }
    let current = read_journal(operation, &j.operation_id)?;
    if current.state != j.state
        || current.operation_key != j.operation_key
        || current.scope.root_key != j.scope.root_key
    {
        return Err("资源批次终态记录已变化".into());
    }
    Ok(())
}
fn rollback(root: &Dir, operation: &Dir, j: &Journal) -> Result<()> {
    if j.state != State::Staging {
        verify_outputs(root, j, false)?;
        for item in &j.items {
            if let Some(dir) = target(root, j, &item.kind)? {
                if dir.stat(&item.file_name)?.is_some() {
                    let owned = item.owned.as_ref().ok_or("资源文件所有权未登记")?;
                    if filesystem::verify_named(&dir, &item.file_name, owned).is_ok() {
                        filesystem::verify_named(&dir, &item.file_name, owned)?;
                        dir.unlink(&item.file_name, false)?;
                    } else if item.published {
                        return Err("已发布资源已被外部替换，拒绝清理".into());
                    }
                }
            }
        }
        for (kind, d) in &j.scope.destinations {
            if d.before.is_some() {
                continue;
            }
            if let Some(dir) = target(root, j, kind)? {
                if !dir.names()?.is_empty() {
                    return Err("新建资源目录包含外部内容，已保留".into());
                }
                let (parent, name) = root.parent(&d.relative)?;
                operation_binding(operation, j)?;
                if parent.child(&name)?.key()? != dir.key()? {
                    return Err("新资源目录清理前已替换".into());
                }
                parent.unlink(&name, true)?;
            }
        }
    }
    cleanup_stage(operation, j)
}
fn recover_one(operation: &Dir, j: &Journal) -> Result<()> {
    let root = operation_binding(operation, j)?;
    match j.state {
        State::Committed => {
            verify_outputs(&root, j, true)?;
            cleanup_stage(operation, j)
        }
        State::Staging | State::Prepared => rollback(&root, operation, j),
        #[cfg(windows)]
        State::Finished | State::RolledBack => verify_terminal(operation, j),
    }
}

#[cfg(test)]
pub(super) fn import_verified_batch(
    root: &Path,
    id: &str,
    files: &mut [VerifiedImport],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<()>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<MutationResult> {
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
pub(super) fn import_verified_batch_with_isolation(
    root: &Path,
    id: &str,
    files: &mut [VerifiedImport],
    cancel: &AtomicBool,
    commit: &mut dyn FnMut() -> Result<()>,
    progress: &mut dyn FnMut(u64, u64),
    policy: IsolationPolicy,
) -> Result<MutationResult> {
    check(cancel)?;
    let tokens = source_tokens(files, cancel)?;
    let kinds = files.iter().map(|f| f.kind.clone()).collect();
    let (scope, root_dir) = Scope::capture_with_isolation(root, id, &kinds, policy)?;
    let _history_lock = crate::instance_rename_refs::root_history_lock(&scope.root)?;
    super::ensure_local_resources_ready(&scope.root)?;
    ensure_ready(&scope.root)?;
    super::ensure_updates_ready(&scope.root)?;
    scope.live(&root_dir, false)?;
    if files.is_empty() {
        commit()?;
        check(cancel)?;
        scope.live(&root_dir, false)?;
        return Ok(MutationResult {
            changed: 0,
            undo_id: None,
            message: "所选资源已存在且校验一致".into(),
        });
    }
    let (operation, mut j) = setup(&root_dir, scope, files)?;
    let result = (|| {
        stage(&operation, &mut j, files, &tokens, cancel, progress)?;
        absent_targets(&root_dir, &j, false)?;
        verify_stage(&operation, &j, true)?;
        j.state = State::Prepared;
        write_journal(&operation, &j)?;
        check(cancel)?;
        commit()?;
        recheck_sources(files, &tokens, None)?;
        check(cancel)?;
        // No more cancellation checks after the admitted commit. Ordinary I/O
        // and content conflicts still fail and preserve their own error text.
        publish(&root_dir, &operation, &mut j, progress)?;
        recover_one(&operation, &j).map_err(|e| format!("取消清理失败：{e}"))?;
        Ok(MutationResult {
            changed: j.items.len(),
            undo_id: None,
            message: format!("已导入 {} 个资源文件", j.items.len()),
        })
    })();
    match result {
        Ok(v) => Ok(v),
        Err(original) if j.state == State::Committed => Err(original),
        Err(original) => match recover_one(&operation, &j) {
            Ok(()) => Err(original),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{original}")),
        },
    }
}

pub(super) fn ensure_ready(root: &Path) -> Result<()> {
    let path = root.canonicalize().map_err(error)?;
    let root = Dir::open(&path)?;
    let Some(store) = storage(&root, false)? else {
        return Ok(());
    };
    for name in store.names()? {
        if !valid_op(&name) {
            return Err("资源批次目录包含未知记录".into());
        }
        let operation = store.child(&name)?.with_documents()?;
        if operation.stat("journal.json")?.is_some() {
            let j = read_journal(&operation, &name)?;
            if j.scope.root != path || j.scope.root_key != root.key()? {
                return Err("资源批次记录与当前游戏目录不匹配".into());
            }
            #[cfg(windows)]
            if j.state.terminal() {
                verify_terminal(&operation, &j)?;
                continue;
            }
        }
        return Err("存在未完成的资源下载批次，请先恢复后再操作或启动".into());
    }
    Ok(())
}
pub(super) fn recover_root(root: &Path) -> Result<MutationResult> {
    let path = root.canonicalize().map_err(error)?;
    let dir = Dir::open(&path)?;
    let _history_lock = crate::instance_rename_refs::root_history_lock(&path)?;
    let Some(store) = storage(&dir, false)? else {
        return Ok(MutationResult {
            changed: 0,
            undo_id: None,
            message: "没有待恢复的资源下载批次".into(),
        });
    };
    let names = store.names()?;
    if !names.is_empty() {
        super::ensure_local_resources_ready(&path)?;
    }
    let mut count = 0;
    for name in names {
        if !valid_op(&name) {
            return Err("资源批次目录包含未知记录".into());
        }
        let operation = store.child(&name)?.with_documents()?;
        if operation.stat("journal.json")?.is_none() {
            if operation.names()?.is_empty() {
                let current = Dir::open(&path)?;
                if current.key()? != dir.key()?
                    || storage(&current, false)?.ok_or("资源批次目录缺失")?.key()? != store.key()?
                    || store.child(&name)?.key()? != operation.key()?
                {
                    return Err("未登记资源批次目录已被替换".into());
                }
                store.unlink(&name, true)?;
                count += 1;
                continue;
            }
            return Err("未登记资源批次目录包含文件，已保留".into());
        }
        let j = read_journal(&operation, &name)?;
        if j.scope.root != path || j.scope.root_key != dir.key()? {
            return Err("资源批次记录与当前游戏目录不匹配".into());
        }
        #[cfg(windows)]
        if j.state.terminal() {
            verify_terminal(&operation, &j)?;
            continue;
        }
        recover_one(&operation, &j).map_err(|e| format!("取消清理失败：{e}"))?;
        count += 1;
    }
    ensure_ready(&path)?;
    Ok(MutationResult {
        changed: count,
        undo_id: None,
        message: format!("已恢复 {count} 个资源下载批次"),
    })
}
pub(super) fn recover_pending(root: &Path, id: &str) -> Result<()> {
    pcl_core::identifier(id)?;
    recover_root(root).map(|_| ())
}

#[cfg(test)]
#[path = "verified_batch/tests.rs"]
mod tests;
