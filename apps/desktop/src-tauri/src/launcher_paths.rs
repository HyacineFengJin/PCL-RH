//! Locate the existing portable data/resource root without consulting cwd.
//! These markers identify the archive layout, not resource authenticity. Actual
//! host/source consumers retain their own identity and publication checks.
use std::{
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

const PORTABLE_MARKER: &str = "pcl-rh.json";
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableMarker {
    schema_version: u32,
    product_id: String,
}

pub(crate) fn resolve() -> Result<PathBuf, String> {
    let home = std::env::var_os("PCL_RH_HOME");
    let legacy = std::env::var_os("PCL_LINUX_HOME");
    let executable = std::env::current_exe().ok();
    // A distributed executable must locate its own archive, never a source
    // checkout that happened to exist on the build host. Debug builds retain
    // that convenience for cargo run; explicit roots work in both profiles.
    #[cfg(debug_assertions)]
    let development_manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    #[cfg(not(debug_assertions))]
    let development_manifest = Path::new("");
    resolve_from(
        home.as_deref(),
        legacy.as_deref(),
        executable.as_deref(),
        development_manifest,
    )
}

fn marker(root: &Path, relative: &str, limit: u64) -> Result<Vec<u8>, String> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("发行资源 {relative} 不可用：{error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err(format!("发行资源 {relative} 不是有效的有限大小普通文件"));
    }
    let actual = path.canonicalize().map_err(|error| error.to_string())?;
    if !actual.starts_with(root) {
        return Err(format!("发行资源 {relative} 位于所选根目录之外"));
    }
    let mut bytes = Vec::new();
    File::open(&path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("无法读取发行资源 {relative}：{error}"))?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(format!("发行资源 {relative} 超过读取上限或为空"));
    }
    Ok(bytes)
}

fn has_portable_marker(root: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(root.join(PORTABLE_MARKER)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("无法检查发行布局标识 {PORTABLE_MARKER}：{error}")),
    }
}

fn portable_root(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("启动器根目录必须是绝对路径".into());
    }
    let root = path
        .canonicalize()
        .map_err(|error| format!("根目录 {} 不可用：{error}", path.display()))?;
    if !root.is_dir() {
        return Err(format!("启动器根路径 {} 不是目录", root.display()));
    }
    if has_portable_marker(&root)? {
        let value: PortableMarker = serde_json::from_slice(&marker(&root, PORTABLE_MARKER, 4096)?)
            .map_err(|_| "发行布局标识 pcl-rh.json 损坏或含未知字段，已保留")?;
        if value.schema_version != 1 || value.product_id != "dev.pcl-rh.portable" {
            return Err("发行布局标识 pcl-rh.json 的版本或产品不受支持，已保留".into());
        }
        // The core archive marker is independent of optional Code/AI plugins.
        // It identifies layout only and does not authenticate executable bytes.
        return Ok(root);
    }
    // Historical complete archives predate pcl-rh.json. Retain that existing
    // Linux root without making its former optional resources a new dependency.
    marker(&root, "experimental/host.mjs", 1024 * 1024)?;
    let runtime: serde_json::Value = serde_json::from_slice(&marker(
        &root,
        "experimental/runtime/package.json",
        64 * 1024,
    )?)
    .map_err(|_| "实验功能运行时的发行标识 JSON 无效")?;
    if runtime["name"].as_str() != Some("pcl-rh-experimental-runtime") {
        return Err("实验功能运行时的发行标识不属于 PCL RH".into());
    }
    let plugins: serde_json::Value =
        serde_json::from_slice(&marker(&root, "code/plugins.json", 1024 * 1024)?)
            .map_err(|_| "插件发行标识 JSON 无效")?;
    if !plugins
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["id"] == "rh.project"))
    {
        return Err("插件发行标识缺少 PCL RH 项目模块".into());
    }
    if !marker(&root, "assets/pcl-rh.png", 4 * 1024 * 1024)?.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("PCL RH 品牌资源不是 PNG 文件".into());
    }
    Ok(root)
}

fn development_root(manifest: &Path) -> Result<PathBuf, String> {
    if !manifest.is_absolute() {
        return Err("编译时源码目录不是绝对路径".into());
    }
    let manifest = manifest
        .canonicalize()
        .map_err(|error| format!("开发源码目录不可用：{error}"))?;
    let candidate = manifest
        .ancestors()
        .nth(3)
        .ok_or("开发源码目录不符合 apps/desktop/src-tauri 布局")?;
    let root = portable_root(candidate)?;
    if root
        .join("apps/desktop/src-tauri")
        .canonicalize()
        .ok()
        .as_ref()
        != Some(&manifest)
    {
        return Err("开发源码目录不符合 apps/desktop/src-tauri 布局".into());
    }
    let desktop = marker(&root, "apps/desktop/src-tauri/Cargo.toml", 64 * 1024)?;
    let workspace = marker(&root, "Cargo.toml", 64 * 1024)?;
    marker(&root, "apps/desktop/src-tauri/src/main.rs", 1024 * 1024)?;
    if !String::from_utf8_lossy(&desktop).contains("name = \"pcl-desktop\"")
        || !String::from_utf8_lossy(&workspace).contains("[workspace]")
    {
        return Err("编译时目录不是有效的 PCL RH 开发源码树".into());
    }
    Ok(root)
}

// Inputs are explicit so filesystem fixtures do not mutate process env or cwd.
pub(crate) fn resolve_from(
    home: Option<&OsStr>,
    legacy: Option<&OsStr>,
    executable: Option<&Path>,
    manifest: &Path,
) -> Result<PathBuf, String> {
    for (name, value) in [("PCL_RH_HOME", home), ("PCL_LINUX_HOME", legacy)] {
        if let Some(value) = value {
            return portable_root(Path::new(value)).map_err(|error| {
                format!("显式设置的 {name} 无效：{error}；请修正该路径或取消此设置")
            });
        }
    }
    let mut rejected = Vec::new();
    if let Some(executable) = executable.filter(|path| path.is_absolute()) {
        match executable.canonicalize() {
            Ok(executable) if executable.is_file() => {
                let name = executable.file_name().and_then(OsStr::to_str);
                if matches!(name, Some("pcl-desktop" | "pcl-desktop.exe")) {
                    if let Some(parent) = executable.parent() {
                        let nested = parent.file_name() == Some(OsStr::new("bin"))
                            && parent.parent().and_then(Path::file_name)
                                == Some(OsStr::new(".pcl-rust"));
                        let candidate = if nested {
                            parent.parent().and_then(Path::parent)
                        } else {
                            Some(parent)
                        };
                        if let Some(candidate) = candidate {
                            let authoritative = has_portable_marker(candidate)?;
                            match portable_root(candidate) {
                                Ok(root) => return Ok(root),
                                Err(error) if authoritative => {
                                    return Err(format!("可执行文件旁的发行布局标识无效：{error}"))
                                }
                                Err(error) => {
                                    rejected.push(format!("可执行文件旁的发行目录无效：{error}"))
                                }
                            }
                        }
                    }
                } else {
                    rejected.push("可执行文件名不符合 portable archive 布局".into());
                }
            }
            Ok(_) => rejected.push("当前可执行路径不是普通文件".into()),
            Err(error) => rejected.push(format!("当前可执行路径不可用：{error}")),
        }
    } else {
        rejected.push("无法读取当前可执行文件的绝对路径".into());
    }
    match development_root(manifest) {
        Ok(root) => Ok(root),
        Err(error) => {
            rejected.push(error);
            Err(format!(
                "无法定位 PCL RH portable 根目录：{}。请完整解压发行包，保留资源目录与 .pcl-rust/bin 布局，或将 PCL_RH_HOME 设置为完整发行包根目录的绝对路径",
                rejected.join("；")
            ))
        }
    }
}

#[cfg(test)]
#[path = "launcher_paths/tests.rs"]
mod tests;
