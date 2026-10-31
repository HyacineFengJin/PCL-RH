//! Read-only instance/resource scope capture. File descriptors, complete SHA512
//! fingerprints and inheritance JSONs bind a plan to the actual target files.
use super::*;
use sha2::{Digest, Sha512};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::atomic::Ordering,
};

const KINDS: [&str; 3] = ["mods", "resourcepacks", "shaderpacks"];
const MAX_LOCAL_FILES: usize = 512;
#[cfg(target_os = "linux")]
#[path = "target/linux.rs"]
mod filesystem;
#[cfg(not(target_os = "linux"))]
#[path = "target/native.rs"]
mod filesystem;
use filesystem::Other;
pub(super) use filesystem::{Dir, Key};
pub(super) fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
#[cfg(unix)]
fn stamp(m: &std::fs::Metadata) -> (u64, u64, u64, u32, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mode(),
        m.nlink(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
#[cfg(unix)]
pub(super) fn fingerprint(
    file: &mut File,
    cancel: &AtomicBool,
) -> Result<(u64, String, String, String)> {
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.nlink() != 1 || before.len() > MAX_FILE_BYTES {
        return Err("资源文件不安全或超过大小限制".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut digest = Sha512::new();
    let mut count = 0;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        cancelled(cancel)?;
        let len = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if len == 0 {
            break;
        }
        count += len as u64;
        if count > before.len() {
            return Err("资源文件读取期间变化".into());
        }
        digest.update(&buffer[..len]);
    }
    if count != before.len()
        || stamp(&file.metadata().map_err(|e| e.to_string())?) != stamp(&before)
    {
        return Err("资源文件读取期间变化".into());
    }
    let sha512 = format!("{:x}", digest.finalize());
    let token = serde_json::to_vec(&(stamp(&before), &sha512)).map_err(|e| e.to_string())?;
    // Keep the resource panel's opaque token exactly compatible with
    // resource_ops::fingerprint, while retaining the stronger content token
    // for snapshot rechecks. Both describe the same pre/post-verified FD.
    let resource_token = format!(
        "v2:{}:{}:{}:{}:{}:{}:{}",
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.dev(),
        before.ino(),
        before.ctime(),
        before.ctime_nsec()
    );
    Ok((
        count,
        sha512,
        format!("{:x}", Sha512::digest(token)),
        resource_token,
    ))
}

#[cfg(windows)]
pub(super) fn fingerprint(
    file: &mut File,
    cancel: &AtomicBool,
) -> Result<(u64, String, String, String)> {
    let before = pcl_core::retained_fs::metadata(file).map_err(|e| e.to_string())?;
    if before.kind != pcl_core::retained_fs::Kind::Regular
        || before.reparse_tag.is_some()
        || before.links != 1
        || before.len > MAX_FILE_BYTES
    {
        return Err("资源文件不安全或超过大小限制".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = Sha512::new();
    let mut size = 0u64;
    let mut buf = [0u8; 128 * 1024];
    loop {
        cancelled(cancel)?;
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .filter(|n| *n <= before.len)
            .ok_or("资源文件读取期间变化")?;
        hash.update(&buf[..n]);
    }
    if size != before.len
        || pcl_core::retained_fs::metadata(file).map_err(|e| e.to_string())? != before
    {
        return Err("资源文件读取期间变化".into());
    }
    let sha512 = format!("{:x}", hash.finalize());
    let strong = serde_json::to_vec(&(&before, &sha512)).map_err(|e| e.to_string())?;
    Ok((
        size,
        sha512,
        format!("{:x}", Sha512::digest(strong)),
        crate::resource_ops::token_for(file)?,
    ))
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct Profile {
    id: String,
    directory: Key,
    fingerprint: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct ResourceView {
    directory: Option<Key>,
    others: BTreeMap<String, Other>,
}
#[derive(Clone, Debug, Serialize)]
pub struct TargetSnapshot {
    pub(crate) isolation: pcl_core::content_scope::IsolationPolicy,
    pub(crate) settings_revision: String,
    pub root_id: String,
    pub instance_id: String,
    pub minecraft_version: String,
    pub loader: String,
    pub(super) root: PathBuf,
    pub(super) project: PathBuf,
    pub(super) root_key: Key,
    pub(super) project_key: Key,
    pub(super) compatibility: Compatibility,
    pub(super) local_files: Vec<LocalFile>,
    profiles: Vec<Profile>,
    isolated: bool,
    views: BTreeMap<String, ResourceView>,
}
impl TargetSnapshot {
    pub(super) fn heap_bytes(&self) -> usize {
        use super::allocation::*;
        text(&self.settings_revision)
            + text(&self.root_id)
            + text(&self.instance_id)
            + text(&self.minecraft_version)
            + text(&self.loader)
            + self.root.capacity()
            + self.project.capacity()
            + text(&self.compatibility.minecraft_version)
            + text(&self.compatibility.loader)
            + vector(&self.compatibility.shader_engines, text)
            + vector(&self.local_files, local)
            + vector(&self.profiles, |profile| {
                text(&profile.id) + text(&profile.fingerprint)
            })
            + map(&self.views, text, |view| map(&view.others, text, |_| 0))
    }
    /// Queued confirmation may tolerate inventory additions, never a different
    /// root/profile, deletion, rename, enable toggle or same-byte replacement
    /// inode. A previously absent resource directory may be safely created by
    /// an earlier admitted publisher; an existing one keeps its physical key.
    pub(super) fn permits_inventory_extension(&self, current: &Self) -> bool {
        self.isolation == current.isolation
            && self.settings_revision == current.settings_revision
            && self.root_id == current.root_id
            && self.instance_id == current.instance_id
            && self.root == current.root
            && self.project == current.project
            && self.root_key == current.root_key
            && self.project_key == current.project_key
            && self.compatibility == current.compatibility
            && self.profiles == current.profiles
            && self.isolated == current.isolated
            && self
                .local_files
                .iter()
                .all(|old| current.local_files.contains(old))
            && self.views.iter().all(|(kind, old)| {
                current.views.get(kind).is_some_and(|new| {
                    old.others == new.others
                        && (old.directory.is_none() || old.directory == new.directory)
                })
            })
    }
}
fn profiles(root: &Dir, id: &str, cancel: &AtomicBool) -> Result<Vec<Profile>> {
    let versions = root.child("versions")?;
    let mut profiles = Vec::new();
    let mut next = Some(id.to_owned());
    let mut seen = BTreeSet::new();
    while let Some(id) = next {
        provider::file_name(&id)?;
        if !seen.insert(id.clone()) || seen.len() > 64 {
            return Err("实例继承存在循环或超过深度限制".into());
        }
        let instance = versions.child(&id)?;
        let mut file = instance.regular(&format!("{id}.json"))?;
        if file.metadata().map_err(|e| e.to_string())?.len() > 8 * 1024 * 1024 {
            return Err("实例JSON超过安全上限".into());
        }
        let (_, _, fingerprint, _) = fingerprint(&mut file, cancel)?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "实例JSON损坏")?;
        if !value.is_object() || value["id"].as_str() != Some(id.as_str()) {
            return Err("实例JSON标识与目录不符".into());
        }
        next = match value.get("inheritsFrom") {
            Some(v) => Some(v.as_str().ok_or("实例继承标识无效")?.into()),
            None => None,
        };
        profiles.push(Profile {
            id,
            directory: instance.key()?,
            fingerprint,
        });
    }
    Ok(profiles)
}
#[cfg(test)]
pub(super) fn capture(
    root: &Path,
    project: &Path,
    root_id: &str,
    id: &str,
    cancel: &AtomicBool,
) -> Result<TargetSnapshot> {
    capture_with_isolation(
        root,
        project,
        root_id,
        id,
        cancel,
        pcl_core::content_scope::IsolationPolicy::Automatic,
        "",
    )
}
pub(super) fn capture_with_isolation(
    root: &Path,
    project: &Path,
    root_id: &str,
    id: &str,
    cancel: &AtomicBool,
    isolation: pcl_core::content_scope::IsolationPolicy,
    settings_revision: &str,
) -> Result<TargetSnapshot> {
    cancelled(cancel)?;
    provider::file_name(id)?;
    if root_id.is_empty()
        || root_id.len() > 128
        || !root_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("游戏目录标识无效".into());
    }
    let root_path = root.canonicalize().map_err(|e| e.to_string())?;
    let project = project.canonicalize().map_err(|e| e.to_string())?;
    let root = Dir::open(&root_path)?;
    let project_dir = Dir::open(&project)?;
    let initial_profiles = profiles(&root, id, cancel)?;
    let instance = pcl_core::scan_instance_with_isolation(&root_path, id, isolation)?
        .ok_or("未找到所选实例")?;
    if profiles(&root, id, cancel)? != initial_profiles {
        return Err("实例JSON在读取期间变化，请重新检查".into());
    }
    let loader = match instance.loader.split_whitespace().next().unwrap_or("") {
        "Fabric" => "fabric",
        "Quilt" => "quilt",
        "Forge" => "forge",
        "NeoForge" => "neoforge",
        "Vanilla" => "minecraft",
        _ => return Err("所选实例加载器暂不支持资源兼容性检查".into()),
    }
    .to_owned();
    let base = if instance.isolated {
        root.child("versions")?.child(id)?
    } else {
        Dir::open(&root_path)?
    };
    let mut local_files = Vec::new();
    let mut views = BTreeMap::new();
    let mut bytes = 0u64;
    for kind in KINDS {
        let dir = base.optional(kind)?;
        let mut view = ResourceView {
            directory: dir.as_ref().map(|d| d.key()).transpose()?,
            others: BTreeMap::new(),
        };
        if let Some(dir) = dir {
            for name in dir.names()? {
                cancelled(cancel)?;
                let stat = dir.stat(&name)?.ok_or("资源目录在读取期间变化")?;
                let eligible = if kind == "mods" {
                    name.ends_with(".jar") || name.ends_with(".jar.disabled")
                } else {
                    name.ends_with(".zip")
                };
                if eligible && filesystem::is_regular(&stat) {
                    if local_files.len() >= MAX_LOCAL_FILES {
                        return Err("本地资源文件超过512项，请先减少文件后下载".into());
                    }
                    let mut file = dir.regular(&name)?;
                    let (size, sha512, fingerprint, resource_fingerprint) =
                        fingerprint(&mut file, cancel)?;
                    bytes = bytes.checked_add(size).ok_or("本地资源总大小过大")?;
                    if bytes > 16 * 1024 * 1024 * 1024 {
                        return Err("本地资源总大小超过兼容性检查上限".into());
                    }
                    if Key::from_file(&dir.regular(&name)?)? != Key::from_file(&file)? {
                        return Err("本地资源文件路径已被替换".into());
                    }
                    local_files.push(LocalFile {
                        kind: kind.into(),
                        file_name: name.clone(),
                        size,
                        sha512,
                        enabled: !name.ends_with(".disabled"),
                        fingerprint,
                        resource_fingerprint,
                    });
                } else {
                    view.others.insert(name, filesystem::other(&stat));
                }
            }
        }
        views.insert(kind.into(), view);
    }
    let compatibility = Compatibility {
        minecraft_version: instance.minecraft_version.clone(),
        loader: loader.clone(),
        shader_engines: vec![],
    };
    let target = TargetSnapshot {
        isolation,
        settings_revision: settings_revision.into(),
        root_id: root_id.into(),
        instance_id: id.into(),
        minecraft_version: instance.minecraft_version,
        loader,
        root: root_path,
        project,
        root_key: root.key()?,
        project_key: project_dir.key()?,
        compatibility,
        local_files,
        profiles: initial_profiles,
        isolated: instance.isolated,
        views,
    };
    if Dir::open(&target.root)?.key() != Ok(target.root_key.clone())
        || Dir::open(&target.project)?.key() != Ok(target.project_key.clone())
    {
        return Err("资源目标目录在检查期间被替换".into());
    }
    Ok(target)
}
pub(super) fn check(expected: &TargetSnapshot, cancel: &AtomicBool) -> Result<()> {
    let current = capture_with_isolation(
        &expected.root,
        &expected.project,
        &expected.root_id,
        &expected.instance_id,
        cancel,
        expected.isolation,
        &expected.settings_revision,
    )?;
    let views_match = expected.views.iter().all(|(kind, old)| {
        current
            .views
            .get(kind)
            .is_some_and(|new| old.others == new.others && old.directory == new.directory)
    });
    if expected.root_key != current.root_key
        || expected.project_key != current.project_key
        || expected.compatibility != current.compatibility
        || expected.profiles != current.profiles
        || expected.isolated != current.isolated
        || expected.local_files != current.local_files
        || !views_match
    {
        return Err("目标实例、资源目录或已装文件已变化，请重新检查后下载".into());
    }
    Ok(())
}
pub(super) fn existing_name(target: &TargetSnapshot, kind: &str, name: &str) -> bool {
    target
        .local_files
        .iter()
        .any(|f| f.kind == kind && f.file_name == name)
        || target
            .views
            .get(kind)
            .is_some_and(|v| v.others.contains_key(name))
}

#[cfg(all(test, target_os = "linux"))]
#[path = "target/staging_tests.rs"]
mod staging_tests;

#[cfg(test)]
#[path = "target/tests.rs"]
mod tests;
