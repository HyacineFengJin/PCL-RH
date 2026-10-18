//! Native submissions bind a provider identity and a private project directory.
//! The queue owns cancellation through cleanup; registry additions merge into
//! current config after publication and cannot replay stale game selections.
use crate::{java_download, tasks, Shared};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn java_download_catalog(
    state: State<'_, Arc<Shared>>,
) -> Result<Vec<java_download::Package>, String> {
    if state.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    java_download::list().await
}
#[tauri::command]
pub fn java_download_start(
    component: String,
    sha1: String,
    revision: String,
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let shared = state.inner().clone();
    let _operation = shared.operations.lock().unwrap();
    crate::require_network_submission(&shared)?;
    java_download::validate_request(&component, &sha1)?;
    let settings = shared.config.snapshot();
    if settings.revision != revision || root_id.as_ref().is_some_and(|id| id != &settings.root_id) {
        return Err("设置或游戏目录已改变，请刷新后重新下载 Java".into());
    }
    let store_binding = java_download::StoreBinding::capture(&shared.project)?;
    let scope = tasks::TaskScope::managed_runtime_store(&shared.project)?;
    store_binding.verify()?;
    let store_path = shared.project.join(".pcl-rust/java");
    let http = java_download::http(pcl_network::snapshot(), pcl_network::download_snapshot())?;
    let task = Arc::new(shared.tasks.admit_queued(
        tasks::TaskTarget {
            root_id: "launcher".into(),
            root_path: store_path.display().to_string(),
            instance_id: None,
        },
        tasks::TaskKind::JavaInstall,
        scope,
    )?);
    shared.downloads.track(&task);
    let id = task.id().to_string();
    let worker_task = task.clone();
    let worker_shared = shared.clone();
    let spawn = std::thread::Builder::new()
        .name(format!("pcl-java-{id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker_task.wait_turn()?;
                let store = store_binding.open()?;
                tauri::async_runtime::block_on(java_download::install(
                    &store,
                    &component,
                    &sha1,
                    http,
                    &worker_task,
                ))
            }))
            .unwrap_or_else(|_| {
                Err("Java 下载工作线程意外退出，暂存文件会在下次下载时恢复清理".into())
            });
            match result {
                Ok(installed) => {
                    let warning = {
                        let _operation = worker_shared.operations.lock().unwrap();
                        worker_shared
                            .config
                            .register_downloaded_java(installed.path.display().to_string())
                            .err()
                    };
                    worker_task.finish(tasks::TaskOutcome::Complete {
                        result: Some(serde_json::json!({"java":installed.runtime})),
                        message: "Java 安装完成".into(),
                        error: warning
                            .map(|e| format!("Java 已安装，登记失败；可从列表选择或手动添加：{e}")),
                    });
                }
                Err(error) => {
                    worker_task.finish(
                        if matches!(error.as_str(), java_download::CANCELLED | tasks::CANCELLED) {
                            tasks::TaskOutcome::Failed(error)
                        } else {
                            tasks::TaskOutcome::Error(error)
                        },
                    );
                }
            }
        });
    if spawn.is_err() {
        task.finish(tasks::TaskOutcome::Error(
            "无法启动 Java 下载工作线程".into(),
        ));
        return Err("无法启动 Java 下载工作线程".into());
    }
    Ok(serde_json::json!({"id":id}))
}
