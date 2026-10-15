use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

const API: &str = "https://api.modrinth.com/v2";
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const MAX_VERSIONS: usize = 4_000;
const MAX_DEPENDENCIES: usize = 32;

fn valid_id(value: &str) -> bool {
    value.len() == 8 && value.bytes().all(|c| c.is_ascii_alphanumeric())
}

fn client() -> Result<reqwest::Client, String> {
    client_with(&pcl_network::snapshot())
}

fn client_with(network: &pcl_network::ClientFactory) -> Result<reqwest::Client, String> {
    network
        .async_client()
        .user_agent("PCL-RH/0.2.0 (https://github.com/HyacineFengJin/PCL-RH)")
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())
}

async fn get<T: DeserializeOwned>(
    client: &reqwest::Client,
    endpoint: &str,
    query: &[(&str, String)],
) -> Result<T, String> {
    let mut response = client
        .get(format!("{API}/{endpoint}"))
        .query(query)
        .send()
        .await
        .map_err(|e| format!("无法读取资源详情：{e}"))?
        .error_for_status()
        .map_err(|e| format!("资源详情请求失败：{e}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE as u64)
    {
        return Err("资源详情响应过大".into());
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if chunk.len() > MAX_RESPONSE.saturating_sub(data.len()) {
            return Err("资源详情响应过大".into());
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|e| format!("资源详情格式无效：{e}"))
}

#[derive(Deserialize)]
struct ApiProject {
    id: String,
    #[serde(default)]
    slug: Option<String>,
    title: String,
    description: String,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    additional_categories: Vec<String>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    downloads: u64,
    updated: String,
    project_type: String,
}

#[derive(Clone, Serialize)]
pub struct ResourceProject {
    project_id: String,
    slug: Option<String>,
    title: String,
    description: String,
    icon_url: Option<String>,
    categories: Vec<String>,
    display_categories: Vec<String>,
    game_versions: Vec<String>,
    loaders: Vec<String>,
    downloads: u64,
    date_modified: String,
    project_type: String,
    source: &'static str,
    url: String,
}

fn normalize_project(project: ApiProject) -> ResourceProject {
    let icon_url = project.icon_url.filter(|value| {
        reqwest::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("cdn.modrinth.com")
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
        })
    });
    let kind = match project.project_type.as_str() {
        "modpack" => "modpack",
        "resourcepack" => "resourcepack",
        "shader" => "shader",
        _ => "mod",
    };
    // IDs give a stable URL without trusting slugs or user supplied paths.
    let url = format!("https://modrinth.com/{kind}/{}", project.id);
    ResourceProject {
        project_id: project.id,
        slug: project.slug,
        title: project.title,
        description: project.description,
        icon_url,
        display_categories: project.categories.clone(),
        categories: project
            .categories
            .into_iter()
            .chain(project.additional_categories)
            .collect(),
        game_versions: project.game_versions,
        loaders: project.loaders,
        downloads: project.downloads,
        date_modified: project.updated,
        project_type: project.project_type,
        source: "Modrinth",
        url,
    }
}

/// Resolve an encyclopedia's explicit project link. Provider-confirmed slug
/// equality is required; search text never grants project equivalence.
pub(crate) async fn project_for_link(
    raw: &str,
    network: &pcl_network::ClientFactory,
) -> Result<serde_json::Value, String> {
    let canonical =
        crate::community_identity::canonical_project_url(raw).ok_or("Modrinth 项目链接无效")?;
    let url = reqwest::Url::parse(&canonical).map_err(|_| "Modrinth 项目链接无效")?;
    if url.host_str() != Some("modrinth.com") {
        return Err("Modrinth 项目链接来源不符".into());
    }
    let segments: Vec<_> = url
        .path_segments()
        .ok_or("Modrinth 项目链接无效")?
        .collect();
    let [kind, slug] = segments.as_slice() else {
        return Err("Modrinth 项目链接无效".into());
    };
    let project: ApiProject = get(&client_with(network)?, &format!("project/{slug}"), &[]).await?;
    if !valid_id(&project.id)
        || (project.id != *slug && project.slug.as_deref() != Some(*slug))
        || project.project_type != *kind
    {
        return Err("Modrinth 项目与百科链接不一致".into());
    }
    serde_json::to_value(normalize_project(project)).map_err(|_| "无法读取 Modrinth 项目".into())
}

/// Bookmarks fetch only this project endpoint. The renderer supplies a stable
/// identity; its display text, URLs and file metadata cannot enter the store.
pub(crate) async fn project_for_favorite(
    project_id: &str,
) -> Result<crate::launcher_favorites::Entry, String> {
    if !valid_id(project_id) {
        return Err("无效的 Modrinth 项目 ID".into());
    }
    let project: ApiProject = get(&client()?, &format!("project/{project_id}"), &[]).await?;
    if project.id != project_id {
        return Err("收藏资源项目 ID 不匹配".into());
    }
    favorite_entry(project)
}

fn favorite_entry(project: ApiProject) -> Result<crate::launcher_favorites::Entry, String> {
    let project = normalize_project(project);
    let entry = crate::launcher_favorites::Entry {
        provider: "modrinth".into(),
        project_id: project.project_id,
        folder_id: "default".into(),
        title: project.title,
        project_type: project.project_type,
        icon_url: project.icon_url,
        summary: project.description,
    };
    entry.validate()?;
    Ok(entry)
}

pub(crate) async fn projects_for_favorites(
    ids: &[String],
) -> Result<Vec<crate::launcher_favorites::Entry>, String> {
    crate::launcher_favorites::validate_projects(ids)?;
    let query = serde_json::to_string(ids).map_err(|_| "收藏项目标识无法编码")?;
    let projects: Vec<ApiProject> = get(&client()?, "projects", &[("ids", query)]).await?;
    let mut seen = std::collections::BTreeSet::new();
    if projects.len() != ids.len() {
        return Err("提供方未返回全部收藏项目，未保存任何项目".into());
    }
    let mut entries = Vec::new();
    for project in projects {
        if !ids.contains(&project.id) || !seen.insert(project.id.clone()) {
            return Err("提供方返回了重复或无关收藏项目".into());
        }
        entries.push(favorite_entry(project)?);
    }
    Ok(entries)
}

#[derive(Clone, Deserialize, Serialize)]
pub struct VersionDependency {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    version_id: Option<String>,
    #[serde(default)]
    file_name: Option<String>,
    dependency_type: String,
}

#[derive(Deserialize, Serialize)]
pub struct ResourceFile {
    filename: String,
    size: u64,
    #[serde(default)]
    primary: bool,
}

#[derive(Deserialize, Serialize)]
pub struct ResourceVersion {
    id: String,
    project_id: String,
    name: String,
    version_number: String,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    date_published: String,
    downloads: u64,
    version_type: String,
    #[serde(default)]
    files: Vec<ResourceFile>,
    #[serde(default)]
    dependencies: Vec<VersionDependency>,
}

#[derive(Serialize)]
pub struct ResourceDetails {
    project: ResourceProject,
    versions: Vec<ResourceVersion>,
    versions_truncated: bool,
}

#[tauri::command]
pub async fn resource_details(project_id: String) -> Result<ResourceDetails, String> {
    if !valid_id(&project_id) {
        return Err("无效的 Modrinth 项目 ID".into());
    }
    let client = client()?;
    let project: ApiProject = get(&client, &format!("project/{project_id}"), &[]).await?;
    if project.id != project_id {
        return Err("资源详情项目 ID 不匹配".into());
    }
    let mut versions: Vec<ResourceVersion> = get(
        &client,
        &format!("project/{project_id}/version"),
        &[("include_changelog", "false".into())],
    )
    .await?;
    versions.retain(|version| version.project_id == project_id && valid_id(&version.id));
    versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    let versions_truncated = versions.len() > MAX_VERSIONS;
    versions.truncate(MAX_VERSIONS);
    Ok(ResourceDetails {
        project: normalize_project(project),
        versions,
        versions_truncated,
    })
}

#[derive(Serialize)]
pub struct ResourceDependency {
    #[serde(flatten)]
    reference: VersionDependency,
    project: Option<ResourceProject>,
}

#[derive(Serialize)]
pub struct ResourceDependencies {
    version_id: String,
    dependencies: Vec<ResourceDependency>,
    truncated: bool,
}

#[tauri::command]
pub async fn resource_dependencies(version_id: String) -> Result<ResourceDependencies, String> {
    if !valid_id(&version_id) {
        return Err("无效的 Modrinth 版本 ID".into());
    }
    let client = client()?;
    let version: ResourceVersion = get(&client, &format!("version/{version_id}"), &[]).await?;
    if version.id != version_id {
        return Err("资源详情版本 ID 不匹配".into());
    }
    let mut references: Vec<_> = version
        .dependencies
        .into_iter()
        .filter(|reference| matches!(reference.dependency_type.as_str(), "required" | "optional"))
        .collect();
    let truncated = references.len() > MAX_DEPENDENCIES;
    references.truncate(MAX_DEPENDENCIES);

    // Resolve references with only version IDs in one request, then read project
    // metadata in one request. No per-dependency requests or local file hashes.
    let mut version_ids: Vec<_> = references
        .iter()
        .filter(|reference| reference.project_id.is_none())
        .filter_map(|reference| reference.version_id.as_ref())
        .filter(|id| valid_id(id))
        .cloned()
        .collect();
    version_ids.sort();
    version_ids.dedup();
    if !version_ids.is_empty() {
        let versions: Vec<ResourceVersion> = get(
            &client,
            "versions",
            &[(
                "ids",
                serde_json::to_string(&version_ids).map_err(|e| e.to_string())?,
            )],
        )
        .await?;
        let projects: HashMap<_, _> = versions
            .into_iter()
            .filter(|version| version_ids.contains(&version.id) && valid_id(&version.project_id))
            .map(|version| (version.id, version.project_id))
            .collect();
        for reference in &mut references {
            if reference.project_id.is_none() {
                reference.project_id = reference
                    .version_id
                    .as_ref()
                    .and_then(|id| projects.get(id).cloned());
            }
        }
    }
    let mut project_ids: Vec<_> = references
        .iter()
        .filter_map(|reference| reference.project_id.as_ref())
        .filter(|id| valid_id(id))
        .cloned()
        .collect();
    project_ids.sort();
    project_ids.dedup();
    let projects: HashMap<_, _> = if project_ids.is_empty() {
        HashMap::new()
    } else {
        let projects: Vec<ApiProject> = get(
            &client,
            "projects",
            &[(
                "ids",
                serde_json::to_string(&project_ids).map_err(|e| e.to_string())?,
            )],
        )
        .await?;
        projects
            .into_iter()
            .filter(|project| project_ids.contains(&project.id))
            .map(normalize_project)
            .map(|project| (project.project_id.clone(), project))
            .collect()
    };
    Ok(ResourceDependencies {
        version_id,
        dependencies: references
            .into_iter()
            .map(|reference| ResourceDependency {
                project: reference
                    .project_id
                    .as_ref()
                    .and_then(|id| projects.get(id).cloned()),
                reference,
            })
            .collect(),
        truncated,
    })
}

fn allowed_link(value: &str) -> Result<reqwest::Url, String> {
    if value.len() > 2_048 {
        return Err("链接过长".into());
    }
    let parsed = reqwest::Url::parse(value).map_err(|_| "无效链接")?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || !matches!(
            parsed.host_str(),
            Some("modrinth.com" | "www.mcmod.cn" | "search.mcmod.cn" | "www.curseforge.com")
        )
    {
        return Err("不支持的资源链接".into());
    }
    Ok(parsed)
}

#[tauri::command]
pub fn resource_open_link(url: String) -> Result<(), String> {
    let parsed = allowed_link(&url)?;
    crate::open_official(parsed.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_paths_and_queries() {
        assert!(valid_id("Ab12Cd34"));
        for value in [
            "",
            "fabric-api",
            "../Ab1234",
            "Ab12?d34",
            "Ab12/d34",
            "Ab12Cd345",
        ] {
            assert!(!valid_id(value), "{value}");
        }
    }

    #[test]
    fn links_are_limited_to_https_resource_hosts() {
        assert!(allowed_link("https://modrinth.com/mod/Ab12Cd34").is_ok());
        assert!(allowed_link("https://www.mcmod.cn/").is_ok());
        for url in [
            "http://modrinth.com/",
            "https://modrinth.com.example.org/",
            "https://user@modrinth.com/",
            "https://modrinth.com:444/",
            "file:///tmp/resource.jar",
            "https://example.org/",
        ] {
            assert!(allowed_link(url).is_err(), "{url}");
        }
    }

    #[test]
    fn project_normalization_keeps_actual_metadata_and_filters_icons() {
        let input = br#"{"id":"Ab12Cd34","title":"Example resource","description":"A resource","downloads":42,"updated":"2026-10-01T00:00:00Z","project_type":"mod","icon_url":"https://cdn.modrinth.com.example.org/icon.png","categories":["library"],"loaders":["fabric"],"game_versions":["1.21.1"]}"#;
        let project = normalize_project(serde_json::from_slice(input).unwrap());
        assert_eq!(project.downloads, 42);
        assert_eq!(project.game_versions, ["1.21.1"]);
        assert_eq!(project.loaders, ["fabric"]);
        assert!(project.icon_url.is_none());
        assert_eq!(project.url, "https://modrinth.com/mod/Ab12Cd34");
    }

    #[test]
    fn version_parser_preserves_dependency_kinds_and_file_metadata() {
        let input = br#"{"id":"Ve12Cd34","project_id":"Ab12Cd34","name":"Example 1.0","version_number":"1.0","date_published":"2026-10-01T00:00:00Z","downloads":7,"version_type":"beta","files":[{"filename":"example.jar","size":1024,"primary":true,"url":"https://cdn.modrinth.com/example.jar"}],"dependencies":[{"project_id":"De12Cd34","version_id":null,"file_name":null,"dependency_type":"required"},{"project_id":null,"version_id":"Op12Cd34","dependency_type":"optional"}]}"#;
        let version: ResourceVersion = serde_json::from_slice(input).unwrap();
        assert_eq!(version.files[0].filename, "example.jar");
        assert_eq!(version.files[0].size, 1024);
        assert_eq!(version.dependencies[0].dependency_type, "required");
        assert_eq!(
            version.dependencies[1].version_id.as_deref(),
            Some("Op12Cd34")
        );
        assert_eq!(version.version_type, "beta");
    }
}
