//! Unix keeps nonblocking descriptor IO and the existing wire deadline.
use super::super::Failure;
use std::{io, os::fd::RawFd, time::Instant};
pub(in crate::code_runner) fn nonblocking(fd: RawFd) -> Result<(), Failure> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(Failure::io(
            "Cannot configure build pipe",
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

pub(super) struct Reader(RawFd);
impl Reader {
    pub(super) fn new(fd: RawFd) -> Result<Self, Failure> {
        nonblocking(fd)?;
        Ok(Self(fd))
    }
    pub(super) fn stdin() -> Result<Self, Failure> {
        Self::new(libc::STDIN_FILENO)
    }
    pub(super) fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = unsafe { libc::read(self.0, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(count as usize)
        }
    }
}
pub(super) struct Writer(RawFd);
impl Writer {
    pub(super) fn new(fd: RawFd) -> Result<Self, Failure> {
        nonblocking(fd)?;
        Ok(Self(fd))
    }
    pub(super) fn stdout() -> Result<Self, Failure> {
        Self::new(libc::STDOUT_FILENO)
    }
    pub(super) fn write(&self, bytes: &[u8], deadline: Instant) -> Result<(), Failure> {
        let mut written = 0;
        while written < bytes.len() {
            if Instant::now() >= deadline {
                return Err(Failure::new(
                    "BUILD_EVENT_TIMEOUT",
                    "Build event owner stopped reading",
                ));
            }
            let count = unsafe {
                libc::write(
                    self.0,
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
                fd: self.0,
                events: libc::POLLOUT,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut poll, 1, 10);
            }
        }
        Ok(())
    }
}
