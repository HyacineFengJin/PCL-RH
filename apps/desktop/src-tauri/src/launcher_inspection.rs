//! Read-only launch diagnostics at the desktop boundary.
//!
//! Admission captures one submitted root, instance and settings revision. Java
//! probing runs without the application or settings locks. The result is only
//! published after those inputs, directory identities and writer guards still
//! agree; navigation can never substitute a newly selected instance.

use crate::{
    config::GameRoot, ensure_instance_files_ready, instance_context, instance_reset, java_service,
    launcher_monitor_runtime, resource_ops, RunStatus, Shared,
};
use pcl_core::{inspection::LaunchInspection, java::JavaSelection};
use serde::Serialize;
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::ErrorKind,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

const CHANGED: &str = "检查期间实例、游戏目录或设置已变化，请重新检查启动环境";

#[derive(Serialize)]
pub(super) struct Inspection {
    pub root_id: String,
    #[serde(flatten)]
    pub report: LaunchInspection,
}

struct CapturedInspection {
    id: String,
    root: GameRoot,
    settings_revision: String,
    preferences_revision: String,
    tasks_revision: String,
    close_generation: u64,
    run: RunStatus,
    java: JavaSelection,
    extra_java_paths: Vec<String>,
    nodes: Vec<BoundNode>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct NodeIdentity {
    device: u64,
    inode: u64,
    mode: u32,
}
impl NodeIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode() & libc::S_IFMT,
        }
    }
}

/// Open directories remain owned throughout the probe, preventing inode reuse
/// from making a replacement look like the original target. Other node types
/// are recorded without following them, so the core can report invalid layout.
struct BoundNode {
    path: PathBuf,
    identity: Option<NodeIdentity>,
    directory: Option<File>,
}
impl BoundNode {
    fn capture(path: PathBuf) -> Result<Self, String> {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(format!("无法检查实例目录：{error}")),
        };
        let identity = metadata.as_ref().map(NodeIdentity::of);
        let directory = if metadata.as_ref().is_some_and(Metadata::is_dir) {
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .map_err(|error| format!("无法检查实例目录：{error}"))?;
            if Some(NodeIdentity::of(
                &directory.metadata().map_err(|error| error.to_string())?,
            )) != identity
            {
                return Err(CHANGED.into());
            }
            Some(directory)
        } else {
            None
        };
        Ok(Self {
            path,
            identity,
            directory,
        })
    }

    fn verify(&self) -> Result<(), String> {
        let identity = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => Some(NodeIdentity::of(&metadata)),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(_) => return Err(CHANGED.into()),
        };
        if identity != self.identity {
            return Err(CHANGED.into());
        }
        Ok(())
    }
}

/// Caller holds the admission lock. Keep the same game, writer and recovery
/// protections as launch planning, without looking up an account identity.
fn require_available(shared: &Shared) -> Result<(), String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请重新打开后再检查启动环境".into());
    }
    launcher_monitor_runtime::require_idle(shared)?;
    shared.config.checked_snapshot()?;
    if matches!(
        shared.status.lock().unwrap().stage.as_str(),
        "preparing" | "running"
    ) {
        return Err("请在游戏退出后检查启动环境".into());
    }
    if shared.tasks.active().is_some() {
        return Err("请在文件操作结束后检查启动环境".into());
    }
    Ok(())
}

fn require_files_ready(shared: &Shared, root: &GameRoot) -> Result<(), String> {
    resource_ops::ensure_ready(Path::new(&root.path))?;
    instance_reset::ensure_ready(Path::new(&root.path))?;
    ensure_instance_files_ready(shared, root)
}

fn capture(shared: &Shared, id: &str, root_id: Option<&str>) -> Result<CapturedInspection, String> {
    let _operation = shared.operations.lock().unwrap();
    require_available(shared)?;
    let (settings, root) = instance_context(&shared.config, root_id, id)?;
    require_files_ready(shared, &root)?;
    let preferences = shared.launcher_preferences.snapshot();
    if let Some(warning) = preferences.warning {
        return Err(warning);
    }
    preferences.preferences.validate()?;

    let mut nodes = vec![BoundNode::capture(PathBuf::from(&root.path))?];
    if nodes[0].directory.is_none() {
        return Err("游戏目录不是文件夹".into());
    }
    nodes.push(BoundNode::capture(Path::new(&root.path).join("versions"))?);
    // A missing or invalid versions entry is itself a core diagnostic. Do not
    // traverse a symlink or other foreign carrier while binding descendants.
    if nodes[1].directory.is_some() {
        nodes.push(BoundNode::capture(
            Path::new(&root.path).join("versions").join(id),
        )?);
    }
    Ok(CapturedInspection {
        id: id.into(),
        java: java_service::effective_choice(&settings, &root, id).clone(),
        extra_java_paths: settings.java_paths,
        settings_revision: settings.revision,
        preferences_revision: preferences.revision,
        root,
        tasks_revision: shared.tasks.list_snapshot().revision,
        close_generation: shared.close_generation.load(Ordering::SeqCst),
        run: shared.status.lock().unwrap().clone(),
        nodes,
    })
}

fn verify(shared: &Shared, captured: &CapturedInspection) -> Result<(), String> {
    let _operation = shared.operations.lock().unwrap();
    require_available(shared)?;
    let settings = shared.config.snapshot();
    let preferences = shared.launcher_preferences.snapshot();
    if let Some(warning) = preferences.warning {
        return Err(warning);
    }
    if settings.revision != captured.settings_revision
        || preferences.revision != captured.preferences_revision
        || shared.tasks.list_snapshot().revision != captured.tasks_revision
        || shared.close_generation.load(Ordering::SeqCst) != captured.close_generation
        || *shared.status.lock().unwrap() != captured.run
    {
        return Err(CHANGED.into());
    }
    // Always resolve the submitted ID. Using resolve(None) here would accept a
    // different active root selected during the slow Java probe.
    let root = shared.config.resolve(Some(&captured.root.id))?;
    if root != captured.root {
        return Err(CHANGED.into());
    }
    for node in &captured.nodes {
        node.verify()?;
    }
    require_files_ready(shared, &root)?;
    crate::instance_delete::ensure_name_available(Path::new(&root.path), &captured.id)
}

fn inspect_with(
    shared: &Shared,
    id: &str,
    root_id: Option<&str>,
    inspect: impl FnOnce(&CapturedInspection) -> Result<LaunchInspection, String>,
) -> Result<Inspection, String> {
    let captured = capture(shared, id, root_id)?;
    let report = inspect(&captured);
    // Validate even an unsuccessful probe so its error cannot be mistaken for
    // a current diagnosis after a concurrent settings or directory change.
    verify(shared, &captured)?;
    let report = report?;
    if report.id != captured.id {
        return Err("启动检查返回了其他实例，请重新检查".into());
    }
    Ok(Inspection {
        root_id: captured.root.id,
        report,
    })
}

pub(super) fn inspect(
    shared: &Shared,
    id: &str,
    root_id: Option<&str>,
) -> Result<Inspection, String> {
    inspect_with(shared, id, root_id, |captured| {
        pcl_core::inspection::inspect_instance(
            Path::new(&captured.root.path),
            &shared.project,
            &captured.id,
            &captured.java,
            &captured.extra_java_paths,
        )
    })
}

#[cfg(test)]
#[path = "launcher_inspection/tests.rs"]
mod tests;
