//! Managed runtimes publish as a complete directory with no replacement.
//! Every write and cleanup is relative to held private-directory capabilities;
//! links are leaf entries from a validated manifest, never traversal authority.
//! A durable owner record makes interrupted stages recognizable. Unknown or
//! modified contents are retained, and cleanup failure outranks cancellation.
//! Schema 2 keeps native named sources outside the runtime manifest. Ordinary
//! cancellation seals partial bytes before cleanup; a process loss before that
//! evidence is durable retains the unsealed lease for inspection. Unix schema 1
//! remains readable without granting it authority over this new source area.
use super::{
    catalog::{self, Entry, Manifest, Package},
    Result,
};
use crate::launcher_local::filesystem::DirectoryIdentity;
use crate::launcher_local::filesystem::{Dir, WriteLock};
use pcl_core::retained_fs::stage::{Evidence, PublishOutcome, Stage as FileStage};
use pcl_core::retained_fs::{
    self, Directory, Identity, Kind, Leaf, Links, Metadata, Security, Traversal, WorkspaceRemoval,
};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
#[cfg(unix)]
use std::{
    ffi::CString,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
const SOURCES: &str = ".pcl-java-sources";
const OWNER: &str = ".pcl-java-owner.json";
const OWNER_LIMIT: u64 = 4 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Owner {
    schema: u32,
    stage: String,
    final_name: String,
    identity: DirectoryIdentity,
    manifest: Manifest,
}
pub struct Store {
    pub path: PathBuf,
    pub(super) dir: Dir,
}
pub struct Stage<'a> {
    store: &'a Store,
    pub(super) dir: Dir,
    name: String,
    owner: Owner,
    published: bool,
    _lock: &'a WriteLock,
}
fn native(dir: &Dir) -> Result<Directory> {
    Directory::from_file(dir.0.try_clone().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn leaf(name: &str) -> Result<Leaf> {
    Leaf::new(name).map_err(|e| e.to_string())
}
#[cfg(unix)]
fn c(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| "Java 路径含有空字符".into())
}
#[cfg(unix)]
fn io(label: &str) -> String {
    format!("{label}：{}", std::io::Error::last_os_error())
}
fn child(parent: &Dir, name: &str) -> Result<Dir> {
    let dir = native(parent)?
        .child(&leaf(name)?, Traversal::SameFilesystem)
        .map_err(|e| e.to_string())?;
    dir.metadata()
        .and_then(|m| m.require_private())
        .map_err(|e| e.to_string())?;
    Ok(Dir(dir.try_clone_file().map_err(|e| e.to_string())?))
}
fn descend(root: &Dir, path: &str) -> Result<(Dir, String)> {
    let mut parts = catalog::parts(path)?;
    let leaf = parts.pop().unwrap().to_string();
    let mut dir = Dir(root.0.try_clone().map_err(|e| e.to_string())?);
    for part in parts {
        dir = child(&dir, part)?;
    }
    Ok((dir, leaf))
}
#[cfg(unix)]
fn link_target(parent: &Dir, name: &str) -> Result<String> {
    let mut bytes = vec![0u8; 4097];
    let n = unsafe {
        libc::readlinkat(
            parent.0.as_raw_fd(),
            c(name)?.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if n < 0 || n as usize == bytes.len() {
        return Err(io("Java 内部链接无效或过长"));
    }
    String::from_utf8(bytes[..n as usize].to_vec()).map_err(|_| "Java 内部链接不是 UTF-8".into())
}
#[cfg(windows)]
fn link_target(_: &Dir, _: &str) -> Result<String> {
    Err("Windows 官方 Java 清单不支持内部链接".into())
}
#[cfg(unix)]
fn create_link(parent: &Dir, name: &str, target: &str) -> Result<()> {
    if unsafe { libc::symlinkat(c(target)?.as_ptr(), parent.0.as_raw_fd(), c(name)?.as_ptr()) } != 0
    {
        return Err(io("无法建立 Java 内部链接"));
    }
    parent.sync()
}
#[cfg(windows)]
fn create_link(_: &Dir, _: &str, _: &str) -> Result<()> {
    Err("Windows 官方 Java 清单不支持内部链接".into())
}
fn remove(parent: &Directory, name: &Leaf, metadata: &Metadata) -> Result<()> {
    // A Darwin entry snapshot intentionally has no extended-ACL evidence.
    // Obtain complete security/change evidence from the held original instead
    // of comparing its ACL to a pathname-only placeholder or granting it trust.
    let held = match metadata.kind {
        Kind::Directory => parent
            .child(name, Traversal::SameFilesystem)
            .and_then(|directory| directory.try_clone_file()),
        Kind::Regular => parent.open_regular(name, Links::Single),
        _ => return Err("Java 清理只允许已验证的普通文件或空目录".into()),
    }
    .map_err(|e| e.to_string())?;
    if retained_fs::metadata(&held).map_err(|e| e.to_string())? != *metadata {
        return Err("Java 暂存项在删除前已变化，已保留目录".into());
    }
    parent
        .recheck(name, &metadata.identity)
        .map_err(|e| e.to_string())?;
    // This full-stamp preflight does not make Unix unlink a content CAS. The
    // native operation additionally binds the original object identity; it must
    // never acquire cleanup authority from a replacement same-name entry.
    match parent
        .remove_workspace_entry(name, metadata)
        .map_err(|e| e.to_string())?
    {
        WorkspaceRemoval::Removed => Ok(()),
        WorkspaceRemoval::MayHaveRemoved { error } => {
            Err(format!("Java 暂存项可能已清理，已停止后续删除：{error}"))
        }
    }
}
fn unlink(parent: &Dir, name: &str, directory: bool) -> Result<()> {
    let parent = native(parent)?;
    let name = leaf(name)?;
    let stamp = parent
        .entry(&name)
        .map_err(|e| e.to_string())?
        .ok_or("Java 暂存项丢失")?;
    if stamp.kind
        != if directory {
            Kind::Directory
        } else {
            Kind::Regular
        }
    {
        return Err("Java 暂存项类型改变".into());
    }
    remove(&parent, &name, &stamp)
}
#[cfg(unix)]
fn unlink_link(parent: &Dir, name: &str, target: &str) -> Result<()> {
    if link_target(parent, name)? != target {
        return Err("Java 内部链接已修改，已保留目录".into());
    }
    if unsafe { libc::unlinkat(parent.0.as_raw_fd(), c(name)?.as_ptr(), 0) } != 0 {
        return Err(io("无法清理 Java 链接"));
    }
    parent.sync()
}
#[cfg(windows)]
fn unlink_link(_: &Dir, _: &str, _: &str) -> Result<()> {
    Err("Windows 官方 Java 清单不支持内部链接".into())
}
fn verify_file(file: &mut File, size: u64, hash: &str, executable: bool) -> Result<Metadata> {
    let meta = retained_fs::metadata(file).map_err(|e| e.to_string())?;
    meta.require_private().map_err(|e| e.to_string())?;
    let mode_matches = match &meta.security {
        Security::Unix { mode, .. } => mode & 0o777 == if executable { 0o700 } else { 0o600 },
        Security::Windows { .. } => true,
    };
    if meta.len != size || !mode_matches {
        return Err("Java 暂存文件大小或权限已变化，已保留文件".into());
    }
    let mut digest = Sha1::new();
    let mut total = 0u64;
    let mut bytes = [0u8; 65536];
    loop {
        let n = file.read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > size {
            return Err("Java 暂存文件已变化".into());
        }
        digest.update(&bytes[..n]);
    }
    if total != size
        || format!("{:x}", digest.finalize()) != hash
        || retained_fs::metadata(file).map_err(|e| e.to_string())? != meta
    {
        return Err("Java 暂存内容已被修改，已保留文件".into());
    }
    Ok(meta)
}
impl Store {
    pub fn capture(project: &Path) -> Result<Self> {
        super::StoreBinding::capture(project)?.open()
    }
    fn recheck(&self) -> Result<()> {
        if Dir::absolute(&self.path)?.identity()? != self.dir.identity()? {
            return Err("Java 托管目录已改变".into());
        }
        Ok(())
    }
    pub(super) fn recover(&self, lock: &WriteLock) -> Result<()> {
        self.recheck()?;
        for name in self
            .dir
            .names()?
            .into_iter()
            .filter(|n| n.starts_with(".stage-"))
        {
            let dir = child(&self.dir, &name)?;
            let mut file = dir
                .file(OWNER)?
                .ok_or("Java 暂存目录缺少所有权记录，已保留目录")?;
            let mut raw = Vec::new();
            Read::by_ref(&mut file)
                .take(OWNER_LIMIT + 1)
                .read_to_end(&mut raw)
                .map_err(|e| e.to_string())?;
            if raw.len() as u64 > OWNER_LIMIT {
                return Err("Java 暂存记录超过限制，已保留目录".into());
            }
            let owner: Owner = serde_json::from_slice(&raw)
                .map_err(|_| "Java 暂存记录不支持或已修改，已保留目录")?;
            if !supported_schema(owner.schema)
                || owner.stage != name
                || owner.identity != dir.identity()?
            {
                return Err("Java 暂存记录身份不匹配，已保留目录".into());
            }
            owner.manifest.validate()?;
            Stage {
                store: self,
                dir,
                name,
                owner,
                published: false,
                _lock: lock,
            }
            .cleanup()?;
        }
        Ok(())
    }
    pub(super) fn create<'a>(
        &'a self,
        package: &Package,
        manifest: &Manifest,
        task_id: &str,
        lock: &'a WriteLock,
    ) -> Result<Stage<'a>> {
        self.recheck()?;
        let final_name = format!("{}-{}", package.component, package.sha1);
        // Refuse an existing runtime before transferring any payload. It is
        // never overwritten, even if its executable later becomes unavailable.
        if native(&self.dir)?
            .entry(&leaf(&final_name)?)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("该 Java 运行时已安装，原文件已保留".into());
        }
        let name = format!(".stage-{task_id}");
        let parent = native(&self.dir)?;
        let created = parent
            .create_private(&leaf(&name)?, Traversal::SameFilesystem)
            .map_err(|e| e.to_string())?;
        parent.sync().map_err(|e| e.to_string())?;
        let dir = Dir(created.try_clone_file().map_err(|e| e.to_string())?);
        let owner = Owner {
            schema: current_schema(),
            stage: name.clone(),
            final_name,
            identity: dir.identity()?,
            manifest: manifest.clone(),
        };
        let stage = Stage {
            store: self,
            dir,
            name,
            owner,
            published: false,
            _lock: lock,
        };
        let raw = serde_json::to_vec(&stage.owner).map_err(|e| e.to_string())?;
        if raw.len() as u64 > OWNER_LIMIT {
            unlink(&self.dir, &stage.name, true)?;
            return Err("Java 暂存记录超过限制".into());
        }
        if let Err(e) = write_record(&native(&stage.dir)?, OWNER, &raw) {
            // A failed directory sync may mean the marker exists. Its content
            // is still compared with our exact in-memory owner before cleanup.
            if stage.dir.names()?.is_empty() {
                unlink(&self.dir, &stage.name, true)?;
            } else {
                stage.cleanup()?;
            }
            return Err(e);
        }
        Ok(stage)
    }
}
impl Stage<'_> {
    pub fn prepare_directories(&self) -> Result<()> {
        let mut directories: Vec<_> = self
            .owner
            .manifest
            .files
            .iter()
            .filter_map(|(p, e)| matches!(e, Entry::Directory).then_some(p))
            .collect();
        directories.sort_by_key(|p| p.matches('/').count());
        for path in directories {
            let (parent, name) = descend(&self.dir, path)?;
            parent.create(&name)?;
            child(&parent, &name)?;
        }
        Ok(())
    }
    pub fn download(&self, path: &str) -> Result<DownloadFile> {
        let (target, _) = descend(&self.dir, path)?;
        let Some(Entry::File { downloads, .. }) = self.owner.manifest.files.get(path) else {
            return Err("Java 下载目标不在清单中".into());
        };
        let root = native(&self.dir)?;
        let name = leaf(SOURCES)?;
        let source = match root.child(&name, Traversal::SameFilesystem) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match root.create_private(&name, Traversal::SameFilesystem) {
                    Ok(dir) => {
                        root.sync().map_err(|e| e.to_string())?;
                        dir
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => root
                        .child(&name, Traversal::SameFilesystem)
                        .map_err(|e| e.to_string())?,
                    Err(e) => return Err(e.to_string()),
                }
            }
            Err(e) => return Err(e.to_string()),
        };
        source
            .metadata()
            .and_then(|m| m.require_private())
            .map_err(|e| e.to_string())?;
        let stage = FileStage::new(&source).map_err(|e| e.to_string())?;
        Ok(DownloadFile {
            stage: Some(stage),
            path: path.into(),
            max: downloads["raw"].size,
            target: native(&target)?,
        })
    }
    pub fn finish_download(
        &self,
        path: &str,
        mut file: DownloadFile,
        executable: bool,
    ) -> Result<()> {
        if file.path != path {
            return Err("Java 下载目标已改变".into());
        }
        #[cfg(unix)]
        file.stage
            .as_mut()
            .ok_or("Java 下载已封存")?
            .file_mut()
            .set_permissions(std::fs::Permissions::from_mode(if executable {
                0o700
            } else {
                0o600
            }))
            .map_err(|e| e.to_string())?;
        #[cfg(windows)]
        let _ = executable;
        let sealed = seal_download(&mut file)?;
        let (parent, name) = descend(&self.dir, path)?;
        if native(&parent)?.identity().map_err(|e| e.to_string())?
            != file.target.identity().map_err(|e| e.to_string())?
        {
            return Err("Java 下载目标父目录已改变，已保留命名源".into());
        }
        let outcome = sealed
            .publish_new(&file.target, &leaf(&name)?)
            .map_err(|e| e.to_string())?;
        match outcome {
            PublishOutcome::Published { .. } => Ok(()),
            PublishOutcome::Exists { .. } => Err("Java 暂存文件目标已存在，原文件已保留".into()),
            PublishOutcome::MayHavePublished { error, .. } => Err(format!(
                "Java 文件可能已发布，命名源与清单均已保留：{error}"
            )),
        }
    }
    pub fn links(&self) -> Result<()> {
        for (path, entry) in &self.owner.manifest.files {
            if let Entry::Link { target } = entry {
                let (parent, name) = descend(&self.dir, path)?;
                create_link(&parent, &name, target)?;
            }
        }
        Ok(())
    }
    fn validate(&self, complete: bool) -> Result<Metadata> {
        let mut file = self.dir.file(OWNER)?.ok_or("Java 暂存所有权记录缺失")?;
        let owner_stamp = retained_fs::metadata(&file).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(OWNER_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > OWNER_LIMIT
            || serde_json::from_slice::<Owner>(&bytes).ok().as_ref() != Some(&self.owner)
            || retained_fs::metadata(&file).map_err(|e| e.to_string())? != owner_stamp
        {
            return Err("Java 暂存记录已改变，已保留目录".into());
        }
        if self.owner.schema == 2 {
            self.check_sources(false)?;
        }
        let count = self.walk(&self.dir, "", false, &mut |_| {})?;
        if complete && count != self.owner.manifest.files.len() {
            return Err("Java 运行时文件不完整".into());
        }
        Ok(owner_stamp)
    }
    fn walk(
        &self,
        dir: &Dir,
        prefix: &str,
        remove_files: bool,
        before_remove: &mut impl FnMut(&str),
    ) -> Result<usize> {
        let mut count = 0;
        // A legal 4096-entry manifest can have 4095 names at its root.
        // Owner/source-area entries are protocol state, not manifest entries.
        // Keep their budget separate; every business name is still checked
        // against the manifest below and complete counts only business entries.
        let internal = if prefix.is_empty() {
            if self.owner.schema == 2 {
                2
            } else {
                1
            }
        } else {
            0
        };
        for name in native(dir)?
            .names(4096 + internal)
            .map_err(|e| e.to_string())?
        {
            let name = name
                .into_string()
                .map_err(|_| "Java 暂存文件名不是 UTF-8")?;
            if prefix.is_empty() && (name == OWNER || (name == SOURCES && self.owner.schema == 2)) {
                continue;
            }
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let entry = self
                .owner
                .manifest
                .files
                .get(&path)
                .ok_or("Java 暂存目录出现额外文件，已保留目录")?;
            match entry {
                Entry::Directory => {
                    let next = child(dir, &name)?;
                    count += self.walk(&next, &path, remove_files, before_remove)?;
                    if remove_files {
                        let stamp = native(&next)?.metadata().map_err(|e| e.to_string())?;
                        before_remove(&path);
                        remove(&native(dir)?, &leaf(&name)?, &stamp)?;
                    }
                }
                Entry::File {
                    executable,
                    downloads,
                } => {
                    let raw = downloads.get("raw").unwrap();
                    let mut file = dir.file(&name)?.ok_or("Java 暂存文件丢失")?;
                    let stamp = verify_file(&mut file, raw.size, &raw.sha1, *executable)?;
                    if remove_files {
                        before_remove(&path);
                        // Keep the verified original handle and its full stamp
                        // through deletion. A new same-name leaf cannot inherit
                        // the original payload's cleanup authority.
                        remove(&native(dir)?, &leaf(&name)?, &stamp)?;
                    }
                }
                Entry::Link { target } => {
                    if link_target(dir, &name)? != *target {
                        return Err("Java 内部链接已修改，已保留目录".into());
                    }
                    if remove_files {
                        before_remove(&path);
                        unlink_link(dir, &name, target)?;
                    }
                }
            }
            count += 1;
        }
        Ok(count)
    }
    pub fn java_path(&self) -> Result<PathBuf> {
        self.store.recheck()?;
        if child(&self.store.dir, &self.name)?.identity()? != self.dir.identity()? {
            return Err("Java 暂存目录已改变".into());
        }
        Ok(self
            .store
            .path
            .join(&self.name)
            .join(self.owner.manifest.java_entry()?))
    }
    pub fn verify_complete(&self) -> Result<()> {
        self.validate(true).map(|_| ())
    }
    pub fn publish(&mut self) -> Result<PathBuf> {
        self.validate(true)?;
        self.java_path()?;
        self.dir.sync()?;
        // All named sources/receipts must reconcile before the public tree moves.
        if self.owner.schema == 2 {
            self.check_sources(true)?;
        }
        let parent = native(&self.store.dir)?;
        let expected = native(&self.dir)?.identity().map_err(|e| e.to_string())?;
        if let Err(error) = pcl_core::file_ops::rename_noreplace_at(
            parent.file(),
            leaf(&self.name)?.as_os_str(),
            parent.file(),
            leaf(&self.owner.final_name)?.as_os_str(),
        ) {
            // An error after a native rename may already have committed. Do not
            // let the ordinary failure path authorize deleting that runtime.
            self.published = true;
            let old = parent.entry(&leaf(&self.name)?).map_err(|e| e.to_string());
            let new = parent
                .entry(&leaf(&self.owner.final_name)?)
                .map_err(|e| e.to_string());
            if matches!((&old, &new), (Ok(Some(old)), Ok(None)) if old.identity == expected) {
                self.published = false;
            }
            return Err(format!(
                "无法发布 Java 运行时，可能已移动；已保留现存目录：{error}"
            ));
        }
        self.published = true;
        parent
            .recheck(&leaf(&self.owner.final_name)?, &expected)
            .map_err(|e| format!("Java 目录已移动，但身份复核失败，已保留运行时：{e}"))?;
        // Once renamed, a later sync error cannot authorize deleting a complete
        // runtime. The caller reports the retained installation for recovery.
        self.store
            .dir
            .sync()
            .map_err(|e| format!("Java 已安装，但目录同步失败，请保留运行时：{e}"))?;
        Ok(self
            .store
            .path
            .join(&self.owner.final_name)
            .join(self.owner.manifest.java_entry()?))
    }
    pub fn cleanup(&self) -> Result<()> {
        self.cleanup_before_remove(&mut |_| {})
    }
    fn cleanup_before_remove(&self, before_remove: &mut impl FnMut(&str)) -> Result<()> {
        if self.published {
            return Ok(());
        }
        if child(&self.store.dir, &self.name)?.identity()? != self.dir.identity()? {
            return Err("Java 暂存目录身份已改变，已保留目录".into());
        }
        let owner_stamp = self.validate(false)?;
        if self.owner.schema == 2 {
            self.check_sources(true)?;
        }
        self.walk(&self.dir, "", true, before_remove)?;
        before_remove(OWNER);
        remove(&native(&self.dir)?, &leaf(OWNER)?, &owner_stamp)?;
        let stamp = native(&self.dir)?.metadata().map_err(|e| e.to_string())?;
        before_remove(&self.name);
        remove(&native(&self.store.dir)?, &leaf(&self.name)?, &stamp)
    }
}

fn current_schema() -> u32 {
    2
}
fn supported_schema(schema: u32) -> bool {
    schema == current_schema() || (cfg!(unix) && schema == 1)
}

// Records are never replaced. A short write/failed flush is recovery evidence,
// and is deliberately retained instead of being interpreted as an empty source.
fn write_record(dir: &Directory, name: &str, raw: &[u8]) -> Result<()> {
    let mut file = dir.create_file(&leaf(name)?).map_err(|e| e.to_string())?;
    file.write_all(raw)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    dir.sync().map_err(|e| e.to_string())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRecord {
    path: String,
    stamp: Metadata,
    target: Identity,
    evidence: Evidence,
}
fn record_name(path: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(".record-{:x}", Sha256::digest(path.as_bytes()))
}
pub struct DownloadFile {
    stage: Option<FileStage>,
    path: String,
    max: u64,
    target: Directory,
}
pub fn download_handle(file: &mut DownloadFile) -> &mut File {
    file.stage.as_mut().expect("live Java download").file_mut()
}
fn seal_download(file: &mut DownloadFile) -> Result<pcl_core::retained_fs::stage::Sealed> {
    let mut stage = file.stage.take().ok_or("Java 下载已经封存")?;
    let evidence_file = stage.file_mut().try_clone().map_err(|e| e.to_string())?;
    let sealed = stage.seal(file.max).map_err(|e| e.to_string())?;
    let stamp = retained_fs::metadata(&evidence_file).map_err(|e| e.to_string())?;
    drop(evidence_file);
    let record = SourceRecord {
        path: file.path.clone(),
        stamp,
        target: file.target.identity().map_err(|e| e.to_string())?,
        evidence: sealed.evidence().clone(),
    };
    let bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    write_record(sealed.parent(), &record_name(&file.path), &bytes)?;
    Ok(sealed)
}
impl Drop for DownloadFile {
    fn drop(&mut self) {
        if self.stage.is_some() {
            // Cancel/error retires futures before tree cleanup. Seal the actual
            // partial bytes while their original handle is still held, so the
            // cleanup preflight can distinguish these bytes from a replacement.
            // A failed seal/record is intentionally retained and cleanup errors.
            let _ = seal_download(self);
        }
    }
}
fn source_hash(file: &mut File, expected: &Evidence, expected_stamp: &Metadata) -> Result<()> {
    use sha2::{Digest, Sha256};
    let before = retained_fs::metadata(file).map_err(|e| e.to_string())?;
    before.require_private().map_err(|e| e.to_string())?;
    if before != *expected_stamp
        || before.identity != expected.identity
        || before.len != expected.len
        || before.links != 1
    {
        return Err("Java 命名暂存源身份或大小已改变，已保留文件".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .filter(|n| *n <= expected.len)
            .ok_or("Java 命名暂存源超过清单大小")?;
        hash.update(&buffer[..n]);
    }
    if total != expected.len
        || <[u8; 32]>::from(hash.finalize()) != expected.sha256
        || retained_fs::metadata(file).map_err(|e| e.to_string())? != before
    {
        return Err("Java 命名暂存源已改变，已保留文件".into());
    }
    Ok(())
}
impl Stage<'_> {
    fn check_sources(&self, remove_files: bool) -> Result<()> {
        use std::collections::BTreeSet;
        let root = native(&self.dir)?;
        let root_name = leaf(SOURCES)?;
        let source = match root.child(&root_name, Traversal::SameFilesystem) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        source
            .metadata()
            .and_then(|m| m.require_private())
            .map_err(|e| e.to_string())?;
        let source_identity = source.identity().map_err(|e| e.to_string())?;
        let names = source.names(16384).map_err(|e| e.to_string())?;
        let mut accounted = BTreeSet::new();
        let mut sources = Vec::new();
        let mut records = Vec::new();
        for name in &names {
            let text = name.to_str().ok_or("Java 命名暂存记录名不是 UTF-8")?;
            if !text.starts_with(".record-") {
                continue;
            }
            let name = leaf(text)?;
            let mut file = source
                .open_regular(&name, Links::Single)
                .map_err(|e| e.to_string())?;
            let before = retained_fs::metadata(&file).map_err(|e| e.to_string())?;
            before.require_private().map_err(|e| e.to_string())?;
            let mut raw = Vec::new();
            Read::by_ref(&mut file)
                .take(16385)
                .read_to_end(&mut raw)
                .map_err(|e| e.to_string())?;
            if raw.len() > 16384
                || retained_fs::metadata(&file).map_err(|e| e.to_string())? != before
            {
                return Err("Java 命名暂存记录已改变或过长".into());
            }
            let record: SourceRecord =
                serde_json::from_slice(&raw).map_err(|_| "Java 命名暂存记录损坏，已保留目录")?;
            let Some(Entry::File { downloads, .. }) = self.owner.manifest.files.get(&record.path)
            else {
                return Err("Java 命名暂存目标不在清单中".into());
            };
            let (parent, _) = descend(&self.dir, &record.path)?;
            if record_name(&record.path) != text
                || record.target != native(&parent)?.identity().map_err(|e| e.to_string())?
                || record.evidence.parent_identity != source_identity
                || record.evidence.len > downloads["raw"].size
                || record.stamp.identity != record.evidence.identity
                || record.stamp.len != record.evidence.len
            {
                return Err("Java 命名暂存记录身份不匹配".into());
            }
            accounted.insert(text.to_string());
            if let Some(source_name) = &record.evidence.source_name {
                let name = Leaf::new(source_name).map_err(|e| e.to_string())?;
                let text = source_name.to_str().ok_or("Java 命名暂存源不是 UTF-8")?;
                if !text.starts_with(".pcl-stage-")
                    || !text.ends_with(".lease")
                    || !accounted.insert(text.into())
                {
                    return Err("Java 命名暂存源重复或无效".into());
                }
                if let Some(visible) = source.entry(&name).map_err(|e| e.to_string())? {
                    let mut file = source
                        .open_regular(&name, Links::Single)
                        .map_err(|e| e.to_string())?;
                    let stamp = retained_fs::metadata(&file).map_err(|e| e.to_string())?;
                    if !visible.same_object(&stamp) || visible.reparse_tag.is_some() {
                        return Err("Java 命名暂存源已被替换".into());
                    }
                    source_hash(&mut file, &record.evidence, &record.stamp)?;
                    if retained_fs::metadata(&file).map_err(|e| e.to_string())? != stamp {
                        return Err("Java 命名暂存源已变化".into());
                    }
                    source
                        .recheck(&name, &stamp.identity)
                        .map_err(|e| e.to_string())?;
                    sources.push((name, stamp));
                }
            }
            records.push((name, before));
        }
        if names
            .iter()
            .any(|name| !name.to_str().is_some_and(|name| accounted.contains(name)))
        {
            return Err("Java 命名暂存目录有未封存或未知文件，已保留目录".into());
        }
        if remove_files {
            // Preflight covered every source and record before the first delete.
            // Delete sources first: an interrupted cleanup may leave a valid
            // record with an absent source, which is safe to reconcile again.
            for (name, stamp) in sources {
                remove(&source, &name, &stamp)?;
            }
            for (name, stamp) in records {
                remove(&source, &name, &stamp)?;
            }
            let stamp = source.metadata().map_err(|e| e.to_string())?;
            drop(source);
            remove(&root, &root_name, &stamp)?;
        }
        Ok(())
    }
}

#[cfg(test)]
impl Stage<'_> {
    pub(super) fn cleanup_before_remove_for_fixture(
        &self,
        mut before_remove: impl FnMut(&str),
    ) -> Result<()> {
        self.cleanup_before_remove(&mut before_remove)
    }
    pub(super) fn check_sources_for_fixture(&self) {
        self.check_sources(true).unwrap();
    }
}
