//! Local, snapshot-checked ZIP export. Callers authorize the captured root and
//! serialize this task with game/launcher writers. No source file is changed.
use pcl_core::content_scope::{resolve_scope, IsolationPolicy, MarkerState};
use pcl_install::{InstallStep, Progress};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const MAX_FILES: usize = 200_000;
const MAX_NODES: usize = 400_000;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const MAX_JSON_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 96;
const MAX_EXCLUSIONS: usize = 20_000;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ExportChecks {
    pub game: bool,
    pub game_settings: bool,
    pub game_personal: bool,
    pub mods: bool,
    pub pack_data: bool,
    pub mod_settings: bool,
    pub maps: bool,
    pub jei_personal: bool,
    pub guide_personal: bool,
    pub resourcepacks: bool,
    pub shaderpacks: bool,
    pub screenshots: bool,
    pub saves: bool,
    pub server: bool,
    pub other: bool,
    pub launcher: bool,
    pub bundle_assets: bool,
    pub modrinth: bool,
}

impl Default for ExportChecks {
    fn default() -> Self {
        Self {
            game: true,
            game_settings: true,
            game_personal: false,
            mods: true,
            pack_data: true,
            mod_settings: true,
            maps: false,
            jei_personal: false,
            guide_personal: false,
            resourcepacks: true,
            shaderpacks: true,
            screenshots: false,
            saves: false,
            server: false,
            other: false,
            launcher: false,
            bundle_assets: false,
            modrinth: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRequest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub checks: ExportChecks,
    /// Resource kind -> immediate file/world directory names, never paths.
    #[serde(default)]
    pub excluded: BTreeMap<String, Vec<String>>,
}

impl ExportRequest {
    pub fn validate(&self) -> Result<(), String> {
        for (label, value, limit) in [
            ("整合包名称", self.name.as_str(), 200),
            ("整合包版本", self.version.as_str(), 128),
        ] {
            if value.is_empty()
                || value.trim() != value
                || value.chars().count() > limit
                || value.chars().any(char::is_control)
            {
                return Err(format!(
                    "{label}不能为空、含有前后空格或控制字符，也不能超过 {limit} 个字符"
                ));
            }
        }
        if !self.checks.game {
            return Err("导出必须包含游戏本体".into());
        }
        if self.checks.launcher {
            return Err("暂不支持打包启动器程序，请取消此选项".into());
        }
        if self.checks.modrinth {
            return Err("暂不支持 Modrinth 上传模式，请取消此选项".into());
        }
        let mut count = 0usize;
        for (kind, names) in &self.excluded {
            if ![
                "mods",
                "resourcepacks",
                "shaderpacks",
                "screenshots",
                "saves",
            ]
            .contains(&kind.as_str())
            {
                return Err("导出排除列表的资源类型无效".into());
            }
            count = count.checked_add(names.len()).ok_or("导出排除列表过大")?;
            for name in names {
                component(name)?;
            }
        }
        if count > MAX_EXCLUSIONS {
            return Err("导出排除列表过大".into());
        }
        Ok(())
    }

    fn excluded(&self, kind: &str, name: &str) -> bool {
        self.excluded
            .get(kind)
            .is_some_and(|values| values.iter().any(|value| value == name))
    }
}

/// Its JSON is a reviewable summary, never a serializable authorization token.
#[derive(Clone, Debug, Serialize)]
pub struct ExportPlan {
    pub revision: String,
    pub bytes: u64,
    pub file_count: usize,
    pub request: ExportRequest,
    pub instance_id: String,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub root: PathBuf,
    #[serde(skip)]
    nodes: BTreeMap<String, Node>,
    #[serde(skip)]
    absent: BTreeSet<String>,
    #[serde(skip)]
    entries: BTreeMap<String, Entry>,
    #[serde(skip)]
    manifest: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    fn metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            bytes: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    fn stat(stat: &libc::stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            bytes: stat.st_size.max(0) as u64,
            modified: (stat.st_mtime, stat.st_mtime_nsec),
            changed: (stat.st_ctime, stat.st_ctime_nsec),
        }
    }

    fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFDIR
    }
    fn is_file(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
    }
    fn identity(&self) -> (u64, u64) {
        (self.device, self.inode)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Node {
    stamp: Stamp,
    digest: Option<String>,
}

#[derive(Clone, Debug)]
enum Entry {
    Directory,
    File,
}

struct Dir(File);

fn component(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 255
        || value == "."
        || value == ".."
        || value.contains(['/', '\\', ':', '\0'])
        || value.chars().any(char::is_control)
    {
        return Err("文件名包含不允许的路径字符，无法安全导出".into());
    }
    Ok(())
}

fn relative(value: &str) -> Result<Vec<&str>, String> {
    if value.is_empty() {
        return Ok(vec![]);
    }
    let pieces: Vec<_> = value.split('/').collect();
    if pieces.len() > MAX_DEPTH || value.len() > 4096 {
        return Err("导出目录层级或路径过长".into());
    }
    for name in &pieces {
        component(name)?;
    }
    Ok(pieces)
}

fn cstring(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "路径含有无效字符".into())
}

fn last_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}

impl Dir {
    fn open_absolute(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("导出目录必须是绝对路径".into());
        }
        let slash = cstring("/")?;
        let fd = unsafe {
            libc::open(
                slash.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开导出目录"));
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let name = name.to_str().ok_or("导出目录名称必须是 UTF-8")?;
                    // Existing absolute path components may include colons;
                    // they are never used as archive paths.
                    if name.contains(['\\', '\0']) || name.chars().any(char::is_control) {
                        return Err("导出目录路径无效".into());
                    }
                    dir = dir.child(name)?;
                }
                _ => return Err("导出目录不能包含相对路径跳转".into()),
            }
        }
        Ok(dir)
    }

    fn clone_dir(&self) -> Result<Self, String> {
        self.0
            .try_clone()
            .map(Self)
            .map_err(|error| format!("无法复制目录句柄：{error}"))
    }

    fn child(&self, name: &str) -> Result<Self, String> {
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开目录（符号链接不允许导出）"));
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }

    fn parent(&self, path: &str) -> Result<(Self, String), String> {
        let pieces = relative(path)?;
        let name = pieces.last().ok_or("文件路径不能为空")?.to_string();
        let mut dir = self.clone_dir()?;
        for part in &pieces[..pieces.len() - 1] {
            dir = dir.child(part)?;
        }
        Ok((dir, name))
    }

    fn stamp(&self) -> Result<Stamp, String> {
        self.0
            .metadata()
            .map(|metadata| Stamp::metadata(&metadata))
            .map_err(|error| error.to_string())
    }

    fn metadata(&self, name: &str) -> Result<Option<Stamp>, String> {
        let name = cstring(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(Stamp::stat(&unsafe { stat.assume_init() })));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(format!("无法读取导出文件信息：{error}"))
        }
    }

    fn regular_file(&self, name: &str) -> Result<File, String> {
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(last_error("无法打开导出文件（符号链接不允许导出）"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("导出只允许普通文件和目录".into());
        }
        Ok(file)
    }

    fn open_file(&self, path: &str) -> Result<File, String> {
        let (dir, name) = self.parent(path)?;
        dir.regular_file(&name)
    }

    fn create_file(&self, name: &str) -> Result<File, String> {
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(last_error("无法创建 ZIP 暂存文件"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn names(&self) -> Result<Vec<String>, String> {
        // Use a fresh open file description: dup shares readdir's seek offset.
        let dot = cstring(".")?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(last_error("无法读取导出目录"));
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe { libc::close(fd) };
            return Err(last_error("无法读取导出目录"));
        }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        let mut names = Vec::new();
        loop {
            unsafe { *libc::__errno_location() = 0 };
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if std::io::Error::last_os_error().raw_os_error() != Some(0) {
                    return Err(last_error("读取导出目录失败"));
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| "导出文件名必须是 UTF-8")?
                .to_owned();
            names.push(name);
            if names.len() > MAX_NODES {
                return Err("导出目录包含过多文件".into());
            }
        }
        names.sort();
        Ok(names)
    }

    fn sync(&self) -> Result<(), String> {
        self.0
            .sync_all()
            .map_err(|error| format!("无法保存导出目录：{error}"))
    }
}

fn changed() -> String {
    "导出源文件已经变化，请重新检查内容后再导出".into()
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        Err("导出已取消".into())
    } else {
        Ok(())
    }
}

fn hash_file(file: &mut File, cancel: Option<&AtomicBool>) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    loop {
        if let Some(cancel) = cancel {
            check_cancel(cancel)?;
        }
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("无法读取导出文件：{error}"))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

struct Scanner {
    root: Dir,
    nodes: BTreeMap<String, Node>,
    absent: BTreeSet<String>,
    entries: BTreeMap<String, Entry>,
    bytes: u64,
    files: usize,
}

impl Scanner {
    fn new(root: Dir) -> Result<Self, String> {
        let stamp = root.stamp()?;
        Ok(Self {
            root,
            nodes: BTreeMap::from([(
                String::new(),
                Node {
                    stamp,
                    digest: None,
                },
            )]),
            absent: BTreeSet::new(),
            entries: BTreeMap::new(),
            bytes: 0,
            files: 0,
        })
    }

    fn remember(&mut self, path: &str, node: Node) -> Result<(), String> {
        if let Some(previous) = self.nodes.get(path) {
            if previous != &node {
                return Err(changed());
            }
        } else {
            self.nodes.insert(path.into(), node);
            if self.nodes.len() > MAX_NODES {
                return Err("导出包含过多文件或目录".into());
            }
        }
        Ok(())
    }

    fn directory(&mut self, path: &str) -> Result<Dir, String> {
        let mut dir = self.root.clone_dir()?;
        let mut prefix = String::new();
        self.remember(
            "",
            Node {
                stamp: dir.stamp()?,
                digest: None,
            },
        )?;
        for name in relative(path)? {
            dir = dir.child(name)?;
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            self.remember(
                &prefix,
                Node {
                    stamp: dir.stamp()?,
                    digest: None,
                },
            )?;
        }
        Ok(dir)
    }

    fn metadata(&mut self, path: &str) -> Result<Option<Stamp>, String> {
        let pieces = relative(path)?;
        let name = pieces.last().ok_or("文件路径不能为空")?;
        let parent = pieces[..pieces.len() - 1].join("/");
        let dir = self.directory(&parent)?;
        let stamp = dir.metadata(name)?;
        if stamp.is_none() {
            self.absent.insert(path.into());
        }
        Ok(stamp)
    }

    fn add_directory(&mut self, path: &str) -> Result<(), String> {
        let dir = self.directory(path)?;
        if !dir.stamp()?.is_dir() {
            return Err("导出只允许普通文件和目录".into());
        }
        self.entries.insert(path.into(), Entry::Directory);
        Ok(())
    }

    fn add_file(&mut self, path: &str) -> Result<(), String> {
        if self.entries.contains_key(path) {
            return Ok(());
        }
        let expected = self
            .metadata(path)?
            .ok_or_else(|| format!("缺少导出所需的本地文件：{path}"))?;
        if !expected.is_file() {
            return Err(format!("导出只允许普通文件（不允许符号链接）：{path}"));
        }
        let mut file = self.root.open_file(path)?;
        let before = Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?);
        if before != expected {
            return Err(changed());
        }
        let digest = hash_file(&mut file, None)?;
        if Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?) != expected {
            return Err(changed());
        }
        self.remember(
            path,
            Node {
                stamp: expected.clone(),
                digest: Some(digest),
            },
        )?;
        self.bytes = self
            .bytes
            .checked_add(expected.bytes)
            .filter(|size| *size <= MAX_TOTAL_BYTES)
            .ok_or("导出内容超过 256 GiB 限制")?;
        self.files += 1;
        if self.files > MAX_FILES {
            return Err("导出文件数量过多".into());
        }
        self.entries.insert(path.into(), Entry::File);
        Ok(())
    }

    fn json(&mut self, path: &str) -> Result<Value, String> {
        self.add_file(path)?;
        let expected = &self.nodes[path];
        if expected.stamp.bytes > MAX_JSON_BYTES {
            return Err("版本或资源索引 JSON 文件过大".into());
        }
        let mut file = self.root.open_file(path)?;
        let before = Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?);
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_JSON_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_JSON_BYTES
            || before != expected.stamp
            || Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?) != before
            || Some(format!("{:x}", Sha256::digest(&bytes))) != expected.digest
        {
            return Err(changed());
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("版本或资源索引 JSON 无效：{error}"))?;
        if !value.is_object() {
            return Err("版本或资源索引 JSON 必须是对象".into());
        }
        Ok(value)
    }
}

fn join(base: &str, child: &str) -> String {
    if base.is_empty() {
        child.into()
    } else {
        format!("{base}/{child}")
    }
}

// Categories use explicit locations; unknown folders are offered only by
// "other". Recognized personal paths take precedence over broad config trees.
#[derive(Clone, Copy)]
enum Category {
    Never,
    Traverse,
    GameSettings,
    GamePersonal,
    Mods,
    PackData,
    ModSettings,
    Maps,
    JeiPersonal,
    GuidePersonal,
    Resourcepacks,
    Shaderpacks,
    Screenshots,
    Saves,
    Server,
    Other,
}

fn category(path: &str, directory: bool, root_top_is_directory: bool) -> Category {
    let lower = path.to_ascii_lowercase();
    let parts: Vec<_> = lower.split('/').collect();
    let top = parts[0];
    let name = *parts.last().unwrap_or(&top);
    if parts.iter().any(|part| {
        matches!(
            *part,
            "pcl"
                | ".pcl"
                | ".pcl-linux"
                | ".pcl-rust"
                | ".git"
                | "logs"
                | "crash-reports"
                | "cache"
                | "caches"
                | "natives"
                | "backups"
        ) || part.starts_with(".pcl-")
            || part.starts_with(".install-")
    }) || matches!(
        name,
        "launcher_accounts.json"
            | "launcher_profiles.json"
            | "accounts.json"
            | "account.json"
            | "usercache.json"
            | "usernamecache.json"
            | "session.lock"
            | "launcher_log.txt"
            | "debug.log"
            | "latest.log"
    ) {
        return Category::Never;
    }
    if matches!(
        top,
        "versions"
            | "libraries"
            | "assets"
            | "runtime"
            | "runtimes"
            | "bin"
            | "java"
            | "jre"
            | "jre8"
            | "jre17"
            | "jre21"
    ) {
        return Category::Never;
    }
    if name == "patchouli_data.json" || top == "patchouli_data" {
        return Category::GuidePersonal;
    }
    if (parts.iter().any(|part| *part == "jei")
        && (name.starts_with("bookmarks") || name.starts_with("worldsettings")))
        || matches!(top, "jei_bookmarks.ini" | "jei_bookmarks.json")
    {
        return Category::JeiPersonal;
    }
    if parts.iter().any(|part| {
        matches!(
            *part,
            "xaerowaypoints" | "xaeroworldmap" | "voxelmap" | "voxelmods" | "rei_minimap"
        )
    }) || parts
        .windows(2)
        .any(|pair| pair[0] == "journeymap" && matches!(pair[1], "data" | "server"))
    {
        return Category::Maps;
    }
    if top == "journeymap" {
        return if parts.len() == 1 {
            Category::Traverse
        } else {
            Category::ModSettings
        };
    }
    if matches!(top, "options.txt" | "optionsof.txt" | "optionsshaders.txt") {
        return Category::GameSettings;
    }
    if matches!(top, "hotbar.nbt" | "command_history.txt") {
        return Category::GamePersonal;
    }
    if matches!(top, "servers.dat" | "servers.dat_old") {
        return Category::Server;
    }
    match top {
        "mods" => Category::Mods,
        "resourcepacks" => Category::Resourcepacks,
        "shaderpacks" => Category::Shaderpacks,
        "screenshots" => Category::Screenshots,
        "saves" => Category::Saves,
        "scripts" | "resources" | "resource" | "kubejs" | "datapacks" | "defaultconfigs"
        | "openloader" | "global_packs" | "resource_loader" | "paxi" | "contenttweaker"
        | "patchouli_books" => Category::PackData,
        "config" if parts.len() == 1 => Category::Traverse,
        "config"
            if parts
                .get(1)
                .is_some_and(|part| matches!(*part, "paxi" | "openloader")) =>
        {
            Category::PackData
        }
        "config" => Category::ModSettings,
        _ if root_top_is_directory || (directory && parts.len() == 1) => Category::Other,
        _ => Category::Never,
    }
}

fn selected(category: Category, checks: &ExportChecks) -> bool {
    match category {
        Category::Never | Category::Traverse => false,
        Category::GameSettings => checks.game_settings,
        Category::GamePersonal => checks.game_personal,
        Category::Mods => checks.mods,
        Category::PackData => checks.mods && checks.pack_data,
        Category::ModSettings => checks.mods && checks.mod_settings,
        Category::Maps => checks.mods && checks.maps,
        Category::JeiPersonal => checks.mods && checks.jei_personal,
        Category::GuidePersonal => checks.mods && checks.guide_personal,
        Category::Resourcepacks => checks.resourcepacks,
        Category::Shaderpacks => checks.shaderpacks,
        Category::Screenshots => checks.screenshots,
        Category::Saves => checks.saves,
        Category::Server => checks.server,
        Category::Other => checks.other,
    }
}

fn should_traverse(category: Category, checks: &ExportChecks) -> bool {
    match category {
        Category::Traverse => {
            checks.mods
                && (checks.mod_settings
                    || checks.pack_data
                    || checks.maps
                    || checks.jei_personal
                    || checks.guide_personal)
        }
        // Fine-grained overrides inside config must be examined even when
        // generic mod settings are off.
        Category::ModSettings => {
            checks.mods && (checks.mod_settings || checks.jei_personal || checks.guide_personal)
        }
        _ => selected(category, checks),
    }
}

fn walk_game(
    scanner: &mut Scanner,
    game: &str,
    path: &str,
    request: &ExportRequest,
    top_is_dir: bool,
) -> Result<(), String> {
    let source = join(game, path);
    let stamp = scanner.metadata(&source)?.ok_or_else(changed)?;
    let kind = category(path, stamp.is_dir(), top_is_dir);
    if matches!(kind, Category::Never) {
        return Ok(());
    }
    let parts = relative(path)?;
    if parts.len() >= 2 && request.excluded(parts[0], parts[1]) {
        return Ok(());
    }
    if stamp.is_dir() {
        if !should_traverse(kind, &request.checks) {
            return Ok(());
        }
        if selected(kind, &request.checks) {
            scanner.add_directory(&source)?;
        }
        let directory = scanner.directory(&source)?;
        for name in directory.names()? {
            component(&name)?;
            walk_game(scanner, game, &join(path, &name), request, top_is_dir)?;
        }
        if directory.stamp()? != stamp {
            return Err(changed());
        }
    } else if selected(kind, &request.checks) {
        scanner.add_file(&source)?;
    }
    Ok(())
}

fn inspect_version_strings(value: &Value, root: &Path) -> Result<(), String> {
    // Generated versions occasionally contain an installer machine's absolute
    // paths. Such metadata cannot be transported unchanged.
    let root_text = root.to_str().ok_or("游戏目录必须是 UTF-8")?;
    fn account_argument(flag: &str, value: &str) -> bool {
        matches!(
            flag,
            "--username" | "--uuid" | "--accessToken" | "--clientId" | "--xuid"
        ) && !(value.starts_with("${") && value.ends_with('}'))
    }
    fn visit(value: &Value, root: &str, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("版本 JSON 层级过深".into());
        }
        match value {
            Value::String(text) => {
                if text.contains(root)
                    || text.contains("file://")
                    || text.split(['=', ';', ' ', ',']).any(|token| {
                        token.starts_with('/')
                            || (token.as_bytes().get(1) == Some(&b':')
                                && token
                                    .as_bytes()
                                    .get(2)
                                    .is_some_and(|c| *c == b'\\' || *c == b'/'))
                    })
                {
                    return Err("版本 JSON 含有本机绝对路径，无法生成可移植导出包".into());
                }
                let words: Vec<_> = text.split_whitespace().collect();
                if words
                    .windows(2)
                    .any(|pair| account_argument(pair[0], pair[1]))
                    || words.iter().any(|word| {
                        word.split_once('=')
                            .is_some_and(|(flag, value)| account_argument(flag, value))
                    })
                {
                    return Err("版本 JSON 含有固定账户参数，无法安全导出".into());
                }
            }
            Value::Array(values) => {
                if values.windows(2).any(|pair| {
                    pair[0]
                        .as_str()
                        .zip(pair[1].as_str())
                        .is_some_and(|(flag, value)| account_argument(flag, value))
                }) {
                    return Err("版本 JSON 含有固定账户参数，无法安全导出".into());
                }
                for value in values {
                    visit(value, root, depth + 1)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if [
                        "accessToken",
                        "refreshToken",
                        "clientToken",
                        "accounts",
                        "authenticationDatabase",
                    ]
                    .contains(&key.as_str())
                    {
                        return Err("版本 JSON 含有账户信息，无法安全导出".into());
                    }
                    visit(value, root, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(value, root_text, 0)
}

fn collect_version(
    scanner: &mut Scanner,
    root: &Path,
    id: &str,
    visiting: &mut BTreeSet<String>,
    versions: &mut BTreeMap<String, Value>,
) -> Result<(), String> {
    component(id)?;
    if versions.contains_key(id) {
        return Ok(());
    }
    if visiting.len() >= 64 || !visiting.insert(id.into()) {
        return Err("版本继承关系存在循环或层级过深".into());
    }
    let folder = format!("versions/{id}");
    let data = scanner.json(&format!("{folder}/{id}.json"))?;
    inspect_version_strings(&data, root)?;
    if let Some(parent) = data.get("inheritsFrom") {
        let parent = parent.as_str().ok_or("版本 inheritsFrom 字段无效")?;
        collect_version(scanner, root, parent, visiting, versions)?;
    }
    let jar_path = format!("{folder}/{id}.jar");
    let own_jar = scanner.metadata(&jar_path)?;
    if own_jar.is_some() {
        scanner.add_file(&jar_path)?;
    }
    if let Some(jar) = data.get("jar") {
        let jar = jar.as_str().ok_or("版本 jar 字段无效")?;
        component(jar)?;
        scanner.add_file(&format!("versions/{jar}/{jar}.jar"))?;
        if jar != id && !versions.contains_key(jar) {
            let json_path = format!("versions/{jar}/{jar}.json");
            if scanner.metadata(&json_path)?.is_some() {
                collect_version(scanner, root, jar, visiting, versions)?;
            }
        }
    } else if own_jar.is_none() && data.get("inheritsFrom").is_none() {
        return Err(format!("缺少游戏本体 JAR：{id}"));
    }
    visiting.remove(id);
    versions.insert(id.into(), data);
    Ok(())
}

fn maven_path(name: &str) -> Result<String, String> {
    let (coordinate, extension) = name.split_once('@').unwrap_or((name, "jar"));
    component(extension)?;
    let parts: Vec<_> = coordinate.split(':').collect();
    if !(3..=4).contains(&parts.len()) {
        return Err("版本包含无效的 Maven 组件坐标".into());
    }
    for part in &parts[1..] {
        component(part)?;
    }
    let group = parts[0]
        .split('.')
        .map(|part| {
            component(part)?;
            Ok(part)
        })
        .collect::<Result<Vec<_>, String>>()?
        .join("/");
    let classifier = parts
        .get(3)
        .map(|part| format!("-{part}"))
        .unwrap_or_default();
    let path = format!(
        "{group}/{}/{}/{}-{}{classifier}.{extension}",
        parts[1], parts[2], parts[1], parts[2]
    );
    relative(&path)?;
    Ok(path)
}

fn library_applies(library: &Value) -> Result<bool, String> {
    let Some(rules) = library.get("rules") else {
        return Ok(true);
    };
    let rules = rules.as_array().ok_or("组件规则无效")?;
    let mut allowed = false;
    for rule in rules {
        let os = &rule["os"];
        let mut matches = os["name"].as_str().is_none_or(|name| name == "linux");
        if let Some(arch) = os["arch"].as_str() {
            matches &= match std::env::consts::ARCH {
                "x86_64" => matches!(arch, "x86_64" | "amd64" | "x64"),
                "x86" => matches!(arch, "x86" | "i386" | "i686"),
                "aarch64" => matches!(arch, "aarch64" | "arm64"),
                current => arch == current,
            };
        }
        // Features are false during ordinary exports (no demo/custom window).
        if let Some(features) = rule["features"].as_object() {
            matches &= features
                .values()
                .all(|value| value.as_bool() == Some(false));
        }
        if os.get("version").is_some() && matches {
            return Err("打包资源暂不支持带有操作系统版本条件的组件规则".into());
        }
        if matches {
            allowed = match rule["action"].as_str() {
                Some("allow") => true,
                Some("disallow") => false,
                _ => return Err("组件规则动作无效".into()),
            };
        }
    }
    Ok(allowed)
}

fn add_library(
    scanner: &mut Scanner,
    artifact: &Value,
    fallback: Option<&str>,
) -> Result<(), String> {
    let path = match artifact["path"].as_str() {
        Some(path) => path.to_string(),
        None => maven_path(fallback.ok_or("组件缺少本地文件路径")?)?,
    };
    if relative(&path)?.is_empty() {
        return Err("组件本地文件路径无效".into());
    }
    scanner.add_file(&format!("libraries/{path}"))
}

fn bundle_assets(scanner: &mut Scanner, versions: &BTreeMap<String, Value>) -> Result<(), String> {
    let mut indexes = BTreeSet::new();
    for data in versions.values() {
        for library in data["libraries"].as_array().into_iter().flatten() {
            if !library_applies(library)? {
                continue;
            }
            let downloads = &library["downloads"];
            if !downloads["artifact"].is_null() {
                add_library(scanner, &downloads["artifact"], library["name"].as_str())?;
            } else if downloads.is_null()
                || (downloads.get("classifiers").is_none() && library["name"].is_string())
            {
                add_library(scanner, &Value::Null, library["name"].as_str())?;
            }
            if let Some(classifier) = library["natives"]["linux"].as_str() {
                let classifier = classifier.replace(
                    "${arch}",
                    if cfg!(target_pointer_width = "64") {
                        "64"
                    } else {
                        "32"
                    },
                );
                let artifact = &downloads["classifiers"][&classifier];
                if artifact.is_null() {
                    return Err("Linux 本地库缺少文件声明，无法完整打包资源".into());
                }
                add_library(scanner, artifact, None)?;
            }
        }
        if let Some(index) = data["assetIndex"]["id"]
            .as_str()
            .or_else(|| data["assets"].as_str())
        {
            component(index)?;
            indexes.insert(index.to_string());
        }
        // Legacy logging configuration is a referenced runtime resource.
        if let Some(id) = data["logging"]["client"]["file"]["id"].as_str() {
            component(id)?;
            scanner.add_file(&format!("assets/log_configs/{id}"))?;
        }
    }
    for index in indexes {
        let data = scanner.json(&format!("assets/indexes/{index}.json"))?;
        let objects = data["objects"].as_object().ok_or("资源索引缺少 objects")?;
        let virtual_assets = data["virtual"].as_bool().unwrap_or(false);
        let map_to_resources = data["map_to_resources"].as_bool().unwrap_or(false);
        for (name, descriptor) in objects {
            let hash = descriptor["hash"].as_str().ok_or("资源索引缺少对象哈希")?;
            if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("资源索引对象哈希无效".into());
            }
            let hash = hash.to_ascii_lowercase();
            scanner.add_file(&format!("assets/objects/{}/{hash}", &hash[..2]))?;
            if virtual_assets || map_to_resources {
                if relative(name)?.is_empty() {
                    return Err("旧版资源路径无效".into());
                }
                if virtual_assets {
                    scanner.add_file(&format!("assets/virtual/{index}/{name}"))?;
                }
                if map_to_resources {
                    scanner.add_file(&format!("resources/{name}"))?;
                }
            }
        }
    }
    // Forge/NeoForge bootstrap code also locates processor outputs that do not
    // occur in the launch JSON's libraries array. Include the local library
    // tree conservatively rather than omitting generated runtime artifacts.
    if scanner.metadata("libraries")?.is_some() {
        bundle_tree(scanner, "libraries")?;
    }
    Ok(())
}

fn bundle_tree(scanner: &mut Scanner, path: &str) -> Result<(), String> {
    relative(path)?;
    let lower = path.to_ascii_lowercase();
    let parts: Vec<_> = lower.split('/').collect();
    if parts
        .iter()
        .any(|name| name.starts_with(".pcl-") || matches!(*name, ".git" | "pcl" | ".pcl"))
        || parts.last().is_some_and(|name| {
            matches!(
                *name,
                "accounts.json"
                    | "account.json"
                    | "launcher_accounts.json"
                    | "launcher_profiles.json"
                    | "usercache.json"
                    | "usernamecache.json"
            )
        })
    {
        return Ok(());
    }
    let stamp = scanner.metadata(path)?.ok_or_else(changed)?;
    if stamp.is_dir() {
        scanner.add_directory(path)?;
        let directory = scanner.directory(path)?;
        for name in directory.names()? {
            component(&name)?;
            bundle_tree(scanner, &join(path, &name))?;
        }
        if directory.stamp()? != stamp {
            return Err(changed());
        }
    } else {
        scanner.add_file(path)?;
    }
    Ok(())
}

fn manifest(
    id: &str,
    request: &ExportRequest,
    versions: &BTreeMap<String, Value>,
    isolated: bool,
) -> Result<Vec<u8>, String> {
    let mut current = id;
    let game = loop {
        let data = &versions[current];
        if let Some(version) = data["clientVersion"].as_str().or_else(|| {
            data["arguments"]["game"]
                .as_array()?
                .windows(2)
                .find(|pair| pair[0].as_str() == Some("--fml.mcVersion"))?
                .get(1)?
                .as_str()
        }) {
            break version;
        }
        if let Some(parent) = data["inheritsFrom"].as_str() {
            current = parent;
        } else {
            break data["id"].as_str().unwrap_or(current);
        }
    };
    let mut loaders = BTreeSet::new();
    for data in versions.values() {
        for library in data["libraries"].as_array().into_iter().flatten() {
            let Some(name) = library["name"].as_str() else {
                continue;
            };
            let parts: Vec<_> = name.split(':').collect();
            if parts.len() < 3 {
                continue;
            }
            let loader = match (parts[0], parts[1]) {
                ("net.fabricmc", "fabric-loader") => "Fabric",
                ("org.quiltmc", "quilt-loader") => "Quilt",
                ("net.minecraftforge", "forge" | "fmlloader") => "Forge",
                ("net.neoforged", "neoforge" | "forge") => "NeoForge",
                ("optifine", _) => "OptiFine",
                _ => continue,
            };
            loaders.insert((loader, parts[2].to_string()));
        }
    }
    let loaders: Vec<_> = loaders
        .into_iter()
        .map(|(name, version)| json!({"name":name,"version":version}))
        .collect();
    serde_json::to_vec_pretty(&json!({
        "format":"pcl-local-instance", "format_version":1,
        "name":request.name, "version":request.version,
        "game":{"minecraft":game,"instance":id,"isolated":isolated},
        "loaders":loaders, "bundled_assets":request.checks.bundle_assets,
        "content_root":if isolated { format!(".minecraft/versions/{id}") } else { ".minecraft".into() }
    })).map_err(|error| error.to_string())
}

#[cfg(test)]
pub fn prepare(root: &Path, id: &str, request: ExportRequest) -> Result<ExportPlan, String> {
    prepare_with_isolation(root, id, request, IsolationPolicy::Automatic, "")
}

pub fn prepare_with_isolation(
    root: &Path,
    id: &str,
    request: ExportRequest,
    policy: IsolationPolicy,
    settings_revision: &str,
) -> Result<ExportPlan, String> {
    request.validate()?;
    component(id)?;
    let mut scanner = Scanner::new(Dir::open_absolute(root)?)?;
    let mut versions = BTreeMap::new();
    collect_version(&mut scanner, root, id, &mut BTreeSet::new(), &mut versions)?;
    let instance_dir = format!("versions/{id}");
    let scope = resolve_scope(policy, |marker| {
        Ok(match scanner.metadata(&join(&instance_dir, marker))? {
            None => MarkerState::Missing,
            Some(stamp) if stamp.is_dir() || stamp.is_file() => MarkerState::Present,
            Some(_) => MarkerState::Unsupported,
        })
    })?;
    let isolated = scope.is_isolated();
    let game = if isolated { instance_dir.as_str() } else { "" };
    let game_dir = scanner.directory(game)?;
    let game_stamp = game_dir.stamp()?;
    for name in game_dir.names()? {
        // Mandatory metadata was already captured. Other version files are
        // deliberately excluded rather than treated as unknown content.
        if isolated && (name == format!("{id}.json") || name == format!("{id}.jar")) {
            continue;
        }
        let stamp = game_dir.metadata(&name)?.ok_or_else(changed)?;
        let kind = category(&name, stamp.is_dir(), stamp.is_dir());
        if matches!(kind, Category::Never) {
            continue;
        }
        component(&name)?;
        walk_game(&mut scanner, game, &name, &request, stamp.is_dir())?;
    }
    if game_dir.stamp()? != game_stamp {
        return Err(changed());
    }
    if request.checks.bundle_assets {
        bundle_assets(&mut scanner, &versions)?;
    }
    let manifest = manifest(id, &request, &versions, isolated)?;
    let mut digest = Sha256::new();
    digest.update(b"pcl-local-export-plan-v2\0");
    digest.update(
        serde_json::to_vec(&(policy, settings_revision)).map_err(|error| error.to_string())?,
    );
    // Include the captured physical scope, but expose only its opaque hash.
    digest.update(root.as_os_str().as_bytes());
    digest.update(id.as_bytes());
    digest.update(serde_json::to_vec(&request).map_err(|error| error.to_string())?);
    digest.update(serde_json::to_vec(&scanner.nodes).map_err(|error| error.to_string())?);
    digest.update(serde_json::to_vec(&scanner.absent).map_err(|error| error.to_string())?);
    digest.update(&manifest);
    let mut warnings = Vec::new();
    if request.checks.mods || request.checks.other {
        warnings.push("模组个人信息按已知文件位置区分；自定义位置和其他文件夹中的个人数据请在导出前自行检查。".into());
    }
    if request.checks.bundle_assets {
        warnings.push("为保留 Forge / NeoForge 生成的运行文件，打包资源包含整个本地 libraries 目录，可能包含其他实例的依赖。".into());
        warnings.push(
            "打包资源包含当前 Linux 平台声明的本地依赖；其他平台的原生库可能仍需下载。".into(),
        );
    }
    let plan = ExportPlan {
        revision: format!("{:x}", digest.finalize()),
        bytes: scanner.bytes,
        file_count: scanner.files,
        request,
        instance_id: id.into(),
        warnings,
        root: root.into(),
        nodes: scanner.nodes,
        absent: scanner.absent,
        entries: scanner.entries,
        manifest,
    };
    // Refuse a plan assembled across concurrent edits, including files scanned
    // earlier than a long mod/resource tree.
    validate_snapshot(&plan, &AtomicBool::new(false), false, None::<&fn(Progress)>)?;
    Ok(plan)
}

fn validate_snapshot(
    plan: &ExportPlan,
    cancel: &AtomicBool,
    hashes: bool,
    callback: Option<&(impl Fn(Progress) + Send + Sync)>,
) -> Result<(), String> {
    let root = Dir::open_absolute(&plan.root).map_err(|_| changed())?;
    let mut completed = 0u64;
    for (path, node) in &plan.nodes {
        check_cancel(cancel)?;
        let actual = if path.is_empty() {
            root.stamp()?
        } else {
            let (dir, name) = root.parent(path).map_err(|_| changed())?;
            dir.metadata(&name)?.ok_or_else(changed)?
        };
        if actual != node.stamp {
            return Err(changed());
        }
        if hashes && node.digest.is_some() {
            let mut file = root.open_file(path).map_err(|_| changed())?;
            if Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?) != node.stamp
                || Some(hash_file(&mut file, Some(cancel))?) != node.digest
                || Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?)
                    != node.stamp
            {
                return Err(changed());
            }
            completed += 1;
            if let Some(callback) = callback {
                callback(progress(
                    "export-scan",
                    "正在检查导出源文件",
                    completed,
                    plan.file_count as u64,
                    0,
                    plan.bytes,
                    false,
                ));
            }
        }
    }
    for path in &plan.absent {
        check_cancel(cancel)?;
        let (dir, name) = root.parent(path).map_err(|_| changed())?;
        if dir.metadata(&name)?.is_some() {
            return Err(changed());
        }
    }
    // Recheck metadata after all hashes. A previously checked directory or
    // file may have changed while a later, larger file was being read.
    if hashes {
        validate_snapshot(plan, cancel, false, None::<&fn(Progress)>)?;
    }
    Ok(())
}

fn progress(
    stage: &str,
    message: &str,
    completed: u64,
    total: u64,
    bytes_done: u64,
    bytes_total: u64,
    done: bool,
) -> Progress {
    let stages = [
        ("export-scan", "检查导出文件"),
        ("export-archive", "写入 ZIP 文件"),
        ("export-finalize", "完成导出"),
    ];
    let current = stages.iter().position(|(id, _)| *id == stage).unwrap_or(0);
    Progress {
        steps: stages
            .iter()
            .enumerate()
            .map(|(index, (id, label))| InstallStep {
                id: (*id).into(),
                label: (*label).into(),
                state: if done || index < current {
                    "complete"
                } else if index == current {
                    "running"
                } else {
                    "pending"
                }
                .into(),
                progress: if done || index < current {
                    Some(1.0)
                } else if index == current {
                    Some(if stage == "export-archive" && bytes_total > 0 {
                        bytes_done as f64 / bytes_total as f64
                    } else if total > 0 {
                        completed as f64 / total as f64
                    } else {
                        0.0
                    })
                } else {
                    None
                },
            })
            .collect(),
        stage: stage.into(),
        message: message.into(),
        // Task statistics count source files awaiting archival. The scan row
        // keeps its own fraction while these global counters stay monotonic.
        completed: match stage {
            "export-scan" => 0,
            "export-finalize" => total,
            _ => completed,
        },
        total,
        bytes_done,
        bytes_total,
        network_bytes: 0,
    }
}

struct Stage {
    parent: Dir,
    name: String,
    identity: (u64, u64),
    armed: bool,
}

impl Stage {
    fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        let Some(stamp) = self.parent.metadata(&self.name)? else {
            self.armed = false;
            return Ok(());
        };
        if stamp.identity() != self.identity {
            return Err("ZIP 暂存文件被外部替换，请检查目标目录".into());
        }
        let name = cstring(&self.name)?;
        if unsafe { libc::unlinkat(self.parent.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(last_error("无法清理 ZIP 暂存文件"));
        }
        self.armed = false;
        self.parent.sync()
    }

    fn publish(&mut self, destination: &str) -> Result<(), String> {
        if self
            .parent
            .metadata(&self.name)?
            .is_none_or(|stamp| stamp.identity() != self.identity || !stamp.is_file())
        {
            return Err("ZIP 暂存文件已变化，无法发布".into());
        }
        let source = cstring(&self.name)?;
        let destination = cstring(destination)?;
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.parent.0.as_raw_fd(),
                source.as_ptr(),
                self.parent.0.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                return Err("目标文件已经存在，请使用新的文件名".into());
            }
            return Err(format!(
                "无法原子发布 ZIP 文件（目标文件系统须支持不覆盖重命名）：{error}"
            ));
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn destination_parent(plan: &ExportPlan, destination: &Path) -> Result<(Dir, String), String> {
    if !destination.is_absolute() {
        return Err("ZIP 目标必须是绝对路径".into());
    }
    if destination.starts_with(&plan.root) {
        return Err("ZIP 目标不能放在正在导出的游戏目录内".into());
    }
    let parent_path = destination.parent().ok_or("ZIP 目标目录无效")?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("ZIP 目标文件名必须是 UTF-8")?;
    component(name)?;
    if !name.to_ascii_lowercase().ends_with(".zip") {
        return Err("导出目标文件须使用 .zip 扩展名".into());
    }
    let parent = Dir::open_absolute(parent_path)?;
    // Also reject an alias/bind mount into any captured source directory.
    let input_ids: BTreeSet<_> = plan
        .nodes
        .values()
        .filter(|node| node.stamp.is_dir())
        .map(|node| node.stamp.identity())
        .collect();
    let mut ancestor = Some(parent_path);
    while let Some(path) = ancestor {
        if input_ids.contains(&Dir::open_absolute(path)?.stamp()?.identity()) {
            return Err("ZIP 目标目录与导出源目录重合".into());
        }
        ancestor = path.parent();
    }
    if parent.metadata(name)?.is_some() {
        return Err("目标文件已经存在，请使用新的文件名".into());
    }
    Ok((parent, name.into()))
}

#[cfg(test)]
pub fn execute(
    plan: ExportPlan,
    destination: &Path,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
) -> Result<Value, String> {
    execute_with_guard(plan, destination, cancel, callback, || Ok(()))
}

/// The desktop supplies the captured configuration authority after slow ZIP
/// work. Its guard stays owned until the final destination link is published.
pub fn execute_with_guard<G>(
    plan: ExportPlan,
    destination: &Path,
    cancel: &AtomicBool,
    callback: impl Fn(Progress) + Send + Sync,
    authorize: impl Fn() -> Result<G, String>,
) -> Result<Value, String> {
    check_cancel(cancel)?;
    callback(progress(
        "export-scan",
        "正在检查导出源文件",
        0,
        plan.file_count as u64,
        0,
        plan.bytes,
        false,
    ));
    validate_snapshot(&plan, cancel, true, Some(&callback))?;
    let (parent, filename) = destination_parent(&plan, destination)?;
    check_cancel(cancel)?;
    let nonce = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let name = format!(".pcl-export-{}-{now}-{nonce}.tmp", std::process::id());
    let file = parent.create_file(&name)?;
    let identity = Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?).identity();
    let mut stage = Stage {
        parent,
        name,
        identity,
        armed: true,
    };
    let result = (|| {
        let root = Dir::open_absolute(&plan.root)?;
        let mut zip = ZipWriter::new(file);
        let mut bytes_done = 0u64;
        let mut files_done = 0u64;
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644)
            .large_file(true);
        callback(progress(
            "export-archive",
            "正在写入 ZIP 文件",
            0,
            plan.file_count as u64,
            0,
            plan.bytes,
            false,
        ));
        for (path, entry) in &plan.entries {
            check_cancel(cancel)?;
            let archive_path = format!(".minecraft/{path}");
            match entry {
                Entry::Directory => {
                    zip.add_directory(format!("{archive_path}/"), options.unix_permissions(0o755))
                        .map_err(|error| format!("无法写入 ZIP 目录：{error}"))?;
                }
                Entry::File => {
                    let node = &plan.nodes[path];
                    let mut file = root.open_file(path).map_err(|_| changed())?;
                    if Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?)
                        != node.stamp
                    {
                        return Err(changed());
                    }
                    zip.start_file(&archive_path, options)
                        .map_err(|error| format!("无法写入 ZIP 文件：{error}"))?;
                    let mut digest = Sha256::new();
                    let mut buffer = [0u8; 128 * 1024];
                    let mut copied = 0u64;
                    loop {
                        check_cancel(cancel)?;
                        let count = file
                            .read(&mut buffer)
                            .map_err(|error| format!("无法读取导出文件：{error}"))?;
                        if count == 0 {
                            break;
                        }
                        copied = copied.checked_add(count as u64).ok_or_else(changed)?;
                        if copied > node.stamp.bytes {
                            return Err(changed());
                        }
                        digest.update(&buffer[..count]);
                        zip.write_all(&buffer[..count])
                            .map_err(|error| format!("无法写入 ZIP 内容：{error}"))?;
                        bytes_done += count as u64;
                        callback(progress(
                            "export-archive",
                            "正在写入 ZIP 文件",
                            files_done,
                            plan.file_count as u64,
                            bytes_done,
                            plan.bytes,
                            false,
                        ));
                    }
                    if copied != node.stamp.bytes
                        || Some(format!("{:x}", digest.finalize())) != node.digest
                        || Stamp::metadata(&file.metadata().map_err(|error| error.to_string())?)
                            != node.stamp
                    {
                        return Err(changed());
                    }
                    files_done += 1;
                }
            }
        }
        check_cancel(cancel)?;
        zip.start_file("pcl-export.json", options)
            .map_err(|error| error.to_string())?;
        zip.write_all(&plan.manifest)
            .map_err(|error| error.to_string())?;
        callback(progress(
            "export-finalize",
            "正在完成 ZIP 文件并复核源文件",
            0,
            plan.file_count as u64,
            bytes_done,
            plan.bytes,
            false,
        ));
        let mut file = zip
            .finish()
            .map_err(|error| format!("无法完成 ZIP 文件：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("无法保存 ZIP 文件：{error}"))?;
        let archive_bytes = file.metadata().map_err(|error| error.to_string())?.len();
        let archive_hash = hash_file(&mut file, Some(cancel))?;
        // Rehash after archiving: a file that was changed after its own copy
        // cannot silently leave an otherwise successful snapshot behind.
        validate_snapshot(&plan, cancel, true, None::<&fn(Progress)>)?;
        stage.parent.sync()?;
        // Check the destination directory still occupies its captured pathname.
        let current_parent = Dir::open_absolute(destination.parent().ok_or("ZIP 目标目录无效")?)?;
        if current_parent.stamp()?.identity() != stage.parent.stamp()?.identity() {
            return Err("ZIP 目标目录已经变化".into());
        }
        {
            let _authority = authorize()?;
            validate_snapshot(&plan, cancel, false, None::<&fn(Progress)>)?;
            check_cancel(cancel)?;
            stage.publish(&filename)?;
        }
        let mut warnings = plan.warnings.clone();
        let warning = stage.parent.sync().err().map(|error| {
            // Publication already succeeded. Report its durability warning as
            // success rather than falsely claiming that no output was made.
            format!("ZIP 已创建，但目标目录同步失败：{error}")
        });
        if let Some(warning) = &warning {
            warnings.push(warning.clone());
        }
        callback(progress(
            "export-finalize",
            "导出完成",
            1,
            plan.file_count as u64,
            bytes_done,
            plan.bytes,
            true,
        ));
        Ok(
            json!({"destination":destination.to_string_lossy(), "file_name":filename,
            "file_count":plan.file_count, "bytes":plan.bytes, "archive_bytes":archive_bytes,
            "sha256":archive_hash, "revision":plan.revision, "warnings":warnings, "warning":warning}),
        )
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => match stage.cleanup() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{error}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Mutex};
    use zip::ZipArchive;

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
    }

    impl Fixture {
        fn new(isolated: bool) -> Self {
            let number = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let base = std::env::current_dir()
                .unwrap()
                .join("target/instance-export-fixtures")
                .join(format!("{}-{number}", std::process::id()));
            let root = base.join("game");
            fs::create_dir_all(&root).unwrap();
            let fixture = Self { base, root };
            fixture.write(
                "versions/base/base.json",
                br#"{"id":"base","clientVersion":"1.20.1","libraries":[]}"#,
            );
            fixture.write("versions/base/base.jar", b"client jar fixture");
            fixture.write("versions/example/example.json", br#"{"id":"example","inheritsFrom":"base","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]}"#);
            if isolated {
                fixture.write("versions/example/options.txt", b"music:0.5");
            }
            fixture
        }

        fn write(&self, path: &str, bytes: &[u8]) {
            let path = self.root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }

        fn game_write(&self, path: &str, bytes: &[u8], isolated: bool) {
            self.write(
                &if isolated {
                    format!("versions/example/{path}")
                } else {
                    path.into()
                },
                bytes,
            );
        }

        fn destination(&self) -> PathBuf {
            self.base.join("export.zip")
        }

        fn plan(&self, request: ExportRequest) -> ExportPlan {
            prepare(&self.root, "example", request).unwrap()
        }

        fn clean_output(&self) {
            assert!(!self.destination().exists());
            let entries: Vec<_> = fs::read_dir(&self.base)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(entries, vec![std::ffi::OsString::from("game")]);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn request() -> ExportRequest {
        ExportRequest {
            name: "Example Pack".into(),
            version: "1.0.0".into(),
            checks: ExportChecks::default(),
            excluded: BTreeMap::new(),
        }
    }

    fn unzip(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut zip = ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut entries = BTreeMap::new();
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).unwrap();
            assert!(entry.enclosed_name().is_some());
            assert!(!entry.is_symlink());
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert!(entries.insert(entry.name().to_string(), bytes).is_none());
        }
        entries
    }

    #[test]
    fn default_zip_filters_personal_files_and_preserves_manifest_content_and_hash() {
        let fixture = Fixture::new(true);
        for path in [
            "mods/example.jar",
            "config/settings.toml",
            "config/jei/jei-client.ini",
            "scripts/init.zs",
            "patchouli_books/example/book.json",
            "resourcepacks/texture.zip",
            "shaderpacks/shader.zip",
        ] {
            fixture.game_write(path, path.as_bytes(), true);
        }
        for path in [
            "hotbar.nbt",
            "command_history.txt",
            "config/jei/bookmarks.ini",
            "config/jei/worldSettings.ini",
            "config/patchouli_data.json",
            "patchouli_data.json",
            "XaeroWaypoints/server/points.txt",
            "XaeroWorldMap/server/tile.zip",
            "journeymap/data/server/tile.png",
            "config/voxelmap/server.points",
            "screenshots/image.png",
            "saves/world/level.dat",
            "servers.dat",
            "other/private.txt",
            "PCL/accounts.json",
            "logs/latest.log",
            "usercache.json",
        ] {
            fixture.game_write(path, b"private fixture", true);
        }
        let plan = fixture.plan(request());
        let expected_files = plan.file_count;
        let expected_bytes = plan.bytes;
        let serialized = serde_json::to_string(&plan).unwrap();
        assert!(!serialized.contains(fixture.root.to_str().unwrap()));
        assert!(!serialized.contains("inode"));
        let progress_log = Mutex::new(Vec::new());
        let result = execute(plan, &fixture.destination(), &AtomicBool::new(false), |p| {
            progress_log.lock().unwrap().push(p);
        })
        .unwrap();
        let entries = unzip(&fixture.destination());
        assert_eq!(
            entries[".minecraft/versions/base/base.jar"],
            b"client jar fixture"
        );
        assert_eq!(
            entries[".minecraft/versions/example/config/settings.toml"],
            b"config/settings.toml"
        );
        assert!(entries.contains_key(".minecraft/versions/example/config/jei/jei-client.ini"));
        assert!(entries.keys().all(|path| ![
            "hotbar",
            "command_history",
            "bookmarks",
            "worldSettings",
            "patchouli_data",
            "Xaero",
            "journeymap/data",
            "voxelmap",
            "screenshots",
            "saves",
            "servers.dat",
            "accounts",
            "logs",
            "usercache",
            "other"
        ]
        .iter()
        .any(|needle| path.contains(needle))));
        let manifest: Value = serde_json::from_slice(&entries["pcl-export.json"]).unwrap();
        assert_eq!(manifest["name"], "Example Pack");
        assert_eq!(manifest["version"], "1.0.0");
        assert_eq!(manifest["game"]["minecraft"], "1.20.1");
        assert_eq!(manifest["loaders"][0]["name"], "Fabric");
        assert!(!String::from_utf8(entries["pcl-export.json"].clone())
            .unwrap()
            .contains(fixture.root.to_str().unwrap()));
        assert_eq!(result["file_count"], expected_files);
        assert_eq!(result["bytes"], expected_bytes);
        assert_eq!(
            result["sha256"],
            format!(
                "{:x}",
                Sha256::digest(fs::read(fixture.destination()).unwrap())
            )
        );
        let progress_log = progress_log.lock().unwrap();
        assert_eq!(progress_log.first().unwrap().stage, "export-scan");
        assert_eq!(progress_log.last().unwrap().bytes_done, expected_bytes);
        assert!(progress_log
            .windows(2)
            .all(|pair| pair[0].completed <= pair[1].completed));
        assert!(progress_log
            .iter()
            .all(|p| p.total == expected_files as u64));
        assert!(progress_log
            .iter()
            .filter(|p| p.stage == "export-scan")
            .all(|p| p.completed == 0));
        assert!(progress_log.iter().all(|p| p.network_bytes == 0));
        assert!(progress_log
            .last()
            .unwrap()
            .steps
            .iter()
            .all(|step| step.state == "complete"));
    }

    #[test]
    fn selected_personal_categories_excluded_resources_and_empty_worlds() {
        let fixture = Fixture::new(true);
        for path in [
            "hotbar.nbt",
            "command_history.txt",
            "config/jei/bookmarks.ini",
            "patchouli_data.json",
            "XaeroWaypoints/server/points.txt",
            "journeymap/data/server/tile.png",
            "screenshots/keep.png",
            "screenshots/skip.png",
            "resourcepacks/keep.zip",
            "resourcepacks/skip.zip",
            "shaderpacks/skip.zip",
            "mods/keep.jar",
            "mods/skip.jar",
            "saves/keep/level.dat",
            "saves/skip/level.dat",
            "servers.dat",
            "other/note.txt",
            "other/accounts.json",
        ] {
            fixture.game_write(path, path.as_bytes(), true);
        }
        fs::create_dir_all(fixture.root.join("versions/example/saves/empty")).unwrap();
        let mut request = request();
        request.checks.game_personal = true;
        request.checks.maps = true;
        request.checks.jei_personal = true;
        request.checks.guide_personal = true;
        request.checks.screenshots = true;
        request.checks.saves = true;
        request.checks.server = true;
        request.checks.other = true;
        request.excluded = BTreeMap::from([
            ("mods".into(), vec!["skip.jar".into()]),
            ("saves".into(), vec!["skip".into()]),
            ("resourcepacks".into(), vec!["skip.zip".into()]),
            ("shaderpacks".into(), vec!["skip.zip".into()]),
            ("screenshots".into(), vec!["skip.png".into()]),
        ]);
        execute(
            fixture.plan(request),
            &fixture.destination(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let entries = unzip(&fixture.destination());
        assert!(entries
            .keys()
            .all(|path| !path.contains("skip") && !path.contains("accounts")));
        for path in [
            "hotbar.nbt",
            "command_history.txt",
            "config/jei/bookmarks.ini",
            "patchouli_data.json",
            "XaeroWaypoints/server/points.txt",
            "journeymap/data/server/tile.png",
            "screenshots/keep.png",
            "saves/keep/level.dat",
            "servers.dat",
            "other/note.txt",
            "saves/empty/",
        ] {
            assert!(
                entries.contains_key(&format!(".minecraft/versions/example/{path}")),
                "missing {path}"
            );
        }
    }

    #[test]
    fn global_game_paths_and_mod_suboptions_follow_parent_checkbox() {
        let fixture = Fixture::new(false);
        for path in [
            "options.txt",
            "mods/example.jar",
            "config/settings.toml",
            "scripts/example.zs",
            "resourcepacks/pack.zip",
        ] {
            fixture.game_write(path, path.as_bytes(), false);
        }
        let mut request = request();
        request.checks.mods = false;
        request.checks.other = true;
        execute(
            fixture.plan(request),
            &fixture.destination(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let entries = unzip(&fixture.destination());
        assert!(entries.contains_key(".minecraft/options.txt"));
        assert!(entries.contains_key(".minecraft/resourcepacks/pack.zip"));
        assert!(entries.contains_key(".minecraft/versions/example/example.json"));
        assert!(!entries.keys().any(|path| path.contains("/mods/")
            || path.contains("/config/")
            || path.contains("/scripts/")));
    }

    #[test]
    fn personal_files_never_fall_back_to_mod_settings_or_other() {
        let fixture = Fixture::new(true);
        for path in [
            "other/patchouli_data.json",
            "other/jei/bookmarks.ini",
            "other/XaeroWaypoints/server/points.txt",
            "config/jei/worldSettings.ini",
            "config/jei/settings.ini",
            "config/voxelmap/server.points",
        ] {
            fixture.game_write(path, b"fixture", true);
        }
        let mut request = request();
        request.checks.other = true;
        let plan = fixture.plan(request);
        assert!(!plan
            .entries
            .keys()
            .any(|path| path.contains("patchouli_data")
                || path.contains("bookmarks")
                || path.contains("worldSettings")
                || path.contains("server.points")
                || path.contains("XaeroWaypoints")));
        assert!(plan
            .entries
            .contains_key("versions/example/config/jei/settings.ini"));
    }

    #[test]
    fn bundle_retains_processor_runtime_library_tree_and_selected_assets() {
        let fixture = Fixture::new(true);
        fixture.write("versions/example/example.json", br#"{"id":"example","inheritsFrom":"base","libraries":[{"name":"net.neoforged:neoforge:20.1.1","downloads":{"artifact":{"path":"net/neoforged/neoforge/20.1.1/neoforge-20.1.1-client.jar"}}}]}"#);
        fixture.write("versions/base/base.json", br#"{"id":"base","clientVersion":"1.20.1","libraries":[{"name":"example:runtime:1","downloads":{"artifact":{"path":"example/runtime/1/runtime-1.jar"}}},{"name":"example:windows:1","rules":[{"action":"allow","os":{"name":"windows"}}]}],"assetIndex":{"id":"fixture"},"logging":{"client":{"file":{"id":"client.xml"}}}}"#);
        fixture.write("libraries/example/runtime/1/runtime-1.jar", b"runtime");
        fixture.write(
            "libraries/net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar",
            b"loader",
        );
        fixture.write("libraries/unrelated/never.jar", b"unrelated");
        fixture.write(
            "libraries/net/neoforged/neoforge/20.1.1/neoforge-20.1.1-client.jar",
            b"declared client",
        );
        fixture.write(
            "libraries/net/minecraft/client/1.20.1/client-1.20.1-srg.jar",
            b"generated MC_SRG",
        );
        fixture.write(
            "libraries/net/minecraft/client/1.20.1/client-1.20.1-extra.jar",
            b"generated MC_EXTRA",
        );
        fixture.write(
            "libraries/net/neoforged/neoforge/20.1.1/neoforge-20.1.1-universal.jar",
            b"undeclared universal",
        );
        fixture.write("libraries/accounts.json", b"private account fixture");
        fixture.write("assets/indexes/fixture.json", br#"{"objects":{"sound/example.ogg":{"hash":"0123456789012345678901234567890123456789","size":6}}}"#);
        fixture.write(
            "assets/objects/01/0123456789012345678901234567890123456789",
            b"sound",
        );
        fixture.write("assets/objects/ff/unrelated", b"unrelated");
        fixture.write("assets/log_configs/client.xml", b"logging");
        let mut request = request();
        request.checks.bundle_assets = true;
        let plan = fixture.plan(request.clone());
        assert!(plan
            .entries
            .contains_key("libraries/example/runtime/1/runtime-1.jar"));
        assert!(plan.entries.contains_key("libraries/unrelated/never.jar"));
        assert!(plan
            .entries
            .contains_key("libraries/net/minecraft/client/1.20.1/client-1.20.1-srg.jar"));
        assert!(plan
            .entries
            .contains_key("libraries/net/minecraft/client/1.20.1/client-1.20.1-extra.jar"));
        assert!(plan
            .entries
            .contains_key("libraries/net/neoforged/neoforge/20.1.1/neoforge-20.1.1-universal.jar"));
        assert!(!plan.entries.contains_key("assets/objects/ff/unrelated"));
        assert!(!plan.entries.contains_key("libraries/accounts.json"));
        execute(
            plan,
            &fixture.destination(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let entries = unzip(&fixture.destination());
        assert_eq!(
            entries[".minecraft/assets/objects/01/0123456789012345678901234567890123456789"],
            b"sound"
        );
        fs::remove_file(
            fixture
                .root
                .join("libraries/example/runtime/1/runtime-1.jar"),
        )
        .unwrap();
        assert!(prepare(&fixture.root, "example", request)
            .unwrap_err()
            .contains("缺少"));
    }

    #[test]
    fn stale_content_and_added_world_reject_before_output() {
        let fixture = Fixture::new(true);
        fixture.game_write("mods/example.jar", b"old", true);
        let plan = fixture.plan(request());
        fixture.game_write("mods/example.jar", b"new", true);
        assert!(execute(
            plan,
            &fixture.destination(),
            &AtomicBool::new(false),
            |_| {}
        )
        .unwrap_err()
        .contains("变化"));
        fixture.clean_output();
        let mut request = request();
        request.checks.saves = true;
        fixture.game_write("saves/old/level.dat", b"old", true);
        let plan = fixture.plan(request);
        fixture.game_write("saves/new/level.dat", b"new", true);
        assert!(execute(
            plan,
            &fixture.destination(),
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err());
        fixture.clean_output();
    }

    #[test]
    fn source_edits_during_stream_and_finalize_remove_staged_zip() {
        for stage in ["export-archive", "export-finalize"] {
            let fixture = Fixture::new(true);
            fixture.game_write("mods/example.jar", &[b'x'; 300_000], true);
            let plan = fixture.plan(request());
            let edited = AtomicBool::new(false);
            let result = execute(plan, &fixture.destination(), &AtomicBool::new(false), |p| {
                if p.stage == stage
                    && (p.bytes_done > 0 || stage == "export-finalize")
                    && !edited.swap(true, Ordering::SeqCst)
                {
                    fixture.game_write("options.txt", b"changed settings", true);
                }
            });
            assert!(edited.load(Ordering::SeqCst));
            assert!(result.unwrap_err().contains("变化"));
            fixture.clean_output();
        }
    }

    #[test]
    fn cancellation_before_scan_during_copy_and_finalize_cleans_output() {
        for stage in ["before", "export-archive", "export-finalize"] {
            let fixture = Fixture::new(true);
            fixture.game_write("mods/example.jar", &[b'x'; 300_000], true);
            let plan = fixture.plan(request());
            let cancel = AtomicBool::new(stage == "before");
            let result = execute(plan, &fixture.destination(), &cancel, |p| {
                if p.stage == stage && p.bytes_done > 0 {
                    cancel.store(true, Ordering::SeqCst);
                }
            });
            assert!(result.unwrap_err().contains("取消"));
            fixture.clean_output();
        }
    }

    #[test]
    fn existing_and_late_destination_collision_never_overwrite() {
        for late in [false, true] {
            let fixture = Fixture::new(true);
            let plan = fixture.plan(request());
            if !late {
                fs::write(fixture.destination(), b"existing data").unwrap();
            }
            let result = execute(plan, &fixture.destination(), &AtomicBool::new(false), |p| {
                if late && p.stage == "export-finalize" {
                    fs::write(fixture.destination(), b"existing data").unwrap();
                }
            });
            assert!(result.unwrap_err().contains("已经存在"));
            assert_eq!(fs::read(fixture.destination()).unwrap(), b"existing data");
            assert!(!fs::read_dir(&fixture.base).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pcl-export-")));
        }
    }

    #[test]
    fn cancel_cleanup_conflict_is_reported_and_external_replacement_preserved() {
        let fixture = Fixture::new(true);
        let cancel = AtomicBool::new(false);
        let replacement = Mutex::new(None);
        let result = execute(
            fixture.plan(request()),
            &fixture.destination(),
            &cancel,
            |p| {
                if p.stage == "export-archive" && !cancel.load(Ordering::SeqCst) {
                    let path = fs::read_dir(&fixture.base)
                        .unwrap()
                        .map(|entry| entry.unwrap().path())
                        .find(|path| {
                            path.file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with(".pcl-export-")
                        })
                        .unwrap();
                    fs::remove_file(&path).unwrap();
                    fs::write(&path, b"external replacement").unwrap();
                    *replacement.lock().unwrap() = Some(path);
                    cancel.store(true, Ordering::SeqCst);
                }
            },
        );
        assert!(result.unwrap_err().starts_with("取消清理失败："));
        assert_eq!(
            fs::read(replacement.lock().unwrap().as_ref().unwrap()).unwrap(),
            b"external replacement"
        );
        assert!(!fixture.destination().exists());
    }

    #[test]
    fn selected_symlinks_special_files_and_symlink_ancestors_are_refused() {
        let fixture = Fixture::new(true);
        fs::create_dir_all(fixture.root.join("versions/example/mods")).unwrap();
        std::os::unix::fs::symlink(
            fixture.root.join("versions/base/base.jar"),
            fixture.root.join("versions/example/mods/link.jar"),
        )
        .unwrap();
        assert!(prepare(&fixture.root, "example", request())
            .unwrap_err()
            .contains("符号链接"));
        fs::remove_file(fixture.root.join("versions/example/mods/link.jar")).unwrap();
        let fifo = fixture.root.join("versions/example/mods/special.jar");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(prepare(&fixture.root, "example", request())
            .unwrap_err()
            .contains("普通文件"));
        fs::remove_file(fifo).unwrap();
        let alias = fixture.base.join("alias");
        std::os::unix::fs::symlink(&fixture.root, &alias).unwrap();
        assert!(prepare(&alias, "example", request()).is_err());
        let nested_alias = fixture.base.join("nested");
        fs::create_dir(&nested_alias).unwrap();
        std::os::unix::fs::symlink(&fixture.root, nested_alias.join("alias")).unwrap();
        assert!(prepare(&nested_alias.join("alias"), "example", request()).is_err());
    }

    #[test]
    fn output_inside_source_or_symlink_parent_is_refused() {
        let fixture = Fixture::new(true);
        let inside = fixture.root.join("export.zip");
        assert!(execute(
            fixture.plan(request()),
            &inside,
            &AtomicBool::new(false),
            |_| {}
        )
        .unwrap_err()
        .contains("游戏目录内"));
        assert!(!inside.exists());
        let alias = fixture.base.join("output-alias");
        let output = fixture.base.join("output");
        fs::create_dir(&output).unwrap();
        std::os::unix::fs::symlink(&output, &alias).unwrap();
        assert!(execute(
            fixture.plan(request()),
            &alias.join("export.zip"),
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err());
        assert!(fs::read_dir(output).unwrap().next().is_none());
    }

    #[test]
    fn bundled_library_symlink_is_refused_even_when_not_declared() {
        let fixture = Fixture::new(true);
        fixture.write(
            "versions/example/example.json",
            br#"{"id":"example","inheritsFrom":"base","libraries":[]}"#,
        );
        fs::create_dir_all(fixture.root.join("libraries/example")).unwrap();
        std::os::unix::fs::symlink(
            fixture.root.join("versions/base/base.jar"),
            fixture.root.join("libraries/example/undeclared.jar"),
        )
        .unwrap();
        let mut request = request();
        request.checks.bundle_assets = true;
        assert!(prepare(&fixture.root, "example", request)
            .unwrap_err()
            .contains("符号链接"));
    }

    #[test]
    fn request_schema_path_exclusions_and_unsupported_options_are_checked() {
        let fixture = Fixture::new(true);
        for flag in ["game", "launcher", "modrinth"] {
            let mut request = request();
            match flag {
                "game" => request.checks.game = false,
                "launcher" => request.checks.launcher = true,
                "modrinth" => request.checks.modrinth = true,
                _ => unreachable!(),
            }
            assert!(prepare(&fixture.root, "example", request).is_err());
        }
        let mut invalid = request();
        invalid
            .excluded
            .insert("saves".into(), vec!["../world".into()]);
        assert!(invalid.validate().is_err());
        invalid = request();
        invalid.version.clear();
        assert!(invalid.validate().is_err());
        assert!(serde_json::from_value::<ExportRequest>(
            json!({"name":"pack","version":"1","checks":{"unexpected":true}})
        )
        .is_err());
        let defaults: ExportChecks = serde_json::from_value(json!({})).unwrap();
        assert_eq!(defaults, ExportChecks::default());
        assert!(prepare(&fixture.root, "../example", request()).is_err());
        fixture.write(
            "versions/example/example.json",
            br#"{"id":"example","inheritsFrom":"base","accessToken":"private"}"#,
        );
        assert!(prepare(&fixture.root, "example", request())
            .unwrap_err()
            .contains("账户"));
        fixture.write("versions/example/example.json", br#"{"id":"example","inheritsFrom":"base","arguments":{"game":["--username","PrivatePlayer"]}}"#);
        assert!(prepare(&fixture.root, "example", request())
            .unwrap_err()
            .contains("账户"));
        fixture.write("versions/example/example.json", br#"{"id":"example","inheritsFrom":"base","arguments":{"jvm":["-Dexample=/opt/private/instance"]}}"#);
        assert!(prepare(&fixture.root, "example", request())
            .unwrap_err()
            .contains("绝对路径"));
        fixture.write("versions/example/example.json", br#"{"id":"example","inheritsFrom":"base","arguments":{"game":["--username","${auth_player_name}","--accessToken","${auth_access_token}"]}}"#);
        assert!(prepare(&fixture.root, "example", request()).is_ok());
    }
}
