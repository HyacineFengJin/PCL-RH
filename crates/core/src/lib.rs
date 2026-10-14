//! Native launch planning for installed Mojang-format instances.
pub mod content_scope;
mod dependencies;
pub mod inspection;
pub mod java;
pub mod launch_options;
pub mod platform;
pub mod repair;
mod rules;
use launch_options::{LaunchOptions, WindowSize};
use regex::Regex;
use rules::RuleContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

type Result<T> = std::result::Result<T, String>;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub minecraft_version: String,
    pub loader: String,
    pub java_major: u32,
    pub mod_count: usize,
    pub isolated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanIssue {
    pub id: String,
    pub message: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanReport {
    pub instances: Vec<Instance>,
    pub issues: Vec<ScanIssue>,
}
pub struct LaunchPlan {
    pub java: PathBuf,
    pub args: Vec<String>,
    pub game_dir: PathBuf,
    pub log_path: PathBuf,
    pub version: String,
    secret_token: Option<String>,
}
/// Verified Minecraft identity supplied by the authentication layer.
/// Never expose this value through Debug or serialization.
pub struct OnlineIdentity {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub xuid: String,
    pub client_id: String,
}
#[derive(Debug, Serialize)]
struct PublicLaunchPlan<'a> {
    java: &'a Path,
    args: Vec<String>,
    game_dir: &'a Path,
    log_path: &'a Path,
    version: &'a str,
}
impl LaunchPlan {
    fn public_view(&self) -> PublicLaunchPlan<'_> {
        let args = self
            .args
            .iter()
            .map(|arg| match self.secret_token.as_deref() {
                Some(token) if !token.is_empty() => arg.replace(token, "<redacted>"),
                _ => arg.clone(),
            })
            .collect();
        PublicLaunchPlan {
            java: &self.java,
            args,
            game_dir: &self.game_dir,
            log_path: &self.log_path,
            version: &self.version,
        }
    }
}
impl Serialize for LaunchPlan {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.public_view().serialize(serializer)
    }
}
impl std::fmt::Debug for LaunchPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.public_view().fmt(f)
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn identifier(id: &str) -> Result<()> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\', ':', '\0']) {
        return Err(format!("Unsafe metadata identifier: {id:?}"));
    }
    #[cfg(windows)]
    if platform::windows_reserved_component(id) {
        return Err(format!("Unsafe Windows metadata identifier: {id:?}"));
    }
    Ok(())
}
/// Reject traversal and symlink escapes, including when only a parent already exists.
pub fn safe_join(base: &Path, relative: impl AsRef<Path>) -> Result<PathBuf> {
    let relative = relative.as_ref();
    if relative.as_os_str().is_empty()
        || relative.to_string_lossy().contains('\0')
        || (!cfg!(windows) && relative.to_string_lossy().contains('\\'))
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!("Unsafe metadata path: {}", relative.display()));
    }
    #[cfg(windows)]
    for component in relative.components() {
        if let Component::Normal(name) = component {
            identifier(name.to_str().ok_or("Metadata path is not UTF-8")?)?;
        }
    }
    let result = base.join(relative);
    let canonical_base = fs::canonicalize(base).map_err(err)?;
    let mut existing = result.as_path();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or("Invalid metadata path")?
            }
            Err(e) => return Err(err(e)),
        }
    }
    if !fs::canonicalize(existing)
        .map_err(err)?
        .starts_with(canonical_base)
    {
        return Err(format!("Path escapes installation: {}", result.display()));
    }
    Ok(result)
}
fn read_json(path: &Path) -> Result<Value> {
    if !fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .is_file()
    {
        return Err(format!(
            "Metadata must be a regular file: {}",
            path.display()
        ));
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(raw.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{}: {e}", path.display()))
}
fn library_key(v: &Value) -> String {
    let name = text(v, "name");
    let parts: Vec<_> = name.split(':').collect();
    // Version overrides preserve distinct platform classifiers.
    if parts.len() >= 3 {
        format!("{}:{}:{}", parts[0], parts[1], parts.get(3).unwrap_or(&""))
    } else {
        name.to_string()
    }
}
fn merge(mut base: Value, child: Value) -> Value {
    let mut libraries = base["libraries"].as_array().cloned().unwrap_or_default();
    for library in child["libraries"].as_array().into_iter().flatten() {
        let key = library_key(library);
        libraries.retain(|old| library_key(old) != key);
        libraries.push(library.clone());
    }
    let mut arguments = serde_json::Map::new();
    for key in ["game", "jvm"] {
        let mut list = base["arguments"][key]
            .as_array()
            .cloned()
            .unwrap_or_default();
        list.extend(
            child["arguments"][key]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
        arguments.insert(key.into(), Value::Array(list));
    }
    let has_arguments = base.get("arguments").is_some() || child.get("arguments").is_some();
    for (k, v) in child.as_object().unwrap() {
        base[k] = v.clone();
    }
    base["libraries"] = libraries.into();
    if has_arguments {
        base["arguments"] = arguments.into();
    }
    base
}
fn metadata(root: &Path, id: &str, visiting: &mut HashSet<String>) -> Result<Value> {
    identifier(id)?;
    if visiting.len() >= 64 || !visiting.insert(id.to_owned()) {
        return Err(format!("Version inheritance cycle or depth limit at {id}"));
    }
    let path = safe_join(root, format!("versions/{id}/{id}.json"))?;
    let mut data = read_json(&path)?;
    if !data.is_object() {
        return Err(format!("Version metadata is not an object: {id}"));
    }
    if let Some(parent) = data.get("inheritsFrom").and_then(Value::as_str) {
        let base = metadata(root, parent, visiting)?;
        // A child may omit its client jar and reuse its parent's.
        if data.get("jar").is_none() {
            data["jar"] = inherited_jar(
                id,
                parent,
                &base,
                safe_join(root, format!("versions/{id}/{id}.jar"))?.is_file(),
            );
        }
        data = merge(base, data);
    }
    visiting.remove(id);
    Ok(data)
}
fn inherited_jar(id: &str, parent: &str, base: &Value, own_present: bool) -> Value {
    if own_present {
        Value::String(id.into())
    } else {
        base.get("jar")
            .cloned()
            .unwrap_or(Value::String(parent.into()))
    }
}
fn required_java(data: &Value) -> u32 {
    match data["javaVersion"].get("majorVersion") {
        None => 8,
        Some(value) => value
            .as_u64()
            .and_then(|major| u32::try_from(major).ok())
            .unwrap_or(0),
    }
}
fn game_argument<'a>(data: &'a Value, flag: &str) -> Option<&'a str> {
    data["arguments"]["game"]
        .as_array()?
        .windows(2)
        .find(|pair| pair[0].as_str() == Some(flag))
        .and_then(|pair| pair[1].as_str())
}
fn minecraft_version(data: &Value) -> &str {
    game_argument(data, "--fml.mcVersion")
        .or_else(|| data["clientVersion"].as_str())
        .or_else(|| data["inheritsFrom"].as_str())
        .unwrap_or_else(|| text(data, "id"))
}
fn loader_description(data: &Value) -> String {
    let coordinate_version = |coordinates: &[&str]| -> Option<&str> {
        data["libraries"].as_array()?.iter().find_map(|library| {
            let name = text(library, "name");
            coordinates.iter().find_map(|coordinate| {
                name.strip_prefix(coordinate)
                    .and_then(|tail| tail.strip_prefix(':'))
                    .and_then(|tail| tail.split(':').next())
                    .filter(|version| !version.is_empty())
            })
        })
    };
    // Prefer NeoForge's own identity even if its metadata includes Forge-derived dependencies.
    let (name, version) = if let Some(version) = coordinate_version(&["net.neoforged:neoforge"]) {
        ("NeoForge", Some(version))
    } else if let Some(version) = coordinate_version(&["net.neoforged:forge"]) {
        (
            "NeoForge",
            version.split_once('-').map(|(_, v)| v).or(Some(version)),
        )
    } else if let Some(version) = game_argument(data, "--fml.neoForgeVersion") {
        ("NeoForge", Some(version))
    } else if let Some(version) = game_argument(data, "--fml.forgeVersion") {
        ("Forge", Some(version))
    } else if let Some(version) =
        coordinate_version(&["net.minecraftforge:forge", "net.minecraftforge:fmlloader"])
    {
        (
            "Forge",
            version.split_once('-').map(|(_, v)| v).or(Some(version)),
        )
    } else if let Some(version) = coordinate_version(&["net.fabricmc:fabric-loader"]) {
        ("Fabric", Some(version))
    } else if let Some(version) = coordinate_version(&["org.quiltmc:quilt-loader"]) {
        ("Quilt", Some(version))
    } else {
        ("Vanilla", None)
    };
    match version {
        Some(version) => format!("{name} {version}"),
        None => name.into(),
    }
}
pub fn scan_instances(root: &Path) -> Result<Vec<Instance>> {
    Ok(scan_instances_report(root)?.instances)
}
/// Keeps readable instances when another entry has damaged metadata. Failures
/// at the root or versions boundary still fail the complete scan.
pub fn scan_instances_report(root: &Path) -> Result<ScanReport> {
    let root = fs::canonicalize(root).map_err(err)?;
    if !root.is_dir() {
        return Err(format!("Game root is not a directory: {}", root.display()));
    }
    let versions = safe_join(&root, "versions")?;
    if !versions.exists() {
        return Ok(ScanReport::default());
    }
    let mut report = ScanReport::default();
    for entry in fs::read_dir(versions).map_err(err)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.issues.push(ScanIssue {
                    id: String::new(),
                    message: err(error),
                });
                continue;
            }
        };
        let name = entry.file_name();
        let Some(id) = name.to_str() else {
            report.issues.push(ScanIssue {
                id: name.to_string_lossy().into_owned(),
                message: "Version directory name is not valid UTF-8".into(),
            });
            continue;
        };
        match scan_instance_at_root(&root, id) {
            Ok(Some(instance)) => report.instances.push(instance),
            Ok(None) => {}
            Err(message) => report.issues.push(ScanIssue {
                id: id.into(),
                message,
            }),
        }
    }
    report.instances.sort_by(|a, b| a.id.cmp(&b.id));
    report
        .issues
        .sort_by(|a, b| a.id.cmp(&b.id).then(a.message.cmp(&b.message)));
    Ok(report)
}
/// Read one selected instance without discarding its scan error or making it
/// depend on every other instance in the game directory.
pub fn scan_instance(root: &Path, id: &str) -> Result<Option<Instance>> {
    identifier(id)?;
    let root = fs::canonicalize(root).map_err(err)?;
    if !root.is_dir() {
        return Err("Game root is not a directory".into());
    }
    safe_join(&root, "versions")?;
    scan_instance_at_root(&root, id)
}

fn scan_instance_at_root(root: &Path, id: &str) -> Result<Option<Instance>> {
    identifier(id)?;
    let version_folder = safe_join(root, format!("versions/{id}"))?;
    if !version_folder.is_dir() {
        return Ok(None);
    }
    if !safe_join(root, format!("versions/{id}/{id}.json"))?.is_file() {
        return Ok(None);
    }
    let data = metadata(root, id, &mut HashSet::new())?;
    let loader = loader_description(&data);
    let content_scope = content_scope::inspect(root, id)?;
    let isolated = content_scope.is_isolated();
    let folder = content_scope::verified_base(root, id, content_scope)?;
    let mods = safe_join(&folder, "mods")?;
    let mut mod_count = 0;
    if mods.is_dir() {
        for entry in fs::read_dir(&mods).map_err(err)? {
            let entry = entry.map_err(err)?;
            let entry_path = entry.path();
            let relative = entry_path
                .strip_prefix(root)
                .map_err(|_| "Mod entry is outside the game root")?;
            let path = safe_join(root, relative)?;
            if path.is_file() && path.extension().is_some_and(|ext| ext == "jar") {
                mod_count += 1;
            }
        }
    }
    let minecraft_version = minecraft_version(&data).to_string();
    Ok(Some(Instance {
        id: id.into(),
        minecraft_version,
        loader,
        java_major: required_java(&data),
        mod_count,
        isolated,
    }))
}
fn maven_path(name: &str) -> Result<String> {
    let (name, extension) = name.split_once('@').unwrap_or((name, "jar"));
    let p: Vec<_> = name.split(':').collect();
    if !(3..=4).contains(&p.len()) {
        return Err(format!("Invalid Maven coordinate: {name}"));
    }
    for component in p.iter().chain(std::iter::once(&extension)) {
        identifier(component)?;
    }
    let classifier = p.get(3).map(|c| format!("-{c}")).unwrap_or_default();
    Ok(format!(
        "{}/{}/{}/{}-{}{classifier}.{extension}",
        p[0].replace('.', "/"),
        p[1],
        p[2],
        p[1],
        p[2]
    ))
}
fn library_path(root: &Path, artifact: &Value, name: &str) -> Result<PathBuf> {
    let relative = if let Some(p) = artifact["path"].as_str() {
        p.into()
    } else {
        maven_path(name)?
    };
    // Path joining preserves an absolute metadata path as absolute, allowing
    // safe_join to reject it instead of disguising it as libraries//absolute.
    safe_join(root, Path::new("libraries").join(relative))
}
pub fn extract_natives(archive: &Path, target: &Path, excludes: &[Value]) -> Result<()> {
    extract_natives_for(archive, target, excludes, platform::Platform::current()?)
}

fn extract_natives_for(
    archive: &Path,
    target: &Path,
    excludes: &[Value],
    platform: platform::Platform,
) -> Result<()> {
    let file = fs::File::open(archive).map_err(err)?;
    let mut jar = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", archive.display()))?;
    for i in 0..jar.len() {
        let mut entry = jar.by_index(i).map_err(err)?;
        if excludes
            .iter()
            .filter_map(Value::as_str)
            .any(|prefix| entry.name().starts_with(prefix))
        {
            continue;
        }
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| format!("Unsafe native archive entry: {}", entry.name()))?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            continue;
        }
        let Some(name) = enclosed.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if !platform.native_filename(name) {
            continue;
        }
        if entry.size() > 256 * 1024 * 1024 {
            return Err("Native archive entry exceeds 256 MiB".into());
        }
        let path = safe_join(target, name)?;
        // Never truncate an inode that a running JVM may have mmap'ed. Each
        // concurrent plan writes a unique same-directory file and atomically
        // replaces the directory entry only after extraction succeeds. Existing
        // mappings keep their complete old inode; failed copies clean up on drop.
        let mut output = tempfile::NamedTempFile::new_in(target).map_err(err)?;
        std::io::copy(&mut entry, &mut output).map_err(err)?;
        output.persist(&path).map_err(err)?;
    }
    Ok(())
}
fn expand(
    raw: &[Value],
    replacements: &BTreeMap<&str, String>,
    context: &RuleContext,
) -> Result<Vec<String>> {
    let mut result = vec![];
    for entry in raw {
        let value = if entry.is_object() {
            if !context.allowed(entry)? {
                continue;
            }
            &entry["value"]
        } else {
            entry
        };
        let list = if let Some(list) = value.as_array() {
            list.clone()
        } else {
            vec![value.clone()]
        };
        for v in list {
            let mut s = v.as_str().ok_or("Invalid argument metadata")?.to_string();
            for (key, value) in replacements {
                s = s.replace(&format!("${{{key}}}"), value);
            }
            if s.contains("${") {
                return Err(format!("Unsupported launch placeholder: {s}"));
            }
            result.push(s);
        }
    }
    Ok(result)
}
/// Complete metadata may repeat a jar through several library declarations.
/// Keep the first occurrence of each exact path, without collapsing different
/// versions/classifiers or changing module-related JVM arguments.
fn deduplicate_launch_paths(classpath: &mut Vec<PathBuf>, native_jars: &mut Vec<(PathBuf, Value)>) {
    let mut seen = HashSet::new();
    classpath.retain(|path| seen.insert(path.clone()));
    seen.clear();
    native_jars.retain(|(path, _)| seen.insert(path.clone()));
}

pub fn build_launch_plan(
    root: &Path,
    project: &Path,
    id: &str,
    player: &str,
    memory_gib: u32,
) -> Result<LaunchPlan> {
    build_launch_plan_with_java(
        root,
        project,
        id,
        player,
        memory_gib,
        &java::JavaSelection::Auto,
        &[],
    )
}

/// Build an offline plan using the same Java policy as discovery and settings.
pub fn build_launch_plan_with_java(
    root: &Path,
    project: &Path,
    id: &str,
    player: &str,
    memory_gib: u32,
    java: &java::JavaSelection,
    extra_java_paths: &[String],
) -> Result<LaunchPlan> {
    build_launch_plan_with_options(
        root,
        project,
        id,
        player,
        memory_gib,
        java,
        extra_java_paths,
        &LaunchOptions::default(),
    )
}

/// Existing callers keep default argv; the desktop captures these options once
/// before handing preparation to its worker so later preference edits cannot
/// change a submitted game's plan.
pub fn build_launch_plan_with_options(
    root: &Path,
    project: &Path,
    id: &str,
    player: &str,
    memory_gib: u32,
    java: &java::JavaSelection,
    extra_java_paths: &[String],
    options: &LaunchOptions,
) -> Result<LaunchPlan> {
    build_launch_plan_inner(
        root,
        project,
        id,
        player,
        memory_gib,
        None,
        java,
        extra_java_paths,
        options,
    )
}

pub fn build_launch_plan_authenticated(
    root: &Path,
    project: &Path,
    id: &str,
    identity: &OnlineIdentity,
    memory_gib: u32,
) -> Result<LaunchPlan> {
    build_launch_plan_authenticated_with_java(
        root,
        project,
        id,
        identity,
        memory_gib,
        &java::JavaSelection::Auto,
        &[],
    )
}

/// Authenticated arguments remain private even when Java validation fails.
pub fn build_launch_plan_authenticated_with_java(
    root: &Path,
    project: &Path,
    id: &str,
    identity: &OnlineIdentity,
    memory_gib: u32,
    java: &java::JavaSelection,
    extra_java_paths: &[String],
) -> Result<LaunchPlan> {
    build_launch_plan_authenticated_with_options(
        root,
        project,
        id,
        identity,
        memory_gib,
        java,
        extra_java_paths,
        &LaunchOptions::default(),
    )
}

pub fn build_launch_plan_authenticated_with_options(
    root: &Path,
    project: &Path,
    id: &str,
    identity: &OnlineIdentity,
    memory_gib: u32,
    java: &java::JavaSelection,
    extra_java_paths: &[String],
    options: &LaunchOptions,
) -> Result<LaunchPlan> {
    if identity.uuid.len() != 32
        || !identity.uuid.chars().all(|c| c.is_ascii_hexdigit())
        || identity.access_token.is_empty()
        || identity.client_id.is_empty()
        || !identity.xuid.chars().all(|c| c.is_ascii_digit())
    {
        return Err("Invalid authenticated Minecraft identity".into());
    }
    build_launch_plan_inner(
        root,
        project,
        id,
        &identity.name,
        memory_gib,
        Some(identity),
        java,
        extra_java_paths,
        options,
    )
    .map_err(|error| error.replace(&identity.access_token, "<redacted>"))
}
fn build_launch_plan_inner(
    root: &Path,
    project: &Path,
    id: &str,
    player: &str,
    memory_gib: u32,
    online: Option<&OnlineIdentity>,
    java: &java::JavaSelection,
    extra_java_paths: &[String],
    options: &LaunchOptions,
) -> Result<LaunchPlan> {
    options.validate()?;
    identifier(id)?;
    if !Regex::new(r"^[A-Za-z0-9_]{3,16}$")
        .unwrap()
        .is_match(player)
    {
        return Err(
            "Offline player name must contain 3–16 ASCII letters, digits or underscores".into(),
        );
    }
    if !(2..=64).contains(&memory_gib) {
        return Err("Memory must be between 2 and 64 GiB".into());
    }
    let root = fs::canonicalize(root).map_err(err)?;
    let data = metadata(&root, id, &mut HashSet::new())?;
    let game_scope = content_scope::inspect(&root, id)?;
    let game_dir = content_scope::verified_base(&root, id, game_scope)?;
    let context = RuleContext::current()?;
    let dependencies = dependencies::resolve(&root, id, &data, &context)?;
    let dependencies::InstalledDependencies {
        classpath,
        native_jars,
        asset_id,
        log_config,
        missing,
        asset_index: _,
    } = dependencies;
    if missing.total > 0 {
        return Err(format!(
            "Missing {} installed dependencies (downloads are not available in phase 1):\n{}{}",
            missing.total,
            missing
                .paths
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
            if missing.total > 12 { "\n…" } else { "" }
        ));
    }
    let logging = &data["logging"]["client"];
    let loader = loader_description(&data);
    let exact_java = loader.starts_with("Forge ") || loader.starts_with("NeoForge ");
    let java = java::select(
        project,
        Some(&root),
        extra_java_paths,
        java,
        required_java(&data),
        exact_java,
    )?;
    let natives = safe_join(&root, format!(".pcl-linux/natives/{id}"))?;
    fs::create_dir_all(&natives).map_err(err)?;
    for (path, lib) in native_jars {
        extract_natives(
            &path,
            &natives,
            lib["extract"]["exclude"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?;
    }
    let mut uuid = md5::compute(format!("OfflinePlayer:{player}")).0;
    uuid[6] = (uuid[6] & 0x0f) | 0x30;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    let uuid = uuid.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let mut replacements = BTreeMap::from([
        ("auth_player_name", player.into()),
        ("version_name", id.into()),
        ("game_directory", game_dir.display().to_string()),
        ("assets_root", root.join("assets").display().to_string()),
        ("assets_index_name", asset_id.into()),
        ("auth_uuid", uuid),
        ("auth_access_token", "0".into()),
        ("clientid", "0".into()),
        ("auth_xuid", "0".into()),
        ("user_type", "legacy".into()),
        ("user_properties", "{}".into()),
        ("auth_session", "0".into()),
        (
            "version_type",
            data["type"].as_str().unwrap_or("release").into(),
        ),
        ("natives_directory", natives.display().to_string()),
        ("launcher_name", "PCL-RH".into()),
        ("launcher_version", "0.1.0".into()),
        (
            "classpath",
            std::env::join_paths(&classpath)
                .map_err(|error| format!("Invalid Java classpath: {error}"))?
                .into_string()
                .map_err(|_| "Java classpath is not UTF-8")?,
        ),
        (
            "classpath_separator",
            context.platform.classpath_separator().into(),
        ),
        (
            "library_directory",
            root.join("libraries").display().to_string(),
        ),
    ]);
    replacements.insert(
        "game_assets",
        root.join("assets/virtual/legacy").display().to_string(),
    );
    if let Some(identity) = online {
        replacements.insert("auth_uuid", identity.uuid.clone());
        replacements.insert("auth_access_token", identity.access_token.clone());
        replacements.insert("clientid", identity.client_id.clone());
        replacements.insert("client_id", identity.client_id.clone());
        replacements.insert("auth_xuid", identity.xuid.clone());
        replacements.insert("user_type", "msa".into());
        replacements.insert(
            "auth_session",
            format!("token:{}:{}", identity.access_token, identity.uuid),
        );
    }
    let raw_game = data["arguments"]["game"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            text(&data, "minecraftArguments")
                .split_whitespace()
                .map(|s| Value::String(s.into()))
                .collect()
        });
    let raw_jvm = data["arguments"]["jvm"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            vec![
                "-Djava.library.path=${natives_directory}".into(),
                "-cp".into(),
                "${classpath}".into(),
            ]
        });
    let mut argument_context = context;
    argument_context.custom_resolution = options.custom_resolution();
    if let WindowSize::Custom { width, height } = options.window {
        replacements.insert("resolution_width", width.to_string());
        replacements.insert("resolution_height", height.to_string());
    }
    let mut args = vec![format!("-Xmx{memory_gib}G")];
    args.extend(expand(&raw_jvm, &replacements, &argument_context)?);
    if let Some(path) = log_config {
        args.push(text(logging, "argument").replace("${path}", &path.display().to_string()));
    }
    options.apply_jvm(&mut args);
    let main = text(&data, "mainClass");
    if main.is_empty() {
        return Err("Version has no mainClass".into());
    }
    args.push(main.into());
    let mut game_args = expand(&raw_game, &replacements, &argument_context)?;
    options.apply_game(&mut game_args);
    args.extend(game_args);
    // Java probing and native preparation may take time. A newly added/removed
    // marker must fail preparation rather than silently switch its game data.
    if content_scope::inspect(&root, id)? != game_scope {
        return Err("实例隔离目录在准备期间变化，请重新启动".into());
    }
    let log_path = safe_join(&root, format!(".pcl-linux/logs/{id}.log"))?;
    Ok(LaunchPlan {
        java,
        args,
        game_dir,
        log_path,
        version: id.into(),
        secret_token: online.map(|identity| identity.access_token.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture_dir() -> tempfile::TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/core-tests");
        fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new()
            .prefix("fixture-")
            .tempdir_in(base)
            .unwrap()
    }
    #[cfg(unix)]
    #[test]
    fn partial_scan_retains_valid_entries_and_reports_damaged_or_escaping_entries() {
        let root = fixture_dir();
        for (id, data) in [
            ("Good", r#"{"id":"Good","javaVersion":{"majorVersion":17}}"#),
            ("Broken", "{broken"),
            ("Array", "[]"),
            ("Child", r#"{"id":"Child","inheritsFrom":"Missing"}"#),
            ("bad:identifier", r#"{"id":"bad:identifier"}"#),
        ] {
            let folder = root.path().join("versions").join(id);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join(format!("{id}.json")), data).unwrap();
        }
        let outside = fixture_dir();
        fs::write(outside.path().join("Escape.json"), r#"{"id":"Escape"}"#).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("versions/Escape")).unwrap();
        let report = scan_instances_report(root.path()).unwrap();
        assert_eq!(
            report
                .instances
                .iter()
                .map(|instance| instance.id.as_str())
                .collect::<Vec<_>>(),
            ["Good"]
        );
        assert_eq!(report.instances[0].java_major, 17);
        assert_eq!(
            report
                .issues
                .iter()
                .map(|issue| issue.id.as_str())
                .collect::<Vec<_>>(),
            ["Array", "Broken", "Child", "Escape", "bad:identifier"]
        );
        assert!(report.issues.iter().all(|issue| !issue.message.is_empty()));
        assert!(report
            .issues
            .iter()
            .find(|issue| issue.id == "Escape")
            .unwrap()
            .message
            .contains("escapes"));
        assert_eq!(scan_instances(root.path()).unwrap()[0].id, "Good");
    }
    #[cfg(unix)]
    #[test]
    fn scan_rejects_versions_boundary_escape_and_excludes_escaping_mods() {
        let root = fixture_dir();
        let outside = fixture_dir();
        std::os::unix::fs::symlink(outside.path(), root.path().join("versions")).unwrap();
        assert!(scan_instances_report(root.path())
            .unwrap_err()
            .contains("escapes"));
        fs::remove_file(root.path().join("versions")).unwrap();
        for id in ["Good", "UnsafeMods"] {
            let folder = root.path().join("versions").join(id);
            fs::create_dir_all(&folder).unwrap();
            fs::write(
                folder.join(format!("{id}.json")),
                json!({"id":id}).to_string(),
            )
            .unwrap();
        }
        let mods = root.path().join("versions/UnsafeMods/mods");
        fs::create_dir_all(&mods).unwrap();
        let external_jar = outside.path().join("external.jar");
        fs::write(&external_jar, "fixture").unwrap();
        std::os::unix::fs::symlink(&external_jar, mods.join("external.jar")).unwrap();
        let internal_jar = root.path().join("shared/internal.jar");
        fs::create_dir_all(internal_jar.parent().unwrap()).unwrap();
        fs::write(&internal_jar, "fixture").unwrap();
        let good_mods = root.path().join("versions/Good/mods");
        fs::create_dir_all(&good_mods).unwrap();
        std::os::unix::fs::symlink(&internal_jar, good_mods.join("internal.jar")).unwrap();
        let report = scan_instances_report(root.path()).unwrap();
        assert_eq!(report.instances.len(), 1);
        assert_eq!(report.instances[0].id, "Good");
        assert_eq!(report.instances[0].mod_count, 1);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].id, "UnsafeMods");
        assert!(report.issues[0].message.contains("escapes"));
        fs::remove_dir_all(root.path().join("versions")).unwrap();
        fs::write(root.path().join("versions"), "not a directory").unwrap();
        assert!(scan_instances_report(root.path()).is_err());
    }
    #[test]
    fn repeated_gson_and_native_paths_keep_first_order_and_distinct_versions() {
        let gson = PathBuf::from("libraries/com/google/code/gson/gson/2.10.1/gson-2.10.1.jar");
        let older_gson = PathBuf::from("libraries/com/google/code/gson/gson/2.10/gson-2.10.jar");
        let native = PathBuf::from("libraries/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar");
        let second_native = PathBuf::from(
            "libraries/org/lwjgl/lwjgl-glfw/3.3.3/lwjgl-glfw-3.3.3-natives-linux.jar",
        );
        let client = PathBuf::from("versions/Example Instance/Example Instance.jar");
        let expected = vec![
            gson.clone(),
            native.clone(),
            older_gson.clone(),
            client.clone(),
        ];
        let mut classpath = vec![
            gson.clone(),
            native.clone(),
            gson,
            older_gson,
            native.clone(),
            client,
        ];
        let first_metadata = json!({"extract":{"exclude":["META-INF/"]}});
        let mut natives = vec![
            (native.clone(), first_metadata.clone()),
            (second_native.clone(), json!({})),
            (native.clone(), json!({"duplicate":true})),
        ];
        deduplicate_launch_paths(&mut classpath, &mut natives);
        assert_eq!(classpath, expected);
        assert_eq!(
            natives,
            vec![(native, first_metadata), (second_native, json!({}))]
        );
    }
    #[test]
    fn loader_versions_use_concrete_metadata() {
        assert_eq!(
            loader_description(&json!({"arguments":{"game":["--fml.forgeVersion","47.4.18"]}})),
            "Forge 47.4.18"
        );
        assert_eq!(
            loader_description(
                &json!({"libraries":[{"name":"net.minecraftforge:fmlloader:1.20.1-47.3.0"}]})
            ),
            "Forge 47.3.0"
        );
        assert_eq!(
            loader_description(
                &json!({"libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]})
            ),
            "Fabric 0.16.0"
        );
        assert_eq!(
            loader_description(
                &json!({"libraries":[{"name":"net.neoforged:neoforge:21.1.1"},{"name":"net.minecraftforge:fmlloader:1.20.1-47.3.0"}]})
            ),
            "NeoForge 21.1.1"
        );
        assert_eq!(
            loader_description(
                &json!({"libraries":[{"name":"net.minecraftforge:mergetool:1.1.1"}]})
            ),
            "Vanilla"
        );
    }
    #[test]
    fn forge_java_policy_rejects_newer_major() {
        assert!(java::compatible(17, 17, true));
        assert!(!java::compatible(25, 17, true));
        assert!(!java::compatible(8, 17, true));
        assert!(java::compatible(25, 17, false));
    }
    #[test]
    fn rules_obey_order_os_and_false_features() {
        let context = RuleContext {
            platform: platform::Platform {
                os: platform::OperatingSystem::Linux,
                arch: platform::Architecture::X86_64,
            },
            release: "6.8.0".into(),
            custom_resolution: false,
        };
        assert!(context.allowed(&json!({})).unwrap());
        assert!(!context.allowed(&json!({"rules":[]})).unwrap());
        assert!(context.allowed(&json!({"rules":[{"action":"allow","os":{"name":"linux","arch":"x86_64","version":"^6"}}]})).unwrap());
        assert!(!context
            .allowed(
                &json!({"rules":[{"action":"allow"},{"action":"disallow","os":{"name":"linux"}}]})
            )
            .unwrap());
        assert!(!context
            .allowed(&json!({"rules":[{"action":"allow","features":{"is_demo_user":true}}]}))
            .unwrap());
        assert!(context
            .allowed(&json!({"rules":[{"action":"allow","features":{"is_demo_user":false}}]}))
            .unwrap());
        assert!(!context
            .library_allowed(&json!({"name":"x:y:1:natives-linux-arm64"}))
            .unwrap());
    }
    #[test]
    fn child_replaces_library_version_but_keeps_classifiers() {
        let merged = merge(
            json!({"libraries":[{"name":"a:b:1"},{"name":"a:b:1:natives-linux"}],"arguments":{"game":["base"]}}),
            json!({"libraries":[{"name":"a:b:2"}],"arguments":{"game":["child"]}}),
        );
        assert_eq!(
            merged["libraries"],
            json!([{"name":"a:b:1:natives-linux"},{"name":"a:b:2"}])
        );
        assert_eq!(merged["arguments"]["game"], json!(["base", "child"]));
    }
    #[cfg(unix)]
    #[test]
    fn traversal_and_symlink_escape_rejected() {
        let root = fixture_dir();
        for relative in ["../outside", "/absolute", "x/../../outside", "x\\y"] {
            assert!(safe_join(root.path(), relative).is_err());
        }
        for id in ["../x", "..", "/tmp/a", "a\\b"] {
            assert!(identifier(id).is_err());
        }
        let outside = fixture_dir();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(safe_join(root.path(), "escape/file").is_err());
        assert!(safe_join(root.path(), "new/nested/file").is_ok());
    }
    #[test]
    fn inheritance_cycle_is_reported() {
        let root = fixture_dir();
        for (id, parent) in [("a", "b"), ("b", "a")] {
            let folder = root.path().join("versions").join(id);
            fs::create_dir_all(&folder).unwrap();
            fs::write(
                folder.join(format!("{id}.json")),
                json!({"id":id,"inheritsFrom":parent}).to_string(),
            )
            .unwrap();
        }
        assert!(metadata(root.path(), "a", &mut HashSet::new())
            .unwrap_err()
            .contains("cycle"));
    }
    #[test]
    fn inherited_client_jar_is_selected() {
        let root = fixture_dir();
        for (id, data) in [
            ("base", json!({"id":"base","mainClass":"Main"})),
            ("child", json!({"id":"child","inheritsFrom":"base"})),
        ] {
            let folder = root.path().join("versions").join(id);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join(format!("{id}.json")), data.to_string()).unwrap();
        }
        fs::write(root.path().join("versions/base/base.jar"), "").unwrap();
        assert_eq!(
            metadata(root.path(), "child", &mut HashSet::new()).unwrap()["jar"],
            "base"
        );
    }
    #[cfg(unix)]
    #[test]
    fn modern_nested_natives_are_flattened() {
        use std::io::Write;
        let root = fixture_dir();
        let archive = root.path().join("native.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        zip.start_file(
            "linux/x64/liblwjgl.so",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"native").unwrap();
        zip.start_file("META-INF/no.so", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"excluded").unwrap();
        zip.start_file(
            "linux/x64/liblwjgl.so.sha1",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"not a shared library").unwrap();
        zip.finish().unwrap();
        let target = root.path().join("natives");
        fs::create_dir(&target).unwrap();
        let library = target.join("liblwjgl.so");
        fs::write(&library, b"old native library still used by a JVM").unwrap();
        let mut old_handle = fs::File::open(&library).unwrap();
        use std::os::unix::fs::MetadataExt;
        let old_inode = old_handle.metadata().unwrap().ino();
        extract_natives_for(
            &archive,
            &target,
            &[json!("META-INF/")],
            platform::Platform {
                os: platform::OperatingSystem::Linux,
                arch: platform::Architecture::X86_64,
            },
        )
        .unwrap();
        assert_eq!(fs::read(&library).unwrap(), b"native");
        assert_ne!(fs::metadata(&library).unwrap().ino(), old_inode);
        let mut old_contents = Vec::new();
        std::io::Read::read_to_end(&mut old_handle, &mut old_contents).unwrap();
        assert_eq!(old_contents, b"old native library still used by a JVM");
        assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
        assert!(!target.join("no.so").exists());
        assert!(!target.join("liblwjgl.so.sha1").exists());
    }
    #[cfg(unix)]
    #[test]
    fn authenticated_fixture_has_real_process_arguments_but_safe_output() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = fixture_dir();
        let root = fixture.path().join("minecraft");
        let version = root.join("versions/fixture");
        fs::create_dir_all(&version).unwrap();
        let java = fixture.path().join("runtime/bin/java");
        fs::create_dir_all(java.parent().unwrap()).unwrap();
        fs::write(
            &java,
            "#!/bin/sh\nprintf 'openjdk version \"21.0.1\"\\n' >&2\n",
        )
        .unwrap();
        fs::set_permissions(&java, fs::Permissions::from_mode(0o755)).unwrap();
        let arguments = json!([
            "--username",
            "${auth_player_name}",
            "--uuid",
            "${auth_uuid}",
            "--accessToken",
            "${auth_access_token}",
            "--userType",
            "${user_type}",
            "--xuid",
            "${auth_xuid}",
            "--clientId",
            "${clientid}",
            "--session",
            "${auth_session}",
            "--embedded=prefix:${auth_access_token}:suffix"
        ]);
        fs::write(version.join("fixture.json"), json!({"id":"fixture","mainClass":"Main","javaVersion":{"majorVersion":21},"arguments":{"game":arguments,"jvm":[]},"libraries":[]}).to_string()).unwrap();
        fs::write(version.join("fixture.jar"), "").unwrap();
        let identity = OnlineIdentity {
            name: "RealPlayer".into(),
            uuid: "1234567890abcdef1234567890abcdef".into(),
            access_token: "mc-token-secret".into(),
            xuid: "12345".into(),
            client_id: "project-client".into(),
        };
        let selection = java::JavaSelection::Manual {
            path: java.display().to_string(),
        };
        let online = build_launch_plan_authenticated_with_java(
            &root,
            fixture.path(),
            "fixture",
            &identity,
            4,
            &selection,
            &[],
        )
        .unwrap();
        for expected in [
            "RealPlayer",
            &identity.uuid,
            &identity.access_token,
            "msa",
            "12345",
            "project-client",
            &format!("token:{}:{}", identity.access_token, identity.uuid),
        ] {
            assert!(online.args.iter().any(|arg| arg == expected));
        }
        assert!(online
            .args
            .contains(&"--embedded=prefix:mc-token-secret:suffix".into()));
        assert!(!serde_json::to_string(&online)
            .unwrap()
            .contains(&identity.access_token));
        assert!(!format!("{online:?}").contains(&identity.access_token));
        let offline = build_launch_plan_with_java(
            &root,
            fixture.path(),
            "fixture",
            "OfflinePlayer",
            4,
            &selection,
            &[],
        )
        .unwrap();
        assert!(offline.args.contains(&"legacy".into()));
        assert!(offline.args.contains(&"0".into()));
        assert!(!offline.args.contains(&"msa".into()));
        let json = serde_json::to_value(&offline).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 5);
        assert_eq!(json["args"], serde_json::to_value(&offline.args).unwrap());
        // Even errors caused by mixed known and unknown placeholders must be safe.
        fs::write(version.join("fixture.json"), json!({"id":"fixture","mainClass":"Main","arguments":{"game":["${auth_access_token}-${unknown}"],"jvm":[]},"libraries":[]}).to_string()).unwrap();
        let error = build_launch_plan_authenticated_with_java(
            &root,
            fixture.path(),
            "fixture",
            &identity,
            4,
            &selection,
            &[],
        )
        .unwrap_err();
        assert!(!error.contains(&identity.access_token));
    }
}

#[cfg(all(test, unix))]
#[path = "launch_options_tests.rs"]
mod launch_options_tests;

/// Apply the same Linux library rules used by launch planning.
pub fn library_allowed(lib: &Value) -> Result<bool> {
    RuleContext::current()?.library_allowed(lib)
}
