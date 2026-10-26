//! Common export task admission, chooser and publication. Official shortcut
//! plugins supply checked native plans and retain their call lease here; the
//! existing manual export keeps its independent public command contract.
use crate::{config::IsolationSettings, instance_export, tasks, Shared};
use std::sync::{atomic::Ordering, Arc};

pub(crate) async fn start_checked<G: Send + 'static>(
    shared: Arc<Shared>,
    window: tauri::WebviewWindow,
    root: IsolationSettings,
    checked: instance_export::ExportPlan,
    keep_alive: G,
) -> Result<Option<String>, String> {
    let filename = format!("{}-{}.zip", checked.request.name, checked.request.version)
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let choice = shared
        .desktop
        .save_zip(window, shared.project.clone(), filename)
        .await?;
    if choice.status == "cancelled" {
        return Ok(None);
    }
    if choice.status != "selected" {
        return Err(choice
            .message
            .unwrap_or_else(|| "无法选择导出文件位置".into()));
    }
    let destination = choice
        .paths
        .into_iter()
        .next()
        .ok_or("未选择导出文件位置")?;
    let s = shared;
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = s.operations.lock().unwrap();
        crate::require_instance_job(&s)?;
        let current = s.config.isolation_settings(Some(&root.root.id))?;
        crate::ensure_instance_files_ready(&s, &current.root)?;
        if current.revision != root.revision || current.root.path != root.root.path {
            return Err("游戏目录或实例设置已改变，请重新导出".into());
        }
        let task = s.tasks.admit(
            tasks::TaskTarget {
                root_id: root.root.id.clone(),
                root_path: root.root.path.clone(),
                instance_id: Some(checked.instance_id.clone()),
            },
            tasks::TaskKind::InstanceExport,
        )?;
        let task_id = task.id().to_owned();
        s.downloads.track(&task);
        let worker = s.clone();
        std::thread::Builder::new()
            .name(format!("pcl-export-{task_id}"))
            .spawn(move || {
                let _keep_alive = keep_alive;
                let cancel = task.cancellation_token();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let instance_id = checked.instance_id.clone();
                    instance_export::execute_with_guard(
                        checked,
                        &destination,
                        &cancel,
                        |p| worker.downloads.progress(&task, p),
                        || {
                            let operation = worker.operations.lock().unwrap();
                            if worker.closing.load(Ordering::SeqCst) {
                                return Err("启动器正在关闭".into());
                            }
                            let current = worker.config.isolation_settings(Some(&root.root.id))?;
                            if current.revision != root.revision
                                || current.root.path != root.root.path
                                || current.policy(&instance_id) != root.policy(&instance_id)
                            {
                                return Err("实例设置已改变，请重新导出".into());
                            }
                            Ok(operation)
                        },
                    )
                }))
                .unwrap_or_else(|_| {
                    Err("取消清理失败：导出任务意外退出，请检查保存目录中的临时文件".into())
                });
                let message = result
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("file_name"))
                    .and_then(|v| v.as_str())
                    .map(|name| format!("导出完成：{name}"))
                    .unwrap_or_else(|| "导出完成".into());
                crate::finish_instance_task(task, result, &message);
            })
            .map_err(|e| format!("无法启动导出任务：{e}"))?;
        Ok(Some(task_id))
    })
    .await
    .map_err(|_| "启动导出任务失败".to_string())?
}
