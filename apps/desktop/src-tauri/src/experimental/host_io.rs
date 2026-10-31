//! Bounded JSON-line RPC over the same owned pipe transport as the CLI helper.
//! A request's writes and every response chunk share one absolute deadline.
use super::host_process::Process;
use crate::code_runner::host_pipe::{PipeReader, PipeWriter};
use std::{
    fs::File,
    io,
    process::{ChildStdin, ChildStdout},
    time::{Duration, Instant},
};

const MAX_WIRE: usize = 24 * 1024 * 1024;
fn input_file(pipe: ChildStdin) -> File {
    #[cfg(unix)]
    {
        let owned: std::os::fd::OwnedFd = pipe.into();
        File::from(owned)
    }
    #[cfg(windows)]
    {
        let owned: std::os::windows::io::OwnedHandle = pipe.into();
        File::from(owned)
    }
}
fn output_file(pipe: ChildStdout) -> File {
    #[cfg(unix)]
    {
        let owned: std::os::fd::OwnedFd = pipe.into();
        File::from(owned)
    }
    #[cfg(windows)]
    {
        let owned: std::os::windows::io::OwnedHandle = pipe.into();
        File::from(owned)
    }
}
fn before(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Experimental host RPC deadline expired",
        ))
    } else {
        Ok(())
    }
}
fn pause(deadline: Instant) -> io::Result<()> {
    before(deadline)?;
    std::thread::sleep(
        Duration::from_millis(2).min(deadline.saturating_duration_since(Instant::now())),
    );
    Ok(())
}
fn reader(process: &mut Process) -> io::Result<PipeReader> {
    let output = process
        .child()
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing experimental host output pipe"))?;
    let reader = PipeReader::new(output_file(output))?;
    process.track_pipe(reader.closed());
    Ok(reader)
}
pub(super) struct Channel {
    input: PipeWriter,
    output: PipeReader,
    buffered: Vec<u8>,
    consumed: usize,
}
impl Channel {
    pub(super) fn open(process: &mut Process) -> io::Result<Self> {
        let output = reader(process)?;
        let input = process
            .child()
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("Missing experimental host input pipe"))?;
        let input = PipeWriter::new(input_file(input))?;
        process.track_pipe(input.closed());
        Ok(Self {
            input,
            output,
            buffered: Vec::new(),
            consumed: 0,
        })
    }
    pub(super) fn request(
        &mut self,
        text: &[u8],
        reply_limit: usize,
        deadline: Instant,
    ) -> io::Result<Vec<u8>> {
        if text.len() > MAX_WIRE || reply_limit > MAX_WIRE {
            return Err(io::Error::other(
                "Experimental host RPC exceeds its wire bound",
            ));
        }
        before(deadline)?;
        for chunk in text.chunks(64 * 1024) {
            self.input.write(chunk, deadline)?;
        }
        self.input.write(b"\n", deadline)?;
        let mut line = Vec::new();
        loop {
            before(deadline)?;
            if self.consumed < self.buffered.len() {
                let bytes = &self.buffered[self.consumed..];
                let count = bytes
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|n| n + 1)
                    .unwrap_or(bytes.len());
                if line
                    .len()
                    .checked_add(count)
                    .is_none_or(|size| size > reply_limit + 1)
                {
                    return Err(io::Error::other(
                        "Experimental host reply exceeds its interface bound",
                    ));
                }
                line.extend_from_slice(&bytes[..count]);
                self.consumed += count;
                if line.last() == Some(&b'\n') {
                    return Ok(line);
                }
            }
            let mut bytes = [0; 4096];
            match self.output.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Experimental host exited before replying",
                    ))
                }
                Ok(count) => {
                    self.buffered.clear();
                    self.buffered.extend_from_slice(&bytes[..count]);
                    self.consumed = 0;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => pause(deadline)?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
}
pub(super) fn version(process: &mut Process, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut output = reader(process)?;
    let mut result = Vec::new();
    loop {
        before(deadline)?;
        let mut bytes = [0; 4096];
        match output.read(&mut bytes) {
            Ok(0) => return Ok(result),
            Ok(count) => {
                if result.len() + count > 256 {
                    return Err(io::Error::other(
                        "System Node version response exceeds its bound",
                    ));
                }
                result.extend_from_slice(&bytes[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => pause(deadline)?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}
