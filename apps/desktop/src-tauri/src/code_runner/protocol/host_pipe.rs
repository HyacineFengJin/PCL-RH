//! Internal transport for a trusted host, sharing the build helper's bounded
//! workers. A closed token observes actual File retirement, not a request to
//! cancel IO; the host owner can retain its lock until every token is closed.
use super::{super::Failure, transport};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::{
    fs::File,
    io,
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};

fn failure(error: Failure) -> io::Error {
    let kind = match error.code {
        "BUILD_EVENT_TIMEOUT" => io::ErrorKind::TimedOut,
        "BUILD_PROTOCOL_LIMIT" => io::ErrorKind::InvalidInput,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.message)
}

pub(crate) struct PipeReader {
    reader: transport::Reader,
    #[cfg(unix)]
    file: Option<File>,
    closed: Arc<AtomicBool>,
    pending: Vec<u8>,
    offset: usize,
}
impl PipeReader {
    pub(crate) fn new(file: File) -> io::Result<Self> {
        #[cfg(unix)]
        let reader = transport::Reader::new(file.as_raw_fd()).map_err(failure)?;
        #[cfg(windows)]
        let reader = transport::Reader::new(file).map_err(failure)?;
        #[cfg(unix)]
        let closed = Arc::new(AtomicBool::new(false));
        #[cfg(windows)]
        let closed = reader.closed();
        Ok(Self {
            reader,
            #[cfg(unix)]
            file: Some(file),
            closed,
            pending: Vec::new(),
            offset: 0,
        })
    }
    pub(crate) fn closed(&self) -> Arc<AtomicBool> {
        self.closed.clone()
    }
    pub(crate) fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Host read buffer exceeds 4096 bytes",
            ));
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.pending.is_empty() {
            // The worker has one fixed bounded read. Preserve every byte if a
            // later caller supplies a smaller buffer than the in-flight call.
            let mut buffer = [0; 4096];
            let count = self.reader.read(&mut buffer)?;
            if count == 0 {
                return Ok(0);
            }
            self.pending.extend_from_slice(&buffer[..count]);
        }
        let length = bytes.len().min(self.pending.len() - self.offset);
        bytes[..length].copy_from_slice(&self.pending[self.offset..self.offset + length]);
        self.offset += length;
        if self.offset == self.pending.len() {
            self.pending.clear();
            self.offset = 0;
        }
        Ok(length)
    }
}
impl Drop for PipeReader {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            drop(self.file.take());
            self.closed.store(true, Ordering::Release);
        }
        // Windows marks closed only inside the existing exact IO worker after
        // it drops its File. Dropping this facade merely requests retirement.
    }
}

pub(crate) struct PipeWriter {
    writer: transport::Writer,
    #[cfg(unix)]
    file: Option<File>,
    closed: Arc<AtomicBool>,
    failed: bool,
}
impl PipeWriter {
    pub(crate) fn new(file: File) -> io::Result<Self> {
        #[cfg(unix)]
        let writer = transport::Writer::new(file.as_raw_fd()).map_err(failure)?;
        #[cfg(windows)]
        let writer = transport::Writer::new(file).map_err(failure)?;
        #[cfg(unix)]
        let closed = Arc::new(AtomicBool::new(false));
        #[cfg(windows)]
        let closed = writer.closed();
        Ok(Self {
            writer,
            #[cfg(unix)]
            file: Some(file),
            closed,
            failed: false,
        })
    }
    pub(crate) fn closed(&self) -> Arc<AtomicBool> {
        self.closed.clone()
    }
    pub(crate) fn write(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Host write pipe has already failed",
            ));
        }
        if bytes.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Host write chunk exceeds 64KiB",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Host call deadline expired",
            ));
        }
        if let Err(error) = self.writer.write(bytes, deadline) {
            self.failed = true;
            return Err(failure(error));
        }
        Ok(())
    }
}
impl Drop for PipeWriter {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            drop(self.file.take());
            self.closed.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_runner::portable_tests::{io_fixture, pipe};
    use std::{
        io::{Read, Write},
        sync::atomic::Ordering,
        time::Duration,
    };

    fn wait_closed(closed: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !closed.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "host pipe File did not actually close"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn reader_preserves_bytes_when_a_pending_read_gets_a_smaller_buffer() {
        let _guard = io_fixture();
        let (input, mut owner) = pipe();
        let mut reader = PipeReader::new(input).unwrap();
        let closed = reader.closed();
        assert!(!closed.load(Ordering::Acquire));
        assert_eq!(
            reader.read(&mut [0; 4096]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        owner.write_all(b"abcdef").unwrap();
        let mut collected = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while collected.len() < 6 {
            let mut byte = [0];
            match reader.read(&mut byte) {
                Ok(1) => collected.push(byte[0]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(2));
                }
                result => panic!("unexpected host read result: {result:?}"),
            }
        }
        assert_eq!(collected, b"abcdef");
        assert!(!closed.load(Ordering::Acquire));
        drop(reader);
        wait_closed(&closed);
        assert_eq!(
            owner.write_all(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn pending_reader_retirement_closes_the_owned_file_while_owner_stays_open() {
        let _guard = io_fixture();
        let (input, mut owner) = pipe();
        let mut reader = PipeReader::new(input).unwrap();
        let closed = reader.closed();
        assert_eq!(
            reader.read(&mut [0; 4096]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(!closed.load(Ordering::Acquire));
        let start = Instant::now();
        drop(reader);
        assert!(start.elapsed() < Duration::from_millis(250));
        wait_closed(&closed);
        // EOF from closing the owner cannot account for this retirement.
        assert_eq!(
            owner.write_all(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn writer_closes_before_its_token_and_rejects_invalid_bounds_without_writing() {
        let _guard = io_fixture();
        let (mut reader, output) = pipe();
        let mut writer = PipeWriter::new(output).unwrap();
        let closed = writer.closed();
        assert!(!closed.load(Ordering::Acquire));
        assert_eq!(
            writer
                .write(
                    &vec![0; 64 * 1024 + 1],
                    Instant::now() + Duration::from_secs(1)
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            writer.write(b"expired", Instant::now()).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        writer
            .write(b"owned pipe", Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert!(!closed.load(Ordering::Acquire));
        drop(writer);
        wait_closed(&closed);
        let mut text = String::new();
        reader.read_to_string(&mut text).unwrap();
        assert_eq!(text, "owned pipe");

        let (input, _owner) = pipe();
        let mut reader = PipeReader::new(input).unwrap();
        assert_eq!(
            reader.read(&mut [0; 4097]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn backpressured_chunks_share_one_absolute_deadline_then_retire() {
        let _guard = io_fixture();
        let (_unread, output) = pipe();
        let mut writer = PipeWriter::new(output).unwrap();
        let closed = writer.closed();
        let start = Instant::now();
        let deadline = start + Duration::from_millis(80);
        let bytes = vec![b'x'; 64 * 1024];
        let error = (0..64)
            .find_map(|_| writer.write(&bytes, deadline).err())
            .expect("pipe backpressure must reach the shared deadline");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(1));
        drop(writer);
        wait_closed(&closed);
    }
}
