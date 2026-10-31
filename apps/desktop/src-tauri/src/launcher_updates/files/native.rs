//! Native generated sources keep their registry Ticket through final publication.
use super::super::{binary, model::Architecture, provider::MAX_BINARY_BYTES};
use pcl_core::retained_fs::{
    self, Directory as Held, Identity as NativeIdentity, Kind, Leaf, Links, Metadata, Security,
    Time, Traversal,
};
use pcl_install::staging::{Area, DiscardOutcome, Limits, Outcome, Ticket};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex, OnceLock, Weak},
};
type Result<T> = std::result::Result<T, String>;
fn native<T>(r: std::io::Result<T>) -> Result<T> {
    r.map_err(|e| e.to_string())
}
fn leaf(name: &str) -> Result<Leaf> {
    native(Leaf::new(name))
}
pub(in crate::launcher_updates) fn c(name: &str) -> Result<CString> {
    CString::new(name).map_err(|_| "更新叶名含NUL".into())
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(in crate::launcher_updates) struct Identity(pub(in crate::launcher_updates) NativeIdentity);
pub(in crate::launcher_updates) struct Directory(pub(in crate::launcher_updates) File);
impl Directory {
    pub(in crate::launcher_updates) fn open(path: &Path) -> Result<Self> {
        native(retained_fs::validate_absolute_path(path))?;
        let d = native(Held::open_absolute(path))?;
        Ok(Self(native(d.try_clone_file())?))
    }
    pub(in crate::launcher_updates) fn held(&self) -> Result<Held> {
        native(Held::from_file(native(self.0.try_clone())?))
    }
    pub(in crate::launcher_updates) fn child(&self, name: &CString) -> Result<Self> {
        let d = native(self.held()?.child(
            &leaf(name.to_str().map_err(|_| "更新叶名无效")?)?,
            Traversal::SameFilesystem,
        ))?;
        Ok(Self(native(d.try_clone_file())?))
    }
    fn ensure(&self, name: &str) -> Result<Self> {
        let d = self.held()?;
        let name = leaf(name)?;
        let child = match d.child(&name, Traversal::SameFilesystem) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                native(d.create_private(&name, Traversal::SameFilesystem))?
            }
            Err(e) => return Err(e.to_string()),
        };
        native(d.sync())?;
        Ok(Self(native(child.try_clone_file())?))
    }
    pub(in crate::launcher_updates) fn identity(&self) -> Result<Identity> {
        native(self.held()?.identity()).map(Identity)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::launcher_updates) struct FileToken {
    pub(in crate::launcher_updates) identity: NativeIdentity,
    pub(in crate::launcher_updates) size: u64,
    modified: Time,
    security: Security,
    #[cfg(unix)]
    pub(in crate::launcher_updates) mode: u32,
    pub(in crate::launcher_updates) sha256: String,
}
impl FileToken {
    pub(in crate::launcher_updates) fn from_file(file: &File, sha256: String) -> Result<Self> {
        let m = native(retained_fs::metadata(file))?;
        Ok(Self {
            identity: m.identity,
            size: m.len,
            modified: m.modified,
            #[cfg(unix)]
            mode: match m.security {
                Security::Unix { mode, .. } => mode & 0o7777,
                _ => return Err("原生权限格式错误".into()),
            },
            security: m.security,
            sha256,
        })
    }
    pub(in crate::launcher_updates) fn valid(&self) -> bool {
        self.size >= 64
            && self.size <= MAX_BINARY_BYTES
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            && match self.security {
                Security::Unix { mode, .. } => mode & 0o7000 == 0,
                Security::Windows {
                    owner_is_current, ..
                } => owner_is_current,
            }
    }
    #[cfg(test)]
    pub(in crate::launcher_updates) fn identity(&self) -> pcl_core::retained_fs::Identity {
        self.identity.clone()
    }
    pub(in crate::launcher_updates) fn same_identity(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
    pub(in crate::launcher_updates) fn same_filesystem(&self, dir: &Directory) -> Result<bool> {
        Ok(match (&self.identity, &dir.identity()?.0) {
            (NativeIdentity::Unix { device: a, .. }, NativeIdentity::Unix { device: b, .. }) => {
                a == b
            }
            (
                NativeIdentity::Windows { volume: a, .. },
                NativeIdentity::Windows { volume: b, .. },
            ) => a == b,
            _ => false,
        })
    }
}
struct Sources {
    registry: Area,
    failures: Mutex<Vec<String>>,
}
impl Sources {
    fn fail(&self, error: String) {
        if let Ok(mut failures) = self.failures.lock() {
            if failures.len() < 64 {
                failures.push(error)
            }
        }
    }
    fn check(&self) -> Result<()> {
        let failures = self.failures.lock().map_err(|_| "更新来源清理状态不可用")?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "更新来源退役未确认，launcher-update-staging登记区和原件已保留：{}",
                failures.join("；")
            ))
        }
    }
    fn allocate(self: &Arc<Self>, target: &Directory, name: &str, max: u64) -> Result<Ticket> {
        self.check()?;
        self.registry
            .allocate(&target.held()?, &leaf(name)?, max)
            .map_err(|e| e.to_string())
    }
}
fn pool() -> &'static Mutex<BTreeMap<String, Weak<Sources>>> {
    static POOL: OnceLock<Mutex<BTreeMap<String, Weak<Sources>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(BTreeMap::new()))
}
#[cfg(test)]
pub(in crate::launcher_updates) fn pending(staging: &Path) -> Result<usize> {
    let directory = Directory::open(staging)?;
    let held = directory.held()?;
    if native(held.entry(&leaf("registered-sources")?))?.is_none() {
        return Ok(0);
    }
    let key = serde_json::to_string(&directory.identity()?).map_err(|e| e.to_string())?;
    if let Some(area) = pool()
        .lock()
        .map_err(|_| "更新来源池不可用")?
        .get(&key)
        .and_then(Weak::upgrade)
    {
        let report = area.registry.report().map_err(|e| e.to_string())?;
        if !report.writable() {
            return Err("更新来源登记报告有未知证据".into());
        }
        return Ok(report.pending_tickets);
    }
    let mut limits = Limits::default();
    limits.area_bytes = MAX_BINARY_BYTES * 2 + limits.log_bytes;
    let area = Area::open_at_private(&held, &leaf("registered-sources")?, false, limits)
        .map_err(|e| e.to_string())?;
    let report = area.report().map_err(|e| e.to_string())?;
    if !report.writable() {
        return Err("更新来源登记报告有未知证据".into());
    }
    Ok(report.pending_tickets)
}
fn sources(project: &Path) -> Result<Arc<Sources>> {
    let project = Directory::open(project)?;
    let storage = project.ensure(".pcl-rust")?;
    let staging = storage.ensure("launcher-update-staging")?;
    native(native(staging.held()?.metadata())?.require_private())?;
    let key = serde_json::to_string(&staging.identity()?).map_err(|e| e.to_string())?;
    let mut pool = pool().lock().map_err(|_| "更新来源登记池不可用")?;
    pool.retain(|_, weak| weak.strong_count() != 0);
    if let Some(area) = pool.get(&key).and_then(Weak::upgrade) {
        area.check()?;
        return Ok(area);
    }
    let mut limits = Limits::default();
    limits.area_bytes = MAX_BINARY_BYTES * 2 + limits.log_bytes;
    let registry =
        Area::open_at_private(&staging.held()?, &leaf("registered-sources")?, true, limits)
            .map_err(|e| e.to_string())?;
    registry.reconcile_discarded().map_err(|e| e.to_string())?;
    let report = registry.report().map_err(|e| e.to_string())?;
    if !report.writable() || report.pending_tickets != 0 {
        return Err(format!("launcher-update-staging/registered-sources保留{}个未退役ticket和{}项证据，未删除任何来源",report.pending_tickets,report.issue_count));
    }
    let area = Arc::new(Sources {
        registry,
        failures: Mutex::new(Vec::new()),
    });
    pool.insert(key, Arc::downgrade(&area));
    Ok(area)
}
#[derive(Clone)]
pub(in crate::launcher_updates) struct Cleanup(Arc<Sources>);
impl Cleanup {
    pub(in crate::launcher_updates) fn check(&self) -> Result<()> {
        self.0.check()
    }
}
pub(in crate::launcher_updates) struct Staging {
    project: PathBuf,
    project_dir: Directory,
    storage: Directory,
    directory: Directory,
    area: Arc<Sources>,
}
impl Staging {
    pub(in crate::launcher_updates) fn prepare(project: &Path) -> Result<Self> {
        let root = Directory::open(project)?;
        let storage = root.ensure(".pcl-rust")?;
        let directory = storage.ensure("launcher-update-staging")?;
        let s = Self {
            project: project.into(),
            project_dir: root,
            storage,
            directory,
            area: sources(project)?,
        };
        s.check_visible()?;
        Ok(s)
    }
    pub(in crate::launcher_updates) fn cleanup(&self) -> Cleanup {
        Cleanup(self.area.clone())
    }
    pub(in crate::launcher_updates) fn check_visible(&self) -> Result<()> {
        let root = Directory::open(&self.project)?;
        if root.identity()? != self.project_dir.identity()?
            || root.child(&c(".pcl-rust")?)?.identity()? != self.storage.identity()?
            || root
                .child(&c(".pcl-rust")?)?
                .child(&c("launcher-update-staging")?)?
                .identity()?
                != self.directory.identity()?
        {
            return Err("更新暂存目录已被替换".into());
        }
        self.area.check()
    }
    pub(in crate::launcher_updates) fn anonymous(&self) -> Result<Stage> {
        self.check_visible()?;
        let ticket = self
            .area
            .allocate(&self.directory, "verified-download", MAX_BINARY_BYTES)?;
        Ok(Stage {
            #[cfg(test)]
            retire_hook: None,
            ticket: Some(ticket),
            area: self.area.clone(),
            original: None,
            received: 0,
            digest: Sha256::new(),
        })
    }
    pub(in crate::launcher_updates) fn original_token(&self) -> Result<FileToken> {
        self.check_visible()?;
        let bin = self.storage.child(&c("bin")?)?;
        let token = read_named(&bin, "pcl-desktop", MAX_BINARY_BYTES, None)?
            .ok_or("原启动器不存在")?
            .0;
        if self.storage.child(&c("bin")?)?.identity()? != bin.identity()? {
            return Err("启动器安装目录被替换".into());
        }
        self.check_visible()?;
        Ok(token)
    }
}
pub(in crate::launcher_updates) struct Stage {
    #[cfg(test)]
    pub(in crate::launcher_updates) retire_hook: Option<Box<dyn FnOnce() + Send>>,

    ticket: Option<Ticket>,
    area: Arc<Sources>,
    pub(in crate::launcher_updates) original: Option<FileToken>,
    received: u64,
    digest: Sha256,
}
impl Stage {
    pub(in crate::launcher_updates) fn file(&self) -> &File {
        self.ticket.as_ref().expect("live updater source").file()
    }
    pub(in crate::launcher_updates) fn file_mut(&mut self) -> &mut File {
        self.ticket
            .as_mut()
            .expect("live updater source")
            .file_mut()
    }
    pub(in crate::launcher_updates) fn append(
        &mut self,
        bytes: &[u8],
        expected: u64,
    ) -> Result<u64> {
        let count = self
            .received
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= expected && *n <= MAX_BINARY_BYTES)
            .ok_or("更新资产超过清单大小")?;
        self.file_mut()
            .write_all(bytes)
            .map_err(|e| e.to_string())?;
        self.digest.update(bytes);
        self.received = count;
        Ok(count)
    }
    pub(in crate::launcher_updates) fn finish(
        &mut self,
        size: u64,
        sha256: &str,
        architecture: &Architecture,
    ) -> Result<()> {
        if self.received != size
            || format!("{:x}", self.digest.clone().finalize()) != sha256.to_ascii_lowercase()
        {
            return Err("更新资产大小或SHA256不匹配".into());
        }
        native(self.file().sync_all())?;
        let before = native(retained_fs::metadata(self.file()))?;
        native(before.require_private())?;
        if before.len != size
            || before.links != 1
            || before.kind != Kind::Regular
            || before.reparse_tag.is_some()
        {
            return Err("原生更新来源大小或类型改变".into());
        }
        let allocation = self.ticket.as_ref().unwrap().allocation();
        if allocation.source_identity != before.identity {
            return Err("更新来源身份改变".into());
        }
        if let Some(name) = &allocation.source_name {
            native(
                self.area
                    .registry
                    .directory()
                    .recheck(&native(Leaf::new(name))?, &before.identity),
            )?;
        }
        binary::verify(self.file_mut(), architecture)?;
        self.file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let mut digest = Sha256::new();
        let mut count = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = self
                .file_mut()
                .read(&mut buffer)
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            count = count
                .checked_add(n as u64)
                .filter(|n| *n <= size)
                .ok_or("更新来源读取期间变大")?;
            digest.update(&buffer[..n]);
        }
        if count != size
            || format!("{:x}", digest.finalize()) != sha256.to_ascii_lowercase()
            || native(retained_fs::metadata(self.file()))? != before
        {
            return Err("更新来源SHA256或元数据改变".into());
        }
        Ok(())
    }
    pub(in crate::launcher_updates) fn bind_target(
        &mut self,
        target: &Directory,
        name: &str,
    ) -> Result<()> {
        let mut next = self.area.allocate(target, name, self.received)?;
        self.file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let result = (|| {
            let mut digest = Sha256::new();
            let mut copied = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let n = self
                    .file_mut()
                    .read(&mut buffer)
                    .map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                copied = copied
                    .checked_add(n as u64)
                    .filter(|n| *n <= self.received)
                    .ok_or("更新来源复制期间变大")?;
                next.file_mut()
                    .write_all(&buffer[..n])
                    .map_err(|e| e.to_string())?;
                digest.update(&buffer[..n]);
            }
            if copied != self.received
                || digest.finalize()[..] != self.digest.clone().finalize()[..]
            {
                return Err("更新来源复制时改变".into());
            }
            native(next.file_mut().sync_all())
        })();
        if let Err(error) = result {
            discard(&self.area, next)?;
            return Err(error);
        }
        let old = self.ticket.replace(next).unwrap();
        discard(&self.area, old)
    }
    pub(in crate::launcher_updates) fn publish(
        &mut self,
        target: &Directory,
        name: &str,
    ) -> Result<()> {
        let allocation = self.ticket.as_ref().ok_or("更新来源已退役")?.allocation();
        if allocation.target_parent != target.identity()?.0
            || allocation.target_leaf.as_os_str() != leaf(name)?.as_os_str()
        {
            return Err("更新发布目标与持久登记不同".into());
        }
        publish(&self.area, self.ticket.take().unwrap(), target, name)
    }
    pub(in crate::launcher_updates) fn retire(mut self) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = self.retire_hook.take() {
            hook();
        }
        self.retire_inner()?;
        self.area.check()
    }
    fn retire_inner(&mut self) -> Result<()> {
        if let Some(ticket) = self.ticket.take() {
            discard(&self.area, ticket)
        } else {
            Ok(())
        }
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(hook) = self.retire_hook.take() {
            hook();
        }
        let _ = self.retire_inner();
    }
}
fn discard(area: &Sources, ticket: Ticket) -> Result<()> {
    let result = match ticket.discard() {
        Ok(DiscardOutcome::Discarded { .. }) => Ok(()),
        Ok(DiscardOutcome::MayHaveDiscarded { error, .. }) => {
            Err(format!("MayHaveDiscarded：{error}"))
        }
        Err(e) => Err(e.to_string()),
    };
    if let Err(error) = &result {
        area.fail(error.clone())
    }
    result
}
fn publish(area: &Sources, ticket: Ticket, target: &Directory, name: &str) -> Result<()> {
    let destination = match target
        .held()
        .and_then(|held| leaf(name).map(|name| (held, name)))
    {
        Ok(destination) => destination,
        Err(error) => {
            return match discard(area, ticket) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}；{cleanup}")),
            };
        }
    };
    let result = match ticket.publish(&destination.0, &destination.1) {
        Ok(Outcome::Published { .. }) => Ok(()),
        Ok(Outcome::Exists { ticket }) => {
            match ticket.discard() {
                Ok(DiscardOutcome::Discarded { .. }) => {}
                Ok(DiscardOutcome::MayHaveDiscarded { error, .. }) => {
                    area.fail(format!("MayHaveDiscarded：{error}"))
                }
                Err(e) => area.fail(e.to_string()),
            }
            Err("更新目标已存在，未覆盖原件".into())
        }
        Ok(Outcome::MayHavePublished { error, .. }) => Err(format!("MayHavePublished：{error}")),
        Err(e) => Err(e.to_string()),
    };
    if let Err(error) = &result {
        area.fail(error.clone())
    }
    result
}
pub(in crate::launcher_updates) fn publish_bytes(
    project: &Path,
    target: &Directory,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    let area = sources(project)?;
    let mut ticket = area.allocate(target, name, bytes.len() as u64)?;
    if let Err(error) = ticket.file_mut().write_all(bytes) {
        discard(&area, ticket)?;
        return Err(error.to_string());
    }
    publish(&area, ticket, target, name)
}
pub(in crate::launcher_updates) fn entry(dir: &Directory, name: &str) -> Result<Option<Metadata>> {
    native(dir.held()?.entry(&leaf(name)?))
}
pub(in crate::launcher_updates) fn read_named(
    dir: &Directory,
    name: &str,
    max: u64,
    cancel: Option<&AtomicBool>,
) -> Result<Option<(FileToken, Vec<u8>)>> {
    let held = dir.held()?;
    let Some(visible) = native(held.entry(&leaf(name)?))? else {
        return Ok(None);
    };
    if visible.kind != Kind::Regular || visible.reparse_tag.is_some() || visible.len > max {
        return Err("更新文件类型或大小不受支持；所有原件保留".into());
    }
    let mut file = native(held.open_regular(&leaf(name)?, Links::Single))?;
    let before = native(retained_fs::metadata(&file))?;
    native(before.require_owned())?;
    if matches!(
        before.security,
        Security::Unix {
            extended_acl: true,
            ..
        }
    ) {
        return Err("更新文件含无法由当前token完整记录的扩展ACL，原件已保留".into());
    }
    if before.identity != visible.identity || before.len > max {
        return Err("更新文件打开期间改变".into());
    }
    let mut bytes = Vec::new();
    let mut total = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
            return Err("更新读取已取消".into());
        }
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|n| *n <= max)
            .ok_or("更新文件读取期间变大")?;
        digest.update(&buffer[..count]);
        if max <= 8192 {
            bytes.extend_from_slice(&buffer[..count]);
        }
    }
    if total != before.len || native(retained_fs::metadata(&file))? != before {
        return Err("更新文件读取期间改变".into());
    }
    native(held.recheck(&leaf(name)?, &before.identity))?;
    Ok(Some((
        FileToken::from_file(&file, format!("{:x}", digest.finalize()))?,
        bytes,
    )))
}

impl Directory {
    pub(in crate::launcher_updates) fn sync(&self) -> Result<()> {
        native(self.held()?.sync())
    }
}
