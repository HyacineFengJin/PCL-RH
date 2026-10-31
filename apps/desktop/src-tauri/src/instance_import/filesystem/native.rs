//! Native retained source/target access. Journal mutation is admitted only once
//! its named staging ownership is registered; ordinary paths never substitute
//! for captured directories. No generated-workspace API authorizes user deletion.
use super::*;
use pcl_core::retained_fs::{self, Directory, Kind, Leaf, Links, Lock, Traversal};
use std::ops::DerefMut;
fn native<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(error)
}
fn leaf(name: &str) -> Result<Leaf> {
    component(name)?;
    native(Leaf::new(name))
}
struct Sources {
    area: pcl_install::staging::Area,
    cleanup: std::sync::Mutex<Vec<String>>,
}
impl Sources {
    fn failed(&self, error: String) {
        if let Ok(mut errors) = self.cleanup.lock() {
            if errors.len() < 128 {
                errors.push(error);
            }
        }
    }
    fn check(&self) -> Result<()> {
        let errors = self.cleanup.lock().map_err(|_| "导入来源清理状态不可用")?;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "登记来源清理失败，原件与登记证据已保留：{}",
                errors.join("；")
            ))
        }
    }
    #[cfg(test)]
    fn report(&self) -> std::io::Result<pcl_install::staging::AreaReport> {
        self.area.report()
    }
}
type JournalState = std::sync::Arc<
    std::sync::Mutex<Option<crate::launcher_local::filesystem::documents::DocumentSnapshot>>,
>;
pub(crate) struct Dir(
    pub(crate) File,
    Option<std::sync::Arc<Sources>>,
    Option<JournalState>,
);
impl Dir {
    fn native(&self) -> Result<Directory> {
        native(Directory::from_file(self.0.try_clone().map_err(error)?))
    }
    pub(crate) fn open(path: &Path) -> Result<Self> {
        native(retained_fs::validate_absolute_path(path))?;
        native(Directory::open_absolute(path))?
            .try_clone_file()
            .map(|file| Self(file, None, None))
            .map_err(error)
    }
    pub(crate) fn duplicate(&self) -> Result<Self> {
        self.0
            .try_clone()
            .map(|file| Self(file, self.1.clone(), self.2.clone()))
            .map_err(error)
    }
    pub(crate) fn key(&self) -> Result<Key> {
        Ok(file_stamp(&self.0)?.key)
    }
    pub(crate) fn sync(&self) -> Result<()> {
        self.native()?.sync().map_err(error)
    }
    pub(crate) fn with_journal(&self) -> Result<Self> {
        let local = crate::launcher_local::filesystem::Dir(self.0.try_clone().map_err(error)?);
        let document = crate::launcher_local::filesystem::documents::Document::new(
            &local,
            "journal.json",
            super::super::MAX_JOURNAL,
        )?;
        let current = document.snapshot()?;
        Ok(Self(
            self.0.try_clone().map_err(error)?,
            self.1.clone(),
            Some(std::sync::Arc::new(std::sync::Mutex::new(current))),
        ))
    }
    pub(crate) fn read_journal_bytes(&self, name: &str) -> Result<Vec<u8>> {
        if name != "journal.json" {
            return Err("原生导入目录出现未登记的备用日志，已保留".into());
        }
        let role = self.2.as_ref().ok_or("日志目录缺少显式所有权能力")?;
        let mut current = role.lock().map_err(|_| "导入日志状态不可用")?;
        let snapshot = current.as_mut().ok_or("导入日志缺失")?;
        let local = crate::launcher_local::filesystem::Dir(self.0.try_clone().map_err(error)?);
        let document = crate::launcher_local::filesystem::documents::Document::new(
            &local,
            "journal.json",
            super::super::MAX_JOURNAL,
        )?;
        document.bytes(snapshot)
    }
    pub(crate) fn write_journal_bytes(&self, bytes: &[u8]) -> Result<()> {
        let role = self.2.as_ref().ok_or("日志目录缺少显式所有权能力")?;
        let mut current = role.lock().map_err(|_| "导入日志状态不可用")?;
        let local = crate::launcher_local::filesystem::Dir(self.0.try_clone().map_err(error)?);
        let document = crate::launcher_local::filesystem::documents::Document::new(
            &local,
            "journal.json",
            super::super::MAX_JOURNAL,
        )?;
        if let Some(expected) = current.as_ref() {
            document.recheck(expected)?;
        }
        if let Err(error) = document.replace(current.as_ref(), bytes) {
            // Once the backend is entered, an error may follow the logical
            // commit or the actual Unix exchange. Reopen without authorizing
            // rollback of a possibly committed imported tree.
            if let Ok(Some(fresh)) = document.snapshot() {
                if fresh.digest() == format!("{:x}", Sha256::digest(bytes)) {
                    *current = Some(fresh);
                }
            }
            return Err(format!(
                "MayHaveCommitted：导入日志发布或持久性未确认，现存记录与实例已保留：{error}"
            ));
        }
        let fresh = document
            .snapshot()
            .map_err(|e| format!("MayHaveCommitted：导入日志可能已更新，复核失败：{e}"))?
            .ok_or("MayHaveCommitted：导入日志写入后消失，已保留现存文件")?;
        if fresh.digest() != format!("{:x}", Sha256::digest(bytes)) {
            return Err("MayHaveCommitted：导入日志可能已写入，但复核失败，现存记录已保留".into());
        }
        *current = Some(fresh);
        Ok(())
    }
    pub(crate) fn open_at(&self, name: &str, directory: bool) -> Result<File> {
        let dir = self.native()?;
        let name = leaf(name)?;
        if directory {
            native(dir.child(&name, Traversal::SameFilesystem))?
                .try_clone_file()
                .map_err(error)
        } else {
            native(dir.open_regular(&name, Links::Any))
        }
    }
    pub(crate) fn child(&self, name: &str) -> Result<Self> {
        self.open_at(name, true)
            .map(|file| Self(file, self.1.clone(), None))
    }
    pub(crate) fn regular(&self, name: &str) -> Result<File> {
        if name == "journal.json" {
            if let Some(role) = &self.2 {
                let current = role.lock().map_err(|_| "导入日志状态不可用")?;
                let snapshot = current.as_ref().ok_or("导入日志缺失")?;
                let local =
                    crate::launcher_local::filesystem::Dir(self.0.try_clone().map_err(error)?);
                crate::launcher_local::filesystem::documents::Document::new(
                    &local,
                    "journal.json",
                    super::super::MAX_JOURNAL,
                )?
                .recheck(snapshot)?;
                return snapshot.file().try_clone().map_err(error);
            }
        }
        self.open_at(name, false)
    }
    pub(crate) fn stat(&self, name: &str) -> Result<Option<Stamp>> {
        let dir = self.native()?;
        let name = leaf(name)?;
        let Some(visible) = native(dir.entry(&name))? else {
            return Ok(None);
        };
        // Darwin entry metadata is conservative about ACLs. Only a held literal
        // object supplies complete evidence; links remain non-traversable.
        let actual = match visible.kind {
            Kind::Directory if visible.reparse_tag.is_none() => {
                native(native(dir.child(&name, Traversal::SameFilesystem))?.metadata())?
            }
            Kind::Regular if visible.reparse_tag.is_none() => native(retained_fs::metadata(
                &native(dir.open_regular(&name, Links::Any))?,
            ))?,
            _ => return Ok(Some(Stamp::native(&visible)?)),
        };
        if !actual.same_object(&visible) {
            return Err(super::super::changed());
        }
        native(dir.recheck(&name, &actual.identity))?;
        Stamp::native(&actual).map(Some)
    }
    pub(crate) fn parent(&self, path: &str) -> Result<(Self, String)> {
        let parts = relative(path)?;
        let mut dir = self.duplicate()?;
        for part in &parts[..parts.len() - 1] {
            dir = dir.child(part)?;
        }
        Ok((dir, parts[parts.len() - 1].into()))
    }
    pub(crate) fn optional_parent(&self, path: &str) -> Result<Option<(Self, String)>> {
        let parts = relative(path)?;
        let mut dir = self.duplicate()?;
        for part in &parts[..parts.len() - 1] {
            if dir.stat(part)?.is_none() {
                return Ok(None);
            }
            dir = dir.child(part)?;
        }
        Ok(Some((dir, parts[parts.len() - 1].into())))
    }
    pub(crate) fn at(&self, path: &str) -> Result<Self> {
        let mut dir = self.duplicate()?;
        for part in relative(path)? {
            dir = dir.child(part)?;
        }
        Ok(dir)
    }
    pub(crate) fn file(&self, path: &str) -> Result<File> {
        let (dir, name) = self.parent(path)?;
        dir.regular(&name)
    }
    pub(crate) fn mkdir(&self, name: &str) -> Result<Self> {
        let dir = native(
            self.native()?
                .create_private(&leaf(name)?, Traversal::SameFilesystem),
        )?;
        self.sync()?;
        dir.try_clone_file()
            .map(|file| Self(file, self.1.clone(), None))
            .map_err(error)
    }
    pub(crate) fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_some() {
            self.child(name).map(Some)
        } else {
            Ok(None)
        }
    }
    pub(crate) fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(d) => Ok(d),
            None => self.mkdir(name),
        }
    }
    pub(crate) fn with_sources(&self) -> Result<Self> {
        let app = self.ensure(".pcl-linux")?;
        app.native()?
            .metadata()
            .and_then(|m| m.require_private())
            .map_err(error)?;
        self.attach_sources(&app, "instance-import-sources")
    }
    pub(crate) fn with_input_sources(&self) -> Result<Self> {
        self.attach_sources(self, "registered-sources")
    }
    fn attach_sources(&self, parent: &Dir, name: &str) -> Result<Self> {
        // Area owns one native cross-process lock. Concurrent confirmations in
        // this process share that same Area rather than lock it against each
        // other. Weak entries do not retain source registries after their last
        // stage/descriptor-owning input has retired.
        type Pool = std::sync::Mutex<std::collections::BTreeMap<String, std::sync::Weak<Sources>>>;
        static POOL: std::sync::OnceLock<Pool> = std::sync::OnceLock::new();
        let parent = parent.native()?;
        let key = serde_json::to_string(&(native(parent.identity())?, name)).map_err(error)?;
        let mut pool = POOL
            .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
            .lock()
            .map_err(|_| "导入来源登记池不可用")?;
        pool.retain(|_, weak| weak.strong_count() != 0);
        if let Some(sources) = pool.get(&key).and_then(std::sync::Weak::upgrade) {
            return Ok(Self(
                self.0.try_clone().map_err(error)?,
                Some(sources),
                self.2.clone(),
            ));
        }
        let area = pcl_install::staging::Area::open_at_private(
            &parent,
            &leaf(name)?,
            true,
            source_limits(),
        )
        .map_err(error)?;
        area.reconcile_discarded().map_err(error)?;
        let sources = std::sync::Arc::new(Sources {
            area,
            cleanup: std::sync::Mutex::new(Vec::new()),
        });
        pool.insert(key, std::sync::Arc::downgrade(&sources));
        Ok(Self(
            self.0.try_clone().map_err(error)?,
            Some(sources),
            self.2.clone(),
        ))
    }
    pub(crate) fn check_source_cleanup(&self) -> Result<()> {
        if let Some(sources) = &self.1 {
            sources.check()?;
        }
        Ok(())
    }
    pub(crate) fn reconcile_sources(&self) -> Result<()> {
        let Some(app) = self.optional(".pcl-linux")? else {
            return Ok(());
        };
        if app.stat("instance-import-sources")?.is_none() {
            return Ok(());
        }
        let area = pcl_install::staging::Area::open_at_private(
            &app.native()?,
            &leaf("instance-import-sources")?,
            false,
            source_limits(),
        )
        .map_err(error)?;
        area.reconcile_discarded().map_err(error)?;
        Ok(())
    }
    pub(crate) fn stage_for(&self, name: &str, max: u64) -> Result<StageFile> {
        let area = self
            .1
            .as_ref()
            .ok_or("导入暂存分配缺少已锁定的来源登记区")?;
        let ticket = area
            .area
            .allocate(&self.native()?, &leaf(name)?, max)
            .map_err(error)?;
        Ok(StageFile {
            ticket: Some(ticket),
            sources: area.clone(),
        })
    }
    pub(crate) fn anonymous(&self) -> Result<StageFile> {
        self.stage_for(
            &format!("input-{}", super::super::nonce()),
            super::super::MAX_FILE,
        )
    }
    pub(crate) fn create(&self, name: &str) -> Result<File> {
        native(self.native()?.create_file(&leaf(name)?))
    }
    pub(crate) fn unlink_snapshot(&self, name: &str, expected: &Snapshot) -> Result<()> {
        // Only durable import-owned rows or the registered private build may
        // call this. Existing shared resources and the selected ZIP never do.
        let file = self.regular(name)?;
        let metadata = native(retained_fs::metadata(&file))?;
        if !expected.owned_matches(&hash_file(
            file.try_clone().map_err(error)?,
            super::super::MAX_FILE,
            None,
        )?) || native(retained_fs::metadata(&file))? != metadata
        {
            return Err(super::super::changed());
        }
        match native(
            self.native()?
                .remove_generated_link(&leaf(name)?, &metadata),
        )? {
            retained_fs::WorkspaceRemoval::Removed => Ok(()),
            retained_fs::WorkspaceRemoval::MayHaveRemoved { error } => {
                Err(format!("导入所有权链接可能已清理，已保留其余内容：{error}"))
            }
        }
    }
    pub(crate) fn unlink_key(&self, name: &str, expected: &Key) -> Result<()> {
        let child = self.child(name)?;
        if child.key()? != *expected || !child.names()?.is_empty() {
            return Err("导入所有权目录已变化或非空，已保留".into());
        }
        let directory = child.native()?;
        let metadata = native(directory.metadata())?;
        match native(
            self.native()?
                .remove_workspace_entry(&leaf(name)?, &metadata),
        )? {
            retained_fs::WorkspaceRemoval::Removed => Ok(()),
            retained_fs::WorkspaceRemoval::MayHaveRemoved { error } => Err(format!(
                "导入所有权空目录可能已清理，已保留其余内容：{error}"
            )),
        }
    }
    pub(crate) fn names(&self) -> Result<Vec<String>> {
        let mut names = native(self.native()?.names(MAX_FILES * 4))?
            .into_iter()
            .map(|name| {
                let name = name
                    .into_string()
                    .map_err(|_| "目录包含无效 UTF-8 文件名")?;
                component(&name)?;
                Ok(name)
            })
            .collect::<Result<Vec<_>>>()?;
        #[cfg(windows)]
        if self.2.is_some() {
            let local = crate::launcher_local::filesystem::Dir(self.0.try_clone().map_err(error)?);
            let document = crate::launcher_local::filesystem::documents::Document::new(
                &local,
                "journal.json",
                super::super::MAX_JOURNAL,
            )?;
            let auxiliary = document.auxiliary_names()?;
            names.retain(|name| !auxiliary.contains(name));
        }
        names.sort();
        Ok(names)
    }
}
pub(crate) fn file_stamp(file: &File) -> Result<Stamp> {
    Stamp::native(&native(retained_fs::metadata(file))?)
}
pub(crate) fn is_anonymous(file: &StageFile) -> Result<bool> {
    let Some(ticket) = &file.ticket else {
        return Ok(false);
    };
    let metadata = native(retained_fs::metadata(ticket.file()))?;
    Ok(metadata.kind == Kind::Regular
        && metadata.links
            == if ticket.allocation().source_name.is_some() {
                1
            } else {
                0
            }
        && metadata.require_private().is_ok()
        && metadata.identity == ticket.allocation().source_identity)
}
pub(crate) fn owns_stage(stage: &StageFile, file: &File) -> Result<bool> {
    if !is_anonymous(stage)? {
        return Ok(false);
    }
    Ok(file_stamp(stage)? == file_stamp(file)?)
}
fn source_limits() -> pcl_install::staging::Limits {
    let mut limits = pcl_install::staging::Limits::default();
    limits.area_bytes = super::super::MAX_BYTES.saturating_add(limits.log_bytes);
    limits.pending_tickets = 100_000;
    limits.entries = 100_000;
    limits
}
pub(crate) struct StageFile {
    ticket: Option<pcl_install::staging::Ticket>,
    sources: std::sync::Arc<Sources>,
}
impl StageFile {
    fn discard(&mut self) -> Result<()> {
        let Some(ticket) = self.ticket.take() else {
            return Ok(());
        };
        match ticket.discard().map_err(error)? {
            pcl_install::staging::DiscardOutcome::Discarded { .. } => Ok(()),
            pcl_install::staging::DiscardOutcome::MayHaveDiscarded { error, .. } => Err(format!(
                "MayHaveDiscarded：导入来源可能已退役，登记证据已保留：{error}"
            )),
        }
    }
}
impl Drop for StageFile {
    fn drop(&mut self) {
        if let Err(error) = self.discard() {
            self.sources.failed(error);
        }
    }
}
impl std::ops::Deref for StageFile {
    type Target = File;
    fn deref(&self) -> &File {
        self.ticket
            .as_ref()
            .expect("live registered import stage")
            .file()
    }
}
impl std::ops::DerefMut for StageFile {
    fn deref_mut(&mut self) -> &mut File {
        self.ticket
            .as_mut()
            .expect("live registered import stage")
            .file_mut()
    }
}
impl std::io::Write for StageFile {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.deref_mut().write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.deref_mut().flush()
    }
}
impl std::io::Read for StageFile {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.deref_mut().read(b)
    }
}
impl std::io::Seek for StageFile {
    fn seek(&mut self, p: std::io::SeekFrom) -> std::io::Result<u64> {
        self.deref_mut().seek(p)
    }
}
pub(crate) fn require_mutation_support() -> Result<()> {
    Ok(())
}
pub(crate) struct ImportLock(#[allow(dead_code)] Lock);
pub(crate) fn lock(store: &Dir) -> Result<ImportLock> {
    native(store.native()?.lock(&leaf(".lock")?)).map(ImportLock)
}
pub(crate) fn link_anonymous(file: &mut StageFile, dir: &Dir, name: &str) -> Result<()> {
    let target = dir.native()?;
    let name = leaf(name)?;
    let ticket = file.ticket.as_ref().ok_or("导入暂存已经尝试发布")?;
    if ticket.allocation().target_parent != native(target.identity())?
        || ticket.allocation().target_leaf.as_os_str() != name.as_os_str()
    {
        return Err("导入目标与已登记的父目录或叶名不符，未发布文件".into());
    }
    let ticket = file.ticket.take().ok_or("导入暂存已经尝试发布")?;
    let outcome = match ticket.publish(&target, &name) {
        Ok(outcome) => outcome,
        Err(error) => {
            file.sources
                .failed(format!("登记来源在发布失败后保留，需复核：{error}"));
            return Err(error.to_string());
        }
    };
    match outcome {
        pcl_install::staging::Outcome::Published { .. } => Ok(()),
        pcl_install::staging::Outcome::Exists { ticket } => {
            let cleanup = match ticket.discard() {
                Ok(pcl_install::staging::DiscardOutcome::Discarded { .. }) => Ok(()),
                Ok(pcl_install::staging::DiscardOutcome::MayHaveDiscarded { error, .. }) => {
                    Err(format!("MayHaveDiscarded：{error}"))
                }
                Err(error) => Err(error.to_string()),
            };
            if let Err(error) = cleanup {
                file.sources.failed(error.clone());
                return Err(format!(
                    "导入目标已存在，登记来源清理未确认，原件与证据均已保留：{error}"
                ));
            }
            Err("导入目标已存在，原文件已保留".into())
        }
        pcl_install::staging::Outcome::MayHavePublished { error, .. } => {
            file.sources.failed(format!("MayHavePublished：{error}"));
            Err(format!(
                "MayHavePublished：导入文件可能已发布，登记与现存文件已保留：{error}"
            ))
        }
    }
}
pub(crate) fn link_file(from: &Dir, source: &str, to: &Dir, target: &str) -> Result<()> {
    pcl_core::file_ops::hard_link_noreplace_at(
        &from.0,
        leaf(source)?.as_os_str(),
        &to.0,
        leaf(target)?.as_os_str(),
    )
    .map_err(|e| format!("导入硬链接可能已发布，现存文件均已保留：{e}"))?;
    to.sync()
}
pub(crate) fn move_new(from: &Dir, source: &str, to: &Dir, target: &str) -> Result<()> {
    let expected = from.child(source)?.key()?;
    pcl_core::file_ops::rename_noreplace_at(
        &from.0,
        leaf(source)?.as_os_str(),
        &to.0,
        leaf(target)?.as_os_str(),
    )
    .map_err(|e| format!("导入目录可能已移动，现存目录已保留：{e}"))?;
    if to.child(target)?.key() != Ok(expected) {
        return Err("导入目录已移动但身份复核失败，已保留目录".into());
    }
    from.sync()?;
    to.sync()
}
#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
