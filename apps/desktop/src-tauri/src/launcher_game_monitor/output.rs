//! Bounded byte streaming: keep the longest-secret suffix until another chunk
//! arrives, so a token split at any input boundary cannot be written to disk.
//! There is no line buffer. UTF-8 normalization also retains only a 3-byte suffix.
use std::{
    fs::File,
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
pub(super) const LOG_LIMIT: u64 = 64 * 1024 * 1024;
const BLOCK: usize = 8192;
pub(super) struct Redactor {
    secrets: Vec<Vec<u8>>,
    pending: Vec<u8>,
    keep: usize,
}
impl Redactor {
    #[cfg(test)]
    pub(super) fn pending_len(&self) -> usize {
        self.pending.len()
    }
    pub fn new(secrets: Vec<String>) -> Result<Self, String> {
        if secrets.len() > 256
            || secrets.iter().any(|secret| secret.len() > 16 * 1024)
            || secrets.iter().map(String::len).sum::<usize>() > 256 * 1024
        {
            return Err("日志脱敏数据超过安全上限".into());
        }
        let mut secrets = secrets
            .into_iter()
            .filter(|secret| !secret.is_empty())
            .map(String::into_bytes)
            .collect::<Vec<_>>();
        secrets.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        secrets.dedup();
        let keep = secrets
            .first()
            .map(|secret| secret.len().saturating_sub(1))
            .unwrap_or(0);
        Ok(Self {
            secrets,
            pending: Vec::new(),
            keep,
        })
    }
    pub fn feed(&mut self, bytes: &[u8], finished: bool) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let safe = if finished {
            self.pending.len()
        } else {
            self.pending.len().saturating_sub(self.keep)
        };
        let mut output = Vec::new();
        let mut consumed = 0;
        while consumed < safe {
            if let Some(secret) = self
                .secrets
                .iter()
                .find(|secret| self.pending[consumed..].starts_with(secret))
            {
                output.extend_from_slice(b"[REDACTED]");
                consumed += secret.len();
            } else {
                output.push(self.pending[consumed]);
                consumed += 1;
            }
        }
        self.pending.drain(..consumed);
        output
    }
}
struct Utf8 {
    pending: Vec<u8>,
}
impl Utf8 {
    fn convert(&mut self, bytes: &[u8], finished: bool) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        let mut output = Vec::new();
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(text) => {
                    output.extend(text.as_bytes());
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    output.extend_from_slice(&self.pending[consumed..end]);
                    consumed = end;
                    match error.error_len() {
                        Some(length) => {
                            output.extend_from_slice("\u{fffd}".as_bytes());
                            consumed += length;
                        }
                        None if finished => {
                            output.extend_from_slice("\u{fffd}".as_bytes());
                            consumed = self.pending.len();
                        }
                        None => break,
                    }
                }
            }
        }
        self.pending.drain(..consumed);
        output
    }
}
/// The file cap is shared across stdout/stderr under the same mutex. After the
/// cap or a write failure, streams keep draining so a verbose game cannot block.
pub(super) fn stream(
    mut reader: impl Read,
    log: Arc<Mutex<File>>,
    mut redactor: Redactor,
    stop: Option<Arc<AtomicBool>>,
) -> Result<(), String> {
    let mut block = [0u8; BLOCK];
    let mut utf8 = Utf8 {
        pending: Vec::new(),
    };
    let mut frame = Vec::new();
    let mut disabled = false;
    loop {
        if stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            break;
        }
        let count = match reader.read(&mut block) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            Err(_) => break,
        };
        let safe = redactor.feed(&block[..count], count == 0);
        let safe = utf8.convert(&safe, count == 0);
        frame.extend(safe);
        // Preserve ordinary stdout/stderr lines with a fixed frame cap for
        // unterminated lines. Input reads never allocate an unbounded line.
        loop {
            let cut = frame
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|cut| cut + 1)
                .filter(|cut| *cut <= 64 * 1024)
                .or_else(|| (frame.len() >= 64 * 1024).then_some(64 * 1024))
                .or_else(|| (count == 0 && !frame.is_empty()).then_some(frame.len()));
            let Some(mut cut) = cut else {
                break;
            };
            while cut < frame.len() && frame[cut] & 0b1100_0000 == 0b1000_0000 {
                cut -= 1;
            }
            if !disabled {
                let mut file = log.lock().map_err(|_| "日志文件状态不可用")?;
                let metadata =
                    pcl_core::retained_fs::metadata(&file).map_err(|_| "日志文件状态不可用")?;
                if metadata.links != 1
                    || metadata.kind != pcl_core::retained_fs::Kind::Regular
                    || metadata.reparse_tag.is_some()
                    || metadata.len.saturating_add(cut as u64) > LOG_LIMIT
                    || file.write_all(&frame[..cut]).is_err()
                {
                    disabled = true;
                }
            }
            frame.drain(..cut);
        }
        if count == 0 {
            break;
        }
    }
    // Cancellation only discards the withheld suffix; emitting it after an
    // interrupted stream could expose an incomplete credential prefix.
    Ok(())
}
