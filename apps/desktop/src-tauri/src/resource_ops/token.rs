//! Shared resource-panel tokens; no IPC or filesystem mutation authority.
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
pub(crate) fn token_for(file: &File) -> Result<String, String> {
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
pub(crate) fn token_for(file: &File) -> Result<String, String> {
    use pcl_core::retained_fs::{self, Identity, Kind};
    use sha2::{Digest, Sha256};
    let stat = retained_fs::metadata(file).map_err(|e| e.to_string())?;
    if stat.kind != Kind::Regular || stat.reparse_tag.is_some() {
        return Err("只允许操作普通文件".into());
    }
    let Identity::Windows { volume, file } = stat.identity else {
        return Err("资源文件身份平台不匹配".into());
    };
    let id = file
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let security = serde_json::to_vec(&stat.security).map_err(|e| e.to_string())?;
    Ok(format!(
        "w1:{}:{}:{}:{}:{}:{}:{}:{}:{:x}",
        stat.len,
        stat.modified.seconds,
        stat.modified.nanos,
        volume,
        id,
        stat.changed.seconds,
        stat.changed.nanos,
        stat.links,
        Sha256::digest(security)
    ))
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
    parts.len() == 10
        && parts[0] == "w1"
        && value.len() <= 300
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
}
