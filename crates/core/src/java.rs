//! Shared Java discovery and launch selection.
//!
//! A catalog is a bounded snapshot, not a promise that a runtime stays available.
//! Manual selection probes only that executable again; automatic selection uses
//! the lowest compatible major and a canonical path tie-breaker.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const CATALOG_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const OUTPUT_LIMIT: usize = 64 * 1024;
const MAX_CANDIDATES: usize = 64;
const MAX_DISCOVERY_PATHS: usize = 256;

/// `manual` is an explicit choice: failures never fall back to automatic Java.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum JavaSelection {
    #[default]
    Auto,
    Manual {
        path: String,
    },
}

impl<'de> Deserialize<'de> for JavaSelection {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Serde ignores unknown fields on an internally tagged unit variant.
        // An empty struct variant enforces the same strict schema as Manual.
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum StrictSelection {
            Auto {},
            Manual { path: String },
        }
        Ok(match StrictSelection::deserialize(deserializer)? {
            StrictSelection::Auto {} => Self::Auto,
            StrictSelection::Manual { path } => Self::Manual { path },
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JavaRuntime {
    pub path: String,
    pub major: u32,
    pub vendor: String,
    pub arch: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JavaFailure {
    pub path: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct JavaCatalog {
    pub runtimes: Vec<JavaRuntime>,
    pub unavailable: Vec<JavaFailure>,
}

fn canonical_executable(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path).map_err(|e| format!("Java 路径不可用：{e}"))?;
    if path.to_str().is_none() {
        return Err("Java 路径不是有效的 UTF-8 文本".into());
    }
    let metadata = fs::metadata(&path).map_err(|e| format!("无法读取 Java 文件：{e}"))?;
    if !metadata.is_file() {
        return Err("Java 路径必须指向可执行文件".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("Java 文件没有执行权限".into());
        }
    }
    Ok(path)
}

/// Probe one canonical executable, with a two-second deadline and 64 KiB per
/// output stream. The child is always waited for, including every failure path.
pub fn inspect(path: &Path) -> Result<JavaRuntime> {
    inspect_until(path, Instant::now() + PROBE_TIMEOUT)
}

fn inspect_until(path: &Path, deadline: Instant) -> Result<JavaRuntime> {
    let canonical = canonical_executable(path)?;
    let output = probe(&canonical, deadline.min(Instant::now() + PROBE_TIMEOUT))?;
    parse_runtime(&canonical, &output)
}

fn major_version(value: &str) -> Option<u32> {
    let value = value.trim().strip_prefix("1.").unwrap_or(value.trim());
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    let major = digits.parse::<u32>().ok()?;
    (major > 0 && major <= 999).then_some(major)
}

fn normalized_arch(arch: &str) -> Option<&'static str> {
    crate::platform::Architecture::parse(arch).map(crate::platform::Architecture::name)
}

fn parse_runtime(path: &Path, output: &str) -> Result<JavaRuntime> {
    let property = |key: &str| {
        output.lines().find_map(|line| {
            let (name, value) = line.trim().split_once('=')?;
            (name.trim() == key).then(|| value.trim())
        })
    };
    let version = property("java.specification.version")
        .or_else(|| property("java.version"))
        .or_else(|| {
            output.lines().find_map(|line| {
                let line = line.trim();
                for prefix in ["openjdk version ", "java version ", "openjdk "] {
                    if let Some(value) = line.strip_prefix(prefix) {
                        return Some(value.trim_start_matches('"'));
                    }
                }
                None
            })
        })
        .ok_or("无法识别 Java 版本：输出缺少版本信息")?;
    let major = major_version(version).ok_or("无法识别 Java 主版本号")?;
    let host = normalized_arch(std::env::consts::ARCH).unwrap_or(std::env::consts::ARCH);
    let arch = if let Some(arch) = property("os.arch") {
        let arch = normalized_arch(arch).ok_or_else(|| format!("无法识别 Java 架构：{arch}"))?;
        if arch != host {
            return Err(format!("Java 架构 {arch} 与当前系统 {host} 不兼容"));
        }
        arch
    } else {
        // Older -version output lacks os.arch. A successful native executable
        // is usable on this host; explicit bitness evidence must still agree.
        if output.contains("32-Bit") && cfg!(target_pointer_width = "64")
            || output.contains("64-Bit") && cfg!(target_pointer_width = "32")
        {
            return Err("Java 位数与当前系统不兼容".into());
        }
        host
    };
    let vendor = property("java.vendor").unwrap_or_else(|| {
        if output.to_ascii_lowercase().contains("openjdk") {
            "OpenJDK"
        } else {
            "Java"
        }
    });
    Ok(JavaRuntime {
        path: path
            .to_str()
            .ok_or("Java 路径不是有效的 UTF-8 文本")?
            .into(),
        major,
        vendor: vendor.into(),
        arch: arch.into(),
    })
}

/// Discover local, environment and system runtimes. Only explicitly registered
/// paths appear in `unavailable`; ordinary missing discovery locations are quiet.
pub fn catalog(project: &Path, root: Option<&Path>, extras: &[String]) -> Result<JavaCatalog> {
    catalog_until(project, root, extras, Instant::now() + CATALOG_TIMEOUT)
}

fn discovery_paths(project: &Path, root: Option<&Path>) -> Vec<PathBuf> {
    let Ok(platform) = crate::platform::Platform::current() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let mut bases = vec![project.to_path_buf(), project.join("PCL-Linux")];
    bases.extend(root.map(Path::to_path_buf));
    for base in bases {
        for runtime in ["runtime", "runtime-21", "runtime-25"] {
            paths.extend(platform.java_paths(&base.join(runtime)));
        }
    }
    paths.extend(managed_runtime_paths(project));
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        paths.extend(platform.java_paths(&PathBuf::from(home)));
    }
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(
            std::env::split_paths(&path)
                .take(MAX_DISCOVERY_PATHS / 2)
                .map(|p| p.join(platform.java_executable())),
        );
    }
    for directory in system_java_directories(platform.os) {
        if paths.len() >= MAX_DISCOVERY_PATHS {
            break;
        }
        if let Ok(entries) = fs::read_dir(directory) {
            let mut system: Vec<_> = entries
                .take((MAX_DISCOVERY_PATHS - paths.len()).min(MAX_DISCOVERY_PATHS / 2))
                .filter_map(|entry| entry.ok())
                .flat_map(|entry| platform.java_paths(&entry.path()))
                .collect();
            system.sort();
            paths.extend(system);
        }
    }
    paths.truncate(MAX_DISCOVERY_PATHS);
    paths
}

fn system_java_directories(os: crate::platform::OperatingSystem) -> Vec<PathBuf> {
    use crate::platform::OperatingSystem;
    match os {
        OperatingSystem::Linux => vec!["/usr/lib/jvm".into()],
        OperatingSystem::MacOs => vec!["/Library/Java/JavaVirtualMachines".into()],
        OperatingSystem::Windows => ["ProgramFiles", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(std::env::var_os)
            .flat_map(|base| {
                [
                    "Java",
                    "Eclipse Adoptium",
                    "Microsoft",
                    "Amazon Corretto",
                    "BellSoft",
                    "Zulu",
                ]
                .map(|vendor| PathBuf::from(&base).join(vendor))
            })
            .collect(),
    }
}

/// Published managed runtimes also remain discoverable if a crash happened
/// after the directory commit but before the registry addition was saved.
/// Private stages and unrelated folders never become automatic candidates.
pub fn managed_runtime_paths(project: &Path) -> Vec<PathBuf> {
    let Ok(platform) = crate::platform::Platform::current() else {
        return Vec::new();
    };
    let directory = project.join(".pcl-rust/java");
    if !fs::symlink_metadata(&directory).is_ok_and(|m| m.file_type().is_dir()) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .take(MAX_DISCOVERY_PATHS)
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let name = entry.file_name();
            let name = name.to_str()?;
            let (component, hash) = name.rsplit_once('-')?;
            if !(component.starts_with("java-runtime-") || component == "jre-legacy")
                || hash.len() != 40
                || !hash
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || !fs::symlink_metadata(entry.path().join(".pcl-java-owner.json"))
                    .is_ok_and(|m| m.file_type().is_file())
            {
                return None;
            }
            Some(platform.java_paths(&entry.path()))
        })
        .flatten()
        .filter(|path| path.is_file())
        .collect();
    paths.sort();
    paths.truncate(MAX_CANDIDATES);
    paths
}

fn catalog_until(
    project: &Path,
    root: Option<&Path>,
    extras: &[String],
    deadline: Instant,
) -> Result<JavaCatalog> {
    if extras.len() > MAX_CANDIDATES {
        return Err(format!("最多可登记 {MAX_CANDIDATES} 个 Java 路径"));
    }
    catalog_candidates(extras, discovery_paths(project, root), deadline)
}

fn catalog_candidates(
    extras: &[String],
    discovered: Vec<PathBuf>,
    deadline: Instant,
) -> Result<JavaCatalog> {
    let mut catalog = JavaCatalog::default();
    let mut probed: BTreeMap<PathBuf, Result<JavaRuntime>> = BTreeMap::new();
    let mut extra_seen = BTreeSet::new();
    // Registered paths are first so an automatic discovery flood cannot hide
    // their errors. Canonical aliases share one execution, not one per label.
    for (path, registered) in extras
        .iter()
        .map(|p| (PathBuf::from(p), true))
        .chain(discovered.into_iter().map(|p| (p, false)))
    {
        let label = path.to_string_lossy().into_owned();
        if registered && !extra_seen.insert(label.clone()) {
            continue;
        }
        let result = match canonical_executable(&path) {
            Err(error) => Err(error),
            Ok(canonical) => {
                if let Some(previous) = probed.get(&canonical) {
                    previous.clone()
                } else {
                    let result = if Instant::now() >= deadline {
                        Err("Java 检测已达到 10 秒总时间限制，请单独检测该路径".into())
                    } else if probed.len() >= MAX_CANDIDATES {
                        Err(format!("Java 检测已达到 {MAX_CANDIDATES} 个运行环境限制"))
                    } else {
                        inspect_until(&canonical, deadline)
                    };
                    if let Ok(runtime) = &result {
                        catalog.runtimes.push(runtime.clone());
                    }
                    probed.insert(canonical, result.clone());
                    result
                }
            }
        };
        if registered {
            if let Err(error) = result {
                catalog.unavailable.push(JavaFailure { path: label, error });
            }
        }
    }
    catalog
        .runtimes
        .sort_by(|a, b| a.major.cmp(&b.major).then(a.path.cmp(&b.path)));
    catalog.unavailable.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(catalog)
}

pub(crate) fn compatible(major: u32, required: u32, exact: bool) -> bool {
    if exact {
        major == required
    } else {
        major >= required
    }
}

/// Automatic choice uses the lowest compatible major. Forge/NeoForge pass
/// `exact=true`; manually choosing a newer major does not bypass that contract.
pub fn select(
    project: &Path,
    root: Option<&Path>,
    extras: &[String],
    choice: &JavaSelection,
    required: u32,
    exact: bool,
) -> Result<PathBuf> {
    let requirement = requirement(required, exact)?;
    match choice {
        JavaSelection::Manual { path } => {
            let runtime = inspect(Path::new(path))
                .map_err(|e| format!("手动选择的 Java 不可用（{path}）：{e}"))?;
            if !compatible(runtime.major, required, exact) {
                return Err(format!(
                    "{requirement}；手动选择的是 Java {}（{}）",
                    runtime.major, runtime.path
                ));
            }
            Ok(runtime.path.into())
        }
        JavaSelection::Auto => {
            select_from_catalog(&catalog(project, root, extras)?, required, exact)
        }
    }
}

fn requirement(required: u32, exact: bool) -> Result<String> {
    if required == 0 || required > 999 {
        return Err("实例要求的 Java 主版本无效".into());
    }
    Ok(if exact {
        format!("此实例需要 Java {required}，必须使用相同主版本")
    } else {
        format!("此实例需要 Java {required} 或更新版本")
    })
}

/// Separate policy from discovery so deterministic callers/tests can supply an
/// already inspected catalog without running Java a second time.
pub fn select_from_catalog(catalog: &JavaCatalog, required: u32, exact: bool) -> Result<PathBuf> {
    let requirement = requirement(required, exact)?;
    catalog
        .runtimes
        .iter()
        .filter(|runtime| compatible(runtime.major, required, exact))
        .min_by(|a, b| a.major.cmp(&b.major).then(a.path.cmp(&b.path)))
        .map(|runtime| PathBuf::from(&runtime.path))
        .ok_or_else(|| {
            format!("{requirement}；未找到兼容运行环境，请登记 Java 路径或设置 JAVA_HOME")
        })
}

#[cfg(unix)]
fn probe(path: &Path, deadline: Instant) -> Result<String> {
    use std::{
        io::Read,
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::{Child, Command, Stdio},
    };
    struct ChildGuard(Child);
    impl ChildGuard {
        fn kill_group(&self) {
            // A private group also closes pipes inherited by background children.
            // Killing only the JVM could leave a reader waiting after it exits.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
        }
    }
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            self.kill_group();
            let _ = self.0.wait();
        }
    }
    fn nonblocking(pipe: &impl AsRawFd) -> Result<()> {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return Err(format!(
                "无法读取 Java 检测输出：{}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    fn drain(pipe: &mut impl Read, output: &mut Vec<u8>, ended: &mut bool) -> Result<()> {
        if *ended {
            return Ok(());
        }
        let mut chunk = [0; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => {
                    *ended = true;
                    return Ok(());
                }
                Ok(count) => {
                    if output.len() + count > OUTPUT_LIMIT {
                        return Err("Java 检测输出超过 64 KiB 限制".into());
                    }
                    output.extend_from_slice(&chunk[..count]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("Java 检测输出读取失败：{e}")),
            }
        }
    }
    if Instant::now() >= deadline {
        return Err("Java 检测超时".into());
    }
    let mut command = Command::new(path);
    command
        .args(["-XshowSettings:properties", "-version"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("JAVA_TOOL_OPTIONS")
        .env_remove("_JAVA_OPTIONS")
        .env_remove("JDK_JAVA_OPTIONS")
        .process_group(0);
    let retry_deadline = deadline.min(Instant::now() + Duration::from_millis(100));
    let mut child = ChildGuard(loop {
        match command.spawn() {
            Ok(child) => break child,
            // A concurrent fork can briefly retain a just-written executable's
            // descriptor before exec closes it. Retry only that transient case.
            Err(e)
                if e.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < retry_deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(format!("无法执行 Java：{e}")),
        }
    });
    let mut stdout = child.0.stdout.take().ok_or("无法读取 Java 标准输出")?;
    let mut stderr = child.0.stderr.take().ok_or("无法读取 Java 错误输出")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_end, mut err_end) = (false, false);
    let mut status = None;
    let mut drain_deadline = deadline;
    loop {
        drain(&mut stdout, &mut out, &mut out_end)?;
        drain(&mut stderr, &mut err, &mut err_end)?;
        if status.is_none() {
            status = child
                .0
                .try_wait()
                .map_err(|e| format!("无法等待 Java 检测进程：{e}"))?;
            if status.is_some() {
                child.kill_group();
                drain_deadline = deadline.min(Instant::now() + Duration::from_millis(100));
            }
        }
        if let Some(status) = status {
            if out_end && err_end {
                if !status.success() {
                    return Err(format!("Java 检测进程异常退出：{status}"));
                }
                return Ok(format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out),
                    String::from_utf8_lossy(&err)
                ));
            }
        }
        if Instant::now() >= drain_deadline {
            return Err(if status.is_some() {
                "Java 检测进程退出后输出管道未关闭"
            } else {
                "Java 检测超时（最长 2 秒）"
            }
            .into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(not(unix))]
fn probe(_path: &Path, _deadline: Instant) -> Result<String> {
    Err("当前平台不支持有时间限制的 Java 检测".into())
}

#[cfg(test)]
#[path = "java_tests.rs"]
mod tests;
