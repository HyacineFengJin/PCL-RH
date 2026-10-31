//! New output bytes only. Cleanup consumes the original sealed lease; it never
//! gains authority by reopening a temporary filename or the published target.
use super::ExportOutcome;
use pcl_core::retained_fs::{
    stage::{DiscardOutcome, PublishOutcome, Sealed, Stage},
    Directory, Leaf, Metadata, WorkspaceRemoval,
};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};

const LIMIT: u64 = 8 * 1024 * 1024;
fn native<T>(result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}
struct Output {
    parent: Directory,
    workspace: Directory,
    name: Leaf,
    original: Metadata,
    writable: Option<Stage>,
    sealed: Option<Sealed>,
    cleanup_enabled: bool,
}
impl Output {
    fn new(parent: Directory, parent_path: &Path) -> Result<Self, String> {
        Self::new_checked(parent, parent_path, Directory::metadata, |output| {
            native(output.parent.sync())?;
            output.writable = Some(native(Stage::new(&output.workspace))?);
            Ok(())
        })
    }
    fn new_checked(
        parent: Directory,
        parent_path: &Path,
        read_metadata: impl FnOnce(&Directory) -> std::io::Result<Metadata>,
        initialize: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<Self, String> {
        let (name, workspace) = native(parent.create_private_workspace())?;
        let location = parent_path.join(name.as_os_str());
        let original = read_metadata(&workspace).map_err(|error| format!(
            "导出暂存目录已创建但身份读取失败，未删除无法确认的条目；请检查原所选目录中的证据（{}）：{error}", location.display()
        ))?;
        let mut output = Self {
            parent,
            workspace,
            name,
            original,
            writable: None,
            sealed: None,
            cleanup_enabled: true,
        };
        if let Err(error) = initialize(&mut output) {
            return match output.cleanup() {
                Ok(()) => Err(format!("导出暂存初始化失败，已清理所创建的暂存：{error}")),
                Err(cleanup) => Err(format!(
                    "导出暂存初始化失败：{error}；清理未确认，请检查原所选目录中的证据（{}）：{cleanup}", location.display()
                )),
            };
        }
        Ok(output)
    }
    fn seal(&mut self) -> Result<(), String> {
        if let Some(stage) = self.writable.take() {
            match stage.seal(LIMIT) {
                Ok(sealed) => self.sealed = Some(sealed),
                Err(error) => {
                    self.writable = Some(error.stage);
                    return Err(format!("导出暂存封存失败：{}", error.error));
                }
            }
        }
        Ok(())
    }
    fn cleanup(&mut self) -> Result<(), String> {
        if !self.cleanup_enabled {
            return Ok(());
        }
        self.cleanup_enabled = false;
        // Partial failed writes are still our own newly allocated bytes. Seal
        // their actual content before requesting deletion of that same owner.
        self.seal()?;
        if let Some(sealed) = self.sealed.take() {
            match sealed.discard() {
                Ok(DiscardOutcome::Discarded { .. }) => {}
                Ok(DiscardOutcome::MayHaveDiscarded { stage, error, .. }) => {
                    self.sealed = stage;
                    return Err(format!("暂存清理结果未确认：{error}"));
                }
                Err(error) => {
                    self.sealed = Some(error.stage);
                    return Err(format!("暂存内容已变化，已保留：{}", error.error));
                }
            }
        }
        // This removes only the originally allocated empty workspace. Unknown
        // entries cause an error; no recursive or prefix-based removal exists.
        match native(
            self.parent
                .remove_workspace_entry(&self.name, &self.original),
        )? {
            WorkspaceRemoval::Removed => Ok(()),
            WorkspaceRemoval::MayHaveRemoved { error } => Err(error.to_string()),
        }
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

pub(super) fn publish_new<G>(
    path: &Path,
    bytes: &[u8],
    extension: &str,
    count: usize,
    authorize: impl FnOnce() -> Result<G, String>,
) -> Result<ExportOutcome, String> {
    if bytes.is_empty() || bytes.len() as u64 > LIMIT {
        return Err("导出文件为空或超过 8 MiB 上限".into());
    }
    if path
        .extension()
        .and_then(|v| v.to_str())
        .is_none_or(|v| !v.eq_ignore_ascii_case(extension))
    {
        return Err(format!("保存文件应使用 .{extension} 扩展名"));
    }
    let parent_path = path.parent().ok_or("保存目录无效")?;
    let name = native(Leaf::new(path.file_name().ok_or("保存文件名无效")?))?;
    let parent = native(Directory::open_absolute(parent_path))?;
    let identity = native(parent.identity())?;
    if native(parent.entry(&name))?.is_some() {
        return Err("保存位置已有文件，不会覆盖，请选择新的文件名".into());
    }
    let mut output = Output::new(parent, parent_path)?;
    let location = parent_path.join(output.name.as_os_str());
    let result = (|| {
        output
            .writable
            .as_mut()
            .ok_or("导出暂存已退役")?
            .file_mut()
            .write_all(bytes)
            .map_err(|e| e.to_string())?;
        output.seal()?;
        let expected: [u8; 32] = Sha256::digest(bytes).into();
        let sealed = output.sealed.as_ref().ok_or("导出暂存缺少封存证据")?;
        if sealed.evidence().len != bytes.len() as u64 || sealed.evidence().sha256 != expected {
            return Err("导出暂存长度或 SHA256 已变化".into());
        }
        let _guard = authorize()?;
        if native(native(Directory::open_absolute(parent_path))?.identity())? != identity {
            return Err("保存目录已被替换，请重新选择".into());
        }
        let sealed = output.sealed.take().ok_or("导出暂存已退役")?;
        match sealed.publish_new(&output.parent, &name) {
            Err(error) => {
                output.sealed = Some(error.stage);
                return Err(format!("导出发布前检查失败：{}", error.error));
            }
            Ok(PublishOutcome::Exists { stage }) => {
                output.sealed = Some(stage);
                return Err("保存位置已有文件，不会覆盖，请选择新的文件名".into());
            }
            Ok(PublishOutcome::MayHavePublished {
                stage,
                receipt,
                error,
            }) => {
                // The original handle may now refer to the chosen target.
                // Discard is forbidden after any uncertain publication.
                output.cleanup_enabled = false;
                output.sealed = Some(stage);
                let evidence = (|| -> Result<(), String> {
                    let mut file = native(
                        output
                            .workspace
                            .create_file(&native(Leaf::new("publication.json"))?),
                    )?;
                    let data = serde_json::to_vec(&receipt).map_err(|e| e.to_string())?;
                    file.write_all(&data)
                        .and_then(|_| file.sync_all())
                        .map_err(|e| e.to_string())?;
                    native(output.workspace.sync())
                })()
                .err()
                .map(|e| format!("；回执保存未确认：{e}"))
                .unwrap_or_default();
                let warning = format!(
                    "导出发布或持久性未确认，现存文件与证据已保留（{}）：{error}{evidence}",
                    location.display()
                );
                if !receipt.publication_confirmed {
                    return Err(format!("MayHavePublished：{warning}"));
                }
                return Ok(Some(warning));
            }
            Ok(PublishOutcome::Published { .. }) => {}
        }
        let mut warnings = Vec::new();
        if Directory::open_absolute(parent_path)
            .and_then(|d| d.identity())
            .ok()
            != Some(identity)
        {
            warnings
                .push("文件已发布到原保存目录，但该目录路径随后变化；现存内容已保留".to_string());
        }
        if let Err(error) = output.cleanup() {
            warnings.push(format!(
                "文件已保存，暂存清理未确认（{}）：{error}",
                location.display()
            ));
        }
        Ok((!warnings.is_empty()).then(|| warnings.join("；")))
    })();
    match result {
        Ok(warning) => Ok(ExportOutcome {
            status: "complete".into(),
            path: Some(path.display().to_string()),
            count,
            warning,
        }),
        Err(error) => match output.cleanup() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!(
                "{error}；暂存已保留（{}）：{cleanup}",
                location.display()
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn failed_workspace_observation_reports_its_actual_location_without_guessing_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let error = Output::new_checked(
            Directory::open_absolute(&root).unwrap(),
            &root,
            |_| Err(std::io::Error::other("fixture metadata failure")),
            |_| panic!("cannot initialize without identity"),
        )
        .err()
        .unwrap();
        let retained = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
        assert!(error.contains(&retained.display().to_string()));
        assert!(error.contains("fixture metadata failure"));
        assert!(retained.is_dir());
    }
    #[test]
    fn initialization_failure_discards_original_partial_owner_and_reports_unknown_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let error = Output::new_checked(
            Directory::open_absolute(&root).unwrap(),
            &root,
            Directory::metadata,
            |output| {
                let mut stage = native(Stage::new(&output.workspace))?;
                stage.file_mut().write_all(b"partial owned bytes").unwrap();
                output.writable = Some(stage);
                Err("fixture after stage".into())
            },
        )
        .err()
        .unwrap();
        assert!(error.contains("fixture after stage"));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let error = Output::new_checked(
            Directory::open_absolute(&root).unwrap(),
            &root,
            Directory::metadata,
            |output| {
                let mut unknown =
                    native(output.workspace.create_file(&native(Leaf::new("unknown"))?))?;
                unknown.write_all(b"preserve evidence").unwrap();
                Err("fixture initialization failed with unknown entry".into())
            },
        )
        .err()
        .unwrap();
        let retained = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
        assert!(error.contains(&retained.display().to_string()));
        assert_eq!(
            fs::read(retained.join("unknown")).unwrap(),
            b"preserve evidence"
        );
    }
}
