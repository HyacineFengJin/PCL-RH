//! Source-aware resource details. Provider IDs never cross the download API
//! boundary. Encyclopedia links can resolve a slug into an authoritative ID;
//! ambiguous relations remain encyclopedia information, not identity aliases.
use crate::{
    community_curseforge::CurseforgeClient,
    community_identity::{merge_identities, AliasTarget, ProjectKind},
    community_mcmod::{McmodClient, McmodDetail},
    resource_details,
};
use serde_json::{json, Value};
use std::time::Duration;

#[tauri::command]
pub async fn community_details(
    source: String,
    project_id: String,
    project_type: Option<String>,
) -> Result<Value, String> {
    match source.as_str() {
        "Modrinth" => {
            let details = resource_details::resource_details(project_id).await?;
            let mut value = serde_json::to_value(details).map_err(|_| "无法读取资源详情")?;
            add_primary(&mut value, "modrinth");
            Ok(value)
        }
        "CurseForge" => {
            let mut value = CurseforgeClient::new(pcl_network::snapshot())?
                .details(&project_id)
                .await?;
            add_primary(&mut value, "curseforge");
            Ok(value)
        }
        "MC百科" => {
            let kind = match project_type.as_deref().unwrap_or("mod") {
                "mod" => ProjectKind::Mod,
                "modpack" => ProjectKind::Modpack,
                _ => return Err("MC百科当前支持模组与整合包资料".into()),
            };
            let network = pcl_network::snapshot();
            let detail = McmodClient::new(network.clone())?
                .detail(&project_id, kind)
                .await?;
            let mut value = encyclopedia_details(&detail)?;
            // At most one link per file provider survives ambiguity checks.
            // Both reads share a deadline; failures retain the original web
            // link rather than substituting an unrelated search result.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
            let links = value["sources"].as_array_mut().ok_or("百科来源列表无效")?;
            for link in links {
                if !link["project_id"].is_null() {
                    continue;
                }
                let Some(url) = link["url"].as_str().map(str::to_owned) else {
                    continue;
                };
                let result = match link["source"].as_str() {
                    Some("Modrinth") => tokio::time::timeout_at(
                        deadline,
                        resource_details::project_for_link(&url, &network),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok),
                    Some("CurseForge") => match CurseforgeClient::new(network.clone()) {
                        Ok(client) => {
                            tokio::time::timeout_at(deadline, client.project_by_url(&url, kind))
                                .await
                                .ok()
                                .and_then(Result::ok)
                        }
                        Err(_) => None,
                    },
                    _ => None,
                };
                if let Some(project) = result {
                    link["project_id"] = project["project_id"].clone()
                }
            }
            value["project"]["sources"] = value["sources"].clone();
            Ok(value)
        }
        _ => Err("不支持的社区来源".into()),
    }
}

fn add_primary(value: &mut Value, prefix: &str) {
    let project = &value["project"];
    let identity_key = format!(
        "{prefix}:{}",
        project["project_id"].as_str().unwrap_or_default()
    );
    let sources = json!([{"source": project["source"], "project_id":project["project_id"],"url":project["url"]}]);
    value["identity_key"] = json!(identity_key);
    value["sources"] = sources.clone();
    value["project"]["identity_key"] = json!(identity_key);
    value["project"]["sources"] = sources;
}

fn encyclopedia_details(detail: &McmodDetail) -> Result<Value, String> {
    let kind = if detail.project.kind == ProjectKind::Modpack {
        "modpack"
    } else {
        "mod"
    };
    let route = if kind == "modpack" {
        "modpack"
    } else {
        "class"
    };
    let identity = format!("mcmod:{route}/{}", detail.project.id);
    let mut sources =
        vec![json!({"source":"MC百科","project_id":detail.project.id,"url":detail.url})];
    if merge_identities(&[detail.identity()])?.conflicts.is_empty() {
        for alias in &detail.aliases {
            let AliasTarget::Url(url) = &alias.target else {
                continue;
            };
            let source = if url.starts_with("https://modrinth.com/") {
                "Modrinth"
            } else if url.starts_with("https://www.curseforge.com/") {
                "CurseForge"
            } else {
                continue;
            };
            sources.push(json!({"source":source,"project_id":Value::Null,"url":url}));
        }
    }
    Ok(json!({
        "project": {
            "project_id":detail.project.id,"title":detail.title,"description":detail.summary,
            "icon_url":Value::Null,"categories":[],"display_categories":[],"game_versions":[],
            "loaders":[],"downloads":0,"date_modified":"","project_type":kind,
            "source":"MC百科","url":detail.url,"sources":sources,"identity_key":identity,
            "attribution":detail.attribution,
        },
        "versions":[],"versions_truncated":false,"sources":sources,"identity_key":identity,
        "attribution":detail.attribution,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community_identity::{AliasEvidence, CommunitySource, ProjectKey};

    fn detail(urls: &[&str]) -> McmodDetail {
        let url = "https://www.mcmod.cn/class/123.html";
        McmodDetail {
            project: ProjectKey::new(CommunitySource::Mcmod, ProjectKind::Mod, "123").unwrap(),
            title: "Example".into(),
            secondary_title: None,
            summary: "Summary".into(),
            url: url.into(),
            aliases: urls
                .iter()
                .map(|target| AliasEvidence {
                    target: AliasTarget::Url((*target).into()),
                    evidence_url: url.into(),
                })
                .collect(),
            attribution: "MC百科",
        }
    }

    #[test]
    fn single_known_link_is_an_unresolved_source_not_an_invented_id() {
        let value = encyclopedia_details(&detail(&["https://modrinth.com/mod/example"])).unwrap();
        assert_eq!(value["sources"].as_array().unwrap().len(), 2);
        assert!(value["sources"][1]["project_id"].is_null());
        assert_eq!(value["versions"], json!([]));
    }

    #[test]
    fn encyclopedia_covering_multiple_projects_does_not_offer_identity_switches() {
        let value = encyclopedia_details(&detail(&[
            "https://modrinth.com/mod/example",
            "https://modrinth.com/mod/example-fork",
        ]))
        .unwrap();
        assert_eq!(value["sources"].as_array().unwrap().len(), 1);
    }
}
