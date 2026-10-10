//! RH Code owns projects, conversations and workspace layout independently of
//! panel lifetimes. The native store is authoritative; renderer plugin state
//! cannot grant filesystem access or overwrite a newer project revision.
mod files;
use crate::Shared;
use files::{atomic, atomic_with_permissions, inventory, read, safe_file};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::State;

const LIMIT: usize = 8 * 1024 * 1024;
const MANIFESTS: &str = include_str!("../../../../code/plugins.json");
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: String,
    pub job_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub mode: String,
    pub messages: Vec<Message>,
    pub active_job: Option<String>,
    #[serde(default)]
    pub draft: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub minecraft: String,
    pub loaders: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub folder: String,
    pub updated_at: u64,
    pub targets: Vec<Target>,
    pub conversations: Vec<Conversation>,
    pub layout: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    schema_version: u32,
    projects: Vec<Project>,
    disabled_plugins: Vec<String>,
}
impl Default for Document {
    fn default() -> Self {
        Self {
            schema_version: 1,
            projects: vec![],
            disabled_plugins: vec![],
        }
    }
}
pub struct Store {
    root: PathBuf,
    gate: Mutex<()>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn id() -> String {
    hash(
        format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
        .as_bytes(),
    )[..24]
        .to_owned()
}
fn text(a: &Value, key: &str, max: usize) -> Result<String, String> {
    let s = a[key].as_str().ok_or_else(|| format!("Missing {key}"))?;
    if s.trim().is_empty()
        || s.chars().count() > max
        || s.chars()
            .any(|c| c == '\0' || (c.is_control() && c != '\n' && c != '\t'))
    {
        return Err(format!("Invalid {key}"));
    }
    Ok(s.to_owned())
}
fn manifests() -> Value {
    serde_json::from_str(MANIFESTS).expect("bundled plugin manifests")
}
fn ensure_plugin(d: &Document, plugin: &str) -> Result<(), String> {
    if d.disabled_plugins.iter().any(|v| v == plugin) {
        Err(format!("Plugin disabled: {plugin}"))
    } else {
        Ok(())
    }
}
fn checked_layout(node: &Value, seen: &mut HashSet<String>, depth: usize) -> Result<(), String> {
    if node.is_null() {
        if depth > 0 {
            return Err("A split area cannot be empty".into());
        }
        return Ok(());
    }
    let identifier = text(node, "id", 100)?;
    if depth > 7 || seen.len() >= 31 || !seen.insert(identifier) {
        return Err("Invalid workspace layout".into());
    }
    match node["type"].as_str() {
        Some("area")
            if node["view"].as_str().is_some_and(|v| {
                [
                    "chat", "threads", "graph", "files", "source", "targets", "changes", "logs",
                    "porting", "legacy",
                ]
                .contains(&v)
            }) =>
        {
            Ok(())
        }
        Some("split")
            if [Some("row"), Some("column")].contains(&node["axis"].as_str())
                && node["ratio"]
                    .as_f64()
                    .is_some_and(|v| (0.15..=0.85).contains(&v)) =>
        {
            let children = node["children"]
                .as_array()
                .filter(|v| v.len() == 2)
                .ok_or("Invalid split children")?;
            for child in children {
                checked_layout(child, seen, depth + 1)?;
            }
            Ok(())
        }
        _ => Err("Invalid workspace area".into()),
    }
}
impl Store {
    pub fn new(project: &Path) -> Self {
        Self {
            root: project.join(".pcl-rust/rh-code"),
            gate: Mutex::new(()),
        }
    }
    fn load(&self) -> Result<(Document, String), String> {
        let path = self.root.join("workspace.json");
        let bytes = match read(&path, LIMIT) {
            Ok(b) => b,
            Err(_)
                if fs::symlink_metadata(&path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok((Document::default(), "new".into()))
            }
            Err(e) => return Err(format!("RH Code state retained for recovery: {e}")),
        };
        let d: Document = serde_json::from_slice(&bytes)
            .map_err(|_| "RH Code state is unreadable; retained for recovery")?;
        if d.schema_version != 1 || d.projects.len() > 128 {
            return Err("Unsupported RH Code state; retained for recovery".into());
        }
        let list = manifests();
        let list = list.as_array().unwrap();
        let mut disabled = HashSet::new();
        for id in &d.disabled_plugins {
            let plugin = list
                .iter()
                .find(|p| p["id"] == *id)
                .ok_or("Unknown stored product plugin")?;
            if plugin["required"] == true || !disabled.insert(id) {
                return Err("Invalid stored plugin state".into());
            }
        }
        for p in list {
            if !d
                .disabled_plugins
                .contains(&p["id"].as_str().unwrap().to_owned())
                && p["dependencies"].as_array().unwrap().iter().any(|id| {
                    d.disabled_plugins
                        .contains(&id.as_str().unwrap().to_owned())
                })
            {
                return Err("Stored plugin dependency is disabled; retained for recovery".into());
            }
        }
        let mut ids = HashSet::new();
        for p in &d.projects {
            if p.id.len() != 24
                || !p.id.bytes().all(|v| v.is_ascii_hexdigit())
                || !ids.insert(&p.id)
                || p.conversations.len() > 128
            {
                return Err("Invalid RH Code project identity".into());
            }
            if p.targets.iter().any(|t| {
                t.minecraft.is_empty()
                    || t.minecraft.len() > 50
                    || t.loaders.is_empty()
                    || t.loaders
                        .iter()
                        .any(|l| !["Fabric", "NeoForge"].contains(&l.as_str()))
            }) {
                return Err("Invalid stored build target".into());
            }
            if p.name.trim().is_empty()
                || p.name.chars().count() > 120
                || !Path::new(&p.folder).is_absolute()
                || p.targets.is_empty()
                || p.targets.len() > 32
            {
                return Err("Invalid stored RH Code project".into());
            }
            let mut conversations = HashSet::new();
            for c in &p.conversations {
                if c.id.len() != 24
                    || !c.id.bytes().all(|v| v.is_ascii_hexdigit())
                    || !conversations.insert(&c.id)
                    || !["ask", "plan", "execute"].contains(&c.mode.as_str())
                    || c.messages.len() > 4096
                    || c.draft.chars().count() > 12000
                {
                    return Err("Invalid stored conversation".into());
                }
                let mut messages = HashSet::new();
                for m in &c.messages {
                    if !messages.insert(&m.id)
                        || m.content.chars().count() > 128000
                        || !["user", "assistant", "notice"].contains(&m.role.as_str())
                    {
                        return Err("Invalid stored message".into());
                    }
                }
            }
            checked_layout(&p.layout, &mut HashSet::new(), 0)?;
        }
        Ok((d, hash(&bytes)))
    }
    fn project_mut<'a>(d: &'a mut Document, a: &Value) -> Result<&'a mut Project, String> {
        d.projects
            .iter_mut()
            .find(|p| Some(p.id.as_str()) == a["projectId"].as_str())
            .ok_or_else(|| "Choose an RH Code project".into())
    }
    fn path(p: &Project, relative: &str) -> Result<PathBuf, String> {
        let rel = Path::new(relative);
        if rel.is_absolute()
            || rel.components().any(|v| !matches!(v, Component::Normal(_)))
            || relative.split(['/', '\\']).any(|v| v.starts_with('.'))
        {
            return Err("Choose a visible project file".into());
        }
        let root = fs::canonicalize(&p.folder).map_err(|e| e.to_string())?;
        let mut path = root.clone();
        for part in rel.components() {
            path.push(part.as_os_str());
            if fs::symlink_metadata(&path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Symbolic links are not project files".into());
            }
        }
        if !fs::canonicalize(&path)
            .map_err(|e| e.to_string())?
            .starts_with(&root)
        {
            return Err("File leaves the project".into());
        }
        Ok(path)
    }
    pub fn call(&self, operation: &str, a: Value) -> Result<Value, String> {
        self.call_inner(operation, a, None)
    }
    // Artifact bytes are accepted only from the private, receipt-verifying host,
    // never from the generic renderer operation dispatcher.
    pub(crate) fn adopt(&self, a: Value, source: Value) -> Result<Value, String> {
        self.call_inner("source_adopt", a, Some(source))
    }
    fn call_inner(
        &self,
        operation: &str,
        a: Value,
        source: Option<Value>,
    ) -> Result<Value, String> {
        let _gate = self.gate.lock().map_err(|_| "RH Code store unavailable")?;
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        // Share one native store lease across launcher processes. Taking the
        // lease before reading makes disk CAS meaningful across those processes.
        safe_file(&self.root.join("store.lock"))?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lease = options
            .open(self.root.join("store.lock"))
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err("Another launcher is updating RH Code".into());
            }
        }
        let (mut d, revision) = self.load()?;
        if operation == "bootstrap" {
            return Ok(
                json!({"revision":revision,"projects":d.projects,"disabledPlugins":d.disabled_plugins,"plugins":manifests()}),
            );
        }
        if operation == "artifact_owner" || operation == "source_adopt" {
            ensure_plugin(&d, "rh.changes")?;
            if operation == "source_adopt" {
                ensure_plugin(&d, "rh.source")?;
            }
            let p = Self::project_mut(&mut d, &a)?;
            let job = text(&a, "jobId", 100)?;
            if !p
                .conversations
                .iter()
                .any(|c| c.messages.iter().any(|m| m.job_id.as_deref() == Some(&job)))
            {
                return Err("Artifact does not belong to this project".into());
            }
            if operation == "source_adopt" {
                if a["expectedRevision"].as_str() != Some(&revision) {
                    return Err("RH Code changed; refresh before adopting source".into());
                }
                files::adopt(
                    p,
                    &source.ok_or("Source adoption requires the native artifact bridge")?,
                )?;
            }
            return Ok(
                json!({"revision":revision,"projects":d.projects,"disabledPlugins":d.disabled_plugins,"plugins":manifests()}),
            );
        }
        if operation == "agent_bound" {
            ensure_plugin(&d, "rh.agent")?;
            let p = Self::project_mut(&mut d, &a)?;
            let c = p
                .conversations
                .iter()
                .find(|c| Some(c.id.as_str()) == a["conversationId"].as_str())
                .ok_or("Choose a project conversation")?;
            let job = text(&a, "jobId", 100)?;
            if c.active_job.as_deref() != Some(&job) {
                return Err("Agent task does not belong to this conversation".into());
            }
            return Ok(json!({"jobId":job,"projectId":p.id,"conversationId":c.id}));
        }
        if operation == "agent_context" {
            ensure_plugin(&d, "rh.agent")?;
            if a["expectedRevision"].as_str() != Some(&revision) {
                return Err("RH Code changed; refresh before starting the Agent".into());
            }
            let p = Self::project_mut(&mut d, &a)?;
            let c = p
                .conversations
                .iter()
                .find(|c| Some(c.id.as_str()) == a["conversationId"].as_str())
                .ok_or("Choose a project conversation")?;
            if c.active_job.is_some() {
                return Err("Wait for this conversation's Agent task to finish".into());
            }
            let request = text(&a, "request", 12000)?;
            let mode = text(&a, "mode", 20)?;
            let index = a["targetIndex"].as_u64().unwrap_or(0) as usize;
            p.targets.get(index).ok_or("Choose a build target")?;
            if !["ask", "plan", "execute"].contains(&mode.as_str()) {
                return Err("Invalid Agent mode".into());
            }
            // Freeze bounded UTF-8 source in Rust. The Agent host receives bytes,
            // not a renderer-supplied folder with ambient filesystem access.
            ensure_plugin(&d, "rh.files")?;
            let p = d
                .projects
                .iter()
                .find(|p| Some(p.id.as_str()) == a["projectId"].as_str())
                .unwrap();
            let inventory = inventory(p)?;
            let mut source = serde_json::Map::new();
            let mut source_files = vec![];
            let mut total = 0;
            let mut excerpt_budget = 24000usize;
            for rel in inventory {
                let bytes = read(&Self::path(p, &rel)?, 256000)?;
                total += bytes.len();
                if total > 256000 || source_files.len() >= 300 {
                    return Err(
                        "Agent source snapshot is too large; choose a narrower folder".into(),
                    );
                }
                use base64::Engine;
                source_files.push(json!({"path":rel,"sha256":hash(&bytes),"data":base64::engine::general_purpose::STANDARD.encode(&bytes)}));
                if let Ok(content) = String::from_utf8(bytes) {
                    if excerpt_budget > 0 {
                        let excerpt: String =
                            content.chars().take(excerpt_budget.min(5000)).collect();
                        excerpt_budget -= excerpt.chars().count();
                        source.insert(rel, Value::String(excerpt));
                    }
                }
            }
            let c = p
                .conversations
                .iter()
                .find(|c| Some(c.id.as_str()) == a["conversationId"].as_str())
                .unwrap();
            // Project history can grow to megabytes. Transport only the model
            // projection; the canonical log remains in the native document.
            let start = c.messages.len().saturating_sub(8);
            let messages: Vec<Value> = c.messages[start..].iter().map(|m|
                json!({"role":m.role,"content":m.content.chars().take(1800).collect::<String>()})).collect();
            return Ok(
                json!({"project":{"id":p.id,"name":p.name},"conversation":{"id":c.id,"messages":messages,"omittedMessages":start},"request":request,"mode":mode,"target":p.targets[index],"source":source,"sourceFiles":source_files}),
            );
        }
        if operation == "files" || operation == "file_read" {
            ensure_plugin(&d, "rh.files")?;
            let p = Self::project_mut(&mut d, &a)?;
            if operation == "file_read" {
                let rel = text(&a, "path", 4096)?;
                let bytes = read(&Self::path(p, &rel)?, 1024 * 1024)?;
                let content =
                    String::from_utf8(bytes.clone()).map_err(|_| "This file is not UTF-8 text")?;
                return Ok(json!({"path":rel,"content":content,"sha256":hash(&bytes)}));
            }
            return Ok(json!({"files":inventory(p)?}));
        }
        if a["expectedRevision"].as_str() != Some(&revision) {
            return Err("RH Code changed; refresh before saving".into());
        }
        match operation {
            "project_create" => {
                if d.projects.len() >= 128 {
                    return Err("Project limit reached".into());
                }
                let name = text(&a, "name", 120)?.trim().to_owned();
                let folder = text(&a, "folder", 4096)?;
                let canonical =
                    fs::canonicalize(&folder).map_err(|_| "Choose an existing project folder")?;
                if !canonical.is_dir() {
                    return Err("Choose a project folder".into());
                }
                if d.projects.iter().any(|p| Path::new(&p.folder) == canonical) {
                    return Err("This folder is already an RH Code project".into());
                }
                d.projects.push(Project {
                    id: id(),
                    name,
                    folder: canonical.to_string_lossy().into_owned(),
                    updated_at: now(),
                    targets: vec![Target {
                        minecraft: "1.21.1".into(),
                        loaders: vec!["Fabric".into(), "NeoForge".into()],
                    }],
                    conversations: vec![],
                    layout: Value::Null,
                });
            }
            "plugin_set" => {
                let key = text(&a, "id", 120)?;
                let enable = a["enabled"].as_bool().ok_or("Choose a plugin state")?;
                let list = manifests();
                let list = list.as_array().ok_or("Invalid bundled plugins")?;
                let plugin = list
                    .iter()
                    .find(|p| p["id"] == key)
                    .ok_or("Unknown bundled plugin")?;
                if !enable && plugin["required"] == true {
                    return Err("This plugin is required by RH Code".into());
                }
                if !enable
                    && key == "rh.agent"
                    && d.projects
                        .iter()
                        .any(|p| p.conversations.iter().any(|c| c.active_job.is_some()))
                {
                    return Err("Stop active Agent tasks before disabling this plugin".into());
                }
                if !enable
                    && list.iter().any(|p| {
                        !d.disabled_plugins
                            .contains(&p["id"].as_str().unwrap_or_default().to_owned())
                            && p["dependencies"]
                                .as_array()
                                .is_some_and(|deps| deps.iter().any(|v| v == &key))
                    })
                {
                    return Err("Disable dependent plugins first".into());
                }
                if enable
                    && plugin["dependencies"].as_array().is_some_and(|deps| {
                        deps.iter().any(|v| {
                            d.disabled_plugins
                                .contains(&v.as_str().unwrap_or_default().to_owned())
                        })
                    })
                {
                    return Err("Enable this plugin's dependencies first".into());
                }
                d.disabled_plugins.retain(|v| v != &key);
                if !enable {
                    d.disabled_plugins.push(key);
                }
            }
            "layout_save" => {
                checked_layout(&a["layout"], &mut HashSet::new(), 0)?;
                Self::project_mut(&mut d, &a)?.layout = a["layout"].clone();
            }
            "targets_save" => {
                ensure_plugin(&d, "rh.targets")?;
                let targets: Vec<Target> =
                    serde_json::from_value(a["targets"].clone()).map_err(|_| "Invalid targets")?;
                if targets.is_empty()
                    || targets.len() > 32
                    || targets.iter().any(|t| {
                        t.minecraft.is_empty()
                            || t.minecraft.len() > 50
                            || t.minecraft.chars().any(|v| v.is_control())
                            || t.loaders.is_empty()
                            || t.loaders
                                .iter()
                                .any(|v| !["Fabric", "NeoForge"].contains(&v.as_str()))
                    })
                {
                    return Err("Invalid build target".into());
                }
                Self::project_mut(&mut d, &a)?.targets = targets;
            }
            "conversation_create" => {
                ensure_plugin(&d, "rh.conversation")?;
                let p = Self::project_mut(&mut d, &a)?;
                if p.conversations.len() >= 128 {
                    return Err("Conversation limit reached".into());
                }
                let title = text(&a, "title", 120)?;
                p.conversations.push(Conversation {
                    id: id(),
                    title,
                    mode: "plan".into(),
                    messages: vec![],
                    active_job: None,
                    draft: String::new(),
                });
            }
            "conversation_draft" => {
                ensure_plugin(&d, "rh.conversation")?;
                let p = Self::project_mut(&mut d, &a)?;
                let c = p
                    .conversations
                    .iter_mut()
                    .find(|c| Some(c.id.as_str()) == a["conversationId"].as_str())
                    .ok_or("Choose a project conversation")?;
                c.draft = a["draft"]
                    .as_str()
                    .filter(|s| s.chars().count() <= 12000 && !s.contains('\0'))
                    .ok_or("Draft exceeds 12000 characters")?
                    .into();
                if let Some(mode) = a["mode"].as_str() {
                    if !["ask", "plan", "execute"].contains(&mode) {
                        return Err("Invalid Agent mode".into());
                    }
                    c.mode = mode.into();
                }
            }
            "conversation_append" | "conversation_complete" => {
                ensure_plugin(&d, "rh.conversation")?;
                let p = Self::project_mut(&mut d, &a)?;
                let c = p
                    .conversations
                    .iter_mut()
                    .find(|c| Some(c.id.as_str()) == a["conversationId"].as_str())
                    .ok_or("Choose a project conversation")?;
                let content = text(&a, "content", 128000)?;
                let role = text(&a, "role", 20)?;
                if operation == "conversation_complete" {
                    let job = text(&a, "jobId", 100)?;
                    if c.messages
                        .iter()
                        .any(|m| m.role == "assistant" && m.job_id.as_deref() == Some(&job))
                    {
                        return Ok(
                            json!({"revision":revision,"projects":d.projects,"disabledPlugins":d.disabled_plugins,"plugins":manifests()}),
                        );
                    }
                    if c.active_job.as_deref() != Some(&job) || role != "assistant" {
                        return Err("Task completion does not match the active conversation".into());
                    }
                } else if role == "user" && a["activeJob"].is_string() && c.active_job.is_some() {
                    return Err("This conversation already has an active Agent".into());
                }
                if !["user", "assistant", "notice"].contains(&role.as_str())
                    || c.messages.len() >= 4096
                {
                    return Err("Invalid conversation message".into());
                }
                c.messages.push(Message {
                    id: id(),
                    role,
                    content,
                    job_id: a["jobId"].as_str().map(str::to_owned),
                });
                c.active_job = a["activeJob"].as_str().map(str::to_owned);
                if let Some(mode) = a["mode"].as_str() {
                    if !["ask", "plan", "execute"].contains(&mode) {
                        return Err("Invalid agent mode".into());
                    }
                    c.mode = mode.to_owned();
                }
            }
            "file_save" => {
                ensure_plugin(&d, "rh.source")?;
                let p = Self::project_mut(&mut d, &a)?;
                let rel = text(&a, "path", 4096)?;
                let path = Self::path(p, &rel)?;
                let original = read(&path, 1024 * 1024)?;
                if a["sha256"].as_str() != Some(&hash(&original)) {
                    return Err("Source changed on disk; refresh and compare before saving".into());
                }
                let content = a["content"]
                    .as_str()
                    .filter(|v| v.len() <= 1024 * 1024 && !v.contains('\0'))
                    .ok_or("Invalid source buffer")?;
                let backup = self.root.join("source-backups").join(&p.id);
                fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
                let backup = backup.join(format!("{}-{}", hash(rel.as_bytes()), hash(&original)));
                if !backup.exists() {
                    atomic(&backup, &original)?;
                }
                let permissions = fs::metadata(&path)
                    .map_err(|e| e.to_string())?
                    .permissions();
                atomic_with_permissions(&path, content.as_bytes(), Some(permissions))?;
            }
            _ => return Err("Unsupported RH Code operation".into()),
        }
        for p in &mut d.projects {
            if Some(p.id.as_str()) == a["projectId"].as_str() {
                p.updated_at = now();
            }
        }
        let bytes = serde_json::to_vec(&d).map_err(|e| e.to_string())?;
        if bytes.len() > LIMIT {
            return Err("RH Code state exceeds 8 MiB; retained previous state".into());
        }
        atomic(&self.root.join("workspace.json"), &bytes)?;
        Ok(
            json!({"revision":hash(&bytes),"projects":d.projects,"disabledPlugins":d.disabled_plugins,"plugins":manifests()}),
        )
    }
}
#[tauri::command]
pub async fn rh_code_call(
    store: State<'_, Arc<Store>>,
    shared: State<'_, Arc<Shared>>,
    operation: String,
    args: Value,
) -> Result<Value, String> {
    if shared.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Launcher is closing".into());
    }
    let owner = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || owner.call(&operation, args))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn rh_code_choose(
    app: tauri::AppHandle,
    shared: State<'_, Arc<Shared>>,
) -> Result<Option<String>, String> {
    if shared.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Launcher is closing".into());
    }
    use tauri_plugin_dialog::DialogExt;
    let result =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?;
    result
        .map(|p| {
            p.into_path()
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|e| e.to_string())
        })
        .transpose()
}

#[cfg(test)]
mod tests;
