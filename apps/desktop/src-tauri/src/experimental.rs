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
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::State;

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
    ExtensionsRereview,
    ExtensionsSafeMode,
    ExtensionsCards,
    ExtensionsAction,
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
        // RH snapshots are captured by Rust, include bounded base64 bytes, and
        // need more room than renderer-authored legacy requests.
        let limit = if operation == "rh_code_run" {
            1_000_000
        } else {
            128_000
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
            if line.len() > 1_000_001 {
                return Err("实验功能结果超过 1 MB".into());
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
}
impl Host {
    pub fn new(project: PathBuf) -> Self {
        Self {
            project,
            session: Mutex::new(None),
            closing: AtomicBool::new(false),
            stopped: AtomicBool::new(true),
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
        Ok(Session {
            child,
            input,
            output,
            _lease: lease,
            broken: false,
        })
    }
    fn call(&self, operation: &str, args: Value) -> Result<Value, String> {
        let mut owner = self.session.lock().map_err(|_| "实验功能引擎状态不可用")?;
        if self.closing.load(Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        if owner.is_none() {
            *owner = Some(self.start()?);
        }
        let result = owner.as_mut().unwrap().call(operation, args);
        if owner.as_ref().is_some_and(|session| session.broken) {
            drop(owner.take());
        }
        result
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
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
    if matches!(operation, Operation::RhCodeStart) {
        args = code.call("agent_bound", args)?;
    }
    if matches!(operation, Operation::RhCodeRun) {
        args = code.call("agent_context", args)?;
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
    tauri::async_runtime::spawn_blocking(move || host.call(&operation, args))
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
