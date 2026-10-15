use super::{types::*, CurseforgeDownload, MINECRAFT_GAME, PAGE_SIZE};
use crate::community_identity::{canonical_project_url, ProjectKind};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) fn numeric_id(value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|id| *id > 0 && id.to_string() == value)
        .ok_or_else(|| "CurseForge 项目或文件 ID 无效".into())
}

pub(super) fn class_id(kind: ProjectKind) -> Result<u32, String> {
    match kind {
        ProjectKind::Mod => Ok(6),
        ProjectKind::Modpack => Ok(4471),
        ProjectKind::Resourcepack => Ok(12),
        ProjectKind::Shader => Ok(6552),
        ProjectKind::Datapack => Err("CurseForge 当前不支持数据包检索".into()),
    }
}

fn project_kind(class: Option<u32>) -> Result<(ProjectKind, &'static str, &'static str), String> {
    match class {
        Some(6) => Ok((ProjectKind::Mod, "mod", "mc-mods")),
        Some(4471) => Ok((ProjectKind::Modpack, "modpack", "modpacks")),
        Some(12) => Ok((ProjectKind::Resourcepack, "resourcepack", "texture-packs")),
        Some(6552) => Ok((ProjectKind::Shader, "shader", "shaders")),
        _ => Err("CurseForge 项目类型不受支持".into()),
    }
}

fn filter(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !matches!(*value, "" | "任意" | "全部" | "any"))
}

pub(super) fn project_url_query(
    url: &str,
    kind: ProjectKind,
) -> Result<Vec<(&'static str, String)>, String> {
    let class = class_id(kind)?;
    let (_, _, route) = project_kind(Some(class))?;
    if canonical_project_url(url).as_deref() != Some(url) {
        return Err("CurseForge 项目链接必须是官方 HTTPS canonical 项目页".into());
    }
    let prefix = format!("https://www.curseforge.com/minecraft/{route}/");
    let slug = url
        .strip_prefix(&prefix)
        .filter(|value| slug(value))
        .ok_or("CurseForge 项目链接路径或资源类型无效")?;
    Ok(vec![
        ("gameId", MINECRAFT_GAME.to_string()),
        ("classId", class.to_string()),
        ("slug", slug.into()),
        ("pageSize", "2".into()),
        ("index", "0".into()),
    ])
}

pub(super) fn search_query(
    query: &str,
    kind: ProjectKind,
    version: Option<&str>,
    loader: Option<&str>,
    sort: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<(&'static str, String)>, String> {
    let class = class_id(kind)?;
    if query.len() > 1024 || query.chars().count() > 255 || query.chars().any(char::is_control) {
        return Err("CurseForge 搜索内容过长或包含无效字符".into());
    }
    if filter(tag).is_some() {
        return Err("CurseForge 不支持当前标签筛选，请清除标签后重试".into());
    }
    let mut parameters = vec![
        ("gameId", MINECRAFT_GAME.to_string()),
        ("classId", class.to_string()),
        ("pageSize", PAGE_SIZE.to_string()),
        ("index", "0".into()),
    ];
    let query = query.trim();
    if !query.is_empty() {
        parameters.push(("searchFilter", query.into()));
    }
    let version = filter(version);
    if let Some(version) = version {
        if version.len() > 64
            || !version
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'-' | b'_'))
        {
            return Err("CurseForge 游戏版本筛选无效".into());
        }
        parameters.push(("gameVersion", version.into()));
    }
    if let Some(loader) = filter(loader) {
        if version.is_none() {
            return Err("CurseForge 加载器筛选需要同时选择游戏版本".into());
        }
        let loader = match loader {
            "forge" => 1,
            "cauldron" => 2,
            "liteloader" => 3,
            "fabric" => 4,
            "quilt" => 5,
            "neoforge" => 6,
            _ => return Err("CurseForge 不支持此加载器筛选".into()),
        };
        parameters.push(("modLoaderType", loader.to_string()));
    }
    let sort = match sort.unwrap_or("default") {
        "" | "默认" | "default" if !query.is_empty() => None,
        "" | "默认" | "default" | "下载量" | "downloads" => Some(6),
        "最近更新" | "最新更新" | "updated" => Some(3),
        "最新发布" | "newest" => Some(11),
        "popularity" => Some(2),
        "name" => Some(4),
        _ => return Err("CurseForge 不支持此排序方式".into()),
    };
    if let Some(sort) = sort {
        parameters.push(("sortField", sort.to_string()));
        parameters.push(("sortOrder", if sort == 4 { "asc" } else { "desc" }.into()));
    }
    Ok(parameters)
}

fn text(value: &str, limit: usize, required: bool) -> Result<(), String> {
    if value.chars().count() > limit
        || (required && value.trim().is_empty())
        || value
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        Err("CurseForge 元数据内容无效或过长".into())
    } else {
        Ok(())
    }
}

fn slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
}

fn date(value: &str) -> Result<(), String> {
    if value.len() > 64 || chrono::DateTime::parse_from_rfc3339(value).is_err() {
        Err("CurseForge 元数据日期无效".into())
    } else {
        Ok(())
    }
}

pub(super) fn normalize_project(project: &Project, search: bool) -> Result<Value, String> {
    let (_, kind, route) = project_kind(project.class_id)?;
    if project.id == 0 || project.game_id != MINECRAFT_GAME || !slug(&project.slug) {
        return Err("CurseForge 项目 ID、游戏归属或项目链接无效".into());
    }
    text(&project.name, 256, true)?;
    text(&project.summary, 4096, false)?;
    date(&project.date_modified)?;
    let url = format!(
        "https://www.curseforge.com/minecraft/{route}/{}",
        project.slug
    );
    if canonical_project_url(&project.links.website_url).as_deref() != Some(url.as_str()) {
        return Err("CurseForge 项目链接与资源类型或 slug 不匹配".into());
    }
    if project.categories.len() > 64 || project.latest_files_indexes.len() > 256 {
        return Err("CurseForge 项目元数据数量超过上限".into());
    }
    let mut categories = BTreeSet::new();
    for category in &project.categories {
        if !slug(&category.slug) {
            return Err("CurseForge 分类标识无效".into());
        }
        categories.insert(category.slug.clone());
    }
    let mut versions = BTreeSet::new();
    let mut loaders = BTreeSet::new();
    for index in &project.latest_files_indexes {
        text(&index.game_version, 64, true)?;
        versions.insert(index.game_version.clone());
        if let Some(loader) = index.mod_loader.and_then(loader_name) {
            loaders.insert(loader);
        }
    }
    let display_categories = categories.clone();
    categories.extend(loaders.iter().map(|loader| (*loader).to_owned()));
    let icon = project.logo.as_ref().and_then(|logo| image_url(&logo.url));
    let mut value = json!({
        "project_id": project.id.to_string(), "slug": project.slug,
        "title": project.name, "description": project.summary,
        "icon_url": icon, "categories": categories, "display_categories": display_categories,
        "downloads": project.download_count, "date_modified": project.date_modified,
        "project_type": kind, "source": "CurseForge", "url": url,
        "game_versions": versions, "loaders": loaders,
        "is_available": project.is_available,
        "download_allowed": project.is_available && project.allow_mod_distribution != Some(false),
    });
    if search {
        value["versions"] = value["game_versions"].clone();
    }
    Ok(value)
}

fn loader_name(value: u32) -> Option<&'static str> {
    match value {
        1 => Some("forge"),
        2 => Some("cauldron"),
        3 => Some("liteloader"),
        4 => Some("fabric"),
        5 => Some("quilt"),
        6 => Some("neoforge"),
        _ => None,
    }
}

pub(super) fn check_filename(value: &str) -> Result<(), String> {
    let stem = value.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if value.is_empty()
        || value.len() > 255
        || matches!(value, "." | "..")
        || value.ends_with(['.', ' '])
        || value
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.len() > 64)
        || reserved
        || value
            .chars()
            .any(|ch| ch.is_control() || "/\\:<>\"|?*".contains(ch))
    {
        return Err("CurseForge 文件名无效".into());
    }
    Ok(())
}

pub(super) fn normalize_version(file: &File) -> Result<Value, String> {
    check_filename(&file.file_name)?;
    text(&file.display_name, 256, true)?;
    date(&file.file_date)?;
    if file.game_versions.len() > 128 || file.dependencies.len() > 256 || file.hashes.len() > 16 {
        return Err("CurseForge 文件元数据数量超过上限".into());
    }
    let mut versions = Vec::new();
    let mut loaders = BTreeSet::new();
    for version in &file.game_versions {
        text(version, 64, true)?;
        let lower = version.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "forge" | "cauldron" | "liteloader" | "fabric" | "quilt" | "neoforge"
        ) {
            loaders.insert(lower);
        } else {
            versions.push(version.clone());
        }
    }
    let dependencies: Vec<_> = file
        .dependencies
        .iter()
        .filter_map(|reference| {
            let relation = match reference.relation_type {
                1 => "embedded",
                2 => "optional",
                3 => "required",
                4 => "tool",
                5 => "incompatible",
                6 => "include",
                _ => return None,
            };
            (reference.mod_id > 0).then(|| {
                json!({"project_id": reference.mod_id.to_string(),
            "version_id": null, "file_name": null, "dependency_type": relation})
            })
        })
        .collect();
    let version_type = match file.release_type {
        1 => "release",
        2 => "beta",
        3 => "alpha",
        _ => return Err("CurseForge 文件发布类型无效".into()),
    };
    Ok(json!({
        "id": file.id.to_string(), "project_id": file.mod_id.to_string(),
        "name": file.display_name, "version_number": file.display_name,
        "game_versions": versions, "loaders": loaders, "date_published": file.file_date,
        "downloads": file.download_count, "version_type": version_type,
        "files": [{"filename": file.file_name, "size": file.file_length, "primary": true}],
        "dependencies": dependencies,
        "is_available": file.is_available && !file.is_early_access_content,
    }))
}

fn trusted_cdn_url(value: &str, image: bool) -> Option<String> {
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return None;
    }
    let url = reqwest::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
        || !matches!(
            url.host_str()?,
            "edge.forgecdn.net" | "media.forgecdn.net" | "mediafilez.forgecdn.net"
        )
        || (!image && !url.path().starts_with("/files/"))
    {
        return None;
    }
    Some(url.into())
}

fn image_url(value: &str) -> Option<String> {
    trusted_cdn_url(value, true)
}

pub(super) fn download_url(raw: &str) -> Result<reqwest::Url, String> {
    let validated =
        trusted_cdn_url(raw, false).ok_or("CurseForge 未提供可用的官方下载链接，请访问项目页面")?;
    reqwest::Url::parse(&validated).map_err(|_| "CurseForge 下载链接无效".into())
}

pub(super) fn download_metadata(
    project: &Project,
    file: &File,
    file_id: u32,
    file_name: &str,
) -> Result<CurseforgeDownload, String> {
    file.check_owner(project.id)?;
    normalize_version(file)?;
    if file.id != file_id || file.file_name != file_name {
        return Err("CurseForge 文件 ID 或文件名与所选版本不匹配".into());
    }
    if !project.is_available
        || project.allow_mod_distribution == Some(false)
        || !file.is_available
        || file.is_early_access_content
    {
        return Err("CurseForge 文件不可用或作者未允许此次下载，请访问项目页面".into());
    }
    let url = download_url(file.download_url.as_deref().unwrap_or(""))?;
    let sha1s: Vec<_> = file.hashes.iter().filter(|hash| hash.algo == 1).collect();
    if sha1s.len() != 1
        || sha1s[0].value.len() != 40
        || !sha1s[0].value.bytes().all(|ch| ch.is_ascii_hexdigit())
        || file.file_length == 0
        || file.file_length > 2 * 1024 * 1024 * 1024
    {
        return Err("CurseForge 文件缺少有效 SHA1 或文件大小，无法安全下载".into());
    }
    Ok(CurseforgeDownload {
        title: project.name.clone(),
        version: file.display_name.clone(),
        name: file.file_name.clone(),
        size: file.file_length,
        url: url.into(),
        sha1: sha1s[0].value.to_ascii_lowercase(),
    })
}
