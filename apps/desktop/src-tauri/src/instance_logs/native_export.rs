//! Native named leases live in a newly created private workspace, never in the
//! user's source tree. SHA-256 sealing and no-replace publication come from the
//! retained capability backend. Uncertain publication retains its evidence;
//! neither a destination nor a source log is ever removed as rollback.
use pcl_core::retained_fs::{
    stage::{PublishOutcome, Stage},
    Directory, Leaf, Links, WorkspaceRemoval,
};
use std::{
    io::{Read, Write},
    path::Path,
};

pub(super) struct Saved {
    pub warning: Option<String>,
}

pub(super) fn publish<G>(
    target: &Path,
    bytes: &[u8],
    authorize: impl FnOnce() -> Result<G, String>,
) -> Result<Saved, String> {
    if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
        return Err("导出文件为空或超过 8 MiB 上限".into());
    }
    if target
        .extension()
        .and_then(|v| v.to_str())
        .is_none_or(|v| !v.eq_ignore_ascii_case("txt"))
    {
        return Err("保存文件应使用 .txt 扩展名".into());
    }
    let parent = target.parent().ok_or("保存目录无效")?;
    let name = target
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("保存文件名无效")?;
    crate::launcher_local::filesystem::component(name)?;
    let leaf = Leaf::new(name).map_err(|e| e.to_string())?;
    let directory = Directory::open_absolute(parent).map_err(|e| e.to_string())?;
    let identity = directory.identity().map_err(|e| e.to_string())?;
    if directory.entry(&leaf).map_err(|e| e.to_string())?.is_some() {
        return Err("保存位置已有文件，不会覆盖，请选择新的文件名".into());
    }
    let (workspace_name, workspace) = directory
        .create_private_workspace()
        .map_err(|e| e.to_string())?;
    directory
        .sync()
        .map_err(|e| format!("导出暂存目录同步失败，私有暂存已保留：{e}"))?;
    let workspace_stamp = workspace.metadata().map_err(|e| e.to_string())?;
    let residue = || {
        format!(
            "私有暂存目录 {} 已保留",
            parent.join(workspace_name.as_os_str()).display()
        )
    };
    let mut stage =
        Stage::new(&workspace).map_err(|e| format!("无法创建导出暂存：{e}；{}", residue()))?;
    stage
        .file_mut()
        .write_all(bytes)
        .map_err(|e| format!("无法写入导出暂存：{e}；{}", residue()))?;
    let sealed = stage
        .seal(bytes.len() as u64)
        .map_err(|e| format!("无法封存导出暂存：{e}；{}", residue()))?;
    let _guard = authorize().map_err(|e| format!("{e}；{}", residue()))?;
    if Directory::open_absolute(parent)
        .and_then(|d| d.identity())
        .map_err(|e| format!("无法复核保存目录：{e}；{}", residue()))?
        != identity
    {
        return Err(format!("保存目录已被替换，请重新选择；{}", residue()));
    }
    let result = sealed
        .publish_new(&directory, &leaf)
        .map_err(|e| format!("无法发布导出文件：{e}；{}", residue()))?;
    match result {
        PublishOutcome::Exists { .. } => Err(format!("保存位置已有文件，不会覆盖；{}", residue())),
        PublishOutcome::MayHavePublished { error, .. } => {
            // A confirmed durable publication and an unconfirmed namespace
            // attempt are distinct. Never retry, overwrite or delete either.
            Err(format!(
                "导出可能已发布，请检查目标文件后重新操作：{error}；{}",
                residue()
            ))
        }
        PublishOutcome::Published { receipt } => {
            let cleanup = directory.remove_workspace_entry(&workspace_name, &workspace_stamp);
            let warning = match cleanup {
                Ok(WorkspaceRemoval::Removed) => None,
                Ok(WorkspaceRemoval::MayHaveRemoved { error }) => {
                    Some(format!("文件已保存，但私有暂存目录清理未确认：{error}"))
                }
                Err(error) => Some(format!(
                    "文件已保存，但私有暂存目录清理失败：{error}；{}",
                    residue()
                )),
            };
            let same_file = (|| -> std::io::Result<bool> {
                let mut file = directory.open_regular(&leaf, Links::Single)?;
                let before = pcl_core::retained_fs::metadata(&file)?;
                if before.identity != receipt.source.identity || before.len != bytes.len() as u64 {
                    return Ok(false);
                }
                let mut actual = Vec::with_capacity(bytes.len());
                (&mut file)
                    .take(bytes.len() as u64 + 1)
                    .read_to_end(&mut actual)?;
                Ok(actual == bytes
                    && pcl_core::retained_fs::metadata(&file)? == before
                    && directory
                        .entry(&leaf)?
                        .is_some_and(|visible| visible == before))
            })()
            .unwrap_or(false);
            let same_parent = Directory::open_absolute(parent)
                .and_then(|d| d.identity())
                .is_ok_and(|current| current == identity);
            Ok(Saved {
                warning: if same_parent && same_file {
                    warning
                } else {
                    Some("文件已发布，但保存位置随后发生变化；已有内容已保留".into())
                },
            })
        }
    }
}
