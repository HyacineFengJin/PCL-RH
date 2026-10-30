//! Java discovery and registration at the application boundary.
//!
//! The core owns executable probing and compatibility policy. Config owns the
//! persistent selections and registry. A chooser request captures a transport
//! revision and root ID; both are rechecked after the dialog and process probe,
//! because either can outlive navigation or another settings mutation.
//! Slow probes never hold the shared operation mutex.

use crate::{
    config::{GameRoot, Settings},
    require_instance_job, require_reference_write, Shared,
};
use pcl_core::java::{JavaCatalog, JavaSelection};
use std::path::{Path, PathBuf};

pub(super) struct RegistrationContext {
    pub root_id: String,
    pub revision: String,
    pub initial_directory: PathBuf,
}

pub(super) fn catalog(shared: &Shared, root_id: Option<&str>) -> Result<JavaCatalog, String> {
    let (settings, roots) = shared.config.view();
    let root_id = root_id.unwrap_or(&settings.root_id);
    let registered = roots
        .iter()
        .find(|root| root.id == root_id)
        .ok_or("游戏目录未注册或已移除")?;
    // Offline roots retain their settings. Java management can still show the
    // project/system runtimes without treating an unavailable root as a new one.
    let root = if registered.available {
        Path::new(&registered.path).canonicalize().ok()
    } else {
        None
    };
    pcl_core::java::catalog(&shared.project, root.as_deref(), &settings.java_paths)
}

pub(super) fn effective_choice<'a>(
    settings: &'a Settings,
    root: &'a GameRoot,
    id: &str,
) -> &'a JavaSelection {
    root.java_overrides.get(id).unwrap_or(&settings.java)
}

pub(super) fn registration_context(
    shared: &Shared,
    root_id: Option<&str>,
    revision: &str,
) -> Result<RegistrationContext, String> {
    let _operation = shared.operations.lock().unwrap();
    require_instance_job(shared)?;
    require_reference_write(shared)?;
    let settings = shared.config.snapshot();
    if settings.revision != revision || root_id.is_some_and(|id| id != settings.root_id) {
        return Err("设置或游戏目录已改变，请刷新后重新添加 Java".into());
    }
    let initial_directory = match &settings.java {
        JavaSelection::Manual { path } => Path::new(path).parent().map(Path::to_path_buf),
        JavaSelection::Auto => None,
    }
    .filter(|path| path.is_dir())
    .or_else(pcl_core::java::system_java_directory)
    .unwrap_or_else(|| shared.project.clone());
    Ok(RegistrationContext {
        root_id: settings.root_id,
        revision: settings.revision,
        initial_directory,
    })
}

pub(super) fn register(
    shared: &Shared,
    path: &Path,
    context: &RegistrationContext,
) -> Result<Settings, String> {
    // Reject stale requests before launching even a bounded probe. Recheck after
    // probing too: the process must not make a stale chooser result authoritative.
    registration_context(shared, Some(&context.root_id), &context.revision)?;
    let runtime = pcl_core::java::inspect(path)?;
    let _operation = shared.operations.lock().unwrap();
    require_instance_job(shared)?;
    require_reference_write(shared)?;
    shared
        .config
        .register_java(runtime.path, &context.revision, &context.root_id)
}
