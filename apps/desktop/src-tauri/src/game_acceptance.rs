//! Explicit disposable client acceptance, dispatched before GUI or accounts.
//!
//! A bounded closed request may allocate only a new native private child under
//! an existing parent. Installation, official Java acquisition, core readiness
//! and launch planning, and detached supervision reuse the production engines.
//! No existing launcher configuration, account book or game root is opened.
//! JSON-lines events let an external native observer bind window evidence to
//! the ACKed game PID. Log initialization never substitutes for visible render.
mod world;
use crate::{
    java_download, launcher_game_monitor,
    tasks::{TaskKind, TaskOutcome, TaskTarget, Tasks},
};
use pcl_core::{
    content_scope::IsolationPolicy,
    java::JavaSelection,
    launch_options::{LaunchOptions, WindowSize},
    retained_fs::{Directory, Leaf, Links, Traversal},
};
use pcl_install::{ComponentSelection, InstallRequest, Installer};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

const INPUT_LIMIT: u64 = 16 * 1024;
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(900);
const LOG_LIMIT: u64 = 2 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Prepare,
    Run,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    parent: PathBuf,
    minecraft: String,
    fabric: Option<String>,
    mode: Mode,
    duration_seconds: u64,
}

impl Request {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.minecraft != "1.21.4"
            || !(30..=120).contains(&self.duration_seconds)
        {
            return Err("Acceptance requires schema 1, Minecraft 1.21.4 and a 30–120 second observation budget".into());
        }
        pcl_core::retained_fs::validate_absolute_path(&self.parent).map_err(|e| e.to_string())?;
        if self.parent.to_str().is_none()
            || self.parent.as_os_str().len() > 4096
            || self.parent.to_string_lossy().chars().any(char::is_control)
        {
            return Err("Acceptance parent must be a bounded absolute UTF-8 path".into());
        }
        if let Some(fabric) = &self.fabric {
            if fabric.is_empty()
                || fabric.len() > 80
                || !fabric
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            {
                return Err("Acceptance Fabric version must be an exact version identifier".into());
            }
        }
        Ok(())
    }
}

fn emit(event: &str, data: &Value) -> Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(
        &mut output,
        &json!({"schema_version":1,"event":event,"data":data}),
    )
    .map_err(|_| "Cannot write acceptance evidence")?;
    output
        .write_all(b"\n")
        .and_then(|_| output.flush())
        .map_err(|e| e.to_string())
}

/// Return None for every other role. Invalid arity never initializes a store.
pub(crate) fn dispatch_cli() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new("--game-acceptance")) {
        return None;
    }
    let result = if args.next().is_some() {
        Err("--game-acceptance accepts only its bounded JSON stdin request".into())
    } else {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(INPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                if bytes.len() as u64 > INPUT_LIMIT {
                    return Err("Acceptance request exceeds 16 KiB".into());
                }
                let request: Request = serde_json::from_slice(&bytes)
                    .map_err(|_| "Invalid acceptance request".to_owned())?;
                request.validate()?;
                execute(request)
            })
    };
    match result {
        Ok(()) => Some(0),
        Err(error) => {
            let _ = emit("request_error", &json!({"status":"fail","reason":error}));
            Some(1)
        }
    }
}

struct Workspace {
    parent: Directory,
    child: Directory,
    name: Leaf,
    path: PathBuf,
}

fn traversal_cap(parent: &Directory, name: &Leaf, created: Directory) -> Result<Directory> {
    // Native creation capabilities may retain DELETE access for rollback.
    // Windows' sealed cwd lease must exclude such access for its entire graph.
    // Reopen from the original parent (not from `created`, whose parent chain
    // would keep that capability alive), bind the same object, then release it.
    let identity = created.identity().map_err(|e| e.to_string())?;
    let child = parent
        .child(name, Traversal::SameFilesystem)
        .map_err(|e| e.to_string())?;
    if child.identity().map_err(|e| e.to_string())? != identity {
        return Err("New acceptance directory changed during capability handoff".into());
    }
    parent.recheck(name, &identity).map_err(|e| e.to_string())?;
    drop(created);
    child
        .metadata()
        .and_then(|metadata| metadata.require_private())
        .map_err(|e| e.to_string())?;
    Ok(child)
}

impl Workspace {
    fn new(parent: &Path) -> Result<Self> {
        let directory = Directory::open_absolute(parent).map_err(|e| e.to_string())?;
        // The caller explicitly grants this parent only for allocating a new
        // random private child. A system-owned temporary parent need not be
        // owned by TokenUser; the child itself must pass the native private
        // policy and retained identity checks before any game data is written.
        let (name, created) = directory
            .create_private_workspace()
            .map_err(|e| e.to_string())?;
        let child = traversal_cap(&directory, &name, created)?;
        // Production launch planning canonicalizes its game root. Keep the
        // same native representation here too (notably Windows' verbatim
        // prefix), so containment checks compare names in one namespace. The
        // retained parent/child identity below still decides which directory
        // this pathname is allowed to reference.
        let path = parent
            .join(name.as_os_str())
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let workspace = Self {
            parent: directory,
            child,
            name,
            path,
        };
        workspace.verify()?;
        let mut marker = workspace
            .child
            .create_file(&Leaf::new("game-acceptance-owner.json").map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        marker
            .write_all(b"{\"schema_version\":1,\"purpose\":\"disposable-game-acceptance\"}\n")
            .and_then(|_| marker.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(workspace)
    }
    fn verify(&self) -> Result<()> {
        self.child
            .metadata()
            .and_then(|m| m.require_private())
            .map_err(|e| e.to_string())?;
        let held = self.child.identity().map_err(|e| e.to_string())?;
        self.parent
            .recheck(&self.name, &held)
            .map_err(|e| e.to_string())?;
        if Directory::open_absolute(&self.path)
            .and_then(|d| d.identity())
            .map_err(|e| e.to_string())?
            != held
        {
            return Err("Acceptance workspace identity changed".into());
        }
        Ok(())
    }
}

/// One timer cancels both production transfer engines at the same deadline.
/// Dropping it wakes and joins the timer; no sleeper survives the role.
struct Deadline {
    until: Instant,
    tokens: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    done: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Deadline {
    fn new(cancel: Arc<AtomicBool>) -> Self {
        let tokens = Arc::new(Mutex::new(vec![cancel]));
        let shared = tokens.clone();
        let (done, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            if receiver.recv_timeout(DOWNLOAD_LIMIT).is_err() {
                for token in shared.lock().unwrap().iter() {
                    token.store(true, Ordering::SeqCst);
                }
            }
        });
        Self {
            until: Instant::now() + DOWNLOAD_LIMIT,
            tokens,
            done: Some(done),
            worker: Some(worker),
        }
    }
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.until {
            Err("Acceptance download deadline exceeded".into())
        } else {
            Ok(())
        }
    }
    fn register(&self, token: Arc<AtomicBool>) -> Result<()> {
        let mut tokens = self.tokens.lock().unwrap();
        self.check()?;
        tokens.push(token);
        Ok(())
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn set_stage(report: &mut Value, name: &str, status: &str, evidence: Value) {
    report["stages"][name] = json!({"status":status,"evidence":evidence});
}

fn execute(request: Request) -> Result<()> {
    let workspace = Workspace::new(&request.parent)?;
    // This single-threaded early role removes JVM injection variables from its
    // own descendants. It never changes the user's environment or home path.
    for variable in [
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "CLASSPATH",
    ] {
        std::env::remove_var(variable);
    }
    let mut report = json!({
        "schema_version":1,"kind":"game_acceptance_client","workspace":workspace.path,
        "source":{"commit":option_env!("PCL_BUILD_COMMIT"),"profile":option_env!("PCL_BUILD_PROFILE"),
                  "target":option_env!("PCL_BUILD_TARGET"),"version":env!("CARGO_PKG_VERSION")},
        "request":{"minecraft":request.minecraft,"fabric":request.fabric,"duration_seconds":request.duration_seconds},
        "identity":{"kind":"offline","name":"PCLAcceptance","accounts_opened":false},
        "production_engines":["Installer::resolve_request","Installer::install_resolved_request",
            "java_download::install_recommended_guarded","inspection::inspect_instance_with_isolation",
            "build_launch_plan_with_options","launcher_game_monitor::spawn_monitor"],
        "stages":{},"game_acceptance":"not_run",
        "limitations":["Offline client only; Microsoft login and authenticated launch were not tested.",
            "Quick Play joins a new disposable local world; no manual gameplay is exercised.",
            "A PID, initialization log or window alone does not establish interactive gameplay.",
            "The external observer must bind native window evidence to the emitted game PID.",
            "PreparedLaunch settings/admission and launcher UI are not exercised by this diagnostic role."]
    });
    for name in [
        "official_install",
        "official_java_download",
        "launch_readiness",
        "launch_plan",
        "world_prepare",
        "singleplayer_join",
        "supervised_spawn",
        "game_initialization",
        "visible_render",
        "owned_shutdown",
        "microsoft_login",
        "authenticated_launch",
    ] {
        set_stage(&mut report, name, "not_run", Value::Null);
    }
    emit("workspace", &json!({"path":workspace.path}))?;
    let mut current = "official_install";
    let result = run_pipeline(&request, &workspace, &mut report, &mut current);
    if let Err(error) = &result {
        report["stages"][current]["status"] = json!("fail");
        if !report["stages"][current]["evidence"].is_object() {
            report["stages"][current]["evidence"] = json!({});
        }
        report["stages"][current]["evidence"]["reason"] = json!(error);
        report["attempt_status"] = json!("fail");
    } else {
        report["attempt_status"] = json!("complete");
    }
    // Preserve only this newly generated private tree for CI evidence. A caller
    // may remove it after its own evidence collection; no user-root cleanup runs.
    report["workspace_retained"] = json!(true);
    emit("result", &report)?;
    result
}

fn run_pipeline(
    request: &Request,
    workspace: &Workspace,
    report: &mut Value,
    current: &mut &'static str,
) -> Result<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    let deadline = Deadline::new(cancel.clone());
    let game_name = Leaf::new("game").map_err(|e| e.to_string())?;
    let created_game = workspace
        .child
        .create_private(&game_name, Traversal::SameFilesystem)
        .map_err(|e| e.to_string())?;
    let game = traversal_cap(&workspace.child, &game_name, created_game)?;
    let root = workspace.path.join("game");
    let id = if request.fabric.is_some() {
        "acceptance-fabric"
    } else {
        "acceptance-vanilla"
    };
    let install = InstallRequest {
        minecraft: request.minecraft.clone(),
        name: id.into(),
        components: request
            .fabric
            .iter()
            .map(|version| ComponentSelection {
                provider: "fabric".into(),
                version: version.clone(),
            })
            .collect(),
    };
    let installer = Installer::new()?;
    let resolved = installer.resolve_request(&install, &cancel)?;
    if resolved.required_java() != 21 || resolved.processor_java_requirement()?.is_some() {
        return Err("Acceptance scope requires Java 21 and no installer processors".into());
    }
    workspace.verify()?;
    let installed = installer.install_resolved_request(&root, &resolved, &cancel, |progress| {
        if progress.completed == progress.total || progress.completed % 100 == 0 {
            eprintln!(
                "acceptance-install {} / {}",
                progress.completed, progress.total
            );
        }
    })?;
    workspace.verify()?;
    if Directory::open_absolute(&root)
        .and_then(|d| d.identity())
        .map_err(|e| e.to_string())?
        != game.identity().map_err(|e| e.to_string())?
    {
        return Err("Acceptance game root identity changed".into());
    }
    set_stage(
        report,
        "official_install",
        "pass",
        serde_json::to_value(installed).map_err(|e| e.to_string())?,
    );

    *current = "official_java_download";
    deadline.check()?;
    let tasks = Arc::new(Tasks::new());
    let task = tasks.admit(
        TaskTarget {
            root_id: "acceptance".into(),
            root_path: root.display().to_string(),
            instance_id: Some(id.into()),
        },
        TaskKind::JavaInstall,
    )?;
    deadline.register(task.cancellation_token())?;
    let binding = java_download::StoreBinding::capture(&workspace.path)?;
    let store = binding.open()?;
    let http = java_download::http(pcl_network::snapshot(), pcl_network::download_snapshot())?;
    // Always acquire Java into the new store: a preinstalled host Java cannot
    // turn this automatic-download acceptance stage into a discovery-only pass.
    let java = tauri::async_runtime::block_on(java_download::install_recommended_guarded(
        &store,
        21,
        http,
        &task,
        &|| deadline.check(),
        &|| {
            deadline.check()?;
            workspace.verify()
        },
    ));
    let java = match java {
        Ok(java) => {
            task.finish(TaskOutcome::Complete {
                result: None,
                message: "Acceptance Java installed".into(),
                error: None,
            });
            java
        }
        Err(error) => {
            task.finish(TaskOutcome::Error(error.clone()));
            return Err(error);
        }
    };
    if java.runtime.major != 21 || !java.path.starts_with(workspace.path.join(".pcl-rust/java")) {
        return Err("Automatic Java result escaped the fresh managed store".into());
    }
    let choice = JavaSelection::Manual {
        path: java.path.display().to_string(),
    };
    set_stage(
        report,
        "official_java_download",
        "pass",
        json!({"runtime":java.runtime,"tasks":tasks.list()}),
    );

    *current = "launch_readiness";
    let inspection = pcl_core::inspection::inspect_instance_with_isolation(
        &root,
        &workspace.path,
        id,
        &choice,
        &[],
        IsolationPolicy::Isolated,
    )?;
    let ready = inspection.ready;
    set_stage(
        report,
        "launch_readiness",
        if ready { "pass" } else { "fail" },
        serde_json::to_value(inspection).map_err(|e| e.to_string())?,
    );
    if !ready {
        return Err("Production launch inspection is not ready".into());
    }

    *current = "launch_plan";
    let home = pcl_core::java::invocation_path(&workspace.path)?
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let options = LaunchOptions {
        window: WindowSize::Custom {
            width: 854,
            height: 480,
        },
        jvm_args: format!("\"-Duser.home={home}\""),
        game_args: format!(
            "--quickPlaySingleplayer {} --quickPlayPath {}",
            world::WORLD_NAME,
            world::QUICK_LOG
        ),
        isolation: IsolationPolicy::Isolated,
        ..Default::default()
    };
    let plan = pcl_core::build_launch_plan_with_options(
        &root,
        &workspace.path,
        id,
        "PCLAcceptance",
        2,
        &choice,
        &[],
        &options,
    )?;
    let plan_bytes = serde_json::to_vec(&plan).map_err(|e| e.to_string())?;
    set_stage(
        report,
        "launch_plan",
        "pass",
        json!({"sha256":format!("{:x}",Sha256::digest(&plan_bytes)),
        "java":plan.java,"game_dir":plan.game_dir,"log_path":plan.log_path,"argument_count":plan.args.len()}),
    );
    *current = "world_prepare";
    let world = world::World::prepare(&root, id, &plan.game_dir)?;
    set_stage(report, "world_prepare", "pass", world.evidence.clone());
    emit(
        "prepared",
        &json!({"workspace":workspace.path,"instance":id,"java":plan.java}),
    )?;
    if request.mode == Mode::Prepare {
        report["game_acceptance"] = json!("not_run");
        return Ok(());
    }

    *current = "supervised_spawn";
    deadline.check()?;
    workspace.verify()?;
    let monitor_request = launcher_game_monitor::MonitorRequest::new(
        workspace.path.clone(),
        "acceptance".into(),
        root.clone(),
        id.into(),
        plan.java,
        plan.args,
        plan.game_dir,
        plan.log_path.clone(),
        Vec::new(),
    )?;
    let spawn_time_ms = world::epoch_ms()?;
    let started = launcher_game_monitor::spawn_monitor(monitor_request)?;
    let mut owned = OwnedMonitor {
        project: workspace.path.clone(),
        session: started.session_id.clone(),
        pid: started.pid,
        monitor_pid: started.monitor_pid,
        finished: false,
    };
    set_stage(
        report,
        "supervised_spawn",
        "pass",
        serde_json::to_value(&started).map_err(|e| e.to_string())?,
    );
    emit(
        "spawned",
        &json!({"pid":started.pid,"monitor_pid":started.monitor_pid,"session_id":started.session_id,"workspace":workspace.path}),
    )?;
    *current = "owned_shutdown";
    let began = Instant::now();
    let mut early_exit = None;
    let mut joined = None;
    let mut join_poll_error = None;
    while began.elapsed() < Duration::from_secs(request.duration_seconds) {
        let status = owned.status()?;
        if joined.is_none() && join_poll_error.is_none() {
            match world.poll_join(spawn_time_ms) {
                Ok(Some(evidence)) => {
                    emit(
                        "world_joined",
                        &json!({"world":world::WORLD_NAME,"pid":started.pid}),
                    )?;
                    joined = Some(evidence);
                }
                Ok(None) => {}
                Err(error) => {
                    join_poll_error = Some(error);
                }
            }
        }
        if !status.busy {
            early_exit = Some(status.exit_code);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let stopped = owned.finish()?;
    set_stage(
        report,
        "owned_shutdown",
        "pass",
        json!({"monitor":stopped,"observed_seconds":began.elapsed().as_secs_f64(),
        "stop_requested":early_exit.is_none(),"early_exit":early_exit}),
    );
    *current = "game_initialization";
    let evidence = log_evidence(&root, &plan.log_path)?;
    set_stage(report, "game_initialization", "observed", evidence.clone());
    *current = "singleplayer_join";
    let joined =
        match joined {
            Some(joined) => joined,
            None => return Err(join_poll_error.unwrap_or_else(|| {
                "Real client did not produce matching Quick Play join evidence before its deadline"
                    .into()
            })),
        };
    let join_evidence = world.finish_join(joined, &evidence)?;
    let join_passed = join_evidence["passed"] == true;
    set_stage(
        report,
        "singleplayer_join",
        if join_passed { "pass" } else { "fail" },
        join_evidence,
    );
    if !join_passed {
        return Err("Quick Play log was written but integrated-server/player login log evidence is incomplete".into());
    }
    set_stage(
        report,
        "visible_render",
        "not_observed",
        json!({"reason":"A native external observer must examine only the emitted game PID's window"}),
    );
    report["game_acceptance"] = json!("awaiting_native_window_evidence");
    if early_exit.is_some_and(|code| code != Some(0)) {
        *current = "game_initialization";
        return Err(
            "Client exited before the observation budget; inspect the retained own-game log".into(),
        );
    }
    Ok(())
}

struct OwnStatus {
    revision: String,
    busy: bool,
    game_running: bool,
    exit_code: Option<i32>,
    evidence: Value,
}
struct OwnedMonitor {
    project: PathBuf,
    session: String,
    pid: u32,
    monitor_pid: u32,
    finished: bool,
}
impl OwnedMonitor {
    fn status(&self) -> Result<OwnStatus> {
        let status = launcher_game_monitor::read_status(&self.project)?
            .ok_or("Acceptance monitor marker disappeared")?;
        if status.session_id != self.session
            || status.monitor_pid != self.monitor_pid
            || status.pid != Some(self.pid)
        {
            return Err(
                "Acceptance monitor identity changed; refusing to stop another session".into(),
            );
        }
        let evidence = serde_json::to_value(&status).map_err(|e| e.to_string())?;
        Ok(OwnStatus {
            revision: status.revision,
            busy: status.busy,
            game_running: status.game_running,
            exit_code: status.exit_code,
            evidence,
        })
    }
    fn finish(&mut self) -> Result<Value> {
        let mut status = self.status()?;
        if status.game_running {
            if let Err(error) = launcher_game_monitor::stop_monitor(&self.project, &status.revision)
            {
                // Java may have exited between status and the production stop
                // admission. An actually retired own session needs no signal.
                status = self.status()?;
                if status.game_running {
                    return Err(error);
                }
            }
        }
        let until = Instant::now() + Duration::from_secs(15);
        while status.busy {
            if Instant::now() >= until {
                return Err(
                    "Owned acceptance monitor has not acknowledged shutdown; workspace retained"
                        .into(),
                );
            }
            std::thread::sleep(Duration::from_millis(100));
            status = self.status()?;
        }
        self.finished = true;
        Ok(status.evidence)
    }
}
impl Drop for OwnedMonitor {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish();
        }
    }
}

fn log_evidence(root: &Path, log: &Path) -> Result<Value> {
    let relative = log
        .strip_prefix(root)
        .map_err(|_| "Acceptance log escaped its own root")?;
    let mut components = relative.components().peekable();
    let mut directory = Directory::open_absolute(root).map_err(|e| e.to_string())?;
    while let Some(component) = components.next() {
        let name = Leaf::new(component.as_os_str()).map_err(|e| e.to_string())?;
        if components.peek().is_some() {
            directory = directory
                .child(&name, Traversal::SameFilesystem)
                .map_err(|e| e.to_string())?;
        } else {
            let mut file = directory
                .open_regular(&name, Links::Single)
                .map_err(|e| e.to_string())?;
            let len = file.metadata().map_err(|e| e.to_string())?.len();
            file.seek(SeekFrom::Start(len.saturating_sub(LOG_LIMIT)))
                .map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            file.take(LOG_LIMIT)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&bytes);
            let markers = [
                "Backend library:",
                "OpenGL",
                "Created:",
                "OpenAL initialized",
                "Reloading ResourceManager",
                "Setting user:",
            ]
            .iter()
            .filter(|marker| text.contains(**marker))
            .copied()
            .collect::<Vec<_>>();
            let errors = text
                .lines()
                .filter(|line| {
                    [
                        "GLFW error",
                        "Failed to create window",
                        "does not support OpenGL",
                        "FATAL",
                        "Exception in thread",
                    ]
                    .iter()
                    .any(|word| line.contains(word))
                })
                .take(16)
                .map(|line| line.chars().take(1024).collect::<String>())
                .collect::<Vec<_>>();
            return Ok(
                json!({"path":log,"total_bytes":len,"tail_bytes":bytes.len(),
                "tail_sha256":format!("{:x}",Sha256::digest(&bytes)),"initialization_markers":markers,"renderer_errors":errors,
                "singleplayer_markers":world::singleplayer_log_markers(&text),
                "proves_visible_render":false}),
            );
        }
    }
    Err("Acceptance log has no file name".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocated_workspace_and_production_log_use_the_same_canonical_namespace() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(parent.path()).unwrap();
        assert_eq!(workspace.path.canonicalize().unwrap(), workspace.path);
        let name = Leaf::new("own.log").unwrap();
        workspace
            .child
            .create_file(&name)
            .unwrap()
            .write_all(b"Backend library: LWJGL\n")
            .unwrap();
        let log = workspace.path.join("own.log").canonicalize().unwrap();
        assert_eq!(
            log_evidence(&workspace.path, &log).unwrap()["total_bytes"],
            23
        );
        workspace.verify().unwrap();
    }
    fn request() -> Value {
        json!({"schema_version":1,"parent":"/fixture/parent","minecraft":"1.21.4","fabric":null,"mode":"prepare","duration_seconds":30})
    }
    #[test]
    fn request_is_closed_and_has_no_identity_or_arbitrary_command_fields() {
        for key in ["access_token", "account", "java", "args", "root", "player"] {
            let mut value = request();
            value[key] = json!("unexpected");
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
    }
    #[test]
    fn version_duration_and_fabric_are_bounded_before_any_allocation() {
        for (key, value) in [
            ("minecraft", json!("1.20.1")),
            ("duration_seconds", json!(121)),
            ("fabric", json!("../bad")),
            ("schema_version", json!(2)),
        ] {
            let mut input = request();
            input[key] = value;
            assert!(serde_json::from_value::<Request>(input)
                .unwrap()
                .validate()
                .is_err());
        }
    }
}
