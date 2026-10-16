//! Bounded tails. Plain files seek directly to their suffix. Gzip validates all
//! members and trailers within fixed input/output budgets while retaining only
//! a suffix. A partial first line is discarded before redaction, so a truncated
//! credential-bearing line cannot leak a fragment of a known secret.
use crate::launcher_local::filesystem::Stamp;
use flate2::read::MultiGzDecoder;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
pub(super) const MAX_TEXT_BYTES: usize = 256 * 1024;
pub(super) const MAX_GZIP_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const MAX_INFLATED_BYTES: u64 = 32 * 1024 * 1024;

pub(super) struct TextTail {
    pub text: String,
    pub truncated: bool,
}
fn check(closing: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，游戏日志读取已取消".into());
    }
    if Instant::now() >= deadline {
        return Err("游戏日志读取超过 15 秒上限，请稍后重试".into());
    }
    Ok(())
}
fn tail(bytes: Vec<u8>, truncated: bool) -> TextTail {
    let start = if truncated {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1)
    } else {
        0
    };
    // Lossy UTF-8 can expand each retained byte into three bytes. Keep that
    // bounded expansion intact until account redaction has seen every complete
    // retained line; cutting it here could split a known credential first.
    TextTail {
        text: String::from_utf8_lossy(&bytes[start..]).into_owned(),
        truncated,
    }
}
pub(super) fn bound_redacted(text: String, mut truncated: bool) -> TextTail {
    if text.len() <= MAX_TEXT_BYTES {
        return TextTail { text, truncated };
    }
    truncated = true;
    let mut start = text.len() - MAX_TEXT_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    TextTail {
        text: text[start..].to_owned(),
        truncated,
    }
}
pub(super) fn read(
    file: &mut File,
    stamp: &Stamp,
    gzip: bool,
    closing: &AtomicBool,
) -> Result<TextTail, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    check(closing, deadline)?;
    if gzip {
        if stamp.bytes > MAX_GZIP_BYTES {
            return Err("压缩游戏日志超过 8 MiB 输入上限".into());
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "无法读取压缩游戏日志")?;
        // Take also bounds unexpected concurrent growth. Selected::verify will
        // reject every stamp change before either text or an error is returned.
        let mut decoder = MultiGzDecoder::new(file.take(MAX_GZIP_BYTES + 1));
        let mut total = 0u64;
        let mut suffix = Vec::with_capacity(MAX_TEXT_BYTES + 64 * 1024);
        let mut buffer = [0; 64 * 1024];
        loop {
            check(closing, deadline)?;
            let count = decoder
                .read(&mut buffer)
                .map_err(|_| "压缩游戏日志损坏或不完整，无法读取")?;
            if count == 0 {
                break;
            }
            total = total.checked_add(count as u64).ok_or("压缩日志长度无效")?;
            if total > MAX_INFLATED_BYTES {
                return Err("压缩游戏日志解压超过 32 MiB 输出上限".into());
            }
            suffix.extend_from_slice(&buffer[..count]);
            if suffix.len() > MAX_TEXT_BYTES {
                suffix.drain(..suffix.len() - MAX_TEXT_BYTES);
            }
        }
        Ok(tail(suffix, total > MAX_TEXT_BYTES as u64))
    } else {
        let offset = stamp.bytes.saturating_sub(MAX_TEXT_BYTES as u64);
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| "无法读取游戏日志尾部")?;
        let mut bytes = Vec::with_capacity(MAX_TEXT_BYTES);
        (&mut *file)
            .take(MAX_TEXT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "无法读取游戏日志尾部")?;
        check(closing, deadline)?;
        if bytes.len() as u64 != stamp.bytes - offset {
            return Err(super::CHANGED.into());
        }
        Ok(tail(bytes, offset != 0))
    }
}
