//! Renderer selections carry component and manifest identity, never URLs.
//! Both the file tree and links are validated as a complete bounded plan before
//! any executable is downloaded or any local runtime directory is populated.
use super::{network::Http, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::atomic::AtomicBool,
};
pub const MAX_METADATA: u64 = 2 * 1024 * 1024;
pub const ALL:&str="https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Package {
    pub component: String,
    pub version: String,
    pub major: u32,
    pub platform: String,
    pub sha1: String,
    pub released: String,
    #[serde(skip_serializing)]
    pub manifest: Download,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Download {
    pub url: String,
    pub sha1: String,
    pub size: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Entry {
    Directory,
    File {
        executable: bool,
        downloads: BTreeMap<String, Download>,
    },
    Link {
        target: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Manifest {
    pub files: BTreeMap<String, Entry>,
}
#[derive(Deserialize)]
struct Version {
    name: String,
    released: String,
}
#[derive(Deserialize)]
struct Available {
    manifest: Download,
    version: Version,
}
pub fn platform() -> Result<&'static str> {
    pcl_core::platform::Platform::current()?
        .mojang_java_platform()
        .ok_or_else(|| "Mojang 当前未提供本平台的 Java 运行时，请手动添加系统 Java".into())
}
pub fn sha1(hash: &str) -> Result<()> {
    if hash.len() != 40
        || !hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        Err("Java SHA-1 无效".into())
    } else {
        Ok(())
    }
}
pub fn component(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 64
        || !s
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        Err("Java 组件名无效".into())
    } else {
        Ok(())
    }
}
fn major(s: &str) -> Result<u32> {
    let s = s.strip_prefix("1.").unwrap_or(s);
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse::<u32>()
        .ok()
        .filter(|n| (1..=999).contains(n))
        .ok_or_else(|| "无法识别 Java 官方版本号".into())
}
pub fn parse_catalog(body: &[u8], selected_platform: &str) -> Result<Vec<Package>> {
    if body.len() as u64 > MAX_METADATA {
        return Err("Java 目录超过限制".into());
    }
    let platforms: BTreeMap<String, BTreeMap<String, Vec<Available>>> =
        serde_json::from_slice(body).map_err(|e| format!("Java 官方目录无效：{e}"))?;
    let components = platforms
        .get(selected_platform)
        .ok_or("官方 Java 目录未包含本平台")?;
    let mut packages = Vec::new();
    let mut identities = BTreeSet::new();
    for (name, versions) in components {
        component(name)?;
        for version in versions {
            let manifest = &version.manifest;
            sha1(&manifest.sha1)?;
            if manifest.size == 0
                || manifest.size > MAX_METADATA
                || manifest.url
                    != format!(
                        "https://piston-meta.mojang.com/v1/packages/{}/manifest.json",
                        manifest.sha1
                    )
            {
                return Err("Java 官方清单地址或大小无效".into());
            }
            if version.version.name.len() > 128
                || version.version.released.len() > 128
                || !identities.insert((name, manifest.sha1.clone()))
            {
                return Err("Java 目录条目重复或过长".into());
            }
            packages.push(Package {
                component: name.clone(),
                version: version.version.name.clone(),
                major: major(&version.version.name)?,
                platform: selected_platform.into(),
                sha1: manifest.sha1.clone(),
                released: version.version.released.clone(),
                manifest: manifest.clone(),
            });
            if packages.len() > 64 {
                return Err("Java 目录条目超过限制".into());
            }
        }
    }
    packages.sort_by(|a, b| {
        b.major
            .cmp(&a.major)
            .then_with(|| b.released.cmp(&a.released))
            .then_with(|| {
                a.component
                    .ends_with("-snapshot")
                    .cmp(&b.component.ends_with("-snapshot"))
            })
            .then_with(|| b.component.cmp(&a.component))
    });
    // Several Mojang component channels can offer the same major version
    // (including a snapshot alias of the stable manifest). Offer the newest
    // runtime per major; component/hash still identify the exact chosen build.
    packages.dedup_by_key(|p| p.major);
    Ok(packages)
}
pub async fn list(http: &Http, cancel: &AtomicBool) -> Result<Vec<Package>> {
    parse_catalog(&http.metadata(ALL, None, cancel).await?, platform()?)
}
pub async fn selected(http: &Http, name: &str, hash: &str, cancel: &AtomicBool) -> Result<Package> {
    component(name)?;
    sha1(hash)?;
    list(http, cancel)
        .await?
        .into_iter()
        .find(|p| p.component == name && p.sha1 == hash)
        .ok_or_else(|| "Java 官方版本信息已改变，请刷新目录重新选择".into())
}
pub fn parts(path: &str) -> Result<Vec<&str>> {
    let parts: Vec<_> = path.split('/').collect();
    if path.len() > 4096
        || parts.len() > 24
        || parts.iter().any(|p| {
            p.is_empty()
                || matches!(*p, "." | "..")
                || p.len() > 255
                || p.contains('\\')
                || p.chars().any(char::is_control)
        })
    {
        return Err("Java 清单路径越界或无效".into());
    }
    Ok(parts)
}
fn link_destination(files: &BTreeMap<String, Entry>, path: &str, target: &str) -> Result<String> {
    if target.is_empty()
        || target.len() > 4096
        || target.starts_with('/')
        || target.contains('\\')
        || target.chars().any(char::is_control)
    {
        return Err("Java 内部链接无效".into());
    }
    let mut parent = parts(path)?;
    parent.pop();
    let pieces: Vec<_> = target.split('/').collect();
    for (index, part) in pieces.iter().copied().enumerate() {
        match part {
            ".." => {
                parent.pop().ok_or("Java 内部链接指向运行时之外")?;
            }
            "." => {}
            "" => return Err("Java 内部链接无效".into()),
            _ => {
                if part.len() > 255 {
                    return Err("Java 内部链接过长".into());
                }
                parent.push(part);
            }
        }
        // Normalizing `alias/..` lexically differs from filesystem traversal
        // when alias points to a shallower directory. Only real manifest
        // directories may precede another target component, including `..`.
        // Leaf links may still chain to an in-tree file or directory.
        if index + 1 < pieces.len()
            && !parent.is_empty()
            && !matches!(files.get(&parent.join("/")), Some(Entry::Directory))
        {
            return Err("Java 链接不能穿过其他链接或文件".into());
        }
    }
    let dest = parent.join("/");
    parts(&dest)?;
    Ok(dest)
}
impl Manifest {
    pub fn validate(&self) -> Result<u64> {
        if self.files.is_empty() || self.files.len() > 4096 {
            return Err("Java 清单文件数量无效".into());
        }
        let mut total = 0u64;
        for (path, entry) in &self.files {
            for part in parts(path)? {
                pcl_core::retained_fs::Leaf::new(part)
                    .map_err(|_| "Java 清单路径不适用于当前文件系统")?;
            }
            if path.starts_with('.') {
                return Err("Java 清单不能占用启动器内部文件名".into());
            }
            let mut parent = Path::new(path).parent();
            while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
                if !matches!(
                    self.files.get(p.to_str().ok_or("Java 路径不是 UTF-8")?),
                    Some(Entry::Directory)
                ) {
                    return Err("Java 文件父目录缺失、冲突或是链接".into());
                }
                parent = p.parent();
            }
            match entry {
                Entry::Directory => {}
                Entry::File { downloads, .. } => {
                    let raw = downloads.get("raw").ok_or("Java 文件缺少原始下载")?;
                    sha1(&raw.sha1)?;
                    if raw.size > 512 * 1024 * 1024
                        || !raw.url.starts_with(&format!(
                            "https://piston-data.mojang.com/v1/objects/{}/",
                            raw.sha1
                        ))
                    {
                        return Err("Java 文件官方地址或大小无效".into());
                    }
                    let url = reqwest::Url::parse(&raw.url).map_err(|_| "Java 文件地址无效")?;
                    let expected_prefix = format!("/v1/objects/{}/", raw.sha1);
                    let leaf = url.path().strip_prefix(&expected_prefix);
                    if !leaf.is_some_and(|s| !s.is_empty() && !s.contains('/') && parts(s).is_ok())
                        || url.query().is_some()
                        || url.fragment().is_some()
                        || url.port().is_some()
                        || !url.username().is_empty()
                        || url.password().is_some()
                    {
                        return Err("Java 文件地址包含额外授权字段".into());
                    }
                    total = total
                        .checked_add(raw.size)
                        .filter(|n| *n <= 1024 * 1024 * 1024)
                        .ok_or("Java 运行时超过 1GiB 限制")?;
                }
                Entry::Link { target } => {
                    let mut dest = link_destination(&self.files, path, target)?;
                    let mut seen = BTreeSet::from([path.clone()]);
                    loop {
                        if !seen.insert(dest.clone()) {
                            return Err("Java 内部链接循环".into());
                        }
                        match self.files.get(&dest) {
                            Some(Entry::Link { target }) => {
                                dest = link_destination(&self.files, &dest, target)?;
                            }
                            Some(_) => break,
                            None => return Err("Java 内部链接目标不存在".into()),
                        }
                        if seen.len() > 24 {
                            return Err("Java 内部链接层数过多".into());
                        }
                    }
                }
            }
        }
        self.java_entry()?;
        // NTFS commonly folds names. Reject portable ASCII aliases before any
        // bytes are transferred rather than rely on a late create collision.
        if cfg!(windows) {
            let mut names = BTreeSet::new();
            for (path, entry) in &self.files {
                if !names.insert(path.to_ascii_lowercase()) {
                    return Err("Java 清单路径在 Windows 上重复".into());
                }
                if matches!(entry, Entry::Link { .. }) {
                    return Err("Windows 官方 Java 清单不支持内部链接".into());
                }
            }
        }
        Ok(total)
    }
    pub fn java_entry(&self) -> Result<String> {
        let platform = pcl_core::platform::Platform::current()?;
        self.java_entry_for(platform)
    }
    pub fn java_entry_for(&self, platform: pcl_core::platform::Platform) -> Result<String> {
        use pcl_core::platform::OperatingSystem;
        for path in platform.java_paths(Path::new("")) {
            let path = path
                .to_str()
                .ok_or("Java 可执行路径无效")?
                .replace('\\', "/");
            if matches!(self.files.get(&path), Some(Entry::File { executable, .. })
                if *executable || platform.os == OperatingSystem::Windows)
            {
                return Ok(path);
            }
        }
        Err("Java 清单缺少当前平台的可执行文件".into())
    }
}
