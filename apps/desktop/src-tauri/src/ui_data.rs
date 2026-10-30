use crate::Shared;
use base64::Engine;
use serde::Serialize;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tauri::State;

#[derive(Serialize)]
pub struct SystemInfo {
    pub(crate) total_memory_bytes: u64,
    pub(crate) available_memory_bytes: u64,
}

#[tauri::command]
pub fn system_info() -> Result<SystemInfo, String> {
    let memory = pcl_core::system::memory()?;
    Ok(SystemInfo {
        total_memory_bytes: memory.total_bytes,
        available_memory_bytes: memory.available_bytes,
    })
}

#[derive(Serialize)]
pub struct ResourceInfo {
    name: String,
    file_name: String,
    path: String,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
}

fn capped_text(value: &str) -> String {
    let mut end = value.len().min(8192);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].trim().to_owned()
}

fn jar_entry<R: Read + std::io::Seek>(
    jar: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Option<Vec<u8>> {
    // Metadata names are archive members only; nothing is extracted onto disk.
    if name.starts_with('/') || name.split('/').any(|part| part == "..") || name.contains('\\') {
        return None;
    }
    let entry = jar.by_name(name).ok()?;
    if entry.size() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    entry.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataPurpose {
    Display,
    Export,
}
impl MetadataPurpose {
    fn text(self, value: &str) -> String {
        match self {
            Self::Display => capped_text(value),
            Self::Export => value.to_owned(),
        }
    }
}
fn mod_metadata<R: Read + std::io::Seek>(
    jar: &mut zip::ZipArchive<R>,
    row: &mut ResourceInfo,
    purpose: MetadataPurpose,
) -> bool {
    let mut logo = None;
    let mut available = false;
    if let Some(json) = jar_entry(jar, "fabric.mod.json", 262144)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        available = ["name", "id", "version", "description"]
            .iter()
            .any(|key| json[*key].is_string());
        row.name = json["name"]
            .as_str()
            .or_else(|| json["id"].as_str())
            .map(|value| purpose.text(value))
            .unwrap_or_else(|| row.name.clone());
        row.version = json["version"].as_str().map(|value| purpose.text(value));
        row.description = json["description"]
            .as_str()
            .map(|value| purpose.text(value));
        logo = json["icon"].as_str().map(str::to_owned).or_else(|| {
            json["icon"]
                .as_object()?
                .iter()
                .filter_map(|(size, path)| Some((size.parse::<u32>().ok()?, path.as_str()?)))
                .min_by_key(|(size, _)| size.abs_diff(64))
                .map(|(_, path)| path.to_owned())
        });
    } else {
        for filename in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
            let Some(data) = jar_entry(jar, filename, 262144)
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|text| text.parse::<toml::Value>().ok())
            else {
                continue;
            };
            let Some(info) = data
                .get("mods")
                .and_then(toml::Value::as_array)
                .and_then(|mods| mods.first())
            else {
                continue;
            };
            available = ["displayName", "modId", "version", "description"]
                .iter()
                .any(|key| info.get(*key).and_then(toml::Value::as_str).is_some());
            row.name = info
                .get("displayName")
                .or_else(|| info.get("modId"))
                .and_then(toml::Value::as_str)
                .map(|value| purpose.text(value))
                .unwrap_or_else(|| row.name.clone());
            row.version = info
                .get("version")
                .and_then(toml::Value::as_str)
                .map(|value| purpose.text(value));
            row.description = info
                .get("description")
                .and_then(toml::Value::as_str)
                .map(|value| purpose.text(value));
            logo = info
                .get("logoFile")
                .or_else(|| data.get("logoFile"))
                .and_then(toml::Value::as_str)
                .map(str::to_owned);
            break;
        }
    }
    if purpose == MetadataPurpose::Display
        && row
            .version
            .as_deref()
            .is_some_and(|version| version.contains("${file.jarVersion}"))
    {
        let manifest = jar_entry(jar, "META-INF/MANIFEST.MF", 65536)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|text| text.replace("\r\n ", "").replace("\n ", ""));
        let version = manifest.as_deref().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("Implementation-Version: "))
        });
        row.version = version.map(|version| {
            capped_text(
                &row.version
                    .as_deref()
                    .unwrap()
                    .replace("${file.jarVersion}", version),
            )
        });
    }
    if purpose == MetadataPurpose::Display
        && row.version.as_deref().is_some_and(|v| v.contains("${"))
    {
        row.version = None;
    }
    if purpose == MetadataPurpose::Export {
        return available;
    }
    if let Some(bytes) = logo.and_then(|name| jar_entry(jar, &name, 128 * 1024)) {
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("image/png")
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some("image/jpeg")
        } else {
            None
        };
        if let Some(mime) = mime {
            row.icon = Some(format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ));
        }
    }
    available
}

/// The export service supplies a verified, retained descriptor after its ZIP
/// central-directory preflight. This adapter never reopens a UI path, extracts
/// an archive member or includes an icon. Metadata inflation is separately
/// capped by `jar_entry`, including malformed advertised uncompressed lengths.
pub(crate) fn resource_metadata_from_file(
    file: fs::File,
    file_name: &str,
    kind: &str,
) -> Result<serde_json::Value, String> {
    if !matches!(kind, "mods" | "resourcepacks" | "shaderpacks") {
        return Err("不支持导出此资源类型信息".into());
    }
    let mut row = ResourceInfo {
        name: file_name.into(),
        file_name: file_name.into(),
        path: String::new(),
        enabled: !file_name.ends_with(".disabled"),
        fingerprint: None,
        version: None,
        description: None,
        icon: None,
    };
    let mut jar = zip::ZipArchive::new(file).map_err(|_| "资源压缩文件无法解析")?;
    let mut description = serde_json::Value::Null;
    let available = if kind == "mods" {
        let available = mod_metadata(&mut jar, &mut row, MetadataPurpose::Export);
        if let Some(value) = row.description {
            description = serde_json::Value::String(value);
        }
        available
    } else if let Some(pack) = jar_entry(&mut jar, "pack.mcmeta", 262144)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        // Minecraft text components remain structured JSON in the artifact;
        // no translation, trimming or lossy plain-text conversion is applied.
        description = pack["pack"].get("description").cloned().unwrap_or_default();
        !description.is_null()
    } else {
        false
    };
    Ok(serde_json::json!({
        "name": row.name, "file_name": row.file_name, "enabled": row.enabled,
        "version": row.version, "description": description, "metadataAvailable": available,
    }))
}

#[cfg(test)]
pub fn resource_dir(root: &Path, id: &str, kind: &str) -> Result<PathBuf, String> {
    resource_dir_with_isolation(
        root,
        id,
        kind,
        pcl_core::content_scope::IsolationPolicy::Automatic,
    )
}

pub fn resource_dir_with_isolation(
    root: &Path,
    id: &str,
    kind: &str,
    policy: pcl_core::content_scope::IsolationPolicy,
) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let instance =
        pcl_core::scan_instance_with_isolation(&root, id, policy)?.ok_or("未找到所选版本")?;
    let scope = pcl_core::content_scope::ContentScope::from_isolated(instance.isolated);
    let base = root.join(scope.relative_base(id)?);
    let child = match kind {
        "mods" => "mods",
        "saves" => "saves",
        "screenshots" => "screenshots",
        "resourcepacks" => "resourcepacks",
        "shaderpacks" => "shaderpacks",
        "litematics" => "schematics",
        "server" => "",
        _ => return Err("未知资源类型".into()),
    };
    let base = base.canonicalize().map_err(|e| e.to_string())?;
    if !base.starts_with(&root) {
        return Err("目录超出游戏目录".into());
    }
    let path = base.join(child);
    if path.exists()
        && !path
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&root)
    {
        return Err("目录超出游戏目录".into());
    }
    Ok(path)
}

#[tauri::command]
pub async fn instance_resources(
    id: String,
    kind: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<Vec<ResourceInfo>, String> {
    let shared = state.inner().clone();
    let captured = shared.config.isolation_settings(root_id.as_deref())?;
    let policy = captured.policy(&id);
    let root = PathBuf::from(&captured.root.path);
    tauri::async_runtime::spawn_blocking(move || {
        let folder = resource_dir_with_isolation(&root, &id, &kind, policy)?;
        if !folder.exists() {
            if shared
                .config
                .isolation_settings(Some(&captured.root.id))?
                .revision
                != captured.revision
            {
                return Err("实例隔离设置已变化，请重新读取资源列表".into());
            }
            return Ok(Vec::new());
        }
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(&folder).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if crate::resource_ops::is_stage_entry(&name) {
                continue;
            }
            if kind == "server" && name != "servers.dat" {
                continue;
            }
            let path = entry.path().canonicalize().map_err(|e| e.to_string())?;
            if !path.starts_with(&root) {
                continue;
            }
            if kind == "saves" && !path.is_dir() {
                continue;
            }
            let mut row = ResourceInfo {
                enabled: !name.ends_with(".disabled"),
                file_name: name.clone(),
                name,
                path: path.display().to_string(),
                version: None,
                description: None,
                icon: None,
                fingerprint: if (kind == "mods"
                    && (entry.file_name().to_string_lossy().ends_with(".jar")
                        || entry
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".jar.disabled")))
                    || (matches!(kind.as_str(), "resourcepacks" | "shaderpacks")
                        && entry.file_name().to_string_lossy().ends_with(".zip"))
                {
                    crate::resource_ops::fingerprint(&entry.path()).ok()
                } else {
                    None
                },
            };
            if kind == "mods"
                && (row.file_name.ends_with(".jar") || row.file_name.ends_with(".jar.disabled"))
            {
                if let Ok(file) = fs::File::open(&path) {
                    if let Ok(mut jar) = zip::ZipArchive::new(file) {
                        mod_metadata(&mut jar, &mut row, MetadataPurpose::Display);
                    }
                }
            }
            entries.push(row);
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        if resource_dir_with_isolation(&root, &id, &kind, policy)? != folder {
            return Err("实例内容目录已变化，请重新读取资源列表".into());
        }
        if shared
            .config
            .isolation_settings(Some(&captured.root.id))?
            .revision
            != captured.revision
        {
            return Err("实例隔离设置已变化，请重新读取资源列表".into());
        }
        Ok(entries)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse_fixture(files: &[(&str, &[u8])]) -> ResourceInfo {
        use std::io::{Cursor, Write};
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in files {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        let mut archive = zip::ZipArchive::new(writer.finish().unwrap()).unwrap();
        let mut row = ResourceInfo {
            name: "fixture.jar".into(),
            file_name: "fixture.jar".into(),
            path: String::new(),
            enabled: true,
            fingerprint: None,
            version: None,
            description: None,
            icon: None,
        };
        mod_metadata(&mut archive, &mut row, MetadataPurpose::Display);
        row
    }
    #[test]
    fn fabric_metadata_and_safe_icon_are_read_from_archive() {
        let row = parse_fixture(&[
            ("fabric.mod.json", br#"{"name":"Fixture Mod","version":"1.2.3","description":"Actual description","icon":"icon.png"}"#),
            ("icon.png", b"\x89PNG\r\n\x1a\nfixture"),
        ]);
        assert_eq!(row.name, "Fixture Mod");
        assert_eq!(row.version.as_deref(), Some("1.2.3"));
        assert_eq!(row.description.as_deref(), Some("Actual description"));
        assert!(row.icon.unwrap().starts_with("data:image/png;base64,"));
        let unsafe_row = parse_fixture(&[
            ("fabric.mod.json", br#"{"icon":"../icon.png"}"#),
            ("../icon.png", b"\x89PNG\r\n\x1a\nfixture"),
        ]);
        assert!(unsafe_row.icon.is_none());
    }
    #[test]
    fn forge_manifest_version_is_resolved_and_missing_version_omitted() {
        let metadata = b"[[mods]]\nmodId = 'fixture'\ndisplayName = 'Fixture Forge'\nversion = '${file.jarVersion}'\ndescription = 'Description'\n";
        let row = parse_fixture(&[
            ("META-INF/mods.toml", metadata),
            (
                "META-INF/MANIFEST.MF",
                b"Manifest-Version: 1.0\r\nImplementation-Version: 2.4.\r\n 1\r\n",
            ),
        ]);
        assert_eq!(row.name, "Fixture Forge");
        assert_eq!(row.version.as_deref(), Some("2.4.1"));
        assert!(parse_fixture(&[("META-INF/mods.toml", metadata)])
            .version
            .is_none());
        assert_eq!(capped_text(&"界".repeat(4000)).len(), 8190);
    }
}

/// Read-only public catalog. No account credentials or game files are sent.
#[tauri::command]
pub async fn modrinth_search(
    query: String,
    project_type: Option<String>,
    source: Option<String>,
    version: Option<String>,
    loader: Option<String>,
    sort: Option<String>,
    tag: Option<String>,
) -> Result<serde_json::Value, String> {
    if source.as_deref() == Some("CurseForge") {
        return Err("CurseForge 目录尚未接入".into());
    }
    if query.len() > 1024 {
        return Err("搜索内容过长".into());
    }
    let facet = match project_type.as_deref().unwrap_or("mod") {
        "mod" => "project_type:mod",
        "modpack" => "project_type:modpack",
        "resourcepack" => "project_type:resourcepack",
        "shader" => "project_type:shader",
        "datapack" => "all_project_types:datapack",
        _ => return Err("不支持的资源类型".into()),
    };
    let mut facets = vec![vec![facet.to_owned()]];
    for (key, value) in [
        ("versions", version),
        ("categories", loader),
        ("categories", tag),
    ] {
        if let Some(value) = value.filter(|v| v != "任意" && v != "全部" && !v.is_empty()) {
            if value.len() > 128 {
                return Err("筛选内容过长".into());
            }
            facets.push(vec![format!("{key}:{value}")]);
        }
    }
    let index = match sort.as_deref().unwrap_or("默认") {
        "下载量" | "downloads" => "downloads",
        "关注量" | "follows" => "follows",
        "最新发布" | "newest" => "newest",
        "最近更新" | "最新更新" | "updated" => "updated",
        _ if query.is_empty() => "downloads",
        _ => "relevance",
    };
    let client = pcl_network::async_client()
        .user_agent("PCL-RH/0.2.0 (https://github.com/HyacineFengJin/PCL-RH)")
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let facet_json = serde_json::to_string(&facets).map_err(|e| e.to_string())?;
    let mut response = client
        .get("https://api.modrinth.com/v2/search")
        .query(&[
            ("query", query.as_str()),
            ("facets", facet_json.as_str()),
            ("index", index),
            ("limit", "30"),
        ])
        .send()
        .await
        .map_err(|e| format!("无法读取资源目录：{e}"))?
        .error_for_status()
        .map_err(|e| format!("资源目录请求失败：{e}"))?;
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if data.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err("资源目录响应过大".into());
        }
        data.extend_from_slice(&chunk);
    }
    let mut result: serde_json::Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if let Some(hits) = result["hits"].as_array_mut() {
        for hit in hits {
            let valid = hit["icon_url"]
                .as_str()
                .is_some_and(|url| url.starts_with("https://cdn.modrinth.com/"));
            if !valid {
                hit["icon_url"] = serde_json::Value::Null;
            }
        }
    }
    Ok(result)
}
