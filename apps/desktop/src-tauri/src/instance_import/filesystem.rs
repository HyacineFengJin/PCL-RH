//! Retained source and target identity for instance import transactions.
use super::{check, component, error, relative, Result, MAX_BYTES, MAX_FILES};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::AtomicBool,
};
#[cfg(windows)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum WindowsFormat {
    #[serde(rename = "windows-file-id-128-v1")]
    FileId128V1,
}
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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stamp {
    pub(super) key: Key,
    #[cfg(unix)]
    pub(super) mode: u32,
    #[cfg(windows)]
    kind: pcl_core::retained_fs::Kind,
    #[cfg(windows)]
    security: pcl_core::retained_fs::Security,
    #[cfg(windows)]
    reparse_tag: Option<u32>,
    pub(super) size: u64,
    pub(super) mtime: (i64, i64),
    pub(super) ctime: (i64, i64),
}
impl Stamp {
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn stat(s: &libc::stat) -> Self {
        platform::stat_stamp(s)
    }
    #[cfg(unix)]
    pub(super) fn of(m: &std::fs::Metadata) -> Self {
        Self {
            key: Key {
                dev: m.dev(),
                ino: m.ino(),
            },
            mode: m.mode(),
            size: m.len(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        }
    }
    pub(super) fn same_type_security(&self, other: &Self) -> bool {
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
    #[cfg(any(not(target_os = "linux"), test))]
    fn native(m: &pcl_core::retained_fs::Metadata) -> Result<Self> {
        use pcl_core::retained_fs::Identity;
        #[cfg(unix)]
        use pcl_core::retained_fs::Security;
        let key = match &m.identity {
            #[cfg(unix)]
            Identity::Unix { device, inode } => Key {
                dev: *device,
                ino: *inode,
            },
            #[cfg(windows)]
            Identity::Windows { volume, file } => Key {
                format: WindowsFormat::FileId128V1,
                volume: *volume,
                file_id: *file,
            },
            _ => return Err("导入文件身份不属于当前平台".into()),
        };
        #[cfg(unix)]
        let mode = match m.security {
            Security::Unix { mode, .. } => mode,
            _ => return Err("导入权限不属于当前平台".into()),
        };
        Ok(Self {
            key,
            size: m.len,
            #[cfg(unix)]
            mode,
            #[cfg(windows)]
            kind: m.kind,
            #[cfg(windows)]
            security: m.security.clone(),
            #[cfg(windows)]
            reparse_tag: m.reparse_tag,
            mtime: (m.modified.seconds, i64::from(m.modified.nanos)),
            ctime: (m.changed.seconds, i64::from(m.changed.nanos)),
        })
    }
    pub(super) fn directory(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
        }
        #[cfg(windows)]
        {
            self.kind == pcl_core::retained_fs::Kind::Directory && self.reparse_tag.is_none()
        }
    }
    pub(super) fn regular(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
        }
        #[cfg(windows)]
        {
            self.kind == pcl_core::retained_fs::Kind::Regular && self.reparse_tag.is_none()
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub(super) stamp: Stamp,
    pub(super) hash: String,
}
impl Snapshot {
    // Hard-link publication changes ctime. Identity and full content remain
    // mandatory for rollback; timestamps remain mandatory for external inputs.
    pub(super) fn owned_matches(&self, other: &Self) -> bool {
        self.stamp.key == other.stamp.key
            && self.stamp.size == other.stamp.size
            && self.stamp.same_type_security(&other.stamp)
            && self.hash == other.hash
    }
}

#[cfg(target_os = "linux")]
#[path = "filesystem/linux.rs"]
mod platform;
#[cfg(not(target_os = "linux"))]
#[path = "filesystem/native.rs"]
mod platform;
pub(super) use platform::*;

pub(super) fn hash_file(
    mut file: File,
    limit: u64,
    cancel: Option<&AtomicBool>,
) -> Result<Snapshot> {
    let before = file_stamp(&file)?;
    if !before.regular() || before.size > limit {
        return Err("导入文件类型无效或超过大小限制".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if let Some(c) = cancel {
            check(c)?;
        }
        let n = file.read(&mut buffer).map_err(error)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .filter(|s| *s <= limit)
            .ok_or("导入文件超过大小限制")?;
        hash.update(&buffer[..n]);
    }
    if size != before.size || file_stamp(&file)? != before {
        return Err(super::changed());
    }
    Ok(Snapshot {
        stamp: before,
        hash: format!("{:x}", hash.finalize()),
    })
}
pub(super) fn open_source(path: &Path) -> Result<File> {
    let parent = path.parent().ok_or("导入源路径无效")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("导入源文件名必须是 UTF-8")?;
    Dir::open(parent)?.regular(name)
}
pub(super) fn source_snapshot(path: &Path, cancel: Option<&AtomicBool>) -> Result<Snapshot> {
    hash_file(open_source(path)?, MAX_BYTES, cancel)
}
#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "filesystem/native.rs"]
mod portable;
