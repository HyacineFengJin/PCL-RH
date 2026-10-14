//! Pure required-file enumeration for a captured installed instance. Callers
//! supply bounded metadata reads and existence observations; this module grants
//! no filesystem or network authority. Repair and launch share inheritance
//! merging, jar selection, Maven paths and platform rules.
use crate::{identifier, inherited_jar, maven_path, merge, rules::RuleContext, text, Result};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path},
};

pub const MAX_REQUIRED_FILES: usize = 100_000;
pub const MAX_REPAIR_FILE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_REQUIRED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const MAX_ASSET_INDEX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDownload {
    pub url: String,
    pub sha1: String,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct RequiredFile {
    /// Only validated relative paths are present. The diagnostic label must
    /// never be used as a destination when path validation failed.
    pub relative: Option<String>,
    pub label: String,
    pub download: Option<FileDownload>,
    pub unsupported: Option<String>,
}

pub struct RepairMetadata {
    merged: Value,
    client: Value,
    jar_id: String,
}

pub struct RequiredFiles {
    pub files: Vec<RequiredFile>,
    pub asset_index: Option<RequiredFile>,
}

/// Resolve exactly the jar launch would select, then use that jar owner's
/// download declaration. An inherited child's descriptor cannot authorize
/// writing different bytes into its parent's jar.
pub fn resolve_metadata(
    id: &str,
    mut read: impl FnMut(&str) -> Result<Value>,
    mut jar_present: impl FnMut(&str) -> Result<bool>,
) -> Result<RepairMetadata> {
    fn visit(
        id: &str,
        read: &mut impl FnMut(&str) -> Result<Value>,
        present: &mut impl FnMut(&str) -> Result<bool>,
        visiting: &mut HashSet<String>,
        originals: &mut BTreeMap<String, Value>,
    ) -> Result<Value> {
        identifier(id)?;
        if visiting.len() >= 64 || !visiting.insert(id.into()) {
            return Err(format!("Version inheritance cycle or depth limit at {id}"));
        }
        let mut data = read(id)?;
        if !data.is_object() {
            return Err(format!("Version metadata is not an object: {id}"));
        }
        originals.insert(id.into(), data.clone());
        if let Some(parent) = data["inheritsFrom"].as_str().map(str::to_owned) {
            let base = visit(&parent, read, present, visiting, originals)?;
            if data.get("jar").is_none() {
                data["jar"] = inherited_jar(id, &parent, &base, present(id)?);
            }
            data = merge(base, data);
        }
        visiting.remove(id);
        Ok(data)
    }
    let mut originals = BTreeMap::new();
    let merged = visit(
        id,
        &mut read,
        &mut jar_present,
        &mut HashSet::new(),
        &mut originals,
    )?;
    let jar_id = merged["jar"].as_str().unwrap_or(id).to_owned();
    identifier(&jar_id)?;
    let owner = match originals.remove(&jar_id) {
        Some(value) => value,
        None => read(&jar_id)?,
    };
    if !owner.is_object() {
        return Err("核心 JAR 所属版本元数据不是对象".into());
    }
    Ok(RepairMetadata {
        merged,
        client: owner["downloads"]["client"].clone(),
        jar_id,
    })
}

/// Strict lexical validation precedes any destination lookup or creation.
pub fn relative_path(raw: &str) -> Result<String> {
    if raw.is_empty() || raw.len() > 4096 || raw.contains('\\') || raw.chars().any(char::is_control)
    {
        return Err("所需文件路径无效或过长".into());
    }
    let mut names = Vec::new();
    for part in Path::new(raw).components() {
        match part {
            Component::Normal(name) => {
                let name = name.to_str().ok_or("所需文件路径不是 UTF-8")?;
                identifier(name)?;
                names.push(name);
            }
            _ => return Err("所需文件路径含绝对路径或目录跳转".into()),
        }
    }
    if names.is_empty() || names.len() > 64 {
        return Err("所需文件路径层级无效".into());
    }
    Ok(names.join("/"))
}

fn descriptor(value: &Value, limit: u64) -> Result<FileDownload> {
    let url = value["url"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("缺少明确下载地址")?;
    if url.len() > 8192 || url.chars().any(char::is_control) {
        return Err("下载地址无效或过长".into());
    }
    let sha1 = value["sha1"]
        .as_str()
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("缺少有效 SHA1 校验值")?;
    let size = value["size"].as_u64().ok_or("缺少明确文件大小")?;
    if size > limit {
        return Err("文件大小超过缺失补全上限".into());
    }
    Ok(FileDownload {
        url: url.into(),
        sha1: sha1.to_ascii_lowercase(),
        size,
    })
}

fn required(raw: String, value: &Value, limit: u64) -> RequiredFile {
    match relative_path(&raw) {
        Err(reason) => RequiredFile {
            relative: None,
            label: raw,
            download: None,
            unsupported: Some(reason),
        },
        Ok(relative) => match descriptor(value, limit) {
            Ok(download) => RequiredFile {
                label: relative.clone(),
                relative: Some(relative),
                download: Some(download),
                unsupported: None,
            },
            Err(reason) => RequiredFile {
                label: relative.clone(),
                relative: Some(relative),
                download: None,
                unsupported: Some(reason),
            },
        },
    }
}

fn library(artifact: &Value, name: &str) -> RequiredFile {
    let raw = artifact["path"]
        .as_str()
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(|| maven_path(name));
    match raw {
        Ok(raw) => {
            // Validate before adding the libraries prefix; an absolute path
            // must not become a disguised libraries//absolute destination.
            match relative_path(&raw) {
                Ok(path) => required(format!("libraries/{path}"), artifact, MAX_REPAIR_FILE_BYTES),
                Err(reason) => RequiredFile {
                    relative: None,
                    label: format!("libraries/{raw}"),
                    download: None,
                    unsupported: Some(reason),
                },
            }
        }
        Err(reason) => RequiredFile {
            relative: None,
            label: format!("library:{name}"),
            download: None,
            unsupported: Some(reason),
        },
    }
}

/// Select current-platform libraries, the inherited core jar, logging config
/// and asset index. Existing indexes are supplied separately to objects().
pub fn required_files(metadata: &RepairMetadata) -> Result<RequiredFiles> {
    required_for(metadata, &RuleContext::current()?)
}
fn required_for(metadata: &RepairMetadata, context: &RuleContext) -> Result<RequiredFiles> {
    let data = &metadata.merged;
    let mut files = Vec::new();
    for lib in data["libraries"].as_array().into_iter().flatten() {
        if !context.library_allowed(lib)? {
            continue;
        }
        let name = text(lib, "name");
        let artifact = &lib["downloads"]["artifact"];
        if !artifact.is_null() || lib.get("downloads").is_none() {
            files.push(library(artifact, name));
        }
        if let Some(classifier) = context.platform.legacy_native_classifier(lib)? {
            files.push(library(
                &lib["downloads"]["classifiers"][&classifier],
                &format!("{name}:{classifier}"),
            ));
        }
        if files.len() > MAX_REQUIRED_FILES {
            return Err("所需文件数量超过缺失补全上限".into());
        }
    }
    files.push(required(
        format!("versions/{0}/{0}.jar", metadata.jar_id),
        &metadata.client,
        MAX_REPAIR_FILE_BYTES,
    ));
    let asset_id = data["assetIndex"]["id"]
        .as_str()
        .or_else(|| data["assets"].as_str())
        .unwrap_or("");
    let asset_index = if asset_id.is_empty() {
        None
    } else {
        identifier(asset_id)?;
        Some(required(
            format!("assets/indexes/{asset_id}.json"),
            &data["assetIndex"],
            MAX_ASSET_INDEX_BYTES,
        ))
    };
    if let Some(log_id) = data["logging"]["client"]["file"]["id"].as_str() {
        identifier(log_id)?;
        files.push(required(
            format!("assets/log_configs/{log_id}"),
            &data["logging"]["client"]["file"],
            MAX_REPAIR_FILE_BYTES,
        ));
    }
    check_budget(&files)?;
    Ok(RequiredFiles { files, asset_index })
}

/// Asset logical names never become paths. Only bounded SHA1 hashes form
/// canonical object destinations and official resource URLs.
pub fn asset_objects(index: &Value) -> Result<Vec<RequiredFile>> {
    let objects = index["objects"]
        .as_object()
        .ok_or("资源索引缺少 objects 对象")?;
    if objects.len() > MAX_REQUIRED_FILES {
        return Err("资源对象数量超过缺失补全上限".into());
    }
    let mut files = Vec::with_capacity(objects.len());
    for (name, object) in objects {
        let Some(hash) = object["hash"]
            .as_str()
            .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            files.push(RequiredFile {
                relative: None,
                label: format!("asset:{name}"),
                download: None,
                unsupported: Some("资源对象缺少有效 SHA1 校验值".into()),
            });
            continue;
        };
        let hash = hash.to_ascii_lowercase();
        let value = serde_json::json!({"url": format!("https://resources.download.minecraft.net/{}/{hash}", &hash[..2]), "sha1": hash, "size": object.get("size").cloned().unwrap_or(Value::Null)});
        files.push(required(
            format!("assets/objects/{}/{}", &hash[..2], hash),
            &value,
            MAX_REPAIR_FILE_BYTES,
        ));
    }
    check_budget(&files)?;
    Ok(files)
}

/// Deduplicate declarations; conflicting checksums never pick an arbitrary
/// winner. Missing conflicting files remain explicit unsupported entries.
pub fn deduplicate(files: Vec<RequiredFile>) -> Result<Vec<RequiredFile>> {
    let mut result: Vec<RequiredFile> = Vec::new();
    let mut paths = BTreeMap::<String, usize>::new();
    for file in files {
        if let Some(path) = &file.relative {
            if let Some(index) = paths.get(path) {
                let old = &mut result[*index];
                if old.download != file.download {
                    old.download = None;
                    old.unsupported = Some("同一路径存在相互冲突的下载声明".into());
                }
                continue;
            }
            paths.insert(path.clone(), result.len());
        }
        result.push(file);
    }
    check_budget(&result)?;
    Ok(result)
}
fn check_budget(files: &[RequiredFile]) -> Result<()> {
    if files.len() > MAX_REQUIRED_FILES {
        return Err("所需文件数量超过缺失补全上限".into());
    }
    let mut total = 0u64;
    for file in files {
        if let Some(download) = &file.download {
            total = total
                .checked_add(download.size)
                .ok_or("所需文件总大小溢出")?;
            if total > MAX_REQUIRED_BYTES {
                return Err("所需文件总大小超过缺失补全上限".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;
