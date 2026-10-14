//! Root-scoped game launch logs (`.pcl-linux/logs`), separate from the desktop
//! launcher's own diagnostics. Commands bind roots, choose native destinations
//! and hold task/launch admission; this service owns bounded reads, redaction,
//! export publication and recoverable history cleanup.
//!
//! Preview and recovery listing are read-only. A confirmed clear first saves an
//! immutable intent journal, then moves files with no overwrite. Recovery infers
//! each move from its two locations, so a crash between rename and bookkeeping
//! cannot lose ownership. A durable completion receipt releases ownership of
//! restored originals before future launches replace them. Unknown or externally
//! changed data is always retained.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

#[path = "launcher_logs/filesystem.rs"]
mod filesystem;
use filesystem::{component, snapshot, Dir, Stamp};
#[path = "launcher_logs/history.rs"]
mod history;
pub use history::{execute_clear, list_recovery, prepare_clear, restore};

const MAX_ENTRIES: usize = 4096;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const FAILURE_TAIL_BYTES: u64 = 256 * 1024;
const FAILURE_TAIL_LINES: usize = 400;

#[derive(Clone, Debug, Serialize)]
pub struct LogRow {
    pub name: String,
    pub path: String,
    pub modified: u64,
    pub current: bool,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "name",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum ExportSelection {
    Named(String),
    Current,
    All,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    Text,
    Zip,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPlan {
    pub revision: String,
    pub file_count: usize,
    pub bytes: u64,
    pub suggested_name: String,
    pub format: ExportFormat,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    pub path: String,
    pub file_count: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearPlan {
    pub revision: String,
    pub file_count: usize,
    pub bytes: u64,
    pub names: Vec<String>,
    pub retained_current: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearOutcome {
    pub operation_id: String,
    pub file_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRow {
    pub operation_id: String,
    pub revision: String,
    pub created_at: u64,
    pub file_count: usize,
    pub bytes: u64,
    pub names: Vec<String>,
    pub warnings: Vec<String>,
    /// Files are back, but an interrupted task still needs its durable receipt.
    pub needs_completion: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcome {
    pub operation_id: String,
    pub file_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    stamp: Stamp,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    operation_id: String,
    created_at: u64,
    root: (u64, u64),
    logs: (u64, u64),
    entries: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    schema: u32,
    journal_digest: String,
}

struct Scope {
    path: PathBuf,
    root: Dir,
    owner: Option<Dir>,
    logs: Option<Dir>,
}

impl Scope {
    fn open(path: &Path) -> Result<Self, String> {
        let root = Dir::absolute(path)?;
        let owner = root.optional_child(".pcl-linux")?;
        let logs = owner
            .as_ref()
            .map(|dir| dir.optional_child("logs"))
            .transpose()?
            .flatten();
        Ok(Self {
            path: path.to_owned(),
            root,
            owner,
            logs,
        })
    }
    fn identities(&self) -> Result<((u64, u64), Option<(u64, u64)>), String> {
        Ok((
            self.root.stamp()?.identity(),
            self.logs
                .as_ref()
                .map(|d| d.stamp().map(|s| s.identity()))
                .transpose()?,
        ))
    }
    fn recheck(&self) -> Result<(), String> {
        let fresh = Self::open(&self.path)?;
        if fresh.identities()? != self.identities()?
            || fresh
                .owner
                .as_ref()
                .map(|d| d.stamp().map(|s| s.identity()))
                .transpose()?
                != self
                    .owner
                    .as_ref()
                    .map(|d| d.stamp().map(|s| s.identity()))
                    .transpose()?
        {
            return Err("日志目录在操作期间发生变化，已保留文件，请重新读取".into());
        }
        Ok(())
    }
    fn current_name(&self, current: Option<&Path>) -> Option<String> {
        let current = current?;
        if current.parent()? != self.path.join(".pcl-linux/logs") {
            return None;
        }
        let name = current.file_name()?.to_str()?;
        valid_log(name).ok()?;
        Some(name.to_owned())
    }
    fn store(&self) -> Result<Option<Dir>, String> {
        let store = self
            .owner
            .as_ref()
            .map(|owner| owner.optional_child("log-trash"))
            .transpose()?
            .flatten();
        if let Some(store) = &store {
            store.verify_private()?;
        }
        Ok(store)
    }
}

fn valid_log(name: &str) -> Result<(), String> {
    component(name)?;
    if !name.ends_with(".log") {
        return Err("只能选择 .log 日志文件".into());
    }
    Ok(())
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        Err("日志任务已取消".into())
    } else {
        Ok(())
    }
}

fn scan(
    scope: &Scope,
    names: Option<&[String]>,
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, Entry>, String> {
    let Some(logs) = &scope.logs else {
        if names.is_some_and(|v| !v.is_empty()) {
            return Err("没有找到所选日志".into());
        }
        return Ok(BTreeMap::new());
    };
    let names = match names {
        Some(names) => names.to_vec(),
        None => logs
            .names()?
            .into_iter()
            .filter(|n| n.ends_with(".log"))
            .collect(),
    };
    let mut entries = BTreeMap::new();
    let mut bytes = 0u64;
    for name in names {
        cancelled(cancel)?;
        valid_log(&name)?;
        let (stamp, digest, _) =
            snapshot(logs, &name, MAX_FILE_BYTES)?.ok_or("日志已消失，请重新读取")?;
        bytes = bytes.checked_add(stamp.bytes).ok_or("日志总大小过大")?;
        if bytes > MAX_TOTAL_BYTES {
            return Err("日志总大小超过 256 MiB，请分次导出或清理".into());
        }
        entries.insert(name, Entry { stamp, digest });
    }
    scope.recheck()?;
    Ok(entries)
}

fn revision(
    scope: &Scope,
    current: Option<&str>,
    entries: &BTreeMap<String, Entry>,
    purpose: &str,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(purpose, &scope.path, scope.identities()?, current, entries))
        .map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Existing CE list/read actions can use the same no-follow ownership boundary.
pub fn list(root: &Path, current: Option<&Path>) -> Result<Vec<LogRow>, String> {
    let scope = Scope::open(root)?;
    let Some(logs) = &scope.logs else {
        return Ok(Vec::new());
    };
    let current = scope.current_name(current);
    let mut rows = Vec::new();
    for name in logs.names()?.into_iter().filter(|n| n.ends_with(".log")) {
        valid_log(&name)?;
        let file = logs.file(&name)?.ok_or("日志已消失，请重试")?;
        let stamp = Stamp::of(&file.metadata().map_err(|e| e.to_string())?);
        rows.push(LogRow {
            path: root
                .join(".pcl-linux/logs")
                .join(&name)
                .display()
                .to_string(),
            current: current.as_ref() == Some(&name),
            name,
            modified: stamp.modified.0.max(0) as u64,
            bytes: stamp.bytes,
        });
    }
    scope.recheck()?;
    rows.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(rows)
}

pub fn read(root: &Path, name: &str, redact: impl Fn(&str) -> String) -> Result<String, String> {
    valid_log(name)?;
    let scope = Scope::open(root)?;
    let logs = scope.logs.as_ref().ok_or("日志目录不存在")?;
    let (_, _, bytes) = snapshot(logs, name, MAX_FILE_BYTES)?.ok_or("日志不存在")?;
    scope.recheck()?;
    Ok(redact(&String::from_utf8_lossy(&bytes)))
}

/// A live log may grow beyond the history-export limit. Read a bounded suffix
/// from one FD, retain at most the requested lines and permit append-only size
/// changes; replacing its pathname during the read is still refused.
pub fn read_tail(
    root: &Path,
    name: &str,
    lines: usize,
    redact: impl Fn(&str) -> String,
) -> Result<String, String> {
    valid_log(name)?;
    let scope = Scope::open(root)?;
    let logs = scope.logs.as_ref().ok_or("日志目录不存在")?;
    let mut file = logs.file(name)?.ok_or("日志不存在")?;
    let before = Stamp::of(&file.metadata().map_err(|e| e.to_string())?);
    let start = before.bytes.saturating_sub(MAX_FILE_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(before.bytes - start)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let current = logs.file(name)?.ok_or("日志已消失")?;
    if Stamp::of(&current.metadata().map_err(|e| e.to_string())?).identity() != before.identity() {
        return Err("当前日志已被替换，请重新读取".into());
    }
    scope.recheck()?;
    // Drop a partial first line before decoding, including a split UTF-8 code
    // point. The byte bound also prevents one enormous line from growing memory.
    let bytes = if start > 0 {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| &bytes[i + 1..])
            .unwrap_or(&[])
    } else {
        &bytes
    };
    let text = String::from_utf8_lossy(bytes);
    let count = text.lines().count();
    let suffix = text
        .lines()
        .skip(count.saturating_sub(lines.clamp(100, 100_000)))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(redact(&suffix))
}

/// Failure analysis binds one session log before releasing admission. It never
/// scans for a latest file, and keeps the root, directory and file FDs alive
/// through the final session check to prevent inode reuse hiding replacements.
pub(super) struct FailureLogCapture {
    path: PathBuf,
    root: Dir,
    owner: Option<FailureDirectory>,
    logs: Option<FailureDirectory>,
    pub name: Option<String>,
    file: Option<File>,
    stamp: Option<Stamp>,
    error: Option<String>,
}

struct FailureDirectory {
    stamp: Option<Stamp>,
    directory: Option<Dir>,
    error: Option<String>,
}
impl FailureDirectory {
    fn capture(parent: &Dir, name: &str) -> Result<Self, String> {
        let stamp = parent.entry_stamp(name)?;
        let (directory, error) = match parent.optional_child(name) {
            Ok(directory) => (directory, None),
            Err(error) => (None, Some(error)),
        };
        let captured = Self {
            stamp,
            directory,
            error,
        };
        captured.verify(parent, name)?;
        Ok(captured)
    }

    fn verify(&self, parent: &Dir, name: &str) -> Result<(), String> {
        let identity = |stamp: &Stamp| (stamp.identity(), stamp.mode & libc::S_IFMT);
        let fresh = parent.entry_stamp(name)?;
        if fresh.as_ref().map(identity) != self.stamp.as_ref().map(identity)
            || self.directory.as_ref().is_some_and(|directory| {
                directory.stamp().ok().as_ref().map(identity) != self.stamp.as_ref().map(identity)
            })
        {
            return Err("本次启动日志目录已变化，请重新诊断".into());
        }
        Ok(())
    }
}

pub(super) struct FailureLogTail {
    pub text: String,
    pub truncated: bool,
}

impl FailureLogCapture {
    pub fn capture(path: &Path, current: Option<&Path>) -> Result<Self, String> {
        let mut captured = Self {
            path: path.to_owned(),
            root: Dir::absolute(path)?,
            owner: None,
            logs: None,
            name: None,
            file: None,
            stamp: None,
            error: Some("本次启动尚未生成可读取的日志".into()),
        };
        let Some(current) = current else {
            return Ok(captured);
        };
        if current.parent() != Some(path.join(".pcl-linux/logs").as_path()) {
            captured.error = Some("本次会话日志未绑定到该游戏目录".into());
            return Ok(captured);
        }
        let Some(name) = current.file_name().and_then(|name| name.to_str()) else {
            captured.error = Some("本次会话日志文件名无效".into());
            return Ok(captured);
        };
        if let Err(error) = valid_log(name) {
            captured.error = Some(error);
            return Ok(captured);
        }
        captured.name = Some(name.to_owned());
        captured.owner = Some(FailureDirectory::capture(&captured.root, ".pcl-linux")?);
        let owner = captured.owner.as_ref().unwrap();
        let Some(owner_directory) = &owner.directory else {
            captured.error = Some(
                owner
                    .error
                    .clone()
                    .unwrap_or_else(|| "本次启动日志目录不存在".into()),
            );
            return Ok(captured);
        };
        captured.logs = Some(FailureDirectory::capture(owner_directory, "logs")?);
        let logs = captured.logs.as_ref().unwrap();
        let Some(logs_directory) = &logs.directory else {
            captured.error = Some(
                logs.error
                    .clone()
                    .unwrap_or_else(|| "本次启动日志目录不存在".into()),
            );
            return Ok(captured);
        };
        captured.stamp = logs_directory.entry_stamp(name)?;
        match logs_directory.file(name) {
            Ok(Some(file)) => {
                if Some(Stamp::of(
                    &file.metadata().map_err(|error| error.to_string())?,
                )) != captured.stamp
                {
                    return Err("本次启动日志已变化，请重新诊断".into());
                }
                captured.file = Some(file);
                captured.error = None;
            }
            Ok(None) => captured.error = Some("本次启动日志不存在".into()),
            Err(error) => captured.error = Some(error),
        }
        captured.verify()?;
        Ok(captured)
    }

    pub fn verify(&self) -> Result<(), String> {
        if Dir::absolute(&self.path)?.stamp()?.identity() != self.root.stamp()?.identity() {
            return Err("本次启动游戏目录已变化，请重新诊断".into());
        }
        if let Some(owner) = &self.owner {
            owner.verify(&self.root, ".pcl-linux")?;
            if let (Some(parent), Some(logs)) = (&owner.directory, &self.logs) {
                logs.verify(parent, "logs")?;
                if let (Some(directory), Some(name)) = (&logs.directory, &self.name) {
                    if directory.entry_stamp(name)? != self.stamp
                        || self.file.as_ref().is_some_and(|file| {
                            file.metadata().ok().map(|metadata| Stamp::of(&metadata)) != self.stamp
                        })
                    {
                        return Err("本次启动日志已变化，请重新诊断".into());
                    }
                }
            }
        }
        Ok(())
    }

    /// Read a fixed suffix from the captured FD. Dropping the initial partial
    /// line also removes a split UTF-8 character; one giant line cannot enlarge
    /// the buffer or manufacture a diagnostic from a fragment of a message.
    pub fn read_tail(&mut self) -> Result<FailureLogTail, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let file = self.file.as_mut().ok_or("本次启动日志不存在")?;
        let stamp = self.stamp.as_ref().ok_or("本次启动日志不存在")?;
        let start = stamp.bytes.saturating_sub(FAILURE_TAIL_BYTES);
        file.seek(SeekFrom::Start(start))
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        (&mut *file)
            .take(stamp.bytes - start)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 != stamp.bytes - start
            || Stamp::of(&file.metadata().map_err(|error| error.to_string())?) != *stamp
        {
            return Err("本次启动日志读取期间发生变化，请重新诊断".into());
        }
        let suffix = if start > 0 {
            bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| &bytes[index + 1..])
                .unwrap_or(&[])
        } else {
            &bytes
        };
        let text = String::from_utf8_lossy(suffix);
        let count = text.lines().count();
        Ok(FailureLogTail {
            text: text
                .lines()
                .skip(count.saturating_sub(FAILURE_TAIL_LINES))
                .collect::<Vec<_>>()
                .join("\n"),
            truncated: start > 0 || count > FAILURE_TAIL_LINES,
        })
    }
}

fn export_names(
    scope: &Scope,
    selection: &ExportSelection,
    current: Option<&Path>,
) -> Result<Option<Vec<String>>, String> {
    match selection {
        ExportSelection::Named(name) => {
            valid_log(name)?;
            Ok(Some(vec![name.clone()]))
        }
        ExportSelection::Current => Ok(Some(vec![scope
            .current_name(current)
            .ok_or("当前游戏目录没有本次会话日志，请先选择日志")?])),
        ExportSelection::All => Ok(None),
    }
}

pub fn prepare_export(
    root: &Path,
    selection: &ExportSelection,
    current: Option<&Path>,
) -> Result<ExportPlan, String> {
    let scope = Scope::open(root)?;
    let names = export_names(&scope, selection, current)?;
    let entries = scan(&scope, names.as_deref(), &AtomicBool::new(false))?;
    if entries.is_empty() {
        return Err("没有可导出的日志".into());
    }
    let all = matches!(selection, ExportSelection::All);
    Ok(ExportPlan {
        revision: revision(
            &scope,
            scope.current_name(current).as_deref(),
            &entries,
            if all { "export-all" } else { "export-one" },
        )?,
        file_count: entries.len(),
        bytes: entries.values().map(|e| e.stamp.bytes).sum(),
        suggested_name: if all {
            "game-logs.zip".into()
        } else {
            entries.keys().next().unwrap().clone()
        },
        format: if all {
            ExportFormat::Zip
        } else {
            ExportFormat::Text
        },
    })
}

/// The destination must be a newly chosen file. An anonymous stage disappears
/// on every pre-publication failure/cancellation; only fully redacted output is
/// linked into the user's directory, and linkat refuses any existing pathname.
pub fn execute_export(
    root: &Path,
    selection: &ExportSelection,
    expected_revision: &str,
    current: Option<&Path>,
    destination: &Path,
    cancel: &AtomicBool,
    redact: impl Fn(&str) -> String,
) -> Result<ExportOutcome, String> {
    cancelled(cancel)?;
    let scope = Scope::open(root)?;
    let names = export_names(&scope, selection, current)?;
    let entries = scan(&scope, names.as_deref(), cancel)?;
    let all = matches!(selection, ExportSelection::All);
    if entries.is_empty()
        || revision(
            &scope,
            scope.current_name(current).as_deref(),
            &entries,
            if all { "export-all" } else { "export-one" },
        )? != expected_revision
    {
        return Err("日志内容已变化，请重新准备导出；游戏运行时请先停止游戏".into());
    }
    let parent_path = destination.parent().ok_or("导出目标目录无效")?;
    let name = destination
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("导出文件名无效")?;
    component(name)?;
    let extension = destination.extension().and_then(|s| s.to_str());
    if (all && extension != Some("zip")) || (!all && !matches!(extension, Some("log" | "txt"))) {
        return Err(if all {
            "全部日志必须保存为 .zip 文件"
        } else {
            "日志必须保存为 .log 或 .txt 文件"
        }
        .into());
    }
    let parent = Dir::absolute(parent_path)?;
    // Saving into either service-owned directory would manufacture a new log or
    // corrupt its recovery topology; native destinations must be elsewhere.
    let parent_id = parent.stamp()?.identity();
    if scope
        .logs
        .as_ref()
        .is_some_and(|d| d.stamp().is_ok_and(|s| s.identity() == parent_id))
        || destination.starts_with(root.join(".pcl-linux/log-trash"))
    {
        return Err("请选择日志目录和恢复目录以外的导出位置".into());
    }
    let mut output = parent.anonymous()?;
    let logs = scope.logs.as_ref().ok_or("日志目录不存在")?;
    let mut written = 0u64;
    if all {
        let mut zip = ZipWriter::new(output);
        for (name, expected) in &entries {
            cancelled(cancel)?;
            let safe = export_text(logs, name, expected, &redact)?;
            written = written
                .checked_add(safe.len() as u64)
                .ok_or("脱敏后日志过大")?;
            if written > MAX_TOTAL_BYTES {
                return Err("脱敏后日志总大小超过安全上限".into());
            }
            zip.start_file(
                name,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .unix_permissions(0o600),
            )
            .map_err(|e| e.to_string())?;
            zip.write_all(safe.as_bytes()).map_err(|e| e.to_string())?;
        }
        output = zip.finish().map_err(|e| e.to_string())?;
    } else {
        let (name, expected) = entries.iter().next().ok_or("没有所选日志")?;
        let safe = export_text(logs, name, expected, &redact)?;
        written = safe.len() as u64;
        output
            .write_all(safe.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    cancelled(cancel)?;
    let fresh = scan(&scope, names.as_deref(), cancel)?;
    if revision(
        &scope,
        scope.current_name(current).as_deref(),
        &fresh,
        if all { "export-all" } else { "export-one" },
    )? != expected_revision
    {
        return Err("导出期间日志内容已变化，目标文件尚未保存，请重新导出".into());
    }
    if Dir::absolute(parent_path)?.stamp()?.identity() != parent_id {
        return Err("导出目标目录已变化，目标文件尚未保存".into());
    }
    cancelled(cancel)?;
    parent.publish(&output, name)?;
    Ok(ExportOutcome {
        path: destination.display().to_string(),
        file_count: entries.len(),
        bytes: written,
    })
}

fn export_text(
    logs: &Dir,
    name: &str,
    expected: &Entry,
    redact: &impl Fn(&str) -> String,
) -> Result<String, String> {
    let (stamp, digest, bytes) = snapshot(logs, name, MAX_FILE_BYTES)?.ok_or("所选日志已消失")?;
    if stamp != expected.stamp || digest != expected.digest {
        return Err("所选日志已变化，请重新准备导出".into());
    }
    let safe = redact(&String::from_utf8_lossy(&bytes));
    if safe.len() as u64 > MAX_FILE_BYTES {
        return Err("脱敏后日志超过 16 MiB 安全上限".into());
    }
    Ok(safe)
}

#[cfg(test)]
#[path = "launcher_logs/tests.rs"]
mod tests;
