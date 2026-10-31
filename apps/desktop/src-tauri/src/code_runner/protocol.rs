//! Nonblocking NDJSON transport and incremental decoding. Input ownership and
//! output backpressure have separate, short bounds from the build deadline.
use super::Failure;
use serde_json::{json, Value};
use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

#[path = "protocol/host_pipe.rs"]
pub(crate) mod host_pipe;

const MAX_REQUEST: usize = 16 * 1024;
const MAX_CONTROL: usize = 1024;
const MAX_EVENT: usize = 64 * 1024;
const MAX_WIRE: usize = super::MAX_OUTPUT * 8;
const TERMINAL_RESERVE: usize = 8192;

#[cfg(unix)]
#[path = "protocol/unix.rs"]
mod unix;
#[cfg(windows)]
#[path = "protocol/windows.rs"]
mod windows;
#[cfg(unix)]
use unix as transport;
#[cfg(target_os = "linux")]
pub(super) use unix::nonblocking;
#[cfg(windows)]
use windows as transport;
fn parent_cancelled() -> bool {
    #[cfg(target_os = "linux")]
    {
        return super::process::parent_cancelled();
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    } // Native execution is refused; ownership is the actual stdin pipe.
}
pub(super) struct Input {
    pipe: transport::Reader,
    pending: Vec<u8>,
    eof: bool,
    cancelled: bool,
}
impl Input {
    pub fn stdin() -> Result<Self, Failure> {
        Ok(Self::from_transport(transport::Reader::stdin()?))
    }
    #[cfg(all(test, unix))]
    pub fn new(fd: std::os::fd::RawFd) -> Result<Self, Failure> {
        Ok(Self::from_transport(transport::Reader::new(fd)?))
    }
    #[cfg(all(test, windows))]
    pub fn new(file: std::fs::File) -> Result<Self, Failure> {
        Ok(Self::from_transport(transport::Reader::new(file)?))
    }
    fn from_transport(pipe: transport::Reader) -> Self {
        Self {
            pipe,
            pending: vec![],
            eof: false,
            cancelled: false,
        }
    }
    fn read(&mut self, limit: usize) -> Result<(), Failure> {
        let mut bytes = [0u8; 4096];
        loop {
            let error = match self.pipe.read(&mut bytes) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(count) => {
                    self.pending.extend_from_slice(&bytes[..count]);
                    if self.pending.len() > limit {
                        return Err(Failure::new(
                            "BUILD_CONTROL_LIMIT",
                            "Build input exceeds its transport limit",
                        ));
                    }
                    continue;
                }
                Err(error) => error,
            };
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(Failure::io("Cannot read build ownership pipe", error));
            }
        }
    }
    pub fn request(&mut self, timeout: Duration) -> Result<Vec<u8>, Failure> {
        let deadline = Instant::now() + timeout;
        loop {
            self.read(MAX_REQUEST)?;
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                let result = self.pending.drain(..=end).collect();
                return Ok(result);
            }
            if self.eof || parent_cancelled() {
                return Err(Failure::new(
                    "BUILD_OWNER_CLOSED",
                    "Build owner closed before its request",
                ));
            }
            if Instant::now() >= deadline {
                return Err(Failure::new(
                    "BUILD_REQUEST_TIMEOUT",
                    "Build owner did not provide its request",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn cancelled(&mut self) -> Result<bool, Failure> {
        if self.cancelled || parent_cancelled() {
            return Ok(true);
        }
        self.read(MAX_CONTROL)?;
        // Every post-request message must be one explicit cancel envelope. A
        // broken/unknown ownership message refuses further execution too.
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let bytes: Vec<_> = self.pending.drain(..=end).collect();
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                Failure::new("INVALID_BUILD_CONTROL", "Invalid build cancellation JSON")
            })?;
            if value != json!({"type":"cancel"}) {
                return Err(Failure::new(
                    "INVALID_BUILD_CONTROL",
                    "Expected a build cancel envelope",
                ));
            }
            self.cancelled = true;
        }
        if self.eof {
            self.cancelled = true;
        }
        Ok(self.cancelled)
    }
}

pub(super) struct Writer {
    pipe: transport::Writer,
    run_id: Option<String>,
    wire_bytes: Cell<usize>,
    disconnected: Cell<bool>,
}
impl Writer {
    pub fn stdout() -> Result<Self, Failure> {
        Ok(Self::from_transport(transport::Writer::stdout()?))
    }
    #[cfg(all(test, unix))]
    pub fn new(fd: std::os::fd::RawFd) -> Result<Self, Failure> {
        Ok(Self::from_transport(transport::Writer::new(fd)?))
    }
    #[cfg(all(test, windows))]
    pub fn new(file: std::fs::File) -> Result<Self, Failure> {
        Ok(Self::from_transport(transport::Writer::new(file)?))
    }
    fn from_transport(pipe: transport::Writer) -> Self {
        Self {
            pipe,
            run_id: None,
            wire_bytes: Cell::new(0),
            disconnected: Cell::new(false),
        }
    }
    pub fn set_run_id(&mut self, run_id: &str) {
        self.run_id = Some(run_id.to_owned());
    }
    pub fn event(&self, value: &Value) -> Result<(), Failure> {
        if self.disconnected.get() {
            return Err(Failure::new(
                "BUILD_RUNNER_IO",
                "Build event pipe is already disconnected",
            ));
        }
        let mut value = value.clone();
        value["schemaVersion"] = json!(1);
        if let Some(run_id) = &self.run_id {
            value["runId"] = json!(run_id);
        }
        let mut bytes = serde_json::to_vec(&value)
            .map_err(|_| Failure::new("BUILD_PROTOCOL_ERROR", "Cannot encode build event"))?;
        if bytes.len() > MAX_EVENT {
            return Err(Failure::new(
                "BUILD_PROTOCOL_LIMIT",
                "Build event exceeds its transport limit",
            ));
        }
        bytes.push(b'\n');
        // Tiny, frequent writes have framing overhead too. Reserve a terminal
        // receipt so a noisy process cannot turn a bounded stop into an
        // incomplete protocol stream at the Node transport's total wire cap.
        let wire_limit = if value["type"] == "finished" {
            MAX_WIRE
        } else {
            MAX_WIRE - TERMINAL_RESERVE
        };
        if self.wire_bytes.get().saturating_add(bytes.len()) > wire_limit {
            return Err(Failure::new(
                "BUILD_WIRE_LIMIT",
                "Build event framing exceeded its transport budget",
            ));
        }
        if let Err(error) = self
            .pipe
            .write(&bytes, Instant::now() + Duration::from_millis(500))
        {
            self.disconnected.set(true);
            return Err(error);
        }
        self.wire_bytes.set(self.wire_bytes.get() + bytes.len());
        Ok(())
    }
}

#[derive(Default)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) struct Utf8 {
    pending: Vec<u8>,
}
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl Utf8 {
    pub fn decode(&mut self, bytes: &[u8], finished: bool) -> String {
        self.pending.extend_from_slice(bytes);
        let mut offset = 0;
        let mut text = String::new();
        while offset < self.pending.len() {
            match std::str::from_utf8(&self.pending[offset..]) {
                Ok(valid) => {
                    text.push_str(valid);
                    offset = self.pending.len();
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&self.pending[offset..offset + valid]).unwrap(),
                    );
                    offset += valid;
                    if let Some(length) = error.error_len() {
                        text.push('\u{fffd}');
                        offset += length;
                    } else if finished {
                        text.push('\u{fffd}');
                        offset = self.pending.len();
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending.drain(..offset);
        debug_assert!(self.pending.len() <= 3);
        text
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) struct OutputBudget {
    pub(super) raw_bytes: usize,
    pub(super) text_bytes: usize,
    pub(super) limit: usize,
}
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl OutputBudget {
    pub(super) fn emit(
        &mut self,
        stream: &str,
        text: &str,
        output: &mut impl FnMut(&str, &str) -> Result<(), Failure>,
    ) -> Result<bool, Failure> {
        // Invalid UTF-8 becomes U+FFFD, which can expand one raw byte to three.
        // Both raw I/O and the persisted model-visible text obey the same cap.
        let remaining = self.limit.saturating_sub(self.text_bytes);
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end != 0 {
            output(stream, &text[..end])?;
            self.text_bytes += end;
        }
        Ok(end < text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_runner::portable_tests::{io_fixture, pipe, writer};
    use std::io::Read;

    #[test]
    fn wire_budget_rejects_more_output_but_preserves_a_terminal_receipt() {
        let _guard = io_fixture();
        let (mut read, output) = pipe();
        let writer = writer(&output);
        assert_eq!(
            writer
                .event(&json!({"type":"output", "text":"x".repeat(MAX_EVENT)}))
                .unwrap_err()
                .code,
            "BUILD_PROTOCOL_LIMIT"
        );
        writer.wire_bytes.set(MAX_WIRE - TERMINAL_RESERVE);
        assert_eq!(
            writer
                .event(&json!({"type":"output", "text":"x"}))
                .unwrap_err()
                .code,
            "BUILD_WIRE_LIMIT"
        );
        writer
            .event(&json!({"type":"finished", "status":"output_limit"}))
            .unwrap();
        drop(writer);
        drop(output);
        let mut text = String::new();
        read.read_to_string(&mut text).unwrap();
        assert_eq!(text.lines().count(), 1);
        let receipt: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(receipt["type"], "finished");
        assert_eq!(receipt["status"], "output_limit");
    }
}
