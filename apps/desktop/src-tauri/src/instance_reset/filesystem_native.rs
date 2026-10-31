//! Native reset capabilities. User originals only move under Prepared; the
//! generated-source registry and business journal have separate fixed roles.
use super::{
    check, err, limit, name_ok, write_journal, Journal, Result, MAX_DEPTH, MAX_FILES, MAX_JAR,
    MAX_JOURNAL, MAX_JSON,
};
#[cfg(windows)]
use crate::launcher_local::filesystem::documents::DocumentPresence;
use crate::launcher_local::filesystem::{documents::Document, Dir as LocalDir};
use pcl_core::retained_fs::{
    self, Directory, Identity, Kind, Leaf, Links, Lock, Metadata, Traversal, WorkspaceRemoval,
};
use pcl_install::staging::{Area, DiscardOutcome, Limits, Outcome, Ticket};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

pub(super) const SCHEMA: u32 = 2;
pub(super) const REVISION_PREFIX: &str = "reset-native-v2-";
pub(super) const JOURNAL_LIMIT: u64 = 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Format {
    RetainedIdentityV1,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct DirKey {
    format: Format,
    identity: Identity,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    pub(super) size: u64,
    pub(super) hash: String,
    metadata: Metadata,
}
impl Snapshot {
    pub(super) fn stable_matches(&self, other: &Self) -> bool {
        // Publication/rename and the last writer close have an explicit time
        // handoff. Reads remain exact; recovery always hashes the entire file.
        self.size == other.size
            && self.hash == other.hash
            && self.metadata.same_object(&other.metadata)
            && self.metadata.len == other.metadata.len
            && self.metadata.links == other.metadata.links
            && self.metadata.security == other.metadata.security
            && self.metadata.reparse_tag == other.metadata.reparse_tag
    }
}
pub(super) fn valid_snapshot(snapshot: &Snapshot) -> Result<()> {
    let m = &snapshot.metadata;
    if snapshot.size != m.len
        || m.kind != Kind::Regular
        || m.links != 1
        || m.reparse_tag.is_some()
        || m.modified.nanos >= 1_000_000_000
        || m.changed.nanos >= 1_000_000_000
        || !match &m.identity {
            #[cfg(windows)]
            Identity::Windows { .. } => true,
            #[cfg(unix)]
            Identity::Unix { .. } => true,
            _ => false,
        }
    {
        return Err("实例重置原生快照无效".into());
    }
    Ok(())
}
pub(super) fn valid_key(key: &DirKey) -> Result<()> {
    let valid = match &key.identity {
        #[cfg(windows)]
        Identity::Windows { file, .. } => *file != [0; 16],
        #[cfg(unix)]
        Identity::Unix { inode, .. } => *inode != 0,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("实例重置目录身份平台或编码无效".into())
    }
}
fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(err)
}
fn leaf(name: &str) -> Result<Leaf> {
    name_ok(name)?;
    io(Leaf::new(name))
}
fn may(error: impl std::fmt::Display) -> String {
    format!("MayHaveCommitted：实例重置记录可能已提交，原件与证据已保留：{error}")
}
pub(super) struct Dir {
    directory: Directory,
}
impl Dir {
    pub(super) fn open(path: &Path) -> Result<Self> {
        io(Directory::open_absolute(path)).map(|directory| Self { directory })
    }
    pub(super) fn child(&self, name: &str) -> Result<Self> {
        io(self
            .directory
            .child(&leaf(name)?, Traversal::SameFilesystem))
        .map(|directory| Self { directory })
    }
    pub(super) fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(super) fn create_dir(&self, name: &str) -> Result<Self> {
        let directory = io(self
            .directory
            .create_private(&leaf(name)?, Traversal::SameFilesystem))?;
        self.sync()?;
        Ok(Self { directory })
    }
    pub(super) fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(dir) => Ok(dir),
            None => self.create_dir(name),
        }
    }
    pub(super) fn stat(&self, name: &str) -> Result<Option<Metadata>> {
        io(self.directory.entry(&leaf(name)?))
    }
    pub(super) fn regular(&self, name: &str) -> Result<File> {
        io(self.directory.open_regular(&leaf(name)?, Links::Single))
    }
    pub(super) fn cache_file(&self, name: &str) -> Result<File> {
        io(self.directory.open_regular(&leaf(name)?, Links::Any))
    }
    #[cfg(test)]
    pub(super) fn create_file(&self, name: &str) -> Result<File> {
        io(self.directory.create_file(&leaf(name)?))
    }
    pub(super) fn names(&self) -> Result<Vec<String>> {
        let mut names = io(self.directory.names(MAX_FILES))?
            .into_iter()
            .map(|name| {
                let name = name.into_string().map_err(|_| "重置目录包含无效文件名")?;
                name_ok(&name)?;
                Ok(name)
            })
            .collect::<Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }
    pub(super) fn key(&self) -> Result<DirKey> {
        Ok(DirKey {
            format: Format::RetainedIdentityV1,
            identity: io(self.directory.identity())?,
        })
    }
    pub(super) fn sync(&self) -> Result<()> {
        io(self.directory.sync())
    }
    pub(super) fn bound_file(&self) -> Result<File> {
        io(self.directory.try_clone_file())
    }
    fn local(&self) -> Result<LocalDir> {
        Ok(LocalDir(self.bound_file()?))
    }
}
pub(super) fn lock(storage: &Dir) -> Result<Lock> {
    io(storage.directory.lock(&leaf(".lock")?))
}
fn hash_file(file: &mut File, max: u64, cancel: Option<&AtomicBool>) -> Result<(Metadata, String)> {
    let before = io(retained_fs::metadata(file))?;
    if before.kind != Kind::Regular || before.reparse_tag.is_some() || before.len > max {
        return Err("核心文件类型无效或大小超过安全上限".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(err)?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        if let Some(cancel) = cancel {
            check(cancel)?;
        }
        let n = file.read(&mut buf).map_err(err)?;
        if n == 0 {
            break;
        }
        total = total.checked_add(n as u64).ok_or("核心文件过大")?;
        if total > max {
            return Err("核心文件大小超过安全上限".into());
        }
        digest.update(&buf[..n]);
    }
    if total != before.len || io(retained_fs::metadata(file))? != before {
        return Err("读取期间文件发生变化，请刷新后重试".into());
    }
    Ok((before, format!("{:x}", digest.finalize())))
}
pub(super) fn snapshot(mut file: File, max: u64, cancel: Option<&AtomicBool>) -> Result<Snapshot> {
    let (metadata, hash) = hash_file(&mut file, max, cancel)?;
    if metadata.links != 1 {
        return Err("核心文件必须是独立普通文件".into());
    }
    Ok(Snapshot {
        size: metadata.len,
        hash,
        metadata,
    })
}
pub(super) fn journal_bytes(operation: &Dir) -> Result<Option<Vec<u8>>> {
    let local = operation.local()?;
    let document = Document::new(&local, "journal.json", MAX_JOURNAL)?;
    #[cfg(windows)]
    {
        let mut state = document.state()?;
        if state.presence() == DocumentPresence::Tombstone {
            return Err("实例重置记录被逻辑删除，已保留全部证据".into());
        }
        document.read_state(&mut state)
    }
    #[cfg(unix)]
    {
        let Some(mut current) = document.snapshot()? else {
            return Ok(None);
        };
        document.bytes(&mut current).map(Some)
    }
}
// A fresh opaque document CAS only proves that bytes have not changed since
// this read. It does not authorize replacing an unknown business record.
fn validate_previous_journal(bytes: &[u8], next: &Journal) -> Result<()> {
    let previous: Journal =
        serde_json::from_slice(bytes).map_err(|_| "原有实例重置日志损坏，未知原文已保留")?;
    super::validate_journal(&previous, &next.operation_id)?;
    if previous.root != next.root
        || previous.root_key != next.root_key
        || previous.versions_key != next.versions_key
        || previous.instance_key != next.instance_key
        || previous.operation_key != next.operation_key
        || previous.instance_id != next.instance_id
        || previous.files.len() != next.files.len()
        || previous
            .files
            .iter()
            .zip(&next.files)
            .any(|(old, new)| old.name != new.name || old.original != new.original)
    {
        return Err("原有实例重置日志授权绑定不同，未知原文已保留".into());
    }
    // State, replacement snapshots and stage/incoming/backup registrations are
    // legitimate phase progress. Their absence/presence is validated per record,
    // rather than frozen across every recovery write.
    Ok(())
}
pub(super) fn publish_journal(operation: &Dir, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err("实例重置日志超过安全上限".into());
    }
    let next: Journal = serde_json::from_slice(bytes).map_err(|_| "新实例重置日志格式无效")?;
    super::validate_journal(&next, &next.operation_id)?;
    if operation.key()? != next.operation_key {
        return Err("新实例重置日志与操作目录绑定不同".into());
    }
    let local = operation.local()?;
    let document = Document::new(&local, "journal.json", MAX_JOURNAL)?;
    #[cfg(windows)]
    {
        let mut state = document.state()?;
        if state.presence() == DocumentPresence::Tombstone {
            return Err("实例重置记录被逻辑删除，已保留全部证据".into());
        }
        if let Some(previous) = document.read_state(&mut state)? {
            validate_previous_journal(&previous, &next)?;
        }
        document.compare_write(&state, Some(bytes))
    }
    #[cfg(unix)]
    {
        let mut current = document.snapshot()?;
        if let Some(previous) = current.as_mut() {
            validate_previous_journal(&document.bytes(previous)?, &next)?;
        }
        document.replace(current.as_ref(), bytes)
    }
}
pub(super) fn operation_names(operation: &Dir) -> Result<Vec<String>> {
    let allowed = vec![
        "journal.json".to_string(),
        "stage".into(),
        "incoming".into(),
        "backup".into(),
    ];
    #[cfg(windows)]
    let allowed = {
        let mut allowed = allowed;
        allowed.extend(
            Document::new(&operation.local()?, "journal.json", MAX_JOURNAL)?.auxiliary_names()?,
        );
        allowed
    };
    let names = operation.names()?;
    if names.iter().any(|name| !allowed.contains(name)) {
        return Err("重置记录目录含未知条目，已保留全部证据".into());
    }
    Ok(names)
}
pub(super) fn failed_initial_journal(
    _store: &Dir,
    _operation: &Dir,
    _j: &Journal,
    error: String,
) -> String {
    // A failed first publication can already have created the permanent base
    // or immutable log. Never recursively remove that evidence to report failure.
    format!("{error}；首次重置记录写入未确认，操作目录已保留，请重新读取恢复记录")
}
pub(super) fn rename_new(source: &Dir, old: &str, target: &Dir, new: &str) -> Result<()> {
    io(pcl_core::file_ops::rename_noreplace_at(
        source.directory.file(),
        leaf(old)?.as_os_str(),
        target.directory.file(),
        leaf(new)?.as_os_str(),
    ))?;
    source.sync()?;
    target.sync()
}
fn removal(value: std::io::Result<WorkspaceRemoval>) -> Result<()> {
    match io(value)? {
        WorkspaceRemoval::Removed => Ok(()),
        WorkspaceRemoval::MayHaveRemoved { error } => Err(format!("MayHaveRemoved：{error}")),
    }
}
pub(super) fn remove_registered(
    dir: &Dir,
    name: &str,
    expected: &Snapshot,
    max: u64,
) -> Result<()> {
    io(io(dir.directory.metadata())?.require_private())?;
    let file = dir.regular(name)?;
    let evidence = io(retained_fs::metadata(&file))?;
    let actual = snapshot(io(file.try_clone())?, max, None)?;
    if !expected.stable_matches(&actual) || io(retained_fs::metadata(&file))? != evidence {
        return Err("重置替换来源已变化，已保留文件".into());
    }
    removal(dir.directory.remove_generated_link(&leaf(name)?, &evidence))
}
pub(super) fn remove_directory(parent: &Dir, name: &str, expected: &Dir) -> Result<()> {
    let metadata = io(expected.directory.metadata())?;
    io(metadata.require_private())?;
    if !expected.names()?.is_empty() {
        return Err("重置暂存目录非空，已保留内容".into());
    }
    removal(
        parent
            .directory
            .remove_workspace_entry(&leaf(name)?, &metadata),
    )
}
// Only the journal's newly created installer stage enters this recursive path.
// Preflight the complete bounded tree before deleting its first generated node.
pub(super) fn remove_owned_tree(
    parent: &Dir,
    name: &str,
    key: &DirKey,
    count: &mut usize,
) -> Result<()> {
    struct Entry {
        parent: Directory,
        name: Leaf,
        metadata: Metadata,
    }
    fn visit(
        parent: &Directory,
        name: Leaf,
        key: &Identity,
        count: &mut usize,
        depth: usize,
        out: &mut Vec<Entry>,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err("重置暂存目录深度超过安全上限".into());
        }
        let directory = io(parent.child(&name, Traversal::SameFilesystem))?;
        let before = io(directory.metadata())?;
        if before.identity != *key {
            return Err("重置暂存目录已被替换".into());
        }
        io(before.require_owned())?;
        for child in io(directory.names(MAX_FILES))? {
            *count += 1;
            if *count > MAX_FILES {
                return Err("暂存清理数量超过安全上限".into());
            }
            let leaf = io(Leaf::new(child))?;
            let visible = io(directory.entry(&leaf))?.ok_or("暂存条目消失")?;
            if visible.reparse_tag.is_some() {
                return Err("重置暂存目录含重解析点，已保留全部内容".into());
            }
            match visible.kind {
                Kind::Directory => {
                    visit(&directory, leaf, &visible.identity, count, depth + 1, out)?
                }
                Kind::Regular => {
                    let file = io(directory.open_regular(&leaf, Links::Single))?;
                    let metadata = io(retained_fs::metadata(&file))?;
                    io(metadata.require_owned())?;
                    if !metadata.same_object(&visible) {
                        return Err("暂存条目已变化".into());
                    }
                    io(directory.recheck(&leaf, &metadata.identity))?;
                    out.push(Entry {
                        parent: directory.clone(),
                        name: leaf,
                        metadata,
                    });
                }
                _ => return Err("重置暂存目录含符号链接或特殊文件，已保留全部内容".into()),
            }
        }
        if io(directory.metadata())? != before {
            return Err("暂存清理预检期间目录变化".into());
        }
        out.push(Entry {
            parent: parent.clone(),
            name,
            metadata: before,
        });
        Ok(())
    }
    let root = parent.child(name)?;
    if root.key()? != *key {
        return Err("重置暂存目录已被替换".into());
    }
    io(io(root.directory.metadata())?.require_private())?;
    let mut entries = Vec::new();
    visit(
        &parent.directory,
        leaf(name)?,
        &key.identity,
        count,
        0,
        &mut entries,
    )?;
    // Release descendant read capabilities before a Windows directory removal.
    drop(root);
    for entry in entries {
        if entry.metadata.kind == Kind::Regular {
            removal(
                entry
                    .parent
                    .remove_generated_link(&entry.name, &entry.metadata),
            )?;
        } else {
            removal(
                entry
                    .parent
                    .remove_workspace_entry(&entry.name, &entry.metadata),
            )?;
        }
    }
    Ok(())
}
fn area(operation: &Dir, journal: &Journal) -> Result<Area> {
    if operation.key()? != journal.operation_key {
        return Err("重置记录目录已变化".into());
    }
    let root = Dir::open(&journal.root)?;
    if root.key()? != journal.root_key {
        return Err("重置游戏目录已变化".into());
    }
    let private = root.ensure(".pcl-linux")?;
    let mut limits = Limits::default();
    limits.area_bytes = MAX_JAR + MAX_JSON + limits.log_bytes;
    let area = io(Area::open_at_private(
        &private.directory,
        &leaf("instance-reset-sources")?,
        true,
        limits,
    ))?;
    io(area.reconcile_discarded())?;
    Ok(area)
}
fn discard(ticket: Ticket) -> Result<()> {
    match io(ticket.discard())? {
        DiscardOutcome::Discarded { .. } => Ok(()),
        DiscardOutcome::MayHaveDiscarded { error, .. } => Err(format!("MayHaveDiscarded：{error}")),
    }
}
fn publish(ticket: Ticket, target: &Dir, name: &str) -> Result<()> {
    match io(ticket.publish(&target.directory, &leaf(name)?))? {
        Outcome::Published { .. } => Ok(()),
        Outcome::Exists { ticket } => match io(ticket.discard())? {
            DiscardOutcome::Discarded { .. } => Err("目标已有同名文件，原文件已保留".into()),
            DiscardOutcome::MayHaveDiscarded { error, .. } => {
                Err(format!("目标冲突；MayHaveDiscarded：{error}"))
            }
        },
        Outcome::MayHavePublished { error, .. } => Err(format!(
            "MayHavePublished：{error}；来源登记及现存文件已保留"
        )),
    }
}
fn copy_source(
    source: &mut File,
    ticket: &mut Ticket,
    max: u64,
    cancel: &AtomicBool,
) -> Result<String> {
    let before = io(retained_fs::metadata(source))?;
    if before.kind != Kind::Regular || before.reparse_tag.is_some() || before.len > max {
        return Err("安装生成来源不是有界普通文件".into());
    }
    source.seek(SeekFrom::Start(0)).map_err(err)?;
    let mut total = 0u64;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        check(cancel)?;
        let n = source.read(&mut bytes).map_err(err)?;
        if n == 0 {
            break;
        }
        total = total.checked_add(n as u64).ok_or("重置生成来源过大")?;
        if total > before.len {
            return Err("安装生成来源大小已变化".into());
        }
        ticket.file_mut().write_all(&bytes[..n]).map_err(err)?;
        hash.update(&bytes[..n]);
    }
    if total != before.len || io(retained_fs::metadata(source))? != before {
        return Err("安装生成来源复制期间已变化".into());
    }
    ticket.file_mut().sync_all().map_err(err)?;
    Ok(format!("{:x}", hash.finalize()))
}
pub(super) fn copy_core(
    mut source: File,
    incoming: &Dir,
    operation: &Dir,
    journal: &mut Journal,
    index: usize,
    cancel: &AtomicBool,
) -> Result<()> {
    let area = area(operation, journal)?;
    let name = journal.files[index].name.clone();
    let mut ticket = Some(io(area.allocate(
        &incoming.directory,
        &leaf(&name)?,
        limit(index),
    ))?);
    let result = (|| {
        let target = ticket.as_mut().ok_or("核心替换来源已退役")?;
        let copied = copy_source(&mut source, target, limit(index), cancel)?;
        let expected = snapshot(io(target.file().try_clone())?, limit(index), Some(cancel))?;
        if expected.hash != copied {
            return Err("核心替换来源封存前哈希不同".into());
        }
        journal.files[index].replacement = Some(expected);
        // The registry already owns incomplete bytes. Complete ID/hash is
        // durable in the reset journal before giving this source its core name.
        write_journal(operation, journal)?;
        check(cancel)?;
        publish(ticket.take().ok_or("核心替换来源已退役")?, incoming, &name)?;
        let selected = snapshot(incoming.regular(&name)?, limit(index), Some(cancel))?;
        if !journal.files[index]
            .replacement
            .as_ref()
            .unwrap()
            .stable_matches(&selected)
        {
            return Err(may("核心替换发布结果变化"));
        }
        incoming.sync()
    })();
    match (result, ticket) {
        (Ok(()), _) => Ok(()),
        (Err(error), Some(ticket)) => match discard(ticket) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}；来源退役未确认：{cleanup}")),
        },
        (Err(error), None) => Err(error),
    }
}
pub(super) fn merge_cache_bound(
    source: &Dir,
    target: &Dir,
    root: &Dir,
    cancel: &AtomicBool,
    count: &mut usize,
) -> Result<()> {
    // Cache inputs are newly generated inside the recorded installer stage.
    // Keep a separate scope with the same existing durable registry protocol.
    let mut limits = Limits::default();
    limits.area_bytes = MAX_JAR + limits.log_bytes;
    let private = root.ensure(".pcl-linux")?;
    let area = io(Area::open_at_private(
        &private.directory,
        &leaf("instance-reset-cache-sources")?,
        true,
        limits,
    ))?;
    io(area.reconcile_discarded())?;
    merge_cache_at(source, target, &area, cancel, count, 0)
}
fn merge_cache_at(
    source: &Dir,
    target: &Dir,
    area: &Area,
    cancel: &AtomicBool,
    count: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("支持库目录深度超过安全上限".into());
    }
    for name in source.names()? {
        check(cancel)?;
        *count += 1;
        if *count > MAX_FILES {
            return Err("安装支持文件超过重置安全上限".into());
        }
        let visible = source.stat(&name)?.ok_or("安装支持文件消失")?;
        if visible.reparse_tag.is_some() {
            return Err("生成支持文件含重解析点".into());
        }
        match visible.kind {
            Kind::Directory => merge_cache_at(
                &source.child(&name)?,
                &target.ensure(&name)?,
                area,
                cancel,
                count,
                depth + 1,
            )?,
            Kind::Regular => {
                let mut file = source.cache_file(&name)?;
                let (metadata, hash) = hash_file(&mut file, MAX_JAR, Some(cancel))?;
                io(source.directory.recheck(&leaf(&name)?, &metadata.identity))?;
                if target.stat(&name)?.is_some() {
                    let (existing_metadata, existing) =
                        hash_file(&mut target.cache_file(&name)?, MAX_JAR, Some(cancel))?;
                    io(target
                        .directory
                        .recheck(&leaf(&name)?, &existing_metadata.identity))?;
                    if existing != hash || existing_metadata.len != metadata.len {
                        return Err(format!(
                            "已有支持文件与新组件不一致，已保留现有文件：{name}"
                        ));
                    }
                    continue;
                }
                let mut ticket = Some(io(area.allocate(
                    &target.directory,
                    &leaf(&name)?,
                    metadata.len,
                ))?);
                let published_identity = ticket
                    .as_ref()
                    .unwrap()
                    .allocation()
                    .source_identity
                    .clone();
                let result = (|| {
                    let copied = copy_source(&mut file, ticket.as_mut().unwrap(), MAX_JAR, cancel)?;
                    if copied != hash || io(retained_fs::metadata(&file))? != metadata {
                        return Err("支持文件在发布前已变化".into());
                    }
                    io(source.directory.recheck(&leaf(&name)?, &metadata.identity))?;
                    check(cancel)?;
                    publish(ticket.take().unwrap(), target, &name)?;
                    let (actual_metadata, actual) =
                        hash_file(&mut target.cache_file(&name)?, MAX_JAR, Some(cancel))?;
                    if actual != hash
                        || actual_metadata.len != metadata.len
                        || actual_metadata.identity != published_identity
                    {
                        return Err(may("新支持文件发布后身份或哈希变化"));
                    }
                    io(target
                        .directory
                        .recheck(&leaf(&name)?, &actual_metadata.identity))?;
                    Ok(())
                })();
                if let Err(error) = result {
                    return Err(match ticket {
                        Some(ticket) => match discard(ticket) {
                            Ok(()) => error,
                            Err(cleanup) => format!("{error}；来源退役未确认：{cleanup}"),
                        },
                        None => error,
                    });
                }
            }
            _ => return Err("生成的支持文件含符号链接或特殊节点".into()),
        }
    }
    Ok(())
}
