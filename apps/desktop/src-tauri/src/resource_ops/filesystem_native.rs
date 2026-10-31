//! Literal retained native directories. User originals move only by no-replace;
//! deletion is restricted to caller-verified newly generated private workspaces.
use super::{token_for, MAX_BATCH_BYTES, MAX_JOURNAL_BYTES};
use crate::launcher_local::filesystem::{documents::Document, Dir as LocalDir};
use pcl_core::retained_fs::{
    self, Directory, Kind, Leaf, Links, Metadata, Traversal, WorkspaceRemoval,
};
use std::{
    fs::File,
    ops::{Deref, DerefMut},
    path::Path,
    sync::Arc,
};
type Result<T> = std::result::Result<T, String>;
const MAX_NAMES: usize = 100_000;
fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn leaf(name: &str) -> Result<Leaf> {
    super::safe_name(name)?;
    Leaf::new(name).map_err(error)
}
pub(super) struct Dir {
    directory: Directory,
}
impl Dir {
    pub(super) fn open(path: &Path) -> Result<Self> {
        Directory::open_absolute(path)
            .map(|directory| Self { directory })
            .map_err(error)
    }
    pub(super) fn child(&self, name: &str) -> Result<Self> {
        self.directory
            .child(&leaf(name)?, Traversal::SameFilesystem)
            .map(|directory| Self { directory })
            .map_err(error)
    }
    pub(super) fn optional_child(&self, name: &str) -> Result<Option<Self>> {
        if self.metadata(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(super) fn create_child(&self, name: &str) -> Result<Self> {
        let directory = self
            .directory
            .create_private(&leaf(name)?, Traversal::SameFilesystem)
            .map_err(error)?;
        self.sync()?;
        Ok(Self { directory })
    }
    pub(super) fn ensure_child(&self, name: &str) -> Result<Self> {
        match self.optional_child(name)? {
            Some(dir) => Ok(dir),
            None => self.create_child(name),
        }
    }
    pub(super) fn metadata(&self, name: &str) -> Result<Option<Metadata>> {
        self.directory.entry(&leaf(name)?).map_err(error)
    }
    pub(super) fn regular_file(&self, name: &str) -> Result<File> {
        self.directory
            .open_regular(&leaf(name)?, Links::Any)
            .map_err(error)
    }
    pub(super) fn token(&self, name: &str) -> Result<String> {
        token_for(&self.regular_file(name)?)
    }
    pub(super) fn absent(&self, name: &str) -> Result<()> {
        if self.metadata(name)?.is_some() {
            Err(format!("已有同名文件，无法操作：{name}"))
        } else {
            Ok(())
        }
    }
    pub(super) fn sync(&self) -> Result<()> {
        self.directory.sync().map_err(error)
    }
    pub(super) fn names(&self) -> Result<Vec<String>> {
        let mut names = self
            .directory
            .names(MAX_NAMES)
            .map_err(error)?
            .into_iter()
            .map(|name| {
                name.into_string()
                    .map_err(|_| "操作目录含有无效文件名".to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }
    pub(super) fn same(&self, other: &Self) -> Result<bool> {
        Ok(self.directory.identity().map_err(error)?
            == other.directory.identity().map_err(error)?)
    }
    fn removal(result: std::io::Result<WorkspaceRemoval>) -> Result<()> {
        match result.map_err(error)? {
            WorkspaceRemoval::Removed => Ok(()),
            WorkspaceRemoval::MayHaveRemoved { error: e } => Err(format!("MayHaveRemoved：{e}")),
        }
    }
    pub(super) fn remove_file(
        &self,
        name: &str,
        expected: &File,
        evidence: &FileEvidence,
    ) -> Result<()> {
        self.directory
            .metadata()
            .map_err(error)?
            .require_private()
            .map_err(error)?;
        let metadata = retained_fs::metadata(expected).map_err(error)?;
        if &metadata != evidence
            || metadata.kind != Kind::Regular
            || metadata.reparse_tag.is_some()
            || metadata.links != 1
        {
            return Err("暂存条目类型或链接已变化".into());
        }
        // This metadata belongs to the exact hash-checked original descriptor,
        // never to whichever node happens to occupy this name afterwards.
        Self::removal(self.directory.remove_generated_link(&leaf(name)?, evidence))
    }
    pub(super) fn remove_empty_dir(&self, name: &str, expected: &Dir) -> Result<()> {
        let metadata = expected.directory.metadata().map_err(error)?;
        metadata.require_private().map_err(error)?;
        if !expected.names()?.is_empty() {
            return Err("暂存目录仍含有文件".into());
        }
        Self::removal(
            self.directory
                .remove_workspace_entry(&leaf(name)?, &metadata),
        )
    }
    #[cfg(test)]
    pub(super) fn create_file(&self, name: &str) -> Result<File> {
        self.directory.create_file(&leaf(name)?).map_err(error)
    }
    fn document_dir(&self) -> Result<LocalDir> {
        Ok(LocalDir(self.directory.try_clone_file().map_err(error)?))
    }
}
pub(super) fn open_source_parent(path: &Path) -> Result<Dir> {
    Dir::open(path)
}
pub(super) fn source_identity(file: &File) -> Result<String> {
    serde_json::to_string(&retained_fs::metadata(file).map_err(error)?.identity).map_err(error)
}
pub(super) fn rename_new(from: &Dir, source: &str, to: &Dir, target: &str) -> Result<()> {
    let source = leaf(source)?;
    let target = leaf(target)?;
    pcl_core::file_ops::rename_noreplace_at(
        from.directory.file(),
        source.as_os_str(),
        to.directory.file(),
        target.as_os_str(),
    )
    .map_err(error)?;
    from.sync()?;
    to.sync()
}
pub(super) fn journal_bytes(dir: &Dir) -> Result<Option<Vec<u8>>> {
    let local = dir.document_dir()?;
    let document = Document::new(&local, "journal.json", MAX_JOURNAL_BYTES as u64)?;
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
pub(super) fn publish_journal(dir: &Dir, data: &[u8]) -> Result<()> {
    if data.len() > MAX_JOURNAL_BYTES {
        return Err("资源恢复记录过大".into());
    }
    let local = dir.document_dir()?;
    let document = Document::new(&local, "journal.json", MAX_JOURNAL_BYTES as u64)?;
    #[cfg(windows)]
    {
        let mut state = document.state()?;
        // Inspect the exact logical revision before CAS; no raw physical base
        // is mistaken for the current journal or an absent state.
        document.read_state(&mut state)?;
        document.compare_write(&state, Some(data))
    }
    #[cfg(unix)]
    {
        let snapshot = document.snapshot()?;
        document.replace(snapshot.as_ref(), data)
    }
}

// Old business journals start after local copies have completed. The separate
// shared registry therefore owns incomplete native copies before the first
// business journal, and allocates before exposing their writer.
pub(super) struct Copy {
    input: crate::modrinth_install::source::Input,
    area: Arc<crate::modrinth_install::source::Area>,
}
impl Deref for Copy {
    type Target = File;
    fn deref(&self) -> &File {
        &self.input
    }
}
impl DerefMut for Copy {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.input
    }
}
pub(super) fn copy_guard(root: &Dir) -> Result<Arc<crate::modrinth_install::source::Area>> {
    let private = root.ensure_child(".pcl-linux")?;
    crate::modrinth_install::source::open_area(
        private.directory.file(),
        "resource-local-inputs",
        MAX_BATCH_BYTES + 512 * 1024 * 1024,
    )
}
pub(super) type CopyGuard = Arc<crate::modrinth_install::source::Area>;
pub(super) fn create_copy(area: &CopyGuard, target: &Dir, name: &str, max: u64) -> Result<Copy> {
    let input =
        crate::modrinth_install::source::allocate(area, target.directory.file(), name, max)?;
    Ok(Copy {
        input,
        area: area.clone(),
    })
}
pub(super) fn complete_copy(mut copy: Copy, target: &Dir, name: &str) -> Result<String> {
    copy.input.verify_owned()?;
    copy.input.publish(target.directory.file(), name)?;
    copy.area.check_cleanup()?;
    // The original writable owner has retired before observing Windows mtime.
    target.token(name)
}
pub(super) fn check_copy_cleanup(area: &CopyGuard) -> Result<()> {
    area.check_cleanup()
}

pub(super) type FileEvidence = Metadata;
pub(super) fn file_evidence(file: &File) -> Result<FileEvidence> {
    retained_fs::metadata(file).map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};
    #[test]
    fn generated_cleanup_uses_original_descriptor_and_hash_bracket_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Dir::open(&temporary.path().canonicalize().unwrap()).unwrap();
        let stage = root.create_child("stage").unwrap();
        let mut created = stage.create_file("item").unwrap();
        created.write_all(b"original").unwrap();
        drop(created);
        let expected = stage.regular_file("item").unwrap();
        let evidence = file_evidence(&expected).unwrap();
        let stage_path = temporary.path().join("stage");
        fs::rename(stage_path.join("item"), stage_path.join("old")).unwrap();
        fs::write(stage_path.join("item"), b"unrelated keep").unwrap();
        assert!(stage.remove_file("item", &expected, &evidence).is_err());
        assert_eq!(
            fs::read(stage_path.join("item")).unwrap(),
            b"unrelated keep"
        );
        assert_eq!(fs::read(stage_path.join("old")).unwrap(), b"original");
    }
    #[test]
    fn generated_cleanup_rejects_same_inode_changed_after_hash_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Dir::open(&temporary.path().canonicalize().unwrap()).unwrap();
        let stage = root.create_child("stage").unwrap();
        let mut created = stage.create_file("item").unwrap();
        created.write_all(b"before").unwrap();
        drop(created);
        let expected = stage.regular_file("item").unwrap();
        let evidence = file_evidence(&expected).unwrap();
        let path = temporary.path().join("stage/item");
        fs::write(&path, b"changed after verification").unwrap();
        assert!(stage.remove_file("item", &expected, &evidence).is_err());
        assert_eq!(fs::read(path).unwrap(), b"changed after verification");
    }
}
