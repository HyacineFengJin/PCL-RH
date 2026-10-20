//! Thin native ownership boundary for the supplied experimental modules.
//! One private Node host owns permissions and Pi jobs. IPC names are fixed,
//! project paths come from the launcher, and extension context is projected here.
use crate::Shared;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
};
use tauri::State;

#[path = "experimental/effects.rs"]
mod effects;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    RhCodeRun,
    RhCodeStart,
    MakerThreadRead,
    MakerThreads,
    MakerThreadRun,
    MakerThreadApply,
    MakerThreadArchive,
    Status,
    Catalog,
    ProjectsList,
    ProjectCreate,
    ProjectUpdate,
    ProjectUnits,
    ProjectUnitRead,
    ProjectUnitSave,
    ProjectProtection,
    ProjectFiles,
    ProjectFileRead,
    ProjectSearch,
    ProjectCompare,
    ProjectCompareFile,
    ProjectOpen,
    ProjectContinue,
    ExtensionsList,
    ExtensionsReview,
    ExtensionsConfirm,
    ExtensionsCancel,
    ExtensionsRevoke,
    ExtensionsDisable,
    ExtensionsEnable,
    ExtensionsRemove,
    ExtensionsRereview,
    ExtensionsSafeMode,
    ExtensionsCards,
    ExtensionsAction,
    ExtensionsSettings,
    SourceImport,
    ProviderCatalog,
    ProviderPresets,
    ProviderPresetSave,
    ProviderPresetSelect,
    ProviderPresetDelete,
    ProviderStart,
    ProviderStop,
    JobCreate,
    JobRead,
    HarnessBuildLog,
    JobCancel,
    ArtifactRead,
    MakerReview,
    PorterRecipe,
    PorterReview,
    PorterOriginResolve,
    PorterProjects,
    PorterProjectCreate,
    PorterProjectRead,
    PorterProjectMessage,
    PorterProjectMessageResolve,
    PorterProjectSource,
    PorterProjectArchive,
    PorterProjectConfigure,
    PorterProjectRound,
    PorterProjectVersions,
    PorterProjectCompare,
    ReviewRead,
    ReviewApply,
    ReviewCancel,
    ReportRead,
}
struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    _lease: File,
    broken: bool,
}
impl Session {
    fn call(&mut self, operation: &str, args: Value) -> Result<Value, String> {
        let text = serde_json::to_vec(&json!({"operation":operation,"args":args}))
            .map_err(|e| e.to_string())?;
        // RH snapshots contain native-owned base64 bytes. A settings batch can
        // contain sixteen 4000-character values, so Unicode also needs a larger
        // bounded envelope than the older renderer requests.
        let limit = match operation {
            "rh_code_run" => 24 * 1024 * 1024,
            "extensions_settings" => 512 * 1024,
            _ => 128_000,
        };
        if text.len() > limit {
            return Err(format!("引擎请求超过 {} KB", limit / 1000));
        }
        self.broken = true;
        self.input
            .write_all(&text)
            .and_then(|_| self.input.write_all(b"\n"))
            .and_then(|_| self.input.flush())
            .map_err(|_| "实验功能引擎已断开".to_string())?;
        let mut line = Vec::new();
        let reply_limit = if operation == "rh_code_harness_export" {
            24 * 1024 * 1024
        } else {
            1_000_000
        };
        // Poll each chunk instead of allowing a crashed host to block shutdown.
        loop {
            if self.output.buffer().is_empty() {
                let mut fd = libc::pollfd {
                    fd: self.output.get_ref().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut fd, 1, 20_000) };
                if ready <= 0 {
                    return Err("实验功能引擎未及时响应".into());
                }
            }
            let bytes = self
                .output
                .fill_buf()
                .map_err(|_| "无法读取实验功能结果".to_string())?;
            if bytes.is_empty() {
                return Err("实验功能引擎已退出".into());
            }
            let count = bytes
                .iter()
                .position(|v| *v == b'\n')
                .map(|n| n + 1)
                .unwrap_or(bytes.len());
            line.extend_from_slice(&bytes[..count]);
            self.output.consume(count);
            if line.len() > reply_limit + 1 {
                return Err("实验功能结果超过接口上限".into());
            }
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        let reply: Value =
            serde_json::from_slice(&line).map_err(|_| "实验功能引擎返回了无效结果".to_string())?;
        self.broken = false;
        if reply["ok"] == true {
            Ok(reply["result"].clone())
        } else {
            Err(reply["error"]
                .as_str()
                .unwrap_or("实验功能处理失败")
                .to_owned())
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.broken {
            let _ = self.call("shutdown", json!({}));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub struct Host {
    project: PathBuf,
    session: Mutex<Option<Session>>,
    closing: AtomicBool,
    stopped: AtomicBool,
    callers: AtomicUsize,
    resume_preset: Mutex<Option<String>>,
}
// Admission begins before the session lock, so idle retirement also sees calls
// queued behind a snapshot or a child operation. Drop covers all error paths.
struct CallAdmission<'a>(&'a AtomicUsize);
// Native source capture/application also owns the engine lifetime between
// individual stdio requests. App close waits for the complete admitted action.
struct NativeAdmission(Arc<Host>);
impl Drop for NativeAdmission {
    fn drop(&mut self) {
        self.0.callers.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Drop for CallAdmission<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Debug, Serialize, PartialEq)]
pub struct SuspendResult {
    released: bool,
    busy: bool,
}
impl Host {
    fn admit_native(self: &Arc<Self>) -> Result<NativeAdmission, String> {
        self.callers.fetch_add(1, Ordering::SeqCst);
        let admission = NativeAdmission(self.clone());
        // Acquire ownership before checking close: close either observes this
        // owner or this action refuses to begin native work after its fence.
        if self.closing.load(Ordering::SeqCst) {
            return Err("Launcher is closing".into());
        }
        Ok(admission)
    }
    pub fn new(project: PathBuf) -> Self {
        Self {
            project,
            session: Mutex::new(None),
            closing: AtomicBool::new(false),
            stopped: AtomicBool::new(true),
            callers: AtomicUsize::new(0),
            resume_preset: Mutex::new(None),
        }
    }
    fn start(&self) -> Result<Session, String> {
        let data = self.project.join(".pcl-rust/experimental");
        fs::create_dir_all(&data).map_err(|e| e.to_string())?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(data.join("host.lock"))
            .map_err(|e| e.to_string())?;
        if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("另一启动器正在使用实验功能引擎".into());
        }
        let mut child = Command::new("/usr/bin/node")
            .arg(self.project.join("experimental/host.mjs"))
            .arg(&data)
            .current_dir(&self.project)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env(
                "PCL_RH_BUILD_RUNNER_EXE",
                std::env::current_exe().map_err(|e| e.to_string())?,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                format!("无法启动实验功能引擎（需要 Node.js 22.19+ 和 Python 3.11+）：{e}")
            })?;
        let input = child.stdin.take().ok_or("无法打开实验功能通道")?;
        let output = BufReader::new(child.stdout.take().ok_or("无法读取实验功能通道")?);
        self.stopped.store(false, Ordering::SeqCst);
        let mut session = Session {
            child,
            input,
            output,
            _lease: lease,
            broken: false,
        };
        let mut resume_preset = self
            .resume_preset
            .lock()
            .map_err(|_| "实验功能引擎状态不可用")?;
        if let Some(id) = resume_preset.as_ref() {
            session.call("idle_resume", json!({"presetId": id}))?;
        }
        // The resumed process now owns provider state. A later crash must not
        // replay a stale pre-sleep choice after provider_stop or a preset edit.
        *resume_preset = None;
        Ok(session)
    }
    fn call(&self, operation: &str, args: Value) -> Result<Value, String> {
        self.call_with_effect(operation, args, None, |result, _| Ok(result))
    }
    /// An effect consumes a host permission decision before the session can
    /// process revoke, disable, settings changes or shutdown. The optional game
    /// admission lock is acquired first, but never held while waiting for a
    /// busy host: a previous action may be awaiting GTK on the main thread.
    fn call_with_effect<'a>(
        &'a self,
        operation: &str,
        args: Value,
        operations: Option<&'a Mutex<()>>,
        effect: impl FnOnce(Value, Option<MutexGuard<'a, ()>>) -> Result<Value, String>,
    ) -> Result<Value, String> {
        self.callers.fetch_add(1, Ordering::SeqCst);
        let _admission = CallAdmission(&self.callers);
        let (operation_guard, mut owner) = self.lock_effect_session(operations)?;
        if self.closing.load(Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        // A failed idle probe leaves ownership intact until an ordinary call or
        // app exit requests recovery. Its unread stdio reply cannot be consumed
        // as the next operation's result.
        if owner.as_ref().is_some_and(|session| session.broken) {
            drop(owner.take());
        }
        if owner.is_none() {
            match self.start() {
                Ok(session) => *owner = Some(session),
                Err(error) => {
                    self.stopped.store(true, Ordering::SeqCst);
                    return Err(error);
                }
            }
        }
        let result = owner.as_mut().unwrap().call(operation, args);
        if owner.as_ref().is_some_and(|session| session.broken) {
            drop(owner.take());
            self.stopped.store(true, Ordering::SeqCst);
        }
        let result = result?;
        effect(result, operation_guard)
    }
    fn lock_effect_session<'a>(
        &'a self,
        operations: Option<&'a Mutex<()>>,
    ) -> Result<(Option<MutexGuard<'a, ()>>, MutexGuard<'a, Option<Session>>), String> {
        let Some(operations) = operations else {
            return self
                .session
                .lock()
                .map(|owner| (None, owner))
                .map_err(|_| "实验功能引擎状态不可用".into());
        };
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return Err("启动器正在关闭".into());
            }
            let operation = operations.lock().map_err(|_| "启动器操作状态不可用")?;
            match self.session.try_lock() {
                Ok(owner) => return Ok((Some(operation), owner)),
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("实验功能引擎状态不可用".into());
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    drop(operation);
                    // Wait without operations, then retry in the original
                    // order. No permission decision exists until both locks
                    // have been acquired for the same action.
                    drop(self.session.lock().map_err(|_| "实验功能引擎状态不可用")?);
                }
            }
        }
    }
    /// Launcher diagnostics have their own native receipt boundary. Keep these
    /// operations out of the renderer's general experimental Operation enum.
    pub(super) fn diagnostic_call(&self, operation: &str, args: Value) -> Result<Value, String> {
        if !matches!(
            operation,
            "diagnostic_preview" | "diagnostic_analyze" | "diagnostic_status" | "diagnostic_cancel"
        ) {
            return Err("不支持的启动诊断引擎操作".into());
        }
        self.call(operation, args)
    }
    fn suspend(&self) -> Result<SuspendResult, String> {
        // This lock is also the start/call/exit ownership boundary. No operation
        // can enter the retiring process; later calls may lazily start a new one.
        let mut owner = match self.session.try_lock() {
            Ok(owner) => owner,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Ok(SuspendResult {
                    released: false,
                    busy: true,
                });
            }
            Err(_) => return Err("实验功能引擎状态不可用".into()),
        };
        if self.closing.load(Ordering::SeqCst) || self.callers.load(Ordering::SeqCst) != 0 {
            return Ok(SuspendResult {
                released: false,
                busy: true,
            });
        }
        let Some(session) = owner.as_mut() else {
            return Ok(SuspendResult {
                released: false,
                busy: false,
            });
        };
        if session.broken {
            return Err("实验功能引擎已断开".into());
        }
        let prepared = session.call("idle_prepare", json!({}));
        // An uncertain or failed idle check cannot authorize cancellation. Keep
        // the session for the ordinary call/exit recovery path.
        let prepared = prepared?;
        if prepared["busy"] != false || self.callers.load(Ordering::SeqCst) != 0 {
            return Ok(SuspendResult {
                released: false,
                busy: true,
            });
        }
        *self
            .resume_preset
            .lock()
            .map_err(|_| "实验功能引擎状态不可用")? =
            prepared["resumePresetId"].as_str().map(str::to_owned);
        drop(owner.take());
        self.stopped.store(true, Ordering::SeqCst);
        Ok(SuspendResult {
            released: true,
            busy: false,
        })
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst) && self.callers.load(Ordering::SeqCst) == 0
    }
    pub fn shutdown(self: &Arc<Self>) {
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        // Even an in-flight lazy start must be drained before the app exits.
        self.stopped.store(false, Ordering::SeqCst);
        let host = self.clone();
        std::thread::spawn(move || {
            if let Ok(mut owner) = host.session.lock() {
                drop(owner.take());
            }
            host.stopped.store(true, Ordering::SeqCst);
        });
    }
}
#[tauri::command]
pub async fn experimental_suspend(host: State<'_, Arc<Host>>) -> Result<SuspendResult, String> {
    let host = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || host.suspend())
        .await
        .map_err(|e| e.to_string())?
}
// This is the only instance data entering the extension host. An extension gets
// the four summary fields, never the opaque context or the complete bootstrap.
fn extension_context(shared: &Shared) -> Value {
    let (settings, _) = shared.config.view();
    let Some(id) = settings.selected else {
        return Value::Null;
    };
    let root_id = settings.root_id;
    let Ok(report) = pcl_core::scan_instances_report(std::path::Path::new(&settings.root)) else {
        return Value::Null;
    };
    let Some(instance) = report.instances.into_iter().find(|i| i.id == id) else {
        return Value::Null;
    };
    let summary = json!({"minecraftVersion":instance.minecraft_version,"loader":instance.loader,"modCount":instance.mod_count,"isolated":instance.isolated});
    json!({"rootId":root_id,"instanceId":id,"revision":summary.to_string(),"summary":summary})
}
#[tauri::command]
pub async fn experimental_call(
    window: tauri::WebviewWindow,
    host: State<'_, Arc<Host>>,
    shared: State<'_, Arc<Shared>>,
    code: State<'_, Arc<crate::rh_code::Store>>,
    operation: Operation,
    mut args: Value,
) -> Result<Value, String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    if !args.is_object() {
        return Err("实验功能参数必须为对象".into());
    }
    if matches!(
        operation,
        Operation::ExtensionsCards | Operation::ExtensionsAction
    ) {
        args["context"] = extension_context(&shared);
    }
    let operation = serde_json::to_value(operation)
        .map_err(|e| e.to_string())?
        .as_str()
        .unwrap()
        .to_owned();
    let host = host.inner().clone();
    let shared = shared.inner().clone();
    let code = code.inner().clone();
    let admission = host.admit_native()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _admission = admission;
        if operation == "extensions_action" {
            return effects::execute(&host, &shared, args, |text| {
                effects::copy_on_main_thread(&window, shared.clone(), text)
            });
        }
        let mut capture = None;
        if operation == "rh_code_start" {
            args = code.call("agent_bound", args)?;
        }
        if operation == "rh_code_run" {
            args = code.call("agent_context", args)?;
            capture = Some(args.clone());
        }
        let result = host.call(&operation, args)?;
        if let Some(captured) = capture {
            let job = result["jobId"]
                .as_str()
                .ok_or("Harness returned no task identity")?;
            if let Err(error) = code.bind_harness(captured, job) {
                let _ = host.call("job_cancel", json!({"jobId":job}));
                return Err(error);
            }
        }
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    Extension,
    Source,
}
#[tauri::command]
pub async fn experimental_choose(
    window: tauri::WebviewWindow,
    shared: State<'_, Arc<Shared>>,
    kind: Choice,
) -> Result<Value, String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    let picked = match kind {
        Choice::Extension => {
            shared
                .desktop
                .choose_launcher_file(window, shared.project.clone(), "extension_manifest")
                .await?
        }
        Choice::Source => {
            shared
                .desktop
                .choose_named_toolbox_directory(
                    window,
                    shared.project.clone(),
                    "选择模组源代码文件夹",
                )
                .await?
        }
    };
    Ok(
        json!({"status":picked.status,"path":picked.paths.first().map(|p| p.to_string_lossy().into_owned()),"message":picked.message}),
    )
}
#[tauri::command]
pub async fn experimental_open(
    host: State<'_, Arc<Host>>,
    job_id: String,
    operation_id: String,
) -> Result<(), String> {
    let host = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = host.call(
            "artifact_directory",
            json!({"jobId":job_id,"operationId":operation_id}),
        )?;
        let path = PathBuf::from(result["directory"].as_str().ok_or("无效工程路径")?)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !path.starts_with(host.project.join(".pcl-rust/experimental/jobs")) {
            return Err("工程不属于实验工作区".into());
        }
        Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Adopting a generated project is explicit and scoped to its native conversation.
/// The host verifies the artifact receipt; Rust publishes only into an empty folder.
#[tauri::command]
pub async fn rh_code_adopt(
    host: State<'_, Arc<Host>>,
    code: State<'_, Arc<crate::rh_code::Store>>,
    shared: State<'_, Arc<Shared>>,
    args: Value,
) -> Result<Value, String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("Launcher is closing".into());
    }
    let host = host.inner().clone();
    let code = code.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        code.call("artifact_owner", args.clone())?;
        let source = host.call("rh_code_artifact", args.clone())?;
        code.adopt(args, source)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Review requests project only bounded diff text. Exporting source bytes and
/// marking a proposal applied stay behind this native approval boundary.
#[tauri::command]
pub async fn rh_code_harness_reviews(
    host: State<'_, Arc<Host>>,
    code: State<'_, Arc<crate::rh_code::Store>>,
    shared: State<'_, Arc<Shared>>,
    args: Value,
) -> Result<Value, String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("Launcher is closing".into());
    }
    let host = host.inner().clone();
    let code = code.inner().clone();
    let admission = host.admit_native()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _admission = admission;
        code.call("artifact_owner", args.clone())?;
        host.call("rh_code_harness_reviews", args)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn rh_code_harness_apply(
    host: State<'_, Arc<Host>>,
    code: State<'_, Arc<crate::rh_code::Store>>,
    shared: State<'_, Arc<Shared>>,
    args: Value,
) -> Result<Value, String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("Launcher is closing".into());
    }
    let host = host.inner().clone();
    let code = code.inner().clone();
    let admission = host.admit_native()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _admission = admission;
        code.call("artifact_owner", args.clone())?;
        let proposal = host.call("rh_code_harness_export", args.clone())?;
        let snapshot = code.apply_harness(args.clone(), proposal)?;
        // A failed receipt update is retryable: the native transaction records
        // its exact digest and never applies that same proposal twice.
        host.call("rh_code_harness_applied", args)?;
        Ok(snapshot)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Only the native-owned project/job relation and the host's immutable run
/// receipt can select this directory. The renderer never supplies a path.
#[tauri::command]
pub async fn rh_code_build_open(
    host: State<'_, Arc<Host>>,
    code: State<'_, Arc<crate::rh_code::Store>>,
    shared: State<'_, Arc<Shared>>,
    args: Value,
) -> Result<(), String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("Launcher is closing".into());
    }
    let host = host.inner().clone();
    let code = code.inner().clone();
    let admission = host.admit_native()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _admission = admission;
        code.call("build_artifact_owner", args.clone())?;
        let result = host.call("rh_code_build_artifact_directory", args.clone())?;
        let directory = checked_build_directory(&host.project, &args, &result)?;
        Command::new("xdg-open")
            .arg(directory)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn checked_build_directory(
    project: &std::path::Path,
    args: &Value,
    result: &Value,
) -> Result<PathBuf, String> {
    let identifier = |key: &str| -> Result<&str, String> {
        let id = args[key].as_str().ok_or("Missing build identity")?;
        if id.len() != 36
            || id.bytes().enumerate().any(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c != b'-'
                } else {
                    !c.is_ascii_digit() && !(b'a'..=b'f').contains(&c)
                }
            })
        {
            return Err("Invalid build identity".into());
        }
        Ok(id)
    };
    let job = identifier("jobId")?;
    let run = identifier("runId")?;
    if result["runId"] != run || result["artifactCount"].as_u64().unwrap_or(0) == 0 {
        return Err("No completed build artifacts".into());
    }
    let base = project.join(".pcl-rust/experimental/jobs");
    let expected = base
        .join(job)
        .join("build-runs")
        .join(run)
        .join("artifacts");
    if result["directory"].as_str() != expected.to_str() {
        return Err("Build artifact directory differs from its owned run".into());
    }
    // Recheck each descendant, not just the final prefix; an intervening symlink
    // must not redirect the file manager into another run or user directory.
    let mut cursor = fs::canonicalize(&base).map_err(|e| e.to_string())?;
    if cursor != base {
        return Err("Build store path contains a link".into());
    }
    for segment in [job, "build-runs", run, "artifacts"] {
        cursor.push(segment);
        if !fs::symlink_metadata(&cursor)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("Build artifact path is not an owned directory".into());
        }
    }
    Ok(expected)
}

#[cfg(test)]
mod tests;
