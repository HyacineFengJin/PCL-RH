//! The saved manual value is independent of the selected mode. Automatic
//! allocation reads current RAM and the selected content directory only when
//! requested; launch calls this after Java/file preparation, just before argv
//! construction. A renderer preview is never authority for a later launch.
use crate::{
    config::{GameRoot, Settings},
    Shared,
};
use pcl_core::{
    content_scope::IsolationPolicy,
    memory::{self, MemoryMode, MemorySnapshot},
};
use serde::Serialize;
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};
use tauri::State;

#[cfg(test)]
use std::fs;
const MAX_MOD_ENTRIES: usize = 8192;
const CHANGED: &str = "内存预览期间设置或游戏目录已变化，请刷新后重试";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MemoryPreview {
    pub root_id: String,
    pub instance_id: Option<String>,
    pub settings_revision: String,
    pub mode: MemoryMode,
    pub memory_gib: u32,
    pub manual_gib: u32,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    pub mod_count: Option<u32>,
    pub constrained: bool,
    pub error: Option<String>,
}

fn system_memory() -> Result<MemorySnapshot> {
    let system = crate::ui_data::system_info()?;
    Ok(MemorySnapshot {
        total_bytes: system.total_memory_bytes,
        available_bytes: system.available_memory_bytes,
    })
}

#[cfg(target_os = "linux")]
#[path = "launcher_memory/filesystem_linux.rs"]
mod filesystem;
#[cfg(not(target_os = "linux"))]
#[path = "launcher_memory/filesystem_native.rs"]
mod filesystem;
use filesystem::enabled_mods;

fn evaluate_using(
    settings: &Settings,
    root: &GameRoot,
    id: Option<&str>,
    inputs: impl FnOnce() -> Result<(MemorySnapshot, u32)>,
) -> MemoryPreview {
    let mode = id.map_or(settings.memory_mode, |id| {
        root.resolved_memory_mode(id, settings.memory_mode)
    });
    let manual_gib = id
        .and_then(|id| root.overrides.get(id).copied())
        .unwrap_or(settings.memory_gib);
    let mut preview = MemoryPreview {
        root_id: root.id.clone(),
        instance_id: id.map(str::to_owned),
        settings_revision: settings.revision.clone(),
        mode,
        memory_gib: manual_gib,
        manual_gib,
        total_memory_bytes: None,
        available_memory_bytes: None,
        mod_count: None,
        constrained: false,
        error: None,
    };
    if mode == MemoryMode::Auto {
        // Zero with an error is deliberately not a fallback allocation. The UI
        // displays no GiB estimate and actual launch returns the same error.
        preview.memory_gib = 0;
        let result = inputs().and_then(|(snapshot, mods)| {
            preview.total_memory_bytes = Some(snapshot.total_bytes);
            preview.available_memory_bytes = Some(snapshot.available_bytes);
            preview.mod_count = Some(mods);
            memory::recommend(snapshot, mods)
        });
        match result {
            Ok(recommendation) => {
                preview.memory_gib = recommendation.memory_gib;
                preview.constrained = recommendation.constrained;
            }
            Err(error) => preview.error = Some(error),
        }
    }
    preview
}

fn evaluate(
    settings: &Settings,
    root: &GameRoot,
    id: Option<&str>,
    isolation: IsolationPolicy,
) -> MemoryPreview {
    evaluate_using(settings, root, id, || {
        let mods = id.map_or(Ok(0), |id| {
            enabled_mods(Path::new(&root.path), id, isolation)
        })?;
        Ok((system_memory()?, mods))
    })
}

pub(crate) fn allocation(
    settings: &Settings,
    root: &GameRoot,
    id: &str,
    isolation: IsolationPolicy,
) -> Result<u32> {
    let preview = evaluate(settings, root, Some(id), isolation);
    match preview.error {
        Some(error) => Err(error),
        None => Ok(preview.memory_gib),
    }
}

fn preview_with(
    shared: &Shared,
    id: Option<&str>,
    root_id: Option<&str>,
    read: impl FnOnce(&Settings, &GameRoot, Option<&str>, IsolationPolicy) -> MemoryPreview,
) -> Result<MemoryPreview> {
    let (settings, root, generation) = {
        let _operation = shared.operations.lock().unwrap();
        if shared.closing.load(Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        let settings = shared.config.checked_snapshot()?;
        let root = shared
            .config
            .registered(root_id.unwrap_or(&settings.root_id))?;
        if let Some(id) = id {
            pcl_core::identifier(id)?;
        }
        (
            settings,
            root,
            shared.close_generation.load(Ordering::SeqCst),
        )
    };
    let isolation = id.map_or(settings.isolation, |id| {
        root.resolved_isolation(id, settings.isolation)
    });
    let preview = read(&settings, &root, id, isolation);
    let _operation = shared.operations.lock().unwrap();
    if shared.closing.load(Ordering::SeqCst)
        || shared.close_generation.load(Ordering::SeqCst) != generation
        || shared.config.checked_snapshot()?.revision != settings.revision
        || shared.config.registered(&root.id)? != root
    {
        return Err(CHANGED.into());
    }
    Ok(preview)
}

#[tauri::command]
pub(crate) async fn memory_preview(
    id: Option<String>,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<MemoryPreview> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        preview_with(&shared, id.as_deref(), root_id.as_deref(), evaluate)
    })
    .await
    .map_err(|_| "内存预览任务意外退出".to_owned())?
}

#[cfg(all(test, windows))]
#[path = "../../../../crates/core/src/file_ops/junction_fixture.rs"]
mod junction_fixture;
#[cfg(test)]
#[path = "launcher_memory/tests.rs"]
mod tests;
