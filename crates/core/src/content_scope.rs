//! Content-directory selection, shared by scan, launch and desktop
//! file services. Probes own their filesystem checks: a path read here does not
//! replace the descriptor-relative evidence held by export/update transactions.
//! Persisted instance/journal booleans stay unchanged; this module owns the rule,
//! not authority to move files or redirect a previously submitted operation.
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const ISOLATION_MARKERS: [&str; 4] = ["mods", "saves", "config", "options.txt"];

/// A policy selects future content access; it never moves existing files.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IsolationPolicy {
    #[default]
    Automatic,
    Shared,
    Isolated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentScope {
    Shared,
    Isolated,
}

impl ContentScope {
    pub fn from_isolated(isolated: bool) -> Self {
        if isolated {
            Self::Isolated
        } else {
            Self::Shared
        }
    }
    pub fn is_isolated(self) -> bool {
        self == Self::Isolated
    }
    /// Pure relative layout, not a verified directory. Callers still open and
    /// validate this path under their captured game root before any file work.
    pub fn relative_base(self, id: &str) -> Result<PathBuf, String> {
        crate::identifier(id)?;
        Ok(match self {
            Self::Shared => PathBuf::new(),
            Self::Isolated => Path::new("versions").join(id),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerState {
    Missing,
    Present,
    Unsupported,
}

/// Probe every marker, even after finding one. An ordinary `mods` directory
/// must not hide a later unreadable, dangling or special `config` marker.
pub fn automatic_scope(
    probe: impl FnMut(&'static str) -> Result<MarkerState, String>,
) -> Result<ContentScope, String> {
    resolve_scope(IsolationPolicy::Automatic, probe)
}

/// Explicit choices still probe all markers so unsafe instance paths cannot
/// become accessible merely by changing a setting.
pub fn resolve_scope(
    policy: IsolationPolicy,
    mut probe: impl FnMut(&'static str) -> Result<MarkerState, String>,
) -> Result<ContentScope, String> {
    let mut isolated = false;
    for name in ISOLATION_MARKERS {
        match probe(name)? {
            MarkerState::Missing => {}
            MarkerState::Present => isolated = true,
            MarkerState::Unsupported => {
                return Err(format!(
                    "实例隔离标记 {name} 是符号链接或特殊文件，无法确定游戏内容目录"
                ))
            }
        }
    }
    Ok(match policy {
        IsolationPolicy::Automatic => ContentScope::from_isolated(isolated),
        IsolationPolicy::Shared => ContentScope::Shared,
        IsolationPolicy::Isolated => ContentScope::Isolated,
    })
}

/// Read-only path adapter for core scan/launch. Only NotFound means absence;
/// permission and I/O errors cannot silently switch an instance to shared data.
pub fn inspect(root: &Path, id: &str) -> Result<ContentScope, String> {
    inspect_with_isolation(root, id, IsolationPolicy::Automatic)
}

pub fn inspect_with_isolation(
    root: &Path,
    id: &str,
    policy: IsolationPolicy,
) -> Result<ContentScope, String> {
    crate::identifier(id)?;
    let folder = crate::safe_join(root, Path::new("versions").join(id))?;
    resolve_scope(policy, |name| {
        match fs::symlink_metadata(folder.join(name)) {
            Ok(metadata) if metadata.is_file() || metadata.is_dir() => Ok(MarkerState::Present),
            Ok(_) => Ok(MarkerState::Unsupported),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MarkerState::Missing),
            Err(error) => Err(format!("无法读取实例隔离标记 {name}：{error}")),
        }
    })
}

pub(crate) fn verified_base(root: &Path, id: &str, scope: ContentScope) -> Result<PathBuf, String> {
    let relative = scope.relative_base(id)?;
    match scope {
        ContentScope::Shared => Ok(root.to_path_buf()),
        ContentScope::Isolated => crate::safe_join(root, relative),
    }
}

#[cfg(all(test, unix))]
#[path = "content_scope_tests.rs"]
mod tests;
