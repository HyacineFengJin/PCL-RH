//! Native destination choice for a read-only resource snapshot. Root IDs are
//! resolved before preparation and re-resolved after the chooser; hashing never
//! holds writer admission. The short final guard binds the selected root and
//! source stamps through publication, without changing any resource file.
use crate::{
    local_resource_info as info,
    resource_ops::ResourceFile,
    toolbox_images::publish::{self, ExportOutcome},
    Shared,
};
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};
use tauri::State;
#[tauri::command]
pub async fn local_resource_info_export(
    root_id: Option<String>,
    id: String,
    kind: String,
    files: Vec<ResourceFile>,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<ExportOutcome, String> {
    if !info::supported(&kind, &files) {
        return Ok(ExportOutcome::unavailable(
            "当前选择中有不支持导出信息的项目；请选择有指纹的模组、资源包或光影文件",
        ));
    }
    let shared = state.inner().clone();
    let initial = {
        let _admission = shared.operations.lock().unwrap();
        if shared.closing.load(Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        pcl_core::identifier(&id)?;
        shared.config.isolation_settings(root_id.as_deref())?
    };
    let worker = shared.clone();
    let captured = initial.registered_path.clone();
    let policy = initial.policy(&id);
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        info::prepare_with_isolation(
            Path::new(&captured),
            &id,
            &kind,
            &files,
            &worker.closing,
            policy,
        )
    })
    .await
    .map_err(|_| "资源信息读取任务意外退出".to_string())??;
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    let choice = shared
        .desktop
        .choose_launcher_file(window, shared.project.clone(), "export_resource_info")
        .await?;
    if choice.status == "cancelled" {
        return Ok(ExportOutcome::cancelled());
    }
    let target = choice.paths.into_iter().next().ok_or_else(|| {
        choice
            .message
            .unwrap_or_else(|| "未选择有效的 JSON 保存位置".into())
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        prepared.rehash(&shared.closing)?;
        let mut result =
            publish::publish_new(&target, &prepared.bytes, "json", prepared.count, || {
                let guard = shared.operations.lock().unwrap();
                if shared.closing.load(Ordering::SeqCst) {
                    return Err("启动器正在关闭".into());
                }
                let current = shared.config.isolation_settings(Some(&initial.root.id))?;
                if current.revision != initial.revision || current.root != initial.root {
                    return Err("所选游戏目录已变化，请重新导出".into());
                }
                prepared.check()?;
                Ok(guard)
            })?;
        if let Some(warning) = prepared.warning {
            result.warning = Some(match result.warning {
                Some(existing) => format!("{existing}；{warning}"),
                None => warning,
            });
        }
        Ok(result)
    })
    .await
    .map_err(|_| "资源信息导出任务意外退出".to_string())?
}
