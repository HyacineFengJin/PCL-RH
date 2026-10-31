//! Retained filesystem authority for recoverable instance deletion.
//! Unix snapshot field names/order remain the existing journal wire format.
//! Windows stores a tagged native volume + complete 128-bit file identity.
use super::{cancelled, Result, MAX_JOURNAL};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
pub(super) const MAX_FILES: usize = 100_000;
pub(super) const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Key {
    #[cfg(unix)]
    pub(super) dev: u64,
    #[cfg(unix)]
    pub(super) ino: u64,
    #[cfg(windows)]
    format: WindowsFormat,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
}
impl Key {
    #[cfg(unix)]
    pub(super) fn of(m: &std::fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
        }
    }
    pub(super) fn same_volume(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.dev == other.dev
        }
        #[cfg(windows)]
        {
            self.volume == other.volume
        }
    }
    pub(super) fn valid(&self) -> bool {
        #[cfg(unix)]
        {
            self.ino != 0
        }
        #[cfg(windows)]
        {
            self.file_id != [0; 16]
        }
    }
    #[cfg(any(not(target_os = "linux"), test))]
    fn native(m: &pcl_core::retained_fs::Metadata) -> Result<Self> {
        use pcl_core::retained_fs::Identity;
        match &m.identity {
            #[cfg(unix)]
            Identity::Unix { device, inode } => Ok(Self {
                dev: *device,
                ino: *inode,
            }),
            #[cfg(windows)]
            Identity::Windows { volume, file } => Ok(Self {
                format: WindowsFormat::FileId128V1,
                volume: *volume,
                file_id: *file,
            }),
            _ => Err("实例目录身份不属于当前平台".into()),
        }
    }
}

#[cfg(windows)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum WindowsFormat {
    #[serde(rename = "windows-file-id-128-v1")]
    FileId128V1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stamp {
    pub(super) key: Key,
    pub(super) size: u64,
    #[cfg(unix)]
    pub(super) mode: u32,
    #[cfg(windows)]
    kind: pcl_core::retained_fs::Kind,
    #[cfg(windows)]
    security: pcl_core::retained_fs::Security,
    #[cfg(windows)]
    reparse_tag: Option<u32>,
    pub(super) links: u64,
    pub(super) modified: (i64, i64),
    pub(super) changed: (i64, i64),
}
impl Stamp {
    #[cfg(unix)]
    pub(super) fn of(m: &std::fs::Metadata) -> Self {
        Self {
            key: Key::of(m),
            size: m.len(),
            mode: m.mode(),
            links: m.nlink(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
    fn same_type_security(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.mode == other.mode
        }
        #[cfg(windows)]
        {
            self.kind == other.kind
                && self.security == other.security
                && self.reparse_tag == other.reparse_tag
        }
    }
    pub(super) fn is_directory(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT == libc::S_IFDIR
        }
        #[cfg(windows)]
        {
            self.kind == pcl_core::retained_fs::Kind::Directory && self.reparse_tag.is_none()
        }
    }
    pub(super) fn is_regular(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT == libc::S_IFREG
        }
        #[cfg(windows)]
        {
            self.kind == pcl_core::retained_fs::Kind::Regular && self.reparse_tag.is_none()
        }
    }
    #[cfg(any(not(target_os = "linux"), test))]
    fn native(m: &pcl_core::retained_fs::Metadata) -> Result<Self> {
        #[cfg(unix)]
        let mode = match m.security {
            pcl_core::retained_fs::Security::Unix { mode, .. } => mode,
            _ => return Err("实例权限不属于当前平台".into()),
        };
        Ok(Self {
            key: Key::native(m)?,
            size: m.len,
            links: m.links,
            #[cfg(unix)]
            mode,
            #[cfg(windows)]
            kind: m.kind,
            #[cfg(windows)]
            security: m.security.clone(),
            #[cfg(windows)]
            reparse_tag: m.reparse_tag,
            modified: (m.modified.seconds, i64::from(m.modified.nanos)),
            changed: (m.changed.seconds, i64::from(m.changed.nanos)),
        })
    }
    pub(super) fn moved_matches(&self, other: &Self) -> bool {
        self.key == other.key
            && self.size == other.size
            && self.same_type_security(other)
            && self.links == other.links
            && self.modified == other.modified
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileToken {
    pub(super) stamp: Stamp,
    pub(super) hash: String,
}
impl FileToken {
    pub(super) fn moved_matches(&self, other: &Self) -> bool {
        self.stamp.moved_matches(&other.stamp) && self.hash == other.hash
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TreeEntry {
    pub(super) path: String,
    pub(super) stamp: Stamp,
    pub(super) hash: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tree {
    pub(super) entries: Vec<TreeEntry>,
    pub(super) files: u64,
    pub(super) bytes: u64,
}
impl Tree {
    pub(super) fn moved_matches(&self, other: &Self) -> bool {
        self.files == other.files
            && self.bytes == other.bytes
            && self.entries.len() == other.entries.len()
            && self.entries.iter().zip(&other.entries).all(|(a, b)| {
                a.path == b.path
                    && a.hash == b.hash
                    && if a.path.is_empty() {
                        a.stamp.moved_matches(&b.stamp)
                    } else {
                        a.stamp == b.stamp
                    }
            })
    }
}

#[cfg(target_os = "linux")]
#[path = "filesystem/linux.rs"]
mod platform;
#[cfg(not(target_os = "linux"))]
#[path = "filesystem/native.rs"]
mod platform;
pub(super) use platform::*;

#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "filesystem/native.rs"]
mod portable;
