//! Nonblocking NDJSON transport and incremental decoding. Input ownership and
//! output backpressure have separate, short bounds from the build deadline.
use super::{process, Failure};
use serde_json::{json, Value};
use std::{
    cell::Cell,
    io,
    os::fd::RawFd,
    time::{Duration, Instant},
};

const MAX_REQUEST: usize = 16 * 1024;
const MAX_CONTROL: usize = 1024;
const MAX_EVENT: usize = 64 * 1024;
const MAX_WIRE: usize = super::MAX_OUTPUT * 8;
const TERMINAL_RESERVE: usize = 8192;

pub(super) fn nonblocking(fd: RawFd) -> Result<(), Failure> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(Failure::io(
            "Cannot configure build pipe",
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}
pub(super) struct Input {
    fd: RawFd,
    pending: Vec<u8>,
    eof: bool,
    cancelled: bool,
}
impl Input {
    pub fn new(fd: RawFd) -> Result<Self, Failure> {
        nonblocking(fd)?;
        Ok(Self {
            fd,
            pending: vec![],
            eof: false,
            cancelled: false,
        })
    }
    fn read(&mut self, limit: usize) -> Result<(), Failure> {
        let mut bytes = [0u8; 4096];
        loop {
            let count = unsafe { libc::read(self.fd, bytes.as_mut_ptr().cast(), bytes.len()) };
            if count == 0 {
                self.eof = true;
                return Ok(());
            }
            if count > 0 {
                self.pending.extend_from_slice(&bytes[..count as usize]);
                if self.pending.len() > limit {
                    return Err(Failure::new(
                        "BUILD_CONTROL_LIMIT",
                        "Build input exceeds its transport limit",
                    ));
                }
                continue;
            }
            let error = io::Error::last_os_error();
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
            if self.eof || process::parent_cancelled() {
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
        if self.cancelled || process::parent_cancelled() {
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
    fd: RawFd,
    run_id: Option<String>,
    wire_bytes: Cell<usize>,
}
impl Writer {
    pub fn new(fd: RawFd) -> Result<Self, Failure> {
        nonblocking(fd)?;
        Ok(Self {
            fd,
            run_id: None,
            wire_bytes: Cell::new(0),
        })
    }
    pub fn set_run_id(&mut self, run_id: &str) {
        self.run_id = Some(run_id.to_owned());
    }
    pub fn event(&self, value: &Value) -> Result<(), Failure> {
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
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut written = 0;
        while written < bytes.len() {
            let count = unsafe {
                libc::write(
                    self.fd,
                    bytes[written..].as_ptr().cast(),
                    bytes.len() - written,
                )
            };
            if count > 0 {
                written += count as usize;
                continue;
            }
            let error = io::Error::last_os_error();
            if count < 0
                && error.kind() != io::ErrorKind::WouldBlock
                && error.kind() != io::ErrorKind::Interrupted
            {
                return Err(Failure::io("Build event owner disconnected", error));
            }
            if Instant::now() >= deadline {
                return Err(Failure::new(
                    "BUILD_EVENT_TIMEOUT",
                    "Build event owner stopped reading",
                ));
            }
            let mut poll = libc::pollfd {
                fd: self.fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut poll, 1, 10);
            }
        }
        self.wire_bytes.set(self.wire_bytes.get() + bytes.len());
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Utf8 {
    pending: Vec<u8>,
}
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
