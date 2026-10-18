//! Official Java runtime installation, independent of any selected game root.
//! Catalog authority -> bounded manifest -> anonymous checked files -> probe ->
//! no-replacement directory publication. Config registration only appends the
//! resulting canonical executable; it never changes a manual/automatic choice.
mod binding;
mod catalog;
mod files;
mod network;
#[cfg(test)]
mod parallel_tests;
#[cfg(test)]
mod tests;
use crate::tasks::{TaskHandle, TaskProgress, TaskStage};
pub use binding::StoreBinding;
pub use catalog::Package;
use catalog::{Entry, Manifest};
pub use files::Store;
use pcl_install::InstallStep;
use std::{
    cell::Cell,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
type Result<T> = std::result::Result<T, String>;
pub const CANCELLED: &str = "Java 下载已取消";
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
pub async fn list() -> Result<Vec<Package>> {
    let http = network::Http::new(pcl_network::snapshot(), pcl_network::download_snapshot())?;
    catalog::list(&http, &AtomicBool::new(false)).await
}
pub fn validate_request(component: &str, hash: &str) -> Result<()> {
    catalog::component(component)?;
    catalog::sha1(hash)
}
pub struct Installed {
    pub path: PathBuf,
    pub runtime: pcl_core::java::JavaRuntime,
}
fn report(
    task: &TaskHandle,
    index: usize,
    completed: u64,
    total: u64,
    bytes: u64,
    byte_total: u64,
    message: String,
) {
    let phases = ["java-metadata", "java-files", "java-probe", "java-publish"];
    let labels = [
        "获取 Java 版本信息",
        "下载并校验 Java 文件",
        "检查 Java 运行环境",
        "安装 Java",
    ];
    task.update(TaskProgress {
        stage: if index == 1 {
            TaskStage::Downloading
        } else {
            TaskStage::Processing
        },
        phase: phases[index].into(),
        message,
        completed,
        total,
        bytes_done: bytes,
        bytes_total: byte_total,
        network_bytes: bytes,
        steps: phases
            .iter()
            .enumerate()
            .map(|(i, id)| InstallStep {
                id: (*id).into(),
                label: labels[i].into(),
                state: if i < index {
                    "complete"
                } else if i == index {
                    "running"
                } else {
                    "pending"
                }
                .into(),
                progress: None,
            })
            .collect(),
    });
}
pub async fn install(
    store: &Store,
    component: &str,
    hash: &str,
    http: network::Http,
    task: &TaskHandle,
) -> Result<Installed> {
    install_bound(
        store,
        component,
        hash,
        http,
        task,
        &|| Ok(()),
        &|| Ok(()),
        true,
    )
    .await
}

/// Reuses the exact same official catalog, manifest, staging and managed
/// publication engine as explicit downloads. No higher-major substitution.
pub async fn install_recommended_guarded(
    store: &Store,
    major: u32,
    http: network::Http,
    task: &TaskHandle,
    fast: &impl Fn() -> Result<()>,
    full: &impl Fn() -> Result<()>,
) -> Result<Installed> {
    fast()?;
    full()?;
    let package = catalog::list(&http, &task.cancellation_token())
        .await?
        .into_iter()
        .find(|package| package.major == major)
        .ok_or_else(|| format!("Mojang 官方目录没有当前平台的 Java {major}，请手动安装适用版本"))?;
    full()?;
    install_guarded(
        store,
        &package.component,
        &package.sha1,
        http,
        task,
        fast,
        full,
    )
    .await
}

pub async fn install_guarded(
    store: &Store,
    component: &str,
    hash: &str,
    http: network::Http,
    task: &TaskHandle,
    fast: &impl Fn() -> Result<()>,
    full: &impl Fn() -> Result<()>,
) -> Result<Installed> {
    install_bound(store, component, hash, http, task, fast, full, false).await
}

async fn install_bound(
    store: &Store,
    component: &str,
    hash: &str,
    http: network::Http,
    task: &TaskHandle,
    fast: &impl Fn() -> Result<()>,
    full: &impl Fn() -> Result<()>,
    finish_task: bool,
) -> Result<Installed> {
    let cancel = task.cancellation_token();
    check(&cancel)?;
    full()?;
    report(task, 0, 0, 0, 0, 0, "正在获取 Java 版本信息".into());
    let lock = store.dir.lock(".java-install.lock")?;
    store.recover(&lock)?;
    let package = catalog::selected(&http, component, hash, &cancel).await?;
    task.set_resource_name(&format!("Java {}", package.version));
    let body = http
        .metadata(
            &package.manifest.url,
            Some((&package.manifest.sha1, package.manifest.size)),
            &cancel,
        )
        .await?;
    let manifest: Manifest =
        serde_json::from_slice(&body).map_err(|e| format!("Java 清单无效：{e}"))?;
    let total_bytes = manifest.validate()?;
    check(&cancel)?;
    full()?;
    let mut stage = store.create(&package, &manifest, task.id(), &lock)?;
    let result = async {
        stage.prepare_directories()?;
        let total = manifest
            .files
            .values()
            .filter(|entry| matches!(entry, Entry::File { .. }))
            .count() as u64;
        // Futures are polled by this worker, so counters need no lock. Each
        // progress callback adds only its file's new bytes; another active
        // file's callback can never replace or roll back the task total.
        let bytes = Cell::new(0);
        let complete = Cell::new(0);
        let stage_ref = &stage;
        let jobs = manifest.files.iter().filter_map(|(path, entry)| {
            let Entry::File {
                executable,
                downloads,
            } = entry
            else {
                return None;
            };
            let source = &downloads["raw"];
            let http = &http;
            let bytes = &bytes;
            let complete = &complete;
            let cancel = &cancel;
            Some(async move {
                check(cancel)?;
                fast()?;
                let mut file = stage_ref.anonymous(path)?;
                report(
                    task,
                    1,
                    complete.get(),
                    total,
                    bytes.get(),
                    total_bytes,
                    format!("正在下载 Java：{path}"),
                );
                let mut previous = 0;
                http.file(source, &mut file, cancel, fast, |current| {
                    bytes.set(bytes.get() + current - previous);
                    previous = current;
                    report(
                        task,
                        1,
                        complete.get(),
                        total,
                        bytes.get(),
                        total_bytes,
                        format!("正在下载 Java：{path}"),
                    )
                })
                .await?;
                Ok((path, file, *executable))
            })
        });
        // Completion consumes and publishes one checked anonymous descriptor
        // immediately. On any error the helper drops all remaining futures
        // (responses, scheduler permits and FDs) before stage cleanup runs.
        pcl_network::for_each_bounded(jobs, http.parallel_limit(), |(path, file, executable)| {
            check(&cancel)?;
            full()?;
            stage_ref.publish_file(path, &file, executable)?;
            complete.set(complete.get() + 1);
            report(
                task,
                1,
                complete.get(),
                total,
                bytes.get(),
                total_bytes,
                format!("已校验 Java：{path}"),
            );
            Ok(())
        })
        .await?;
        let bytes = bytes.get();
        let complete = complete.get();
        check(&cancel)?;
        stage.links()?;
        // Re-fetch only small provider metadata. An identity change cannot
        // convert files selected for one runtime into a different installation.
        let fresh = catalog::selected(&http, component, hash, &cancel).await?;
        if fresh != package {
            return Err("Java 官方版本信息已改变，请重新下载".into());
        }
        report(
            task,
            2,
            complete,
            total,
            bytes,
            total_bytes,
            "正在检查 Java 版本与架构".into(),
        );
        stage.verify_complete()?;
        let runtime = pcl_core::java::inspect(&stage.java_path()?)?;
        if runtime.major != package.major {
            return Err(format!(
                "Java 实际主版本 {} 与官方清单 {} 不符",
                runtime.major, package.major
            ));
        }
        check(&cancel)?;
        full()?;
        if finish_task {
            task.begin_finishing();
        }
        check(&cancel)?;
        full()?;
        report(
            task,
            3,
            complete,
            total,
            bytes,
            total_bytes,
            "正在安装 Java".into(),
        );
        let path = stage.publish()?;
        let runtime = pcl_core::java::JavaRuntime {
            path: path.to_str().ok_or("Java 路径不是 UTF-8")?.into(),
            ..runtime
        };
        Ok(Installed { path, runtime })
    }
    .await;
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Err(cleanup) = stage.cleanup() {
                return Err(format!(
                    "Java 清理失败，已保留暂存目录：{cleanup}；原错误：{error}"
                ));
            }
            Err(error)
        }
    }
}
pub fn http(
    factory: Arc<pcl_network::ClientFactory>,
    scheduler: Arc<pcl_network::DownloadScheduler>,
) -> Result<network::Http> {
    network::Http::new(factory, scheduler)
}
