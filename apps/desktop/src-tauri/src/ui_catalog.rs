use crate::{ui_data, Shared};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::PathBuf, sync::Arc, time::Duration};
use tauri::State;

fn client() -> Result<reqwest::Client, String> {
    pcl_network::async_client()
        .user_agent("PCL-RH/0.2.0 (https://github.com/HyacineFengJin/PCL-RH)")
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}
async fn get(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut response = client()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}"))?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if data.len() + chunk.len() > limit {
            return Err("响应内容过大".into());
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}
#[tauri::command]
pub fn ui_open_link(url: String) -> Result<(), String> {
    let parsed = reqwest::Url::parse(&url).map_err(|_| "无效链接")?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(
            parsed.host_str(),
            Some(
                "github.com"
                    | "pi.dev"
                    | "neoforged.net"
                    | "www.mcmod.cn"
                    | "files.minecraftforge.net"
                    | "fabricmc.net"
                    | "legacyfabric.net"
                    | "optifine.net"
                    | "cleanroommc.com"
                    | "www.liteloader.com"
                    | "laby.net"
                    | "www.minecraft.net"
            )
        )
    {
        return Err("不支持的链接".into());
    }
    crate::open_official(parsed.as_str())
}
#[derive(Serialize)]
pub struct LoaderGroup {
    minecraft: String,
    versions: Vec<String>,
}
fn neoforge_game(version: &str) -> Option<String> {
    if let Some(rest) = version.strip_prefix("0.") {
        return rest.split('.').next().map(str::to_owned);
    }
    let core = version.split('-').next()?;
    let numbers: Vec<_> = core.split('.').collect();
    if numbers.len() < 3 {
        return None;
    }
    if numbers[0] == "20" || numbers[0] == "21" {
        Some(format!("1.{}.{}", numbers[0], numbers[1]))
    } else if numbers[0].parse::<u32>().ok()? >= 26 {
        Some(if numbers[2] == "0" {
            format!("{}.{}", numbers[0], numbers[1])
        } else {
            format!("{}.{}.{}", numbers[0], numbers[1], numbers[2])
        })
    } else {
        None
    }
}
fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}
// Compare numeric components as numbers, including versions such as 1.9 and 1.21.
fn version_key(value: &str) -> Vec<(bool, u64, String)> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = value.as_bytes();
    while start < bytes.len() {
        let numeric = bytes[start].is_ascii_digit();
        let mut end = start + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() == numeric {
            end += 1;
        }
        let part = &value[start..end];
        parts.push((
            numeric,
            if numeric {
                part.parse().unwrap_or(u64::MAX)
            } else {
                0
            },
            if numeric {
                String::new()
            } else {
                part.to_owned()
            },
        ));
        start = end;
    }
    parts
}
fn grouped_versions(entries: impl IntoIterator<Item = (String, String)>) -> Vec<LoaderGroup> {
    let mut groups = Vec::<LoaderGroup>::new();
    for (game, version) in entries {
        if !valid_version(&game) || !valid_version(&version) {
            continue;
        }
        if let Some(group) = groups.iter_mut().find(|g| g.minecraft == game) {
            if !group.versions.contains(&version) {
                group.versions.push(version);
            }
        } else {
            groups.push(LoaderGroup {
                minecraft: game,
                versions: vec![version],
            });
        }
    }
    groups.sort_by_cached_key(|g| std::cmp::Reverse(version_key(&g.minecraft)));
    for group in &mut groups {
        group
            .versions
            .sort_by_cached_key(|v| std::cmp::Reverse(version_key(v)));
    }
    groups
}
fn filename_versions(text: &str, prefix: &str, suffix: &str) -> Vec<String> {
    text.split(prefix)
        .skip(1)
        .filter_map(|part| {
            let end = part.find(suffix)?;
            let value = &part[..end];
            valid_version(value).then(|| value.to_owned())
        })
        .collect()
}
fn parse_loader_catalog(loader: &str, data: &[u8]) -> Result<Vec<LoaderGroup>, String> {
    let text = std::str::from_utf8(data).map_err(|e| e.to_string())?;
    let entries: Vec<(String, String)> = match loader {
        "NeoForge" => {
            #[derive(Deserialize)]
            struct Catalog {
                versions: Vec<String>,
            }
            let catalog: Catalog = serde_json::from_slice(data).map_err(|e| e.to_string())?;
            catalog
                .versions
                .into_iter()
                .filter(|v| !v.contains('+'))
                .filter_map(|v| neoforge_game(&v).map(|g| (g, v)))
                .collect()
        }
        "Forge" => filename_versions(text, "<version>", "</version>")
            .into_iter()
            .filter_map(|v| v.split_once('-').map(|(g, v)| (g.to_owned(), v.to_owned())))
            .collect(),
        "Fabric" | "Legacy Fabric" => {
            #[derive(Deserialize)]
            struct Installer {
                version: String,
            }
            let entries: Vec<Installer> =
                serde_json::from_slice(data).map_err(|e| e.to_string())?;
            // The official installer list is independent of the game version.
            let mut versions: Vec<_> = entries
                .into_iter()
                .map(|e| e.version)
                .filter(|v| valid_version(v))
                .collect();
            versions.sort_by_cached_key(|v| std::cmp::Reverse(version_key(v)));
            versions.dedup();
            return Ok(if versions.is_empty() {
                vec![]
            } else {
                vec![LoaderGroup {
                    minecraft: "安装器".into(),
                    versions,
                }]
            });
        }
        "OptiFine" => filename_versions(text, "OptiFine_", ".jar")
            .into_iter()
            .filter_map(|v| v.split_once('_').map(|(g, v)| (g.to_owned(), v.to_owned())))
            .collect(),
        "Cleanroom" => filename_versions(text, "cleanroom-", "-installer.jar")
            .into_iter()
            .map(|v| ("1.12.2".to_owned(), v))
            .collect(),
        "LiteLoader" => filename_versions(text, "liteloader-installer-", ".jar")
            .into_iter()
            .filter_map(|v| v.split_once('-').map(|(g, v)| (g.to_owned(), v.to_owned())))
            .collect(),
        _ => return Err("不支持的安装包目录".into()),
    };
    let mut groups = grouped_versions(entries);
    if loader == "NeoForge" {
        if let Some(index) = groups.iter().position(|g| g.minecraft == "25w14craftmine") {
            let group = groups.remove(index);
            let index = groups
                .iter()
                .position(|g| g.minecraft.starts_with("1."))
                .unwrap_or(groups.len());
            groups.insert(index, group);
        }
    }
    Ok(groups)
}
#[tauri::command]
pub async fn loader_catalog(loader: String) -> Result<Vec<LoaderGroup>, String> {
    let url = match loader.as_str() {
        "NeoForge" => {
            "https://maven.neoforged.net/api/maven/versions/releases/net%2Fneoforged%2Fneoforge"
        }
        "Forge" => "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml",
        "Fabric" => "https://meta.fabricmc.net/v2/versions/installer",
        "Legacy Fabric" => "https://meta.legacyfabric.net/v2/versions/installer",
        "OptiFine" => "https://optifine.net/downloads",
        "Cleanroom" => "https://download.cleanroommc.com/",
        "LiteLoader" => "https://www.liteloader.com/download",
        _ => return Err("不支持的安装包目录".into()),
    };
    let groups = parse_loader_catalog(&loader, &get(url, 2 * 1024 * 1024).await?)?;
    if groups.is_empty() {
        return Err("官网未返回可识别的安装包，请前往官网查看".into());
    }
    Ok(groups)
}
#[tauri::command]
pub async fn loader_candidates(loader: String, minecraft: String) -> Result<Vec<String>, String> {
    if !valid_version(&minecraft) {
        return Err("无效的 Minecraft 版本".into());
    }
    if loader == "Fabric" {
        #[derive(Deserialize)]
        struct Loader {
            version: String,
        }
        #[derive(Deserialize)]
        struct Entry {
            loader: Loader,
        }
        let url = format!("https://meta.fabricmc.net/v2/versions/loader/{minecraft}");
        let entries: Vec<Entry> = serde_json::from_slice(&get(&url, 2 * 1024 * 1024).await?)
            .map_err(|e| e.to_string())?;
        let groups = grouped_versions(
            entries
                .into_iter()
                .map(|e| (minecraft.clone(), e.loader.version)),
        );
        return Ok(groups
            .into_iter()
            .next()
            .map(|g| g.versions)
            .unwrap_or_default());
    }
    if !matches!(loader.as_str(), "Forge" | "NeoForge" | "OptiFine") {
        return Err("此组件的兼容版本目录尚未接入".into());
    }
    Ok(loader_catalog(loader)
        .await?
        .into_iter()
        .find(|g| g.minecraft == minecraft)
        .map(|g| g.versions)
        .unwrap_or_default())
}
#[derive(Serialize)]
pub struct Contributor {
    login: String,
    avatar: String,
    url: String,
}
#[tauri::command]
pub async fn upstream_contributors() -> Result<Vec<Contributor>, String> {
    let data = get(
        "https://api.github.com/repos/PCL-Community/PCL-CE/contributors?per_page=30",
        2 * 1024 * 1024,
    )
    .await?;
    let list: Vec<serde_json::Value> = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    Ok(list
        .into_iter()
        .filter_map(|v| {
            let login = v["login"].as_str()?;
            let avatar = v["avatar_url"].as_str()?;
            let url = v["html_url"].as_str()?;
            if !avatar.starts_with("https://avatars.githubusercontent.com/")
                || !url.starts_with("https://github.com/")
            {
                return None;
            }
            Some(Contributor {
                login: login.to_owned(),
                avatar: format!(
                    "{avatar}{}s=80",
                    if avatar.contains('?') { "&" } else { "?" }
                ),
                url: url.to_owned(),
            })
        })
        .collect())
}
#[derive(Serialize)]
pub struct Issue {
    title: String,
    url: String,
    labels: Vec<String>,
    author: String,
    created_at: String,
}
#[tauri::command]
pub async fn project_feedback() -> Result<Vec<Issue>, String> {
    let data = get(
        "https://api.github.com/repos/HyacineFengJin/PCL-RH/issues?state=open&per_page=100",
        2 * 1024 * 1024,
    )
    .await?;
    let list: Vec<serde_json::Value> = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    Ok(list
        .into_iter()
        .filter(|v| v.get("pull_request").is_none())
        .filter_map(|v| {
            Some(Issue {
                title: v["title"].as_str()?.to_owned(),
                url: v["html_url"].as_str()?.to_owned(),
                labels: v["labels"]
                    .as_array()?
                    .iter()
                    .filter_map(|x| x["name"].as_str().map(str::to_owned))
                    .collect(),
                author: v["user"]["login"].as_str()?.to_owned(),
                created_at: v["created_at"].as_str()?.to_owned(),
            })
        })
        .collect())
}
#[derive(Deserialize, Serialize)]
pub struct Server {
    name: String,
    ip: String,
    #[serde(default)]
    icon: Option<String>,
}
#[derive(Deserialize)]
struct Servers {
    #[serde(default)]
    servers: Vec<Server>,
}
fn decode_servers(data: &[u8]) -> Result<Vec<Server>, String> {
    let parsed: Servers =
        fastnbt::from_bytes(data).map_err(|e| format!("服务器列表读取失败：{e}"))?;
    Ok(parsed
        .servers
        .into_iter()
        .map(|mut v| {
            v.name = v.name.chars().take(512).collect();
            v.ip = v.ip.chars().take(512).collect();
            v.icon = v
                .icon
                .filter(|i| i.len() < 180000)
                .filter(|i| {
                    use base64::Engine;
                    let value = i.strip_prefix("data:image/png;base64,").unwrap_or(i);
                    base64::engine::general_purpose::STANDARD
                        .decode(value)
                        .is_ok_and(|b| b.starts_with(b"\x89PNG\r\n\x1a\n"))
                })
                .map(|i| {
                    if i.starts_with("data:") {
                        i
                    } else {
                        format!("data:image/png;base64,{i}")
                    }
                });
            v
        })
        .collect())
}
#[tauri::command]
pub async fn instance_servers(
    id: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<Vec<Server>, String> {
    let shared = state.inner().clone();
    let captured = shared.config.isolation_settings(root_id.as_deref())?;
    let policy = captured.policy(&id);
    let root = PathBuf::from(&captured.root.path);
    tauri::async_runtime::spawn_blocking(move || {
        let base = ui_data::resource_dir_with_isolation(&root, &id, "server", policy)?;
        let path = base.join("servers.dat");
        if !path.exists() {
            if shared
                .config
                .isolation_settings(Some(&captured.root.id))?
                .revision
                != captured.revision
            {
                return Err("实例隔离设置已变化，请重新读取服务器列表".into());
            }
            return Ok(Vec::new());
        }
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        if !path.starts_with(root.canonicalize().map_err(|e| e.to_string())?) {
            return Err("服务器列表超出游戏目录".into());
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("服务器列表过大".into());
        }
        let result = if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut data = Vec::new();
            flate2::read::GzDecoder::new(bytes.as_slice())
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            if data.len() > 8 * 1024 * 1024 {
                return Err("服务器列表过大".into());
            }
            decode_servers(&data)
        } else {
            decode_servers(&bytes)
        };
        if ui_data::resource_dir_with_isolation(&root, &id, "server", policy)? != base {
            return Err("实例内容目录已变化，请重新读取服务器列表".into());
        }
        if shared
            .config
            .isolation_settings(Some(&captured.root.id))?
            .revision
            != captured.revision
        {
            return Err("实例隔离设置已变化，请重新读取服务器列表".into());
        }
        result
    })
    .await
    .map_err(|e| e.to_string())?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_nbt_preserves_names_and_discards_external_icon() {
        #[derive(Serialize)]
        struct Fixture {
            servers: Vec<Server>,
        }
        let bytes = fastnbt::to_bytes(&Fixture {
            servers: vec![Server {
                name: "Fixture Server".into(),
                ip: "localhost:25565".into(),
                icon: Some("https://example.com/icon.png".into()),
            }],
        })
        .unwrap();
        let rows = decode_servers(&bytes).unwrap();
        assert_eq!(rows[0].name, "Fixture Server");
        assert_eq!(rows[0].ip, "localhost:25565");
        assert!(rows[0].icon.is_none());
        assert!(decode_servers(b"invalid").is_err());
    }
    #[test]
    fn forge_catalog_orders_numeric_versions_and_deduplicates() {
        let groups = parse_loader_catalog("Forge", b"<version>1.9-12.1.9</version><version>1.21-51.0.10</version><version>1.21-51.0.2</version><version>1.21-51.0.10</version>").unwrap();
        assert_eq!(groups[0].minecraft, "1.21");
        assert_eq!(groups[0].versions, ["51.0.10", "51.0.2"]);
        assert_eq!(groups[1].minecraft, "1.9");
    }
    #[test]
    fn installer_html_extracts_versions_without_links_or_markup() {
        let groups = parse_loader_catalog("OptiFine", br#"href="OptiFine_1.20.1_HD_U_I6.jar" mirror="OptiFine_1.20.1_HD_U_I6.jar" invalid="OptiFine_../../bad.jar""#).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].minecraft, "1.20.1");
        assert_eq!(groups[0].versions, ["HD_U_I6"]);
        let groups =
            parse_loader_catalog("LiteLoader", b"liteloader-installer-1.12.2-00-SNAPSHOT.jar")
                .unwrap();
        assert_eq!(groups[0].minecraft, "1.12.2");
        assert_eq!(groups[0].versions, ["00-SNAPSHOT"]);
        let groups =
            parse_loader_catalog("Fabric", br#"[{"version":"1.1.2"},{"version":"0.11.2"}]"#)
                .unwrap();
        assert_eq!(groups[0].minecraft, "安装器");
        assert_eq!(groups[0].versions, ["1.1.2", "0.11.2"]);
    }
    #[test]
    fn neoforge_versions_keep_game_patch_separate_from_build() {
        assert_eq!(neoforge_game("21.1.234").as_deref(), Some("1.21.1"));
        assert_eq!(neoforge_game("26.1.0.16-beta").as_deref(), Some("26.1"));
        assert_eq!(neoforge_game("26.1.2.1-beta").as_deref(), Some("26.1.2"));
        assert_eq!(
            neoforge_game("0.25w14craftmine.5-beta").as_deref(),
            Some("25w14craftmine")
        );
        assert!(neoforge_game("bad").is_none());
    }
}
