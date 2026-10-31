//! Platform-native identity and generated source access for resource batches.
use super::{check, error, Result, MAX_FILES, MAX_FILE_BYTES};
#[cfg(not(target_os = "linux"))]
use super::{MAX_BATCH_BYTES, MAX_JOURNAL_BYTES};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::AtomicBool,
};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resource_ops) struct Key {
    #[cfg(unix)]
    pub(in crate::resource_ops) dev: u64,
    #[cfg(unix)]
    pub(in crate::resource_ops) ino: u64,
    #[cfg(windows)]
    format: String,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resource_ops) struct Owned {
    pub(in crate::resource_ops) key: Key,
    pub(in crate::resource_ops) size: u64,
    pub(in crate::resource_ops) sha512: String,
    #[cfg(unix)]
    pub(in crate::resource_ops) mode: u32,
    #[cfg(windows)]
    kind: pcl_core::retained_fs::Kind,
    #[cfg(windows)]
    security: pcl_core::retained_fs::Security,
    #[cfg(windows)]
    reparse_tag: Option<u32>,
}
impl Owned {
    pub(in crate::resource_ops) fn regular(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
        }
        #[cfg(windows)]
        {
            self.kind == pcl_core::retained_fs::Kind::Regular && self.reparse_tag.is_none()
        }
    }
    pub(in crate::resource_ops) fn same_security(&self, other: &Self) -> bool {
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
    pub(in crate::resource_ops) fn from_file(
        file: &File,
        size: u64,
        sha512: String,
    ) -> Result<Self> {
        #[cfg(windows)]
        let metadata = pcl_core::retained_fs::metadata(file).map_err(error)?;
        Ok(Self {
            key: key_file(file)?,
            size,
            sha512,
            #[cfg(unix)]
            mode: file.metadata().map_err(error)?.mode(),
            #[cfg(windows)]
            kind: metadata.kind,
            #[cfg(windows)]
            security: metadata.security,
            #[cfg(windows)]
            reparse_tag: metadata.reparse_tag,
        })
    }
}
pub(in crate::resource_ops) fn key_file(file: &File) -> Result<Key> {
    #[cfg(unix)]
    {
        let m = file.metadata().map_err(error)?;
        Ok(Key {
            dev: m.dev(),
            ino: m.ino(),
        })
    }
    #[cfg(windows)]
    {
        match pcl_core::retained_fs::metadata(file)
            .map_err(error)?
            .identity
        {
            pcl_core::retained_fs::Identity::Windows { volume, file } => Ok(Key {
                format: "windows-file-id-128-v1".into(),
                volume,
                file_id: file,
            }),
            _ => Err("资源身份不属于当前平台".into()),
        }
    }
}
pub(in crate::resource_ops) fn single(file: &File) -> Result<bool> {
    #[cfg(unix)]
    {
        Ok(file.metadata().map_err(error)?.nlink() == 1)
    }
    #[cfg(windows)]
    {
        Ok(pcl_core::retained_fs::metadata(file).map_err(error)?.links == 1)
    }
}
pub(in crate::resource_ops) fn anonymous_source(
    file: &crate::modrinth_install::source::Input,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let m = file.metadata().map_err(error)?;
        if !m.is_file() || m.nlink() != 0 {
            return Err("资源下载来源必须是匿名普通文件描述符".into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        file.verify_owned()
    }
}
#[cfg(target_os = "linux")]
#[path = "filesystem/linux.rs"]
mod native;
#[cfg(not(target_os = "linux"))]
#[path = "filesystem/native.rs"]
mod native;
pub(in crate::resource_ops) use native::*;
pub(in crate::resource_ops) fn verify_file(
    file: &mut File,
    expected_key: Option<&Key>,
    size: u64,
    hash: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Owned> {
    let token = super::super::token_for(file)?;
    let m = file.metadata().map_err(error)?;
    let key = key_file(file)?;
    if !m.is_file()
        || m.len() != size
        || size > MAX_FILE_BYTES
        || expected_key.is_some_and(|k| *k != key)
    {
        return Err("资源文件大小或 inode 已变化".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    let mut digest = Sha512::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if let Some(c) = cancel {
            check(c)?;
        }
        let n = file.read(&mut buffer).map_err(error)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .filter(|n| *n <= size)
            .ok_or("资源文件实际大小超过声明")?;
        digest.update(&buffer[..n]);
    }
    let sha512 = format!("{:x}", digest.finalize());
    if count != size || sha512 != hash || super::super::token_for(file)? != token {
        return Err("资源文件 SHA512 校验失败或读取期间已经变化".into());
    }
    Owned::from_file(file, size, sha512)
}
pub(in crate::resource_ops) fn verify_named(dir: &Dir, name: &str, owned: &Owned) -> Result<()> {
    let mut file = dir.regular(name)?;
    let before = super::super::token_for(&file)?;
    let now = verify_file(&mut file, Some(&owned.key), owned.size, &owned.sha512, None)?;
    if !now.same_security(owned) || super::super::token_for(&dir.regular(name)?)? != before {
        return Err("待恢复资源文件的路径或权限已经变化".into());
    }
    Ok(())
}
