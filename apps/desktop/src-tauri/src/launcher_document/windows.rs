//! Windows document adapter. The common immutable logical-document engine owns
//! commit chains, base fences, read leases, receipts, durability and recovery.
//! These two fixed names retain independent writer locks and size bounds.
use crate::launcher_local::filesystem as local;
use pcl_core::retained_fs::{
    self, Directory as NativeDirectory, Identity, Kind, Leaf, Links, Lock, Traversal,
};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

const DIRECTORY_NAME: &str = ".pcl-rust";
#[derive(Clone, Copy)]
enum DocumentKind {
    Preferences,
    Favorites,
}
impl DocumentKind {
    fn file(self) -> &'static str {
        match self {
            Self::Preferences => "launcher-preferences.json",
            Self::Favorites => "resource-favorites.json",
        }
    }
    fn lock(self) -> &'static str {
        match self {
            Self::Preferences => ".launcher-preferences.lock",
            Self::Favorites => ".resource-favorites.lock",
        }
    }
    fn max_bytes(self) -> usize {
        match self {
            Self::Preferences => 64 * 1024,
            Self::Favorites => 4 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    metadata: retained_fs::Metadata,
    digest: [u8; 32],
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    directory: Option<Identity>,
    file: Option<Token>,
}
impl Snapshot {
    pub(crate) fn has_file(&self) -> bool {
        self.file.is_some()
    }
}
pub(crate) struct Directory(NativeDirectory, DocumentKind);
pub(crate) struct WriteLock(#[allow(dead_code)] Lock);
fn leaf(name: &str) -> Result<Leaf, String> {
    Leaf::new(name).map_err(|e| e.to_string())
}
impl Directory {
    pub(crate) fn open_preferences(path: &Path) -> Result<Self, String> {
        Self::open_for(path, DocumentKind::Preferences)
    }
    pub(crate) fn open_favorites(path: &Path) -> Result<Self, String> {
        Self::open_for(path, DocumentKind::Favorites)
    }
    fn open_for(path: &Path, kind: DocumentKind) -> Result<Self, String> {
        retained_fs::validate_absolute_path(path).map_err(|e| e.to_string())?;
        Ok(Self(
            NativeDirectory::open_absolute(path).map_err(|e| e.to_string())?,
            kind,
        ))
    }
    pub(crate) fn identity(&self) -> Result<Identity, String> {
        self.0.identity().map_err(|e| e.to_string())
    }
    fn local(&self) -> Result<local::Dir, String> {
        Ok(local::Dir(
            self.0.try_clone_file().map_err(|e| e.to_string())?,
        ))
    }
    pub(crate) fn storage(&self) -> Result<Option<Self>, String> {
        let name = leaf(DIRECTORY_NAME)?;
        let Some(before) = self.0.entry(&name).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        if before.kind != Kind::Directory || before.reparse_tag.is_some() {
            return Err("启动器本地数据目录不是普通目录，原内容已保留".into());
        }
        let directory = self
            .0
            .child(&name, Traversal::AllowMounts)
            .map_err(|e| e.to_string())?;
        self.0
            .recheck(&name, &directory.identity().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if directory.identity().map_err(|e| e.to_string())? != before.identity {
            return Err("启动器本地数据目录在读取期间已变化".into());
        }
        Ok(Some(Self(directory, self.1)))
    }
    pub(crate) fn ensure_storage(&self) -> Result<Self, String> {
        if let Some(dir) = self.storage()? {
            return Ok(dir);
        }
        match self
            .0
            .create_private(&leaf(DIRECTORY_NAME)?, Traversal::AllowMounts)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("无法创建启动器本地数据目录：{e}")),
        }
        self.0.sync().map_err(|e| e.to_string())?;
        self.storage()?
            .ok_or_else(|| "启动器本地数据目录已消失".into())
    }
    fn read_document(&self) -> Result<Option<(Token, Vec<u8>)>, String> {
        let name = self.1.file();
        let dir = self.local()?;
        let Some(mut snapshot) = local::optional_snapshot(&dir, name, self.1.max_bytes() as u64)?
        else {
            return Ok(None);
        };
        let bytes = snapshot.bytes(&dir, name)?;
        let metadata = retained_fs::metadata(&snapshot.file).map_err(|e| e.to_string())?;
        if metadata.len != bytes.len() as u64
            || metadata.kind != Kind::Regular
            || metadata.links != 1
            || metadata.reparse_tag.is_some()
        {
            return Err("启动器本地数据文件在读取期间已变化".into());
        }
        let digest = Sha256::digest(&bytes).into();
        Ok(Some((Token { metadata, digest }, bytes)))
    }
    /// A chooser authorizes the selected literal physical file, even when its
    /// basename matches a managed document. Never interpret adjacent sidecars
    /// here or impose the named-stage private-parent policy on read-only input.
    pub(crate) fn read_bytes(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let leaf = leaf(name)?;
        let Some(before) = self.0.entry(&leaf).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        if before.kind != Kind::Regular || before.links != 1 || before.reparse_tag.is_some() {
            return Err("启动器本地数据必须是独立的普通文件，不允许符号链接或硬链接".into());
        }
        let max = self.1.max_bytes();
        if before.len > max as u64 {
            return Err(format!("启动器本地数据文件超过 {} KiB 上限", max / 1024));
        }
        let mut file = self
            .0
            .open_regular(&leaf, Links::Single)
            .map_err(|e| e.to_string())?;
        let opened = retained_fs::metadata(&file).map_err(|e| e.to_string())?;
        if opened != before {
            return Err("启动器本地数据文件在打开期间已变化".into());
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if bytes.len() > max
            || bytes.len() as u64 != opened.len
            || retained_fs::metadata(&file).map_err(|e| e.to_string())? != opened
            || self.0.entry(&leaf).map_err(|e| e.to_string())?.as_ref() != Some(&opened)
        {
            return Err("启动器本地数据文件在读取期间已变化或超过大小上限".into());
        }
        // Independently re-read the held object and compare SHA-256. Native
        // metadata/name checks bracket both reads, without a writable lease.
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 8192];
        let mut count = 0u64;
        loop {
            let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            count = count
                .checked_add(read as u64)
                .ok_or("启动器本地数据文件读取大小溢出")?;
            if count > max as u64 {
                return Err("启动器本地数据文件在读取期间超过大小上限".into());
            }
            hash.update(&buffer[..read]);
        }
        let confirmed: [u8; 32] = hash.finalize().into();
        if count != opened.len
            || confirmed != digest
            || retained_fs::metadata(&file).map_err(|e| e.to_string())? != opened
            || self.0.entry(&leaf).map_err(|e| e.to_string())?.as_ref() != Some(&opened)
        {
            return Err("启动器本地数据文件在读取期间已变化".into());
        }
        Ok(Some(bytes))
    }
    pub(crate) fn write_lock(&self) -> Result<WriteLock, String> {
        self.0
            .lock(&leaf(self.1.lock())?)
            .map(WriteLock)
            .map_err(|e| format!("无法取得启动器本地数据写锁：{e}"))
    }
    pub(crate) fn publish_anonymous(
        &self,
        parent: &Path,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        super::publish_named_export(self.0.clone(), parent, name, bytes, self.1.max_bytes())
    }
}
pub(crate) fn snapshot(project: &Directory) -> Result<(Snapshot, Option<Vec<u8>>), String> {
    let Some(dir) = project.storage()? else {
        return Ok((Snapshot::default(), None));
    };
    let identity = dir.identity()?;
    let read = dir.read_document()?;
    if project.storage()?.map(|d| d.identity()).transpose()? != Some(identity.clone()) {
        return Err("启动器本地数据目录在读取期间已被替换".into());
    }
    Ok((
        Snapshot {
            directory: Some(identity),
            file: read.as_ref().map(|(token, _)| token.clone()),
        },
        read.map(|(_, bytes)| bytes),
    ))
}
pub(crate) fn prepare_directory(
    project: &Directory,
    expected: &mut Snapshot,
) -> Result<Directory, String> {
    let dir = project.ensure_storage()?;
    let identity = dir.identity()?;
    if expected.directory.is_none() && expected.file.is_none() {
        if dir.read_document()?.is_some() {
            return Err("启动器本地数据已被外部创建，请重新读取".into());
        }
        expected.directory = Some(identity.clone());
    }
    if expected.directory.as_ref() != Some(&identity) {
        return Err("启动器本地数据目录已被外部替换".into());
    }
    Ok(dir)
}
pub(crate) fn verify(
    project: &Directory,
    project_path: &Path,
    expected: &Snapshot,
) -> Result<(), String> {
    if Directory::open_for(project_path, project.1)?.identity()? != project.identity()? {
        return Err("启动器项目目录已被外部替换，请重新打开启动器".into());
    }
    let current = snapshot(project)?.0;
    if expected.directory.is_none() && expected.file.is_none() && current.file.is_none() {
        return Ok(());
    }
    if current != *expected {
        return Err("启动器本地数据已被外部修改，原文件已保留，请重新读取后重试".into());
    }
    Ok(())
}
pub(crate) fn persist(
    project: &Directory,
    project_path: &Path,
    dir: &Directory,
    expected: &Snapshot,
    bytes: &[u8],
    _temporary: &str,
    before_replace: impl FnOnce() -> Result<(), String>,
) -> Result<Snapshot, String> {
    if bytes.len() > project.1.max_bytes() {
        return Err("启动器本地数据文件超过安全上限".into());
    }
    verify(project, project_path, expected)?;
    let local = dir.local()?;
    let current = local::optional_snapshot(&local, project.1.file(), project.1.max_bytes() as u64)?;
    before_replace()?;
    verify(project, project_path, expected)?;
    local.replace(
        project.1.file(),
        current.as_ref(),
        bytes,
        project.1.max_bytes() as u64,
    )?;
    let (after, actual) = snapshot(project)?;
    if after.directory != expected.directory
        || actual.as_deref() != Some(bytes)
        || Directory::open_for(project_path, project.1)?.identity()? != project.identity()?
    {
        return Err("启动器本地数据可能已发布，目录或文件发生变化，请重新读取".into());
    }
    Ok(after)
}
