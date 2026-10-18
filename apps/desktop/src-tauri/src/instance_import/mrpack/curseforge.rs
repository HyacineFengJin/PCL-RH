//! Exact CurseForge manifest IDs become download authority only through the
//! configured official client. No URL from an archive enters this path.
use super::*;
use crate::community_curseforge::{CurseforgeClient, CurseforgePackDownload};
use std::{sync::Arc, time::Duration};

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct ResolvedFile {
    pub project_id: u32,
    pub file_id: u32,
    pub path: String,
    pub size: u64,
    pub sha1: String,
    pub url: String,
}

pub(super) fn blocker(count: usize) -> String {
    format!(
        "此包包含 {count} 个 CurseForge 文件；当前尚未连接获授权的官方文件元数据服务，无法完整安装"
    )
}

fn descriptor(ids: (u32, u32), file: CurseforgePackDownload) -> Result<ResolvedFile> {
    let path = format!("{}/{}", file.directory, file.download.name);
    manifest::output_path(&path)?;
    let extension = Path::new(&path)
        .extension()
        .and_then(|value| value.to_str());
    if !matches!(
        (file.directory, extension),
        ("mods", Some("jar")) | ("resourcepacks" | "shaderpacks", Some("zip"))
    ) {
        return Err("CurseForge 整合包文件扩展名与资源类型不匹配".into());
    }
    crate::community_curseforge::download_url(&file.download.url)?;
    Ok(ResolvedFile {
        project_id: ids.0,
        file_id: ids.1,
        path,
        size: file.download.size,
        sha1: file.download.sha1,
        url: file.download.url,
    })
}

async fn metadata(
    client: &CurseforgeClient,
    ids: &[(u32, u32)],
    cancel: &AtomicBool,
) -> Result<Vec<ResolvedFile>> {
    super::super::check(cancel)?;
    let future = client.pack_downloads(ids);
    tokio::pin!(future);
    let files = loop {
        super::super::check(cancel)?;
        if let Ok(result) = tokio::time::timeout(Duration::from_millis(50), &mut future).await {
            break result?;
        }
    };
    super::super::check(cancel)?;
    if files.len() != ids.len() {
        return Err("CurseForge 官方文件元数据未覆盖完整清单".into());
    }
    ids.iter()
        .copied()
        .zip(files)
        .map(|(ids, file)| descriptor(ids, file))
        .collect()
}

/// A missing/invalid service configuration remains a visible blocker. All
/// output changes happen together, after every declared file has resolved.
pub(super) fn configure(
    checked: &mut CheckedPack,
    factory: Arc<pcl_network::ClientFactory>,
    cancel: &AtomicBool,
) -> Result<Option<Arc<CurseforgeClient>>> {
    if checked.curseforge_files.is_empty() {
        return Ok(None);
    }
    let own_blocker = blocker(checked.curseforge_files.len());
    if checked
        .plan
        .preview
        .blockers
        .iter()
        .any(|value| value != &own_blocker)
    {
        return Ok(None);
    }
    let client = match CurseforgeClient::new(factory) {
        Ok(client) => Arc::new(client),
        Err(error) => {
            checked.plan.preview.blockers.push(error);
            return Ok(None);
        }
    };
    resolve(checked, &client, cancel)?;
    Ok(Some(client))
}

fn resolve(
    checked: &mut CheckedPack,
    client: &CurseforgeClient,
    cancel: &AtomicBool,
) -> Result<()> {
    let files =
        tauri::async_runtime::block_on(metadata(client, &checked.curseforge_files, cancel))?;
    apply(checked, files)
}

fn apply(checked: &mut CheckedPack, files: Vec<ResolvedFile>) -> Result<()> {
    if files
        .iter()
        .map(|file| (file.project_id, file.file_id))
        .collect::<Vec<_>>()
        != checked.curseforge_files
    {
        return Err("CurseForge 官方文件与完整清单 ID 不匹配".into());
    }
    let mut paths = checked.outputs.keys().cloned().collect::<BTreeSet<_>>();
    let mut bytes = checked.plan.bytes;
    let mut metadata_bytes = 0usize;
    for file in &files {
        if !paths.insert(file.path.clone()) {
            return Err("CurseForge 文件与其他文件或本地覆盖层路径冲突，无法完整安装".into());
        }
        bytes = bytes
            .checked_add(file.size)
            .filter(|bytes| *bytes <= MAX_OUTPUT)
            .ok_or("CurseForge 整合包有效输出超过 8 GiB 限制")?;
        metadata_bytes = metadata_bytes
            .checked_add(file.path.len() + file.url.len() + file.sha1.len() + 256)
            .filter(|bytes| *bytes <= 1024 * 1024)
            .ok_or("CurseForge 整合包文件元数据超过 1 MiB 限制")?;
    }
    if paths.len() > MAX_ENTRIES {
        return Err("CurseForge 整合包有效文件超过 10000 项限制".into());
    }
    archive::validate_outputs(&paths, &checked.directories)?;
    let revision = format!(
        "{}:cf:{:x}",
        checked.plan.revision,
        Sha256::digest(
            serde_json::to_vec(&(&files, &checked.plan.preview.dependencies))
                .map_err(super::super::error)?
        )
    );
    let download_bytes = files.iter().map(|file| file.size).sum::<u64>();
    checked.plan.revision = revision;
    checked.plan.bytes = bytes;
    checked.plan.file_count += files.len();
    checked.plan.preview.download_bytes += download_bytes;
    checked.plan.preview.required_files += files.len();
    checked
        .plan
        .preview
        .blockers
        .retain(|value| value != &blocker(files.len()));
    for file in files {
        checked.plan.preview.files.push(PreviewFile {
            path: file.path.clone(),
            size: file.size,
            client: Client::Required,
            selected: true,
            overridden: false,
        });
        checked
            .outputs
            .insert(file.path.clone(), Output::CurseForge(file));
    }
    Ok(())
}

/// Queue admission never grants stale download permission. Recheck the exact
/// class/path, IDs, official URL, SHA1 and size before building and publishing.
pub(super) fn recheck(
    checked: &CheckedPack,
    client: Option<&CurseforgeClient>,
    cancel: &AtomicBool,
) -> Result<()> {
    if checked.curseforge_files.is_empty() {
        return Ok(());
    }
    let client = client.ok_or("CurseForge 官方文件服务未绑定，请重新检查整合包")?;
    let files =
        tauri::async_runtime::block_on(metadata(client, &checked.curseforge_files, cancel))?;
    for file in files {
        if !matches!(checked.outputs.get(&file.path), Some(Output::CurseForge(expected)) if expected == &file)
        {
            return Err("CurseForge 官方文件或分发许可已变化，请重新检查整合包".into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "curseforge_tests.rs"]
mod tests;
