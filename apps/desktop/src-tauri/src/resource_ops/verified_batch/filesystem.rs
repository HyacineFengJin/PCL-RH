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
    verify_file_snapshot(file, expected_key, size, hash, cancel).map(|(owned, _)| owned)
}

// Return the selection token from the same stable bytes used for SHA512. Native
// w2 needs no separate before/after SHA256 reads of this entire source.
pub(in crate::resource_ops) fn verify_file_snapshot(
    file: &mut File,
    expected_key: Option<&Key>,
    size: u64,
    hash: &str,
    cancel: Option<&AtomicBool>,
) -> Result<(Owned, String)> {
    if let Some(c) = cancel {
        check(c)?;
    }
    #[cfg(unix)]
    let token = super::super::download_token(file, cancel)?;
    #[cfg(windows)]
    let before = pcl_core::retained_fs::metadata(file).map_err(error)?;
    #[cfg(windows)]
    if before.len != size
        || before.kind != pcl_core::retained_fs::Kind::Regular
        || before.reparse_tag.is_some()
    {
        return Err("资源文件大小或 inode 已变化".into());
    }
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
    #[cfg(windows)]
    let mut contents = sha2::Sha256::new();
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
        #[cfg(windows)]
        contents.update(&buffer[..n]);
        #[cfg(test)]
        AFTER_VERIFY_READ.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    if let Some(c) = cancel {
        check(c)?;
    }
    let sha512 = format!("{:x}", digest.finalize());
    #[cfg(unix)]
    let changed = super::super::download_token(file, cancel)? != token;
    #[cfg(windows)]
    let changed = pcl_core::retained_fs::metadata(file).map_err(error)? != before;
    if count != size || sha512 != hash || changed {
        return Err("资源文件 SHA512 校验失败或读取期间已经变化".into());
    }
    #[cfg(windows)]
    {
        let token =
            super::super::for_verified_snapshot(&before, &format!("{:x}", contents.finalize()))?;
        let owned = Owned {
            key,
            size,
            sha512,
            kind: before.kind,
            security: before.security,
            reparse_tag: before.reparse_tag,
        };
        Ok((owned, token))
    }
    #[cfg(unix)]
    {
        Ok((Owned::from_file(file, size, sha512)?, token))
    }
}

#[cfg(test)]
std::thread_local! {
    static AFTER_VERIFY_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
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

#[cfg(test)]
mod verification_tests {
    use super::*;
    use std::{
        io::{Seek, SeekFrom, Write},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    fn source(bytes: &[u8]) -> (tempfile::TempDir, File) {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("actual-source");
        let mut file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        (temporary, file)
    }

    #[test]
    fn already_cancelled_verification_never_seeks_or_reads_the_source() {
        let bytes = vec![b'x'; 256 * 1024];
        let (_temporary, mut file) = source(&bytes);
        file.seek(SeekFrom::Start(9)).unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let actual_reads = reads.clone();
        AFTER_VERIFY_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                actual_reads.fetch_add(1, Ordering::SeqCst);
            }));
        });
        let result = verify_file(
            &mut file,
            None,
            bytes.len() as u64,
            &format!("{:x}", Sha512::digest(&bytes)),
            Some(&AtomicBool::new(true)),
        );
        AFTER_VERIFY_READ.with(|hook| drop(hook.borrow_mut().take()));
        assert_eq!(result.unwrap_err(), super::super::CANCELLED);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(file.stream_position().unwrap(), 9);
    }

    #[test]
    fn cancellation_after_an_actual_block_stops_before_the_next_read() {
        let bytes = vec![b'x'; 256 * 1024];
        let (_temporary, mut file) = source(&bytes);
        let cancel = Arc::new(AtomicBool::new(false));
        let after_read = cancel.clone();
        AFTER_VERIFY_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                after_read.store(true, Ordering::Release);
            }));
        });
        assert_eq!(
            verify_file(
                &mut file,
                None,
                bytes.len() as u64,
                &format!("{:x}", Sha512::digest(&bytes)),
                Some(&cancel),
            )
            .unwrap_err(),
            super::super::CANCELLED
        );
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(file.stream_position().unwrap(), 128 * 1024);
    }

    #[cfg(windows)]
    #[test]
    fn verified_w2_contains_the_digest_from_the_actual_sha512_read() {
        use sha2::Sha256;
        let bytes = vec![b'x'; 256 * 1024];
        let (_temporary, mut file) = source(&bytes);
        let metadata = pcl_core::retained_fs::metadata(&file).unwrap();
        let expected = super::super::super::for_verified_snapshot(
            &metadata,
            &format!("{:x}", Sha256::digest(&bytes)),
        )
        .unwrap();
        let read = Arc::new(AtomicBool::new(false));
        let actual_read = read.clone();
        AFTER_VERIFY_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                actual_read.store(true, Ordering::Release);
            }));
        });
        let (owned, token) = verify_file_snapshot(
            &mut file,
            None,
            bytes.len() as u64,
            &format!("{:x}", Sha512::digest(&bytes)),
            None,
        )
        .unwrap();
        assert!(read.load(Ordering::Acquire));
        assert_eq!(token, expected);
        assert_eq!(
            token.split(':').last().unwrap(),
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert_eq!(owned.sha512, format!("{:x}", Sha512::digest(&bytes)));
        assert_eq!(owned.key, key_file(&file).unwrap());
        assert_eq!(owned.security, metadata.security);
        assert_eq!(file.stream_position().unwrap(), bytes.len() as u64);
        assert_eq!(super::super::super::token_for(&file).unwrap(), token);
        assert_eq!(file.stream_position().unwrap(), bytes.len() as u64);
    }

    #[cfg(windows)]
    #[test]
    fn full_native_metadata_change_during_verification_never_mints_w2() {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileBasicInfo, SetFileInformationByHandle, FILE_BASIC_INFO,
        };
        let bytes = vec![b'x'; 256 * 1024];
        let (temporary, mut file) = source(&bytes);
        let before = pcl_core::retained_fs::metadata(&file).unwrap();
        let changed = File::options()
            .write(true)
            .open(temporary.path().join("actual-source"))
            .unwrap();
        let modified = (before.modified.seconds + 11_644_473_600) * 10_000_000
            + i64::from(before.modified.nanos / 100)
            + 10_000_000;
        AFTER_VERIFY_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let basic = FILE_BASIC_INFO {
                    LastWriteTime: modified,
                    ..Default::default()
                };
                assert_ne!(
                    unsafe {
                        SetFileInformationByHandle(
                            changed.as_raw_handle(),
                            FileBasicInfo,
                            (&basic as *const FILE_BASIC_INFO).cast(),
                            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                        )
                    },
                    0
                );
            }));
        });
        assert!(verify_file_snapshot(
            &mut file,
            None,
            bytes.len() as u64,
            &format!("{:x}", Sha512::digest(&bytes)),
            None,
        )
        .is_err());
        assert_eq!(
            std::fs::read(temporary.path().join("actual-source")).unwrap(),
            bytes
        );
        assert_ne!(
            pcl_core::retained_fs::metadata(&file).unwrap().modified,
            before.modified
        );
    }
}
