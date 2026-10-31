//! Original nonblocking Unix pipe/poll transport and detached session spawn.
use super::FAILURE;
use std::{
    io,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::{ChildStdin, ChildStdout, Command, Stdio},
    time::Instant,
};
pub(super) type Child = std::process::Child;
pub(super) fn nonblocking(fd: i32) -> Result<(), String> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(FAILURE.into());
    }
    Ok(())
}
fn ready(fd: i32, events: i16, deadline: Instant) -> Result<(), String> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(FAILURE.into());
        }
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().min(100) as i32) };
        if result > 0 {
            return Ok(());
        }
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(FAILURE.into());
        }
    }
}
fn write_pipe(fd: i32, mut bytes: &[u8], deadline: Instant) -> Result<(), String> {
    while !bytes.is_empty() {
        ready(fd, libc::POLLOUT, deadline)?;
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &bytes[count as usize..];
        } else if count < 0
            && matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            )
        {
            continue;
        } else {
            return Err(FAILURE.into());
        }
    }
    Ok(())
}
fn read_exact(fd: i32, mut bytes: &mut [u8], deadline: Instant) -> Result<(), String> {
    while !bytes.is_empty() {
        ready(fd, libc::POLLIN, deadline)?;
        let count = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &mut bytes[count as usize..];
        } else if count < 0
            && matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            )
        {
            continue;
        } else {
            return Err(FAILURE.into());
        }
    }
    Ok(())
}
fn read_line(fd: i32, deadline: Instant) -> Result<String, String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        read_exact(fd, &mut byte, deadline)?;
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
        if bytes.len() > 128 {
            return Err(FAILURE.into());
        }
    }
    String::from_utf8(bytes).map_err(|_| FAILURE.into())
}

pub(super) struct Io {
    input: i32,
    output: i32,
    _parent: Option<(ChildStdin, ChildStdout)>,
}
impl Io {
    pub(super) fn helper() -> Result<Self, String> {
        nonblocking(0)?;
        nonblocking(1)?;
        Ok(Self {
            input: 1,
            output: 0,
            _parent: None,
        })
    }
    pub(super) fn write(&mut self, bytes: &[u8], deadline: Instant) -> Result<(), String> {
        write_pipe(self.input, bytes, deadline)
    }
    pub(super) fn read_exact(&mut self, bytes: &mut [u8], deadline: Instant) -> Result<(), String> {
        read_exact(self.output, bytes, deadline)
    }
    pub(super) fn read_line(&mut self, deadline: Instant) -> Result<String, String> {
        read_line(self.output, deadline)
    }
}
pub(super) fn spawn(executable: &Path) -> Result<(Child, Io), String> {
    let mut command = Command::new(executable);
    command
        .arg("--game-monitor")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|_| FAILURE)?;
    let setup = (|| {
        let input = child.stdin.take().ok_or(FAILURE)?;
        let output = child.stdout.take().ok_or(FAILURE)?;
        nonblocking(input.as_raw_fd())?;
        nonblocking(output.as_raw_fd())?;
        Ok(Io {
            input: input.as_raw_fd(),
            output: output.as_raw_fd(),
            _parent: Some((input, output)),
        })
    })();
    match setup {
        Ok(io) => Ok((child, io)),
        Err(error) => {
            super::super::process::kill_owned(&mut child);
            Err(error)
        }
    }
}
pub(super) fn kill_owned(child: &mut Child) {
    super::super::process::kill_owned(child)
}
