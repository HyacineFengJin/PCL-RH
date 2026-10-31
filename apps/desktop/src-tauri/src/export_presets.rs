//! Launcher-owned export choices, separate from the files being exported.
use crate::instance_export::ExportRequest;
use crate::launcher_local::filesystem::{self as local, Scope, Snapshot};
use pcl_core::retained_fs::{Directory, Leaf, Lock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

const LIMIT: u64 = 64 * 1024;
struct PresetLock(#[allow(dead_code)] Lock);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preset {
    schema_version: u32,
    root_id: String,
    root_path: String,
    instance_id: String,
    request: ExportRequest,
}

pub(crate) fn name(root_id: &str, id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(root_id.len().to_le_bytes());
    hash.update(root_id);
    hash.update(id);
    format!("{:x}.json", hash.finalize())
}
fn directory(project: &Path, create: bool) -> Result<Option<Scope>, String> {
    let scope = if create {
        Scope::create(project, "export-presets")?
    } else {
        Scope::open(project, "export-presets")?
    };
    if scope.folder.is_some() {
        Ok(Some(scope))
    } else {
        Ok(None)
    }
}
fn read_at(scope: &Scope, filename: &str) -> Result<Option<(Snapshot, Preset)>, String> {
    scope.recheck()?;
    let dir = scope.folder.as_ref().ok_or("导出配置目录已消失")?;
    let Some(mut snapshot) = local::optional_snapshot(dir, filename, LIMIT)? else {
        scope.recheck()?;
        return Ok(None);
    };
    let bytes = snapshot.bytes(dir, filename)?;
    if format!("{:x}", Sha256::digest(&bytes)) != snapshot.digest {
        return Err("导出配置在读取期间已变化，原文件已保留".into());
    }
    scope.recheck()?;
    let preset =
        serde_json::from_slice(&bytes).map_err(|_| "导出配置损坏或格式不受支持，原文件已保留")?;
    Ok(Some((snapshot, preset)))
}
fn check(preset: &Preset, root_id: &str, root: &Path, id: &str) -> Result<(), String> {
    if preset.schema_version != 1
        || preset.root_id != root_id
        || preset.root_path != root.to_string_lossy()
        || preset.instance_id != id
    {
        return Err("导出配置版本或所属实例不匹配，原文件已保留".into());
    }
    preset.request.validate()
}
pub fn read(
    project: &Path,
    root_id: &str,
    root: &Path,
    id: &str,
) -> Result<Option<ExportRequest>, String> {
    let Some(dir) = directory(project, false)? else {
        return Ok(None);
    };
    let Some((_, preset)) = read_at(&dir, &name(root_id, id))? else {
        return Ok(None);
    };
    check(&preset, root_id, root, id)?;
    Ok(Some(preset.request))
}
fn lock_preset(scope: &Scope, root_id: &str, id: &str) -> Result<PresetLock, String> {
    scope.recheck()?;
    let dir = scope.folder.as_ref().ok_or("导出配置目录已消失")?;
    let native = Directory::from_file(dir.0.try_clone().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let lock = native
        .lock(&Leaf::new(format!("{}.lock", name(root_id, id))).map_err(|e| e.to_string())?)
        .map_err(|e| format!("导出配置正被另一个启动器使用或锁无效：{e}"))?;
    scope.recheck()?;
    Ok(PresetLock(lock))
}

pub fn save(
    project: &Path,
    root_id: &str,
    root: &Path,
    id: &str,
    request: ExportRequest,
) -> Result<(), String> {
    save_checked(project, root_id, root, id, request, || Ok(()))
}
fn save_checked(
    project: &Path,
    root_id: &str,
    root: &Path,
    id: &str,
    request: ExportRequest,
    before_publish: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    request.validate()?;
    let scope = directory(project, true)?.ok_or("无法创建配置目录")?;
    let filename = name(root_id, id);
    let _lock = lock_preset(&scope, root_id, id)?;
    crate::instance_rename_refs::ensure_project_ready(project)?;
    let current = read_at(&scope, &filename)?;
    if let Some((_, old)) = &current {
        check(old, root_id, root, id)?;
    }
    let data = serde_json::to_vec_pretty(&Preset {
        schema_version: 1,
        root_id: root_id.into(),
        root_path: root.to_string_lossy().into_owned(),
        instance_id: id.into(),
        request,
    })
    .map_err(|e| e.to_string())?;
    if data.len() as u64 > LIMIT {
        return Err("导出配置内容过多".into());
    }
    before_publish()?;
    scope.recheck()?;
    let dir = scope.folder.as_ref().ok_or("导出配置目录已消失")?;
    // The native local writer owns no-replace creation and Unix CAS exchange.
    // Windows permits genuine new files, and explicitly refuses an existing
    // dynamic document until its typed logical-document scope is implemented.
    dir.replace(
        &filename,
        current.as_ref().map(|(snapshot, _)| snapshot),
        &data,
        LIMIT,
    )?;
    scope.recheck()?;
    let Some((after, preset)) = read_at(&scope, &filename)? else {
        return Err("导出配置可能已保存，但发布后的文件已消失，请重新读取".into());
    };
    if after.digest != format!("{:x}", Sha256::digest(&data)) {
        return Err("导出配置可能已保存，但发布后被修改，原件与证据已保留".into());
    }
    check(&preset, root_id, root, id)
}

pub(crate) fn rename_bytes(
    bytes: &[u8],
    root_id: &str,
    root: &Path,
    old: &str,
    new: &str,
) -> Result<Vec<u8>, String> {
    let mut preset: Preset = serde_json::from_slice(bytes)
        .map_err(|_| "导出配置损坏或格式不受支持，原文件已保留".to_string())?;
    check(&preset, root_id, root, old)?;
    preset.instance_id = new.into();
    let result = serde_json::to_vec_pretty(&preset).map_err(|error| error.to_string())?;
    if result.len() as u64 > LIMIT {
        return Err("导出配置内容过多".into());
    }
    Ok(result)
}

#[cfg(test)]
#[path = "export_presets/tests.rs"]
mod tests;
