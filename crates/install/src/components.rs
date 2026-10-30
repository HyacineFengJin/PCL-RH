//! Official loader profile assembly and isolated Forge installer processors.
use super::*;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    io::Seek,
    process::{Command, Stdio},
    time::Instant,
};
pub(super) const MAX_COMPONENT: u64 = 512 * 1024 * 1024;

/// Keep official installer evidence for files Forge opens outside the JVM
/// classpath. Adding them to `libraries` would change classpath/module behavior;
/// this separate list is read only by inspection/integrity/missing-file repair.
pub(super) fn record_runtime_downloads(
    metadata: &mut Value,
    descriptors: &std::collections::BTreeMap<String, Download>,
) -> Result<()> {
    let paths = pcl_core::repair::implicit_runtime_paths(metadata)?;
    if paths.is_empty() {
        return Ok(());
    }
    let receipts: Vec<_> = paths.iter().filter_map(|path| {
        let download = descriptors.get(path)?;
        (!download.url.is_empty()).then(|| json!({"path":path,"url":download.url,"sha1":download.hash,"size":download.size}))
    }).collect();
    metadata["pclRuntimeFiles"] = receipts.into();
    Ok(())
}

pub(crate) fn initial_steps(request: &InstallRequest) -> Vec<InstallStep> {
    let mut steps = vec![
        step("metadata", "下载原版 Json 文件"),
        step("vanilla_libraries", "下载原版支持库文件"),
        step("vanilla_resources", "下载原版资源文件"),
    ];
    if let Some(component) = request.components.first() {
        let name = match component.provider.to_ascii_lowercase().as_str() {
            "fabric" => "Fabric",
            "forge" => "Forge",
            _ => "NeoForge",
        };
        steps.push(step("component_metadata", &format!("下载 {name} 主文件")));
        steps.push(step(
            "component_analyze",
            &format!("分析 {name} 支持库文件"),
        ));
        steps.push(step(
            "component_download",
            &format!("下载 {name} 支持库文件"),
        ));
        steps.push(step("component_install", &format!("安装 {name}")));
    }
    steps.push(step("game_install", "安装游戏"));
    steps.push(step("game_support", "下载游戏支持库文件"));
    steps
}
fn step(id: &str, label: &str) -> InstallStep {
    InstallStep {
        id: id.into(),
        label: label.into(),
        state: "pending".into(),
        progress: Some(0.0),
    }
}
pub(crate) fn download_hint(relative: &str, done: u64, total: u64) -> InstallStep {
    let id = if relative.starts_with("assets/") {
        "vanilla_resources"
    } else {
        "vanilla_libraries"
    };
    InstallStep {
        id: id.into(),
        label: String::new(),
        state: if done == total { "complete" } else { "running" }.into(),
        progress: Some(done as f64 / total.max(1) as f64),
    }
}
pub(crate) fn advance_steps(steps: &mut [InstallStep], p: &Progress) {
    if p.stage == "complete" {
        for s in steps {
            s.state = "complete".into();
            s.progress = Some(1.0);
        }
        return;
    }
    if p.stage == "installing" {
        for s in steps.iter_mut().filter(|s| {
            matches!(
                s.id.as_str(),
                "metadata" | "vanilla_libraries" | "vanilla_resources"
            )
        }) {
            s.state = "complete".into();
            s.progress = Some(1.0);
        }
        return;
    }
    if p.stage == "downloading" {
        if let Some(s) = steps.iter_mut().find(|s| s.id == "metadata") {
            s.state = "complete".into();
            s.progress = Some(1.0);
        }
        // The two groups download concurrently, so only the group's own worker
        // counters can complete its row.
        for hint in &p.steps {
            if let Some(s) = steps.iter_mut().find(|s| s.id == hint.id) {
                if s.state != "complete" {
                    s.state = hint.state.clone();
                }
                s.progress = hint
                    .progress
                    .map(|n| n.max(s.progress.unwrap_or(0.0)).clamp(0.0, 1.0));
            }
        }
        return;
    }
    let target = if p.stage == "publishing" {
        "game_support"
    } else {
        p.stage.as_str()
    };
    if let Some(index) = steps.iter().position(|s| s.id == target) {
        for (i, s) in steps.iter_mut().enumerate() {
            if i < index {
                s.state = "complete".into();
                s.progress = Some(1.0);
            }
            if i == index {
                s.state = "running".into();
                s.progress = p
                    .steps
                    .iter()
                    .find(|x| x.id == p.stage)
                    .and_then(|x| x.progress)
                    .map(|n| n.clamp(0.0, 1.0));
            }
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) struct ComponentStats {
    pub completed: u64,
    pub total: u64,
    pub bytes: u64,
    pub byte_total: u64,
    pub network: u64,
    pub downloaded: u64,
    pub reused: u64,
    pub component_total: u64,
    pub component_completed: u64,
}
impl ComponentStats {
    pub fn new(total: u64, byte_total: u64, network: u64, downloaded: u64, reused: u64) -> Self {
        Self {
            completed: total,
            total,
            bytes: byte_total,
            byte_total,
            network,
            downloaded,
            reused,
            component_total: 0,
            component_completed: 0,
        }
    }
    pub fn event(&self, stage: &str, message: &str, fraction: f64) -> Progress {
        // A one-step progress hint is consumed by the request's full step tracker.
        Progress {
            stage: stage.into(),
            message: message.into(),
            completed: self.completed,
            total: self.total,
            bytes_done: self.bytes,
            bytes_total: self.byte_total,
            network_bytes: self.network,
            steps: vec![InstallStep {
                id: stage.into(),
                label: String::new(),
                state: "running".into(),
                progress: Some(if stage == "component_download" {
                    self.component_completed as f64 / self.component_total.max(1) as f64
                } else {
                    fraction
                }),
            }],
        }
    }
}
pub(super) fn maven_path(coordinate: &str) -> Result<String> {
    let (coordinate, extension) = coordinate.split_once('@').unwrap_or((coordinate, "jar"));
    let parts: Vec<_> = coordinate.split(':').collect();
    if !(3..=4).contains(&parts.len()) || parts.iter().any(|s| s.is_empty()) || extension.is_empty()
    {
        return Err("组件 Maven 坐标无效".into());
    }
    for part in parts.iter().copied().chain(std::iter::once(extension)) {
        pcl_core::identifier(part)?;
        if part.chars().any(char::is_control) {
            return Err("组件 Maven 坐标无效".into());
        }
    }
    let classifier = parts.get(3).map(|x| format!("-{x}")).unwrap_or_default();
    Ok(format!(
        "{}/{}/{}/{}-{}{classifier}.{extension}",
        parts[0].replace('.', "/"),
        parts[1],
        parts[2],
        parts[1],
        parts[2]
    ))
}
fn library_key(library: &Value) -> String {
    let coordinate = library["name"]
        .as_str()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");
    let p: Vec<_> = coordinate.split(':').collect();
    format!(
        "{}:{}:{}",
        p.first().unwrap_or(&""),
        p.get(1).unwrap_or(&""),
        p.get(3).unwrap_or(&"")
    )
}
pub(super) fn merge_profile(mut base: Value, profile: Value, minecraft: &str) -> Result<Value> {
    if !profile.is_object()
        || profile["inheritsFrom"]
            .as_str()
            .is_some_and(|id| id != minecraft)
        || !profile["mainClass"].is_string()
    {
        return Err("组件启动信息与所选 Minecraft 版本不匹配".into());
    }
    let mut libs = base["libraries"]
        .as_array()
        .cloned()
        .ok_or("游戏依赖库信息无效")?;
    for library in profile["libraries"]
        .as_array()
        .ok_or("组件依赖库信息无效")?
    {
        let key = library_key(library);
        libs.retain(|old| library_key(old) != key);
        libs.push(library.clone());
    }
    let mut arguments = json!({});
    for key in ["game", "jvm"] {
        let mut args = base["arguments"][key]
            .as_array()
            .cloned()
            .unwrap_or_default();
        args.extend(
            profile["arguments"][key]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
        arguments[key] = args.into();
    }
    let has_arguments = base.get("arguments").is_some() || profile.get("arguments").is_some();
    for (key, value) in profile.as_object().unwrap() {
        if !matches!(key.as_str(), "inheritsFrom" | "jar" | "downloads") {
            base[key] = value.clone();
        }
    }
    base["libraries"] = libs.into();
    if has_arguments {
        base["arguments"] = arguments;
    }
    Ok(base)
}
pub(super) fn archive_bytes<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let mut entry = zip
        .by_name(name)
        .map_err(|_| format!("官方安装器缺少文件：{name}"))?;
    if entry.is_dir()
        || entry.size() > limit
        || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
    {
        return Err("官方安装器文件无效或过大".into());
    }
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > limit {
        return Err("官方安装器文件过大".into());
    }
    Ok(bytes)
}
impl Installer {
    pub(super) fn component_url(&self, raw: &str) -> Result<String> {
        let url = self.url(raw)?;
        #[cfg(test)]
        if let Some(endpoint) = &self.endpoint {
            let mut target = Url::parse(endpoint).map_err(error)?;
            target.set_path(url.path());
            target.set_query(url.query());
            return Ok(target.to_string());
        }
        Ok(url.to_string())
    }
    pub(super) fn component_bytes(
        &self,
        url: &str,
        limit: u64,
        cancel: &AtomicBool,
        stats: &mut ComponentStats,
        cb: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let consume = |chunk: &[u8]| {
            stats.network += chunk.len() as u64;
            bytes.extend_from_slice(chunk);
            cb(stats.event("component_metadata", "正在读取组件安装信息", 0.0));
            Ok(())
        };
        // JSON/profile/SHA metadata is not charged to artifact pacing; an
        // installer archive has the larger MAX_COMPONENT contract and is.
        if limit <= MAX_METADATA {
            self.metadata_transfer(&self.url(url)?, limit, cancel, consume)?;
        } else {
            self.transfer(&self.url(url)?, limit, cancel, consume)?;
        }
        Ok(bytes)
    }
    fn component_file(
        &self,
        workspace: &InstallDir,
        cache: &InstallDir,
        d: &Download,
        cancel: &AtomicBool,
        stats: &mut ComponentStats,
        cb: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<()> {
        self.component_files(workspace, cache, std::slice::from_ref(d), cancel, stats, cb)
    }

    /// ZIP extraction, profile edits and cache copies stay on the caller. Only
    /// independent remote misses enter the same bounded downloader as vanilla.
    pub(super) fn component_files(
        &self,
        workspace: &InstallDir,
        cache: &InstallDir,
        downloads: &[Download],
        cancel: &AtomicBool,
        stats: &mut ComponentStats,
        cb: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<()> {
        let mut remote = Vec::new();
        let mut seen = HashSet::new();
        for d in downloads {
            check(cancel)?;
            if !seen.insert(&d.relative) {
                continue;
            }
            let path = workspace.file(&d.relative)?;
            path.prepare_parent()?;
            if verify(&path, &d.hash, d.size, cancel)? {
                stats.component_completed += 1;
                continue;
            }
            stats.total = stats.total.checked_add(1).ok_or("组件文件数量超出范围")?;
            stats.byte_total = stats
                .byte_total
                .checked_add(d.size)
                .ok_or("组件文件总大小超出范围")?;
            let mut reused = false;
            if !self.downloads.policy().forbid_cross_root_cache_copy {
                let cached = cache.file(&d.relative)?;
                if verify(&cached, &d.hash, d.size, cancel)? {
                    // Processors may rewrite inputs. Never share cache inodes.
                    copy_cancel(&cached, &path, &d.hash, d.size, cancel)?;
                    reused = true;
                } else if let Some(source) = &self.cache_source {
                    reused = source.copy_to(workspace, d, cancel)?;
                }
            }
            if reused {
                stats.component_completed += 1;
                stats.reused += 1;
                stats.completed += 1;
                stats.bytes += d.size;
                cb(stats.event("component_download", "正在校验组件依赖库", 0.0));
            } else {
                self.url(&d.url)?;
                remote.push(d);
            }
        }
        if remote.is_empty() {
            return Ok(());
        }
        let initial = *stats;
        let done = AtomicU64::new(initial.completed);
        let bytes = AtomicU64::new(initial.bytes);
        let network = AtomicU64::new(initial.network);
        let group = AtomicU64::new(initial.component_completed);
        let downloaded = AtomicU64::new(initial.downloaded);
        let reused = AtomicU64::new(initial.reused);
        let next = AtomicU64::new(0);
        let failure = Mutex::new(None);
        let abort = AtomicBool::new(false);
        let emission = Mutex::new(());
        let progress = |_: Progress| {
            // Sample after admission to this short emission lock so callbacks
            // cannot deliver older counts after a faster sibling's snapshot.
            let _emission = emission.lock().unwrap();
            let snapshot = ComponentStats {
                completed: done.load(Ordering::Relaxed),
                bytes: bytes.load(Ordering::Relaxed),
                network: network.load(Ordering::Relaxed),
                component_completed: group.load(Ordering::Relaxed),
                ..initial
            };
            cb(snapshot.event("component_download", "正在下载组件依赖库", 0.0));
        };
        std::thread::scope(|scope| {
            for _ in
                0..usize::from(self.downloads.policy().max_concurrent_transfers).min(remote.len())
            {
                scope.spawn(|| loop {
                    if abort.load(Ordering::Acquire) {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed) as usize;
                    let Some(d) = remote.get(index) else { break };
                    match self.download_to(
                        workspace,
                        d,
                        cancel,
                        &abort,
                        &done,
                        initial.total,
                        &bytes,
                        &network,
                        initial.byte_total,
                        &group,
                        initial.component_total,
                        &progress,
                    ) {
                        Ok(true) => {
                            downloaded.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(false) => {
                            reused.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) => {
                            record_failure(&failure, error);
                            abort.store(true, Ordering::Release);
                            break;
                        }
                    }
                });
            }
        });
        stats.completed = done.load(Ordering::Relaxed);
        stats.bytes = bytes.load(Ordering::Relaxed);
        stats.network = network.load(Ordering::Relaxed);
        stats.component_completed = group.load(Ordering::Relaxed);
        stats.downloaded = downloaded.load(Ordering::Relaxed);
        stats.reused = reused.load(Ordering::Relaxed);
        if let Some(error) = failure.into_inner().map_err(error)? {
            return Err(error);
        }
        check(cancel)
    }
    pub(super) fn loader_library(
        &self,
        library: &Value,
        provider: &str,
        cancel: &AtomicBool,
    ) -> Result<Download> {
        let coordinate = library["name"].as_str().ok_or("组件库缺少名称")?;
        let art = &library["downloads"]["artifact"];
        if art.is_object() {
            return artifact(
                art,
                format!(
                    "libraries/{}",
                    art["path"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or(maven_path(coordinate)?)
                ),
            );
        }
        let path = maven_path(coordinate)?;
        let repository = library["url"].as_str().unwrap_or(if provider == "fabric" {
            "https://maven.fabricmc.net/"
        } else {
            "https://libraries.minecraft.net/"
        });
        // Old official Forge profiles contain HTTP URLs for its own Maven.
        let repository = repository
            .replace(
                "http://files.minecraftforge.net/maven/",
                "https://maven.minecraftforge.net/",
            )
            .replace(
                "http://maven.minecraftforge.net/",
                "https://maven.minecraftforge.net/",
            );
        let url = Url::parse(&repository)
            .map_err(error)?
            .join(&path)
            .map_err(error)?;
        let hash = if let Some(hash) = library["sha1"].as_str().filter(|s| hash_valid(s)) {
            hash.to_string()
        } else {
            String::from_utf8(self.bytes(&format!("{url}.sha1"), 1024, cancel)?).map_err(error)?
        };
        let hash = hash
            .split_whitespace()
            .next()
            .filter(|x| hash_valid(x))
            .ok_or("组件库缺少官方 SHA1 校验信息")?;
        // Fabric's launch profile does not declare lengths. Resolve the length
        // through the verified artifact response, then record it in launch JSON.
        let size = match library["size"].as_u64() {
            Some(size) => size,
            None => self.head_size(&url, cancel)?,
        };
        Ok(Download {
            url: url.into(),
            hash: hash.into(),
            size,
            relative: format!("libraries/{path}"),
        })
    }
    pub(crate) fn install_component(
        &self,
        root: &InstallDir,
        temporary: &InstallDir,
        name: &str,
        minecraft: &str,
        vanilla: Value,
        component: &ComponentSelection,
        expected: Option<&resolved::ResolvedComponent>,
        cancel: &AtomicBool,
        stats: &mut ComponentStats,
        cb: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<Value> {
        cb(stats.event("component_metadata", "正在获取组件安装信息", 0.0));
        let workspace = temporary.directory("component-work", true)?;
        // Both public installation APIs use this parser. Only prepared native
        // evidence skips provider sidecars/HEAD; source bytes are still checked
        // before any dependency write or Java processor can execute.
        let loaded = self.load_component(minecraft, component, expected, cancel, stats, cb)?;
        let provider = loaded.evidence.selection.provider;
        let mut profile = loaded.evidence.profile;
        let descriptors = loaded.evidence.libraries;
        if provider == "fabric" {
            cb(stats.event("component_analyze", "正在分析 Fabric 支持库文件", 0.0));
            let libs = profile["libraries"]
                .as_array_mut()
                .ok_or("Fabric 依赖信息无效")?;
            let mut dependencies = Vec::new();
            let mut seen = HashSet::new();
            for library in libs.iter_mut() {
                check(cancel)?;
                if !pcl_core::library_allowed(library)? {
                    continue;
                }
                let path = library["downloads"]["artifact"]["path"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or(maven_path(
                        library["name"].as_str().ok_or("组件库缺少名称")?,
                    )?);
                let d = descriptors
                    .get(&format!("libraries/{path}"))
                    .ok_or("已解析的 Fabric 依赖缺失")?;
                if seen.insert(&d.relative) {
                    dependencies.push(d.clone());
                }
                library["downloads"] = json!({"artifact":{"url":d.url,"sha1":d.hash,"size":d.size,"path":d.relative.strip_prefix("libraries/").unwrap()}});
            }
            stats.component_total = dependencies.len() as u64;
            self.component_files(&workspace, root, &dependencies, cancel, stats, cb)?;
            cb(stats.event("component_install", "正在组装 Fabric 启动信息", 1.0));
        } else {
            let bytes = loaded.archive.ok_or("已解析的官方安装器缺失")?;
            let install = loaded.evidence.install.ok_or("已解析的官方安装信息缺失")?;
            let installer_path = workspace.file("installer.jar")?;
            installer_path.write(&bytes)?;
            stats.downloaded += 1;
            stats.total += 1;
            stats.completed += 1;
            stats.bytes += bytes.len() as u64;
            stats.byte_total += bytes.len() as u64;
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(error)?;
            cb(stats.event("component_analyze", "正在分析组件支持库文件", 0.0));
            let mut generated = vec![];
            let mut dependencies = Vec::new();
            stats.component_total = descriptors.len() as u64;
            for d in descriptors.values() {
                check(cancel)?;
                let entry_name =
                    format!("maven/{}", d.relative.strip_prefix("libraries/").unwrap());
                if archive.by_name(&entry_name).is_ok() {
                    let bytes = archive_bytes(&mut archive, &entry_name, MAX_COMPONENT)?;
                    if bytes.len() as u64 != d.size
                        || !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(&d.hash)
                    {
                        return Err("官方安装器内嵌组件校验失败".into());
                    }
                    let target = workspace.file(&d.relative)?;
                    target.prepare_parent()?;
                    target.write(&bytes)?;
                    stats.component_completed += 1;
                } else if d.url.is_empty() {
                    generated.push(d.clone());
                    stats.component_total -= 1;
                } else {
                    dependencies.push(d.clone());
                }
            }
            self.component_files(&workspace, root, &dependencies, cancel, stats, cb)?;
            let processors: Vec<_> = client_processors(&install).collect();
            if !processors.is_empty() {
                let java_major =
                    vanilla["javaVersion"]["majorVersion"].as_u64().unwrap_or(8) as u32;
                let java = self.installer_java_in(
                    java_major,
                    workspace.process_path()?,
                    workspace.is_bound(),
                    cancel,
                )?;
                let minecraft_jar = temporary.file(format!("{name}.jar"))?;
                let mut data = processor_data(
                    &install,
                    &workspace,
                    &minecraft_jar,
                    &installer_path,
                    &mut archive,
                )?;
                // Most mapping tasks consume the raw official text. Tasks with
                // --sanitize must still execute their official transformation.
                if let Some(target) = data.get("MOJMAPS") {
                    if let Some(mapping) = vanilla["downloads"].get("client_mappings") {
                        let path = Path::new(target);
                        let relative = data
                            .relative_output(&workspace, path)?
                            .to_string_lossy()
                            .into_owned();
                        let d = artifact(mapping, relative)?;
                        stats.component_total += 1;
                        self.component_file(&workspace, root, &d, cancel, stats, cb)?;
                    }
                }
                data.insert("SIDE".into(), "client".into());
                cb(stats.event("component_install", "正在安装组件", 0.0));
                for (index, processor) in processors.iter().enumerate() {
                    check(cancel)?;
                    let args: Vec<_> = processor["args"]
                        .as_array()
                        .ok_or("组件处理器缺少参数")?
                        .iter()
                        .map(|v| v.as_str().ok_or_else(|| "组件处理器参数无效".to_string()))
                        .collect::<Result<_>>()?;
                    let mappings_task = args
                        .windows(2)
                        .any(|pair| pair == ["--task", "DOWNLOAD_MOJMAPS"]);
                    if mappings_task
                        && !args.contains(&"--sanitize")
                        && data.get("MOJMAPS").is_some_and(|p| Path::new(p).is_file())
                    {
                        verify_processor_outputs(processor, &data, &workspace, cancel)?;
                        cb(stats.event(
                            "component_install",
                            "正在安装组件",
                            (index + 1) as f64 / processors.len() as f64,
                        ));
                        continue;
                    }
                    if root.is_bound() {
                        // Existing hard links can alias outside bytes despite
                        // an inode-based directory rule. Reject them before
                        // granting a processor writable file access.
                        root.files()?;
                    }
                    run_processor(&java, processor, &data, &workspace, cancel)?;
                    cb(stats.event(
                        "component_install",
                        "正在安装组件",
                        (index + 1) as f64 / processors.len() as f64,
                    ));
                }
                for key in ["MC_SRG", "MC_EXTRA", "PATCHED", "MC_OFF"] {
                    if let Some(path) = data.get(key) {
                        let path =
                            workspace.file(data.relative_output(&workspace, Path::new(path))?)?;
                        let file = path
                            .open()
                            .map_err(|e| format!("组件运行文件 {key} 尚未正确生成：{e}"))?;
                        let archive = zip::ZipArchive::new(file)
                            .map_err(|e| format!("组件运行文件 {key} 不是有效的 JAR：{e}"))?;
                        if archive.is_empty() {
                            return Err(format!("组件运行文件 {key} 为空"));
                        }
                        if let Some(hash) = data.get(&format!("{key}_SHA")) {
                            let size = path.open()?.metadata().map_err(error)?.len();
                            if !hash_valid(hash) || !verify(&path, hash, size, cancel)? {
                                return Err(format!("组件运行文件 {key} SHA1 校验失败"));
                            }
                        }
                    }
                }
            }
            // New NeoForge installers omit processor outputs. All launch-time
            // generated artifacts still carry hashes in version.json.
            for d in &generated {
                let target = workspace.file(&d.relative)?;
                if !verify(&target, &d.hash, d.size, cancel)? {
                    return Err(format!("组件处理结果校验失败：{}", d.relative));
                }
            }
        }
        cb(stats.event("component_install", "组件安装完成", 1.0));
        cb(stats.event("game_install", "正在组装游戏启动信息", 0.0));
        let mut result = merge_profile(vanilla, profile.clone(), minecraft)?;
        // The loader's launch JSON omits some runtime jars (patched Minecraft,
        // split extras, universal). Preserve the complete successful library
        // workspace after verifying declared and generated runtime artifacts.
        for relative in collect_libraries(&workspace)? {
            check(cancel)?;
            let source = workspace.file(&relative)?;
            let relative = relative.to_string_lossy().into_owned();
            if let Some(d) = descriptors.get(&relative) {
                if !verify(&source, &d.hash, d.size, cancel)? {
                    return Err("组件运行库校验失败".into());
                }
            }
        }
        record_runtime_downloads(&mut result, &descriptors)?;
        Ok(result)
    }
    pub(crate) fn publish_component_libraries(
        &self,
        root: &InstallDir,
        workspace: &InstallDir,
        cancel: &AtomicBool,
        stats: &ComponentStats,
        cb: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<()> {
        let files = collect_libraries(workspace)?;
        cb(stats.event("game_support", "正在安装游戏支持库文件", 0.0));
        for (index, relative) in files.iter().enumerate() {
            check(cancel)?;
            let source = workspace.file(relative)?;
            let destination = root.file(relative)?;
            let hash = file_hash(&source, cancel)?;
            let mut input = source.open()?;
            let size = input.metadata().map_err(error)?.len();
            if !verify(&destination, &hash, size, cancel)? {
                let mut output = destination.output(size)?;
                let copied =
                    copy_stream(&mut input, output.file_mut(), size, cancel).and_then(|actual| {
                        if actual.eq_ignore_ascii_case(&hash) {
                            Ok(())
                        } else {
                            Err("组件运行库在复制期间变化".into())
                        }
                    });
                match copied {
                    Ok(()) => output.publish()?,
                    Err(e) => return Err(output.close(e)),
                }
            }
            cb(stats.event(
                "game_support",
                "正在安装游戏支持库文件",
                (index + 1) as f64 / files.len().max(1) as f64,
            ));
        }
        Ok(())
    }
}
fn copy_stream(
    input: &mut fs::File,
    output: &mut fs::File,
    expected_size: u64,
    cancel: &AtomicBool,
) -> Result<String> {
    let before = pcl_core::retained_fs::metadata(input).map_err(error)?;
    if before.len != expected_size {
        return Err("组件复制源大小已变化".into());
    }
    let mut digest = Sha1::new();
    let mut received = 0u64;
    let mut buf = [0; 65536];
    while received < expected_size {
        check(cancel)?;
        let max = (expected_size - received).min(buf.len() as u64) as usize;
        let n = input.read(&mut buf[..max]).map_err(error)?;
        if n == 0 {
            return Err("组件复制源在读取期间缩短".into());
        }
        received += n as u64;
        digest.update(&buf[..n]);
        output.write_all(&buf[..n]).map_err(error)?;
    }
    check(cancel)?;
    if input.read(&mut buf[..1]).map_err(error)? != 0
        || pcl_core::retained_fs::metadata(input).map_err(error)? != before
    {
        return Err("组件复制源在读取期间变化".into());
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn copy_cancel(
    source: &bound_root::InstallPath,
    target: &bound_root::InstallPath,
    hash: &str,
    size: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut input = source.open()?;
    let mut output = target.output(size)?;
    let copied = copy_stream(&mut input, output.file_mut(), size, cancel).and_then(|actual| {
        if actual.eq_ignore_ascii_case(hash) {
            Ok(())
        } else {
            Err("组件缓存副本校验失败".into())
        }
    });
    match copied {
        Ok(()) => output.publish(),
        Err(e) => Err(output.close(e)),
    }
}
/// Strings passed to Java carry retained directory capabilities. Keep every
/// expansion alive through processor kill/wait; data paths are also used by
/// later processors and cannot be reopened through a mutable workspace name.
#[derive(Default)]
struct ProcessorData {
    values: HashMap<String, String>,
    paths: Mutex<Vec<bound_root::InstallPath>>,
    directories: Vec<(PathBuf, InstallDir)>,
}
impl std::ops::Deref for ProcessorData {
    type Target = HashMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
impl std::ops::DerefMut for ProcessorData {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.values
    }
}
impl ProcessorData {
    fn remember(&self, path: bound_root::InstallPath) -> Result<String> {
        let mut paths = self.paths.lock().unwrap();
        if let Some(existing) = paths
            .iter()
            .find(|existing| existing.relative() == path.relative())
        {
            return Ok(existing.process_path()?.to_string_lossy().into_owned());
        }
        let value = path.process_path()?.to_string_lossy().into_owned();
        paths.push(path);
        Ok(value)
    }
    fn relative_output(&self, workspace: &InstallDir, path: &Path) -> Result<PathBuf> {
        if let Ok(relative) = path.strip_prefix(workspace.process_path()?) {
            return Ok(relative.into());
        }
        for source in self.paths.lock().unwrap().iter() {
            if path == source.process_path()? {
                return Ok(source.relative().into());
            }
        }
        for (relative, directory) in &self.directories {
            if let Ok(suffix) = path.strip_prefix(directory.process_path()?) {
                return Ok(relative.join(suffix));
            }
        }
        Err("组件处理器输出超出临时目录".into())
    }
}
fn processor_data<R: Read + Seek>(
    profile: &Value,
    workspace: &InstallDir,
    minecraft_jar: &bound_root::InstallPath,
    installer: &bound_root::InstallPath,
    archive: &mut zip::ZipArchive<R>,
) -> Result<ProcessorData> {
    let libraries = workspace.directory("libraries", true)?;
    let mut data = ProcessorData::default();
    data.directories
        .push((PathBuf::from("libraries"), libraries.clone()));
    for (key, entry) in profile["data"].as_object().into_iter().flatten() {
        let Some(raw) = entry["client"].as_str() else {
            continue;
        };
        let value = if let Some(coordinate) =
            raw.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
        {
            let path = workspace.file(format!("libraries/{}", maven_path(coordinate)?))?;
            path.prepare_parent()?;
            data.remember(path)?
        } else if let Some(literal) = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            literal.to_string()
        } else {
            let archive_path = raw.trim_start_matches('/');
            let path = workspace.file(format!("extracted/{archive_path}"))?;
            path.prepare_parent()?;
            path.write(&archive_bytes(archive, archive_path, MAX_COMPONENT)?)?;
            data.remember(path)?
        };
        data.insert(key.clone(), value);
    }
    for (key, value) in [
        ("SIDE", "client".to_string()),
        (
            "MINECRAFT_VERSION",
            profile["minecraft"]
                .as_str()
                .ok_or("组件安装器缺少 Minecraft 版本")?
                .into(),
        ),
        (
            "MINECRAFT_JAR",
            minecraft_jar.process_path()?.to_string_lossy().into_owned(),
        ),
        (
            "ROOT",
            workspace.process_path()?.to_string_lossy().into_owned(),
        ),
        (
            "LIBRARY_DIR",
            libraries.process_path()?.to_string_lossy().into_owned(),
        ),
        (
            "INSTALLER",
            installer.process_path()?.to_string_lossy().into_owned(),
        ),
    ] {
        data.insert(key.into(), value);
    }
    Ok(data)
}
fn expand(raw: &str, data: &ProcessorData, workspace: &InstallDir) -> Result<String> {
    if let Some(coordinate) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return data.remember(workspace.file(format!("libraries/{}", maven_path(coordinate)?))?);
    }
    let raw = raw
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(raw);
    let mut result = String::new();
    let mut rest = raw;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let end = rest[start..]
            .find('}')
            .ok_or("组件处理器参数缺少闭合标记")?
            + start;
        result.push_str(
            data.get(&rest[start + 1..end])
                .ok_or("组件处理器引用了未知参数")?,
        );
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}
fn main_class(file: impl Read + Seek) -> Result<String> {
    let mut archive = zip::ZipArchive::new(file).map_err(error)?;
    let raw = String::from_utf8(archive_bytes(
        &mut archive,
        "META-INF/MANIFEST.MF",
        1024 * 1024,
    )?)
    .map_err(error)?;
    let mut lines: Vec<String> = vec![];
    for line in raw.lines() {
        if let Some(cont) = line.strip_prefix(' ') {
            if let Some(last) = lines.last_mut() {
                last.push_str(cont);
            }
        } else {
            lines.push(line.into());
        }
    }
    lines
        .iter()
        .find_map(|line| line.strip_prefix("Main-Class: ").map(str::trim))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or("组件处理器缺少 Main-Class".into())
}
fn run_processor(
    java: &Path,
    processor: &Value,
    data: &ProcessorData,
    workspace: &InstallDir,
    cancel: &AtomicBool,
) -> Result<()> {
    let jar = processor["jar"].as_str().ok_or("组件处理器缺少可执行库")?;
    let jar = workspace.file(format!("libraries/{}", maven_path(jar)?))?;
    let mut classpath = vec![jar.clone()];
    for coordinate in processor["classpath"].as_array().into_iter().flatten() {
        classpath.push(workspace.file(format!(
            "libraries/{}",
            maven_path(coordinate.as_str().ok_or("组件处理器依赖无效")?)?
        ))?);
    }
    let args = processor["args"]
        .as_array()
        .ok_or("组件处理器参数无效")?
        .iter()
        .map(|v| expand(v.as_str().ok_or("组件处理器参数无效")?, data, workspace))
        .collect::<Result<Vec<_>>>()?;
    let classpath_files = classpath
        .iter()
        .map(bound_root::InstallPath::open)
        .collect::<Result<Vec<_>>>()?;
    let classpath_argument = if workspace.is_bound() {
        std::env::join_paths(
            classpath_files
                .iter()
                .map(bound_root::anchor)
                .collect::<Result<Vec<_>>>()?,
        )
        .map_err(error)?
    } else {
        std::env::join_paths(
            classpath
                .iter()
                .map(|path| path.process_path())
                .collect::<Result<Vec<_>>>()?,
        )
        .map_err(error)?
    };
    let mut command = Command::new(java);
    if workspace.is_bound() {
        command
            .arg(format!(
                "-Djava.io.tmpdir={}",
                workspace.process_path()?.display()
            ))
            .env("TMPDIR", workspace.process_path()?);
    }
    command
        .args(["-Djava.awt.headless=true", "-cp"])
        .arg(classpath_argument)
        .arg(main_class(jar.open()?)?)
        .args(args)
        .current_dir(workspace.process_path()?);
    // Each processor owns a fresh log. Reopening/truncating one shared name
    // could follow an externally planted hard-link alias on the next processor.
    let log = tempfile::Builder::new()
        .prefix(".processor-")
        .tempfile_in(workspace.process_path()?)
        .map_err(error)?;
    command
        .stdin(if workspace.is_bound() {
            Stdio::from(fs::File::open("/dev/null").map_err(error)?)
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(log.as_file().try_clone().map_err(error)?))
        .stderr(Stdio::from(log.as_file().try_clone().map_err(error)?));
    let status = run_child(&mut command, cancel, Duration::from_secs(15 * 60))?;
    if !status.success() {
        let bytes = fs::read(log.path()).unwrap_or_default();
        let tail = &bytes[bytes.len().saturating_sub(4096)..];
        return Err(format!(
            "组件安装处理器失败（{status}）：{}",
            String::from_utf8_lossy(tail)
        ));
    }
    verify_processor_outputs(processor, data, workspace, cancel)
}
fn verify_processor_outputs(
    processor: &Value,
    data: &ProcessorData,
    workspace: &InstallDir,
    cancel: &AtomicBool,
) -> Result<()> {
    for (raw_path, expected) in processor["outputs"].as_object().into_iter().flatten() {
        let path = PathBuf::from(expand(raw_path, data, workspace)?);
        let relative = data.relative_output(workspace, &path)?;
        let path = workspace.file(relative)?;
        let hash = expand(
            expected.as_str().ok_or("组件处理器输出校验信息无效")?,
            data,
            workspace,
        )?;
        if !hash_valid(&hash) {
            return Err("组件处理器输出缺少有效 SHA1".into());
        }
        let size = path.open()?.metadata().map_err(error)?.len();
        if !verify(&path, &hash, size, cancel)? {
            return Err("组件处理器输出 SHA1 校验失败".into());
        }
    }
    Ok(())
}
fn file_hash(path: &bound_root::InstallPath, cancel: &AtomicBool) -> Result<String> {
    let mut digest = Sha1::new();
    let mut input = path.open()?;
    let mut buf = [0; 65536];
    loop {
        check(cancel)?;
        let n = input.read(&mut buf).map_err(error)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn collect_libraries(workspace: &InstallDir) -> Result<Vec<PathBuf>> {
    if !workspace.entry_exists("libraries")? {
        return Ok(vec![]);
    }
    let libraries = workspace.directory("libraries", false)?;
    Ok(libraries
        .files()?
        .into_iter()
        .map(|p| Path::new("libraries").join(p))
        .collect())
}

fn run_child(
    command: &mut Command,
    cancel: &AtomicBool,
    limit: Duration,
) -> Result<std::process::ExitStatus> {
    check(cancel)?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("无法运行 Java 安装处理器：{e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error(e));
            }
        }
        if cancel.load(Ordering::Relaxed) || start.elapsed() > limit {
            // Wait for the JVM to exit before deleting its workspace or marking
            // the task cancelled. A failed stop remains a cleanup failure.
            if let Err(e) = child.kill() {
                if child.try_wait().map_err(error)?.is_none() {
                    return Err(format!("取消清理失败：无法终止 Java 处理器：{e}"));
                }
            }
            child
                .wait()
                .map_err(|e| format!("取消清理失败：等待 Java 处理器退出失败：{e}"))?;
            return if cancel.load(Ordering::Relaxed) {
                Err("安装已取消".into())
            } else {
                Err("组件安装处理器运行超时".into())
            };
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}
pub(super) fn client_processors(install: &Value) -> impl Iterator<Item = &Value> {
    install["processors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|processor| {
            processor["sides"]
                .as_array()
                .is_none_or(|sides| sides.iter().any(|side| side.as_str() == Some("client")))
        })
}

enum JavaProbe {
    Major(u32),
    Unavailable(String),
}

/// Probe failures may be skipped during automatic discovery. Cancellation and
/// workspace failures remain errors; both selection modes retain the child
/// owner until kill/wait finishes before their caller can clean its workspace.
fn probe_installer_java(
    path: &Path,
    workspace: &Path,
    private: bool,
    cancel: &AtomicBool,
) -> Result<JavaProbe> {
    let log = tempfile::NamedTempFile::new_in(workspace).map_err(error)?;
    let mut command = Command::new(path);
    if private {
        command
            .arg(format!("-Djava.io.tmpdir={}", workspace.display()))
            .env("TMPDIR", workspace);
    }
    command
        .arg("-version")
        .stdin(if private {
            Stdio::from(fs::File::open("/dev/null").map_err(error)?)
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(log.reopen().map_err(error)?))
        .stderr(Stdio::from(log.reopen().map_err(error)?));
    let status = match run_child(&mut command, cancel, Duration::from_secs(5)) {
        Ok(status) => status,
        Err(e) if cancel.load(Ordering::Relaxed) || e.starts_with("取消清理失败：") => {
            return Err(e)
        }
        Err(e) => return Ok(JavaProbe::Unavailable(e)),
    };
    if !status.success() {
        return Ok(JavaProbe::Unavailable(format!(
            "Java 版本探测退出状态：{status}"
        )));
    }
    let text = fs::read_to_string(log.path()).unwrap_or_default();
    let major = text
        .split("version \"")
        .nth(1)
        .and_then(|s| s.strip_prefix("1.").or(Some(s)))
        .and_then(|s| s.split(['.', '"', '-']).next())
        .and_then(|s| s.parse::<u32>().ok());
    Ok(match major {
        Some(major) => JavaProbe::Major(major),
        None => JavaProbe::Unavailable("Java 版本探测结果缺少有效的主版本".into()),
    })
}

impl Installer {
    #[cfg(all(test, unix))]
    fn installer_java(
        &self,
        required: u32,
        workspace: &Path,
        cancel: &AtomicBool,
    ) -> Result<PathBuf> {
        self.installer_java_in(required, workspace, false, cancel)
    }
    fn installer_java_in(
        &self,
        required: u32,
        workspace: &Path,
        private: bool,
        cancel: &AtomicBool,
    ) -> Result<PathBuf> {
        check(cancel)?;
        if let Some(path) = &self.java {
            // A caller-selected executable is authority, not a discovery hint.
            // Resolve/probe it for this processor batch; never replace a failed
            // selection with an otherwise compatible project or system Java.
            let path = fs::canonicalize(path)
                .map_err(|e| format!("指定的 Java 不可用（{}）：{e}", path.display()))?;
            return match probe_installer_java(&path, workspace, private, cancel)? {
                JavaProbe::Major(major) if major == required => Ok(path),
                JavaProbe::Major(major) => Err(format!(
                    "指定的 Java 主版本不兼容（{}）：此组件需要 Java {required}，指定的是 Java {major}",
                    path.display()
                )),
                JavaProbe::Unavailable(reason) => Err(format!(
                    "指定的 Java 不可用（{}）：{reason}",
                    path.display()
                )),
            };
        }
        let mut paths = vec![];
        let project = self
            .project
            .clone()
            .or_else(|| std::env::current_dir().ok());
        if let Some(project) = project {
            paths.extend(pcl_core::java::managed_runtime_paths(&project));
            for base in [project.clone(), project.join("PCL-Linux")] {
                if let Ok(entries) = fs::read_dir(base) {
                    for entry in entries.flatten() {
                        if entry.file_name().to_string_lossy().starts_with("runtime") {
                            paths.push(entry.path().join("bin/java"));
                        }
                    }
                }
            }
        }
        if let Some(home) = std::env::var_os("JAVA_HOME") {
            paths.push(PathBuf::from(home).join("bin/java"));
        }
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path).map(|p| p.join("java")));
        }
        if let Ok(entries) = fs::read_dir("/usr/lib/jvm") {
            paths.extend(entries.flatten().map(|e| e.path().join("bin/java")));
        }
        let mut seen = HashSet::new();
        for path in paths {
            check(cancel)?;
            let Ok(path) = fs::canonicalize(path) else {
                continue;
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            match probe_installer_java(&path, workspace, private, cancel)? {
                JavaProbe::Major(major) if major == required => return Ok(path),
                _ => {}
            }
        }
        Err(format!(
            "安装此组件需要 Java {required}，请在 Java 设置中下载或添加兼容版本后重试"
        ))
    }
}

#[cfg(all(test, unix))]
#[path = "components/java_tests.rs"]
mod java_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[test]
    fn installer_runtime_evidence_is_consumed_without_changing_launch_libraries() {
        let mut metadata = json!({"id":"fixture",
            "mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher","libraries":[],
            "arguments":{"game":["--launchTarget","forgeclient","--fml.forgeGroup",
                "net.minecraftforge","--fml.mcVersion","1.20.1","--fml.forgeVersion",
                "47.4.0","--fml.mcpVersion","20230612.114412"]}});
        let original = metadata.clone();
        let paths = pcl_core::repair::implicit_runtime_paths(&metadata).unwrap();
        let descriptor = |relative: &str, url: &str| Download {
            relative: relative.into(),
            url: url.into(),
            hash: "1".repeat(40),
            size: 123,
        };
        let descriptors = BTreeMap::from([
            (
                paths[0].clone(),
                descriptor(&paths[0], "https://maven.minecraftforge.net/fmlcore.jar"),
            ),
            (paths[7].clone(), descriptor(&paths[7], "")),
            (
                "libraries/unrelated.jar".into(),
                descriptor(
                    "libraries/unrelated.jar",
                    "https://maven.minecraftforge.net/unrelated.jar",
                ),
            ),
        ]);
        record_runtime_downloads(&mut metadata, &descriptors).unwrap();
        assert_eq!(metadata["libraries"], original["libraries"]);
        assert_eq!(metadata["arguments"], original["arguments"]);
        assert_eq!(metadata["pclRuntimeFiles"].as_array().unwrap().len(), 1);
        let captured =
            pcl_core::repair::resolve_metadata("fixture", |_| Ok(metadata.clone()), |_| Ok(true))
                .unwrap();
        let required = pcl_core::repair::required_files(&captured).unwrap();
        let runtime = required
            .files
            .iter()
            .find(|file| file.label == paths[0])
            .unwrap();
        assert_eq!(runtime.download.as_ref().unwrap().sha1, "1".repeat(40));
        assert!(runtime.unsupported.is_none());
        assert!(required
            .files
            .iter()
            .all(|file| file.label != "libraries/unrelated.jar"));
        assert!(required
            .files
            .iter()
            .find(|file| file.label == paths[7])
            .unwrap()
            .unsupported
            .is_some());
    }

    #[test]
    #[cfg(unix)]
    fn cancellation_kills_and_reaps_running_processor_before_return() {
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        let mut command = Command::new("/bin/sleep");
        command
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                cancelled.store(true, Ordering::Relaxed);
            });
            assert!(run_child(&mut command, &cancelled, Duration::from_secs(10))
                .unwrap_err()
                .contains("取消"));
        });
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn concurrent_download_steps_retain_labels_and_own_completion() {
        let request = InstallRequest {
            minecraft: "fixture".into(),
            name: "fixture".into(),
            components: vec![ComponentSelection {
                provider: "NeoForge".into(),
                version: "1".into(),
            }],
        };
        let mut steps = initial_steps(&request);
        let stats = ComponentStats::new(10, 100, 100, 10, 0);
        let mut progress = stats.event("downloading", "", 0.0);
        progress.steps = vec![download_hint("libraries/library.jar", 1, 2)];
        advance_steps(&mut steps, &progress);
        progress.steps = vec![download_hint("assets/indexes/index.json", 1, 1)];
        advance_steps(&mut steps, &progress);
        assert_eq!(steps[1].state, "running");
        assert_eq!(steps[1].progress, Some(0.5));
        assert_eq!(steps[2].state, "complete");
        advance_steps(&mut steps, &stats.event("component_metadata", "", 0.0));
        assert_eq!(steps.len(), 9);
        assert!(steps.iter().all(|step| !step.label.is_empty()));
    }
    #[test]
    fn output_hash_verification_is_kept_when_mapping_task_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mappings.txt");
        fs::write(&path, b"raw mappings").unwrap();
        let data = HashMap::from([("MOJMAPS".into(), path.to_string_lossy().into_owned())]);
        let processor =
            json!({"outputs":{"{MOJMAPS}":"'0000000000000000000000000000000000000000'"}});
        assert!(verify_processor_outputs(
            &processor,
            &ProcessorData {
                values: data,
                ..Default::default()
            },
            &InstallDir::legacy(root.path().into()),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .contains("SHA1"));
    }
}
