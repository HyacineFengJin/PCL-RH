//! Verified installation of versions from Mojang's official catalog.
use reqwest::{Client, Url};
mod bound_root;
use bound_root::{InstallDir, InstallTemporary};
mod cache;
mod components;
mod network;
mod repair;
/// Shared durable staging for caller-authorized, app-owned namespaces.
///
/// The caller supplies business authority and its private context. Registry
/// evidence diagnoses staging/publication; explicit discard intents cover only
/// generated stages, never business recovery or user-original deletion. The
/// original repair namespace and schema are retained.
pub mod staging {
    pub use crate::repair::staging::{
        Allocation, Area, AreaReport, DiscardOutcome, Issue, IssueKind, Limits, LogStatus, Outcome,
        SealedTicket, SourceReport, SourceState, Ticket,
    };
    pub use pcl_core::retained_fs::stage::DiscardReceipt;
}
pub use repair::{
    DamageRecoveryReport, DamageRepair, DamageRepairReport, IntegrityCheck, IntegrityProblem,
    IntegrityProblemKind, IntegrityReport, MissingRepair, MissingRepairReport, RepairUnsupported,
};
mod resolved;
pub use resolved::{ProcessorJavaRequirement, ResolvedInstallRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
type Result<T> = std::result::Result<T, String>;
const MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const MAX_METADATA: u64 = 32 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename(deserialize = "type"))]
    pub kind: String,
    #[serde(rename(deserialize = "releaseTime"))]
    pub release_time: String,
}
#[derive(Clone, Serialize)]
pub struct Progress {
    pub steps: Vec<InstallStep>,
    pub stage: String,
    pub message: String,
    pub completed: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub network_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstallStep {
    pub id: String,
    pub label: String,
    pub state: String,
    pub progress: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSelection {
    pub provider: String,
    pub version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRequest {
    pub minecraft: String,
    pub name: String,
    #[serde(default)]
    pub components: Vec<ComponentSelection>,
}
impl InstallRequest {
    pub fn validate(&self) -> Result<()> {
        pcl_core::identifier(&self.minecraft)?;
        if self.name.is_empty()
            || self.name.len() > 120
            || self.name.trim() != self.name
            || self.name.chars().any(char::is_control)
            || self.name.starts_with(".install-")
        {
            return Err("版本名称不能为空、超过 120 字节、包含控制字符或前后空格".into());
        }
        pcl_core::identifier(&self.name).map_err(|_| "版本名称包含不允许的路径字符".to_string())?;
        if self.components.len() > 1 {
            return Err("Fabric、Forge 与 NeoForge 不能同时安装；请选择一个加载器".into());
        }
        for component in &self.components {
            pcl_core::identifier(&component.version)?;
            if component.version.len() > 160
                || component.version.trim() != component.version
                || component.version.chars().any(char::is_control)
            {
                return Err("组件版本无效".into());
            }
            match component.provider.to_ascii_lowercase().as_str() {
                "fabric" | "forge" | "neoforge" => {}
                "optifine" => return Err("暂不支持自动安装 OptiFine，请取消此组件后重试".into()),
                "labymod" => return Err("暂不支持自动安装 LabyMod，请取消此组件后重试".into()),
                _ => return Err(format!("暂不支持自动安装组件：{}", component.provider)),
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct InstallResult {
    pub id: String,
    pub java_major: u32,
    pub files_downloaded: u64,
    pub files_reused: u64,
}
pub struct Installer {
    client: Client,
    // None discovers a processor runtime; Some is a strict caller selection.
    // This choice is independent of the Java used later to launch the game.
    java: Option<PathBuf>,
    project: Option<PathBuf>,
    cache_source: Option<cache::CacheSource>,
    downloads: Arc<pcl_network::DownloadScheduler>,
    #[cfg(test)]
    endpoint: Option<String>,
}
#[derive(Clone)]
struct Download {
    url: String,
    hash: String,
    size: u64,
    relative: String,
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn network_error(e: reqwest::Error) -> String {
    use std::error::Error;
    let e = e.without_url();
    let mut details = e.to_string();
    let mut source = e.source();
    for _ in 0..8 {
        let Some(reason) = source else {
            break;
        };
        details.push_str(": ");
        details.push_str(&reason.to_string());
        source = reason.source();
    }
    details
}
fn retry_wait(cancel: &AtomicBool, attempt: u32) -> Result<()> {
    for _ in 0..attempt * 10 {
        check(cancel)?;
        std::thread::sleep(Duration::from_millis(100));
    }
    check(cancel)
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err("安装已取消".into())
    } else {
        Ok(())
    }
}
fn hash_valid(hash: &str) -> bool {
    hash.len() == 40 && hash.bytes().all(|c| c.is_ascii_hexdigit())
}
fn verify(
    path: &bound_root::InstallPath,
    hash: &str,
    size: u64,
    cancel: &AtomicBool,
) -> Result<bool> {
    let Some(mut f) = path.open_optional()? else {
        return Ok(false);
    };
    if f.metadata().map_err(error)?.len() != size {
        return Ok(false);
    }
    let mut digest = Sha1::new();
    let mut buf = [0; 65536];
    loop {
        check(cancel)?;
        let n = f.read(&mut buf).map_err(error)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(format!("{:x}", digest.finalize()).eq_ignore_ascii_case(hash))
}
impl Installer {
    pub fn new() -> Result<Self> {
        Self::from_factory(&pcl_network::snapshot())
    }
    /// Build from the caller's captured factory so preparation and execution
    /// share its proxy and resolver policy. Download admission is captured
    /// separately with `with_download_policy`.
    pub fn from_factory(factory: &pcl_network::ClientFactory) -> Result<Self> {
        Ok(Self {
            client: factory
                .async_client()
                .user_agent("PCL-RH/0.1")
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(error)?,
            java: None,
            project: None,
            cache_source: None,
            downloads: pcl_network::download_snapshot(),
            #[cfg(test)]
            endpoint: None,
        })
    }
    /// Select only this executable for Forge/NeoForge installation processors.
    /// An unavailable executable or a different required Java major is an error;
    /// omitting this call retains automatic processor-runtime discovery.
    pub fn with_java(mut self, path: impl AsRef<Path>) -> Self {
        self.java = Some(path.as_ref().to_path_buf());
        self
    }
    pub fn with_project(mut self, path: impl AsRef<Path>) -> Self {
        self.project = Some(path.as_ref().to_path_buf());
        self
    }
    /// Capture this once at submission. Existing transfers keep their slots and
    /// byte clock even if the launcher's later preference snapshot changes.
    pub fn with_download_policy(mut self, scheduler: Arc<pcl_network::DownloadScheduler>) -> Self {
        self.downloads = scheduler;
        self
    }
    /// Reuse only SHA1/size-verified library and asset bytes by copying them into
    /// this installation. Source files are opened read-only beneath a pinned FD.
    pub fn with_cache_source(mut self, root: impl AsRef<Path>) -> Result<Self> {
        self.cache_source = Some(cache::CacheSource::open(root.as_ref())?);
        Ok(self)
    }
    fn manifest_url(&self) -> &str {
        #[cfg(test)]
        if let Some(url) = &self.endpoint {
            return url;
        }
        MANIFEST
    }
    fn url(&self, raw: &str) -> Result<Url> {
        let url = Url::parse(raw).map_err(error)?;
        #[cfg(test)]
        if self.endpoint.is_some() && url.scheme() == "http" && url.host_str() == Some("127.0.0.1")
        {
            return Ok(url);
        }
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || !matches!(
                url.host_str(),
                Some(
                    "piston-meta.mojang.com"
                        | "piston-data.mojang.com"
                        | "launchermeta.mojang.com"
                        | "launcher.mojang.com"
                        | "libraries.minecraft.net"
                        | "resources.download.minecraft.net"
                        | "meta.fabricmc.net"
                        | "maven.fabricmc.net"
                        | "maven.minecraftforge.net"
                        | "files.minecraftforge.net"
                        | "maven.neoforged.net"
                        | "repo.maven.apache.org"
                )
            )
        {
            return Err("下载地址不是受信任的官方 HTTPS 地址".into());
        }
        Ok(url)
    }
    fn bytes(&self, raw_url: &str, limit: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
        let url = self.url(raw_url)?;
        let host = url.host_str().unwrap_or("官方服务器").to_owned();
        let mut last = String::new();
        for attempt in 0..3 {
            check(cancel)?;
            if attempt > 0 {
                retry_wait(cancel, attempt)?;
            }
            let mut out = Vec::new();
            let body = self.metadata_transfer(&url, limit, cancel, |chunk| {
                out.extend_from_slice(chunk);
                Ok(())
            });
            match body {
                Ok(()) => return Ok(out),
                Err(e) => {
                    check(cancel)?;
                    last = format!("读取 {host} 的版本信息失败：{e}");
                    if e.contains("HTTP 4") && !e.contains("HTTP 429") {
                        return Err(last);
                    }
                }
            }
        }
        Err(format!("{last}（已尝试 3 次，请检查网络后重试）"))
    }
    fn manifest(&self, cancel: &AtomicBool) -> Result<Value> {
        serde_json::from_slice(&self.bytes(self.manifest_url(), MAX_METADATA, cancel)?)
            .map_err(error)
    }
    pub fn catalog(&self) -> Result<Vec<VersionEntry>> {
        let data = self.manifest(&AtomicBool::new(false))?;
        serde_json::from_value(data["versions"].clone()).map_err(error)
    }
    #[cfg(test)]
    fn download(
        &self,
        root: &Path,
        d: &Download,
        cancel: &AtomicBool,
        abort: &AtomicBool,
        done: &AtomicU64,
        total: u64,
        bytes: &AtomicU64,
        network_bytes: &AtomicU64,
        byte_total: u64,
        group_done: &AtomicU64,
        group_total: u64,
        cb: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<bool> {
        self.download_to(
            &InstallDir::legacy(root.into()),
            d,
            cancel,
            abort,
            done,
            total,
            bytes,
            network_bytes,
            byte_total,
            group_done,
            group_total,
            cb,
        )
    }
    fn download_to(
        &self,
        root: &InstallDir,
        d: &Download,
        cancel: &AtomicBool,
        abort: &AtomicBool,
        done: &AtomicU64,
        total: u64,
        bytes: &AtomicU64,
        network_bytes: &AtomicU64,
        byte_total: u64,
        group_done: &AtomicU64,
        group_total: u64,
        cb: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<bool> {
        check(cancel)?;
        let path = root.file(&d.relative)?;
        if verify(&path, &d.hash, d.size, cancel)?
            || match &self.cache_source {
                Some(cache) if !self.downloads.policy().forbid_cross_root_cache_copy => {
                    cache.copy_to(root, d, cancel)?
                }
                _ => false,
            }
        {
            done.fetch_add(1, Ordering::Relaxed);
            group_done.fetch_add(1, Ordering::Relaxed);
            bytes.fetch_add(d.size, Ordering::Relaxed);
            cb(Progress {
                steps: vec![components::download_hint(
                    &d.relative,
                    group_done.load(Ordering::Relaxed),
                    group_total,
                )],
                stage: "downloading".into(),
                message: "正在校验已安装文件".into(),
                completed: done.load(Ordering::Relaxed),
                total,
                bytes_done: bytes.load(Ordering::Relaxed),
                bytes_total: byte_total,
                network_bytes: network_bytes.load(Ordering::Relaxed),
            });
            return Ok(false);
        }
        let url = self.url(&d.url)?;
        let path = root.file(&d.relative)?;
        let mut last = String::new();
        for _ in 0..3 {
            check(cancel)?;
            if abort.load(Ordering::Acquire) {
                return Err("其他下载失败，传输已停止".into());
            }
            let mut transferred = 0;
            let mut file = path.output(d.size)?;
            let result = (|| {
                let mut digest = Sha1::new();
                self.transfer_with_abort(&url, d.size, cancel, abort, |chunk| {
                    file.file_mut().write_all(chunk).map_err(error)?;
                    transferred += chunk.len() as u64;
                    digest.update(chunk);
                    bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    network_bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    cb(Progress {
                        steps: vec![components::download_hint(
                            &d.relative,
                            group_done.load(Ordering::Relaxed),
                            group_total,
                        )],
                        stage: "downloading".into(),
                        message: download_message(&d.relative).into(),
                        completed: done.load(Ordering::Relaxed),
                        total,
                        bytes_done: bytes.load(Ordering::Relaxed),
                        bytes_total: byte_total,
                        network_bytes: network_bytes.load(Ordering::Relaxed),
                    });
                    Ok(())
                })?;
                if transferred != d.size
                    || !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(&d.hash)
                {
                    return Err(format!("文件 SHA1 或大小校验失败：{}", d.relative));
                }
                root.file(&d.relative)?;
                Ok(())
            })();
            let result = match result {
                // Publication can have committed even when durability or its
                // postcheck fails. It is outside the transfer retry boundary.
                Ok(()) => {
                    file.publish()?;
                    Ok(())
                }
                Err(e) => Err(file.close(e)),
            };
            match result {
                Ok(()) => {
                    done.fetch_add(1, Ordering::Relaxed);
                    group_done.fetch_add(1, Ordering::Relaxed);
                    cb(Progress {
                        steps: vec![components::download_hint(
                            &d.relative,
                            group_done.load(Ordering::Relaxed),
                            group_total,
                        )],
                        stage: "downloading".into(),
                        message: download_message(&d.relative).into(),
                        completed: done.load(Ordering::Relaxed),
                        total,
                        bytes_done: bytes.load(Ordering::Relaxed),
                        bytes_total: byte_total,
                        network_bytes: network_bytes.load(Ordering::Relaxed),
                    });
                    return Ok(true);
                }
                Err(e) => {
                    bytes.fetch_sub(transferred.min(d.size), Ordering::Relaxed);
                    if e.starts_with("取消清理失败：") {
                        return Err(e);
                    }
                    check(cancel)?;
                    last = e;
                }
            }
        }
        Err(last)
    }
    pub fn install(
        &self,
        root: &Path,
        id: &str,
        cancel: &AtomicBool,
        on_progress: impl Fn(Progress) + Send + Sync,
    ) -> Result<InstallResult> {
        self.install_request(
            root,
            &InstallRequest {
                minecraft: id.into(),
                name: id.into(),
                components: vec![],
            },
            cancel,
            on_progress,
        )
    }
    pub fn install_request(
        &self,
        root: &Path,
        request: &InstallRequest,
        cancel: &AtomicBool,
        on_progress: impl Fn(Progress) + Send + Sync,
    ) -> Result<InstallResult> {
        self.install_request_with_resolution(Some(root), request, None, None, cancel, on_progress)
    }
    fn install_request_with_resolution(
        &self,
        root: Option<&Path>,
        request: &InstallRequest,
        resolved: Option<&ResolvedInstallRequest>,
        prepared_root: Option<InstallDir>,
        cancel: &AtomicBool,
        on_progress: impl Fn(Progress) + Send + Sync,
    ) -> Result<InstallResult> {
        request.validate()?;
        check(cancel)?;
        if prepared_root.is_none() && root.ok_or("缺少安装根目录")?.exists() {
            let root = root.ok_or("缺少安装根目录")?;
            let existing = root.join("versions").join(&request.name);
            match fs::symlink_metadata(existing) {
                Ok(_) => return Err("该版本目录已存在，请修改版本名称".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(error(e)),
            }
        }
        let steps = Mutex::new(components::initial_steps(request));
        let previous = Mutex::new(None::<Progress>);
        let emit = |mut p: Progress| {
            let mut steps = steps.lock().unwrap();
            components::advance_steps(&mut steps, &p);
            if let Some(previous) = previous.lock().unwrap().as_ref() {
                // Worker snapshots can reach this lock in a different order
                // from their samples; serialize the delivered network counter.
                p.network_bytes = p.network_bytes.max(previous.network_bytes);
                if p.stage == "downloading" && previous.stage == "downloading" {
                    p.completed = p.completed.max(previous.completed);
                }
                if p.stage == "complete" {
                    p.completed = previous.completed;
                    p.total = previous.total;
                    p.bytes_done = previous.bytes_done;
                    p.bytes_total = previous.bytes_total;
                }
            }
            p.steps = steps.clone();
            *previous.lock().unwrap() = Some(p.clone());
            on_progress(p);
        };
        self.install_inner(root, request, resolved, prepared_root, cancel, &emit)
    }
    fn install_inner(
        &self,
        root: Option<&Path>,
        request: &InstallRequest,
        resolved: Option<&ResolvedInstallRequest>,
        prepared_root: Option<InstallDir>,
        cancel: &AtomicBool,
        on_progress: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<InstallResult> {
        let id = request.minecraft.as_str();
        let name = request.name.as_str();
        check(cancel)?;
        pcl_core::identifier(id)?;
        on_progress(Progress {
            steps: vec![],
            stage: "metadata".into(),
            message: id.into(),
            completed: 0,
            total: 0,
            bytes_done: 0,
            bytes_total: 0,
            network_bytes: 0,
        });
        // The ordinary API resolves at its historical metadata stage. A native
        // prepared request owns these exact bytes' parsed metadata and never
        // silently selects a replacement catalog entry during execution.
        let mut metadata = match resolved {
            Some(resolved) => resolved.vanilla.metadata.clone(),
            None => self.resolve_vanilla(request, cancel)?.metadata,
        };
        let root = match prepared_root {
            Some(root) => root,
            None => {
                let root = root.ok_or("缺少安装根目录")?;
                fs::create_dir_all(root).map_err(error)?;
                InstallDir::legacy(fs::canonicalize(root).map_err(error)?)
            }
        };
        let versions = root.directory("versions", true)?;
        if versions.entry_exists(name)? {
            return Err("该版本目录已存在，请选择其他版本或安装目录".into());
        }
        let mut temporary = InstallTemporary::new(&versions)?;
        let temp_relative = Path::new("versions")
            .join(temporary.name())
            .to_string_lossy()
            .into_owned();
        let installation = (|| {
            let mut downloads = vec![artifact(
                &metadata["downloads"]["client"],
                format!("{temp_relative}/{name}.jar"),
            )?];
            let index_id = metadata["assetIndex"]["id"]
                .as_str()
                .ok_or("Version lacks asset index")?;
            pcl_core::identifier(index_id)?;
            let index = artifact(
                &metadata["assetIndex"],
                format!("assets/indexes/{index_id}.json"),
            )?;
            let index_raw = self.bytes(&index.url, MAX_METADATA, cancel)?;
            if index_raw.len() as u64 != index.size
                || !format!("{:x}", Sha1::digest(&index_raw)).eq_ignore_ascii_case(&index.hash)
            {
                return Err("资源索引 SHA1 或大小校验失败".into());
            }
            let asset_data: Value = serde_json::from_slice(&index_raw).map_err(error)?;
            if asset_data["virtual"].as_bool() == Some(true)
                || asset_data["map_to_resources"].as_bool() == Some(true)
            {
                return Err("暂不支持旧版本的虚拟资源或 resources 资源布局".into());
            }
            downloads.push(index);
            let platform = pcl_core::platform::Platform::current()?;
            let mut natives = vec![];
            for lib in metadata["libraries"]
                .as_array()
                .ok_or("Version lacks libraries")?
            {
                if !pcl_core::library_allowed(lib)? {
                    continue;
                }
                if lib.get("downloads").is_none() {
                    return Err("暂不支持缺少官方校验信息的旧版本依赖库".into());
                }
                let art = &lib["downloads"]["artifact"];
                if !art.is_null() {
                    let d = artifact(
                        art,
                        format!(
                            "libraries/{}",
                            art["path"].as_str().ok_or("Library lacks path")?
                        ),
                    )?;
                    if platform.native_artifact(lib["name"].as_str().unwrap_or(""))? {
                        natives.push((
                            d.relative.clone(),
                            lib["extract"]["exclude"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default(),
                        ));
                    }
                    downloads.push(d);
                }
                if let Some(class) = platform.legacy_native_classifier(lib)? {
                    let art = &lib["downloads"]["classifiers"][class];
                    let d = artifact(
                        art,
                        format!(
                            "libraries/{}",
                            art["path"].as_str().ok_or("Native library lacks path")?
                        ),
                    )?;
                    natives.push((
                        d.relative.clone(),
                        lib["extract"]["exclude"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default(),
                    ));
                    downloads.push(d);
                }
            }
            if let Some(file) = metadata["logging"]["client"].get("file") {
                let name = file["id"].as_str().ok_or("Logging config lacks ID")?;
                pcl_core::identifier(name)?;
                downloads.push(artifact(file, format!("assets/log_configs/{name}"))?);
            }
            for obj in asset_data["objects"]
                .as_object()
                .ok_or("Invalid asset objects")?
                .values()
            {
                let hash = obj["hash"]
                    .as_str()
                    .filter(|h| hash_valid(h))
                    .ok_or("Invalid asset hash")?;
                downloads.push(Download {
                    url: format!(
                        "https://resources.download.minecraft.net/{}/{hash}",
                        &hash[..2]
                    ),
                    hash: hash.into(),
                    size: obj["size"].as_u64().ok_or("Invalid asset size")?,
                    relative: format!("assets/objects/{}/{hash}", &hash[..2]),
                });
            }
            let mut fingerprints = std::collections::HashMap::new();
            for d in &downloads {
                if let Some(previous) =
                    fingerprints.insert(d.relative.clone(), (d.hash.clone(), d.size))
                {
                    if previous != (d.hash.clone(), d.size) {
                        return Err("版本信息包含相互冲突的文件路径".into());
                    }
                }
            }
            let mut seen = std::collections::HashMap::new();
            downloads.retain(|d| {
                if seen.contains_key(&d.relative) {
                    false
                } else {
                    seen.insert(d.relative.clone(), (d.hash.clone(), d.size));
                    true
                }
            });
            for d in &downloads {
                self.url(&d.url)?;
                root.file(&d.relative)?;
            }
            let total = downloads.len() as u64;
            let byte_total = downloads
                .iter()
                .try_fold(0u64, |n, d| n.checked_add(d.size))
                .ok_or("下载文件总大小超出范围")?;
            let group_done = [AtomicU64::new(0), AtomicU64::new(0)];
            let group_total = [
                downloads
                    .iter()
                    .filter(|d| !d.relative.starts_with("assets/"))
                    .count() as u64,
                downloads
                    .iter()
                    .filter(|d| d.relative.starts_with("assets/"))
                    .count() as u64,
            ];
            let done = AtomicU64::new(0);
            let bytes = AtomicU64::new(0);
            let network_bytes = AtomicU64::new(0);
            let downloaded = AtomicU64::new(0);
            let next = AtomicU64::new(0);
            let failure = Mutex::new(None);
            let abort = AtomicBool::new(false);
            std::thread::scope(|scope| {
                for _ in 0..usize::from(self.downloads.policy().max_concurrent_transfers)
                    .min(downloads.len())
                {
                    scope.spawn(|| loop {
                        if failure.lock().unwrap().is_some() {
                            break;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed) as usize;
                        let Some(d) = downloads.get(i) else {
                            break;
                        };
                        match self.download_to(
                            &root,
                            d,
                            cancel,
                            &abort,
                            &done,
                            total,
                            &bytes,
                            &network_bytes,
                            byte_total,
                            &group_done[usize::from(d.relative.starts_with("assets/"))],
                            group_total[usize::from(d.relative.starts_with("assets/"))],
                            &on_progress,
                        ) {
                            Ok(true) => {
                                downloaded.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(false) => {}
                            Err(e) => {
                                record_failure(&failure, e);
                                // In-flight siblings drop their full HTTP
                                // future, then clean partial files before join.
                                abort.store(true, Ordering::Release);
                                break;
                            }
                        }
                    });
                }
            });
            if let Some(e) = failure.into_inner().map_err(error)? {
                return Err(e);
            }
            check(cancel)?;
            temporary.dir().directory("natives", true)?;
            on_progress(Progress {
                steps: vec![],
                stage: "installing".into(),
                message: "解压运行库并安装游戏".into(),
                completed: total,
                total,
                bytes_done: byte_total,
                bytes_total: byte_total,
                network_bytes: network_bytes.load(Ordering::Relaxed),
            });
            for (path, excludes) in natives {
                check(cancel)?;
                let source = root.file(path)?.open()?;
                temporary.extract_natives(
                    source,
                    &excludes,
                    pcl_core::platform::Platform::current()?,
                )?;
            }
            check(cancel)?;
            let mut component_stats = components::ComponentStats::new(
                total,
                byte_total,
                network_bytes.load(Ordering::Relaxed),
                downloaded.load(Ordering::Relaxed),
                total - downloaded.load(Ordering::Relaxed),
            );
            if let Some(component) = request.components.first() {
                metadata = self.install_component(
                    &root,
                    temporary.dir(),
                    name,
                    id,
                    metadata,
                    component,
                    resolved.and_then(|r| r.component.as_ref()),
                    cancel,
                    &mut component_stats,
                    on_progress,
                )?;
                temporary.dir().directory("mods", true)?;
            }
            if request.components.is_empty() {
                on_progress(component_stats.event("game_install", "正在安装游戏", 0.0));
            }
            metadata["id"] = Value::String(name.into());
            metadata["jar"] = Value::String(name.into());
            metadata["clientVersion"] = Value::String(id.into());
            metadata
                .as_object_mut()
                .ok_or("版本信息无效")?
                .remove("inheritsFrom");
            temporary
                .dir()
                .file(format!("{name}.json"))?
                .write(&serde_json::to_vec_pretty(&metadata).map_err(error)?)?;
            check(cancel)?;
            if !request.components.is_empty() {
                self.publish_component_libraries(
                    &root,
                    &temporary.dir().directory("component-work", false)?,
                    cancel,
                    &component_stats,
                    on_progress,
                )?;
            }
            on_progress(component_stats.event("publishing", "正在完成安装", 1.0));
            if temporary.dir().entry_exists("component-work")? {
                temporary
                    .dir()
                    .remove_directory(Path::new("component-work"))
                    .map_err(|e| format!("取消清理失败：{e}"))?;
            }
            check(cancel)?;
            // Commit the whole directory in one operation. Once committed, a late
            // cancellation is a successful installation, never a partial rollback.
            temporary.publish(name)?;
            Ok(InstallResult {
                id: name.into(),
                java_major: metadata["javaVersion"]["majorVersion"]
                    .as_u64()
                    .unwrap_or(8) as u32,
                files_downloaded: component_stats.downloaded,
                files_reused: component_stats.reused,
            })
        })();
        if installation.is_err() {
            if let Err(cleanup) = temporary.close() {
                return Err(format!(
                    "取消清理失败：{cleanup}；原错误：{}",
                    installation.as_ref().unwrap_err()
                ));
            }
        }
        if installation.is_ok() {
            on_progress(Progress {
                steps: vec![],
                stage: "complete".into(),
                message: name.into(),
                completed: 1,
                total: 1,
                bytes_done: 0,
                bytes_total: 0,
                network_bytes: 0,
            });
        }
        installation
    }
}
#[cfg(all(test, target_os = "linux"))]
#[path = "bound_root_tests.rs"]
mod bound_root_tests;
#[cfg(test)]
#[path = "download_policy_tests.rs"]
mod download_policy_tests;
#[cfg(test)]
mod native_port_tests;
fn record_failure(failure: &Mutex<Option<String>>, new: String) {
    let mut failure = failure.lock().unwrap();
    if failure.is_none()
        || (new.starts_with("取消清理失败：")
            && !failure.as_ref().unwrap().starts_with("取消清理失败："))
    {
        *failure = Some(new);
    }
}
fn publish_directory(source: &Path, target: &Path) -> Result<()> {
    pcl_core::file_ops::rename_noreplace(source, target)
        .map_err(|e| format!("无法完成版本安装（目标可能已存在）：{e}"))
}
fn download_message(relative: &str) -> &'static str {
    if relative.starts_with("libraries/") {
        "正在下载游戏依赖库"
    } else if relative.starts_with("assets/log_configs/") {
        "正在下载日志配置"
    } else if relative.starts_with("assets/") {
        "正在下载游戏资源"
    } else {
        "正在下载游戏客户端"
    }
}
fn artifact(v: &Value, relative: String) -> Result<Download> {
    Ok(Download {
        url: v["url"].as_str().ok_or("Download lacks URL")?.into(),
        hash: v["sha1"]
            .as_str()
            .filter(|h| hash_valid(h))
            .ok_or("Download lacks valid SHA1")?
            .into(),
        size: v["size"].as_u64().ok_or("Download lacks size")?,
        relative,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{collections::HashMap, net::TcpListener, sync::Arc, thread};
    struct Server {
        url: String,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
        }
    }
    fn server(legacy: bool, bad_metadata: bool) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let desc = |path: &str, bytes: &[u8]| json!({"url":format!("{base}/{path}"),"size":bytes.len(),"sha1":format!("{:x}",Sha1::digest(bytes))});
        let index = serde_json::to_vec(&json!({"objects":{},"virtual":legacy})).unwrap();
        let mut index_desc = desc("index", &index);
        index_desc["id"] = json!("fixture-index");
        let mut lib = desc("library", b"library");
        lib["path"] = json!("org/example/library/1/library-1.jar");
        let mut log = desc("logging", b"logging");
        log["id"] = json!("client.xml");
        let metadata=serde_json::to_vec(&json!({"id":"fixture","javaVersion":{"majorVersion":21},"downloads":{"client":desc("client",b"client")},"assetIndex":index_desc,"libraries":[{"name":"org.example:library:1","downloads":{"artifact":lib}}],"logging":{"client":{"file":log}}})).unwrap();
        let catalog=serde_json::to_vec(&json!({"versions":[{"id":"fixture","type":"release","releaseTime":"2024-01-01T00:00:00Z","url":format!("{base}/metadata"),"sha1":if bad_metadata {"0000000000000000000000000000000000000000".into()} else {format!("{:x}",Sha1::digest(&metadata))}}]})).unwrap();
        let routes: HashMap<String, Vec<u8>> = [
            ("catalog", catalog),
            ("metadata", metadata),
            ("index", index),
            ("client", b"client".to_vec()),
            ("library", b"library".to_vec()),
            ("logging", b"logging".to_vec()),
        ]
        .into_iter()
        .map(|(p, b)| (format!("/{p}"), b))
        .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut buf = [0; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..n]);
                        let path = request.split_whitespace().nth(1).unwrap_or("");
                        if let Some(body) = routes.get(path) {
                            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                            stream.write_all(body).unwrap();
                        } else {
                            stream
                                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                                .unwrap();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Server {
            url: format!("{base}/catalog"),
            stop,
            worker: Some(worker),
        }
    }
    fn installer(server: &Server) -> Installer {
        let mut i = Installer::new().unwrap();
        i.endpoint = Some(server.url.clone());
        i
    }
    #[test]
    fn empty_install_and_existing_version_are_safe() {
        let server = server(false, false);
        let i = installer(&server);
        let root = tempfile::tempdir().unwrap();
        let cancelled = AtomicBool::new(false);
        let progress = Mutex::new(vec![]);
        assert_eq!(i.catalog().unwrap()[0].id, "fixture");
        let result = i
            .install(root.path(), "fixture", &cancelled, |p| {
                progress.lock().unwrap().push(p)
            })
            .unwrap();
        assert_eq!(result.java_major, 21);
        assert_eq!(result.files_downloaded, 4);
        // Network telemetry excludes reused files and includes actual response bodies.
        let network = progress.lock().unwrap().last().unwrap().network_bytes;
        assert!(network > 0);
        assert_eq!(
            fs::read(root.path().join("versions/fixture/fixture.jar")).unwrap(),
            b"client"
        );
        assert!(root.path().join("versions/fixture/fixture.json").exists());
        let entries = progress.into_inner().unwrap();
        assert!(entries
            .iter()
            .any(|p| p.stage == "downloading" && p.completed == p.total));
        assert!(entries.iter().any(|p| p.stage == "installing"));
        let cached_root = tempfile::tempdir().unwrap();
        for relative in ["libraries", "assets"] {
            fn copy_tree(source: &Path, target: &Path) {
                fs::create_dir_all(target).unwrap();
                for entry in fs::read_dir(source).unwrap() {
                    let entry = entry.unwrap();
                    let path = target.join(entry.file_name());
                    if entry.file_type().unwrap().is_dir() {
                        copy_tree(&entry.path(), &path);
                    } else {
                        fs::copy(entry.path(), path).unwrap();
                    }
                }
            }
            copy_tree(
                &root.path().join(relative),
                &cached_root.path().join(relative),
            );
        }
        let cached_progress = Mutex::new(vec![]);
        let reused = i
            .install(cached_root.path(), "fixture", &cancelled, |p| {
                cached_progress.lock().unwrap().push(p)
            })
            .unwrap();
        assert!(reused.files_reused > 0);
        assert!(
            cached_progress
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .network_bytes
                < network
        );
        assert!(entries
            .iter()
            .filter(|p| p.stage == "downloading")
            .all(|p| !p.message.contains('/')));
        let empty_root = tempfile::tempdir().unwrap();
        fs::create_dir_all(empty_root.path().join("versions/fixture")).unwrap();
        assert!(i
            .install(empty_root.path(), "fixture", &cancelled, |_| {})
            .unwrap_err()
            .contains("已存在"));
        assert_eq!(
            fs::read_dir(empty_root.path().join("versions/fixture"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            entries.last().unwrap().bytes_done,
            entries.last().unwrap().bytes_total
        );
        assert!(i
            .install(root.path(), "fixture", &cancelled, |_| {})
            .unwrap_err()
            .contains("已存在"));
        assert_eq!(
            fs::read(root.path().join("versions/fixture/fixture.jar")).unwrap(),
            b"client"
        );
    }
    #[test]
    fn cancellation_cleans_version_and_cached_files_are_verified() {
        let server = server(false, false);
        let i = installer(&server);
        let root = tempfile::tempdir().unwrap();
        let cancelled = AtomicBool::new(false);
        assert!(i
            .install(root.path(), "fixture", &cancelled, |p| {
                if p.stage == "downloading" {
                    cancelled.store(true, Ordering::Relaxed);
                }
            })
            .is_err());
        assert!(!root.path().join("versions/fixture").exists());
        assert_eq!(
            fs::read_dir(root.path().join("versions")).unwrap().count(),
            0
        );
        cancelled.store(false, Ordering::Relaxed);
        let library = root
            .path()
            .join("libraries/org/example/library/1/library-1.jar");
        fs::create_dir_all(library.parent().unwrap()).unwrap();
        fs::write(&library, b"corrupt").unwrap();
        i.install(root.path(), "fixture", &cancelled, |_| {})
            .unwrap();
        assert_eq!(fs::read(library).unwrap(), b"library");
    }
    #[test]
    fn rejects_unverified_metadata_legacy_and_symlink_escape() {
        for (legacy, bad, expected) in [(true, false, "暂不支持"), (false, true, "版本信息 SHA1")]
        {
            let server = server(legacy, bad);
            let root = tempfile::tempdir().unwrap();
            assert!(installer(&server)
                .install(root.path(), "fixture", &AtomicBool::new(false), |_| {})
                .unwrap_err()
                .contains(expected));
            assert!(!root.path().join("versions/fixture").exists());
        }
        #[cfg(unix)]
        {
            let server = server(false, false);
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), root.path().join("libraries")).unwrap();
            assert!(installer(&server)
                .install(root.path(), "fixture", &AtomicBool::new(false), |_| {})
                .is_err());
            assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        }
    }
    #[test]
    fn metadata_retries_server_failures_and_reports_transport_reason() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/catalog", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for status in ["503 Service Unavailable", "200 OK"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0; 4096];
                stream.read(&mut buf).unwrap();
                let body = if status.starts_with("200") {
                    "{\"versions\":[]}"
                } else {
                    ""
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let mut i = Installer::new().unwrap();
        i.endpoint = Some(url.clone());
        assert!(i.catalog().unwrap().is_empty());
        worker.join().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(async {
                i.client
                    .get(format!("{url}?token=private-value"))
                    .send()
                    .await
            })
            .unwrap_err();
        let message = network_error(err);
        assert!(message.contains("Connection refused") || message.contains("connect"));
        assert!(!message.contains("private-value"));
    }
    #[test]
    fn metadata_retry_wait_is_cancellable() {
        let cancelled = AtomicBool::new(false);
        let started = std::time::Instant::now();
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(30));
                cancelled.store(true, Ordering::Relaxed);
            });
            assert!(retry_wait(&cancelled, 2).unwrap_err().contains("取消"));
        });
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn rejects_urls_and_paths() {
        let i = Installer::new().unwrap();
        for url in [
            "http://libraries.minecraft.net/x",
            "https://example.org/x",
            "https://libraries.minecraft.net.evil.org/x",
            "https://user@libraries.minecraft.net/x",
            "https://libraries.minecraft.net:8443/x",
        ] {
            assert!(i.url(url).is_err(), "{url}");
        }
        assert!(i.url("https://libraries.minecraft.net/x").is_ok());
        assert!(artifact(
            &json!({"url":"https://libraries.minecraft.net/x","size":1,"sha1":"../bad"}),
            "x".into()
        )
        .is_err());
    }
}

#[cfg(test)]
mod loader_tests;
