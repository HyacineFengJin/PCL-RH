//! Explicit capability for a caller-authorized launcher document. No basename
//! heuristic applies to arbitrary game files or chooser inputs. This internal
//! Rust handle is not serializable and is not an additional plugin IPC grant.
use super::*;

pub(crate) struct Document<'a> {
    directory: &'a Dir,
    name: String,
    max: u64,
}

// Optional-state CAS is implemented by the Windows immutable revision store.
// Unix consumers retain their existing physical multi-file transaction; no
// weaker path-existence token is exposed as an equivalent absence guarantee.
#[cfg(windows)]
pub(crate) use super::windows_versions::Presence as DocumentPresence;
#[cfg(windows)]
pub(crate) struct DocumentState {
    inner: windows_versions::OwnedState,
}
#[cfg(windows)]
impl DocumentState {
    /// Stable journal evidence only; it cannot construct or authorize a CAS.
    pub fn fact_digest(&self) -> String {
        self.inner.fact_digest()
    }
    pub fn presence(&self) -> DocumentPresence {
        self.inner.presence()
    }
    /// The sequence is informative; only the opaque state authorizes CAS.
    /// NeverCreated and the initial physical Present both have sequence zero.
    pub fn generation(&self) -> usize {
        self.inner.generation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn fixture() -> (tempfile::TempDir, Dir) {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let owned = root
            .create_private(
                &Leaf::new("documents").unwrap(),
                pcl_core::retained_fs::Traversal::AllowMounts,
            )
            .unwrap();
        let directory = Dir(owned.try_clone_file().unwrap());
        (temporary, directory)
    }
    const NAME: &str = "caller-owned-dynamic.json";
    #[test]
    fn explicit_document_updates_do_not_expand_raw_file_reading_heuristics() {
        let (temporary, directory) = fixture();
        let document = Document::new(&directory, NAME, 65536).unwrap();
        assert!(document.snapshot().unwrap().is_none());
        assert_eq!(
            fs::read_dir(temporary.path().join("documents"))
                .unwrap()
                .count(),
            0
        );
        document.replace(None, b"original").unwrap();
        let original = document.snapshot().unwrap().unwrap();
        document
            .replace(Some(&original), b"logical revision")
            .unwrap();
        let mut current = document.snapshot().unwrap().unwrap();
        assert_eq!(document.bytes(&mut current).unwrap(), b"logical revision");
        assert!(document.recheck(&original).is_err());
        let raw = Snapshot::physical(&directory, NAME, 65536).unwrap();
        #[cfg(windows)]
        assert_eq!(raw.header, b"original");
        #[cfg(unix)]
        assert_eq!(raw.header, b"logical revision");
        document.replace(Some(&current), b"third revision").unwrap();
    }
    #[test]
    fn explicit_document_bounds_and_external_edits_preserve_the_physical_original() {
        let (temporary, directory) = fixture();
        assert!(Document::new(&directory, "../escape", 64).is_err());
        assert!(Document::new(&directory, NAME, 0).is_err());
        assert!(Document::new(&directory, NAME, 64 * 1024 * 1024 + 1).is_err());
        let document = Document::new(&directory, NAME, 64).unwrap();
        assert!(document.replace(None, b"").is_err());
        assert!(document.replace(None, &[b'x'; 65]).is_err());
        document.replace(None, b"original").unwrap();
        let snapshot = document.snapshot().unwrap().unwrap();
        let path = temporary.path().join("documents").join(NAME);
        fs::write(&path, b"outside edit").unwrap();
        assert!(document.replace(Some(&snapshot), b"new bytes").is_err());
        assert_eq!(fs::read(path).unwrap(), b"outside edit");
    }
    #[test]
    fn dynamic_documents_retain_their_explicit_limit_across_repeated_updates() {
        let (_temporary, directory) = fixture();
        let document = Document::new(&directory, NAME, 65536).unwrap();
        document.replace(None, &vec![b'a'; 20000]).unwrap();
        let original = document.snapshot().unwrap().unwrap();
        document
            .replace(Some(&original), &vec![b'b'; 20000])
            .unwrap();
        let mut current = document.snapshot().unwrap().unwrap();
        assert_eq!(document.bytes(&mut current).unwrap(), vec![b'b'; 20000]);
    }
    #[cfg(windows)]
    #[test]
    fn auxiliary_names_require_a_valid_exact_store_and_do_not_ignore_prefixes() {
        let (_temporary, directory) = fixture();
        let document = Document::new(&directory, NAME, 64).unwrap();
        document.replace(None, b"base").unwrap();
        let original = document.snapshot().unwrap().unwrap();
        document.replace(Some(&original), b"new bytes").unwrap();
        let mut expected = vec![
            format!(".local-versions-{NAME}"),
            format!(".local-versions-{NAME}.lock"),
        ];
        expected.sort();
        let mut names = document.auxiliary_names().unwrap();
        names.sort();
        assert_eq!(names, expected);
        directory
            .child(&format!(".local-versions-{NAME}"))
            .unwrap()
            .write_new("unknown", b"keep")
            .unwrap();
        assert!(document.auxiliary_names().is_err());
    }
    #[cfg(windows)]
    #[test]
    fn optional_document_facade_distinguishes_never_created_from_deleted() {
        let (_temporary, directory) = fixture();
        let document = Document::new(&directory, NAME, 65536).unwrap();
        let never = document.state().unwrap();
        assert_eq!(never.presence(), DocumentPresence::NeverCreated);
        assert_eq!(never.generation(), 0);
        document.compare_write(&never, Some(b"base")).unwrap();
        let current = document.state().unwrap();
        document.compare_write(&current, None).unwrap();
        let mut missing = document.state().unwrap();
        assert_eq!(missing.presence(), DocumentPresence::Tombstone);
        assert_eq!(missing.generation(), 1);
        assert_eq!(document.read_state(&mut missing).unwrap(), None);
        assert!(document.compare_write(&never, Some(b"stale")).is_err());
        document
            .compare_write(&missing, Some(b"reactivated"))
            .unwrap();
        let mut current = document.state().unwrap();
        assert_eq!(
            document.read_state(&mut current).unwrap().unwrap(),
            b"reactivated"
        );
        assert_eq!(current.generation(), 2);
        assert!(document.read_state(&mut missing).is_err());
        assert_eq!(document.auxiliary_names().unwrap().len(), 2);
    }
}
pub(crate) struct DocumentSnapshot {
    inner: Snapshot,
}
impl DocumentSnapshot {
    pub fn file(&self) -> &File {
        &self.inner.file
    }
    pub fn digest(&self) -> &str {
        &self.inner.digest
    }
}
impl<'a> Document<'a> {
    /// The caller must authorize this exact app-owned directory/name. This
    /// constructor checks bounds/privacy; it cannot infer business authority.
    pub fn new(directory: &'a Dir, name: &str, max: u64) -> Result<Self, String> {
        component(name)?;
        if max == 0 || max > 64 * 1024 * 1024 {
            return Err("启动器文档上限必须在 1 字节至 64 MiB 之间".into());
        }
        directory.require_private()?;
        Ok(Self {
            directory,
            name: name.into(),
            max,
        })
    }
    pub fn snapshot(&self) -> Result<Option<DocumentSnapshot>, String> {
        #[cfg(windows)]
        let current = windows_versions::snapshot_owned(self.directory, &self.name, self.max)?;
        #[cfg(unix)]
        let current = match self.directory.file(&self.name)? {
            None => None,
            Some(_) => Some(Snapshot::physical(self.directory, &self.name, self.max)?),
        };
        Ok(current.map(|inner| DocumentSnapshot { inner }))
    }
    #[cfg(windows)]
    pub fn state(&self) -> Result<DocumentState, String> {
        windows_versions::state_owned(self.directory, &self.name, self.max)
            .map(|inner| DocumentState { inner })
    }
    #[cfg(windows)]
    pub fn read_state(&self, expected: &mut DocumentState) -> Result<Option<Vec<u8>>, String> {
        windows_versions::read_state_owned(
            self.directory,
            &self.name,
            self.max,
            &mut expected.inner,
        )
    }
    /// `None` selects logical absence and never removes the permanent base.
    /// A real state token is required even when the expected value is absent.
    #[cfg(windows)]
    pub fn compare_write(
        &self,
        expected: &DocumentState,
        bytes: Option<&[u8]>,
    ) -> Result<(), String> {
        windows_versions::compare_owned(
            self.directory,
            &self.name,
            self.max,
            &expected.inner,
            bytes,
        )
    }
    pub fn recheck(&self, expected: &DocumentSnapshot) -> Result<(), String> {
        let fresh = self.snapshot()?.ok_or("启动器文档已消失")?;
        if fresh.inner.stamp != expected.inner.stamp
            || fresh.inner.digest != expected.inner.digest
            || Stamp::from_file(expected.file())? != expected.inner.stamp
        {
            return Err("启动器文档已变化，请重新读取；原内容已保留".into());
        }
        Ok(())
    }
    pub fn bytes(&self, expected: &mut DocumentSnapshot) -> Result<Vec<u8>, String> {
        if expected.inner.stamp.bytes > self.max {
            return Err("启动器文档超过当前能力的读取上限".into());
        }
        let file = &mut expected.inner.file;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(expected.inner.stamp.bytes as usize);
        Read::by_ref(file)
            .take(self.max + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        self.recheck(expected)?;
        if bytes.len() as u64 != expected.inner.stamp.bytes
            || format!("{:x}", Sha256::digest(&bytes)) != expected.inner.digest
        {
            return Err("启动器文档在读取期间已被编辑，原内容已保留".into());
        }
        Ok(bytes)
    }
    pub fn replace(&self, expected: Option<&DocumentSnapshot>, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() || bytes.len() as u64 > self.max {
            return Err("启动器文档为空或超过大小上限".into());
        }
        if let Some(snapshot) = expected {
            self.recheck(snapshot)?;
        }
        #[cfg(windows)]
        {
            windows_versions::replace_owned(
                self.directory,
                &self.name,
                expected.map(|v| &v.inner),
                bytes,
                self.max,
            )
        }
        #[cfg(unix)]
        {
            self.directory
                .replace(&self.name, expected.map(|v| &v.inner), bytes, self.max)
        }
    }
    /// Only validated protocol entries may be accounted for alongside a
    /// journal. This is not permission to delete them or ignore a name prefix.
    #[cfg(windows)]
    pub fn auxiliary_names(&self) -> Result<Vec<String>, String> {
        windows_versions::auxiliary_names_owned(self.directory, &self.name, self.max)
    }
}
