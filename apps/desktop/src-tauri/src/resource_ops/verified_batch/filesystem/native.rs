//! Native resource transaction capability. Registered copy tickets never lose
//! their owner; logical document roles are granted explicitly to operation dirs.
use super::*;
use crate::launcher_local::filesystem::{documents::Document, Dir as LocalDir};
use pcl_core::retained_fs::{
    self, Directory, Kind, Leaf, Links, Metadata, Traversal, WorkspaceRemoval,
};
use std::sync::{Arc, Mutex};
#[cfg(windows)]
pub(in crate::resource_ops) type DocState =
    crate::launcher_local::filesystem::documents::DocumentState;
#[cfg(unix)]
pub(in crate::resource_ops) type DocState =
    Option<crate::launcher_local::filesystem::documents::DocumentSnapshot>;
struct Shared {
    root: File,
    area: Mutex<Option<Arc<crate::modrinth_install::source::Area>>>,
}
pub(in crate::resource_ops) struct Dir(File, Arc<Shared>, Option<u64>);
fn native<T>(r: std::io::Result<T>) -> Result<T> {
    r.map_err(error)
}
fn leaf(name: &str) -> Result<Leaf> {
    crate::resource_ops::safe_name(name)?;
    native(Leaf::new(name))
}
fn held(file: &File) -> Result<Directory> {
    native(Directory::from_file(file.try_clone().map_err(error)?))
}
fn document(name: &str) -> bool {
    matches!(name, "journal.json" | "status.json" | "owned-marker.json")
}
impl Dir {
    pub(in crate::resource_ops) fn open(path: &Path) -> Result<Self> {
        native(retained_fs::validate_absolute_path(path))?;
        let d = native(Directory::open_absolute(path))?;
        let file = native(d.try_clone_file())?;
        Ok(Self(
            file,
            Arc::new(Shared {
                root: native(d.try_clone_file())?,
                area: Mutex::new(None),
            }),
            None,
        ))
    }
    pub(in crate::resource_ops) fn key(&self) -> Result<Key> {
        key_file(&self.0)
    }
    fn duplicate(&self) -> Result<Self> {
        Ok(Self(
            self.0.try_clone().map_err(error)?,
            self.1.clone(),
            self.2,
        ))
    }
    pub(in crate::resource_ops) fn with_documents(&self) -> Result<Self> {
        let mut d = self.duplicate()?;
        d.2 = Some(MAX_JOURNAL_BYTES);
        Ok(d)
    }
    pub(in crate::resource_ops) fn with_documents_max(&self, max: u64) -> Result<Self> {
        let mut d = self.duplicate()?;
        d.2 = Some(max);
        Ok(d)
    }
    pub(in crate::resource_ops) fn sync(&self) -> Result<()> {
        native(held(&self.0)?.sync())
    }
    pub(in crate::resource_ops) fn child(&self, name: &str) -> Result<Self> {
        let d = native(held(&self.0)?.child(&leaf(name)?, Traversal::SameFilesystem))?;
        Ok(Self(native(d.try_clone_file())?, self.1.clone(), None))
    }
    #[cfg(windows)]
    fn document_max(&self, name: &str) -> u64 {
        if name == "journal.json" {
            self.2.unwrap_or(MAX_JOURNAL_BYTES)
        } else {
            16 * 1024
        }
    }
    pub(in crate::resource_ops) fn regular(&self, name: &str) -> Result<File> {
        #[cfg(windows)]
        if self.2.is_some() && document(name) {
            let dir = LocalDir(self.0.try_clone().map_err(error)?);
            let snapshot = Document::new(&dir, name, self.document_max(name))?
                .snapshot()?
                .ok_or("资源文档缺失")?;
            return snapshot.file().try_clone().map_err(error);
        }
        native(held(&self.0)?.open_regular(&leaf(name)?, Links::Any))
    }
    pub(in crate::resource_ops) fn stat(&self, name: &str) -> Result<Option<Metadata>> {
        #[cfg(windows)]
        if self.2.is_some() && document(name) {
            use crate::launcher_local::filesystem::documents::DocumentPresence;
            let dir = LocalDir(self.0.try_clone().map_err(error)?);
            let doc = Document::new(&dir, name, self.document_max(name))?;
            let s = doc.state()?;
            if s.presence() != DocumentPresence::Present {
                return Ok(None);
            }
            let snapshot = doc.snapshot()?.ok_or("资源文档缺失")?;
            return native(retained_fs::metadata(snapshot.file())).map(Some);
        }
        let directory = held(&self.0)?;
        let name = leaf(name)?;
        let Some(visible) = native(directory.entry(&name))? else {
            return Ok(None);
        };
        let actual = match visible.kind {
            Kind::Directory if visible.reparse_tag.is_none() => {
                native(native(directory.child(&name, Traversal::SameFilesystem))?.metadata())?
            }
            Kind::Regular if visible.reparse_tag.is_none() => native(retained_fs::metadata(
                &native(directory.open_regular(&name, Links::Any))?,
            ))?,
            _ => return Ok(Some(visible)),
        };
        if !actual.same_object(&visible) {
            return Err("资源目录条目已被替换".into());
        }
        native(directory.recheck(&name, &actual.identity))?;
        Ok(Some(actual))
    }
    pub(in crate::resource_ops) fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_some() {
            self.child(name).map(Some)
        } else {
            Ok(None)
        }
    }
    pub(in crate::resource_ops) fn optional_at(&self, path: &str) -> Result<Option<Self>> {
        let mut d = self.duplicate()?;
        for name in path.split('/') {
            let Some(child) = d.optional(name)? else {
                return Ok(None);
            };
            d = child;
        }
        Ok(Some(d))
    }
    pub(in crate::resource_ops) fn parent(&self, path: &str) -> Result<(Self, String)> {
        let parts: Vec<_> = path.split('/').collect();
        let name = parts.last().ok_or("资源路径无效")?;
        let mut d = self.duplicate()?;
        for name in &parts[..parts.len() - 1] {
            d = d.child(name)?;
        }
        leaf(name)?;
        Ok((d, (*name).into()))
    }
    pub(in crate::resource_ops) fn mkdir(&self, name: &str) -> Result<Self> {
        let d = native(held(&self.0)?.create_private(&leaf(name)?, Traversal::SameFilesystem))?;
        self.sync()?;
        Ok(Self(native(d.try_clone_file())?, self.1.clone(), None))
    }
    pub(in crate::resource_ops) fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(d) => Ok(d),
            None => self.mkdir(name),
        }
    }
    pub(in crate::resource_ops) fn absent(&self, name: &str) -> Result<()> {
        if self.stat(name)?.is_some() {
            Err(format!("已有同名或禁用状态的资源文件，拒绝覆盖：{name}"))
        } else {
            Ok(())
        }
    }
    fn area(&self) -> Result<Arc<crate::modrinth_install::source::Area>> {
        let mut area = self.1.area.lock().map_err(|_| "资源来源状态不可用")?;
        if let Some(a) = area.as_ref() {
            a.check_cleanup()?;
            return Ok(a.clone());
        }
        let root = held(&self.1.root)?;
        let app = match root.child(&leaf(".pcl-linux")?, Traversal::SameFilesystem) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                native(root.create_private(&leaf(".pcl-linux")?, Traversal::SameFilesystem))?
            }
            Err(e) => return Err(error(e)),
        };
        let a = crate::modrinth_install::source::open_area(
            app.file(),
            "resource-batch-sources",
            MAX_BATCH_BYTES * 2,
        )?;
        *area = Some(a.clone());
        Ok(a)
    }
    pub(in crate::resource_ops) fn stage_for(
        &self,
        name: &str,
        max: u64,
    ) -> Result<crate::modrinth_install::source::Input> {
        crate::modrinth_install::source::allocate(&self.area()?, &self.0, name, max)
    }
    pub(in crate::resource_ops) fn stage_batch_for(
        &self,
        requests: &[(String, u64)],
        cancel: &AtomicBool,
    ) -> Result<crate::modrinth_install::source::InputBatch> {
        crate::modrinth_install::source::allocate_batch(&self.area()?, &self.0, requests, cancel)
    }
    #[cfg(test)]
    pub(in crate::resource_ops) fn anonymous(
        &self,
    ) -> Result<crate::modrinth_install::source::Input> {
        self.stage_for("unpublished-fixture", MAX_FILE_BYTES)
    }
    pub(in crate::resource_ops) fn check_cleanup(&self) -> Result<()> {
        if let Some(a) = self
            .1
            .area
            .lock()
            .map_err(|_| "资源来源状态不可用")?
            .as_ref()
        {
            a.check_cleanup()?;
        }
        Ok(())
    }
    pub(in crate::resource_ops) fn link_anonymous(
        &self,
        file: &mut crate::modrinth_install::source::Input,
        name: &str,
    ) -> Result<()> {
        file.publish(&self.0, name)
    }
    pub(in crate::resource_ops) fn link(
        &self,
        name: &str,
        target: &Self,
        target_name: &str,
    ) -> Result<()> {
        pcl_core::file_ops::hard_link_noreplace_at(
            &self.0,
            leaf(name)?.as_os_str(),
            &target.0,
            leaf(target_name)?.as_os_str(),
        )
        .map_err(|e| format!("MayHavePublished：资源链接可能已发布，原件均已保留：{e}"))?;
        target.sync()
    }
    pub(in crate::resource_ops) fn move_directory(
        &self,
        name: &str,
        target: &Self,
        target_name: &str,
    ) -> Result<()> {
        let expected = self.stat(name)?.ok_or("资源移动来源缺失")?;
        if expected.reparse_tag.is_some()
            || !matches!(expected.kind, Kind::Directory | Kind::Regular)
        {
            return Err("资源移动来源不是普通文件或目录".into());
        }
        pcl_core::file_ops::rename_noreplace_at(
            &self.0,
            leaf(name)?.as_os_str(),
            &target.0,
            leaf(target_name)?.as_os_str(),
        )
        .map_err(|e| format!("MayHavePublished：资源可能已移动，原件均已保留：{e}"))?;
        let after = || -> Result<()> {
            if target
                .stat(target_name)?
                .is_none_or(|m| !m.same_object(&expected))
            {
                return Err("资源移动后的身份核验失败".into());
            }
            self.sync()?;
            target.sync()
        };
        after().map_err(|e| format!("MayHavePublished：资源已移动但持久性或复核未确认：{e}"))
    }
    pub(in crate::resource_ops) fn unlink(&self, name: &str, directory: bool) -> Result<()> {
        #[cfg(windows)]
        if self.2.is_some() && document(name) && !directory {
            let state = self.document_state(name, self.document_max(name))?;
            return self.compare_document(name, self.document_max(name), &state, None);
        }
        let d = held(&self.0)?;
        let name = leaf(name)?;
        let m = if directory {
            native(native(d.child(&name, Traversal::SameFilesystem))?.metadata())?
        } else {
            native(retained_fs::metadata(&native(
                d.open_regular(&name, Links::Any),
            )?))?
        };
        let result = if directory {
            d.remove_workspace_entry(&name, &m)
        } else {
            d.remove_generated_link(&name, &m)
        };
        match native(result)? {
            WorkspaceRemoval::Removed => Ok(()),
            WorkspaceRemoval::MayHaveRemoved { error: e } => {
                Err(format!("资源所有权条目清理未确认：{e}"))
            }
        }
    }
    pub(in crate::resource_ops) fn names(&self) -> Result<Vec<String>> {
        self.names_with_limit(MAX_FILES * 4 + 8)
    }
    pub(in crate::resource_ops) fn names_with_limit(&self, limit: usize) -> Result<Vec<String>> {
        let mut names = native(held(&self.0)?.names(limit))?
            .into_iter()
            .map(|name| {
                name.into_string()
                    .map_err(|_| "资源目录含无效UTF8名称".into())
            })
            .collect::<Result<Vec<_>>>()?;
        #[cfg(windows)]
        if self.2.is_some() {
            use crate::launcher_local::filesystem::documents::DocumentPresence;
            let local = LocalDir(self.0.try_clone().map_err(error)?);
            for name in ["journal.json", "status.json", "owned-marker.json"] {
                let doc = Document::new(&local, name, self.document_max(name))?;
                let state = doc.state()?;
                if state.presence() != DocumentPresence::Present {
                    names.retain(|n| n != name);
                }
                let aux = doc.auxiliary_names()?;
                names.retain(|n| !aux.contains(n));
            }
        }
        for name in &names {
            crate::resource_ops::safe_name(name)?;
        }
        names.sort();
        Ok(names)
    }
    pub(in crate::resource_ops) fn document_state(&self, name: &str, max: u64) -> Result<DocState> {
        if self.2.is_none() || !document(name) {
            return Err("资源目录没有此逻辑文档的显式所有权".into());
        }
        let local = LocalDir(self.0.try_clone().map_err(error)?);
        let doc = Document::new(&local, name, max)?;
        #[cfg(windows)]
        {
            doc.state()
        }
        #[cfg(unix)]
        {
            doc.snapshot()
        }
    }
    pub(in crate::resource_ops) fn read_document(
        &self,
        name: &str,
        max: u64,
        state: &mut DocState,
    ) -> Result<Option<Vec<u8>>> {
        let local = LocalDir(self.0.try_clone().map_err(error)?);
        let doc = Document::new(&local, name, max)?;
        #[cfg(windows)]
        {
            doc.read_state(state)
        }
        #[cfg(unix)]
        {
            state.as_mut().map(|s| doc.bytes(s)).transpose()
        }
    }
    pub(in crate::resource_ops) fn compare_document(
        &self,
        name: &str,
        max: u64,
        state: &DocState,
        bytes: Option<&[u8]>,
    ) -> Result<()> {
        let local = LocalDir(self.0.try_clone().map_err(error)?);
        let doc = Document::new(&local, name, max)?;
        #[cfg(windows)]
        {
            doc.compare_write(state, bytes)
        }
        #[cfg(unix)]
        {
            doc.replace(state.as_ref(), bytes.ok_or("Unix资源文档删除需原物理事务")?)
        }
    }
}
pub(in crate::resource_ops) fn stat_regular(m: &Metadata) -> bool {
    m.kind == Kind::Regular && m.reparse_tag.is_none()
}
pub(in crate::resource_ops) fn stat_directory(m: &Metadata) -> bool {
    m.kind == Kind::Directory && m.reparse_tag.is_none()
}
#[cfg(unix)]
pub(in crate::resource_ops) fn stat_mode(m: &Metadata) -> u32 {
    match m.security {
        retained_fs::Security::Unix { mode, .. } => mode,
        _ => unreachable!("native Unix security"),
    }
}
#[cfg(windows)]
pub(in crate::resource_ops) fn stat_security(m: &Metadata) -> retained_fs::Security {
    m.security.clone()
}
