//! Desktop binding and task lifetime for Modrinth resource installation.
//!
//! The UI submits provider IDs, an exact file selection and a confirmation
//! revision. Network metadata, compatibility and file publication stay in the
//! planner/transfer/batch services. Read-only preparation releases operations;
//! start admits a queued root writer before any network work. Each worker waits
//! outside operations, then repeats binding/recovery checks before metadata;
//! cancellation and shutdown own the full queued, transfer and cleanup lifetime.

use crate::{config::GameRoot, modrinth_install, resource_ops, tasks, Shared};
use pcl_install::InstallStep;
use serde_json::{json, Value};
use std::{path::Path, sync::atomic::Ordering, sync::Arc};
use tauri::State;

type Result<T> = std::result::Result<T, String>;

/// Called under operations, before writer admission. Plans always address a
/// physical, non-reserved instance in the explicitly registered root.
pub(super) fn capture(shared: &Shared, root_id: Option<&str>, id: &str) -> Result<GameRoot> {
    crate::require_instance_job(shared)?;
    pcl_core::identifier(id)?;
    let root = shared.config.resolve(root_id)?;
    ready(shared, &root, id)?;
    Ok(root)
}

fn ready(shared: &Shared, root: &GameRoot, id: &str) -> Result<()> {
    crate::ensure_instance_files_ready(shared, root)?;
    let path = Path::new(&root.path);
    crate::instance_delete::ensure_name_available(path, id)?;
    crate::instance_reset::ensure_ready(path)?;
    resource_ops::ensure_ready(path)?;
    Ok(())
}

/// Submission checks the registered root and global reference store. Root-local
/// recovery/reservation checks belong to the turn: another legitimate network
/// writer may currently own its temporary publication journal in this root.
fn capture_submission(shared: &Shared, root_id: Option<&str>, id: &str) -> Result<GameRoot> {
    crate::require_network_submission(shared)?;
    pcl_core::identifier(id)?;
    shared.config.resolve(root_id)
}

fn worker_ready(
    shared: &Shared,
    root: &GameRoot,
    id: &str,
    task: &tasks::TaskHandle,
) -> Result<()> {
    let _operation = shared.operations.lock().unwrap();
    if task.cancellation_token().load(Ordering::SeqCst) {
        return Err(tasks::CANCELLED.into());
    }
    crate::require_network_submission(shared)?;
    same_root(shared, root)?;
    ready(shared, root, id)
}

/// The owned work closure is consumed even when a queued cancellation returns
/// early, so its captured resources close before the caller finishes the task.
fn with_worker_turn<T>(
    shared: &Shared,
    root: &GameRoot,
    id: &str,
    task: &tasks::TaskHandle,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    task.wait_turn()?;
    worker_ready(shared, root, id, task)?;
    work()
}

pub(super) fn same_root(shared: &Shared, root: &GameRoot) -> Result<()> {
    if shared.config.resolve(Some(&root.id))?.path != root.path {
        return Err("资源安装目标目录已改变，请重新检查".into());
    }
    Ok(())
}

pub(super) fn verify_isolation(
    shared: &Shared,
    root: &GameRoot,
    id: &str,
    policy: pcl_core::content_scope::IsolationPolicy,
    revision: &str,
) -> Result<()> {
    let current = shared.config.isolation_settings(Some(&root.id))?;
    if current.root != *root
        || current.policy(id) != policy
        || (!revision.is_empty() && current.revision != revision)
    {
        return Err("实例隔离设置已变化，请重新检查资源方案".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn resource_install_plan(
    root_id: Option<String>,
    id: String,
    request: modrinth_install::InstallRequest,
    state: State<'_, Arc<Shared>>,
) -> Result<modrinth_install::InstallPlan> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let isolation = {
            let _operation = shared.operations.lock().unwrap();
            let root = capture_submission(&shared, root_id.as_deref(), &id)?;
            ready(&shared, &root, &id)?;
            shared.config.isolation_settings(Some(&root.id))?
        };
        let root = &isolation.root;
        // This confirmation read has no file writer. Start repeats the full
        // authoritative plan under its own admitted cancellation lifetime.
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let prepared_request = request.clone();
        let plan = tauri::async_runtime::block_on(modrinth_install::prepare_with_isolation(
            Path::new(&root.path),
            &shared.project,
            &root.id,
            &id,
            request,
            &cancel,
            isolation.policy(&id),
            &isolation.revision,
        ))?;
        modrinth_install::recheck_target(&plan, &cancel)?;
        let _operation = shared.operations.lock().unwrap();
        same_root(&shared, root)?;
        verify_isolation(
            &shared,
            root,
            &id,
            isolation.policy(&id),
            &isolation.revision,
        )?;
        crate::require_network_submission(&shared)?;
        ready(&shared, &root, &id)?;
        shared
            .resource_confirmations
            .remember(prepared_request, plan)
    })
    .await
    .map_err(|_| "资源安装方案检查意外退出".to_string())?
}

#[tauri::command]
pub fn resource_install_start(
    root_id: Option<String>,
    id: String,
    request: modrinth_install::InstallRequest,
    revision: String,
    state: State<'_, Arc<Shared>>,
) -> Result<Value> {
    start_request(
        state.inner().clone(),
        root_id.as_deref(),
        id,
        request,
        revision,
    )
}

/// Native and integration callers share this admission path. Only the worker
/// owns the captured root and task; browsing a different root cannot retarget it.
pub(super) fn start_request(
    shared: Arc<Shared>,
    root_id: Option<&str>,
    id: String,
    request: modrinth_install::InstallRequest,
    revision: String,
) -> Result<Value> {
    let _operation = shared.operations.lock().unwrap();
    let root = capture_submission(&shared, root_id, &id)?;
    if revision.is_empty() || revision.len() > 256 {
        return Err("缺少有效的资源安装方案，请重新检查".into());
    }
    let confirmed = shared.resource_confirmations.claim(
        Path::new(&root.path),
        &shared.project,
        &root.id,
        &id,
        &request,
        &revision,
    )?;
    let isolation = shared.config.isolation_settings(Some(&root.id))?;
    confirmed.require_isolation(isolation.policy(&id), &isolation.revision)?;
    let task = shared
        .tasks
        .admit_queued(
            tasks::TaskTarget {
                root_id: root.id.clone(),
                root_path: root.path.clone(),
                instance_id: Some(id.clone()),
            },
            tasks::TaskKind::ResourceDownload,
            tasks::TaskScope::root(Path::new(&root.path))?,
        )
        .map_err(|error| format!("{error}；此确认已领取，请重新检查资源方案"))?;
    let task_id = task.id().to_owned();
    if let Some(name) = confirmed.resource_name() {
        task.set_resource_name(name);
    }
    shared.downloads.track(&task);
    let download_policy = pcl_network::download_snapshot();
    let worker = shared.clone();
    std::thread::Builder::new()
        .name(format!("pcl-resource-{task_id}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_worker_turn(&worker, &root, &id, &task, || {
                    install_with_policy(&worker, &root, &id, confirmed, &task, download_policy)
                })
            }))
            .unwrap_or_else(|_| Err("资源安装意外退出，请检查未完成的资源安装恢复记录".into()));
            finish(task, result);
        })
        .map_err(|error| format!("无法启动资源安装任务：{error}；请重新检查资源方案"))?;
    Ok(json!({"id":task_id}))
}

fn install_with_policy(
    shared: &Shared,
    root: &GameRoot,
    id: &str,
    confirmed: modrinth_install::ConfirmedInstall,
    task: &tasks::TaskHandle,
    download_policy: Arc<pcl_network::DownloadScheduler>,
) -> Result<Value> {
    let cancel = task.cancellation_token();
    let isolation = shared.config.isolation_settings(Some(&root.id))?;
    confirmed.require_isolation(isolation.policy(id), &isolation.revision)?;
    let project_id = confirmed.request().project_id.clone();
    task.update(tasks::TaskProgress {
        stage: tasks::TaskStage::Preparing,
        phase: "resource-metadata".into(),
        message: "正在获取资源与必需前置信息…".into(),
        completed: 0,
        total: 0,
        bytes_done: 0,
        bytes_total: 0,
        network_bytes: 0,
        steps: steps(0, None),
    });
    let batch = tauri::async_runtime::block_on(modrinth_install::download_confirmed_with_policy(
        confirmed,
        download_policy,
        &cancel,
        |plan| {
            if let Some(file) = plan.files.iter().find(|file| file.project_id == project_id) {
                task.set_resource_name(&file.title);
            }
        },
        |p| transfer_progress(task, p),
    ))?;
    let modrinth_install::VerifiedBatch {
        plan,
        files,
        network_bytes,
    } = batch;
    let mut files = files
        .into_iter()
        .map(|file| resource_ops::VerifiedImport {
            kind: file.kind,
            file_name: file.file_name,
            file: file.file,
            size: file.size,
            sha512: file.sha512,
        })
        .collect::<Vec<_>>();
    let mut staging = publication_progress(&plan, network_bytes);
    staging.phase = "resource-staging".into();
    staging.message = "下载已校验，正在暂存并复核资源文件…".into();
    task.update(staging);
    let mut commit = || {
        task.begin_finishing();
        if cancel.load(Ordering::SeqCst) {
            return Err(modrinth_install::CANCELLED.into());
        }
        // Batch staging is outside the inventory. Check before the publisher
        // creates target directories or changes resource files. Do not use the
        // combined batch guard here: it would reject this task's own journal.
        modrinth_install::recheck_target(&plan, &cancel)?;
        let _operation = shared.operations.lock().unwrap();
        same_root(shared, root)?;
        let (policy, revision) = modrinth_install::install_isolation(&plan);
        verify_isolation(shared, root, id, policy, revision)?;
        crate::ensure_instance_files_ready(shared, root)?;
        crate::instance_delete::ensure_name_available(Path::new(&root.path), id)?;
        crate::instance_reset::ensure_ready(Path::new(&root.path))?;
        resource_ops::ensure_local_resources_ready(Path::new(&root.path))?;
        task.update(publication_progress(&plan, network_bytes));
        Ok(())
    };
    let result = if files.is_empty() {
        // Reused resources still need the commit-time scope and compatibility
        // check, but must never create an empty batch or enable a disabled mod.
        commit()?;
        resource_ops::MutationResult {
            changed: 0,
            undo_id: None,
            message: "所需资源文件已存在并通过校验".into(),
        }
    } else {
        resource_ops::import_verified_batch_with_isolation(
            Path::new(&root.path),
            id,
            &mut files,
            &cancel,
            &mut commit,
            &mut |_, _| {},
            modrinth_install::install_isolation(&plan).0,
        )?
    };
    let mut done = publication_progress(&plan, network_bytes);
    done.message = "资源文件已安装，暂存文件已清理".into();
    done.steps = steps(3, None);
    task.update(done);
    Ok(json!({
        "id":id,"changed":result.changed,
        "reused":plan.files.iter().filter(|file|file.reused).count(),
        "warnings":plan.warnings,"message":result.message,
    }))
}

fn steps(current: usize, progress: Option<f64>) -> Vec<InstallStep> {
    [
        ("resource-metadata", "获取资源与前置信息"),
        ("resource-download", "下载并校验文件"),
        ("resource-publish", "安装资源文件"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (id, label))| InstallStep {
        id: id.into(),
        label: label.into(),
        state: if index < current {
            "complete"
        } else if index == current {
            "running"
        } else {
            "pending"
        }
        .into(),
        progress: (index == current).then_some(progress).flatten(),
    })
    .collect()
}

fn transfer_progress(task: &tasks::TaskHandle, progress: modrinth_install::DownloadProgress) {
    let metadata = progress.phase == "metadata";
    let ratio = (progress.bytes_total > 0)
        .then(|| progress.bytes_done as f64 / progress.bytes_total as f64);
    task.update(tasks::TaskProgress {
        stage: if metadata {
            tasks::TaskStage::Preparing
        } else {
            tasks::TaskStage::Downloading
        },
        phase: format!("resource-{}", progress.phase),
        message: progress.message,
        completed: progress.completed,
        total: progress.total,
        bytes_done: progress.bytes_done,
        bytes_total: progress.bytes_total,
        network_bytes: progress.network_bytes,
        steps: if metadata {
            steps(0, None)
        } else {
            steps(1, ratio)
        },
    });
}

fn publication_progress(
    plan: &modrinth_install::InstallPlan,
    network_bytes: u64,
) -> tasks::TaskProgress {
    tasks::TaskProgress {
        stage: tasks::TaskStage::Processing,
        phase: "resource-publish".into(),
        message: "文件已校验，正在提交资源文件…".into(),
        completed: plan.files.len() as u64,
        total: plan.files.len() as u64,
        bytes_done: plan.download_bytes,
        bytes_total: plan.download_bytes,
        network_bytes,
        steps: steps(2, None),
    }
}

fn finish(task: tasks::TaskHandle, result: Result<Value>) {
    match result {
        Ok(value) => {
            task.finish(tasks::TaskOutcome::Complete {
                message: "资源安装完成".into(),
                result: Some(value),
                error: None,
            });
        }
        Err(error) if error == modrinth_install::CANCELLED || error == tasks::CANCELLED => {
            task.finish(tasks::TaskOutcome::Failed(error));
        }
        Err(error) => {
            // Cancelling a transfer is explicit. Hash/conflict/rollback errors
            // keep Error even if a cancellation request arrived at the same time.
            task.finish(tasks::TaskOutcome::Error(error));
        }
    }
}

#[tauri::command]
pub async fn resource_install_recover(
    root_id: Option<String>,
    state: State<'_, Arc<Shared>>,
) -> Result<resource_ops::MutationResult> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (root, task) = {
            let _operation = shared.operations.lock().unwrap();
            crate::require_instance_job(&shared)?;
            let root = shared.config.resolve(root_id.as_deref())?;
            crate::ensure_instance_files_ready(&shared, &root)?;
            crate::instance_reset::ensure_ready(Path::new(&root.path))?;
            resource_ops::ensure_local_resources_ready(Path::new(&root.path))?;
            let task = shared.tasks.admit(
                tasks::TaskTarget {
                    root_id: root.id.clone(),
                    root_path: root.path.clone(),
                    instance_id: None,
                },
                tasks::TaskKind::ResourceOperation,
            )?;
            task.begin_finishing();
            (root, task)
        };
        let result = resource_ops::recover_verified_batches(Path::new(&root.path));
        finish(
            task,
            result
                .as_ref()
                .map(|value| json!(value))
                .map_err(Clone::clone),
        );
        result
    })
    .await
    .map_err(|_| "资源安装恢复意外退出，恢复记录已保留".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::Duration,
    };

    struct Loopback {
        url: String,
        stop: Arc<std::sync::atomic::AtomicBool>,
        requests: Arc<std::sync::Mutex<Vec<String>>>,
        worker: Option<thread::JoinHandle<()>>,
    }
    impl Loopback {
        fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
            let signal = stop.clone();
            let seen = requests.clone();
            let worker = thread::spawn(move || {
                while !signal.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            socket
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut bytes = [0; 4096];
                            let count = socket.read(&mut bytes).unwrap();
                            let path = String::from_utf8_lossy(&bytes[..count])
                                .split_whitespace()
                                .nth(1)
                                .unwrap()
                                .to_string();
                            seen.lock().unwrap().push(path);
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture").unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("fixture listener failed: {error}"),
                    }
                }
            });
            Self {
                url,
                stop,
                requests,
                worker: Some(worker),
            }
        }
    }
    impl Drop for Loopback {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    fn fake_http_worker(
        shared: Arc<Shared>,
        root: GameRoot,
        task: tasks::TaskHandle,
        url: String,
        file: &str,
    ) -> (thread::JoinHandle<()>, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (ready, observed) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let file = file.to_string();
        let worker = thread::spawn(move || {
            let result = with_worker_turn(&shared, &root, "Sample", &task, || {
                // This controlled transport exercises the production worker
                // boundary; it grants no production URL/source authority.
                let body = reqwest::blocking::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(2))
                    .build()
                    .unwrap()
                    .get(url)
                    .send()
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .bytes()
                    .unwrap();
                fs::write(Path::new(&root.path).join(file), &body).unwrap();
                ready.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(json!({"bytes":body.len()}))
            });
            finish(task, result);
        });
        (worker, observed, release)
    }

    #[test]
    fn real_worker_gate_serializes_same_root_but_runs_independent_root_http() {
        for independent in [false, true] {
            let fixture = crate::integration_tests::Fixture::new();
            let shared = Arc::new(fixture.shared());
            let first = shared.config.resolve(None).unwrap();
            let second = if independent {
                let path = fixture.0.join("other");
                fs::create_dir(&path).unwrap();
                shared
                    .config
                    .register(path.to_str().unwrap().into(), None)
                    .unwrap()
            } else {
                first.clone()
            };
            let admit = |root: &GameRoot| {
                shared
                    .tasks
                    .admit_queued(
                        tasks::TaskTarget {
                            root_id: root.id.clone(),
                            root_path: root.path.clone(),
                            instance_id: Some("Sample".into()),
                        },
                        tasks::TaskKind::ResourceDownload,
                        tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
                    )
                    .unwrap()
            };
            let a = admit(&first);
            let b = admit(&second);
            let b_id = b.id().to_string();
            let server = Loopback::new();
            let (worker_a, ready_a, release_a) = fake_http_worker(
                shared.clone(),
                first.clone(),
                a,
                format!("{}/first", server.url),
                "first.fixture",
            );
            let (worker_b, ready_b, release_b) = fake_http_worker(
                shared.clone(),
                second.clone(),
                b,
                format!("{}/second", server.url),
                "second.fixture",
            );
            ready_a.recv_timeout(Duration::from_secs(2)).unwrap();
            if independent {
                ready_b.recv_timeout(Duration::from_secs(2)).unwrap();
                assert_eq!(server.requests.lock().unwrap().len(), 2);
                assert_eq!(shared.tasks.running_all().len(), 2);
            } else {
                assert_eq!(
                    shared.tasks.snapshot(&b_id).unwrap().stage,
                    tasks::TaskStage::Queued
                );
                assert_eq!(&*server.requests.lock().unwrap(), &["/first"]);
                assert!(!Path::new(&second.path).join("second.fixture").exists());
            }
            release_a.send(()).unwrap();
            worker_a.join().unwrap();
            if !independent {
                ready_b.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            release_b.send(()).unwrap();
            worker_b.join().unwrap();
            assert_eq!(
                fs::read(Path::new(&first.path).join("first.fixture")).unwrap(),
                b"fixture"
            );
            assert_eq!(
                fs::read(Path::new(&second.path).join("second.fixture")).unwrap(),
                b"fixture"
            );
            assert!(shared.tasks.active_all().is_empty());
        }
    }

    fn admitted() -> (Arc<tasks::Tasks>, tasks::TaskHandle) {
        let tasks = Arc::new(tasks::Tasks::new());
        let handle = tasks
            .admit(
                tasks::TaskTarget {
                    root_id: "fixture-root".into(),
                    root_path: "/fixture".into(),
                    instance_id: Some("Example".into()),
                },
                tasks::TaskKind::ResourceDownload,
            )
            .unwrap();
        (tasks, handle)
    }

    #[test]
    fn metadata_failure_keeps_traffic_and_error_despite_accepted_cancel() {
        let (tasks, handle) = admitted();
        let id = handle.id().to_owned();
        transfer_progress(
            &handle,
            modrinth_install::DownloadProgress {
                phase: "metadata".into(),
                message: "checked".into(),
                completed: 0,
                total: 0,
                bytes_done: 0,
                bytes_total: 0,
                network_bytes: 4096,
            },
        );
        assert_eq!(
            tasks.snapshot(&id).unwrap().stage,
            tasks::TaskStage::Preparing
        );
        tasks.cancel(&id).unwrap();
        finish(handle, Err("Required dependency changed".into()));
        let result = tasks.wait_terminal(&id).unwrap();
        assert_eq!(result.stage, tasks::TaskStage::Error);
        assert_eq!(result.network_bytes, 4096);
        assert_eq!(result.error.as_deref(), Some("Required dependency changed"));
        assert!(tasks.active().is_none());
    }

    #[test]
    fn explicit_cancel_waits_for_worker_cleanup_then_releases_admission() {
        let (tasks, handle) = admitted();
        let id = handle.id().to_owned();
        tasks.cancel(&id).unwrap();
        assert!(tasks.active().is_some());
        finish(handle, Err(modrinth_install::CANCELLED.into()));
        let result = tasks.wait_terminal(&id).unwrap();
        assert_eq!(result.stage, tasks::TaskStage::Cancelled);
        assert!(result.error.is_none());
        assert!(result.message.contains("未完成文件已清理"));
        assert!(tasks.active().is_none());
    }

    #[test]
    fn network_submission_and_worker_ready_allow_own_admitted_root() {
        let fixture = crate::integration_tests::Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        let task = shared
            .tasks
            .admit_queued(
                tasks::TaskTarget {
                    root_id: root.id.clone(),
                    root_path: root.path.clone(),
                    instance_id: Some("Sample".into()),
                },
                tasks::TaskKind::ResourceDownload,
                tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
            )
            .unwrap();
        task.wait_turn().unwrap();
        let bound = {
            let _operation = shared.operations.lock().unwrap();
            capture_submission(&shared, Some(&root.id), "Sample").unwrap()
        };
        assert_eq!(bound.path, root.path);
        // The old generic mutation guard rejected a worker's own active task.
        // This check must retain target readiness without that self-rejection.
        worker_ready(&shared, &bound, "Sample", &task).unwrap();
        task.finish(tasks::TaskOutcome::Complete {
            result: None,
            message: "fixture finished".into(),
            error: None,
        });
    }

    #[test]
    fn queued_resource_worker_rechecks_removed_root_binding_before_network() {
        let fixture = crate::integration_tests::Fixture::new();
        let shared = fixture.shared();
        let root = shared.config.resolve(None).unwrap();
        let target = tasks::TaskTarget {
            root_id: root.id.clone(),
            root_path: root.path.clone(),
            instance_id: Some("Sample".into()),
        };
        let blocker = shared
            .tasks
            .admit_queued(
                target.clone(),
                tasks::TaskKind::Install,
                tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
            )
            .unwrap();
        blocker.wait_turn().unwrap();
        let task = shared
            .tasks
            .admit_queued(
                target,
                tasks::TaskKind::ResourceDownload,
                tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
            )
            .unwrap();
        let other = fixture.0.join("other");
        std::fs::create_dir(&other).unwrap();
        let registered = shared
            .config
            .register(other.to_str().unwrap().into(), None)
            .unwrap();
        shared.config.select(&registered.id).unwrap();
        shared.config.remove(&root.id).unwrap();
        blocker.finish(tasks::TaskOutcome::Complete {
            result: None,
            message: "fixture finished".into(),
            error: None,
        });
        task.wait_turn().unwrap();
        assert!(worker_ready(&shared, &root, "Sample", &task).is_err());
        assert!(!Path::new(&root.path).join(".pcl-linux").exists());
        task.finish(tasks::TaskOutcome::Error(
            "fixture root binding changed".into(),
        ));
    }

    #[test]
    fn real_queued_start_retains_official_resource_title_and_cancels_without_metadata() {
        let fixture = crate::integration_tests::Fixture::new();
        let shared = Arc::new(fixture.shared());
        let root = shared.config.resolve(None).unwrap();
        let instance = Path::new(&root.path).join("versions/Sample");
        fs::create_dir_all(instance.join("mods")).unwrap();
        fs::write(instance.join("Sample.json"),br#"{"id":"Sample","clientVersion":"1.20.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]}"#).unwrap();
        let blocker = shared
            .tasks
            .admit_queued(
                tasks::TaskTarget {
                    root_id: root.id.clone(),
                    root_path: root.path.clone(),
                    instance_id: Some("Sample".into()),
                },
                tasks::TaskKind::Install,
                tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
            )
            .unwrap();
        blocker.wait_turn().unwrap();
        let request = modrinth_install::InstallRequest {
            project_id: "Proj0001".into(),
            version_id: "Vers0001".into(),
            file_name: None,
        };
        let original = modrinth_install::test_install_plan(
            Path::new(&root.path),
            &shared.project,
            &root.id,
            "Sample",
            request.clone(),
        );
        let view = shared
            .resource_confirmations
            .remember(request.clone(), original)
            .unwrap();
        let result = start_request(
            shared.clone(),
            Some(&root.id),
            "Sample".into(),
            request.clone(),
            view.revision.clone(),
        )
        .unwrap();
        let id = result["id"].as_str().unwrap();
        let queued = shared.tasks.snapshot(id).unwrap();
        assert_eq!(queued.stage, tasks::TaskStage::Queued);
        assert_eq!(
            queued.display_name.as_deref(),
            Some("Queued official resource")
        );
        assert_eq!(queued.instance_id.as_deref(), Some("Sample"));
        shared.tasks.cancel(id).unwrap();
        let terminal = shared.tasks.wait_terminal(id).unwrap();
        assert_eq!(terminal.stage, tasks::TaskStage::Cancelled);
        assert_eq!(terminal.network_bytes, 0);
        assert_eq!(fs::read_dir(instance.join("mods")).unwrap().count(), 0);
        assert!(!shared.project.join(".pcl-rust/resource-downloads").exists());
        assert!(shared
            .resource_confirmations
            .claim(
                Path::new(&root.path),
                &shared.project,
                &root.id,
                "Sample",
                &request,
                &view.revision
            )
            .is_err());
        blocker.finish(tasks::TaskOutcome::Complete {
            result: None,
            message: "fixture finished".into(),
            error: None,
        });
        assert!(shared.tasks.active_all().is_empty());
    }
}
