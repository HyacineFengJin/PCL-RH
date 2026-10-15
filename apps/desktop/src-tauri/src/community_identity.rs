//! Conservative, batch-local identity matching for community catalog rows.
//!
//! Only a provider ID or a project URL observed in an identified page's links
//! can join rows. Names, authors, mod IDs and similar slugs are deliberately not
//! inputs. A connected component with multiple projects from one provider is
//! ambiguous as a whole: rejecting all of its cross-provider joins prevents a
//! broad encyclopedia entry from swallowing loader variants or forks.

use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ROWS: usize = 256;
const MAX_ALIASES: usize = 32;
const MAX_URL_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum CommunitySource {
    Modrinth,
    Curseforge,
    Mcmod,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Mod,
    Modpack,
    Resourcepack,
    Shader,
    Datapack,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectKey {
    pub source: CommunitySource,
    pub kind: ProjectKind,
    pub id: String,
}

impl ProjectKey {
    pub fn new(
        source: CommunitySource,
        kind: ProjectKind,
        id: impl Into<String>,
    ) -> Result<Self, String> {
        let key = Self {
            source,
            kind,
            id: id.into(),
        };
        key.validate()?;
        Ok(key)
    }

    fn validate(&self) -> Result<(), String> {
        let valid = match self.source {
            CommunitySource::Modrinth => {
                !self.id.is_empty()
                    && self.id.len() <= 64
                    && self.id.bytes().all(|ch| ch.is_ascii_alphanumeric())
            }
            CommunitySource::Curseforge | CommunitySource::Mcmod => self
                .id
                .parse::<u64>()
                .is_ok_and(|id| id > 0 && id.to_string() == self.id),
        };
        if valid {
            Ok(())
        } else {
            Err("社区项目 ID 无效".into())
        }
    }
}

/// The caller obtains these targets from a provider's structured metadata or
/// the related-links section of a fetched project page, never from description
/// text or search snippets. `evidence_url` must identify the owning row's page.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AliasEvidence {
    pub target: AliasTarget,
    pub evidence_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AliasTarget {
    Project(ProjectKey),
    Url(String),
}

#[derive(Clone, Debug)]
pub struct ProjectIdentity {
    pub project: ProjectKey,
    /// From this project's own provider response, not guessed from its name.
    pub canonical_url: String,
    pub aliases: Vec<AliasEvidence>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct IdentityConflict {
    pub indices: Vec<usize>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct IdentityResolution {
    /// Stable input indices; the caller retains the original per-source DTOs.
    pub groups: Vec<Vec<usize>>,
    pub conflicts: Vec<IdentityConflict>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectUrl {
    source: CommunitySource,
    kind: ProjectKind,
    url: String,
    id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Token {
    Id(ProjectKey),
    Url(ProjectUrl),
}

impl ProjectUrl {
    fn token(&self) -> Token {
        if let Some(id) = &self.id {
            Token::Id(ProjectKey {
                source: self.source,
                kind: self.kind,
                id: id.clone(),
            })
        } else {
            Token::Url(self.clone())
        }
    }
}

/// Normalize only exact public project-page routes. No URL is fetched here.
/// Restricting routes also keeps file, version, gallery and dependency links
/// from being promoted to project-equivalence evidence.
pub fn canonical_project_url(raw: &str) -> Option<String> {
    parse_project_url(raw).map(|url| url.url)
}

fn parse_project_url(raw: &str) -> Option<ProjectUrl> {
    if raw.len() > MAX_URL_BYTES || raw.chars().any(char::is_control) {
        return None;
    }
    let parsed = Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
    {
        return None;
    }
    let host = parsed.host_str()?;
    let segments: Vec<_> = parsed.path().trim_matches('/').split('/').collect();
    let (source, kind, path, id) = match host {
        "modrinth.com" | "www.modrinth.com" => {
            let [route, name] = segments.as_slice() else {
                return None;
            };
            let kind = match *route {
                "mod" => ProjectKind::Mod,
                "modpack" => ProjectKind::Modpack,
                "resourcepack" => ProjectKind::Resourcepack,
                "shader" => ProjectKind::Shader,
                "datapack" => ProjectKind::Datapack,
                _ => return None,
            };
            if !valid_slug(name) {
                return None;
            }
            // Modrinth accepts IDs and slugs in the same route. Treating any
            // eight-character word as an ID would create accidental matches.
            (
                CommunitySource::Modrinth,
                kind,
                format!("/{route}/{name}"),
                None,
            )
        }
        "curseforge.com" | "www.curseforge.com" => {
            let ["minecraft", route, name] = segments.as_slice() else {
                return None;
            };
            let kind = match *route {
                "mc-mods" => ProjectKind::Mod,
                "modpacks" => ProjectKind::Modpack,
                "texture-packs" => ProjectKind::Resourcepack,
                "shaders" => ProjectKind::Shader,
                "data-packs" => ProjectKind::Datapack,
                _ => return None,
            };
            if !valid_slug(name) {
                return None;
            }
            (
                CommunitySource::Curseforge,
                kind,
                format!("/minecraft/{route}/{name}"),
                None,
            )
        }
        "mcmod.cn" | "www.mcmod.cn" | "m.mcmod.cn" => {
            let [route, file] = segments.as_slice() else {
                return None;
            };
            let kind = match *route {
                "class" => ProjectKind::Mod,
                "modpack" => ProjectKind::Modpack,
                _ => return None,
            };
            let number = file.strip_suffix(".html")?.parse::<u64>().ok()?;
            if number == 0 {
                return None;
            }
            (
                CommunitySource::Mcmod,
                kind,
                format!("/{route}/{number}.html"),
                Some(number.to_string()),
            )
        }
        _ => return None,
    };
    let host = match source {
        CommunitySource::Modrinth => "modrinth.com",
        CommunitySource::Curseforge => "www.curseforge.com",
        CommunitySource::Mcmod => "www.mcmod.cn",
    };
    Some(ProjectUrl {
        source,
        kind,
        url: format!("https://{host}{path}"),
        id,
    })
}

fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
}

/// Resolve an already bounded search batch. A conflict rejects cross-provider
/// joins for the entire candidate component, independently of input order.
/// Duplicate rows with the exact same provider ID still collapse safely.
pub fn merge_identities(rows: &[ProjectIdentity]) -> Result<IdentityResolution, String> {
    if rows.len() > MAX_ROWS {
        return Err("社区合并项目数量超过上限".into());
    }
    let mut row_tokens = Vec::with_capacity(rows.len());
    let mut owners: BTreeMap<Token, BTreeSet<ProjectKey>> = BTreeMap::new();
    for row in rows {
        row.project.validate()?;
        if row.aliases.len() > MAX_ALIASES {
            return Err("社区项目关联数量超过上限".into());
        }
        let page = parse_project_url(&row.canonical_url).ok_or("社区项目的来源页面无效")?;
        if page.source != row.project.source
            || page.kind != row.project.kind
            || page.id.as_ref().is_some_and(|id| id != &row.project.id)
        {
            return Err("社区项目 ID 与来源页面不一致".into());
        }
        let mut tokens = BTreeSet::from([Token::Id(row.project.clone()), page.token()]);
        owners
            .entry(page.token())
            .or_default()
            .insert(row.project.clone());
        for alias in &row.aliases {
            if parse_project_url(&alias.evidence_url).as_ref() != Some(&page) {
                return Err("社区项目关联缺少所属页面证据".into());
            }
            let token = match &alias.target {
                AliasTarget::Project(project) => {
                    project.validate()?;
                    Token::Id(project.clone())
                }
                AliasTarget::Url(url) => parse_project_url(url)
                    .ok_or("社区项目关联链接无效")?
                    .token(),
            };
            tokens.insert(token);
        }
        row_tokens.push(tokens);
    }

    let mut parents: Vec<_> = (0..rows.len()).collect();
    let mut first_seen = BTreeMap::new();
    for (index, tokens) in row_tokens.iter().enumerate() {
        for token in tokens {
            if let Some(previous) = first_seen.insert(token.clone(), index) {
                let previous_root = root(&mut parents, previous);
                let current_root = root(&mut parents, index);
                parents[current_root] = previous_root;
            }
        }
    }
    let mut candidates: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..rows.len() {
        candidates
            .entry(root(&mut parents, index))
            .or_default()
            .push(index);
    }
    let mut resolution = IdentityResolution {
        groups: Vec::new(),
        conflicts: Vec::new(),
    };
    for indices in candidates.into_values() {
        let mut claims: BTreeMap<CommunitySource, BTreeSet<Token>> = BTreeMap::new();
        let mut kinds = BTreeSet::new();
        for &index in &indices {
            for token in &row_tokens[index] {
                let equivalents = owners.get(token);
                if let Some(keys) = equivalents {
                    for key in keys {
                        kinds.insert(key.kind);
                        claims
                            .entry(key.source)
                            .or_default()
                            .insert(Token::Id(key.clone()));
                    }
                } else {
                    let (source, kind) = match token {
                        Token::Id(key) => (key.source, key.kind),
                        Token::Url(url) => (url.source, url.kind),
                    };
                    kinds.insert(kind);
                    claims.entry(source).or_default().insert(token.clone());
                }
            }
        }
        if kinds.len() > 1 || claims.values().any(|projects| projects.len() > 1) {
            resolution.conflicts.push(IdentityConflict {
                indices: indices.clone(),
                reason: "关联证据包含多个同站项目或不同资源类型，已保留独立条目".into(),
            });
            let mut duplicates: BTreeMap<ProjectKey, Vec<usize>> = BTreeMap::new();
            for index in indices {
                duplicates
                    .entry(rows[index].project.clone())
                    .or_default()
                    .push(index);
            }
            resolution.groups.extend(duplicates.into_values());
        } else {
            resolution.groups.push(indices);
        }
    }
    resolution.groups.sort_by_key(|group| group[0]);
    resolution
        .conflicts
        .sort_by_key(|conflict| conflict.indices[0]);
    Ok(resolution)
}

fn root(parents: &mut [usize], index: usize) -> usize {
    if parents[index] != index {
        parents[index] = root(parents, parents[index]);
    }
    parents[index]
}

#[cfg(test)]
#[path = "community_identity/tests.rs"]
mod tests;
