//! A selected directory stays a native capability. New bytes live in a private
//! same-filesystem workspace; only the sealed owned lease may be published.
//! No user original is replaced or enrolled in temporary-workspace cleanup.
use super::super::*;
use pcl_core::retained_fs::{
    self,
    stage::{PublishOutcome, Receipt, Sealed, Stage},
    Directory, Identity, Kind, Leaf, Metadata, WorkspaceRemoval,
};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Mutex,
};

fn native<T>(value: io::Result<T>) -> Result<T> {
    value.map_err(|e| e.to_string())
}
fn leaf(name: &str) -> Result<Leaf> {
    native(Leaf::new(name))
}
fn open(path: &Path) -> Result<Directory> {
    native(retained_fs::validate_absolute_path(path))?;
    native(Directory::open_absolute(path))
}
pub(crate) struct SaveTarget {
    pub path: PathBuf,
    pub file_name: String,
    parent_path: PathBuf,
    parent: Directory,
    parent_identity: Identity,
    project_path: PathBuf,
    project: Directory,
    project_identity: Identity,
}
impl SaveTarget {
    pub fn capture(project: &Path, target: &Path) -> Result<Self> {
        let file_name = target
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("保存文件名必须为有效文本")?
            .to_owned();
        super::file_name(&file_name)?;
        leaf(&file_name)?;
        let parent_path = target.parent().ok_or("保存位置缺少父目录")?.to_owned();
        let parent = open(&parent_path)?;
        let project_directory = open(project)?;
        let result = Self {
            path: target.to_owned(),
            file_name,
            parent_path,
            parent_identity: native(parent.identity())?,
            parent,
            project_path: project.to_owned(),
            project_identity: native(project_directory.identity())?,
            project: project_directory,
        };
        result.recheck()?;
        Ok(result)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn recheck_directory(&self) -> Result<()> {
        if native(self.parent.identity())? != self.parent_identity
            || native(open(&self.parent_path)?.identity())? != self.parent_identity
            || native(self.project.identity())? != self.project_identity
            || native(open(&self.project_path)?.identity())? != self.project_identity
        {
            return Err("应用目录或所选保存目录已被替换；文件未发布".into());
        }
        Ok(())
    }
    pub fn recheck(&self) -> Result<()> {
        self.recheck_directory()?;
        if native(self.parent.entry(&leaf(&self.file_name)?))?.is_some() {
            return Err("保存位置已存在文件、目录或链接；不会覆盖已有内容".into());
        }
        Ok(())
    }
    pub fn anonymous(&self) -> Result<Staging> {
        self.recheck()?;
        let (name, workspace) = native(self.parent.create_private_workspace())?;
        native(self.parent.sync())?;
        let mut cleanup = Cleanup {
            parent: self.parent.clone(),
            name,
            identity: native(workspace.identity())?,
            workspace: workspace.clone(),
            source: None,
            expected: None,
            enabled: true,
        };
        let mut stage = native(Stage::new(&workspace))?;
        cleanup.source = stage.source_name().cloned();
        cleanup.expected = Some(native(retained_fs::metadata(stage.file_mut()))?);
        Ok(Staging {
            stage: Some(stage),
            cleanup: Some(cleanup),
        })
    }
    pub(crate) fn publish(&self, file: &VerifiedAnonymous) -> Result<Option<String>> {
        self.publish_with_sync(file, || Ok(()))
    }
    #[cfg(test)]
    pub(crate) fn publish_with_failed_sync(
        &self,
        file: &VerifiedAnonymous,
    ) -> Result<Option<String>> {
        self.publish_with_sync(file, || Err("fixture sync failure".into()))
    }
    fn publish_with_sync(
        &self,
        file: &VerifiedAnonymous,
        sync: impl FnOnce() -> Result<()>,
    ) -> Result<Option<String>> {
        self.recheck()?;
        let stage = file
            .stage
            .lock()
            .map_err(|_| "保存暂存状态不可用")?
            .take()
            .ok_or("保存暂存发布已尝试；必须先复核现存文件")?;
        let outcome = stage.publish_new(&self.parent, &leaf(&self.file_name)?);
        let mut warnings = Vec::new();
        match outcome {
            Err(error) => {
                *file.stage.lock().map_err(|_| "保存暂存状态不可用")? = Some(error.stage);
                return Err(format!(
                    "保存暂存文件在最终复核后变化或发布前检查失败；文件未发布：{}",
                    error.error
                ));
            }
            Ok(PublishOutcome::Exists { stage }) => {
                *file.stage.lock().map_err(|_| "保存暂存状态不可用")? = Some(stage);
                return Err("保存位置已存在文件、目录或链接；不会覆盖已有内容".into());
            }
            Ok(PublishOutcome::Published { .. }) => {}
            Ok(PublishOutcome::MayHavePublished {
                stage,
                receipt,
                error,
            }) => {
                let mut cleanup = file.cleanup.lock().map_err(|_| "保存暂存状态不可用")?;
                cleanup.enabled = false;
                let evidence = retain_receipt(&cleanup.workspace, &receipt, &file.sha512)
                    .err()
                    .map(|e| format!("；发布回执落盘未确认：{e}"))
                    .unwrap_or_default();
                let location = self.parent_path.join(cleanup.name.as_os_str());
                *file.stage.lock().map_err(|_| "保存暂存状态不可用")? = Some(stage);
                let message = format!("保存发布或持久性未确认，现存文件与暂存证据已保留（{}），请检查后重新操作：{error}{evidence}", location.display());
                if receipt.publication_confirmed {
                    return Ok(Some(message));
                }
                return Err(format!("MayHavePublished：{message}"));
            }
        }
        if sync().is_err() {
            warnings.push("文件已保存，但保存目录同步失败；断电后的持久性未确认".to_owned());
        }
        if self.recheck_directory().is_err() {
            warnings.push(
                "文件已保存到所选目录的原始位置，但目录路径随后被外部替换；请检查原目录".to_owned(),
            );
        }
        if let Err(error) = file
            .cleanup
            .lock()
            .map_err(|_| "保存暂存状态不可用")?
            .finish()
        {
            warnings.push(format!(
                "文件已保存，私有暂存清理未确认，现存证据已保留：{error}"
            ));
        }
        Ok((!warnings.is_empty()).then(|| warnings.join("；")))
    }
}
fn retain_receipt(workspace: &Directory, receipt: &Receipt, sha512: &str) -> Result<()> {
    let bytes = serde_json::to_vec(&(1u32, sha512, receipt)).map_err(|e| e.to_string())?;
    let mut file = native(workspace.create_file(&leaf("publication.json")?))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    native(workspace.sync())
}

// Cleanup is confined to this exact new workspace and exact owned source. An
// unknown entry, altered sealed source, identity mismatch or uncertain deletion
// retains evidence. No ordinary target/user file can enter this authority.
struct Cleanup {
    parent: Directory,
    name: Leaf,
    workspace: Directory,
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
                        return Err("暂存内容或目录条目变化".into());
                    }
                    match native(self.workspace.remove_workspace_entry(source, &actual))? {
                        WorkspaceRemoval::Removed => {}
                        WorkspaceRemoval::MayHaveRemoved { error } => return Err(error.to_string()),
                    }
                }
                None if names.is_empty() => {}
                None => return Err("暂存目录存在未知条目".into()),
            }
        } else if !names.is_empty() {
            return Err("暂存目录存在未知条目".into());
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
}
pub(crate) fn staging_file(file: &mut Staging) -> &mut File {
    file.stage
        .as_mut()
        .expect("live owned staging file")
        .file_mut()
}
impl Staging {
    #[cfg(test)]
    pub(crate) fn try_clone(&mut self) -> io::Result<File> {
        staging_file(self).try_clone()
    }
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
                // Before sealing these are only our uncommitted generated
                // bytes. Release the original exclusive handle before the
                // native exact-entry cleanup opens its DELETE handle.
                cleanup.expected = retained_fs::metadata(stage.file_mut()).ok();
            }
        }
        drop(self.stage.take());
    }
}
pub(crate) struct VerifiedAnonymous {
    stage: Mutex<Option<Sealed>>,
    cleanup: Mutex<Cleanup>,
    pub(crate) sha512: String,
}
impl Drop for VerifiedAnonymous {
    fn drop(&mut self) {
        if let Ok(stage) = self.stage.get_mut() {
            drop(stage.take());
        }
        // Cleanup's Drop runs only after the strong native source handle ends.
    }
}
pub(crate) fn verify_anonymous(
    mut file: Staging,
    plan: &SavePlan,
    cancel: &AtomicBool,
) -> Result<VerifiedAnonymous> {
    let before = native(retained_fs::metadata(staging_file(&mut file)))?;
    before.require_private().map_err(|e| e.to_string())?;
    let expected_links = if file
        .stage
        .as_ref()
        .ok_or("保存暂存文件已退役")?
        .source_name()
        .is_some()
    {
        1
    } else {
        0
    };
    if before.kind != Kind::Regular
        || before.reparse_tag.is_some()
        || before.links != expected_links
        || before.len != plan.size
    {
        return Err("保存暂存文件的类型、大小或所有权无效".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let expected = super::super::authority::expected_digest(plan)?;
    let mut hash = expected.hasher();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or("保存暂存文件长度超限")?;
        if bytes > plan.size {
            return Err("保存暂存文件读取期间变化".into());
        }
        hash.update(&buffer[..count]);
    }
    if bytes != plan.size || native(retained_fs::metadata(staging_file(&mut file)))? != before {
        return Err("保存暂存文件大小或身份复核失败".into());
    }
    let sha512 = hash.verify(&expected)?;
    let stage = file.stage.take().ok_or("保存暂存文件已退役")?;
    let sealed = stage
        .seal(plan.size)
        .map_err(|e| format!("无法封存已校验的保存暂存文件：{e}"))?;
    let mut cleanup = file.cleanup.take().ok_or("保存暂存清理证据缺失")?;
    cleanup.expected = Some(before);
    Ok(VerifiedAnonymous {
        stage: Mutex::new(Some(sealed)),
        cleanup: Mutex::new(cleanup),
        sha512,
    })
}

// Run this portable provider as well as Linux's separate production provider.
// In Linux fixtures retained_fs supplies an anonymous stage inside the same
// owned workspace, so these tests exercise real cleanup/publication, not stubs.
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha512};
    fn plan(bytes: &[u8]) -> SavePlan {
        SavePlan {
            request: SaveRequest {
                source: SaveSource::Modrinth,
                project_id: "fixture".into(),
                version_id: "fixture".into(),
                file_name: "fixture.jar".into(),
            },
            project_title: "fixture".into(),
            version_name: "fixture".into(),
            file_name: "fixture.jar".into(),
            size: bytes.len() as u64,
            sha512: format!("{:x}", Sha512::digest(bytes)),
            sha1: None,
            revision: "fixture".into(),
        }
    }
    #[test]
    fn portable_provider_runs_real_publish_and_abandonment_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let target = SaveTarget::capture(&root, &root.join("saved.jar")).unwrap();
        let mut partial = target.anonymous().unwrap();
        partial.write_all(b"partial").unwrap();
        drop(partial);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let mut stage = target.anonymous().unwrap();
        stage.write_all(b"official bytes").unwrap();
        let file =
            verify_anonymous(stage, &plan(b"official bytes"), &AtomicBool::new(false)).unwrap();
        assert!(target.publish(&file).unwrap().is_none());
        drop(file);
        assert_eq!(
            std::fs::read(root.join("saved.jar")).unwrap(),
            b"official bytes"
        );
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
    }
    #[test]
    fn portable_provider_preserves_unknown_workspace_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let target = SaveTarget::capture(&root, &root.join("saved.jar")).unwrap();
        let mut stage = target.anonymous().unwrap();
        stage.write_all(b"partial").unwrap();
        let workspace = std::fs::read_dir(&root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(workspace.join("unknown-evidence"), b"outside actor").unwrap();
        drop(stage);
        assert!(!root.join("saved.jar").exists());
        assert_eq!(
            std::fs::read(workspace.join("unknown-evidence")).unwrap(),
            b"outside actor"
        );
    }
}
