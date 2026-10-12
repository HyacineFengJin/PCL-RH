//! Native boundary for generic Harness projects on the current Linux carrier.
//!
//! `capture` freezes bounded text bytes; `bind` persists their native provenance.
//! Only the private host export bridge calls `apply`. Renderer approval selects
//! a digest, never replacement bytes. Every source path is opened relative to a
//! pinned directory FD with openat2's no-link/no-mount checks.
//!
//! A batch first checks all changes, saves backups, and journals anonymous staged
//! inode identities before linking them into project directories. Publication is
//! atomic per file, not per batch. Original inodes stay in named temporary slots
//! until the durable commit. Recovery checks the whole batch before reversing
//! any publication; an external edit blocks recovery and keeps every backup.
use super::{hash, id, Project};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

const MAX_FILES: usize = 1000;
const MAX_FILE: usize = 1024 * 1024;
const MAX_TOTAL: usize = 16 * 1024 * 1024;
const MAX_RECORD: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 12000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
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
    mode: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    path: String,
    temporary: String,
    before: Option<FileState>,
    after_sha256: Option<String>,
    after_size: usize,
    staged: Option<FileState>,
    parent_identity: Option<Identity>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreatedDirectory {
    path: String,
    identity: Identity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    binding: Binding,
    proposal_id: String,
    digest: String,
    export_hash: String,
    phase: String,
    cleanup_complete: bool,
    entries: Vec<Entry>,
    created_directories: Vec<CreatedDirectory>,
}
struct Change {
    entry: Entry,
    before_bytes: Option<Vec<u8>>,
    after_bytes: Option<Vec<u8>>,
}
struct Dir(File);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Publication {
    Untouched,
    Published,
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn io_error(context: &str) -> String {
    format!("{context}: {}", std::io::Error::last_os_error())
}
fn c(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(error)
}
fn identity(file: &File) -> Result<Identity, String> {
    let m = file.metadata().map_err(error)?;
    Ok(Identity {
        device: m.dev(),
        inode: m.ino(),
    })
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
fn source_path(value: &str) -> Result<(), String> {
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

impl Dir {
    fn absolute(path: &Path, create: bool) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("Harness requires an absolute native folder".into());
        }
        let slash = c("/")?;
        let fd = unsafe {
            libc::open(
                slash.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io_error("Cannot open native root"));
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let name = CString::new(name.as_bytes()).map_err(error)?;
                    if create
                        && unsafe { libc::mkdirat(dir.0.as_raw_fd(), name.as_ptr(), 0o700) } != 0
                        && std::io::Error::last_os_error().kind()
                            != std::io::ErrorKind::AlreadyExists
                    {
                        return Err(io_error("Cannot create native store directory"));
                    }
                    let fd = unsafe {
                        libc::openat(
                            dir.0.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                        )
                    };
                    if fd < 0 {
                        return Err(io_error("Native folder contains a link or changed"));
                    }
                    dir = Self(unsafe { File::from_raw_fd(fd) });
                }
                _ => return Err("Native folder contains relative path components".into()),
            }
        }
        Ok(dir)
    }
    fn key(&self) -> Result<Identity, String> {
        identity(&self.0)
    }
    fn sync(&self) -> Result<(), String> {
        self.0.sync_all().map_err(error)
    }
    fn open(&self, path: &str, directory: bool) -> Result<File, String> {
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let name = c(if path.is_empty() { "." } else { path })?;
        let how = OpenHow {
            flags: (libc::O_RDONLY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | if directory {
                    libc::O_DIRECTORY
                } else {
                    libc::O_NONBLOCK
                }) as u64,
            mode: 0,
            // NO_XDEV | NO_MAGICLINKS | NO_SYMLINKS | BENEATH.
            resolve: 0x01 | 0x02 | 0x04 | 0x08,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.0.as_raw_fd(),
                name.as_ptr(),
                &how,
                std::mem::size_of::<OpenHow>(),
            ) as i32
        };
        if fd < 0 {
            return Err(io_error(
                "Harness path changed, contains a link, or crosses a mount (Linux 5.6+ required)",
            ));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn child(&self, path: &str) -> Result<Self, String> {
        self.open(path, true).map(Self)
    }
    fn stat(&self, name: &str) -> Result<Option<libc::stat>, String> {
        let name = c(name)?;
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
            return Ok(Some(unsafe { stat.assume_init() }));
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(io_error("Cannot inspect Harness path"))
        }
    }
    fn optional_child(&self, name: &str) -> Result<Option<Self>, String> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    fn ensure_child(&self, name: &str) -> Result<Self, String> {
        let name_c = c(name)?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(io_error("Cannot create Harness store directory"));
        }
        self.sync()?;
        self.child(name)
    }
    fn names(&self) -> Result<Vec<String>, String> {
        let mut names = vec![];
        // Enumeration uses the pinned FD, not a path supplied by the renderer.
        for entry in fs::read_dir(format!("/proc/self/fd/{}", self.0.as_raw_fd())).map_err(error)? {
            let name = entry
                .map_err(error)?
                .file_name()
                .into_string()
                .map_err(|_| "Harness tree contains a non-UTF-8 filename")?;
            names.push(name);
            if names.len() > MAX_ENTRIES {
                return Err("Harness directory exceeds its entry budget".into());
            }
        }
        names.sort();
        Ok(names)
    }
    fn read(&self, name: &str, limit: usize) -> Result<(FileState, Vec<u8>), String> {
        let mut file = self.open(name, false)?;
        let before = file.metadata().map_err(error)?;
        if !before.is_file() || before.nlink() != 1 || before.len() > limit as u64 {
            return Err("Harness requires a bounded regular file with one link".into());
        }
        let mut bytes = vec![];
        (&mut file)
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        let after = file.metadata().map_err(error)?;
        if bytes.len() > limit
            || before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || after.nlink() != 1
        {
            return Err("Harness file changed while it was read".into());
        }
        Ok((
            FileState {
                identity: identity(&file)?,
                sha256: hash(&bytes),
                size: bytes.len(),
                mode: after.mode() & 0o777,
            },
            bytes,
        ))
    }
    fn optional_file(&self, name: &str) -> Result<Option<FileState>, String> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.read(name, MAX_FILE).map(|v| Some(v.0))
        }
    }
    fn anonymous(&self, bytes: &[u8], mode: u32) -> Result<File, String> {
        let dot = c(".")?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                dot.as_ptr(),
                libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io_error(
                "Harness staging needs Linux anonymous-file support",
            ));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(error)?;
        if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(io_error("Cannot preserve source permissions"));
        }
        file.sync_all().map_err(error)?;
        Ok(file)
    }
    fn link_anonymous(&self, file: &File, name: &str) -> Result<(), String> {
        let empty = c("")?;
        let name = c(name)?;
        if unsafe {
            libc::linkat(
                file.as_raw_fd(),
                empty.as_ptr(),
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        } != 0
        {
            return Err(io_error("Harness staging slot already exists or changed"));
        }
        self.sync()
    }
    fn immutable(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if self.stat(name)?.is_some() {
            if self.read(name, MAX_RECORD.max(MAX_FILE))?.1 == bytes {
                return Ok(());
            }
            return Err("Immutable Harness record differs; retained for recovery".into());
        }
        let file = self.anonymous(bytes, 0o600)?;
        self.link_anonymous(&file, name)
    }
    fn replace_record(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_RECORD {
            return Err("Harness journal exceeds its limit".into());
        }
        if self.stat(name)?.is_some() {
            self.read(name, MAX_RECORD)?;
        }
        let temporary = format!(".journal-{}", id());
        let file = self.anonymous(bytes, 0o600)?;
        self.link_anonymous(&file, &temporary)?;
        let temporary_c = c(&temporary)?;
        let name_c = c(name)?;
        if unsafe {
            libc::renameat(
                self.0.as_raw_fd(),
                temporary_c.as_ptr(),
                self.0.as_raw_fd(),
                name_c.as_ptr(),
            )
        } != 0
        {
            return Err(io_error("Cannot save Harness journal"));
        }
        self.sync()
    }
    fn rename(&self, from: &str, to: &str, flags: u32) -> Result<(), String> {
        let from = c(from)?;
        let to = c(to)?;
        if unsafe {
            libc::renameat2(
                self.0.as_raw_fd(),
                from.as_ptr(),
                self.0.as_raw_fd(),
                to.as_ptr(),
                flags,
            )
        } != 0
        {
            return Err(io_error(
                "Harness publication conflict; retained transaction",
            ));
        }
        self.sync()
    }
    fn unlink(&self, name: &str, expected: &FileState) -> Result<(), String> {
        if self.optional_file(name)?.as_ref() != Some(expected) {
            return Err("Harness temporary file changed; retained for recovery".into());
        }
        let name = c(name)?;
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io_error("Cannot remove owned Harness temporary"));
        }
        self.sync()
    }
}

fn root(project: &Project) -> Result<Dir, String> {
    let path = Path::new(&project.folder);
    if fs::canonicalize(path).map_err(error)? != path {
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
    let root = root(project)?;
    let root_identity = root.key()?;
    let mut pending = vec![(String::new(), 0usize)];
    let mut source_files = vec![];
    let mut manifest = vec![];
    let mut omissions = vec![];
    let mut total = 0usize;
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
                .stat(&name)?
                .ok_or("Harness tree changed during capture")?;
            match stat.st_mode & libc::S_IFMT {
                libc::S_IFDIR => pending.push((path, depth + 1)),
                libc::S_IFREG => {
                    let reason = if stat.st_size > MAX_FILE as i64 {
                        Some("fileTooLarge")
                    } else if stat.st_nlink != 1 {
                        Some("multipleLinks")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        omissions.push(json!({"path":path,"reason":reason}));
                        continue;
                    }
                    let (state, bytes) = directory.read(&name, MAX_FILE)?;
                    if std::str::from_utf8(&bytes).is_err() || bytes.contains(&0) {
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
                _ => omissions.push(json!({"path":path,"reason":"notRegular"})),
            }
        }
    }
    manifest.sort_by(|a, b| a.path.cmp(&b.path));
    source_files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    omissions.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    verify_root(project, &root_identity)?;
    Ok(
        json!({"sourceFiles":source_files,"sourceManifest":manifest,"sourceFingerprint":manifest_fingerprint(&manifest)?,"rootIdentity":root_identity,"sourceOmissionCount":omissions.len(),"sourceOmissions":omissions.into_iter().take(32).collect::<Vec<_>>()}),
    )
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
    if value.schema_version != 1
        || !Path::new(&value.folder).is_absolute()
        || value.source_manifest.len() > MAX_FILES
        || total > MAX_TOTAL
        || manifest_fingerprint(&value.source_manifest)? != value.source_fingerprint
    {
        return Err("Invalid native Harness provenance; retained binding".into());
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
        schema_version: 1,
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
        .read(&binding_name(job_id), MAX_RECORD)?
        .1;
    parse_binding(
        serde_json::from_slice(&bytes)
            .map_err(|_| "Native Harness binding is unreadable; retained for recovery")?,
    )
}
fn lock(store: &Dir) -> Result<File, String> {
    let name = c(".harness.lock")?;
    let fd = unsafe {
        libc::openat(
            store.0.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io_error("Cannot open Harness writer lease"));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err("Invalid Harness writer lease".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Another native Harness transaction is active".into());
    }
    Ok(file)
}

fn prospective_parent(root: &Dir, path: &str) -> Result<Option<Dir>, String> {
    let mut directory = root.child("")?;
    if path.is_empty() {
        return Ok(Some(directory));
    }
    for part in path.split('/') {
        let Some(child) = directory.optional_child(part)? else {
            return Ok(None);
        };
        directory = child;
    }
    Ok(Some(directory))
}
fn preflight(root: &Dir, binding: &Binding, export: &Value) -> Result<Vec<Change>, String> {
    let files = export["files"]
        .as_array()
        .filter(|f| !f.is_empty() && f.len() <= MAX_FILES)
        .ok_or("Invalid Harness proposal inventory")?;
    let originals: BTreeMap<_, _> = binding
        .source_manifest
        .iter()
        .map(|v| (v.path.as_str(), v))
        .collect();
    let mut paths = HashSet::new();
    let mut changes = vec![];
    let mut total = 0usize;
    let mut result_bytes: usize = binding.source_manifest.iter().map(|entry| entry.size).sum();
    let mut result_files = binding.source_manifest.len();
    for file in files {
        let path = file["path"]
            .as_str()
            .ok_or("Missing Harness proposal path")?;
        source_path(path)?;
        if !paths.insert(path.to_owned()) {
            return Err("Duplicate Harness proposal path".into());
        }
        let expected = match file
            .get("expectedSha256")
            .ok_or("Missing Harness original hash")?
        {
            Value::Null => None,
            Value::String(v) if exact_hash(v) => Some(v.as_str()),
            _ => return Err("Invalid Harness original hash".into()),
        };
        if originals.get(path).map(|v| v.sha256.as_str()) != expected {
            return Err("Proposal original hash differs from the native frozen manifest".into());
        }
        let deleted = file["delete"]
            .as_bool()
            .ok_or("Missing Harness deletion flag")?;
        let data = file.get("data").ok_or("Missing Harness proposal bytes")?;
        let after_bytes = if deleted {
            if expected.is_none() || !data.is_null() {
                return Err("Invalid Harness deletion".into());
            }
            None
        } else {
            let encoded = data
                .as_str()
                .filter(|v| v.len() <= MAX_FILE.div_ceil(3) * 4)
                .ok_or("Invalid Harness replacement bytes")?;
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| "Invalid Harness base64")?;
            total += bytes.len();
            if bytes.len() > MAX_FILE
                || total > MAX_TOTAL
                || std::str::from_utf8(&bytes).is_err()
                || bytes.contains(&0)
            {
                return Err("Harness proposal exceeds the text budget".into());
            }
            Some(bytes)
        };
        if let Some(original) = originals.get(path) {
            result_bytes -= original.size;
            result_files -= 1;
        }
        if let Some(bytes) = &after_bytes {
            result_bytes += bytes.len();
            result_files += 1;
        }
        let (parent_path, name) = split_path(path);
        let parent = prospective_parent(root, parent_path)?;
        let before = match &parent {
            Some(dir) if dir.stat(name)?.is_some() => Some(dir.read(name, MAX_FILE)?),
            _ => None,
        };
        if before.as_ref().map(|v| v.0.sha256.as_str()) != expected {
            return Err(format!("Harness source changed: {path}"));
        }
        let entry = Entry {
            path: path.into(),
            temporary: format!(".rh-harness-{}", id()),
            before: before.as_ref().map(|v| v.0.clone()),
            after_sha256: after_bytes.as_ref().map(|v| hash(v)),
            after_size: after_bytes.as_ref().map_or(0, Vec::len),
            staged: None,
            parent_identity: parent.as_ref().map(Dir::key).transpose()?,
        };
        changes.push(Change {
            entry,
            before_bytes: before.map(|v| v.1),
            after_bytes,
        });
    }
    for path in &paths {
        let mut parent = split_path(path).0;
        while !parent.is_empty() {
            if paths.contains(parent) {
                return Err("Harness file path conflicts with a proposal directory".into());
            }
            parent = split_path(parent).0;
        }
    }
    if result_bytes > MAX_TOTAL || result_files > MAX_FILES {
        return Err("Applied Harness source would exceed its snapshot budget".into());
    }
    Ok(changes)
}
fn write_journal(transaction: &Dir, journal: &Journal) -> Result<(), String> {
    transaction.replace_record("journal.json", &serde_json::to_vec(journal).map_err(error)?)
}
fn read_journal(transaction: &Dir) -> Result<Journal, String> {
    let journal: Journal = serde_json::from_slice(&transaction.read("journal.json", MAX_RECORD)?.1)
        .map_err(|_| "Harness journal is unreadable; retained for recovery")?;
    parse_binding(journal.binding.clone())?;
    if journal.schema_version != 1
        || journal.entries.is_empty()
        || journal.entries.len() > MAX_FILES
        || !["preparing", "publishing", "committed", "rolledBack"].contains(&journal.phase.as_str())
        || !exact_hash(&journal.digest)
        || !exact_hash(&journal.export_hash)
        || journal.cleanup_complete && ["preparing", "publishing"].contains(&journal.phase.as_str())
        || journal.created_directories.len() > MAX_FILES * 12
        || journal.proposal_id.len() != 32
        || !journal
            .proposal_id
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err("Invalid Harness transaction; retained for recovery".into());
    }
    let mut paths = HashSet::new();
    let mut after_bytes = 0usize;
    let originals: BTreeMap<_, _> = journal
        .binding
        .source_manifest
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    for entry in &journal.entries {
        after_bytes += entry.after_size;
        source_path(&entry.path)?;
        if !paths.insert(&entry.path)
            || !entry.temporary.starts_with(".rh-harness-")
            || entry.temporary.len() != 36
            || !entry.temporary[12..].bytes().all(|v| v.is_ascii_hexdigit())
            || entry.after_size > MAX_FILE
            || entry.after_sha256.as_ref().is_some_and(|v| !exact_hash(v))
            || entry.after_sha256.is_none() && entry.after_size != 0
            || entry.before.is_none() && entry.after_sha256.is_none()
            || entry
                .before
                .as_ref()
                .map(|state| (state.sha256.as_str(), state.size))
                != originals
                    .get(entry.path.as_str())
                    .map(|entry| (entry.sha256.as_str(), entry.size))
        {
            return Err("Invalid Harness journal file; retained for recovery".into());
        }
        for state in [entry.before.as_ref(), entry.staged.as_ref()]
            .into_iter()
            .flatten()
        {
            if !exact_hash(&state.sha256) || state.size > MAX_FILE || state.mode > 0o777 {
                return Err("Invalid Harness journal inode; retained for recovery".into());
            }
        }
        if let Some(staged) = &entry.staged {
            if Some(&staged.sha256) != entry.after_sha256.as_ref()
                || staged.size != entry.after_size
            {
                return Err("Harness staged bytes differ from the journal".into());
            }
        }
    }
    if after_bytes > MAX_TOTAL {
        return Err("Harness journal exceeds its source byte budget".into());
    }
    for directory in &journal.created_directories {
        source_path(&directory.path)?;
    }
    Ok(journal)
}

fn ensure_parent(
    root: &Dir,
    parent: &str,
    transaction: &Dir,
    journal: &mut Journal,
) -> Result<Dir, String> {
    let mut dir = root.child("")?;
    let mut path = String::new();
    if parent.is_empty() {
        return Ok(dir);
    }
    for name in parent.split('/') {
        path = if path.is_empty() {
            name.into()
        } else {
            format!("{path}/{name}")
        };
        dir = match dir.optional_child(name)? {
            Some(child) => child,
            None => {
                let child = dir.ensure_child(name)?;
                journal.created_directories.push(CreatedDirectory {
                    path: path.clone(),
                    identity: child.key()?,
                });
                write_journal(transaction, journal)?;
                child
            }
        };
    }
    Ok(dir)
}
fn entry_parent(root: &Dir, entry: &Entry) -> Result<Dir, String> {
    let parent = root.child(split_path(&entry.path).0)?;
    if entry.parent_identity.as_ref() != Some(&parent.key()?) {
        return Err(format!("Harness parent folder changed: {}", entry.path));
    }
    Ok(parent)
}
fn state(root: &Dir, entry: &Entry) -> Result<Publication, String> {
    let parent_path = split_path(&entry.path).0;
    let parent = match prospective_parent(root, parent_path)? {
        Some(parent) => parent,
        None if entry.parent_identity.is_none()
            && entry.before.is_none()
            && entry.staged.is_none() =>
        {
            return Ok(Publication::Untouched)
        }
        None => return Err(format!("Harness parent folder disappeared: {}", entry.path)),
    };
    if entry
        .parent_identity
        .as_ref()
        .is_some_and(|v| parent.key().ok().as_ref() != Some(v))
    {
        return Err("Harness parent identity changed; retained transaction".into());
    }
    let target = parent.optional_file(split_path(&entry.path).1)?;
    let temporary = parent.optional_file(&entry.temporary)?;
    if target == entry.before && (temporary.is_none() || temporary == entry.staged) {
        return Ok(Publication::Untouched);
    }
    if entry.staged.is_some() && target == entry.staged && temporary == entry.before {
        return Ok(Publication::Published);
    }
    if entry.after_sha256.is_none() && target.is_none() && temporary == entry.before {
        return Ok(Publication::Published);
    }
    Err(format!(
        "Harness transaction conflicts with an external edit: {}; backups retained",
        entry.path
    ))
}

fn publish(project: &Project, root: &Dir, entry: &Entry) -> Result<(), String> {
    verify_root(project, &root.key()?)?;
    if state(root, entry)? != Publication::Untouched {
        return Err("Harness file was already published unexpectedly".into());
    }
    let parent = entry_parent(root, entry)?;
    let name = split_path(&entry.path).1;
    match (entry.before.is_some(), entry.after_sha256.is_some()) {
        (true, true) => parent.rename(&entry.temporary, name, libc::RENAME_EXCHANGE)?,
        (true, false) => parent.rename(name, &entry.temporary, libc::RENAME_NOREPLACE)?,
        (false, true) => parent.rename(&entry.temporary, name, libc::RENAME_NOREPLACE)?,
        _ => return Err("Invalid Harness publication".into()),
    }
    // The displaced inode is checked after the atomic operation too. A racing
    // replacement stays in the transaction slot, never silently discarded.
    if state(root, entry)? != Publication::Published {
        return Err("Harness publication changed concurrently; retained transaction".into());
    }
    verify_root(project, &root.key()?)
}
fn clean_temporaries(root: &Dir, journal: &Journal) -> Result<(), String> {
    for entry in &journal.entries {
        let Some(parent) = prospective_parent(root, split_path(&entry.path).0)? else {
            continue;
        };
        if parent.stat(&entry.temporary)?.is_none() {
            continue;
        }
        let expected = if journal.phase == "committed" {
            entry.before.as_ref()
        } else {
            entry.staged.as_ref()
        }
        .ok_or("Unregistered Harness temporary; retained for recovery")?;
        if entry.parent_identity.as_ref() != Some(&parent.key()?) {
            return Err("Harness temporary parent changed; retained file".into());
        }
        parent.unlink(&entry.temporary, expected)?;
    }
    Ok(())
}
fn rollback(
    project: &Project,
    root: &Dir,
    transaction: &Dir,
    journal: &mut Journal,
) -> Result<(), String> {
    verify_root(project, &journal.binding.root_identity)?;
    // Do not start partial restoration when another file already conflicts.
    let states = journal
        .entries
        .iter()
        .map(|entry| state(root, entry))
        .collect::<Result<Vec<_>, _>>()?;
    for (index, entry) in journal.entries.iter().enumerate() {
        if let Some(before) = &entry.before {
            let backup_name = format!("before-{index}");
            // A crash during backup preparation has not published source.
            // Only displaced originals require a backup to permit reversal.
            if transaction.stat(&backup_name)?.is_none() && states[index] == Publication::Untouched
            {
                continue;
            }
            let backup = transaction.read(&backup_name, MAX_FILE)?.0;
            if backup.sha256 != before.sha256 || backup.size != before.size {
                return Err("Harness backup changed; retained transaction".into());
            }
        }
    }
    for (entry, publication) in journal.entries.iter().zip(states).rev() {
        if publication == Publication::Untouched {
            continue;
        }
        verify_root(project, &journal.binding.root_identity)?;
        if state(root, entry)? != Publication::Published {
            return Err("Harness file changed during recovery; retained transaction".into());
        }
        let parent = entry_parent(root, entry)?;
        let name = split_path(&entry.path).1;
        match (entry.before.is_some(), entry.after_sha256.is_some()) {
            (true, true) => parent.rename(&entry.temporary, name, libc::RENAME_EXCHANGE)?,
            (true, false) => parent.rename(&entry.temporary, name, libc::RENAME_NOREPLACE)?,
            (false, true) => parent.rename(name, &entry.temporary, libc::RENAME_NOREPLACE)?,
            _ => return Err("Invalid Harness rollback".into()),
        }
        if state(root, entry)? != Publication::Untouched {
            return Err("Harness restoration changed concurrently; retained transaction".into());
        }
    }
    journal.phase = "rolledBack".into();
    write_journal(transaction, journal)?;
    finish_cleanup(root, transaction, journal)
}
fn finish_cleanup(root: &Dir, transaction: &Dir, journal: &mut Journal) -> Result<(), String> {
    clean_temporaries(root, journal)?;
    if journal.phase == "rolledBack" {
        for created in journal.created_directories.iter().rev() {
            let (parent_path, name) = split_path(&created.path);
            let Some(parent) = prospective_parent(root, parent_path)? else {
                continue;
            };
            let Some(directory) = parent.optional_child(name)? else {
                continue;
            };
            if directory.key()? != created.identity {
                return Err("Harness created directory changed; retained folder".into());
            }
            let name = c(name)?;
            if unsafe { libc::unlinkat(parent.0.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) }
                != 0
            {
                // A user may have added files after preparation. Keep them and
                // their containing directory; no recursive cleanup is allowed.
                let errno = std::io::Error::last_os_error().raw_os_error();
                if errno != Some(libc::ENOTEMPTY) && errno != Some(libc::EEXIST) {
                    return Err(io_error("Cannot clean Harness directory"));
                }
            }
            parent.sync()?;
        }
    }
    journal.cleanup_complete = true;
    write_journal(transaction, journal)
}
fn recovery_locked(store: &Dir, project: &Project) -> Result<Value, String> {
    let transactions = store.ensure_child("harness-transactions")?;
    let mut recovered = 0usize;
    let mut retained_orphans = 0usize;
    let mut pending = 0usize;
    for name in transactions.names()? {
        if !exact_hash(&name) {
            return Err("Unexpected native Harness transaction path; retained store".into());
        }
        let transaction = transactions.child(&name)?;
        if transaction.stat("journal.json")?.is_none() {
            retained_orphans += 1;
            continue;
        }
        let mut journal = read_journal(&transaction)?;
        if journal.binding != load_binding(store, &journal.binding.job_id)?
            || name
                != hash(format!("{}:{}", journal.binding.job_id, journal.proposal_id).as_bytes())
        {
            return Err(
                "Harness journal differs from native job provenance; retained transaction".into(),
            );
        }
        if journal.binding.project_id != project.id {
            continue;
        }
        if journal.cleanup_complete && ["committed", "rolledBack"].contains(&journal.phase.as_str())
        {
            continue;
        }
        if journal.binding.folder != project.folder {
            return Err(
                "Harness recovery belongs to the original project folder; backups retained".into(),
            );
        }
        let root = root(project)?;
        verify_root(project, &journal.binding.root_identity)?;
        if ["preparing", "publishing"].contains(&journal.phase.as_str()) {
            pending += 1;
            rollback(project, &root, &transaction, &mut journal)
                .map_err(|e| format!("Harness recovery required: {e}"))?;
            recovered += 1;
        } else {
            finish_cleanup(&root, &transaction, &mut journal)?;
        }
    }
    Ok(
        json!({"status":"ready","recovered":recovered,"pending":0,"interrupted":pending,"retainedOrphans":retained_orphans,"backupAvailable":recovered>0}),
    )
}
pub(super) fn recover(store_root: &Path, project: &Project) -> Result<Value, String> {
    let store = store(store_root)?;
    let _lease = lock(&store)?;
    recovery_locked(&store, project)
}

pub(super) fn apply(
    store_root: &Path,
    project: &Project,
    args: &Value,
    export: &Value,
) -> Result<Value, String> {
    let job_id = field(args, "jobId")?;
    let proposal_id = field(args, "proposalId")?;
    let digest = field(args, "expectedDigest")?;
    let conversation_id = field(args, "conversationId")?;
    if proposal_id.len() != 32
        || !proposal_id
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        || !exact_hash(&digest)
        || args
            .get("projectId")
            .is_some_and(|v| v.as_str() != Some(&project.id))
    {
        return Err("Invalid Harness review approval".into());
    }
    let store = store(store_root)?;
    let _lease = lock(&store)?;
    let binding = load_binding(&store, &job_id)?;
    if binding.project_id != project.id
        || binding.conversation_id != conversation_id
        || binding.folder != project.folder
        || export["jobId"].as_str() != Some(&job_id)
        || export["proposalId"].as_str() != Some(&proposal_id)
        || export["digest"].as_str() != Some(&digest)
        || export["projectId"].as_str() != Some(&binding.project_id)
        || export["conversationId"].as_str() != Some(&binding.conversation_id)
        || export["sourceFingerprint"].as_str() != Some(&binding.source_fingerprint)
    {
        return Err("Harness export differs from the native job and reviewed digest".into());
    }
    verify_root(project, &binding.root_identity)?;
    let export_hash = hash(&serde_json::to_vec(export).map_err(error)?);
    let transaction_name = hash(format!("{job_id}:{proposal_id}").as_bytes());
    let transactions = store.ensure_child("harness-transactions")?;
    if let Some(transaction) = transactions.optional_child(&transaction_name)? {
        let mut previous = read_journal(&transaction)?;
        if previous.binding != binding
            || previous.proposal_id != proposal_id
            || previous.digest != digest
            || previous.export_hash != export_hash
        {
            return Err(
                "Harness proposal identity was reused with different bytes; retained transaction"
                    .into(),
            );
        }
        if previous.phase == "committed" {
            let root = root(project)?;
            if !previous.cleanup_complete {
                finish_cleanup(&root, &transaction, &mut previous)?;
            }
            return Ok(
                json!({"status":"alreadyApplied","proposalId":proposal_id,"digest":digest,"backupAvailable":true}),
            );
        }
        recovery_locked(&store, project)?;
        let recovered = read_journal(&transaction)?;
        if recovered.phase != "rolledBack" || !recovered.cleanup_complete {
            return Err("Harness proposal still needs native recovery; backups retained".into());
        }
        // A clean rollback restores the exact frozen source. A fresh explicit
        // click may retry this same digest; the preflight below still verifies
        // the complete manifest and records the current original inodes.
    }
    recovery_locked(&store, project)?;
    let captured = capture(project)?;
    if captured["sourceFingerprint"].as_str() != Some(&binding.source_fingerprint) {
        return Err("Harness project source changed since this job; review a new snapshot".into());
    }
    let root = root(project)?;
    verify_root(project, &binding.root_identity)?;
    let changes = preflight(&root, &binding, export)?;
    let mut journal = Journal {
        schema_version: 1,
        binding,
        proposal_id: proposal_id.clone(),
        digest: digest.clone(),
        export_hash,
        phase: "preparing".into(),
        cleanup_complete: false,
        entries: changes.iter().map(|v| v.entry.clone()).collect(),
        created_directories: vec![],
    };
    let transaction = transactions.ensure_child(&transaction_name)?;
    write_journal(&transaction, &journal)?;
    let result = (|| {
        for (index, change) in changes.iter().enumerate() {
            if let Some(bytes) = &change.before_bytes {
                transaction.immutable(&format!("before-{index}"), bytes)?;
            }
        }
        for (index, change) in changes.iter().enumerate() {
            verify_root(project, &journal.binding.root_identity)?;
            let parent_path = split_path(&change.entry.path).0;
            let parent = ensure_parent(&root, parent_path, &transaction, &mut journal)?;
            if change
                .entry
                .parent_identity
                .as_ref()
                .is_some_and(|v| parent.key().ok().as_ref() != Some(v))
            {
                return Err("Harness source parent changed during preparation".into());
            }
            journal.entries[index].parent_identity = Some(parent.key()?);
            if let Some(bytes) = &change.after_bytes {
                let mode = change.entry.before.as_ref().map_or(0o644, |v| v.mode);
                let file = parent.anonymous(bytes, mode)?;
                journal.entries[index].staged = Some(FileState {
                    identity: identity(&file)?,
                    sha256: hash(bytes),
                    size: bytes.len(),
                    mode,
                });
                // Registration precedes the first named staged byte, closing the
                // crash gap that would otherwise leave an unowned temporary.
                write_journal(&transaction, &journal)?;
                parent.link_anonymous(&file, &journal.entries[index].temporary)?;
            } else {
                write_journal(&transaction, &journal)?;
            }
        }
        // All parents and every original are rechecked after staging, before
        // any source publication; staging itself only creates private slots.
        for entry in &journal.entries {
            if state(&root, entry)? != Publication::Untouched {
                return Err("Harness source changed before publication".into());
            }
        }
        if capture(project)?["sourceFingerprint"].as_str()
            != Some(&journal.binding.source_fingerprint)
        {
            return Err("Harness project changed during preparation".into());
        }
        journal.phase = "publishing".into();
        write_journal(&transaction, &journal)?;
        for entry in &journal.entries {
            publish(project, &root, entry)?;
        }
        verify_root(project, &journal.binding.root_identity)?;
        for entry in &journal.entries {
            if state(&root, entry)? != Publication::Published {
                return Err("Harness source changed before durable commit".into());
            }
        }
        journal.phase = "committed".into();
        write_journal(&transaction, &journal)?;
        Ok::<_, String>(())
    })();
    if let Err(reason) = result {
        // The final journal rename may have succeeded even if directory sync
        // failed. A retry must inspect disk before deciding whether to restore.
        if journal.phase == "committed" {
            return Err(format!(
                "Harness commit outcome needs recovery; files and backups retained: {reason}"
            ));
        }
        let rollback_result = rollback(project, &root, &transaction, &mut journal);
        return Err(match rollback_result {
            Ok(()) => format!("Harness application rolled back; backups retained: {reason}"),
            Err(recovery) => format!("Harness application stopped; recovery required and backups retained: {reason}; {recovery}"),
        });
    }
    let cleanup_pending = finish_cleanup(&root, &transaction, &mut journal).is_err();
    Ok(
        json!({"status":"applied","proposalId":proposal_id,"digest":digest,"backupAvailable":true,"cleanupPending":cleanup_pending}),
    )
}

#[cfg(test)]
#[path = "harness/tests.rs"]
mod tests;
