//! Native source capture and provenance for generic Harness projects.
//!
//! `capture` freezes bounded text bytes; `bind` persists their native provenance.
//! Only the private host export bridge calls `apply`. Renderer approval selects
//! a digest, never replacement bytes. Every source path is opened relative to a
//! retained directory capability with native no-link/no-mount checks.
//!
//! Linux source application retains its original journal and anonymous-inode
//! publication protocol. Other hosts expose capture and immutable provenance;
//! source application refuses before mutation until its native protocol exists.
#[cfg(target_os = "linux")]
#[path = "harness/filesystem_linux.rs"]
mod filesystem;
#[cfg(not(target_os = "linux"))]
#[path = "harness/filesystem_native.rs"]
mod filesystem;
#[cfg(target_os = "linux")]
#[path = "harness/transaction_linux.rs"]
mod transaction;
#[cfg(not(target_os = "linux"))]
#[path = "harness/transaction_native.rs"]
mod transaction;
use filesystem::Dir;
pub(super) use transaction::{apply, recover, save_assets};

use super::{hash, id, Project};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

const MAX_FILES: usize = 1000;
const MAX_FILE: usize = 1024 * 1024;
const MAX_TOTAL: usize = 16 * 1024 * 1024;
const MAX_RECORD: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 12000;

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}
#[cfg(windows)]
type Identity = pcl_core::retained_fs::Identity;
const BINDING_SCHEMA: u32 = if cfg!(windows) { 2 } else { 1 };
#[derive(Clone, Copy)]
enum NodeKind {
    Directory,
    Regular,
    Other,
}
struct Facts {
    kind: NodeKind,
    size: u64,
    links: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestEntry {
    path: String,
    sha256: String,
    size: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    schema_version: u32,
    job_id: String,
    project_id: String,
    conversation_id: String,
    folder: String,
    root_identity: Identity,
    source_fingerprint: String,
    source_manifest: Vec<ManifestEntry>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileState {
    identity: Identity,
    sha256: String,
    size: usize,
    #[cfg(unix)]
    mode: u32,
    #[cfg(windows)]
    metadata: pcl_core::retained_fs::Metadata,
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn exact_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identifier(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 100 || value.chars().any(char::is_control) {
        return Err("Invalid Harness identity".into());
    }
    Ok(())
}
fn field(value: &Value, key: &str) -> Result<String, String> {
    let value = value[key]
        .as_str()
        .ok_or_else(|| format!("Missing {key}"))?;
    identifier(value)?;
    Ok(value.to_owned())
}
fn excluded(name: &str) -> bool {
    name.starts_with('.')
        || [
            "node_modules",
            "target",
            "build",
            "dist",
            "vendor",
            "venv",
            "__pycache__",
        ]
        .contains(&name)
}
pub(super) fn source_path(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 512
        || value
            .chars()
            .any(|v| v.is_control() || v == '\\' || v == ':')
        || value.split('/').any(|v| v.is_empty() || excluded(v))
    {
        return Err("Invalid or excluded Harness source path".into());
    }
    Ok(())
}
fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}
fn manifest_fingerprint(manifest: &[ManifestEntry]) -> Result<String, String> {
    // Field order matches the host's canonical JSON keys: path, sha256, size.
    Ok(hash(&serde_json::to_vec(manifest).map_err(error)?))
}

fn root(project: &Project) -> Result<Dir, String> {
    let path = Path::new(&project.folder);
    #[cfg(target_os = "linux")]
    if std::fs::canonicalize(path).map_err(error)? != path {
        return Err("Harness project folder changed or contains a link".into());
    }
    Dir::absolute(path, false)
}
fn verify_root(project: &Project, expected: &Identity) -> Result<(), String> {
    if root(project)?.key()? != *expected {
        return Err("Harness project folder identity changed; retained files".into());
    }
    Ok(())
}

pub(super) fn capture(project: &Project) -> Result<Value, String> {
    capture_with_build(project, false)
}

fn build_resource(path: &str) -> bool {
    path == "gradle/wrapper/gradle-wrapper.jar"
        || path
            .split('/')
            .collect::<Vec<_>>()
            .split_last()
            .is_some_and(|(_, parents)| parents.contains(&"src"))
}

/// Binary build inputs are opt-in and never become readable source context.
/// Capture them in this same pinned walk, so later build staging cannot pick up
/// fresh user files or follow a different project directory between snapshots.
pub(super) fn capture_with_build(project: &Project, include_build: bool) -> Result<Value, String> {
    let root = root(project)?;
    let root_identity = root.key()?;
    let mut pending = vec![(String::new(), 0usize)];
    let mut source_files = vec![];
    let mut manifest = vec![];
    let mut omissions = vec![];
    let mut total = 0usize;
    let mut build_files = vec![];
    let mut build_bytes = 0usize;
    let mut scanned = 0usize;
    while let Some((path, depth)) = pending.pop() {
        if depth > 12 {
            return Err("Harness tree exceeds supported depth; choose a narrower folder".into());
        }
        let directory = root.child(&path)?;
        for name in directory.names()? {
            scanned += 1;
            if scanned > MAX_ENTRIES {
                return Err("Harness tree exceeds its inventory budget".into());
            }
            if excluded(&name) {
                continue;
            }
            let path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            source_path(&path)?;
            let stat = directory
                .facts(&name)?
                .ok_or("Harness tree changed during capture")?;
            match stat.kind {
                NodeKind::Directory => pending.push((path, depth + 1)),
                NodeKind::Regular => {
                    let reason = if stat.size > MAX_FILE as u64 {
                        Some("fileTooLarge")
                    } else if stat.links != 1 {
                        Some("multipleLinks")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        if include_build && build_resource(&path) {
                            return Err(format!("Build input cannot be captured ({reason}): {path}. Use a regular file up to 1 MiB."));
                        }
                        omissions.push(json!({"path":path,"reason":reason}));
                        continue;
                    }
                    let (state, bytes) = directory.read(&name, MAX_FILE)?;
                    if std::str::from_utf8(&bytes).is_err() || bytes.contains(&0) {
                        if include_build && build_resource(&path) {
                            build_bytes += bytes.len();
                            if build_files.len() >= MAX_FILES || build_bytes > 4 * 1024 * 1024 {
                                return Err("Build resources exceed 1000 files or 4 MiB; choose a narrower project".into());
                            }
                            build_files.push(json!({"path":path,"sha256":state.sha256,"size":state.size,"base64":STANDARD.encode(&bytes)}));
                        }
                        omissions.push(json!({"path":path,"reason":"notText"}));
                        continue;
                    }
                    total += bytes.len();
                    if total > MAX_TOTAL || manifest.len() >= MAX_FILES {
                        return Err("Harness source exceeds 1000 text files or 16 MiB; choose a narrower folder".into());
                    }
                    manifest.push(ManifestEntry {
                        path: path.clone(),
                        sha256: state.sha256.clone(),
                        size: state.size,
                    });
                    source_files.push(json!({"path":path,"sha256":state.sha256,"size":state.size,"data":STANDARD.encode(bytes)}));
                }
                _ => {
                    if include_build
                        && (build_resource(&path)
                            || path == "src"
                            || path == "gradle"
                            || path == "gradle/wrapper"
                            || path.ends_with("/src"))
                    {
                        return Err(format!(
                            "Build input is not a regular file or directory: {path}"
                        ));
                    }
                    omissions.push(json!({"path":path,"reason":"notRegular"}));
                }
            }
        }
    }
    manifest.sort_by(|a, b| a.path.cmp(&b.path));
    source_files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    omissions.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    build_files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    verify_root(project, &root_identity)?;
    let mut captured = json!({"sourceFiles":source_files,"sourceManifest":manifest,"sourceFingerprint":manifest_fingerprint(&manifest)?,"rootIdentity":root_identity,"sourceOmissionCount":omissions.len(),"sourceOmissions":omissions.into_iter().take(32).collect::<Vec<_>>()});
    if include_build {
        captured["buildFiles"] = json!(build_files);
    }
    Ok(captured)
}

fn store(store_root: &Path) -> Result<Dir, String> {
    Dir::absolute(store_root, true)
}
fn binding_name(job_id: &str) -> String {
    format!("{}.json", hash(job_id.as_bytes()))
}
fn parse_binding(value: Binding) -> Result<Binding, String> {
    identifier(&value.job_id)?;
    identifier(&value.project_id)?;
    identifier(&value.conversation_id)?;
    let mut previous = None;
    let mut total = 0;
    for entry in &value.source_manifest {
        source_path(&entry.path)?;
        if !exact_hash(&entry.sha256)
            || entry.size > MAX_FILE
            || previous.is_some_and(|p: &str| p >= entry.path.as_str())
        {
            return Err("Invalid native Harness manifest; retained binding".into());
        }
        previous = Some(entry.path.as_str());
        total += entry.size;
    }
    if value.schema_version != BINDING_SCHEMA
        || !Path::new(&value.folder).is_absolute()
        || value.source_manifest.len() > MAX_FILES
        || total > MAX_TOTAL
        || manifest_fingerprint(&value.source_manifest)? != value.source_fingerprint
    {
        return Err("Invalid native Harness provenance; retained binding".into());
    }
    #[cfg(windows)]
    if !matches!(&value.root_identity, Identity::Windows { file, .. } if *file != [0; 16]) {
        return Err("Invalid native Harness platform identity; retained binding".into());
    }
    Ok(value)
}
pub(super) fn bind(
    store_root: &Path,
    project: &Project,
    job_id: &str,
    conversation_id: &str,
    captured: &Value,
) -> Result<(), String> {
    identifier(job_id)?;
    identifier(conversation_id)?;
    let binding = parse_binding(Binding {
        schema_version: BINDING_SCHEMA,
        job_id: job_id.into(),
        project_id: project.id.clone(),
        conversation_id: conversation_id.into(),
        folder: project.folder.clone(),
        root_identity: serde_json::from_value(captured["rootIdentity"].clone()).map_err(error)?,
        source_fingerprint: field(captured, "sourceFingerprint")?,
        source_manifest: serde_json::from_value(captured["sourceManifest"].clone())
            .map_err(error)?,
    })?;
    verify_root(project, &binding.root_identity)?;
    let bindings = store(store_root)?.ensure_child("harness-bindings")?;
    bindings.immutable(
        &binding_name(job_id),
        &serde_json::to_vec(&binding).map_err(error)?,
    )
}
fn load_binding(store: &Dir, job_id: &str) -> Result<Binding, String> {
    let bytes = store
        .child("harness-bindings")?
        .read_binding(&binding_name(job_id))?;
    parse_binding(
        serde_json::from_slice(&bytes)
            .map_err(|_| "Native Harness binding is unreadable; retained for recovery")?,
    )
}

#[cfg(test)]
#[path = "harness/capture_tests.rs"]
mod capture_tests;

// Compile and exercise the common retained native read adapter on Linux too;
// the Windows/macOS jobs still exercise their own actual filesystem primitives.
#[cfg(all(test, target_os = "linux"))]
#[path = "harness/filesystem_native.rs"]
mod native_read_adapter;
