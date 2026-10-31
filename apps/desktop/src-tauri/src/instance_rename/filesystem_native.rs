//! Native retained filesystem boundary for the physical rename state machine.
//! Its schema records real IDs and complete held-file security metadata. It
//! never turns an arbitrary named file into generated staging authority.
use super::{
    bound_operation, check, err, name_ok, owned, slot, write_journal, Journal, Result, MAX_DEPTH,
    MAX_FILES, MAX_JOURNAL, MAX_JSON, MAX_TOTAL_JSON,
};
use crate::launcher_local::filesystem::{documents::Document, Dir as LocalDir};
use pcl_core::retained_fs::{
    self, Directory, Identity, Kind, Leaf, Links, Lock, Metadata, Traversal, WorkspaceRemoval,
};
use pcl_install::staging::{Area, DiscardOutcome, Limits, Outcome};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
pub(super) const SCHEMA: u32 = 2;
pub(super) const REVISION_PREFIX: &str = "rename-native-v2-";
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
        // Moves and the last native writer close can change timestamps. Full
        // bytes, ID, link count, type and security remain required evidence.
        self.size == other.size
            && self.hash == other.hash
            && self.metadata.same_object(&other.metadata)
            && self.metadata.len == other.metadata.len
            && self.metadata.links == other.metadata.links
            && self.metadata.security == other.metadata.security
            && self.metadata.reparse_tag == other.metadata.reparse_tag
    }
}
pub(super) fn valid_snapshot(snapshot: &Snapshot, _limit: u64) -> Result<()> {
    if snapshot.size != snapshot.metadata.len
        || snapshot.metadata.kind != Kind::Regular
        || snapshot.metadata.links != 1
        || snapshot.metadata.reparse_tag.is_some()
        || snapshot.metadata.modified.nanos >= 1_000_000_000
        || snapshot.metadata.changed.nanos >= 1_000_000_000
        || !platform_identity(&snapshot.metadata.identity)
    {
        return Err("重命名原生文件快照无效".into());
    }
    Ok(())
}
fn platform_identity(identity: &Identity) -> bool {
    #[cfg(windows)]
    {
        matches!(identity, Identity::Windows { .. })
    }
    #[cfg(unix)]
    {
        matches!(identity, Identity::Unix { .. })
    }
}
fn leaf(name: &str) -> Result<Leaf> {
    name_ok(name)?;
    Leaf::new(name).map_err(err)
}
pub(super) struct Dir {
    directory: Directory,
}
impl Dir {
    pub(super) fn open(path: &Path) -> Result<Self> {
        Directory::open_absolute(path)
            .map(|directory| Self { directory })
            .map_err(err)
    }
    pub(super) fn child(&self, name: &str) -> Result<Self> {
        self.directory
            .child(&leaf(name)?, Traversal::SameFilesystem)
            .map(|directory| Self { directory })
            .map_err(err)
    }
    pub(super) fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(super) fn create_dir(&self, name: &str) -> Result<Self> {
        let directory = self
            .directory
            .create_private(&leaf(name)?, Traversal::SameFilesystem)
            .map_err(err)?;
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
        self.directory.entry(&leaf(name)?).map_err(err)
    }
    pub(super) fn regular(&self, name: &str) -> Result<File> {
        self.directory
            .open_regular(&leaf(name)?, Links::Single)
            .map_err(err)
    }
    #[cfg(test)]
    pub(super) fn create_file(&self, name: &str) -> Result<File> {
        self.directory.create_file(&leaf(name)?).map_err(err)
    }
    pub(super) fn names(&self) -> Result<Vec<String>> {
        let mut names = self
            .directory
            .names(MAX_FILES)
            .map_err(err)?
            .into_iter()
            .map(|name| {
                let name = name.into_string().map_err(|_| "目录包含无效文件名")?;
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
            identity: self.directory.identity().map_err(err)?,
        })
    }
    pub(super) fn sync(&self) -> Result<()> {
        self.directory.sync().map_err(err)
    }
    fn local(&self) -> Result<LocalDir> {
        Ok(LocalDir(self.directory.try_clone_file().map_err(err)?))
    }
}
pub(super) fn stat_is_key(metadata: &Metadata, key: &DirKey) -> bool {
    metadata.kind == Kind::Directory
        && metadata.reparse_tag.is_none()
        && metadata.identity == key.identity
}
pub(super) fn snapshot(
    mut file: File,
    limit: u64,
    cancel: Option<&AtomicBool>,
) -> Result<Snapshot> {
    let metadata = retained_fs::metadata(&file).map_err(err)?;
    if metadata.kind != Kind::Regular
        || metadata.reparse_tag.is_some()
        || metadata.links != 1
        || metadata.len > limit
    {
        return Err("核心文件必须是大小有限的独立普通文件".into());
    }
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        if let Some(cancel) = cancel {
            check(cancel)?;
        }
        let count = file.read(&mut buffer).map_err(err)?;
        if count == 0 {
            break;
        }
        size = size.checked_add(count as u64).ok_or("核心文件过大")?;
        if size > limit {
            return Err("核心文件大小超过安全上限".into());
        }
        digest.update(&buffer[..count]);
    }
    if size != metadata.len || retained_fs::metadata(&file).map_err(err)? != metadata {
        return Err("读取期间文件发生变化，请刷新后重试".into());
    }
    Ok(Snapshot {
        size,
        hash: format!("{:x}", digest.finalize()),
        metadata,
    })
}
pub(super) fn content_revision(dir: &Dir, old: &str, new: &str) -> Result<String> {
    let mut digest = Sha256::new();
    let mut count = 0;
    inspect_contents(dir, "", old, new, &mut digest, &mut count, 0)?;
    Ok(format!("{:x}", digest.finalize()))
}
fn inspect_contents(
    dir: &Dir,
    relative: &str,
    old: &str,
    new: &str,
    digest: &mut Sha256,
    count: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("实例内容目录深度超过重命名安全上限".into());
    }
    let before = dir.directory.metadata().map_err(err)?;
    digest.update(serde_json::to_vec(&(relative, dir.key()?, &before.security)).map_err(err)?);
    for name in dir.names()? {
        if relative.is_empty()
            && [
                format!("{old}.json"),
                format!("{old}.jar"),
                format!("{new}.json"),
                format!("{new}.jar"),
            ]
            .contains(&name)
        {
            continue;
        }
        *count += 1;
        if *count > MAX_FILES {
            return Err("实例内容数量超过重命名安全上限".into());
        }
        let path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        let visible = dir.stat(&name)?.ok_or("检查期间实例内容已变化")?;
        if visible.reparse_tag.is_some() || visible.kind == Kind::Link {
            return Err(format!(
                "实例内容 {path} 含符号链接或重解析点，请先处理后再重命名"
            ));
        }
        match visible.kind {
            Kind::Directory => {
                let child = dir.child(&name)?;
                let key = child.key()?;
                inspect_contents(&child, &path, old, new, digest, count, depth + 1)?;
                if dir.child(&name)?.key()? != key {
                    return Err("检查期间实例内容目录已变化".into());
                }
            }
            Kind::Regular => {
                // Darwin entry ACL fields are placeholders. Obtain all security
                // evidence from the held literal file, then recheck visibility.
                let file = dir
                    .directory
                    .open_regular(&leaf(&name)?, Links::Any)
                    .map_err(err)?;
                let held = retained_fs::metadata(&file).map_err(err)?;
                if !held.same_object(&visible) {
                    return Err("检查期间实例内容文件已变化".into());
                }
                dir.directory
                    .recheck(&leaf(&name)?, &held.identity)
                    .map_err(err)?;
                digest.update(serde_json::to_vec(&(&path, &held)).map_err(err)?);
            }
            _ => return Err(format!("实例内容 {path} 含特殊文件，无法安全重命名")),
        }
    }
    if dir.directory.metadata().map_err(err)? != before {
        return Err("检查期间实例内容已变化，请重新读取重命名计划".into());
    }
    Ok(())
}
pub(super) fn lock(store: &Dir) -> Result<Lock> {
    store.directory.lock(&leaf(".lock")?).map_err(err)
}
pub(super) fn journal_bytes(operation: &Dir) -> Result<Option<Vec<u8>>> {
    let local = operation.local()?;
    let document = Document::new(&local, "journal.json", MAX_JOURNAL)?;
    #[cfg(windows)]
    {
        let mut state = document.state()?;
        document.read_state(&mut state)
    }
    #[cfg(unix)]
    {
        let Some(mut snapshot) = document.snapshot()? else {
            return Ok(None);
        };
        document.bytes(&mut snapshot).map(Some)
    }
}
pub(super) fn publish_journal(operation: &Dir, bytes: &[u8]) -> Result<()> {
    let local = operation.local()?;
    let document = Document::new(&local, "journal.json", MAX_JOURNAL)?;
    #[cfg(windows)]
    {
        let mut state = document.state()?;
        document.read_state(&mut state)?;
        document.compare_write(&state, Some(bytes))
    }
    #[cfg(unix)]
    {
        let snapshot = document.snapshot()?;
        document.replace(snapshot.as_ref(), bytes)
    }
}
pub(super) fn cleanup_journal_temps(operation: &Dir, _journal: &Journal) -> Result<()> {
    let allowed = vec![
        "journal.json".to_string(),
        "incoming".into(),
        "backup".into(),
    ];
    #[cfg(windows)]
    let allowed = {
        let local = operation.local()?;
        let document = Document::new(&local, "journal.json", MAX_JOURNAL)?;
        let mut allowed = allowed;
        allowed.extend(document.auxiliary_names()?);
        allowed
    };
    if operation
        .names()?
        .iter()
        .any(|name| !allowed.contains(name))
    {
        return Err("重命名记录目录含未知条目，已保留全部内容".into());
    }
    Ok(())
}
pub(super) fn rename_new(source: &Dir, old: &str, target: &Dir, new: &str) -> Result<()> {
    let old = leaf(old)?;
    let new = leaf(new)?;
    pcl_core::file_ops::rename_noreplace_at(
        source.directory.file(),
        old.as_os_str(),
        target.directory.file(),
        new.as_os_str(),
    )
    .map_err(|error| format!("无法无覆盖重命名实例文件或目录：{error}"))?;
    source.sync()?;
    target.sync()
}
fn removal(outcome: std::io::Result<WorkspaceRemoval>) -> Result<()> {
    match outcome.map_err(err)? {
        WorkspaceRemoval::Removed => Ok(()),
        WorkspaceRemoval::MayHaveRemoved { error } => Err(format!("MayHaveRemoved：{error}")),
    }
}
pub(super) fn remove_registered(
    dir: &Dir,
    name: &str,
    expected: &Snapshot,
    limit: u64,
) -> Result<()> {
    let file = dir.regular(name)?;
    let evidence = retained_fs::metadata(&file).map_err(err)?;
    if !expected.stable_matches(&snapshot(file.try_clone().map_err(err)?, limit, None)?)
        || retained_fs::metadata(&file).map_err(err)? != evidence
    {
        return Err("替换文件被外部修改，已保留内容".into());
    }
    dir.directory
        .metadata()
        .map_err(err)?
        .require_private()
        .map_err(err)?;
    removal(dir.directory.remove_generated_link(&leaf(name)?, &evidence))
}
pub(super) fn remove_directory(parent: &Dir, name: &str, expected: &Dir) -> Result<()> {
    let metadata = expected.directory.metadata().map_err(err)?;
    metadata.require_private().map_err(err)?;
    if !expected.names()?.is_empty() {
        return Err("重命名暂存目录非空，已保留内容".into());
    }
    removal(
        parent
            .directory
            .remove_workspace_entry(&leaf(name)?, &metadata),
    )
}
fn discard(ticket: pcl_install::staging::Ticket) -> Result<()> {
    match ticket.discard().map_err(err)? {
        DiscardOutcome::Discarded { .. } => Ok(()),
        DiscardOutcome::MayHaveDiscarded { error, .. } => Err(format!("MayHaveDiscarded：{error}")),
    }
}
pub(super) fn stage_file(
    operation: &Dir,
    j: &mut Journal,
    index: usize,
    bytes: &[u8],
    cancel: &AtomicBool,
) -> Result<()> {
    check(cancel)?;
    bound_operation(operation, j)?;
    let incoming = owned(operation, "incoming", &j.incoming_key)?.ok_or("替换目录缺失")?;
    let root = Dir::open(&j.root)?;
    let private = root.ensure(".pcl-linux")?;
    let mut limits = Limits::default();
    limits.area_bytes = MAX_TOTAL_JSON + limits.log_bytes;
    let area = Area::open_at_private(
        &private.directory,
        &leaf("instance-rename-sources")?,
        true,
        limits,
    )
    .map_err(err)?;
    area.reconcile_discarded().map_err(err)?;
    let name = leaf(&slot(index))?;
    let mut ticket = Some(
        area.allocate(&incoming.directory, &name, bytes.len() as u64)
            .map_err(err)?,
    );
    let result = (|| {
        let staged = ticket.as_mut().ok_or("替换来源已退役")?;
        for chunk in bytes.chunks(65536) {
            check(cancel)?;
            staged.file_mut().write_all(chunk).map_err(err)?;
        }
        staged.file_mut().sync_all().map_err(err)?;
        staged.file_mut().seek(SeekFrom::Start(0)).map_err(err)?;
        j.files[index].replacement = Some(snapshot(
            staged.file().try_clone().map_err(err)?,
            MAX_JSON,
            Some(cancel),
        )?);
        // The complete generated ID/hash is durable before its business name
        // becomes visible; the registry already owns incomplete sources.
        write_journal(operation, j)?;
        check(cancel)?;
        match ticket
            .take()
            .ok_or("替换来源已退役")?
            .publish(&incoming.directory, &name)
            .map_err(err)?
        {
            Outcome::Published { .. } => Ok(()),
            Outcome::Exists { ticket } => {
                let cleanup = ticket.discard().map_err(err)?;
                match cleanup {
                    DiscardOutcome::Discarded { .. } => {
                        Err("替换目录已含同名文件，已保留原文件".into())
                    }
                    DiscardOutcome::MayHaveDiscarded { error, .. } => {
                        Err(format!("替换目标冲突；MayHaveDiscarded：{error}"))
                    }
                }
            }
            Outcome::MayHavePublished { error, .. } => {
                Err(format!("MayHavePublished：{error}；已保留登记证据"))
            }
        }
    })();
    match (result, ticket) {
        (Ok(()), _) => incoming.sync(),
        (Err(error), Some(ticket)) => match discard(ticket) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}；来源退役未确认：{cleanup}")),
        },
        (Err(error), None) => Err(error),
    }
}
