//! Native chosen-directory download staging. A typed writer retains the real
//! Stage and private workspace until verification/publication or abandonment.
//! User originals are never enrolled in cleanup or overwritten.
use super::super::*;
use pcl_core::retained_fs::{
    self,
    stage::{PublishOutcome, Receipt, Sealed, Stage},
    Directory as NativeDirectory, Identity, Kind, Leaf, Metadata, WorkspaceRemoval,
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::Mutex,
};

fn native<T>(result: io::Result<T>) -> Result<T> {
    result.map_err(|error| error.to_string())
}
fn leaf(name: &str) -> Result<Leaf> {
    super::validate_name(name)?;
    native(Leaf::new(name))
}
fn open(path: &Path) -> Result<NativeDirectory> {
    native(retained_fs::validate_absolute_path(path))?;
    native(NativeDirectory::open_absolute(path))
}
pub(crate) struct Directory {
    held: NativeDirectory,
    path: PathBuf,
    identity: Identity,
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        let held = open(path)?;
        let identity = native(held.identity())?;
        let directory = Self {
            held,
            path: path.into(),
            identity,
        };
        directory.recheck()?;
        Ok(directory)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn recheck(&self) -> Result<()> {
        if native(self.held.identity())? != self.identity
            || native(open(&self.path)?.identity())? != self.identity
        {
            return Err("所选下载目录已被替换或移除，请重新选择".into());
        }
        Ok(())
    }
    pub fn absent(&self, name: &str) -> Result<()> {
        if native(self.held.entry(&leaf(name)?))?.is_some() {
            return Err("保存位置已存在文件、目录或链接；不会覆盖已有内容".into());
        }
        Ok(())
    }
    pub fn anonymous(&self) -> Result<Staging> {
        self.recheck()?;
        let (name, workspace) = native(self.held.create_private_workspace())?;
        let mut cleanup = Cleanup {
            parent: self.held.clone(),
            name,
            identity: native(workspace.identity())?,
            workspace: workspace.clone(),
            source: None,
            expected: None,
            enabled: true,
        };
        native(self.held.sync())?;
        let mut stage = native(Stage::new(&workspace))?;
        cleanup.source = stage.source_name().cloned();
        cleanup.expected = Some(native(retained_fs::metadata(stage.file_mut()))?);
        Ok(Staging {
            stage: Some(stage),
            cleanup: Some(cleanup),
            target_parent: self.identity.clone(),
            #[cfg(test)]
            before_seal: None,
        })
    }
    pub fn publish(&self, name: &str, file: &VerifiedAnonymous) -> Result<Option<String>> {
        self.publish_with_sync(name, file, || Ok(()))
    }
    #[cfg(test)]
    pub(crate) fn publish_with_failed_sync(
        &self,
        name: &str,
        file: &VerifiedAnonymous,
    ) -> Result<Option<String>> {
        self.publish_with_sync(name, file, || Err("fixture sync failure".into()))
    }
    fn publish_with_sync(
        &self,
        name: &str,
        file: &VerifiedAnonymous,
        sync: impl FnOnce() -> Result<()>,
    ) -> Result<Option<String>> {
        self.recheck()?;
        self.absent(name)?;
        if file.target_parent != self.identity {
            return Err("下载暂存不属于所选保存目录，文件未发布".into());
        }
        let stage = file
            .stage
            .lock()
            .map_err(|_| "下载暂存状态不可用")?
            .take()
            .ok_or("下载暂存发布已尝试；必须先复核现存文件")?;
        match stage.publish_new(&self.held, &leaf(name)?) {
            Err(error) => {
                *file.stage.lock().map_err(|_| "下载暂存状态不可用")? = Some(error.stage);
                return Err(format!(
                    "保存暂存文件在最终复核后变化或发布前检查失败；文件未发布：{}",
                    error.error
                ));
            }
            Ok(PublishOutcome::Exists { stage }) => {
                *file.stage.lock().map_err(|_| "下载暂存状态不可用")? = Some(stage);
                return Err("保存位置已存在文件、目录或链接；不会覆盖已有内容".into());
            }
            Ok(PublishOutcome::MayHavePublished {
                stage,
                receipt,
                error,
            }) => {
                let mut cleanup = file.cleanup.lock().map_err(|_| "下载暂存状态不可用")?;
                cleanup.enabled = false;
                let evidence = retain_receipt(&cleanup.workspace, &receipt, &file.sha256)
                    .err()
                    .map(|e| format!("；发布回执落盘未确认：{e}"))
                    .unwrap_or_default();
                let location = self.path.join(cleanup.name.as_os_str());
                *file.stage.lock().map_err(|_| "下载暂存状态不可用")? = Some(stage);
                let message = format!("保存发布或持久性未确认，现存文件与暂存证据已保留（{}），请检查后重新操作：{error}{evidence}", location.display());
                if receipt.publication_confirmed {
                    return Ok(Some(message));
                }
                return Err(format!("MayHavePublished：{message}"));
            }
            Ok(PublishOutcome::Published { .. }) => {}
        }
        let mut warnings = Vec::new();
        if sync().is_err() {
            warnings.push("文件已保存，但保存目录同步失败；断电后的持久性未确认".to_owned());
        }
        if self.recheck().is_err() {
            warnings.push(
                "文件已保存到所选目录的原始位置，但目录路径随后被外部替换；请检查原目录".to_owned(),
            );
        }
        if let Err(error) = file
            .cleanup
            .lock()
            .map_err(|_| "下载暂存状态不可用")?
            .finish()
        {
            warnings.push(format!(
                "文件已保存，私有暂存清理未确认，现存证据已保留：{error}"
            ));
        }
        Ok((!warnings.is_empty()).then(|| warnings.join("；")))
    }
}
fn retain_receipt(workspace: &NativeDirectory, receipt: &Receipt, sha256: &str) -> Result<()> {
    let bytes = serde_json::to_vec(&(1u32, "http-actual-bytes-sha256", sha256, receipt))
        .map_err(|error| error.to_string())?;
    let mut file = native(workspace.create_file(&native(Leaf::new("publication.json"))?))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())?;
    native(workspace.sync())
}
struct Cleanup {
    parent: NativeDirectory,
    name: Leaf,
    workspace: NativeDirectory,
    identity: Identity,
    source: Option<Leaf>,
    expected: Option<Metadata>,
    enabled: bool,
}
impl Cleanup {
    fn finish(&mut self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.enabled = false;
        if native(self.workspace.identity())? != self.identity {
            return Err("暂存目录身份变化".into());
        }
        native(self.parent.recheck(&self.name, &self.identity))?;
        let names = native(self.workspace.names(2))?;
        if let Some(source) = &self.source {
            match native(self.workspace.entry(source))? {
                Some(visible) => {
                    let file = native(
                        self.workspace
                            .open_regular(source, retained_fs::Links::Single),
                    )?;
                    let actual = native(retained_fs::metadata(&file))?;
                    if names.len() != 1
                        || names[0].as_os_str() != source.as_os_str()
                        || !visible.same_object(&actual)
                        || visible.reparse_tag.is_some()
                        || self.expected.as_ref() != Some(&actual)
                    {
                        return Err("暂存内容或目录条目变化，现存证据已保留".into());
                    }
                    match native(self.workspace.remove_workspace_entry(source, &actual))? {
                        WorkspaceRemoval::Removed => {}
                        WorkspaceRemoval::MayHaveRemoved { error } => return Err(error.to_string()),
                    }
                }
                None if names.is_empty() => {}
                None => return Err("暂存目录存在未知条目，已保留".into()),
            }
        } else if !names.is_empty() {
            return Err("暂存目录存在未知条目，已保留".into());
        }
        let metadata = native(self.workspace.metadata())?;
        match native(self.parent.remove_workspace_entry(&self.name, &metadata))? {
            WorkspaceRemoval::Removed => Ok(()),
            WorkspaceRemoval::MayHaveRemoved { error } => Err(error.to_string()),
        }
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

pub(crate) struct Staging {
    stage: Option<Stage>,
    cleanup: Option<Cleanup>,
    target_parent: Identity,
    #[cfg(test)]
    before_seal: Option<Box<dyn FnOnce(&mut File) + Send + Sync>>,
}
pub(crate) fn staging_file(file: &mut Staging) -> &mut File {
    file.stage
        .as_mut()
        .expect("live owned download staging")
        .file_mut()
}
impl Write for Staging {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        staging_file(self).write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        staging_file(self).flush()
    }
}
impl Read for Staging {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        staging_file(self).read(bytes)
    }
}
impl Seek for Staging {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        staging_file(self).seek(from)
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(stage) = &mut self.stage {
            if let Some(cleanup) = &mut self.cleanup {
                cleanup.expected = retained_fs::metadata(stage.file_mut()).ok();
            }
        }
        drop(self.stage.take());
    }
}
pub(crate) struct VerifiedAnonymous {
    stage: Mutex<Option<Sealed>>,
    cleanup: Mutex<Cleanup>,
    target_parent: Identity,
    sha256: String,
}
impl Drop for VerifiedAnonymous {
    fn drop(&mut self) {
        if let Ok(stage) = self.stage.get_mut() {
            drop(stage.take());
        }
    }
}
pub(crate) fn verify_anonymous(
    mut file: Staging,
    size: u64,
    sha256: &str,
    cancel: &AtomicBool,
) -> Result<VerifiedAnonymous> {
    let before = native(retained_fs::metadata(staging_file(&mut file)))?;
    before
        .require_private()
        .map_err(|error| error.to_string())?;
    let expected_links = if file
        .stage
        .as_ref()
        .ok_or("下载暂存已退役")?
        .source_name()
        .is_some()
    {
        1
    } else {
        0
    };
    if size > MAX_FILE_BYTES
        || before.kind != Kind::Regular
        || before.reparse_tag.is_some()
        || before.links != expected_links
        || before.len != size
    {
        return Err("保存暂存文件的类型、大小或所有权无效".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut buffer).map_err(|_| "无法复核保存暂存文件")?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or("保存暂存文件长度超限")?;
        if bytes > size {
            return Err("保存暂存文件读取期间变化".into());
        }
        hash.update(&buffer[..count]);
    }
    if bytes != size
        || format!("{:x}", hash.finalize()) != sha256
        || native(retained_fs::metadata(staging_file(&mut file)))? != before
    {
        return Err("保存暂存文件大小或SHA256复核失败".into());
    }
    #[cfg(test)]
    if let Some(callback) = file.before_seal.take() {
        callback(staging_file(&mut file));
    }
    let stage = file.stage.take().ok_or("下载暂存已退役")?;
    let sealed = match stage.seal(size) {
        Ok(sealed) => sealed,
        Err(error) => {
            file.stage = Some(error.stage);
            return Err(format!("无法封存已校验的下载暂存文件：{}", error.error));
        }
    };
    // Seal can perform a real flush and its own full metadata/hash pass. Its
    // actual held-FD stamp, rather than the earlier readback stamp, authorizes
    // eventual cleanup, including a post-seal digest mismatch below.
    file.cleanup
        .as_mut()
        .ok_or("下载暂存清理证据缺失")?
        .expected = Some(sealed.metadata().clone());
    let actual: String = sealed
        .evidence()
        .sha256
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if sealed.evidence().len != size || actual != sha256 {
        return Err("封存后的下载文件SHA256已变化".into());
    }
    let cleanup = file.cleanup.take().ok_or("下载暂存清理证据缺失")?;
    Ok(VerifiedAnonymous {
        stage: Mutex::new(Some(sealed)),
        cleanup: Mutex::new(cleanup),
        target_parent: file.target_parent.clone(),
        sha256: sha256.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_typed_stage_cleans_partial_and_preserves_unknown_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let directory = Directory::open(&root).unwrap();
        let mut stage = directory.anonymous().unwrap();
        stage.write_all(b"partial").unwrap();
        drop(stage);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let mut stage = directory.anonymous().unwrap();
        stage.write_all(b"partial").unwrap();
        let workspace = std::fs::read_dir(&root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(workspace.join("unknown-evidence"), b"kept").unwrap();
        drop(stage);
        assert_eq!(
            std::fs::read(workspace.join("unknown-evidence")).unwrap(),
            b"kept"
        );
    }
    #[test]
    fn failed_real_seal_and_cross_seal_digest_changes_cleanup_the_owned_source() {
        for grow in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let directory = Directory::open(&root).unwrap();
            let mut stage = directory.anonymous().unwrap();
            stage.write_all(b"good").unwrap();
            stage.before_seal = Some(Box::new(move |file| {
                if grow {
                    file.set_len(5).unwrap();
                } else {
                    file.seek(SeekFrom::Start(0)).unwrap();
                    file.write_all(b"bad!").unwrap();
                }
            }));
            let error = verify_anonymous(
                stage,
                4,
                &format!("{:x}", Sha256::digest(b"good")),
                &AtomicBool::new(false),
            )
            .err()
            .unwrap();
            assert!(
                if grow {
                    error.contains("无法封存")
                } else {
                    error.contains("封存后的")
                },
                "{error}"
            );
            assert_eq!(
                std::fs::read_dir(&root).unwrap().count(),
                0,
                "failed native seal left its owned source/workspace behind: {error}"
            );
        }
    }
}
