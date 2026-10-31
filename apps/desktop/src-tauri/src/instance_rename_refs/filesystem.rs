//! Platform facts preserve the original Unix journal field order/encoding.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path},
    sync::atomic::{AtomicU64, Ordering},
};
pub(super) const PRESET_LIMIT: usize = 64 * 1024;
pub(super) const MAX_HISTORY: usize = 8192;
pub(super) const MARKER: &str = "instance-rename-pending.json";
static NEXT: AtomicU64 = AtomicU64::new(0);
pub(super) fn absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.as_os_str().len() > 4096
        || path.components().any(|part| {
            !matches!(part, Component::RootDir | Component::Normal(_))
                && !(cfg!(windows) && matches!(part, Component::Prefix(_)))
        })
        || path
            .to_str()
            .is_none_or(|text| text.chars().any(char::is_control))
    {
        return Err("引用迁移需要规范的绝对路径".into());
    }
    Ok(())
}
pub(super) fn hash(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
pub(super) fn nonce() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(unix)]
use std::{fs::Metadata, os::unix::fs::MetadataExt};
#[cfg(target_os = "linux")]
#[path = "filesystem/linux.rs"]
mod platform;
#[cfg(not(target_os = "linux"))]
#[path = "filesystem/native.rs"]
mod platform;
pub(super) use platform::Dir;
pub(crate) use platform::ReferenceLock;
#[cfg(all(test, target_os = "linux"))]
#[path = "filesystem/native.rs"]
mod native_probe;
#[cfg(test)]
#[path = "filesystem/tests.rs"]
mod tests;
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub(super) device: u64,
    pub(super) inode: u64,
}
#[cfg(unix)]
impl Identity {
    pub(super) fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stamp {
    pub(super) identity: Identity,
    pub(super) bytes: u64,
    pub(super) mode: u32,
    pub(super) links: u64,
    pub(super) modified: (i64, i64),
    pub(super) changed: (i64, i64),
}
#[cfg(unix)]
impl Stamp {
    pub(super) fn of(metadata: &Metadata) -> Self {
        Self {
            identity: Identity::of(metadata),
            bytes: metadata.len(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    // rename/exchange can change ctime. Retain every other stamp property;
    // callers separately compare the content hash to reject a replaced temp.
    pub(super) fn moved_matches(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.bytes == other.bytes
            && self.mode == other.mode
            && self.links == other.links
            && self.modified == other.modified
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Token {
    pub(super) stamp: Stamp,
    pub(super) hash: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiskSnapshot {
    pub(super) directory: Option<Identity>,
    pub(super) file: Option<Token>,
    #[cfg(windows)]
    pub(super) logical: Option<LogicalState>,
}
impl DiskSnapshot {
    pub(crate) fn file_exists(&self) -> bool {
        self.file.is_some()
    }
    pub(crate) fn compatible(&self, current: &Self) -> bool {
        if self == current {
            return true;
        }
        #[cfg(unix)]
        {
            self.directory.is_none() && self.file.is_none() && current.file.is_none()
        }
        #[cfg(windows)]
        {
            self.directory.is_none()
                && self.file.is_none()
                && self.logical.is_none()
                && current.file.is_none()
                && current
                    .logical
                    .as_ref()
                    .is_some_and(|state| state.presence == "never_created")
        }
    }
}

#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum WindowsFormat {
    #[serde(rename = "windows-file-id-128-v1")]
    FileId128V1,
}
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    format: WindowsFormat,
    volume: u64,
    file_id: [u8; 16],
}
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stamp {
    pub(super) identity: Identity,
    metadata: pcl_core::retained_fs::Metadata,
}
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogicalState {
    format: LogicalFormat,
    pub(super) presence: String,
    pub(super) digest: String,
}
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum LogicalFormat {
    #[serde(rename = "windows-optional-document-v2")]
    V2,
}
#[cfg(windows)]
impl DiskSnapshot {
    pub(super) fn valid_logical(&self) -> bool {
        self.logical.as_ref().is_some_and(|state| {
            state.digest.len() == 64
                && state.digest.bytes().all(|b| b.is_ascii_hexdigit())
                && match state.presence.as_str() {
                    "present" => self.file.is_some(),
                    "never_created" | "tombstone" => self.file.is_none(),
                    _ => false,
                }
        })
    }
}

/// The same per-root lock coordinates reference replay and resource writers.
pub(crate) fn root_history_lock(root: &Path) -> Result<ReferenceLock, String> {
    Dir::open(root)?
        .ensure(".pcl-linux")?
        .lock(".resource-operations.lock")
}
