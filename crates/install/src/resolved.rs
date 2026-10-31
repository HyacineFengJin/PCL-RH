//! Native installation authority. Resolution reads official metadata without
//! writing an installation root or running Java. Execution shares the ordinary
//! install transaction, but reuses the captured metadata and library evidence.
//! Loader source bytes are fetched again and must match the captured SHA1/size;
//! this object intentionally owns no installer archive or executable FD.
use super::*;
use components::{archive_bytes, maven_path, ComponentStats, MAX_COMPONENT};
use std::collections::BTreeMap;

/// An installation resolved by native code, never accepted from serialized IPC.
/// Keep it with its configured Installer in a bounded authority cache. The
/// caller owns outer publication when installing into a private modpack root.
pub struct ResolvedInstallRequest {
    request: InstallRequest,
    pub(super) vanilla: ResolvedVanilla,
    pub(super) component: Option<ResolvedComponent>,
}

/// Only a processor batch needs an installation runtime. Its exact-major
/// contract is distinct from the Java eventually used to launch the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessorJavaRequirement {
    pub major: u32,
    pub exact: bool,
}
pub(super) struct ResolvedVanilla {
    descriptor: Download,
    pub(super) metadata: Value,
    required_java: u32,
}
pub(super) struct ResolvedComponent {
    pub(super) selection: ComponentSelection,
    source: ComponentSource,
    pub(super) profile: Value,
    pub(super) install: Option<Value>,
    pub(super) libraries: BTreeMap<String, Download>,
}
enum ComponentSource {
    Fabric(Download),
    Installer {
        coordinate: String,
        download: Download,
    },
}
pub(super) struct LoadedComponent {
    pub(super) evidence: ResolvedComponent,
    pub(super) archive: Option<Vec<u8>>,
}

impl ResolvedInstallRequest {
    pub fn request(&self) -> &InstallRequest {
        &self.request
    }
    pub fn minecraft(&self) -> &str {
        &self.request.minecraft
    }
    pub fn required_java(&self) -> u32 {
        self.vanilla.required_java
    }
    /// Forge/NeoForge processor batches use the existing exact-major policy;
    /// Fabric and vanilla do not execute installation processors.
    pub fn requires_exact_java(&self) -> bool {
        self.component.as_ref().is_some_and(|c| c.install.is_some())
    }
    /// Pure captured-metadata inspection: legacy Forge spec0 and loaders with
    /// no client processors perform no Java discovery or installation work.
    pub fn processor_java_requirement(&self) -> Result<Option<ProcessorJavaRequirement>> {
        let processors = self
            .component
            .as_ref()
            .and_then(|component| component.install.as_ref());
        if processors.is_none_or(|install| components::client_processors(install).next().is_none())
        {
            return Ok(None);
        }
        let major = self.vanilla.required_java;
        if major == 0 || major > 999 {
            return Err("组件要求的 Java 主版本无效".into());
        }
        Ok(Some(ProcessorJavaRequirement { major, exact: true }))
    }
    /// Reject only a known missing platform backend for a private bound install.
    /// This inspects captured client processors, not the loader's name. Success
    /// does not prove that this host's kernel supports confinement: Linux still
    /// checks Landlock/openat2 in its owned worker before installation writes.
    /// The ordinary path-based installation API keeps its existing contract.
    pub fn check_bound_platform_support(&self) -> Result<()> {
        #[cfg(not(target_os = "linux"))]
        if self.processor_java_requirement()?.is_some() {
            return Err("此平台的 Java 安装处理器写入隔离尚未可用".into());
        }
        Ok(())
    }
    /// The launch metadata represented by this captured native authority.
    /// This performs no HTTP or Java work and retains no extra cached value.
    /// Pack adapters can compare their declared runtime against the same profile
    /// that execution will write, including Fabric's resolved download evidence.
    pub fn launch_profile(&self) -> Result<Value> {
        let mut metadata = self.vanilla.metadata.clone();
        if let Some(component) = &self.component {
            let mut profile = component.profile.clone();
            if component.selection.provider == "fabric" {
                for library in profile["libraries"]
                    .as_array_mut()
                    .ok_or("Fabric 依赖信息无效")?
                {
                    if !pcl_core::library_allowed(library)? {
                        continue;
                    }
                    let path = library["downloads"]["artifact"]["path"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or(maven_path(
                            library["name"].as_str().ok_or("组件库缺少名称")?,
                        )?);
                    let descriptor = component
                        .libraries
                        .get(&format!("libraries/{path}"))
                        .ok_or("已解析的 Fabric 依赖缺失")?;
                    library["downloads"] = serde_json::json!({"artifact": {
                        "url": descriptor.url, "sha1": descriptor.hash, "size": descriptor.size,
                        "path": descriptor.relative.strip_prefix("libraries/").ok_or("组件依赖路径无效")?
                    }});
                }
            }
            metadata = components::merge_profile(metadata, profile, &self.request.minecraft)?;
            components::record_runtime_downloads(&mut metadata, &component.libraries)?;
        }
        metadata["id"] = Value::String(self.request.name.clone());
        metadata["jar"] = Value::String(self.request.name.clone());
        metadata["clientVersion"] = Value::String(self.request.minecraft.clone());
        metadata
            .as_object_mut()
            .ok_or("版本信息无效")?
            .remove("inheritsFrom");
        Ok(metadata)
    }
    /// Conservative retained-size charge for a caller's bounded native cache.
    /// Includes parsed JSON, descriptors and request strings, not transient
    /// installer/download buffers or the separately retained Installer client.
    pub fn heap_bytes(&self) -> usize {
        let mut size = std::mem::size_of::<Self>()
            .saturating_add(self.request.minecraft.capacity())
            .saturating_add(self.request.name.capacity())
            .saturating_add(
                self.request.components.capacity() * std::mem::size_of::<ComponentSelection>(),
            )
            .saturating_add(download_heap(&self.vanilla.descriptor))
            .saturating_add(value_heap(&self.vanilla.metadata));
        for c in &self.request.components {
            size = size
                .saturating_add(c.provider.capacity())
                .saturating_add(c.version.capacity());
        }
        if let Some(c) = &self.component {
            size = size
                .saturating_add(std::mem::size_of::<ResolvedComponent>())
                .saturating_add(c.selection.provider.capacity())
                .saturating_add(c.selection.version.capacity())
                .saturating_add(value_heap(&c.profile));
            if let Some(install) = &c.install {
                size = size.saturating_add(value_heap(install));
            }
            let download = match &c.source {
                ComponentSource::Fabric(d) => d,
                ComponentSource::Installer {
                    coordinate,
                    download,
                } => {
                    size = size.saturating_add(coordinate.capacity());
                    download
                }
            };
            size = size.saturating_add(download_heap(download));
            for (key, d) in &c.libraries {
                // A charge larger than BTreeMap's per-entry allocation keeps
                // this useful as an admission bound, not a precise allocator API.
                size = size
                    .saturating_add(256)
                    .saturating_add(key.capacity())
                    .saturating_add(download_heap(d));
            }
        }
        size
    }
}
fn download_heap(d: &Download) -> usize {
    std::mem::size_of::<Download>() + d.url.capacity() + d.hash.capacity() + d.relative.capacity()
}
fn value_heap(v: &Value) -> usize {
    match v {
        Value::String(s) => s.capacity(),
        Value::Array(a) => a
            .iter()
            .fold(a.capacity() * std::mem::size_of::<Value>(), |n, v| {
                n.saturating_add(value_heap(v))
            }),
        Value::Object(m) => m.iter().fold(0usize, |n, (k, v)| {
            n.saturating_add(256)
                .saturating_add(k.capacity())
                .saturating_add(value_heap(v))
        }),
        _ => 0,
    }
}
fn verify_bytes(raw: &[u8], d: &Download, message: &str) -> Result<()> {
    if raw.len() as u64 != d.size
        || !format!("{:x}", Sha1::digest(raw)).eq_ignore_ascii_case(&d.hash)
    {
        return Err(message.into());
    }
    Ok(())
}
fn java_major(metadata: &Value) -> Result<u32> {
    match metadata.get("javaVersion") {
        None => Ok(8),
        Some(v) => v["majorVersion"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| "版本 Java 要求无效".into()),
    }
}
fn metadata_path(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.contains(['\\', '\0'])
        || path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("安装信息包含不安全的文件路径".into());
    }
    Ok(())
}

impl Installer {
    /// Capture an exact officially verified vanilla/loader plan without writing
    /// an installation root or probing/running Java. Retain the returned native
    /// object together with this configured Installer until admission/execution.
    pub fn resolve_request(
        &self,
        request: &InstallRequest,
        cancel: &AtomicBool,
    ) -> Result<ResolvedInstallRequest> {
        request.validate()?;
        check(cancel)?;
        let vanilla = self.resolve_vanilla(request, cancel)?;
        self.validate_resolved_vanilla(&vanilla.metadata, cancel)?;
        let component = request
            .components
            .first()
            .map(|c| {
                let mut stats = ComponentStats::new(0, 0, 0, 0, 0);
                self.load_component(&request.minecraft, c, None, cancel, &mut stats, &|_| {})
                    .map(|loaded| loaded.evidence)
            })
            .transpose()?;
        if let Some(c) = &component {
            if c.profile.get("javaVersion").is_some()
                && java_major(&c.profile)? != vanilla.required_java
            {
                return Err("组件与原版的 Java 要求不匹配".into());
            }
        }
        check(cancel)?;
        Ok(ResolvedInstallRequest {
            request: request.clone(),
            vanilla,
            component,
        })
    }
    /// Execute the captured plan through the ordinary owned-stage transaction.
    /// Loader source changes fail instead of replacing the confirmed evidence.
    /// For modpacks, `root` is the caller's private root; outer atomic publication
    /// and recovery remain the caller's responsibility. Cancellation cleanup
    /// keeps the existing commit boundary: a successfully published version wins.
    pub fn install_resolved_request(
        &self,
        root: &Path,
        resolved: &ResolvedInstallRequest,
        cancel: &AtomicBool,
        on_progress: impl Fn(Progress) + Send + Sync,
    ) -> Result<InstallResult> {
        self.install_request_with_resolution(
            Some(root),
            resolved.request(),
            Some(resolved),
            None,
            cancel,
            on_progress,
        )
    }
    /// Install into a caller-owned private root without reopening its pathname.
    /// The scoped worker retains root/parent FDs and confines destination paths,
    /// including Java-derived paths, to that directory. It does not isolate
    /// against other same-user processes inserting concurrent hard-link aliases.
    /// Linux openat2 and Landlock
    /// ABI 3 are required; missing capabilities fail before any root write.
    /// The caller's permissions and the ordinary pathname API are unchanged.
    pub fn install_resolved_request_bound(
        &self,
        root: fs::File,
        resolved: &ResolvedInstallRequest,
        cancel: &AtomicBool,
        on_progress: impl Fn(Progress) + Send + Sync,
    ) -> Result<InstallResult> {
        check(cancel)?;
        resolved.check_bound_platform_support()?;
        let root = InstallDir::bound(root)?;
        // Initialize the shared HTTP executor outside the confined worker. It
        // only handles HTTP futures; consume/copy/persist run on this worker.
        network::runtime();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    #[cfg(target_os = "linux")]
                    bound_root::confine_writes(root.fd().unwrap())?;
                    #[cfg(not(target_os = "linux"))]
                    if resolved.processor_java_requirement()?.is_some() {
                        // Handle-only metadata/download installation executes no
                        // third-party code. Java batches require a real process
                        // confinement backend, never a successful empty stub.
                        bound_root::confine_writes(root.fd().unwrap())?;
                    }
                    let root = root.reanchor();
                    self.install_request_with_resolution(
                        None,
                        resolved.request(),
                        Some(resolved),
                        Some(root.clone()),
                        cancel,
                        on_progress,
                    )
                })
                .join()
                .map_err(|_| "私有安装工作线程异常退出".to_string())?
        })
    }
    pub(super) fn resolve_vanilla(
        &self,
        request: &InstallRequest,
        cancel: &AtomicBool,
    ) -> Result<ResolvedVanilla> {
        let manifest = self.manifest(cancel)?;
        let entry = manifest["versions"]
            .as_array()
            .ok_or("Invalid catalog")?
            .iter()
            .find(|v| v["id"].as_str() == Some(request.minecraft.as_str()))
            .ok_or("所选版本不在 Mojang 官方版本目录中")?;
        let url = entry["url"].as_str().ok_or("Catalog lacks URL")?;
        let hash = entry["sha1"]
            .as_str()
            .filter(|h| hash_valid(h))
            .ok_or("Catalog lacks version SHA1")?;
        let raw = self.bytes(url, MAX_METADATA, cancel)?;
        let descriptor = Download {
            url: url.into(),
            hash: hash.into(),
            size: raw.len() as u64,
            relative: String::new(),
        };
        verify_bytes(&raw, &descriptor, "版本信息 SHA1 校验失败")?;
        let metadata: Value = serde_json::from_slice(&raw).map_err(error)?;
        if metadata["id"].as_str() != Some(request.minecraft.as_str())
            || metadata.get("inheritsFrom").is_some()
        {
            return Err("原版版本信息中的版本标识无效".into());
        }
        let required_java = java_major(&metadata)?;
        Ok(ResolvedVanilla {
            descriptor,
            metadata,
            required_java,
        })
    }
    fn validate_resolved_vanilla(&self, metadata: &Value, cancel: &AtomicBool) -> Result<()> {
        // Preparation reports the same unsupported legacy layouts execution
        // would reject; it creates no cache or version directory to do so.
        let client = artifact(&metadata["downloads"]["client"], "client.jar".into())?;
        self.url(&client.url)?;
        if !metadata["mainClass"].is_string() {
            return Err("原版版本信息缺少启动入口".into());
        }
        let index_id = metadata["assetIndex"]["id"]
            .as_str()
            .ok_or("Version lacks asset index")?;
        pcl_core::identifier(index_id)?;
        let index = artifact(
            &metadata["assetIndex"],
            format!("assets/indexes/{index_id}.json"),
        )?;
        let raw = self.bytes(&index.url, MAX_METADATA, cancel)?;
        verify_bytes(&raw, &index, "资源索引 SHA1 或大小校验失败")?;
        let data: Value = serde_json::from_slice(&raw).map_err(error)?;
        if data["virtual"].as_bool() == Some(true)
            || data["map_to_resources"].as_bool() == Some(true)
        {
            return Err("暂不支持旧版本的虚拟资源或 resources 资源布局".into());
        }
        for object in data["objects"]
            .as_object()
            .ok_or("Invalid asset index")?
            .values()
        {
            check(cancel)?;
            object["hash"]
                .as_str()
                .filter(|h| hash_valid(h))
                .ok_or("Invalid asset hash")?;
            object["size"].as_u64().ok_or("Invalid asset size")?;
        }
        let validate_library_artifact = |art: &Value| -> Result<()> {
            let d = artifact(
                art,
                format!(
                    "libraries/{}",
                    art["path"].as_str().ok_or("Library lacks path")?
                ),
            )?;
            self.url(&d.url)?;
            metadata_path(&d.relative)
        };
        for library in metadata["libraries"]
            .as_array()
            .ok_or("Version lacks libraries")?
        {
            if !pcl_core::library_allowed(library)? {
                continue;
            }
            if library.get("downloads").is_none() {
                return Err("暂不支持缺少官方校验信息的旧版本依赖库".into());
            }
            if !library["downloads"]["artifact"].is_null() {
                validate_library_artifact(&library["downloads"]["artifact"])?;
            }
            if let Some(class) =
                pcl_core::platform::Platform::current()?.legacy_native_classifier(library)?
            {
                validate_library_artifact(&library["downloads"]["classifiers"][class])?;
            }
        }
        if let Some(file) = metadata["logging"]["client"].get("file") {
            let id = file["id"].as_str().ok_or("Logging config lacks ID")?;
            pcl_core::identifier(id)?;
            let d = artifact(file, format!("assets/log_configs/{id}"))?;
            self.url(&d.url)?;
        }
        if let Some(mapping) = metadata["downloads"].get("client_mappings") {
            let d = artifact(mapping, "client-mappings.txt".into())?;
            self.url(&d.url)?;
        }
        Ok(())
    }
    pub(super) fn load_component(
        &self,
        minecraft: &str,
        component: &ComponentSelection,
        expected: Option<&ResolvedComponent>,
        cancel: &AtomicBool,
        stats: &mut ComponentStats,
        cb: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<LoadedComponent> {
        check(cancel)?;
        let selection = normalized_component(minecraft, component)?;
        let provider = selection.provider.as_str();
        let (source, profile, install, archive) = if provider == "fabric" {
            let mut url =
                Url::parse("https://meta.fabricmc.net/v2/versions/loader/").map_err(error)?;
            url.path_segments_mut()
                .map_err(|_| "组件地址无效")?
                .pop_if_empty()
                .push(minecraft)
                .push(&selection.version)
                .push("profile")
                .push("json");
            let url = self.component_url(url.as_str())?;
            if let Some(e) = expected {
                let ComponentSource::Fabric(d) = &e.source else {
                    return Err("解析的组件类型不匹配".into());
                };
                if d.url != url {
                    return Err("解析的组件地址不匹配".into());
                }
            }
            let raw = self.component_bytes(&url, MAX_METADATA, cancel, stats, cb)?;
            let download = Download {
                url,
                hash: format!("{:x}", Sha1::digest(&raw)),
                size: raw.len() as u64,
                relative: String::new(),
            };
            if let Some(e) = expected {
                let ComponentSource::Fabric(d) = &e.source else {
                    return Err("解析的组件类型不匹配".into());
                };
                verify_bytes(&raw, d, "Fabric 启动信息已改变，请重新确认安装")?;
            }
            let profile: Value = serde_json::from_slice(&raw).map_err(error)?;
            if profile["inheritsFrom"].as_str() != Some(minecraft) {
                return Err("Fabric 不支持所选 Minecraft 版本".into());
            }
            (ComponentSource::Fabric(download), profile, None, None)
        } else {
            let coordinate = installer_coordinate(&selection);
            let repository = if provider == "forge" {
                "https://maven.minecraftforge.net/"
            } else {
                "https://maven.neoforged.net/releases/"
            };
            let url = self.component_url(&format!("{repository}{}", maven_path(&coordinate)?))?;
            let hash = match expected {
                Some(e) => {
                    let ComponentSource::Installer {
                        coordinate: old,
                        download,
                    } = &e.source
                    else {
                        return Err("解析的组件类型不匹配".into());
                    };
                    if old != &coordinate || download.url != url {
                        return Err("解析的组件坐标不匹配".into());
                    }
                    download.hash.clone()
                }
                None => {
                    let raw =
                        self.component_bytes(&format!("{url}.sha1"), 1024, cancel, stats, cb)?;
                    String::from_utf8(raw)
                        .map_err(error)?
                        .split_whitespace()
                        .next()
                        .filter(|s| hash_valid(s))
                        .ok_or("官方安装器缺少有效 SHA1")?
                        .to_owned()
                }
            };
            let raw = self.component_bytes(&url, MAX_COMPONENT, cancel, stats, cb)?;
            let download = Download {
                url,
                hash,
                size: raw.len() as u64,
                relative: String::new(),
            };
            verify_bytes(&raw, &download, "官方组件安装器 SHA1 校验失败")?;
            if let Some(e) = expected {
                if let ComponentSource::Installer { download: d, .. } = &e.source {
                    verify_bytes(&raw, d, "官方组件安装器已改变，请重新确认安装")?;
                }
            }
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&raw)).map_err(error)?;
            let install: Value = serde_json::from_slice(&archive_bytes(
                &mut zip,
                "install_profile.json",
                MAX_METADATA,
            )?)
            .map_err(error)?;
            if install["spec"].as_u64().is_none() {
                return Err(
                    "暂不支持 1.7.10 等旧版 Forge 安装器，请选择 1.12.2 或更新的安装器".into(),
                );
            }
            let json_path = install["json"]
                .as_str()
                .unwrap_or("/version.json")
                .trim_start_matches('/');
            let profile: Value =
                serde_json::from_slice(&archive_bytes(&mut zip, json_path, MAX_METADATA)?)
                    .map_err(error)?;
            let declared_mc = install["minecraft"]
                .as_str()
                .or_else(|| install["install"]["minecraft"].as_str())
                .or_else(|| profile["inheritsFrom"].as_str());
            if declared_mc != Some(minecraft) {
                return Err("组件安装器与所选 Minecraft 版本不匹配".into());
            }
            (
                ComponentSource::Installer {
                    coordinate,
                    download,
                },
                profile,
                Some(install),
                Some(raw),
            )
        };
        validate_profile(minecraft, &selection, &profile, install.as_ref())?;
        if let Some(e) = expected {
            if e.selection.provider != selection.provider
                || e.selection.version != selection.version
                || e.profile != profile
                || e.install != install
            {
                return Err("组件安装信息已改变，请重新确认安装".into());
            }
        }
        let libraries = match expected {
            Some(e) => e.libraries.clone(),
            None => {
                let mut descriptors: BTreeMap<String, Download> = BTreeMap::new();
                let install_libs = install.as_ref().and_then(|i| i["libraries"].as_array());
                for library in install_libs
                    .into_iter()
                    .flatten()
                    .chain(profile["libraries"].as_array().into_iter().flatten())
                {
                    check(cancel)?;
                    if !pcl_core::library_allowed(library)? {
                        continue;
                    }
                    let d = self.loader_library(library, provider, cancel)?;
                    metadata_path(&d.relative)?;
                    if !d.url.is_empty() {
                        self.url(&d.url)?;
                    }
                    if let Some(old) = descriptors.insert(d.relative.clone(), d.clone()) {
                        if old.hash != d.hash || old.size != d.size || old.url != d.url {
                            return Err("组件库包含冲突的文件路径".into());
                        }
                    }
                }
                descriptors
            }
        };
        check(cancel)?;
        Ok(LoadedComponent {
            evidence: ResolvedComponent {
                selection,
                source,
                profile,
                install,
                libraries,
            },
            archive,
        })
    }
}

pub(super) fn normalized_component(
    minecraft: &str,
    component: &ComponentSelection,
) -> Result<ComponentSelection> {
    let provider = component.provider.to_ascii_lowercase();
    let mut version = component.version.clone();
    if provider == "forge" && !version.starts_with(&format!("{minecraft}-")) {
        version = format!("{minecraft}-{version}");
    }
    if provider == "neoforge" {
        if version.starts_with("1.20.1-") && minecraft != "1.20.1" {
            return Err("NeoForge 安装器与所选 Minecraft 版本不匹配".into());
        }
        // NeoForge's first 1.20.1 releases use the forge artifact and 47.x
        // versions. Manifests commonly store only that raw loader version.
        if minecraft == "1.20.1" && version.starts_with("47.") {
            version = format!("1.20.1-{version}");
        }
    }
    Ok(ComponentSelection { provider, version })
}
fn installer_coordinate(c: &ComponentSelection) -> String {
    if c.provider == "forge" {
        format!("net.minecraftforge:forge:{}:installer", c.version)
    } else if c.version.starts_with("1.20.1-") {
        format!("net.neoforged:forge:{}:installer", c.version)
    } else {
        format!("net.neoforged:neoforge:{}:installer", c.version)
    }
}
fn validate_profile(
    minecraft: &str,
    c: &ComponentSelection,
    profile: &Value,
    install: Option<&Value>,
) -> Result<()> {
    if !profile.is_object()
        || !profile["mainClass"].is_string()
        || profile["inheritsFrom"]
            .as_str()
            .is_some_and(|m| m != minecraft)
    {
        return Err("组件启动信息与所选 Minecraft 版本不匹配".into());
    }
    let libraries = profile["libraries"]
        .as_array()
        .ok_or("组件依赖库信息无效")?;
    let identity = if c.provider == "fabric" {
        format!("net.fabricmc:fabric-loader:{}", c.version)
    } else {
        installer_coordinate(c)
            .trim_end_matches(":installer")
            .to_owned()
    };
    let found = libraries
        .iter()
        .chain(
            install
                .and_then(|i| i["libraries"].as_array())
                .into_iter()
                .flatten(),
        )
        .filter(|l| pcl_core::library_allowed(l).unwrap_or(false))
        .any(|l| {
            l["name"].as_str().is_some_and(|name| {
                name == identity
                    || name
                        .strip_prefix(&identity)
                        .is_some_and(|suffix| suffix.starts_with(':') || suffix.starts_with('@'))
            })
        });
    if !found {
        return Err("组件安装信息中的加载器版本与请求不匹配".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
