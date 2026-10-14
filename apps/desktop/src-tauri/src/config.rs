//! Settings v3 with registered roots and an active-root UI projection.
//! Transport revisions reject stale saves. V2 upgrades are deferred while a
//! rename journal binds its original bytes, then backed up before the next write.
use pcl_core::java::JavaSelection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA_VERSION: u32 = 3;
const MAX_JAVA_PATHS: usize = 64;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// The active root projection used by existing settings and launch commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub root_id: String,
    pub root: String,
    pub player: String,
    pub memory_gib: u32,
    pub selected: Option<String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, u32>,
    #[serde(default)]
    pub java: JavaSelection,
    #[serde(default)]
    pub java_paths: Vec<String>,
    #[serde(default)]
    pub java_overrides: BTreeMap<String, JavaSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GameRoot {
    pub id: String,
    pub name: String,
    pub path: String,
    pub selected: Option<String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, u32>,
    #[serde(default)]
    pub java_overrides: BTreeMap<String, JavaSelection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RootSummary {
    pub id: String,
    pub name: String,
    pub path: String,
    pub selected: Option<String>,
    pub overrides: BTreeMap<String, u32>,
    pub java_overrides: BTreeMap<String, JavaSelection>,
    pub available: bool,
    pub error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Persisted {
    schema_version: u32,
    active_root_id: String,
    player: String,
    memory_gib: u32,
    roots: Vec<GameRoot>,
    #[serde(default)]
    java: JavaSelection,
    #[serde(default)]
    java_paths: Vec<String>,
}

// Preserve this exact v2 field layout for already journalled rename payloads.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionTwo {
    schema_version: u32,
    active_root_id: String,
    player: String,
    memory_gib: u32,
    roots: Vec<VersionTwoRoot>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionTwoRoot {
    id: String,
    name: String,
    path: String,
    selected: Option<String>,
    #[serde(default)]
    overrides: BTreeMap<String, u32>,
}
impl VersionTwo {
    fn normalize(self) -> Persisted {
        Persisted {
            schema_version: SCHEMA_VERSION,
            active_root_id: self.active_root_id,
            player: self.player,
            memory_gib: self.memory_gib,
            roots: self
                .roots
                .into_iter()
                .map(|root| GameRoot {
                    id: root.id,
                    name: root.name,
                    path: root.path,
                    selected: root.selected,
                    overrides: root.overrides,
                    java_overrides: BTreeMap::new(),
                })
                .collect(),
            java: JavaSelection::Auto,
            java_paths: vec![],
        }
    }
    fn from_normalized(config: Persisted) -> Self {
        Self {
            schema_version: 2,
            active_root_id: config.active_root_id,
            player: config.player,
            memory_gib: config.memory_gib,
            roots: config
                .roots
                .into_iter()
                .map(|root| VersionTwoRoot {
                    id: root.id,
                    name: root.name,
                    path: root.path,
                    selected: root.selected,
                    overrides: root.overrides,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Copy)]
enum Migration {
    Legacy,
    VersionTwo,
}
impl Migration {
    fn version(self) -> u32 {
        match self {
            Self::Legacy => 1,
            Self::VersionTwo => 2,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySettings {
    root: String,
    player: String,
    memory_gib: u32,
    selected: Option<String>,
    #[serde(default)]
    overrides: BTreeMap<String, u32>,
}

struct Stored {
    config: Persisted,
    blocked: Option<String>,
    disk: crate::instance_rename_refs::DiskSnapshot,
    revision: String,
    pending_migration: Option<Migration>,
}

pub struct ConfigStore {
    file: PathBuf,
    inner: Mutex<Stored>,
}

fn nonce() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{time:x}-{:x}-{sequence:x}", std::process::id())
}

fn transport_revision() -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("pcl-settings-transport-v1:{}", nonce()).as_bytes())
    )
}

fn root_name(path: &Path) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "游戏目录".into())
}

fn defaults(project: &Path) -> Persisted {
    let path = project.join("Minecraft/.minecraft");
    Persisted {
        schema_version: SCHEMA_VERSION,
        active_root_id: "root-default".into(),
        player: "Player".into(),
        memory_gib: 6,
        roots: vec![GameRoot {
            id: "root-default".into(),
            name: root_name(&path),
            path: path.to_string_lossy().into_owned(),
            selected: None,
            overrides: BTreeMap::new(),
            java_overrides: BTreeMap::new(),
        }],
        java: JavaSelection::Auto,
        java_paths: vec![],
    }
}

fn validate_globals(player: &str, memory_gib: u32) -> Result<(), String> {
    if !(3..=16).contains(&player.len())
        || !player
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("玩家名需要 3–16 位英文字母、数字或下划线".into());
    }
    if !(2..=64).contains(&memory_gib) {
        return Err("内存应为 2–64 GiB".into());
    }
    Ok(())
}

fn validate_root_values(root: &GameRoot) -> Result<(), String> {
    if root.id.is_empty()
        || !root
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("游戏目录标识无效".into());
    }
    if !Path::new(&root.path).is_absolute() {
        return Err("游戏目录需要使用绝对路径".into());
    }
    validate_name(&root.name)?;
    if let Some(selected) = &root.selected {
        pcl_core::identifier(selected)?;
    }
    if root.overrides.len() > 4096 || root.java_overrides.len() > 4096 {
        return Err("实例设置超过 4096 条上限".into());
    }
    for (id, memory) in &root.overrides {
        pcl_core::identifier(id)?;
        if !(2..=64).contains(memory) {
            return Err("内存应为 2–64 GiB".into());
        }
    }
    for (id, selection) in &root.java_overrides {
        pcl_core::identifier(id)?;
        validate_java(selection)?;
    }
    Ok(())
}

fn validate_java_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 4096
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
    {
        return Err("Java 路径需要是规范的绝对路径，且不超过 4096 字节".into());
    }
    Ok(())
}
fn validate_java(selection: &JavaSelection) -> Result<(), String> {
    match selection {
        JavaSelection::Auto => Ok(()),
        JavaSelection::Manual { path } => validate_java_path(path),
    }
}
fn add_java_path(config: &mut Persisted, path: &str) -> Result<(), String> {
    validate_java_path(path)?;
    if !config.java_paths.iter().any(|old| old == path) {
        if config.java_paths.len() >= MAX_JAVA_PATHS {
            return Err("手动 Java 最多保存 64 个路径".into());
        }
        config.java_paths.push(path.into());
    }
    Ok(())
}

/// Resolve existing symlinks without requiring the executable to exist. A
/// missing tail stays attached to its resolved parent, including dangling
/// symlink targets. This reads path metadata only and never executes Java.
fn java_path_points_into(path: &Path, folder: &Path) -> Result<bool, String> {
    if path.starts_with(folder) {
        return Ok(true);
    }
    let mut pending: VecDeque<_> = path
        .components()
        .map(|part| part.as_os_str().to_os_string())
        .collect();
    let mut resolved = PathBuf::new();
    let mut symlinks = 0;
    while let Some(part) = pending.pop_front() {
        if part == "/" {
            resolved = PathBuf::from("/");
            continue;
        }
        if part == "." {
            continue;
        }
        if part == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&part);
        // Linux must traverse this directory even if a later symlink target
        // component is `..`; its final canonical destination is insufficient.
        if candidate.starts_with(folder) {
            return Ok(true);
        }
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                symlinks += 1;
                if symlinks > 40 {
                    return Err("无法确认 Java 路径指向，请修复符号链接后再重命名".into());
                }
                let target = fs::read_link(&candidate)
                    .map_err(|e| format!("无法确认 Java 路径指向，请修复路径后再重命名：{e}"))?;
                for part in target.components().rev() {
                    pending.push_front(part.as_os_str().to_os_string());
                }
            }
            Ok(_) => resolved = candidate,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => resolved = candidate,
            Err(error) => {
                return Err(format!(
                    "无法确认 Java 路径指向，请修复路径后再重命名：{error}"
                ))
            }
        }
    }
    Ok(resolved.starts_with(folder))
}

fn decode_settings(bytes: &[u8]) -> Result<(Persisted, Option<Migration>), String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let (config, migration) = match value.get("schema_version") {
        Some(version) if version.as_u64() == Some(3) => (
            serde_json::from_value::<Persisted>(value).map_err(|e| e.to_string())?,
            None,
        ),
        Some(version) if version.as_u64() == Some(2) => (
            serde_json::from_value::<VersionTwo>(value)
                .map_err(|e| e.to_string())?
                .normalize(),
            Some(Migration::VersionTwo),
        ),
        Some(version) => return Err(format!("不支持设置版本 {version}，请使用兼容的启动器")),
        None => {
            let old: LegacySettings = serde_json::from_value(value).map_err(|e| e.to_string())?;
            (
                Persisted {
                    schema_version: SCHEMA_VERSION,
                    active_root_id: "root-legacy".into(),
                    player: old.player,
                    memory_gib: old.memory_gib,
                    roots: vec![GameRoot {
                        id: "root-legacy".into(),
                        name: root_name(Path::new(&old.root)),
                        path: old.root,
                        selected: old.selected,
                        overrides: old.overrides,
                        java_overrides: BTreeMap::new(),
                    }],
                    java: JavaSelection::Auto,
                    java_paths: vec![],
                },
                Some(Migration::Legacy),
            )
        }
    };
    validate_config(&config)?;
    Ok((config, migration))
}

fn backup_migration(
    project: &Path,
    bytes: &[u8],
    disk: &crate::instance_rename_refs::DiskSnapshot,
    migration: Migration,
) -> Result<(), String> {
    match migration {
        Migration::Legacy => {
            crate::instance_rename_refs::backup_legacy_checked(project, bytes, disk)
        }
        Migration::VersionTwo => crate::instance_rename_refs::backup_settings_checked(
            project,
            bytes,
            disk,
            migration.version(),
        ),
    }
    .map_err(|e| format!("旧版设置备份失败：{e}"))
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err("游戏目录名称不能为空或包含控制字符".into());
    }
    Ok(())
}

fn validate_config(config: &Persisted) -> Result<(), String> {
    if config.schema_version != SCHEMA_VERSION {
        return Err(format!("不支持设置版本 {}", config.schema_version));
    }
    validate_globals(&config.player, config.memory_gib)?;
    validate_java(&config.java)?;
    if config.java_paths.len() > MAX_JAVA_PATHS {
        return Err("手动 Java 最多保存 64 个路径".into());
    }
    let mut java_paths = HashSet::new();
    for path in &config.java_paths {
        validate_java_path(path)?;
        if !java_paths.insert(path) {
            return Err("手动 Java 路径不能重复".into());
        }
    }
    if config.roots.is_empty() {
        return Err("设置中至少需要一个游戏目录".into());
    }
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for root in &config.roots {
        validate_root_values(root)?;
        if !ids.insert(&root.id) || !paths.insert(&root.path) {
            return Err("设置中存在重复游戏目录".into());
        }
    }
    if !ids.contains(&config.active_root_id) {
        return Err("当前游戏目录标识不存在".into());
    }
    Ok(())
}

fn canonical_root(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err("游戏目录需要使用绝对路径".into());
    }
    let canonical = fs::canonicalize(path).map_err(|e| format!("游戏目录不可用：{e}"))?;
    let metadata = fs::metadata(&canonical).map_err(|e| format!("游戏目录不可用：{e}"))?;
    if !metadata.is_dir() {
        return Err("游戏目录不是文件夹".into());
    }
    Ok(canonical)
}

impl ConfigStore {
    /// Never replaces unreadable, malformed, or newer settings with defaults.
    /// Such stores remain readable but every mutation is explicitly blocked.
    pub fn load(project: &Path) -> (Self, Option<String>) {
        let file = project.join(".pcl-rust/settings.json");
        let mut config = defaults(project);
        let mut blocked = None;
        let mut disk = crate::instance_rename_refs::DiskSnapshot::default();
        let mut pending_migration = None;
        match crate::instance_rename_refs::store_snapshot(project, "settings.json", 2 * 1024 * 1024)
        {
            Ok((snapshot, None)) => {
                disk = snapshot;
            }
            Err(e) => blocked = Some(format!("设置读取失败，原文件未修改；已禁止保存设置：{e}")),
            Ok((snapshot, Some(bytes))) => {
                disk = snapshot;
                let loaded = (|| -> Result<Persisted, String> {
                    let (migrated, migration) = decode_settings(&bytes)?;
                    // Keep the actual legacy values visible if backing up or
                    // committing migration fails, while continuing to block saves.
                    config = migrated.clone();
                    pending_migration = migration;
                    let Some(migration) = migration else {
                        return Ok(migrated);
                    };
                    // Pending instance transactions bind the existing settings
                    // bytes. A schema upgrade would invalidate their recovery
                    // snapshot, so defer it until the marker has been cleared.
                    if crate::instance_rename_refs::pending_root(project)?.is_some()
                        || crate::instance_delete::pending_root(project)?.is_some()
                    {
                        return Ok(migrated);
                    }
                    crate::instance_rename_refs::with_settings_lock(project, || {
                        crate::instance_rename_refs::ensure_project_ready(project)?;
                        backup_migration(project, &bytes, &disk, migration)?;
                        let data = encode(&migrated)?;
                        disk = crate::instance_rename_refs::write_store_checked(
                            project,
                            "settings.json",
                            &data,
                            &disk,
                            2 * 1024 * 1024,
                        )?;
                        Ok(())
                    })
                    .map_err(|e| format!("设置迁移失败：{e}"))?;
                    pending_migration = None;
                    Ok(migrated)
                })();
                match loaded {
                    Ok(loaded) => config = loaded,
                    Err(e) => {
                        blocked = Some(format!(
                            "设置读取或迁移失败，原设置已保留；已禁止保存设置：{e}"
                        ))
                    }
                }
            }
        }
        let warning = blocked.clone();
        (
            Self {
                file,
                inner: Mutex::new(Stored {
                    config,
                    blocked,
                    disk,
                    revision: transport_revision(),
                    pending_migration,
                }),
            },
            warning,
        )
    }

    fn lock(&self) -> MutexGuard<'_, Stored> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn commit(&self, current: &mut Stored, next: Persisted) -> Result<(), String> {
        if let Some(reason) = &current.blocked {
            return Err(reason.clone());
        }
        validate_config(&next)?;
        let project = self.project()?;
        let data = encode(&next)?;
        let disk = crate::instance_rename_refs::with_settings_lock(project, || {
            crate::instance_rename_refs::ensure_project_ready(project)?;
            if let Some(migration) = current.pending_migration {
                let (_, bytes) = crate::instance_rename_refs::store_snapshot(
                    project,
                    "settings.json",
                    2 * 1024 * 1024,
                )?;
                let bytes = bytes.ok_or("等待迁移的旧设置已消失，原设置已保留")?;
                backup_migration(project, &bytes, &current.disk, migration)?;
            }
            crate::instance_rename_refs::write_store_checked(
                project,
                "settings.json",
                &data,
                &current.disk,
                2 * 1024 * 1024,
            )
        })?;
        current.disk = disk;
        current.config = next;
        current.pending_migration = None;
        current.revision = transport_revision();
        Ok(())
    }

    pub fn warning(&self) -> Option<String> {
        self.lock().blocked.clone()
    }

    /// Call before operations that would mutate game files when settings could
    /// not be read. This avoids acting on the read-only fallback configuration.
    pub fn ensure_writable(&self) -> Result<(), String> {
        match self.lock().blocked.clone() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    pub fn snapshot(&self) -> Settings {
        let current = self.lock();
        project_settings(&current.config, &current.revision)
    }

    /// Checks directory metadata only; this does not read any game contents.
    pub fn roots(&self) -> Vec<RootSummary> {
        let roots = self.lock().config.roots.clone();
        summarize_roots(roots)
    }

    /// Captures the active projection and registry from one locked revision;
    /// availability probes run after releasing the lock.
    pub fn view(&self) -> (Settings, Vec<RootSummary>) {
        let (config, revision) = {
            let current = self.lock();
            (current.config.clone(), current.revision.clone())
        };
        (
            project_settings(&config, &revision),
            summarize_roots(config.roots),
        )
    }

    /// Registry-only lookup for metadata operations. File access must still
    /// use resolve(), which requires a currently available directory.
    pub(crate) fn registered(&self, id: &str) -> Result<GameRoot, String> {
        self.lock()
            .config
            .roots
            .iter()
            .find(|root| root.id == id)
            .cloned()
            .ok_or_else(|| "游戏目录未注册或已移除".into())
    }

    /// Returns a verified canonical path for a root bound to an operation.
    pub fn resolve(&self, root_id: Option<&str>) -> Result<GameRoot, String> {
        let mut root = {
            let current = self.lock();
            let id = root_id.unwrap_or(&current.config.active_root_id);
            current
                .config
                .roots
                .iter()
                .find(|root| root.id == id)
                .cloned()
                .ok_or("游戏目录未注册或已移除")?
        };
        let canonical = canonical_root(&root.path)?;
        root.path = canonical
            .to_str()
            .ok_or("游戏目录路径不是有效的 UTF-8")?
            .into();
        Ok(root)
    }

    /// Canonical aliases reuse the registered id and do not switch the active root.
    pub fn register(&self, path: String, name: Option<String>) -> Result<GameRoot, String> {
        let canonical = canonical_root(&path)?;
        let root = GameRoot {
            id: format!("root-{}", nonce()),
            name: name
                .map(|name| name.trim().to_owned())
                .unwrap_or_else(|| root_name(Path::new(&path))),
            path,
            selected: None,
            overrides: BTreeMap::new(),
            java_overrides: BTreeMap::new(),
        };
        validate_root_values(&root)?;
        loop {
            let roots = {
                let current = self.lock();
                if let Some(reason) = &current.blocked {
                    return Err(reason.clone());
                }
                current.config.roots.clone()
            };
            // Offline mounts and canonicalization must not hold the settings
            // mutex. Recheck the registry before committing to avoid duplicate
            // aliases when callers register directories concurrently.
            let existing = roots.iter().find(|saved| {
                saved.path == root.path
                    || canonical_root(&saved.path).is_ok_and(|saved| saved == canonical)
            });
            let mut current = self.lock();
            if current.config.roots != roots {
                continue;
            }
            if let Some(existing) = existing {
                return Ok(existing.clone());
            }
            let mut next = current.config.clone();
            next.roots.push(root.clone());
            self.commit(&mut current, next)?;
            return Ok(root);
        }
    }

    pub fn select(&self, id: &str) -> Result<Settings, String> {
        let mut current = self.lock();
        if !current.config.roots.iter().any(|root| root.id == id) {
            return Err("游戏目录未注册或已移除".into());
        }
        let mut next = current.config.clone();
        next.active_root_id = id.into();
        self.commit(&mut current, next)?;
        Ok(project_settings(&current.config, &current.revision))
    }

    pub fn update(
        &self,
        id: &str,
        name: Option<String>,
        position: Option<usize>,
    ) -> Result<(), String> {
        let mut current = self.lock();
        let index = current
            .config
            .roots
            .iter()
            .position(|root| root.id == id)
            .ok_or("游戏目录未注册或已移除")?;
        let mut next = current.config.clone();
        if let Some(name) = name {
            let name = name.trim().to_owned();
            validate_name(&name)?;
            next.roots[index].name = name;
        }
        if let Some(position) = position {
            if position >= next.roots.len() {
                return Err("游戏目录排序位置无效".into());
            }
            let root = next.roots.remove(index);
            next.roots.insert(position, root);
        }
        self.commit(&mut current, next)
    }

    /// Removes only a registry reference. Game files are never removed.
    pub fn remove(&self, id: &str) -> Result<Settings, String> {
        loop {
            let (roots, active, revision) = {
                let current = self.lock();
                if let Some(reason) = &current.blocked {
                    return Err(reason.clone());
                }
                (
                    current.config.roots.clone(),
                    current.config.active_root_id.clone(),
                    current.revision.clone(),
                )
            };
            let index = roots
                .iter()
                .position(|root| root.id == id)
                .ok_or("游戏目录未注册或已移除")?;
            if roots.len() == 1 {
                return Err("至少需要保留一个游戏目录，请先添加其他目录".into());
            }
            let mut remaining = roots;
            remaining.remove(index);
            let next_active = if active == id {
                // Offline mounts must not hold the settings mutex. Prefer the
                // next usable neighbour, wrapping before retaining an offline
                // entry; a missing directory must never be silently recreated.
                let start = index.min(remaining.len() - 1);
                (start..remaining.len())
                    .chain(0..start)
                    .find(|&candidate| canonical_root(&remaining[candidate].path).is_ok())
                    .map(|candidate| remaining[candidate].id.clone())
                    .unwrap_or_else(|| remaining[start].id.clone())
            } else {
                active
            };
            let mut current = self.lock();
            if current.revision != revision {
                continue; // Recompute after a concurrent select/register/save.
            }
            let mut next = current.config.clone();
            next.roots = remaining;
            next.active_root_id = next_active;
            self.commit(&mut current, next)?;
            return Ok(project_settings(&current.config, &current.revision));
        }
    }

    pub fn save(&self, settings: Settings) -> Result<(), String> {
        let mut current = self.lock();
        if settings.revision != current.revision {
            return Err("设置已更新，请刷新后再保存设置".into());
        }
        if settings.root_id != current.config.active_root_id {
            return Err("当前游戏目录已改变，请刷新后再保存设置".into());
        }
        let index = current
            .config
            .roots
            .iter()
            .position(|root| root.id == settings.root_id)
            .ok_or("游戏目录未注册或已移除")?;
        if settings.root != current.config.roots[index].path {
            return Err("请通过游戏目录管理注册或切换目录".into());
        }
        let mut next = current.config.clone();
        next.player = settings.player;
        next.memory_gib = settings.memory_gib;
        next.roots[index].selected = settings.selected;
        next.roots[index].overrides = settings.overrides;
        next.java = settings.java;
        next.roots[index].java_overrides = settings.java_overrides;
        let selections: Vec<_> = std::iter::once(&next.java)
            .chain(next.roots[index].java_overrides.values())
            .cloned()
            .collect();
        for selection in selections {
            if let JavaSelection::Manual { path } = selection {
                add_java_path(&mut next, &path)?;
            }
        }
        // java_paths is a readonly projection; imported paths use register_java.
        self.commit(&mut current, next)
    }

    pub fn register_java(
        &self,
        path: String,
        revision: &str,
        root_id: &str,
    ) -> Result<Settings, String> {
        let mut current = self.lock();
        if revision != current.revision {
            return Err("设置已更新，请刷新后再添加 Java".into());
        }
        if root_id != current.config.active_root_id {
            return Err("当前游戏目录已改变，请刷新后再添加 Java".into());
        }
        let mut next = current.config.clone();
        add_java_path(&mut next, &path)?;
        self.commit(&mut current, next)?;
        Ok(project_settings(&current.config, &current.revision))
    }

    /// A verified managed runtime is a global registry addition, independent of
    /// the root/revision captured before a queued download. Merge under the
    /// config lock; never replay that older projection or change Java choices.
    pub(crate) fn register_downloaded_java(&self, path: String) -> Result<Settings, String> {
        let mut current = self.lock();
        let mut next = current.config.clone();
        add_java_path(&mut next, &path)?;
        self.commit(&mut current, next)?;
        Ok(project_settings(&current.config, &current.revision))
    }

    /// Updates the operation's bound root even if another root is now active.
    pub fn select_installed(&self, root_id: &str, id: &str) -> Result<(), String> {
        pcl_core::identifier(id)?;
        let mut current = self.lock();
        let index = current
            .config
            .roots
            .iter()
            .position(|root| root.id == root_id)
            .ok_or("游戏目录未注册或已移除")?;
        let mut next = current.config.clone();
        next.roots[index].selected = Some(id.into());
        self.commit(&mut current, next)
    }

    fn project(&self) -> Result<&Path, String> {
        self.file
            .parent()
            .and_then(Path::parent)
            .ok_or("设置项目目录无效".into())
    }

    pub fn ensure_rename_snapshot(&self) -> Result<(), String> {
        let current = self.lock();
        if let Some(error) = &current.blocked {
            return Err(error.clone());
        }
        let project = self.project()?;
        crate::instance_rename_refs::with_settings_lock(project, || {
            let (disk, _) = crate::instance_rename_refs::store_snapshot(
                project,
                "settings.json",
                2 * 1024 * 1024,
            )?;
            if !current.disk.compatible(&disk) {
                return Err("设置文件已由外部修改，请重新打开启动器后再进行实例操作".into());
            }
            Ok(())
        })
    }

    /// A new physical instance must not silently inherit settings left behind
    /// for an externally removed instance. Deletion reservations are checked
    /// separately; this also covers references predating recoverable deletion.
    pub fn ensure_new_instance_name(&self, root_id: &str, id: &str) -> Result<(), String> {
        self.ensure_rename_snapshot()?;
        let current = self.lock();
        let root = current
            .config
            .roots
            .iter()
            .find(|root| root.id == root_id)
            .ok_or("游戏目录未注册")?;
        if root.selected.as_deref() == Some(id)
            || root.overrides.contains_key(id)
            || root.java_overrides.contains_key(id)
        {
            return Err("此名称仍有保存的实例设置，请使用新的实例名称".into());
        }
        Ok(())
    }

    pub fn refresh_after_rename(&self) -> Result<(), String> {
        let mut current = self.lock();
        let project = self.project()?;
        let result = crate::instance_rename_refs::with_settings_lock(project, || {
            let (disk, bytes) = crate::instance_rename_refs::store_snapshot(
                project,
                "settings.json",
                2 * 1024 * 1024,
            )?;
            if current.disk.file_exists() && !disk.file_exists() {
                return Err("设置文件已消失，已保留当前设置并禁止写入".into());
            }
            let (config, migration) = match bytes {
                Some(bytes) => decode_settings(&bytes)
                    .map_err(|e| format!("设置损坏或格式不受支持，原文件已保留：{e}"))?,
                None => (defaults(project), None),
            };
            validate_config(&config)?;
            current.config = config;
            current.pending_migration = migration;
            current.disk = disk;
            current.revision = transport_revision();
            current.blocked = None;
            Ok(())
        });
        if let Err(error) = &result {
            current.blocked = Some(error.clone());
        }
        result
    }
}

fn encode(config: &impl Serialize) -> Result<Vec<u8>, String> {
    let mut data = serde_json::to_vec_pretty(config).map_err(|error| error.to_string())?;
    data.push(b'\n');
    if data.len() > 2 * 1024 * 1024 {
        return Err("设置文件超过 2 MiB 限制".into());
    }
    Ok(data)
}

/// Moving an instance also moves any runtime stored inside it. Keep the same
/// path-resolution policy for rename and deletion, including missing explicit
/// Manual choices and available registry aliases across every registered root.
fn check_instance_java_dependencies(
    config: &Persisted,
    root: &Path,
    id: &str,
    reason: &str,
) -> Result<(), String> {
    let source_folder = root.join("versions").join(id);
    let selected_paths = std::iter::once(&config.java)
        .chain(
            config
                .roots
                .iter()
                .flat_map(|root| root.java_overrides.values()),
        )
        .filter_map(|selection| match selection {
            JavaSelection::Manual { path } => Some(path.as_str()),
            _ => None,
        });
    let java_paths: HashSet<_> = config
        .java_paths
        .iter()
        .map(String::as_str)
        // Unavailable registry entries remain visible but cannot participate in
        // automatic Java selection. Moving and re-adding a runtime must not
        // leave its missing old registry path as a permanent rename blocker.
        // Explicit Manual choices below remain dependencies even when missing.
        .filter(|path| fs::metadata(path).is_ok_and(|metadata| metadata.is_file()))
        .chain(selected_paths)
        .collect();
    for path in java_paths {
        if java_path_points_into(Path::new(path), &source_folder)? {
            return Err(reason.into());
        }
    }
    Ok(())
}

pub(crate) fn check_delete_dependencies(
    bytes: &[u8],
    root_id: &str,
    root: &Path,
    id: &str,
) -> Result<(), String> {
    pcl_core::identifier(id)?;
    let (config, _) =
        decode_settings(bytes).map_err(|_| "设置损坏或格式不受支持，原文件已保留".to_string())?;
    validate_config(&config)?;
    let registered = config
        .roots
        .iter()
        .find(|item| item.id == root_id)
        .ok_or("删除所属的游戏目录未注册")?;
    if canonical_root(&registered.path)? != root {
        return Err("设置中的游戏目录与删除范围不符".into());
    }
    check_instance_java_dependencies(
        &config,
        root,
        id,
        "Java 位于待删除实例内，请先移到实例之外并重新添加",
    )
}

pub(crate) fn rename_bytes(
    bytes: &[u8],
    root_id: &str,
    root: &Path,
    old: &str,
    new: &str,
) -> Result<Vec<u8>, String> {
    pcl_core::identifier(old)?;
    pcl_core::identifier(new)?;
    let (mut config, migration) =
        decode_settings(bytes).map_err(|_| "设置损坏或格式不受支持，原文件已保留".to_string())?;
    if matches!(migration, Some(Migration::Legacy)) {
        return Err("旧版设置尚未迁移，无法重命名".into());
    }
    validate_config(&config)?;
    check_instance_java_dependencies(
        &config,
        root,
        old,
        "Java 位于待改名实例内，请先移到实例之外并重新添加",
    )?;
    let selected = config
        .roots
        .iter_mut()
        .find(|item| item.id == root_id)
        .ok_or("重命名所属的游戏目录未注册")?;
    if canonical_root(&selected.path)? != root {
        return Err("设置中的游戏目录与重命名范围不符".into());
    }
    if selected.overrides.contains_key(new)
        || selected.java_overrides.contains_key(new)
        || selected.selected.as_deref() == Some(new)
    {
        return Err("目标名称已有选择、内存或 Java 配置，请选择其他名称".into());
    }
    let mut changed = false;
    if selected.selected.as_deref() == Some(old) {
        selected.selected = Some(new.into());
        changed = true;
    }
    if let Some(memory) = selected.overrides.remove(old) {
        selected.overrides.insert(new.into(), memory);
        changed = true;
    }
    if let Some(java) = selected.java_overrides.remove(old) {
        selected.java_overrides.insert(new.into(), java);
        changed = true;
    }
    if !changed {
        return Ok(bytes.to_vec());
    }
    validate_config(&config)?;
    match migration {
        Some(Migration::VersionTwo) => encode(&VersionTwo::from_normalized(config)),
        _ => encode(&config),
    }
}

fn project_settings(config: &Persisted, revision: &str) -> Settings {
    let root = config
        .roots
        .iter()
        .find(|root| root.id == config.active_root_id)
        .expect("validated settings have an active root");
    Settings {
        revision: revision.into(),
        root_id: root.id.clone(),
        root: root.path.clone(),
        player: config.player.clone(),
        memory_gib: config.memory_gib,
        selected: root.selected.clone(),
        overrides: root.overrides.clone(),
        java: config.java.clone(),
        java_paths: config.java_paths.clone(),
        java_overrides: root.java_overrides.clone(),
    }
}

fn summarize_roots(roots: Vec<GameRoot>) -> Vec<RootSummary> {
    roots
        .into_iter()
        .map(|root| {
            let error = canonical_root(&root.path).err();
            RootSummary {
                id: root.id,
                name: root.name,
                path: root.path,
                selected: root.selected,
                overrides: root.overrides,
                java_overrides: root.java_overrides,
                available: error.is_none(),
                error,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
