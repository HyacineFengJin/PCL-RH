//! Read-only instance/resource scope capture. File descriptors, complete SHA512
//! fingerprints and inheritance JSONs bind a plan to the actual target files.
use super::*;
use sha2::{Digest, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::{self, Metadata},
    io::{Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, PathBuf},
    sync::atomic::Ordering,
};

const KINDS: [&str; 3] = ["mods", "resourcepacks", "shaderpacks"];
const MAX_LOCAL_FILES: usize = 512;
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(super) struct Key {
    pub dev: u64,
    pub ino: u64,
}
impl Key {
    pub fn of(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
        }
    }
}
pub(super) struct Dir(pub File);
fn c(name: &str) -> Result<CString> {
    CString::new(name).map_err(|_| "资源路径含无效字符".into())
}
fn error(message: &str) -> String {
    format!("{message}：{}", std::io::Error::last_os_error())
}
impl Dir {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err("资源目标必须是规范绝对路径".into());
        }
        let fd = unsafe {
            libc::open(
                c("/")?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(error("无法打开资源目标目录"));
        }
        let mut directory = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            if let Component::Normal(part) = part {
                let name = CString::new(part.as_bytes()).map_err(|e| e.to_string())?;
                let fd = unsafe {
                    libc::openat(
                        directory.0.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(error("资源目录祖先已变化或含符号链接"));
                }
                directory = Self(unsafe { File::from_raw_fd(fd) });
            }
        }
        Ok(directory)
    }
    fn open_child(&self, name: &str, flags: i32) -> Result<File> {
        provider::file_name(name)?;
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let how = OpenHow {
            flags: (flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
            mode: 0,
            resolve: 0x01 | 0x02 | 0x04 | 0x08,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            ) as i32
        };
        if fd < 0 {
            return Err(error("资源路径已变化、含链接或挂载点（需要Linux5.6以上）"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub fn child(&self, name: &str) -> Result<Self> {
        self.open_child(name, libc::O_RDONLY | libc::O_DIRECTORY)
            .map(Self)
    }
    pub fn regular(&self, name: &str) -> Result<File> {
        let file = self.open_child(name, libc::O_RDONLY | libc::O_NONBLOCK)?;
        let m = file.metadata().map_err(|e| e.to_string())?;
        if !m.is_file() || m.nlink() != 1 {
            return Err("资源必须是独立普通文件，原内容已保留".into());
        }
        Ok(file)
    }
    pub fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub fn ensure(&self, name: &str) -> Result<Self> {
        if let Some(dir) = self.optional(name)? {
            return Ok(dir);
        }
        self.create_shared(name)
    }
    fn create_shared(&self, name: &str) -> Result<Self> {
        // Independent game-root jobs share this anonymous staging parent.
        // Another worker may create it after our missing-path read. EEXIST
        // permits only the same strict child open below; links, ordinary files
        // and mount replacements remain rejected by openat2.
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), c(name)?.as_ptr(), 0o700) } != 0 {
            let cause = std::io::Error::last_os_error();
            if cause.raw_os_error() != Some(libc::EEXIST) {
                return Err(format!("无法创建匿名网络暂存目录：{cause}"));
            }
        }
        self.0.sync_all().map_err(|e| e.to_string())?;
        self.child(name)
    }
    pub fn anonymous(&self) -> Result<File> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(".")?.as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(error("文件系统不支持匿名资源暂存文件，资源目录未改动"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub fn key(&self) -> Result<Key> {
        self.0
            .metadata()
            .map(|m| Key::of(&m))
            .map_err(|e| e.to_string())
    }
    pub fn stat(&self, name: &str) -> Result<Option<libc::stat>> {
        let mut stat = std::mem::MaybeUninit::uninit();
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(unsafe { stat.assume_init() }));
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error("无法检查资源文件"))
        }
    }
    fn names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
            .map_err(|e| e.to_string())?
        {
            let name = entry
                .map_err(|e| e.to_string())?
                .file_name()
                .into_string()
                .map_err(|_| "资源目录含非UTF8名称")?;
            provider::file_name(&name)?;
            names.push(name);
            if names.len() > 4096 {
                return Err("资源目录条目超过安全上限".into());
            }
        }
        names.sort();
        Ok(names)
    }
}
pub(super) fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
fn stamp(m: &Metadata) -> (u64, u64, u64, u32, u64, i64, i64, i64, i64) {
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct Profile {
    id: String,
    directory: Key,
    fingerprint: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct ResourceView {
    directory: Option<Key>,
    others: BTreeMap<String, (u64, u64, u32)>,
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
        use super::confirmation::allocation::*;
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
                // Resource transaction stages are protected by writer admission and
                // are checked by that service's recovery guard, not user inventory.
                if name.starts_with(".pcl-resource-stage-") {
                    continue;
                }
                let stat = dir.stat(&name)?.ok_or("资源目录在读取期间变化")?;
                let eligible = if kind == "mods" {
                    name.ends_with(".jar") || name.ends_with(".jar.disabled")
                } else {
                    name.ends_with(".zip")
                };
                if eligible && stat.st_mode & libc::S_IFMT == libc::S_IFREG {
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
                    let linked = dir.regular(&name)?.metadata().map_err(|e| e.to_string())?;
                    if Key::of(&linked) != Key::of(&file.metadata().map_err(|e| e.to_string())?) {
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
                    view.others
                        .insert(name, (stat.st_dev, stat.st_ino, stat.st_mode));
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

#[cfg(test)]
#[path = "target/staging_tests.rs"]
mod staging_tests;
