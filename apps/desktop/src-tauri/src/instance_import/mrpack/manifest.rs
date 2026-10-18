//! Bounded v1 index schema, exact client choices and installer capability hints.
//! URLs/hashes remain private native declarations. No mirror is contacted.
use super::*;
use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize,
};
use std::fmt;

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Client {
    #[default]
    Required,
    Optional,
    Unsupported,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Environment {
    #[serde(default)]
    pub client: Client,
    #[serde(default)]
    server: Client,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Manifest {
    format_version: u32,
    game: String,
    pub version_id: String,
    pub name: String,
    pub summary: Option<String>,
    pub files: Vec<RemoteFile>,
    pub dependencies: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RemoteFile {
    pub path: String,
    pub hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub env: Environment,
    pub downloads: Vec<String>,
    pub file_size: u64,
}

fn text(value: &str, limit: usize, allow_lines: bool) -> Result<()> {
    if value.len() > limit
        || value.is_empty()
        || value
            .chars()
            .any(|c| c.is_control() && !(allow_lines && matches!(c, '\n' | '\r' | '\t')))
    {
        Err("mrpack 文本字段无效或超过大小限制".into())
    } else {
        Ok(())
    }
}
pub(super) fn output_path(path: &str) -> Result<()> {
    let parts = super::super::relative(path)?;
    if super::super::forbidden(path)
        || matches!(
            parts[0].to_ascii_lowercase().as_str(),
            "versions"
                | "libraries"
                | "assets"
                | "runtime"
                | "runtimes"
                | "natives"
                | "bin"
                | ".minecraft"
        )
        || matches!(
            path.to_ascii_lowercase().as_str(),
            "version.json" | "minecraft.jar" | "client.jar" | "server.jar"
        )
    {
        return Err("mrpack 包含启动器或游戏核心保留输出路径".into());
    }
    Ok(())
}
fn url(value: &str) -> Result<bool> {
    super::mirror::declaration(value).map(|url| url.is_some())
}
impl RemoteFile {
    pub(super) fn has_supported_mirror(&self, local: bool) -> bool {
        self.downloads.iter().any(|value| {
            if local {
                super::mirror::local_declaration(value)
            } else {
                super::mirror::declaration(value)
            }
            .is_ok_and(|url| url.is_some())
        })
    }
}
pub(super) fn parse(bytes: &[u8]) -> Result<Manifest> {
    // Validate decoded keys before any Value/map conversion. This includes
    // escaped-equivalent keys recursively inside env, hashes and dependencies.
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    UniqueJson::deserialize(&mut parser).map_err(|_| "mrpack 清单 JSON 无效、键重复或嵌套过深")?;
    parser.end().map_err(|_| "mrpack 清单包含 JSON 后续内容")?;
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|_| "mrpack v1 清单字段、类型或枚举无效")?;
    if manifest.format_version != 1 || manifest.game != "minecraft" {
        return Err("仅支持 Minecraft 的 mrpack 格式版本 1".into());
    }
    text(&manifest.name, 512, false)?;
    text(&manifest.version_id, 256, false)?;
    if let Some(summary) = &manifest.summary {
        if !summary.is_empty() {
            text(summary, 8192, true)?;
        }
    }
    if manifest.files.len() > MAX_ENTRIES
        || manifest.dependencies.is_empty()
        || manifest.dependencies.len() > 16
        || !manifest.dependencies.contains_key("minecraft")
    {
        return Err("mrpack 文件/依赖数量无效，或缺少 minecraft 依赖".into());
    }
    let mut paths = BTreeSet::new();
    let mut total = 0u64;
    for file in &manifest.files {
        output_path(&file.path)?;
        if !paths.insert(file.path.clone()) {
            return Err("mrpack 清单包含重复文件路径".into());
        }
        if file.hashes.len() > 8 {
            return Err("mrpack 文件哈希种类超过限制".into());
        }
        for (key, hash) in &file.hashes {
            text(key, 32, false)?;
            text(hash, 512, false)?;
        }
        for (key, length) in [("sha1", 40), ("sha512", 128)] {
            if file.hashes.get(key).is_none_or(|hash| {
                hash.len() != length || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            }) {
                return Err("mrpack 文件必须提供有效 SHA1 与 SHA512".into());
            }
        }
        if file.file_size > 2 * 1024 * 1024 * 1024 || file.downloads.len() > 16 {
            return Err("mrpack 单文件大小或镜像数量超过限制".into());
        }
        total = total
            .checked_add(file.file_size)
            .filter(|s| *s <= MAX_OUTPUT)
            .ok_or("mrpack 清单总文件大小超过 8 GiB 限制")?;
        for download in &file.downloads {
            url(download)?;
        }
        // Both environment values are parsed strictly even for excluded files.
        let _ = file.env.server;
    }
    super::archive::validate_outputs(&paths, &BTreeSet::new())?;
    for (id, version) in &manifest.dependencies {
        text(id, 64, false)?;
        text(version, 160, false)?;
        super::super::component(id)?;
        pcl_core::identifier(version)?;
    }
    Ok(manifest)
}
fn release(value: &str) -> Option<(u32, u32, u32)> {
    let parts = value
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    Some((parts[0], parts[1], *parts.get(2).unwrap_or(&0)))
}
fn supported(id: &str, version: &str, minecraft: &str) -> bool {
    match id {
        "minecraft" => true,
        "fabric-loader" => semver::Version::parse(version).is_ok(),
        "forge" => {
            release(minecraft).is_some_and(|v| v >= (1, 12, 2)) && {
                let numbers = version
                    .split('.')
                    .map(str::parse::<u32>)
                    .collect::<std::result::Result<Vec<_>, _>>();
                numbers.is_ok_and(|v| (3..=4).contains(&v.len()) && v[0] >= 14)
            }
        }
        "neoforge" => release(minecraft).is_some_and(|mc| {
            let (core, suffix) = version
                .split_once('-')
                .map_or((version, None), |(core, suffix)| (core, Some(suffix)));
            if suffix.is_some_and(|s| !matches!(s, "beta" | "alpha")) {
                return false;
            }
            let numbers = core
                .split('.')
                .map(str::parse::<u32>)
                .collect::<std::result::Result<Vec<_>, _>>();
            numbers.is_ok_and(|v| {
                if !(3..=4).contains(&v.len()) {
                    return false;
                }
                // Same mapping as the existing official catalog: 47.x is the
                // old 1.20.1 line, 20/21 map to 1.minor.patch, and 26+ use the
                // new major/minor/patch game scheme (zero patch is omitted).
                (mc == (1, 20, 1) && v[0] == 47)
                    || (matches!(v[0], 20 | 21) && mc == (1, v[0], v[1]))
                    || (v[0] >= 26 && mc == (v[0], v[1], v[2]))
            })
        }),
        _ => false,
    }
}
pub(super) fn dependencies(
    declarations: &BTreeMap<String, String>,
) -> (Vec<PreviewDependency>, Vec<String>) {
    let mut blockers = Vec::new();
    let mut values = Vec::new();
    for (id, version) in declarations {
        let supported = supported(id, version, &declarations["minecraft"]);
        if !supported {
            blockers.push(format!("依赖 {id} {version} 暂不受当前安装器支持"));
        }
        values.push(PreviewDependency {
            id: id.clone(),
            version: version.clone(),
            supported,
        });
    }
    if declarations
        .keys()
        .filter(|id| id.as_str() != "minecraft")
        .count()
        > 1
    {
        blockers.push("整合包声明多个加载器/组件，当前安装器仅支持一个加载器".into());
    }
    (values, blockers)
}

struct UniqueJson;
impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}
struct UniqueVisitor;
impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueJson;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded JSON with unique keys")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<UniqueJson, M::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > 256 || keys.len() >= 128 || !keys.insert(key) {
                return Err(serde::de::Error::custom("duplicate or excessive keys"));
            }
            map.next_value::<UniqueJson>()?;
        }
        Ok(UniqueJson)
    }
    fn visit_seq<S: SeqAccess<'de>>(
        self,
        mut sequence: S,
    ) -> std::result::Result<UniqueJson, S::Error> {
        while sequence.next_element::<UniqueJson>()?.is_some() {}
        Ok(UniqueJson)
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<UniqueJson, E> {
        Ok(UniqueJson)
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<UniqueJson, E> {
        Ok(UniqueJson)
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<UniqueJson, E> {
        Ok(UniqueJson)
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<UniqueJson, E> {
        Ok(UniqueJson)
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<UniqueJson, E> {
        Ok(UniqueJson)
    }
    fn visit_none<E: serde::de::Error>(self) -> std::result::Result<UniqueJson, E> {
        Ok(UniqueJson)
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<UniqueJson, E> {
        Ok(UniqueJson)
    }
}
