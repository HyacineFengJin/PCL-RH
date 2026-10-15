//! One independent durable marker and lifetime lock per project. Reading state
//! never creates directories. A damaged/unknown marker is retained and returns an
//! error so admission can conservatively block launch and writes until repaired.
//! Beginning an admitted new attempt may explicitly retire its completed marker;
//! this durable phase fences old failure context across GUI restarts, while the
//! captured log and optional legacy identity remain untouched.
use super::process::{self, Identity};
use crate::launcher_local::filesystem::{
    optional_snapshot, revision, Dir, Scope, Snapshot, Stamp, WriteLock,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
const NAME: &str = "active.json";
const LIMIT: u64 = 16 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Phase {
    Starting,
    Running,
    Exited,
    Failed,
    Retired,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Marker {
    schema_version: u32,
    project: PathBuf,
    root_id: String,
    root_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
    log_path: PathBuf,
    monitor: Identity,
    game: Option<Identity>,
    phase: Phase,
    started_at: u64,
    exit_code: Option<i32>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorStatus {
    pub session_id: String,
    pub root_id: String,
    pub root_path: String,
    pub instance_id: Option<String>,
    pub log_path: String,
    pub pid: Option<u32>,
    pub monitor_pid: u32,
    pub busy: bool,
    pub supervisor_running: bool,
    pub game_running: bool,
    pub phase: String,
    pub started_at: u64,
    pub exit_code: Option<i32>,
    pub revision: String,
}
fn parse(project: &Path, snapshot: &Snapshot) -> Result<Marker, String> {
    let marker: Marker = serde_json::from_slice(&snapshot.header)
        .map_err(|_| "游戏监控状态无效，已保留文件；暂停启动和文件操作")?;
    // Version 1 predates explicit instance identity. Accept its original shape
    // without guessing from the log name or silently upgrading persisted bytes.
    // Version 2 launch records bind one validated instance to this session/root.
    // Explicit retirement can retain a legacy None identity without inventing
    // one; that exception never permits an identity-free version-2 launch.
    let supported_identity = match (marker.schema_version, marker.instance_id.as_deref()) {
        (1, None) => marker.phase != Phase::Retired,
        (2, Some(instance)) => validate_instance_id(instance).is_ok(),
        (2, None) => marker.phase == Phase::Retired,
        _ => false,
    };
    if !supported_identity
        || !project_matches(&marker.project, project)
        || marker.root_id.is_empty()
        || marker.root_id.len() > 128
        || marker.root_id.chars().any(char::is_control)
        || !marker.monitor.valid()
        || marker
            .game
            .as_ref()
            .is_some_and(|game| !game.valid() || game.pid == marker.monitor.pid)
        || matches!(marker.phase, Phase::Running | Phase::Exited) && marker.game.is_none()
    {
        return Err("游戏监控状态版本或身份不受支持，已保留文件".into());
    }
    validate_log_path(&marker.root_path, &marker.log_path)?;
    Ok(marker)
}
/// A project move can retain its former path as a compatibility symlink. Accept
/// that alias only when it resolves to this same project, leaving PID boot/start
/// identity, log ownership, marker revision and writer checks intact. No marker
/// is rewritten while a supervisor might still own it.
fn project_matches(recorded: &Path, current: &Path) -> bool {
    recorded == current
        || (recorded.is_absolute() && recorded.canonicalize().ok().as_deref() == Some(current))
}
fn status(marker: &Marker, revision: String) -> Result<MonitorStatus, String> {
    let supervisor_running = process::alive(&marker.monitor)?;
    let game_running = marker
        .game
        .as_ref()
        .map(process::alive)
        .transpose()?
        .unwrap_or(false);
    Ok(MonitorStatus {
        session_id: process::session_id(&marker.monitor),
        root_id: marker.root_id.clone(),
        root_path: marker.root_path.display().to_string(),
        instance_id: marker.instance_id.clone(),
        log_path: marker.log_path.display().to_string(),
        pid: marker.game.as_ref().map(|game| game.pid),
        monitor_pid: marker.monitor.pid,
        busy: supervisor_running || game_running,
        supervisor_running,
        game_running,
        phase: match marker.phase {
            Phase::Starting => "starting",
            Phase::Running => "running",
            Phase::Exited => "exited",
            Phase::Failed => "failed",
            Phase::Retired => "retired",
        }
        .into(),
        started_at: marker.started_at,
        exit_code: marker.exit_code,
        revision,
    })
}
pub fn read_status(project: &Path) -> Result<Option<MonitorStatus>, String> {
    read_status_observed(project, &mut |_| {})
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadPoint {
    Opened,
    BeforeRecheck,
}
/// A GUI can read while its detached helper atomically exchanges the marker.
/// Preserve the bounded old FD bytes before checking stability: the general
/// Snapshot reader rightly refuses a replaced inode, but cannot classify this
/// service's own phase transition after that refusal. No source text is logged.
fn capture_marker(
    folder: &Dir,
    observed: &mut impl FnMut(ReadPoint),
) -> Result<Option<Snapshot>, String> {
    let fd = unsafe {
        libc::openat(
            folder.0.as_raw_fd(),
            c"active.json".as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err("游戏监控状态无法读取，已保留文件".into())
        };
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|_| "无法检查游戏监控状态")?;
    // An exchange may already have unlinked this opened inode. It is still a
    // bounded owned regular FD; nlink=0 only permits capture, never acceptance.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() > 1
        || metadata.len() == 0
        || metadata.len() > LIMIT
    {
        return Err("游戏监控状态不是当前用户独占的有限普通文件，已保留文件".into());
    }
    let stamp = Stamp::of(&metadata);
    observed(ReadPoint::Opened);
    let mut header = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut header)
        .map_err(|_| "无法读取游戏监控状态")?;
    if header.len() as u64 != stamp.bytes || header.len() as u64 > LIMIT {
        return Err("游戏监控状态在读取期间被编辑，已保留文件".into());
    }
    Ok(Some(Snapshot {
        file,
        stamp,
        digest: format!("{:x}", Sha256::digest(&header)),
        header,
    }))
}
fn progresses(previous: &Marker, current: &Marker) -> bool {
    let same_binding = previous.project == current.project
        && previous.root_id == current.root_id
        && previous.root_path == current.root_path
        && previous.instance_id == current.instance_id
        && previous.log_path == current.log_path
        && previous.monitor == current.monitor
        && previous.started_at == current.started_at;
    if !same_binding {
        return false;
    }
    // A GUI retires only under the helper's lifetime lock and after observing
    // both process identities gone. This is the sole schema-1→2 transition and
    // retains every captured value, including the absent legacy instance ID.
    if current.phase == Phase::Retired && previous.phase != Phase::Retired {
        return current.schema_version == 2
            && previous.game == current.game
            && previous.exit_code == current.exit_code;
    }
    previous.schema_version == current.schema_version
        && previous.exit_code.is_none()
        && match (previous.phase, current.phase) {
            (Phase::Starting, Phase::Running | Phase::Exited) => {
                previous.game.is_none()
                    && current.game.is_some()
                    && (current.phase != Phase::Running || current.exit_code.is_none())
            }
            (Phase::Starting, Phase::Failed) => previous.game.is_none(),
            (Phase::Running, Phase::Exited | Phase::Failed) => previous.game == current.game,
            _ => false,
        }
}
fn lock_is_held(folder: &Dir) -> Result<bool, String> {
    let Some(file) = folder.file(".monitor.lock")? else {
        return Ok(false);
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        // Explicit unlock is required even if another descriptor is duplicated.
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
        return Ok(false);
    }
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err("无法确认独立游戏监控的占用状态".into())
    }
}
pub(super) fn read_status_observed(
    project: &Path,
    observed: &mut impl FnMut(ReadPoint),
) -> Result<Option<MonitorStatus>, String> {
    let scope = Scope::open(project, "game-monitor")?;
    let Some(folder) = &scope.folder else {
        scope.recheck()?;
        return Ok(None);
    };
    let mut previous: Option<(Marker, Snapshot)> = None;
    // One helper publishes starting→running→finished, then an admitted GUI may
    // retire it. Four complete bounded reads cover those legitimate exchanges;
    // repeated churn remains blocked.
    for _ in 0..4 {
        let Some(snapshot) = capture_marker(folder, observed)? else {
            scope.recheck()?;
            if previous.is_some() || lock_is_held(folder)? {
                return Err("独立游戏监控状态正在提交或已消失，请刷新后重试".into());
            }
            return Ok(None);
        };
        let marker = parse(project, &snapshot)?;
        if let Some((old, old_snapshot)) = &previous {
            if old_snapshot.stamp.device != snapshot.stamp.device
                || old_snapshot.stamp.inode == snapshot.stamp.inode
                || !progresses(old, &marker)
            {
                return Err("游戏监控状态被外部修改或替换，已保留文件；暂停启动和文件操作".into());
            }
        }
        let result = status(&marker, revision(&scope.path().join(NAME), &snapshot)?)?;
        observed(ReadPoint::BeforeRecheck);
        scope.recheck()?;
        if snapshot.recheck(folder, NAME).is_ok() {
            return Ok(Some(result));
        }
        previous = Some((marker, snapshot));
    }
    Err("独立游戏监控状态持续变化，请刷新后重试".into())
}
pub fn stop_monitor(project: &Path, expected_revision: &str) -> Result<(), String> {
    let scope = Scope::open(project, "game-monitor")?;
    let folder = scope.folder.as_ref().ok_or("没有可停止的独立游戏监控")?;
    let snapshot = Snapshot::open(folder, NAME, LIMIT)?;
    let marker = parse(project, &snapshot)?;
    if revision(&scope.path().join(NAME), &snapshot)? != expected_revision {
        return Err("游戏监控状态已变化，请刷新后停止".into());
    }
    scope.recheck()?;
    snapshot.recheck(folder, NAME)?;
    process::signal(
        marker.game.as_ref().ok_or("游戏尚未成功启动")?,
        libc::SIGKILL,
    )
}
/// A new preparation has passed its admission checks. Retire only the exact
/// observed record while owning the helper's lifetime lock. A stale revision,
/// unknown format, live identity, or publication conflict leaves its bytes for
/// inspection and prevents the new preparation from becoming current.
pub fn retire_completed(project: &Path, expected_revision: &str) -> Result<(), String> {
    retire_completed_observed(project, expected_revision, || Ok(()))
}
pub(super) fn retire_completed_observed(
    project: &Path,
    expected_revision: &str,
    before_publish: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let scope = Scope::open(project, "game-monitor")?;
    let folder = scope.folder.as_ref().ok_or("待退役的游戏监控记录已消失")?;
    let _lock = folder.lock(".monitor.lock")?;
    let snapshot = Snapshot::open(folder, NAME, LIMIT)?;
    let mut marker = parse(project, &snapshot)?;
    if revision(&scope.path().join(NAME), &snapshot)? != expected_revision {
        return Err("游戏监控记录已变化，请刷新后重新启动".into());
    }
    if status(&marker, String::new())?.busy {
        return Err("独立监控的游戏仍在运行，不能退役其记录".into());
    }
    if marker.phase == Phase::Retired {
        scope.recheck()?;
        return snapshot.recheck(folder, NAME);
    }
    // An interrupted helper can leave Starting/Running after both identities
    // have died. Retirement only removes its claim to current context; it never
    // fabricates a successful exit or changes its log/exit information.
    marker.schema_version = 2;
    marker.phase = Phase::Retired;
    let bytes = serde_json::to_vec(&marker).map_err(|_| "无法保存游戏监控退役状态")?;
    before_publish()?;
    scope.recheck()?;
    snapshot.recheck(folder, NAME)?;
    folder.replace(NAME, Some(&snapshot), &bytes, LIMIT)?;
    scope.recheck()
}
pub(super) struct Session {
    scope: Scope,
    _lock: WriteLock,
    marker: Marker,
    snapshot: Option<Snapshot>,
}
impl Session {
    pub fn start(
        project: &Path,
        root_id: String,
        root_path: PathBuf,
        instance_id: String,
        log_path: PathBuf,
    ) -> Result<Self, String> {
        validate_instance_id(&instance_id)?;
        let scope = Scope::create(project, "game-monitor")?;
        let folder = scope.folder.as_ref().unwrap();
        let lock = folder.lock(".monitor.lock")?;
        let snapshot = optional_snapshot(folder, NAME, LIMIT)?;
        if let Some(snapshot) = &snapshot {
            if status(&parse(project, snapshot)?, String::new())?.busy {
                return Err("该项目仍有独立监控的游戏，不能再次启动".into());
            }
        }
        let marker = Marker {
            schema_version: 2,
            project: project.to_owned(),
            root_id,
            root_path,
            instance_id: Some(instance_id),
            log_path,
            monitor: process::identity(std::process::id())?.ok_or("监控进程身份不可用")?,
            game: None,
            phase: Phase::Starting,
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            exit_code: None,
        };
        let mut session = Self {
            scope,
            _lock: lock,
            marker,
            snapshot,
        };
        session.save()?;
        Ok(session)
    }
    fn save(&mut self) -> Result<(), String> {
        self.scope.recheck()?;
        let folder = self.scope.folder.as_ref().unwrap();
        folder.replace(
            NAME,
            self.snapshot.as_ref(),
            &serde_json::to_vec(&self.marker).map_err(|_| "无法保存游戏监控状态")?,
            LIMIT,
        )?;
        self.scope.recheck()?;
        self.snapshot = Some(Snapshot::open(folder, NAME, LIMIT)?);
        Ok(())
    }
    pub fn spawned(&mut self, pid: u32) -> Result<(), String> {
        self.marker.game = Some(process::identity(pid)?.ok_or("游戏进程在确认前已结束")?);
        self.marker.phase = Phase::Running;
        self.save()
    }
    pub fn finished(&mut self, success: bool, exit_code: Option<i32>) -> Result<(), String> {
        self.marker.phase = if success {
            Phase::Exited
        } else {
            Phase::Failed
        };
        self.marker.exit_code = exit_code;
        self.save()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if matches!(self.marker.phase, Phase::Starting | Phase::Running) {
            self.marker.phase = Phase::Failed;
            let _ = self.save();
        }
    }
}
pub(super) fn validate_log_path(root: &Path, path: &Path) -> Result<(), String> {
    if !root.is_absolute()
        || root.components().any(|part| {
            !matches!(
                part,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
    {
        return Err("游戏监控目录必须是捕获的绝对目录".into());
    }
    if path.parent() != Some(root.join(".pcl-linux/logs").as_path()) {
        return Err("游戏监控只能写入该游戏目录的启动日志".into());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("游戏日志文件名无效")?;
    crate::launcher_local::filesystem::component(name)?;
    if name.starts_with('.') || !name.ends_with(".log") {
        return Err("游戏日志文件类型无效".into());
    }
    Ok(())
}
pub(super) fn validate_instance_id(instance: &str) -> Result<(), String> {
    crate::launcher_local::filesystem::component(instance)
        .map_err(|_| "游戏监控实例 ID 无效".into())
}
/// Only captured-root launch logs can be opened. Existing regular logs are
/// appended; creation never overwrites a competing new file or follows links.
pub(super) fn open_launch_log(root: &Path, path: &Path) -> Result<File, String> {
    validate_log_path(root, path)?;
    let root = Dir::absolute(root)?;
    let owner = root.create(".pcl-linux")?;
    let folder = owner.create("logs")?;
    let name = std::ffi::CString::new(path.file_name().unwrap().as_encoded_bytes())
        .map_err(|_| "游戏日志文件名无效")?;
    // O_PATH pins an existing inode without opening a device/FIFO for I/O. Only
    // that verified regular inode is then reopened through our own /proc FD.
    let pin = unsafe {
        libc::openat(
            folder.0.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    use std::os::unix::fs::MetadataExt;
    let fd = if pin >= 0 {
        let pinned = unsafe { File::from_raw_fd(pin) };
        let metadata = pinned.metadata().map_err(|_| "无法检查游戏日志")?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err("游戏日志不是当前用户独占的普通文件".into());
        }
        let proc_path = std::ffi::CString::new(format!("/proc/self/fd/{pin}")).unwrap();
        unsafe {
            libc::open(
                proc_path.as_ptr(),
                libc::O_WRONLY | libc::O_APPEND | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        }
    } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        unsafe {
            libc::openat(
                folder.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_APPEND
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o600,
            )
        }
    } else {
        return Err("无法检查游戏日志，已保留现有文件".into());
    };
    if fd < 0 {
        return Err("无法打开游戏日志，拒绝符号链接和特殊文件".into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|_| "无法检查游戏日志")?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.len() > super::output::LOG_LIMIT
    {
        return Err("游戏日志不是当前用户独占的有限普通文件".into());
    }
    let current = folder
        .file(path.file_name().unwrap().to_str().unwrap())?
        .ok_or("游戏日志在打开后消失")?
        .metadata()
        .map_err(|_| "无法检查游戏日志")?;
    if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
        return Err("游戏日志在打开期间被替换，已保留文件".into());
    }
    if Dir::absolute(path.parent().unwrap())?.identity()? != folder.identity()? {
        return Err("游戏日志目录在打开时发生变化".into());
    }
    folder.sync()?;
    Ok(file)
}
