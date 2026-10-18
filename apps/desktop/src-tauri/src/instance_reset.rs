//! Existing-instance component reset. Callers hold the shared writer admission.
//! Installation and processors run in an owned root; only the two same-id core
//! files are exchanged. All filesystem mutations are relative to no-follow FDs.
use pcl_install::{
    ComponentSelection, InstallRequest, Installer, Progress, ResolvedInstallRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const STORE: &str = "instance-resets";
const MAX_JSON: u64 = 8 * 1024 * 1024;
const MAX_JAR: u64 = 2 * 1024 * 1024 * 1024;
const MAX_INSTANCES: usize = 4096;
const MAX_FILES: usize = 100_000;
const MAX_DEPTH: usize = 64;
const MAX_JOURNAL: u64 = 128 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize)]
pub struct ResetPlan {
    pub revision: String,
    pub minecraft: String,
    pub id: String,
    pub components: Vec<ComponentSelection>,
    pub current_components: Vec<ComponentSelection>,
    pub current_summary: String,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    root_key: DirKey,
    #[serde(skip)]
    versions_key: DirKey,
    #[serde(skip)]
    instance_key: DirKey,
    #[serde(skip)]
    originals: Vec<Snapshot>,
}

impl ResetPlan {
    pub(crate) fn install_request(&self) -> InstallRequest {
        InstallRequest {
            minecraft: self.minecraft.clone(),
            name: self.id.clone(),
            components: self.components.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DirKey {
    dev: u64,
    ino: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    size: u64,
    hash: String,
    dev: u64,
    ino: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}
impl Snapshot {
    fn stable_matches(&self, other: &Self) -> bool {
        self.size == other.size
            && self.hash == other.hash
            && self.dev == other.dev
            && self.ino == other.ino
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    Staging,
    Prepared,
    Committed,
    Finished,
    RolledBack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreFile {
    name: String,
    original: Snapshot,
    replacement: Option<Snapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    operation_id: String,
    root: PathBuf,
    root_key: DirKey,
    versions_key: DirKey,
    instance_key: DirKey,
    operation_key: DirKey,
    instance_id: String,
    stage_key: Option<DirKey>,
    incoming_key: Option<DirKey>,
    backup_key: Option<DirKey>,
    state: State,
    files: Vec<CoreFile>,
}

struct Dir {
    file: File,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn os_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}
fn name_ok(name: &str) -> Result<()> {
    pcl_core::identifier(name)?;
    if name.len() > 255 || name.chars().any(char::is_control) {
        return Err("文件名称无效".into());
    }
    Ok(())
}
fn c_name(name: &str) -> Result<CString> {
    name_ok(name)?;
    CString::new(name).map_err(err)
}
fn operation_id() -> String {
    format!(
        "r-{:x}-{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}
fn operation_ok(name: &str) -> bool {
    name.starts_with("r-")
        && name.len() <= 80
        && name
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_hexdigit() || b == b'r')
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err("组件重置已取消".into())
    } else {
        Ok(())
    }
}
impl Dir {
    fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err("游戏目录必须是绝对路径".into());
        }
        let slash = CString::new("/").map_err(err)?;
        let fd = unsafe {
            libc::open(
                slash.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(os_error("无法打开实例重置目录"));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        for component in path.components() {
            match component {
                std::path::Component::RootDir => continue,
                std::path::Component::Normal(name) => {
                    let name = CString::new(name.as_bytes()).map_err(err)?;
                    let fd = unsafe {
                        libc::openat(
                            file.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                        )
                    };
                    if fd < 0 {
                        return Err(os_error("游戏目录祖先已变化或包含符号链接"));
                    }
                    file = unsafe { File::from_raw_fd(fd) };
                }
                _ => return Err("游戏目录路径包含不安全的分量".into()),
            }
        }
        Ok(Self { file })
    }
    fn child(&self, name: &str) -> Result<Self> {
        let name = c_name(name)?;
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let how = OpenHow {
            flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
            mode: 0,
            resolve: 0x01 | 0x02 | 0x04 | 0x08,
        };
        // NO_XDEV also rejects bind mounts, so temporary-tree cleanup cannot
        // descend into a mounted user directory even on the same filesystem.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.file.as_raw_fd(),
                name.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            ) as i32
        };
        if fd < 0 {
            return Err(os_error(
                "目录不存在、已变化、包含符号链接或跨挂载点（需要 Linux 5.6 以上）",
            ));
        }
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
        })
    }
    fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    fn create_dir(&self, name: &str) -> Result<Self> {
        let name_c = c_name(name)?;
        if unsafe { libc::mkdirat(self.file.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
            return Err(os_error("无法创建实例重置目录"));
        }
        self.sync()?;
        self.child(name)
    }
    fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(dir) => Ok(dir),
            None => self.create_dir(name),
        }
    }
    fn stat(&self, name: &str) -> Result<Option<libc::stat>> {
        let name = c_name(name)?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(unsafe { st.assume_init() }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(err(error))
        }
    }
    fn regular(&self, name: &str) -> Result<File> {
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(os_error("无法读取核心文件"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(err)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err("核心文件必须是独立普通文件，不能是符号链接或硬链接".into());
        }
        Ok(file)
    }
    fn cache_file(&self, name: &str) -> Result<File> {
        // Published cache files may have one additional staging link until cleanup.
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(os_error("无法读取支持库文件"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata().map_err(err)?.is_file() {
            return Err("支持库必须是普通文件".into());
        }
        Ok(file)
    }
    fn create_file(&self, name: &str) -> Result<File> {
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(os_error("无法创建暂存文件"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn anonymous_file(&self) -> Result<File> {
        // O_TMPFILE takes a directory name. Use the pinned directory itself so
        // neither a partial nor an unregistered completed copy has a pathname.
        let path = CString::new(".").map_err(err)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                path.as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(os_error(
                "此文件系统不支持安全的匿名核心暂存文件，原实例未改动",
            ));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(self.fd_path()).map_err(err)? {
            let name = entry
                .map_err(err)?
                .file_name()
                .into_string()
                .map_err(|_| "目录包含无效文件名")?;
            name_ok(&name)?;
            names.push(name);
            if names.len() > MAX_FILES {
                return Err("目录文件数量超过重置安全上限".into());
            }
        }
        names.sort();
        Ok(names)
    }
    fn fd_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
    }
    fn key(&self) -> Result<DirKey> {
        let st = self.file.metadata().map_err(err)?;
        Ok(DirKey {
            dev: st.dev(),
            ino: st.ino(),
        })
    }
    fn sync(&self) -> Result<()> {
        self.file.sync_all().map_err(err)
    }
    fn unlink(&self, name: &str, directory: bool) -> Result<()> {
        let name = c_name(name)?;
        if unsafe {
            libc::unlinkat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } != 0
        {
            return Err(os_error("无法清理重置暂存文件"));
        }
        self.sync()
    }
}

fn snapshot(mut file: File, limit: u64, cancel: Option<&AtomicBool>) -> Result<Snapshot> {
    let st = file.metadata().map_err(err)?;
    if !st.is_file() || st.len() > limit {
        return Err("核心文件类型无效或大小超过安全上限".into());
    }
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        if let Some(cancel) = cancel {
            check(cancel)?;
        }
        let n = file.read(&mut buf).map_err(err)?;
        if n == 0 {
            break;
        }
        size = size.checked_add(n as u64).ok_or("核心文件过大")?;
        if size > limit {
            return Err("核心文件大小超过安全上限".into());
        }
        digest.update(&buf[..n]);
    }
    let after = file.metadata().map_err(err)?;
    if size != st.len()
        || st.len() != after.len()
        || st.mtime() != after.mtime()
        || st.mtime_nsec() != after.mtime_nsec()
        || st.ctime() != after.ctime()
        || st.ctime_nsec() != after.ctime_nsec()
    {
        return Err("读取期间文件发生变化，请刷新后重试".into());
    }
    Ok(Snapshot {
        size,
        hash: format!("{:x}", digest.finalize()),
        dev: st.dev(),
        ino: st.ino(),
        mtime: st.mtime(),
        mtime_nsec: st.mtime_nsec(),
        ctime: st.ctime(),
        ctime_nsec: st.ctime_nsec(),
    })
}
fn read_json(dir: &Dir, name: &str) -> Result<(Value, Snapshot)> {
    let mut file = dir.regular(name)?;
    if file.metadata().map_err(err)?.len() > MAX_JSON {
        return Err("版本 JSON 超过安全上限".into());
    }
    let snap = snapshot(dir.regular(name)?, MAX_JSON, None)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_JSON + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    let data: Value =
        serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))
            .map_err(err)?;
    if !data.is_object() || snap != snapshot(dir.regular(name)?, MAX_JSON, None)? {
        return Err("版本 JSON 无效或读取期间已变化".into());
    }
    Ok((data, snap))
}
fn game_arg(data: &Value, flag: &str) -> Option<String> {
    data["arguments"]["game"]
        .as_array()?
        .windows(2)
        .find(|pair| pair[0].as_str() == Some(flag))?
        .get(1)?
        .as_str()
        .map(str::to_owned)
}
fn coordinate(data: &Value, prefix: &str) -> Option<String> {
    data["libraries"].as_array()?.iter().find_map(|library| {
        library["name"]
            .as_str()
            .and_then(|name| name.strip_prefix(prefix))
            .and_then(|tail| tail.strip_prefix(':'))
            .and_then(|tail| tail.split(':').next())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    })
}
fn identity(data: &Value, id: &str) -> Result<(String, Vec<ComponentSelection>, String)> {
    if data["id"].as_str() != Some(id)
        || data.get("inheritsFrom").is_some()
        || data.get("jar").is_some_and(|jar| jar.as_str() != Some(id))
    {
        return Err("暂不支持继承版本、外部基础 JAR 或版本标识不一致的实例重置".into());
    }
    if ["patches", "patch", "mmc-pack", "minecraftArguments"]
        .iter()
        .any(|key| data.get(*key).is_some())
    {
        return Err("暂不支持旧版或自定义启动布局的实例重置".into());
    }
    let class = data["mainClass"]
        .as_str()
        .ok_or("版本缺少启动主类，无法确定重置范围")?;
    let libraries = data["libraries"].as_array().ok_or("版本依赖库信息无效")?;
    if libraries.iter().any(|library| {
        library["name"].as_str().is_some_and(|name| {
            [
                "optifine:",
                "com.mumfrey:liteloader:",
                "org.quiltmc:quilt-loader:",
                "net.labymod:",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        })
    }) {
        return Err("暂不支持此实例的加载器或混合组件，请保留原实例".into());
    }
    let fabric = coordinate(data, "net.fabricmc:fabric-loader");
    let neo = coordinate(data, "net.neoforged:neoforge")
        .or_else(|| coordinate(data, "net.neoforged:forge"))
        .or_else(|| game_arg(data, "--fml.neoForgeVersion"));
    let forge = game_arg(data, "--fml.forgeVersion")
        .or_else(|| coordinate(data, "net.minecraftforge:forge"))
        .or_else(|| coordinate(data, "net.minecraftforge:fmlloader"))
        .map(|v| v.split_once('-').map(|(_, v)| v.to_owned()).unwrap_or(v));
    if fabric.is_some() && (neo.is_some() || forge.is_some()) {
        return Err("实例包含混合加载器，无法安全重置".into());
    }
    let component = if let Some(version) = neo {
        Some(ComponentSelection {
            provider: "neoforge".into(),
            version,
        })
    } else if let Some(version) = forge {
        Some(ComponentSelection {
            provider: "forge".into(),
            version,
        })
    } else {
        fabric.map(|version| ComponentSelection {
            provider: "fabric".into(),
            version,
        })
    };
    let compatible = match component.as_ref().map(|c| c.provider.as_str()) {
        None => class == "net.minecraft.client.main.Main",
        Some("fabric") => matches!(
            class,
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
                | "net.fabricmc.loader.launch.knot.KnotClient"
        ),
        Some("forge" | "neoforge") => matches!(
            class,
            "cpw.mods.modlauncher.Launcher"
                | "cpw.mods.bootstraplauncher.BootstrapLauncher"
                | "net.minecraftforge.bootstrap.ForgeBootstrap"
        ),
        _ => false,
    };
    if !compatible {
        return Err("暂不支持此实例的旧版或自定义启动主类重置".into());
    }
    let mc_arg = game_arg(data, "--fml.mcVersion");
    let client = data["clientVersion"].as_str().map(str::to_owned);
    if mc_arg
        .as_ref()
        .zip(client.as_ref())
        .is_some_and(|(a, b)| a != b)
    {
        return Err("实例的 Minecraft 版本信息存在冲突".into());
    }
    let minecraft = mc_arg
        .or(client)
        .or_else(|| component.is_none().then(|| id.to_owned()))
        .ok_or("实例缺少明确的 Minecraft 原版版本号，无法安全重置")?;
    pcl_core::identifier(&minecraft)?;
    let components: Vec<_> = component.into_iter().collect();
    let summary = components
        .first()
        .map(|c| {
            format!(
                "{} {}",
                match c.provider.as_str() {
                    "fabric" => "Fabric",
                    "forge" => "Forge",
                    _ => "NeoForge",
                },
                c.version
            )
        })
        .unwrap_or_else(|| "原版".into());
    Ok((minecraft, components, summary))
}

/// Read-only, root-bound plan. The revision covers both core contents and all
/// other version JSONs so a newly introduced dependent invalidates submission.
pub fn prepare(root: &Path, id: &str, components: Vec<ComponentSelection>) -> Result<ResetPlan> {
    prepare_bound(root, id, components, None)
}
fn prepare_bound(
    root: &Path,
    id: &str,
    mut components: Vec<ComponentSelection>,
    active: Option<&str>,
) -> Result<ResetPlan> {
    name_ok(id)?;
    ensure_ready_except(root, active)?;
    for component in &mut components {
        component.provider = component.provider.to_ascii_lowercase();
    }
    let path = root.canonicalize().map_err(err)?;
    let root_dir = Dir::open(&path)?;
    let versions = root_dir.child("versions")?;
    let instance = versions.child(id)?;
    let (data, metadata) = read_json(&instance, &format!("{id}.json"))?;
    let jar = snapshot(instance.regular(&format!("{id}.jar"))?, MAX_JAR, None)?;
    let (minecraft, current_components, current_summary) = identity(&data, id)?;
    InstallRequest {
        minecraft: minecraft.clone(),
        name: id.into(),
        components: components.clone(),
    }
    .validate()?;
    let root_key = root_dir.key()?;
    let versions_key = versions.key()?;
    let instance_key = instance.key()?;
    let mut digest = Sha256::new();
    digest.update(
        serde_json::to_vec(&(
            &path,
            &root_key,
            &versions_key,
            &instance_key,
            &metadata,
            &jar,
            &minecraft,
            &components,
        ))
        .map_err(err)?,
    );
    let names = versions.names()?;
    if names.len() > MAX_INSTANCES {
        return Err("版本数量超过实例重置安全上限".into());
    }
    for name in names {
        let other = versions.child(&name)?;
        let other_key = other.key()?;
        let filename = format!("{name}.json");
        if other.stat(&filename)?.is_none() {
            digest.update(serde_json::to_vec(&(&name, other_key)).map_err(err)?);
            continue;
        }
        let (profile, snap) = read_json(&other, &filename)?;
        if name != id
            && (profile["inheritsFrom"].as_str() == Some(id) || profile["jar"].as_str() == Some(id))
        {
            return Err(format!(
                "实例 {name} 继承或复用此实例的核心文件，暂不能安全重置"
            ));
        }
        digest.update(serde_json::to_vec(&(&name, other_key, snap)).map_err(err)?);
    }
    if root_dir.key()? != Dir::open(&path)?.key()?
        || versions.key()? != root_dir.child("versions")?.key()?
        || instance.key()? != versions.child(id)?.key()?
        || metadata != snapshot(instance.regular(&format!("{id}.json"))?, MAX_JSON, None)?
        || jar != snapshot(instance.regular(&format!("{id}.jar"))?, MAX_JAR, None)?
    {
        return Err("实例目录读取期间发生变化，请刷新后重试".into());
    }
    Ok(ResetPlan {
        revision: format!("reset-v1-{:x}", digest.finalize()),
        minecraft,
        id: id.into(),
        components,
        current_components,
        current_summary,
        root: path,
        root_key,
        versions_key,
        instance_key,
        originals: vec![metadata, jar],
    })
}

fn storage(root: &Dir, create: bool) -> Result<Option<Dir>> {
    let pcl = if create {
        Some(root.ensure(".pcl-linux")?)
    } else {
        root.optional(".pcl-linux")?
    };
    match pcl {
        None => Ok(None),
        Some(pcl) => {
            if create {
                pcl.ensure(STORE).map(Some)
            } else {
                pcl.optional(STORE)
            }
        }
    }
}
struct ResetLock {
    file: File,
}
impl Drop for ResetLock {
    fn drop(&mut self) {
        // flock belongs to the open file description shared by fork/dup. Closing
        // our descriptor alone can leave it locked until a child reaches exec.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock(storage: &Dir) -> Result<ResetLock> {
    let file = match storage.stat(".lock")? {
        Some(_) => storage.regular(".lock")?,
        None => storage.create_file(".lock")?,
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("其他进程正在重置或恢复实例组件，请稍后重试".into());
    }
    Ok(ResetLock { file })
}
fn validate_journal(j: &Journal, name: &str) -> Result<()> {
    let invalid = || "实例重置记录无效，已保留原文件与备份".to_owned();
    if j.schema != 1
        || j.operation_id != name
        || !operation_ok(name)
        || !j.root.is_absolute()
        || name_ok(&j.instance_id).is_err()
        || j.instance_id.len() > 120
        || j.files.len() != 2
    {
        return Err(invalid());
    }
    for (i, file) in j.files.iter().enumerate() {
        if file.name != format!("{}.{}", j.instance_id, if i == 0 { "json" } else { "jar" }) {
            return Err(invalid());
        }
        for snapshot in std::iter::once(&file.original).chain(file.replacement.as_ref()) {
            if snapshot.size > if i == 0 { MAX_JSON } else { MAX_JAR }
                || snapshot.hash.len() != 64
                || !snapshot.hash.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(invalid());
            }
        }
        if j.state != State::Staging && j.state != State::RolledBack && file.replacement.is_none() {
            return Err(invalid());
        }
    }
    if matches!(
        j.state,
        State::Prepared | State::Committed | State::Finished
    ) && (j.backup_key.is_none() || j.incoming_key.is_none() || j.stage_key.is_none())
    {
        return Err(invalid());
    }
    Ok(())
}
fn read_journal(operation: &Dir, name: &str) -> Result<Journal> {
    let mut file = operation.regular("journal.json")?;
    if file.metadata().map_err(err)?.len() > MAX_JOURNAL {
        return Err("实例重置记录过大".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_JOURNAL + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err("实例重置记录过大".into());
    }
    let journal = serde_json::from_slice(&bytes).map_err(|_| "实例重置记录损坏，已保留备份")?;
    validate_journal(&journal, name)?;
    Ok(journal)
}
fn write_journal(operation: &Dir, journal: &Journal) -> Result<()> {
    validate_journal(journal, &journal.operation_id)?;
    if operation.key()? != journal.operation_key {
        return Err("实例重置记录目录已变化".into());
    }
    let bytes = serde_json::to_vec(journal).map_err(err)?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err("实例重置记录过大".into());
    }
    let name = format!("journal-{}.next", operation_id());
    let mut next = operation.create_file(&name)?;
    let written = (|| {
        next.write_all(&bytes).map_err(err)?;
        next.sync_all().map_err(err)
    })();
    if let Err(error) = written {
        return match operation.unlink(&name, false) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
    if operation.stat("journal.json")?.is_some() {
        operation.regular("journal.json")?;
    }
    let source = c_name(&name)?;
    let target = c_name("journal.json")?;
    if unsafe {
        libc::renameat(
            operation.file.as_raw_fd(),
            source.as_ptr(),
            operation.file.as_raw_fd(),
            target.as_ptr(),
        )
    } != 0
    {
        let error = os_error("无法保存实例重置记录");
        return match operation.unlink(&name, false) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
    operation.sync()
}
fn rename_new(source: &Dir, source_name: &str, target: &Dir, target_name: &str) -> Result<()> {
    let source_name = c_name(source_name)?;
    let target_name = c_name(target_name)?;
    if unsafe {
        libc::renameat2(
            source.file.as_raw_fd(),
            source_name.as_ptr(),
            target.file.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(os_error("无法交换实例核心文件"));
    }
    source.sync()?;
    target.sync()
}
fn bound_dirs(root: &Dir, journal: &Journal) -> Result<(Dir, Dir)> {
    if root.key()? != journal.root_key || Dir::open(&journal.root)?.key()? != journal.root_key {
        return Err("绑定的游戏目录已变化，已保留重置记录和备份".into());
    }
    let versions = root.child("versions")?;
    let instance = versions.child(&journal.instance_id)?;
    if versions.key()? != journal.versions_key || instance.key()? != journal.instance_key {
        return Err("绑定的实例目录已变化，已保留重置记录和备份".into());
    }
    Ok((versions, instance))
}
fn owned_child(operation: &Dir, name: &str, expected: &Option<DirKey>) -> Result<Option<Dir>> {
    let child = operation.optional(name)?;
    if let Some(child) = &child {
        if let Some(expected) = expected {
            if child.key()? != *expected {
                return Err(format!("重置暂存目录 {name} 已被替换，已保留文件"));
            }
        } else if !child.names()?.is_empty() {
            return Err(format!("重置暂存目录 {name} 含未知内容，已保留文件"));
        }
    }
    Ok(child)
}
fn remove_owned_tree(parent: &Dir, name: &str, key: &DirKey, count: &mut usize) -> Result<()> {
    remove_owned_tree_at(parent, name, key, count, 0)
}
fn remove_owned_tree_at(
    parent: &Dir,
    name: &str,
    key: &DirKey,
    count: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("重置暂存目录深度超过安全上限".into());
    }
    let dir = parent.child(name)?;
    if dir.key()? != *key {
        return Err("重置暂存目录已被替换".into());
    }
    for name in dir.names()? {
        *count += 1;
        if *count > MAX_FILES {
            return Err("暂存清理数量超过安全上限".into());
        }
        let st = dir.stat(&name)?.ok_or("暂存文件已变化")?;
        match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => {
                let child = dir.child(&name)?;
                remove_owned_tree_at(&dir, &name, &child.key()?, count, depth + 1)?;
            }
            libc::S_IFREG => {
                dir.cache_file(&name)?;
                dir.unlink(&name, false)?;
            }
            _ => return Err("重置暂存目录含符号链接或特殊文件，已保留待清理内容".into()),
        }
    }
    if parent.child(name)?.key()? != *key {
        return Err("重置暂存目录清理期间已变化".into());
    }
    parent.unlink(name, true)
}
fn cleanup_stage(operation: &Dir, journal: &Journal) -> Result<()> {
    if let Some(stage) = owned_child(operation, "stage", &journal.stage_key)? {
        remove_owned_tree(operation, "stage", &stage.key()?, &mut 0)?;
    }
    Ok(())
}
fn limit(index: usize) -> u64 {
    if index == 0 {
        MAX_JSON
    } else {
        MAX_JAR
    }
}
fn matches_file(dir: &Dir, name: &str, expected: &Snapshot, index: usize) -> Result<bool> {
    if dir.stat(name)?.is_none() {
        return Ok(false);
    }
    Ok(expected.stable_matches(&snapshot(dir.regular(name)?, limit(index), None)?))
}

/// Preflight the complete rollback before moving anything. Conflicting external
/// edits are retained and leave the launch guard active for explicit recovery.
fn rollback(root: &Dir, operation: &Dir, journal: &Journal) -> Result<()> {
    let (_, instance) = bound_dirs(root, journal)?;
    let backup =
        owned_child(operation, "backup", &journal.backup_key)?.ok_or("重置备份目录缺失")?;
    let incoming =
        owned_child(operation, "incoming", &journal.incoming_key)?.ok_or("重置替换目录缺失")?;
    let mut moved = Vec::new();
    for (index, file) in journal.files.iter().enumerate() {
        let replacement = file.replacement.as_ref().ok_or("重置替换文件记录缺失")?;
        let original_here = matches_file(&instance, &file.name, &file.original, index)?;
        let original_backup = matches_file(&backup, &file.name, &file.original, index)?;
        let replacement_here = matches_file(&instance, &file.name, replacement, index)?;
        let replacement_incoming = matches_file(&incoming, &file.name, replacement, index)?;
        if original_here == original_backup
            || replacement_here == replacement_incoming
            || (instance.stat(&file.name)?.is_some() && !original_here && !replacement_here)
            || (backup.stat(&file.name)?.is_some() && !original_backup)
            || (incoming.stat(&file.name)?.is_some() && !replacement_incoming)
        {
            return Err("核心文件或备份被外部修改，无法覆盖恢复；已保留原始备份".into());
        }
        moved.push((original_backup, replacement_here));
    }
    for (file, (original_backup, replacement_here)) in journal.files.iter().zip(moved).rev() {
        bound_dirs(root, journal)?;
        if replacement_here {
            rename_new(&instance, &file.name, &incoming, &file.name)?;
        }
        if original_backup {
            rename_new(&backup, &file.name, &instance, &file.name)?;
        }
    }
    for (index, file) in journal.files.iter().enumerate() {
        if !matches_file(&instance, &file.name, &file.original, index)? {
            return Err("原始核心文件恢复校验失败".into());
        }
    }
    Ok(())
}
fn finish_rollback(operation: &Dir, journal: &mut Journal) -> Result<()> {
    cleanup_stage(operation, journal)?;
    if let Some(incoming) = owned_child(operation, "incoming", &journal.incoming_key)? {
        for name in incoming.names()? {
            let (index, item) = journal
                .files
                .iter()
                .enumerate()
                .find(|(_, file)| file.name == name)
                .ok_or("替换目录含未知文件")?;
            let expected = item.replacement.as_ref().ok_or("替换目录含未登记文件")?;
            if !matches_file(&incoming, &name, expected, index)? {
                return Err("替换文件被外部修改，已保留文件".into());
            }
            incoming.unlink(&name, false)?;
        }
        operation.unlink("incoming", true)?;
    }
    if let Some(backup) = owned_child(operation, "backup", &journal.backup_key)? {
        if !backup.names()?.is_empty() {
            return Err("回滚后的备份目录含未知文件".into());
        }
        operation.unlink("backup", true)?;
    }
    journal.state = State::RolledBack;
    write_journal(operation, journal)
}
fn finish_committed(root: &Dir, operation: &Dir, journal: &mut Journal) -> Result<()> {
    let (_, instance) = bound_dirs(root, journal)?;
    let backup =
        owned_child(operation, "backup", &journal.backup_key)?.ok_or("重置备份目录缺失")?;
    for (index, file) in journal.files.iter().enumerate() {
        if !matches_file(
            &instance,
            &file.name,
            file.replacement.as_ref().ok_or("替换记录缺失")?,
            index,
        )? || !matches_file(&backup, &file.name, &file.original, index)?
        {
            return Err("已提交的核心文件或原始备份发生变化，已保留重置记录".into());
        }
    }
    cleanup_stage(operation, journal)?;
    if let Some(incoming) = owned_child(operation, "incoming", &journal.incoming_key)? {
        if !incoming.names()?.is_empty() {
            return Err("已提交的替换目录含未知文件".into());
        }
        operation.unlink("incoming", true)?;
    }
    journal.state = State::Finished;
    write_journal(operation, journal)
}
fn recover_one(root: &Dir, operation: &Dir, journal: &mut Journal) -> Result<()> {
    if operation.key()? != journal.operation_key {
        return Err("实例重置记录目录已变化".into());
    }
    match journal.state {
        State::Staging => finish_rollback(operation, journal),
        State::Prepared => {
            rollback(root, operation, journal)?;
            finish_rollback(operation, journal)
        }
        State::Committed => finish_committed(root, operation, journal),
        State::Finished | State::RolledBack => Ok(()),
    }
}

/// Read-only readiness guard used before launch and all mutations of this root.
pub fn ensure_ready(root: &Path) -> Result<()> {
    ensure_ready_except(root, None)
}
fn ensure_ready_except(root: &Path, active: Option<&str>) -> Result<()> {
    let path = root.canonicalize().map_err(err)?;
    let dir = Dir::open(&path)?;
    let Some(store) = storage(&dir, false)? else {
        return Ok(());
    };
    for name in store.names()? {
        if name == ".lock" {
            store.regular(&name)?;
            continue;
        }
        if !operation_ok(&name) {
            return Err("实例重置目录含未知记录，已保留文件".into());
        }
        let operation = store.child(&name)?;
        if operation.stat("journal.json")?.is_none() {
            return Err("存在未完成的实例重置，请先恢复后再操作或启动".into());
        }
        let journal = read_journal(&operation, &name)?;
        if journal.root != path
            || journal.root_key != dir.key()?
            || journal.operation_key != operation.key()?
        {
            return Err("实例重置记录与当前游戏目录不匹配，已保留文件".into());
        }
        if !matches!(journal.state, State::Finished | State::RolledBack)
            && !(active == Some(name.as_str()) && journal.state == State::Staging)
        {
            return Err("存在未完成的实例重置，请先恢复后再操作或启动".into());
        }
    }
    Ok(())
}

/// Requires the shared writer admission. Prepared operations restore originals;
/// committed operations retain the completed reset and finish owned cleanup.
pub fn recover_pending(root: &Path) -> Result<Value> {
    let path = root.canonicalize().map_err(err)?;
    let dir = Dir::open(&path)?;
    let Some(store) = storage(&dir, false)? else {
        return Ok(json!({"recovered": 0}));
    };
    let _lock = lock(&store)?;
    let mut recovered = 0usize;
    for name in store.names()? {
        if name == ".lock" {
            continue;
        }
        if !operation_ok(&name) {
            return Err("实例重置目录含未知记录，已保留文件".into());
        }
        let operation = store.child(&name)?;
        if operation.stat("journal.json")?.is_none() {
            if operation.names()?.is_empty() {
                store.unlink(&name, true)?;
                recovered += 1;
                continue;
            }
            return Err("未登记的重置目录含有文件，已保留待人工检查".into());
        }
        let mut journal = read_journal(&operation, &name)?;
        if journal.root != path || journal.root_key != dir.key()? {
            return Err("重置记录与当前游戏目录不匹配".into());
        }
        if !matches!(journal.state, State::Finished | State::RolledBack) {
            recover_one(&dir, &operation, &mut journal)
                .map_err(|e| format!("取消清理失败：{e}"))?;
            recovered += 1;
        }
    }
    ensure_ready(&path)?;
    Ok(json!({"recovered": recovered, "message": "实例重置恢复完成"}))
}

fn setup(root: &Dir, store: &Dir, plan: &ResetPlan) -> Result<(Dir, Journal)> {
    let name = operation_id();
    let operation = store.create_dir(&name)?;
    let mut journal = Journal {
        schema: 1,
        operation_id: name,
        root: plan.root.clone(),
        root_key: root.key()?,
        versions_key: plan.versions_key.clone(),
        instance_key: plan.instance_key.clone(),
        operation_key: operation.key()?,
        instance_id: plan.id.clone(),
        stage_key: None,
        incoming_key: None,
        backup_key: None,
        state: State::Staging,
        files: plan
            .originals
            .iter()
            .enumerate()
            .map(|(i, original)| CoreFile {
                name: format!("{}.{}", plan.id, if i == 0 { "json" } else { "jar" }),
                original: original.clone(),
                replacement: None,
            })
            .collect(),
    };
    if let Err(error) = write_journal(&operation, &journal) {
        return match remove_owned_tree(store, &journal.operation_id, &journal.operation_key, &mut 0)
        {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
    let result = (|| {
        journal.backup_key = Some(operation.create_dir("backup")?.key()?);
        write_journal(&operation, &journal)?;
        journal.incoming_key = Some(operation.create_dir("incoming")?.key()?);
        write_journal(&operation, &journal)?;
        journal.stage_key = Some(operation.create_dir("stage")?.key()?);
        write_journal(&operation, &journal)
    })();
    if let Err(error) = result {
        return match finish_rollback(&operation, &mut journal) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
    Ok((operation, journal))
}
fn copy_core(
    source: File,
    incoming: &Dir,
    operation: &Dir,
    journal: &mut Journal,
    index: usize,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut source = source;
    let mut target = incoming.anonymous_file()?;
    let mut buf = [0u8; 65536];
    let mut size = 0u64;
    (|| {
        loop {
            check(cancel)?;
            let n = source.read(&mut buf).map_err(err)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > limit(index) {
                return Err("生成的核心文件过大".into());
            }
            target.write_all(&buf[..n]).map_err(err)?;
        }
        target.sync_all().map_err(err)?;
        target.seek(SeekFrom::Start(0)).map_err(err)?;
        journal.files[index].replacement = Some(snapshot(
            target.try_clone().map_err(err)?,
            limit(index),
            Some(cancel),
        )?);
        // Publish ownership and complete contents before giving this inode a
        // name. A crash at any earlier point automatically drops the anonymous
        // file; a crash here leaves a registered, optionally absent incoming file.
        write_journal(operation, journal)?;
        check(cancel)?;
        let pinned = CString::new(format!("/proc/self/fd/{}", target.as_raw_fd())).map_err(err)?;
        let name = c_name(&journal.files[index].name)?;
        if unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                pinned.as_ptr(),
                incoming.file.as_raw_fd(),
                name.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        } != 0
        {
            return Err(os_error("无法发布已登记的核心暂存文件"));
        }
        incoming.sync()
    })()
}
fn merge_cache(source: &Dir, target: &Dir, cancel: &AtomicBool, count: &mut usize) -> Result<()> {
    merge_cache_at(source, target, cancel, count, 0)
}
fn merge_cache_at(
    source: &Dir,
    target: &Dir,
    cancel: &AtomicBool,
    count: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("支持库目录深度超过安全上限".into());
    }
    for name in source.names()? {
        check(cancel)?;
        *count += 1;
        if *count > MAX_FILES {
            return Err("安装支持文件超过重置安全上限".into());
        }
        let st = source.stat(&name)?.ok_or("安装支持文件已变化")?;
        match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => merge_cache_at(
                &source.child(&name)?,
                &target.ensure(&name)?,
                cancel,
                count,
                depth + 1,
            )?,
            libc::S_IFREG => {
                let file = source.cache_file(&name)?;
                let expected = snapshot(source.cache_file(&name)?, MAX_JAR, Some(cancel))?;
                if target.stat(&name)?.is_some() {
                    let existing = snapshot(target.cache_file(&name)?, MAX_JAR, Some(cancel))?;
                    if expected.size != existing.size || expected.hash != existing.hash {
                        return Err(format!(
                            "已有支持文件与新组件不一致，已保留现有文件：{name}"
                        ));
                    }
                    continue;
                }
                // Link the pinned ordinary file, rather than its mutable source
                // pathname. No existing shared cache file is replaced. A mount
                // boundary fails safely before any instance core is changed.
                let source_fd =
                    CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(err)?;
                let target_name = c_name(&name)?;
                if unsafe {
                    libc::linkat(
                        libc::AT_FDCWD,
                        source_fd.as_ptr(),
                        target.file.as_raw_fd(),
                        target_name.as_ptr(),
                        libc::AT_SYMLINK_FOLLOW,
                    )
                } != 0
                {
                    return Err(os_error(
                        "无法发布新支持文件（不覆盖已有文件，且须位于同一文件系统）",
                    ));
                }
                target.sync()?;
                let actual = snapshot(target.cache_file(&name)?, MAX_JAR, Some(cancel))?;
                if !expected.stable_matches(&actual) {
                    return Err("新支持文件发布校验失败".into());
                }
            }
            _ => return Err("生成的支持文件包含符号链接或特殊文件".into()),
        }
    }
    Ok(())
}
fn recheck_plan(root: &Path, plan: &ResetPlan) -> Result<()> {
    let fresh = prepare(root, &plan.id, plan.components.clone())?;
    if fresh.revision != plan.revision
        || fresh.root != plan.root
        || fresh.root_key != plan.root_key
        || fresh.minecraft != plan.minecraft
    {
        return Err("实例或关联版本已变化，请重新读取重置计划".into());
    }
    Ok(())
}
fn check_originals(root: &Dir, operation: &Dir, journal: &Journal) -> Result<Dir> {
    if operation.key()? != journal.operation_key {
        return Err("重置记录目录已变化".into());
    }
    let (_, instance) = bound_dirs(root, journal)?;
    for (index, file) in journal.files.iter().enumerate() {
        if snapshot(instance.regular(&file.name)?, limit(index), None)? != file.original {
            return Err("实例核心文件已变化，请重新读取重置计划".into());
        }
    }
    Ok(instance)
}

fn commit(
    root: &Dir,
    operation: &Dir,
    journal: &mut Journal,
    cancel: &AtomicBool,
    changed: &impl Fn(usize) -> Result<()>,
) -> Result<()> {
    check(cancel)?;
    let instance = check_originals(root, operation, journal)?;
    let backup = owned_child(operation, "backup", &journal.backup_key)?.ok_or("备份目录缺失")?;
    let incoming =
        owned_child(operation, "incoming", &journal.incoming_key)?.ok_or("替换目录缺失")?;
    let mut expected_names: Vec<_> = journal.files.iter().map(|f| f.name.clone()).collect();
    expected_names.sort();
    if !backup.names()?.is_empty() || incoming.names()? != expected_names {
        return Err("重置暂存文件与记录不一致".into());
    }
    for (index, file) in journal.files.iter().enumerate() {
        let expected = file.replacement.as_ref().ok_or("替换核心文件未登记")?;
        if !matches_file(&incoming, &file.name, expected, index)? {
            return Err("替换核心文件已变化".into());
        }
    }
    let result = (|| {
        journal.state = State::Prepared;
        write_journal(operation, journal)?;
        let mut moved = 0;
        for (index, file) in journal.files.iter().enumerate() {
            check(cancel)?;
            bound_dirs(root, journal)?;
            if !matches_file(&instance, &file.name, &file.original, index)? {
                return Err("原始核心文件已变化".into());
            }
            rename_new(&instance, &file.name, &backup, &file.name)?;
            moved += 1;
            changed(moved)?;
            check(cancel)?;
            bound_dirs(root, journal)?;
            rename_new(&incoming, &file.name, &instance, &file.name)?;
            moved += 1;
            changed(moved)?;
        }
        check(cancel)?;
        for (index, file) in journal.files.iter().enumerate() {
            if !matches_file(
                &instance,
                &file.name,
                file.replacement.as_ref().unwrap(),
                index,
            )? || !matches_file(&backup, &file.name, &file.original, index)?
            {
                return Err("核心替换结果校验失败".into());
            }
        }
        bound_dirs(root, journal)?;
        journal.state = State::Committed;
        if let Err(error) = write_journal(operation, journal) {
            // A failed fsync may nevertheless have published the commit marker.
            // Re-publish Prepared before rollback; if that fails, recovery keeps
            // the durable record and the backup without guessing its outcome.
            journal.state = State::Prepared;
            if let Err(record) = write_journal(operation, journal) {
                journal.state = State::Committed;
                return Err(format!("取消清理失败：无法确认提交或回滚记录，已保留完整核心和原始备份：{record}；原错误：{error}"));
            }
            return Err(error);
        }
        Ok(())
    })();
    if let Err(error) = result {
        if journal.state == State::Committed {
            return Err(error);
        }
        return match rollback(root, operation, journal)
            .and_then(|_| finish_rollback(operation, journal))
        {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        };
    }
    // Cancellation ends at the durable commit marker. A late cancel retains the
    // complete reset and its backup. Cleanup errors stay visible and recoverable.
    finish_committed(root, operation, journal)
        .map_err(|e| format!("取消清理失败：重置已完成，但暂存清理失败：{e}"))
}

#[cfg(test)]
pub fn execute(
    root: &Path,
    project: &Path,
    plan: ResetPlan,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
) -> Result<Value> {
    execute_with_policy(
        root,
        project,
        plan,
        pcl_network::download_snapshot(),
        cancel,
        callback,
    )
}
#[cfg(test)]
pub fn execute_with_policy(
    root: &Path,
    project: &Path,
    plan: ResetPlan,
    scheduler: std::sync::Arc<pcl_network::DownloadScheduler>,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
) -> Result<Value> {
    check(cancel)?;
    recheck_plan(root, &plan)?;
    let installer = Installer::new()?
        .with_download_policy(scheduler)
        .with_project(project)
        .with_cache_source(&plan.root)?;
    let resolved = installer.resolve_request(&plan.install_request(), cancel)?;
    execute_prepared(root, plan, installer, resolved, cancel, callback, || Ok(()))
}

/// Java acquisition precedes the durable reset journal. The same resolved
/// authority is then installed in its private stage; a failed Java download
/// therefore leaves neither a pending reset nor a partly replaced core.
pub(crate) fn execute_prepared(
    root: &Path,
    plan: ResetPlan,
    installer: Installer,
    resolved: ResolvedInstallRequest,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
    before_commit: impl Fn() -> Result<()>,
) -> Result<Value> {
    check(cancel)?;
    recheck_plan(root, &plan)?;
    if serde_json::to_value(resolved.request()).map_err(err)?
        != serde_json::to_value(plan.install_request()).map_err(err)?
    {
        return Err("安装元数据与重置计划不一致，请重新检查".into());
    }
    let root_dir = Dir::open(&plan.root)?;
    let store = storage(&root_dir, true)?.ok_or("重置记录目录缺失")?;
    let _lock = lock(&store)?;
    recheck_plan(root, &plan)?;
    let (operation, mut journal) = setup(&root_dir, &store, &plan)?;
    let last = Mutex::new(None::<Progress>);
    let result = (|| {
        let stage =
            owned_child(&operation, "stage", &journal.stage_key)?.ok_or("安装暂存目录缺失")?;
        installer.install_resolved_request(&stage.fd_path(), &resolved, cancel, |mut p| {
            if p.stage == "complete" {
                p.stage = "reset_commit".into();
                p.message = "正在替换实例核心文件".into();
            }
            p.steps.push(pcl_install::InstallStep {
                id: "reset_core".into(),
                label: "重置实例组件".into(),
                state: if p.stage == "reset_commit" {
                    "running"
                } else {
                    "pending"
                }
                .into(),
                progress: None,
            });
            *last.lock().unwrap() = Some(p.clone());
            callback(p);
        })?;
        check(cancel)?;
        bound_dirs(&root_dir, &journal)?;
        if stage.key()? != operation.child("stage")?.key()? {
            return Err("安装暂存目录已变化".into());
        }
        let installed = stage.child("versions")?.child(&plan.id)?;
        let (metadata, _) = read_json(&installed, &format!("{}.json", plan.id))?;
        let (mc, selections, _) = identity(&metadata, &plan.id)?;
        let mut requested = plan.components.clone();
        for component in &mut requested {
            if component.provider == "forge" {
                if let Some(version) = component
                    .version
                    .strip_prefix(&format!("{}-", plan.minecraft))
                {
                    component.version = version.to_owned();
                }
            }
        }
        if mc != plan.minecraft
            || serde_json::to_value(&selections).map_err(err)?
                != serde_json::to_value(&requested).map_err(err)?
        {
            return Err("生成的实例版本或组件与重置计划不一致".into());
        }
        let incoming =
            owned_child(&operation, "incoming", &journal.incoming_key)?.ok_or("替换目录缺失")?;
        for index in 0..journal.files.len() {
            let name = journal.files[index].name.clone();
            copy_core(
                installed.regular(&name)?,
                &incoming,
                &operation,
                &mut journal,
                index,
                cancel,
            )?;
        }
        for cache in ["libraries", "assets"] {
            if let Some(source) = stage.optional(cache)? {
                merge_cache(&source, &root_dir.ensure(cache)?, cancel, &mut 0)?;
            }
        }
        // The Staging record intentionally blocks public prepare. Revalidate the
        // dependency set with this operation temporarily considered ready.
        let fresh = prepare_bound(
            &plan.root,
            &plan.id,
            plan.components.clone(),
            Some(&journal.operation_id),
        )?;
        if fresh.revision != plan.revision {
            return Err("安装期间实例或关联版本已变化，请重新读取重置计划".into());
        }
        before_commit()?;
        commit(&root_dir, &operation, &mut journal, cancel, &|_| Ok(()))?;
        Ok(())
    })();
    if let Err(error) = result {
        if journal.state == State::Staging {
            return match finish_rollback(&operation, &mut journal) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
            };
        }
        return Err(error);
    }
    let mut progress = last.into_inner().unwrap().unwrap_or(Progress {
        steps: vec![],
        stage: String::new(),
        message: String::new(),
        completed: 1,
        total: 1,
        bytes_done: 0,
        bytes_total: 0,
        network_bytes: 0,
    });
    progress.stage = "complete".into();
    progress.message = format!("{} 组件重置完成", plan.id);
    for step in &mut progress.steps {
        step.state = "complete".into();
        step.progress = Some(1.0);
    }
    callback(progress);
    Ok(
        json!({"id": plan.id, "minecraft": plan.minecraft, "components": plan.components,
        "backup_id": journal.operation_id, "message": "组件重置完成，实例内容已保留"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
            let path = project
                .join("work/instance-reset-tests")
                .join(operation_id());
            fs::create_dir_all(path.join("versions/fixture")).unwrap();
            let fixture = Self(path.canonicalize().unwrap());
            fixture.profile(json!({"id":"fixture", "jar":"fixture", "clientVersion":"1.20.1",
                "mainClass":"net.minecraft.client.main.Main", "libraries":[], "arguments":{"game":[]}}));
            fs::write(fixture.instance().join("fixture.jar"), b"original jar").unwrap();
            for relative in [
                "mods/user.jar",
                "config/user.toml",
                "saves/world/level.dat",
                "options.txt",
                "PCL/Setup.ini",
            ] {
                let path = fixture.instance().join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, relative.as_bytes()).unwrap();
            }
            fixture
        }
        fn root(&self) -> &Path {
            &self.0
        }
        fn instance(&self) -> PathBuf {
            self.0.join("versions/fixture")
        }
        fn profile(&self, data: Value) {
            fs::write(
                self.instance().join("fixture.json"),
                serde_json::to_vec(&data).unwrap(),
            )
            .unwrap();
        }
        fn plan(&self) -> ResetPlan {
            prepare(self.root(), "fixture", vec![]).unwrap()
        }
        fn transaction(&self) -> (Dir, Dir, Journal) {
            let plan = self.plan();
            let root = Dir::open(self.root()).unwrap();
            let store = storage(&root, true).unwrap().unwrap();
            let (operation, mut journal) = setup(&root, &store, &plan).unwrap();
            let incoming = operation.child("incoming").unwrap();
            for (i, file) in journal.files.iter_mut().enumerate() {
                let mut output = incoming.create_file(&file.name).unwrap();
                output
                    .write_all(if i == 0 {
                        b"replacement json"
                    } else {
                        b"replacement jar"
                    })
                    .unwrap();
                output.sync_all().unwrap();
                file.replacement =
                    Some(snapshot(incoming.regular(&file.name).unwrap(), limit(i), None).unwrap());
            }
            write_journal(&operation, &journal).unwrap();
            (root, operation, journal)
        }
        fn assert_content(&self) {
            for relative in [
                "mods/user.jar",
                "config/user.toml",
                "saves/world/level.dat",
                "options.txt",
                "PCL/Setup.ini",
            ] {
                assert_eq!(
                    fs::read(self.instance().join(relative)).unwrap(),
                    relative.as_bytes()
                );
            }
        }
        fn assert_original(&self, originals: &[CoreFile]) {
            let instance = Dir::open(&self.instance()).unwrap();
            for (i, file) in originals.iter().enumerate() {
                assert!(matches_file(&instance, &file.name, &file.original, i).unwrap());
            }
            self.assert_content();
        }
        fn crash_after(&self, root: &Dir, operation: &Dir, journal: &mut Journal, moves: usize) {
            journal.state = State::Prepared;
            write_journal(operation, journal).unwrap();
            let (_, instance) = bound_dirs(root, journal).unwrap();
            let backup = operation.child("backup").unwrap();
            let incoming = operation.child("incoming").unwrap();
            for step in 0..moves {
                let file = &journal.files[step / 2];
                if step % 2 == 0 {
                    rename_new(&instance, &file.name, &backup, &file.name).unwrap();
                } else {
                    rename_new(&incoming, &file.name, &instance, &file.name).unwrap();
                }
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reset_lock_releases_even_when_an_inherited_descriptor_remains_open() {
        let fixture = Fixture::new();
        let root = Dir::open(fixture.root()).unwrap();
        let store = storage(&root, true).unwrap().unwrap();
        let owned = lock(&store).unwrap();
        let inherited = owned.file.try_clone().unwrap();
        assert!(lock(&store).is_err());
        drop(owned);
        let next = lock(&store).unwrap();
        assert!(inherited.metadata().is_ok());
        assert!(lock(&store).is_err());
        drop(next);
        assert!(lock(&store).is_ok());
    }

    #[test]
    fn successful_core_exchange_keeps_physical_instance_and_all_content_with_backup() {
        let fixture = Fixture::new();
        let original_key = Dir::open(&fixture.instance()).unwrap().key().unwrap();
        let (root, operation, mut journal) = fixture.transaction();
        let old = journal.files.clone();
        commit(
            &root,
            &operation,
            &mut journal,
            &AtomicBool::new(false),
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(journal.state, State::Finished);
        assert_eq!(
            Dir::open(&fixture.instance()).unwrap().key().unwrap(),
            original_key
        );
        assert_eq!(
            fs::read(fixture.instance().join("fixture.json")).unwrap(),
            b"replacement json"
        );
        assert_eq!(
            fs::read(fixture.instance().join("fixture.jar")).unwrap(),
            b"replacement jar"
        );
        for (i, file) in old.iter().enumerate() {
            assert!(matches_file(
                &operation.child("backup").unwrap(),
                &file.name,
                &file.original,
                i
            )
            .unwrap());
        }
        assert!(operation.optional("stage").unwrap().is_none());
        fixture.assert_content();
        ensure_ready(fixture.root()).unwrap();
    }

    #[test]
    fn faults_and_cancellation_at_every_rename_restore_original_core() {
        for stop in 1..=4 {
            for cancel_run in [false, true] {
                let fixture = Fixture::new();
                let (root, operation, mut journal) = fixture.transaction();
                let old = journal.files.clone();
                let cancel = AtomicBool::new(false);
                let error = commit(&root, &operation, &mut journal, &cancel, &|moved| {
                    if moved == stop {
                        if cancel_run {
                            cancel.store(true, Ordering::Relaxed);
                        } else {
                            return Err("fixture injected fault".into());
                        }
                    }
                    Ok(())
                })
                .unwrap_err();
                assert!(!error.starts_with("取消清理失败："), "{error}");
                assert_eq!(journal.state, State::RolledBack);
                fixture.assert_original(&old);
                ensure_ready(fixture.root()).unwrap();
            }
        }
    }

    #[test]
    fn every_interrupted_prepared_position_recovers_originals_and_is_idempotent() {
        for moves in 0..=4 {
            let fixture = Fixture::new();
            let (root, operation, mut journal) = fixture.transaction();
            let old = journal.files.clone();
            fixture.crash_after(&root, &operation, &mut journal, moves);
            assert!(ensure_ready(fixture.root())
                .unwrap_err()
                .starts_with("存在未完成的实例重置"));
            assert_eq!(recover_pending(fixture.root()).unwrap()["recovered"], 1);
            fixture.assert_original(&old);
            ensure_ready(fixture.root()).unwrap();
            assert_eq!(recover_pending(fixture.root()).unwrap()["recovered"], 0);
        }
    }

    #[test]
    fn committed_restart_retains_complete_reset_and_finishes_cleanup() {
        let fixture = Fixture::new();
        let (root, operation, mut journal) = fixture.transaction();
        fixture.crash_after(&root, &operation, &mut journal, 4);
        journal.state = State::Committed;
        write_journal(&operation, &journal).unwrap();
        assert!(ensure_ready(fixture.root()).is_err());
        assert_eq!(recover_pending(fixture.root()).unwrap()["recovered"], 1);
        assert_eq!(
            fs::read(fixture.instance().join("fixture.jar")).unwrap(),
            b"replacement jar"
        );
        assert_eq!(
            read_journal(&operation, &journal.operation_id)
                .unwrap()
                .state,
            State::Finished
        );
        fixture.assert_content();
    }

    #[test]
    fn cleanup_failure_is_visible_and_guarded_without_undoing_committed_reset() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let (root, operation, mut journal) = fixture.transaction();
        let stage = operation.child("stage").unwrap();
        symlink(outside.root(), stage.fd_path().join("external")).unwrap();
        let error = commit(
            &root,
            &operation,
            &mut journal,
            &AtomicBool::new(false),
            &|_| Ok(()),
        )
        .unwrap_err();
        assert!(error.starts_with("取消清理失败：重置已完成"), "{error}");
        assert_eq!(journal.state, State::Committed);
        assert_eq!(
            fs::read(fixture.instance().join("fixture.jar")).unwrap(),
            b"replacement jar"
        );
        assert!(recover_pending(fixture.root())
            .unwrap_err()
            .starts_with("取消清理失败："));
        assert_eq!(
            fs::read(outside.instance().join("fixture.jar")).unwrap(),
            b"original jar"
        );
        outside.assert_content();
        fs::remove_file(stage.fd_path().join("external")).unwrap();
        recover_pending(fixture.root()).unwrap();
        ensure_ready(fixture.root()).unwrap();
        fixture.assert_content();
    }

    #[test]
    fn external_edit_during_failure_is_retained_with_original_backup_and_guard() {
        let fixture = Fixture::new();
        let (root, operation, mut journal) = fixture.transaction();
        let error = commit(
            &root,
            &operation,
            &mut journal,
            &AtomicBool::new(false),
            &|moved| {
                if moved == 2 {
                    fs::write(
                        fixture.instance().join("fixture.json"),
                        b"external user edit",
                    )
                    .unwrap();
                    return Err("fixture fault".into());
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.starts_with("取消清理失败："));
        assert_eq!(
            fs::read(fixture.instance().join("fixture.json")).unwrap(),
            b"external user edit"
        );
        assert!(matches_file(
            &operation.child("backup").unwrap(),
            "fixture.json",
            &journal.files[0].original,
            0
        )
        .unwrap());
        assert!(ensure_ready(fixture.root()).is_err());
        assert!(recover_pending(fixture.root()).is_err());
        fixture.assert_content();
    }

    #[test]
    fn detached_instance_is_never_mutated_through_replaced_path_and_can_recover_later() {
        let fixture = Fixture::new();
        let (root, operation, mut journal) = fixture.transaction();
        let old = journal.files.clone();
        let moved_path = fixture.root().join("versions/detached");
        let error = commit(
            &root,
            &operation,
            &mut journal,
            &AtomicBool::new(false),
            &|moved| {
                if moved == 1 {
                    fs::rename(fixture.instance(), &moved_path).unwrap();
                    fs::create_dir(fixture.instance()).unwrap();
                    fs::write(
                        fixture.instance().join("user.txt"),
                        b"replacement directory",
                    )
                    .unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.starts_with("取消清理失败："));
        assert_eq!(
            fs::read(fixture.instance().join("user.txt")).unwrap(),
            b"replacement directory"
        );
        fs::remove_file(fixture.instance().join("user.txt")).unwrap();
        fs::remove_dir(fixture.instance()).unwrap();
        fs::rename(&moved_path, fixture.instance()).unwrap();
        recover_pending(fixture.root()).unwrap();
        fixture.assert_original(&old);
    }

    #[test]
    fn plan_binds_requested_components_content_inode_root_and_dependencies() {
        let fixture = Fixture::new();
        let original = fixture.plan();
        let fabric = prepare(
            fixture.root(),
            "fixture",
            vec![ComponentSelection {
                provider: "Fabric".into(),
                version: "0.16.0".into(),
            }],
        )
        .unwrap();
        assert_ne!(original.revision, fabric.revision);
        assert_eq!(fabric.components[0].provider, "fabric");
        let equal = prepare(
            fixture.root(),
            "fixture",
            vec![ComponentSelection {
                provider: "fabric".into(),
                version: "0.16.0".into(),
            }],
        )
        .unwrap();
        assert_eq!(equal.revision, fabric.revision);
        fs::write(fixture.instance().join("fixture.jar"), b"modified jar").unwrap();
        assert!(execute(
            fixture.root(),
            fixture.root(),
            original,
            &AtomicBool::new(false),
            |_| {}
        )
        .unwrap_err()
        .contains("已变化"));
        let original = fixture.plan();
        fs::create_dir_all(fixture.root().join("versions/dependent")).unwrap();
        fs::write(
            fixture.root().join("versions/dependent/dependent.json"),
            br#"{"id":"dependent","inheritsFrom":"fixture"}"#,
        )
        .unwrap();
        assert!(execute(
            fixture.root(),
            fixture.root(),
            original,
            &AtomicBool::new(false),
            |_| {}
        )
        .unwrap_err()
        .contains("继承或复用"));
        let other = Fixture::new();
        assert!(execute(
            other.root(),
            other.root(),
            fabric,
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err());
        fixture.assert_content();
    }

    #[test]
    fn ambiguous_external_inherited_unsupported_and_linked_core_layouts_are_rejected() {
        for change in [
            json!({"jar":"external"}),
            json!({"inheritsFrom":"base"}),
            json!({"id":"wrong"}),
            json!({"mainClass":"custom.Main"}),
            json!({"patches":[]}),
            json!({"libraries":[{"name":"org.quiltmc:quilt-loader:1"}]}),
        ] {
            let fixture = Fixture::new();
            let mut profile: Value =
                serde_json::from_slice(&fs::read(fixture.instance().join("fixture.json")).unwrap())
                    .unwrap();
            for (key, value) in change.as_object().unwrap() {
                profile[key] = value.clone();
            }
            fixture.profile(profile);
            assert!(prepare(fixture.root(), "fixture", vec![]).is_err());
            fixture.assert_content();
        }
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::remove_file(fixture.instance().join("fixture.jar")).unwrap();
        symlink(
            outside.instance().join("fixture.jar"),
            fixture.instance().join("fixture.jar"),
        )
        .unwrap();
        assert!(prepare(fixture.root(), "fixture", vec![]).is_err());
        fs::remove_file(fixture.instance().join("fixture.jar")).unwrap();
        fs::hard_link(
            outside.instance().join("fixture.jar"),
            fixture.instance().join("fixture.jar"),
        )
        .unwrap();
        assert!(prepare(fixture.root(), "fixture", vec![]).is_err());
        assert!(prepare(fixture.root(), "../fixture", vec![]).is_err());
    }

    #[test]
    fn verified_support_files_publish_without_overwriting_existing_cache() {
        let fixture = Fixture::new();
        let (root, operation, _) = fixture.transaction();
        let stage = operation.child("stage").unwrap();
        let source = stage.create_dir("libraries").unwrap();
        let target = root.create_dir("libraries").unwrap();
        let mut file = source.create_file("new.jar").unwrap();
        file.write_all(b"new support").unwrap();
        file.sync_all().unwrap();
        let mut file = source.create_file("same.jar").unwrap();
        file.write_all(b"same support").unwrap();
        file.sync_all().unwrap();
        let mut file = target.create_file("same.jar").unwrap();
        file.write_all(b"same support").unwrap();
        file.sync_all().unwrap();
        let old = snapshot(target.cache_file("same.jar").unwrap(), MAX_JAR, None).unwrap();
        merge_cache(&source, &target, &AtomicBool::new(false), &mut 0).unwrap();
        assert_eq!(
            snapshot(target.cache_file("same.jar").unwrap(), MAX_JAR, None).unwrap(),
            old
        );
        assert_eq!(
            fs::read(fixture.root().join("libraries/new.jar")).unwrap(),
            b"new support"
        );
        let mut file = source.create_file("conflict.jar").unwrap();
        file.write_all(b"new").unwrap();
        let mut file = target.create_file("conflict.jar").unwrap();
        file.write_all(b"user").unwrap();
        assert!(
            merge_cache(&source, &target, &AtomicBool::new(false), &mut 0)
                .unwrap_err()
                .contains("已保留现有文件")
        );
        assert_eq!(
            fs::read(fixture.root().join("libraries/conflict.jar")).unwrap(),
            b"user"
        );
        fixture.assert_content();
    }

    #[test]
    fn source_or_destination_cache_symlinks_never_escape() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let (root, operation, _) = fixture.transaction();
        let source = operation
            .child("stage")
            .unwrap()
            .create_dir("libraries")
            .unwrap();
        let target = root.create_dir("libraries").unwrap();
        symlink(
            outside.instance().join("fixture.jar"),
            source.fd_path().join("escape.jar"),
        )
        .unwrap();
        assert!(merge_cache(&source, &target, &AtomicBool::new(false), &mut 0).is_err());
        assert!(!fixture.root().join("libraries/escape.jar").exists());
        fs::remove_file(source.fd_path().join("escape.jar")).unwrap();
        let child = source.create_dir("nested").unwrap();
        let mut file = child.create_file("x.jar").unwrap();
        file.write_all(b"cache").unwrap();
        symlink(outside.root(), target.fd_path().join("nested")).unwrap();
        assert!(merge_cache(&source, &target, &AtomicBool::new(false), &mut 0).is_err());
        assert!(!outside.root().join("x.jar").exists());
    }

    #[test]
    fn staging_restart_cleans_only_owned_temporary_tree() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let root = Dir::open(fixture.root()).unwrap();
        let store = storage(&root, true).unwrap().unwrap();
        let (operation, journal) = setup(&root, &store, &plan).unwrap();
        let mut file = operation
            .child("stage")
            .unwrap()
            .create_file("partial")
            .unwrap();
        file.write_all(b"partial").unwrap();
        assert!(ensure_ready(fixture.root()).is_err());
        recover_pending(fixture.root()).unwrap();
        assert_eq!(
            read_journal(&operation, &journal.operation_id)
                .unwrap()
                .state,
            State::RolledBack
        );
        assert!(!operation.fd_path().join("stage").exists());
        fixture.assert_content();
        assert_eq!(
            fs::read(fixture.instance().join("fixture.jar")).unwrap(),
            b"original jar"
        );
    }

    #[test]
    fn anonymous_core_copy_is_registered_before_named_publication_and_recovers() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let root = Dir::open(fixture.root()).unwrap();
        let store = storage(&root, true).unwrap().unwrap();
        let (operation, mut journal) = setup(&root, &store, &plan).unwrap();
        let incoming = operation.child("incoming").unwrap();
        let stage = operation.child("stage").unwrap();
        let mut source = stage.create_file("source.json").unwrap();
        source.write_all(b"complete replacement").unwrap();
        source.sync_all().unwrap();
        copy_core(
            stage.regular("source.json").unwrap(),
            &incoming,
            &operation,
            &mut journal,
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
        let durable = read_journal(&operation, &journal.operation_id).unwrap();
        assert!(matches_file(
            &incoming,
            "fixture.json",
            durable.files[0].replacement.as_ref().unwrap(),
            0
        )
        .unwrap());
        assert_eq!(incoming.names().unwrap(), vec!["fixture.json"]);
        // A crash before link publication has exactly the same durable journal
        // with its registered inode absent from incoming; both states recover.
        incoming.unlink("fixture.json", false).unwrap();
        recover_pending(fixture.root()).unwrap();
        assert_eq!(
            fs::read(fixture.instance().join("fixture.jar")).unwrap(),
            b"original jar"
        );
        fixture.assert_content();
    }

    #[test]
    fn registered_named_core_copy_recovers_and_external_edits_are_preserved() {
        for external in [false, true] {
            let fixture = Fixture::new();
            let plan = fixture.plan();
            let root = Dir::open(fixture.root()).unwrap();
            let store = storage(&root, true).unwrap().unwrap();
            let (operation, mut journal) = setup(&root, &store, &plan).unwrap();
            let incoming = operation.child("incoming").unwrap();
            let stage = operation.child("stage").unwrap();
            let mut source = stage.create_file("source.json").unwrap();
            source.write_all(b"complete replacement").unwrap();
            source.sync_all().unwrap();
            copy_core(
                stage.regular("source.json").unwrap(),
                &incoming,
                &operation,
                &mut journal,
                0,
                &AtomicBool::new(false),
            )
            .unwrap();
            if external {
                fs::write(
                    incoming.fd_path().join("fixture.json"),
                    b"external changed incoming",
                )
                .unwrap();
                assert!(recover_pending(fixture.root())
                    .unwrap_err()
                    .starts_with("取消清理失败："));
                assert_eq!(
                    fs::read(incoming.fd_path().join("fixture.json")).unwrap(),
                    b"external changed incoming"
                );
                assert!(ensure_ready(fixture.root()).is_err());
            } else {
                recover_pending(fixture.root()).unwrap();
                ensure_ready(fixture.root()).unwrap();
            }
            assert_eq!(
                fs::read(fixture.instance().join("fixture.jar")).unwrap(),
                b"original jar"
            );
            fixture.assert_content();
        }
    }

    #[test]
    fn malformed_or_relocated_journals_preserve_core_and_backup() {
        let fixture = Fixture::new();
        let (root, operation, mut journal) = fixture.transaction();
        let old = journal.files.clone();
        fixture.crash_after(&root, &operation, &mut journal, 1);
        let raw = fs::read(operation.fd_path().join("journal.json")).unwrap();
        let mut damaged: Value = serde_json::from_slice(&raw).unwrap();
        damaged["files"][0]["name"] = Value::String("../outside.json".into());
        fs::write(
            operation.fd_path().join("journal.json"),
            serde_json::to_vec(&damaged).unwrap(),
        )
        .unwrap();
        assert!(ensure_ready(fixture.root()).is_err());
        assert!(recover_pending(fixture.root()).is_err());
        assert!(matches_file(
            &operation.child("backup").unwrap(),
            "fixture.json",
            &old[0].original,
            0
        )
        .unwrap());
        fs::write(operation.fd_path().join("journal.json"), raw).unwrap();
        recover_pending(fixture.root()).unwrap();
        fixture.assert_original(&old);
    }
}
