//! Shared resource-panel tokens; no IPC or filesystem mutation authority.
use std::{
    fs::File,
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) const MAX_FINGERPRINT_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const MAX_TOKEN_BYTES: usize = if cfg!(windows) { 300 } else { 256 };

fn cancelled(cancel: Option<&AtomicBool>) -> Result<(), String> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        Err("操作已取消".into())
    } else {
        Ok(())
    }
}

pub(crate) fn token_for(file: &File) -> Result<String, String> {
    token_for_cancellable(file, None)
}
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
pub(crate) fn token_for_cancellable(
    file: &File,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    cancelled(cancel)?;
    let stat = file.metadata().map_err(|e| e.to_string())?;
    if !stat.is_file() {
        return Err("只允许操作普通文件".into());
    }
    // Every integer is inside a string so JavaScript cannot round dev/ino or
    // nanosecond timestamps. ctime also detects an in-place edit whose writer
    // restores mtime; journal recovery compares the stable identity separately.
    Ok(format!(
        "v2:{}:{}:{}:{}:{}:{}:{}",
        stat.len(),
        stat.mtime(),
        stat.mtime_nsec(),
        stat.dev(),
        stat.ino(),
        stat.ctime(),
        stat.ctime_nsec()
    ))
}

#[cfg(unix)]
pub(crate) fn valid_token(value: &str) -> bool {
    let parts: Vec<_> = value.split(':').collect();
    parts.len() == 8
        && parts[0] == "v2"
        && parts[1].parse::<u64>().is_ok()
        && parts[2].parse::<i64>().is_ok()
        && parts[3]
            .parse::<u32>()
            .is_ok_and(|nanos| nanos < 1_000_000_000)
        && parts[4].parse::<u64>().is_ok()
        && parts[5].parse::<u64>().is_ok()
        && parts[6].parse::<i64>().is_ok()
        && parts[7]
            .parse::<u32>()
            .is_ok_and(|nanos| nanos < 1_000_000_000)
        && value.len() <= 200
}

#[cfg(unix)]
pub(crate) fn same_identity(left: &str, right: &str) -> bool {
    valid_token(left) && valid_token(right) && left.split(':').take(6).eq(right.split(':').take(6))
}

#[cfg(windows)]
fn checked_metadata(stat: &pcl_core::retained_fs::Metadata) -> Result<(), String> {
    use pcl_core::retained_fs::{Identity, Kind};
    if stat.kind != Kind::Regular || stat.reparse_tag.is_some() || stat.links == 0 {
        return Err("只允许操作普通文件".into());
    }
    if stat.len > MAX_FINGERPRINT_FILE_BYTES {
        return Err("文件超过操作大小限制".into());
    }
    if !matches!(stat.identity, Identity::Windows { .. }) {
        return Err("资源文件身份平台不匹配".into());
    }
    Ok(())
}

/// Only internal readers that hashed these actual bytes and checked the same
/// complete native metadata before/after may mint this token. This is not IPC,
/// and timestamp equality alone is never a content-digest cache key.
#[cfg(windows)]
pub(crate) fn for_verified_snapshot(
    stat: &pcl_core::retained_fs::Metadata,
    contents_sha256: &str,
) -> Result<String, String> {
    use pcl_core::retained_fs::Identity;
    use sha2::{Digest, Sha256};
    checked_metadata(stat)?;
    if contents_sha256.len() != 64
        || !contents_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("资源文件内容指纹无效".into());
    }
    let Identity::Windows { volume, file } = &stat.identity else {
        return Err("资源文件身份平台不匹配".into());
    };
    let id = file
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let security = serde_json::to_vec(&stat.security).map_err(|e| e.to_string())?;
    Ok(format!(
        "w2:{}:{}:{}:{}:{}:{}:{}:{}:{:x}:{}",
        stat.len,
        stat.modified.seconds,
        stat.modified.nanos,
        volume,
        id,
        stat.changed.seconds,
        stat.changed.nanos,
        stat.links,
        Sha256::digest(security),
        contents_sha256
    ))
}

#[cfg(windows)]
fn independent_reader(file: &File) -> Result<File, String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            ReOpenFile, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_SEQUENTIAL_SCAN,
            FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, READ_CONTROL,
        },
    };
    // DuplicateHandle/try_clone shares its file cursor, and Windows seek_read
    // changes that cursor too. Reopen this exact held object without a path.
    let handle = unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(all(test, windows))]
std::thread_local! {
    static AFTER_ACTUAL_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(windows)]
pub(crate) fn token_for_cancellable(
    file: &File,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    use pcl_core::retained_fs;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    cancelled(cancel)?;
    let before = retained_fs::metadata(file).map_err(|e| e.to_string())?;
    checked_metadata(&before)?;
    let mut reader = independent_reader(file)?;
    if retained_fs::metadata(&reader).map_err(|e| e.to_string())? != before {
        return Err("资源文件在生成内容指纹前已变化".into());
    }
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    let mut total = 0u64;
    loop {
        cancelled(cancel)?;
        // At most one sentinel byte beyond the admitted size is read. A sparse
        // or growing file cannot turn token generation into an unbounded read.
        let available = (before.len - total + 1).min(buffer.len() as u64) as usize;
        let length = reader
            .read(&mut buffer[..available])
            .map_err(|e| e.to_string())?;
        if length == 0 {
            break;
        }
        total = total
            .checked_add(length as u64)
            .filter(|n| *n <= before.len)
            .ok_or("资源文件在生成内容指纹时变大")?;
        hasher.update(&buffer[..length]);
        #[cfg(test)]
        AFTER_ACTUAL_READ.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    cancelled(cancel)?;
    if total != before.len
        || retained_fs::metadata(&reader).map_err(|e| e.to_string())? != before
        || retained_fs::metadata(file).map_err(|e| e.to_string())? != before
    {
        return Err("资源文件在生成内容指纹时已变化".into());
    }
    for_verified_snapshot(&before, &format!("{:x}", hasher.finalize()))
}

#[cfg(windows)]
pub(crate) fn valid_token(value: &str) -> bool {
    let parts: Vec<_> = value.split(':').collect();
    let hex = |value: &str, length: usize| {
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    ((parts.len() == 10 && parts[0] == "w1")
        || (parts.len() == 11 && parts[0] == "w2" && hex(parts[10], 64)))
        && value.len() <= MAX_TOKEN_BYTES
        && parts[1].parse::<u64>().is_ok()
        && parts[2].parse::<i64>().is_ok()
        && parts[3].parse::<u32>().is_ok_and(|n| n < 1_000_000_000)
        && parts[4].parse::<u64>().is_ok()
        && hex(parts[5], 32)
        && parts[6].parse::<i64>().is_ok()
        && parts[7].parse::<u32>().is_ok_and(|n| n < 1_000_000_000)
        && parts[8].parse::<u64>().is_ok_and(|n| n > 0)
        && hex(parts[9], 64)
}

#[cfg(windows)]
// Legacy w1 stays parseable, but its missing content witness is never
// projected into w2. Mixed-version recovery preserves evidence and refuses.
pub(crate) fn same_identity(left: &str, right: &str) -> bool {
    valid_token(left)
        && valid_token(right)
        && left
            .split(':')
            .enumerate()
            .filter(|(i, _)| !matches!(i, 6 | 7))
            .eq(right
                .split(':')
                .enumerate()
                .filter(|(i, _)| !matches!(i, 6 | 7)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_detect_content_edits_and_keep_identity_across_rename() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("a");
        let second = temp.path().join("b");
        std::fs::write(&first, b"before").unwrap();
        let before = token_for(&File::open(&first).unwrap()).unwrap();
        assert!(valid_token(&before));
        std::fs::rename(&first, &second).unwrap();
        let moved = token_for(&File::open(&second).unwrap()).unwrap();
        assert!(same_identity(&before, &moved));
        std::fs::write(&second, b"after-change").unwrap();
        let edited = token_for(&File::open(&second).unwrap()).unwrap();
        assert_ne!(moved, edited);
        assert!(!same_identity(&moved, &edited));
    }
    #[test]
    fn malformed_tokens_are_never_recovery_authority() {
        for value in ["", "v2:0:0:0:0:0:0:1000000000", "w1:0:0:0:0:0:0:0:1:0"] {
            assert!(!valid_token(value));
            assert!(!same_identity(value, value));
        }
    }

    #[test]
    fn token_reads_preserve_the_callers_offset_and_observe_cancellation() {
        use std::io::{Read, Seek, SeekFrom};
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("offset");
        std::fs::write(&path, b"abcdef").unwrap();
        let mut file = File::open(path).unwrap();
        file.seek(SeekFrom::Start(3)).unwrap();
        assert!(valid_token(&token_for(&file).unwrap()));
        assert_eq!(file.stream_position().unwrap(), 3);
        let cancel = AtomicBool::new(true);
        assert_eq!(
            token_for_cancellable(&file, Some(&cancel)).unwrap_err(),
            "操作已取消"
        );
        assert_eq!(file.stream_position().unwrap(), 3);
        let mut rest = String::new();
        file.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "def");
    }

    #[cfg(windows)]
    #[test]
    fn windows_w2_reads_actual_content_and_never_upgrades_a_w1_witness() {
        use sha2::{Digest, Sha256};
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("content");
        std::fs::write(&path, b"before").unwrap();
        let file = File::open(&path).unwrap();
        let token = token_for(&file).unwrap();
        let mut parts: Vec<_> = token.split(':').collect();
        assert_eq!(parts[0], "w2");
        assert_eq!(parts[10], format!("{:x}", Sha256::digest(b"before")));
        assert!(token.len() <= MAX_TOKEN_BYTES);
        parts.truncate(10);
        parts[0] = "w1";
        let legacy = parts.join(":");
        assert!(valid_token(&legacy));
        assert!(same_identity(&legacy, &legacy));
        assert!(!same_identity(&legacy, &token));
        assert!(!same_identity(&token, &legacy));
        assert!(!valid_token(&token[..token.len() - 1]));
    }

    #[cfg(windows)]
    #[test]
    fn windows_content_version_survives_suppressed_timestamp_updates() {
        use std::{io::Write, os::windows::io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            FileBasicInfo, SetFileInformationByHandle, FILE_BASIC_INFO,
        };
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("unchanged-times");
        std::fs::write(&path, b"before").unwrap();
        let mut writer = File::options().write(true).open(&path).unwrap();
        let suppressed = FILE_BASIC_INFO {
            LastWriteTime: -1,
            ChangeTime: -1,
            ..Default::default()
        };
        assert_ne!(
            unsafe {
                SetFileInformationByHandle(
                    writer.as_raw_handle(),
                    FileBasicInfo,
                    (&suppressed as *const FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            },
            0
        );
        let reader = File::open(&path).unwrap();
        let before = token_for(&reader).unwrap();
        writer.write_all(b"after!").unwrap();
        writer.sync_all().unwrap();
        let after = token_for(&reader).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"after!");
        let left: Vec<_> = before.split(':').collect();
        let right: Vec<_> = after.split(':').collect();
        // This native fixture deliberately keeps both metadata timestamps
        // unchanged; content identity must still invalidate the selection.
        assert_eq!(&left[..10], &right[..10]);
        assert_ne!(left[10], right[10]);
        assert!(!same_identity(&before, &after));
    }

    #[cfg(windows)]
    #[test]
    fn windows_cancel_after_a_real_hash_block_and_oversize_fail_without_cursor_changes() {
        use std::{
            io::{Seek, SeekFrom},
            sync::Arc,
        };
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("cancel");
        std::fs::write(&path, vec![b'x'; 256 * 1024]).unwrap();
        let mut file = File::open(&path).unwrap();
        file.seek(SeekFrom::Start(9)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let after_read = cancel.clone();
        AFTER_ACTUAL_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || after_read.store(true, Ordering::Release)))
        });
        assert_eq!(
            token_for_cancellable(&file, Some(&cancel)).unwrap_err(),
            "操作已取消"
        );
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(file.stream_position().unwrap(), 9);
        let writer = File::options().write(true).open(&path).unwrap();
        writer
            .set_len(super::MAX_FINGERPRINT_FILE_BYTES + 1)
            .unwrap();
        assert!(token_for(&file).unwrap_err().contains("大小限制"));
        assert_eq!(file.stream_position().unwrap(), 9);
    }
}
