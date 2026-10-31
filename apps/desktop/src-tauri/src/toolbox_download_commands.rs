//! Commands bind chooser/session authority and task lifetime. Service workers
//! receive immutable network policies and descriptor ownership; IPC completion
//! never controls cancellation cleanup or no-replacement publication.
use crate::{tasks, toolbox_download::*, Shared};
use std::sync::{atomic::Ordering, Arc};
#[cfg(target_os = "linux")]
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tauri::State;

/// Own prepared URL/chooser authority before waiting. A cancelled queued turn
/// drops this argument and the work closure before the caller emits terminal.
pub(super) fn with_download_turn<T>(
    task: &tasks::TaskHandle,
    prepared: PreparedDownload,
    work: impl FnOnce(PreparedDownload) -> Result<T, String>,
) -> Result<T, String> {
    task.wait_turn()?;
    work(prepared)
}
#[tauri::command]
pub async fn toolbox_download_choose(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<DirectoryView>, String> {
    let shared = state.inner().clone();
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    let (generation, previous) = shared.toolbox_download.begin_choose();
    // This only seeds the real folder dialog. It is never download authority.
    let choice = shared
        .desktop
        .choose_toolbox_directory(window, previous.unwrap_or_else(|| shared.project.clone()))
        .await?;
    if choice.status == "cancelled" {
        return Ok(None);
    }
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    if choice.paths.len() != 1 {
        return Err(choice
            .message
            .unwrap_or_else(|| "未选择一个有效的下载目录".into()));
    }
    let path = choice.paths.into_iter().next().unwrap();
    Ok(Some(
        shared.toolbox_download.adopt_directory(generation, path)?,
    ))
}
#[tauri::command]
pub fn toolbox_download_prepare(
    request: PrepareRequest,
    state: State<'_, Arc<Shared>>,
) -> Result<DownloadPreview, String> {
    let shared = state.inner();
    let _operation = shared.operations.lock().unwrap();
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    let preferences = shared.launcher_preferences.snapshot();
    shared
        .toolbox_download
        .prepare(request, preferences.revision)
}
#[tauri::command]
pub fn toolbox_download_start(
    token: String,
    state: State<'_, Arc<Shared>>,
) -> Result<serde_json::Value, String> {
    let shared = state.inner().clone();
    let _operation = shared.operations.lock().unwrap();
    crate::require_network_submission(&shared)?;
    let preferences = shared.launcher_preferences.snapshot();
    let pending = shared
        .toolbox_download
        .start_guard(&token, &preferences.revision)?;
    let task = shared.tasks.admit_queued(
        tasks::TaskTarget {
            root_id: "launcher".into(),
            root_path: pending.directory().display().to_string(),
            instance_id: None,
        },
        tasks::TaskKind::ToolboxDownload,
        tasks::TaskScope::files(&[pending.target()])?,
    )?;
    let prepared = pending.take();
    let network = pcl_network::snapshot();
    let scheduler = pcl_network::download_snapshot();
    shared.downloads.track(&task);
    let task_id = task.id().to_string();
    std::thread::Builder::new()
        .name(format!("pcl-toolbox-download-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_download_turn(&task, prepared, |prepared| {
                    let cancel = task.cancellation_token();
                    let mut gate = || {
                        task.begin_finishing();
                        if cancel.load(Ordering::SeqCst) {
                            Err(CANCELLED.into())
                        } else {
                            Ok(())
                        }
                    };
                    tauri::async_runtime::block_on(download(
                        prepared,
                        network,
                        scheduler,
                        &cancel,
                        |progress| {
                            task.update(tasks::TaskProgress {
                                stage: if progress.phase == "downloading" {
                                    tasks::TaskStage::Downloading
                                } else {
                                    tasks::TaskStage::Processing
                                },
                                phase: progress.phase,
                                message: progress.message,
                                completed: 0,
                                total: 1,
                                bytes_done: progress.bytes_done,
                                bytes_total: progress.bytes_total,
                                network_bytes: progress.network_bytes,
                                steps: Vec::new(),
                            });
                        },
                        &mut gate,
                    ))
                })
            }))
            .unwrap_or_else(|_| Err("工具箱下载工作线程意外退出，请检查所选目录".into()));
            match result {
                Ok(result) => {
                    let warning = result.warning.clone();
                    task.finish(tasks::TaskOutcome::Complete {
                        result: serde_json::to_value(result).ok(),
                        message: "工具箱文件已保存".into(),
                        error: warning,
                    });
                }
                Err(error) => {
                    task.finish(if error == CANCELLED || error == tasks::CANCELLED {
                        tasks::TaskOutcome::Failed(error)
                    } else {
                        tasks::TaskOutcome::Error(error)
                    });
                }
            }
        })
        .map_err(|_| "无法启动工具箱下载工作线程")?;
    Ok(serde_json::json!({"id":task_id}))
}
#[tauri::command]
pub async fn toolbox_download_open_directory(
    directory_token: String,
    state: State<'_, Arc<Shared>>,
) -> Result<(), String> {
    let shared = state.inner().clone();
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭".into());
    }
    let directory = shared.toolbox_download.held_directory(&directory_token)?;
    tauri::async_runtime::spawn_blocking(move || {
        // The command cannot open an arbitrary frontend path. Keep its captured
        // descriptor alive while the desktop opener accepts this path.
        directory.recheck()?;
        #[cfg(not(target_os = "linux"))]
        {
            pcl_desktop_platform::opener::open_path(directory.path())?;
            return directory.recheck();
        }
        #[cfg(target_os = "linux")]
        {
            let mut child = Command::new("xdg-open")
                .arg(directory.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| "无法打开所选下载目录")?;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return if status.success() {
                            Ok(())
                        } else {
                            Err("默认文件管理器未能打开所选目录".into())
                        }
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(40))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("打开所选目录超时或失败".into());
                    }
                }
            }
        }
    })
    .await
    .map_err(|_| "打开目录工作线程意外退出")?
}
