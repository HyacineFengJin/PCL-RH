//! One owned bwrap leader reserves its PID until the namespace/group is stopped.
//! waitid(WNOWAIT) observes exit without the PID/PGID reuse race from try_wait.
use super::{
    protocol::{nonblocking, Input, OutputBudget, Utf8},
    Failure, Outcome,
};
use std::{
    io::{self, Read},
    os::{fd::AsRawFd, unix::process::ExitStatusExt},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[path = "process/ownership.rs"]
mod ownership;
pub(super) use ownership::Launch;
use ownership::Owned;
#[cfg(test)]
pub(super) use ownership::{Identity, PidFd};

pub(super) const FILE_BYTES: u64 = 256 * 1024 * 1024;
// Linux counts the current UID's existing processes/threads too. A desktop can
// already own thousands; a tiny limit can prevent bubblewrap's first namespace.
pub(super) const UID_TASKS: u64 = 4096;
static PARENT_CANCELLED: AtomicBool = AtomicBool::new(false);
extern "C" fn stop_signal(_: i32) {
    PARENT_CANCELLED.store(true, Ordering::SeqCst);
}
pub(super) fn parent_cancelled() -> bool {
    PARENT_CANCELLED.load(Ordering::SeqCst)
}
fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGINT => "SIGINT".into(),
        libc::SIGXCPU => "SIGXCPU".into(),
        libc::SIGXFSZ => "SIGXFSZ".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGABRT => "SIGABRT".into(),
        libc::SIGPIPE => "SIGPIPE".into(),
        _ => signal.to_string(),
    }
}
pub(super) fn arm_parent_death() -> Result<(), Failure> {
    let parent = unsafe { libc::getppid() };
    if parent <= 1 {
        return Err(Failure::new(
            "BUILD_OWNER_CLOSED",
            "Build runner requires a live owner",
        ));
    }
    unsafe {
        // The handler only stores an atomic flag. Main performs all allocation,
        // protocol, process-tree cleanup and waiting outside signal context.
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop_signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0
            || libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) != 0
            || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0
        {
            return Err(Failure::io(
                "Cannot bind build runner lifetime",
                io::Error::last_os_error(),
            ));
        }
        if libc::getppid() != parent {
            PARENT_CANCELLED.store(true, Ordering::SeqCst);
        }
    }
    Ok(())
}

/// These inherited rlimits apply per process/file (NPROC accounts this UID).
/// They intentionally do not claim an aggregate build-tree resource guarantee.
pub(super) fn set_limits(timeout_ms: u64) -> io::Result<()> {
    for (resource, value) in [
        (libc::RLIMIT_CORE, 0),
        (libc::RLIMIT_FSIZE, FILE_BYTES),
        (libc::RLIMIT_NOFILE, 1024),
        (libc::RLIMIT_NPROC, UID_TASKS),
        (libc::RLIMIT_CPU, timeout_ms.div_ceil(1000) + 5),
    ] {
        let mut existing = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(resource, &mut existing) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let value = (value as libc::rlim_t).min(existing.rlim_max);
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

struct Stream<R> {
    reader: R,
    utf8: Utf8,
    name: &'static str,
    eof: bool,
}

#[derive(Default)]
struct StartupOutput([String; 2]);
impl StartupOutput {
    fn record(&mut self, stream: &str, text: &str) -> Result<(), Failure> {
        // OutputBudget already charges every byte before this private sink.
        // Two coalesced streams avoid per-chunk allocation growth.
        self.0[usize::from(stream == "stderr")].push_str(text);
        Ok(())
    }
    fn flush(
        &mut self,
        output: &mut impl FnMut(&str, &str) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        for (stream, text) in ["stdout", "stderr"].into_iter().zip(&mut self.0) {
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let mut end = rest.len().min(8192);
                while !rest.is_char_boundary(end) {
                    end -= 1;
                }
                output(stream, &rest[..end])?;
                rest = &rest[end..];
            }
            text.clear();
        }
        Ok(())
    }
    fn failure(&self, error: Failure) -> Failure {
        if self.0.iter().all(String::is_empty) {
            return error;
        }
        let context: String = error.message.chars().take(400).collect();
        let diagnostic: String = self.0[1]
            .chars()
            .chain(self.0[0].chars())
            .take(550)
            .collect();
        Failure::new(
            error.code,
            format!("{context}; sandbox setup: {diagnostic}"),
        )
    }
    fn finish<R: Read + AsRawFd, S: Read + AsRawFd>(
        &mut self,
        released: bool,
        stdout: &mut Stream<R>,
        stderr: &mut Stream<S>,
        budget: &mut OutputBudget,
        output: &mut impl FnMut(&str, &str) -> Result<(), Failure>,
    ) -> Result<bool, Failure> {
        if released {
            Ok(stdout.finish(budget, output)? | stderr.finish(budget, output)?)
        } else {
            let mut private = |stream: &str, text: &str| self.record(stream, text);
            Ok(stdout.finish(budget, &mut private)? | stderr.finish(budget, &mut private)?)
        }
    }
}
impl<R: Read + AsRawFd> Stream<R> {
    fn new(reader: R, name: &'static str) -> Result<Self, Failure> {
        nonblocking(reader.as_raw_fd())?;
        Ok(Self {
            reader,
            utf8: Utf8::default(),
            name,
            eof: false,
        })
    }
    fn drain(
        &mut self,
        budget: &mut OutputBudget,
        emit: &mut impl FnMut(&str, &str) -> Result<(), Failure>,
    ) -> Result<bool, Failure> {
        let mut bytes = [0u8; 8192];
        // A perpetually noisy stdout cannot starve cancellation/stderr/deadline.
        for _ in 0..4 {
            if self.eof {
                return Ok(false);
            }
            match self.reader.read(&mut bytes) {
                Ok(0) => {
                    self.eof = true;
                    return self.finish(budget, emit);
                }
                Ok(length) => {
                    let count = length.min(budget.limit.saturating_sub(budget.raw_bytes));
                    budget.raw_bytes += count;
                    let text = self.utf8.decode(&bytes[..count], false);
                    if budget.emit(self.name, &text, emit)? || count < length {
                        return Ok(true);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::io("Cannot read build output", error)),
            }
        }
        Ok(false)
    }
    fn finish(
        &mut self,
        budget: &mut OutputBudget,
        emit: &mut impl FnMut(&str, &str) -> Result<(), Failure>,
    ) -> Result<bool, Failure> {
        let text = self.utf8.decode(&[], true);
        budget.emit(self.name, &text, emit)
    }
}

pub(super) fn run(
    command: Launch,
    input: &mut Input,
    timeout: Duration,
    max_output: usize,
    mut started: impl FnMut(u32) -> Result<(), Failure>,
    mut output: impl FnMut(&str, &str) -> Result<(), Failure>,
) -> Result<Outcome, Failure> {
    if input.cancelled()? {
        return Ok(Outcome {
            status: "cancelled",
            exit_code: None,
            signal: None,
            error: None,
        });
    }
    let mut owned = Owned::spawn(command)?;
    let mut startup = StartupOutput::default();
    let result = (|| {
        let mut stdout = Stream::new(
            owned.child().stdout.take().ok_or_else(|| {
                Failure::new("BUILD_PIPE_UNAVAILABLE", "Build stdout is unavailable")
            })?,
            "stdout",
        )?;
        let mut stderr = Stream::new(
            owned.child().stderr.take().ok_or_else(|| {
                Failure::new("BUILD_PIPE_UNAVAILABLE", "Build stderr is unavailable")
            })?,
            "stderr",
        )?;
        let deadline = Instant::now() + timeout;
        let mut budget = OutputBudget {
            raw_bytes: 0,
            text_bytes: 0,
            limit: max_output,
        };
        let mut released = false;
        let reason = loop {
            if input.cancelled()? {
                break Some("cancelled");
            }
            if Instant::now() >= deadline {
                break Some("timed_out");
            }
            let exhausted = if released {
                stdout.drain(&mut budget, &mut output)? || stderr.drain(&mut budget, &mut output)?
            } else {
                let mut private = |stream: &str, text: &str| startup.record(stream, text);
                stdout.drain(&mut budget, &mut private)?
                    || stderr.drain(&mut budget, &mut private)?
            };
            if exhausted {
                break Some("output_limit");
            }
            let bound = if !released {
                match owned.bind() {
                    Ok(bound) => bound,
                    Err(error) => {
                        // EOF can arrive between the preceding drain and the
                        // bind read. Preserve bwrap's actual setup diagnostic.
                        let mut private = |stream: &str, text: &str| startup.record(stream, text);
                        stdout.drain(&mut budget, &mut private)?;
                        stderr.drain(&mut budget, &mut private)?;
                        return Err(error);
                    }
                }
            } else {
                false
            };
            if bound {
                // No started event or author execution until the actual namespace
                // init has a retained, checked pidfd. Ownership can close during IO.
                if input.cancelled()? {
                    break Some("cancelled");
                }
                if Instant::now() >= deadline {
                    break Some("timed_out");
                }
                started(owned.child().id())?;
                if input.cancelled()? {
                    break Some("cancelled");
                }
                if Instant::now() >= deadline {
                    break Some("timed_out");
                }
                owned.release()?;
                released = true;
                startup.flush(&mut output)?;
            }
            if owned.exited()? {
                if !released {
                    return Err(Failure::new(
                        "BUILD_SANDBOX_IDENTITY",
                        "Sandbox exited before its identity was bound",
                    ));
                }
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let status = owned.stop_and_wait()?;
        // A normal leader exit can leave complete output in both pipes. Drain only
        // bounded reads after namespace cleanup; copied descendant writers cannot
        // postpone completion. Forced stops discard later bytes beyond the receipt.
        let mut reason = reason;
        if reason.is_none() {
            loop {
                let before = budget.raw_bytes;
                if stdout.drain(&mut budget, &mut output)?
                    || stderr.drain(&mut budget, &mut output)?
                {
                    reason = Some("output_limit");
                    break;
                }
                if budget.raw_bytes == before {
                    break;
                }
            }
        }
        let flush_limit =
            startup.finish(released, &mut stdout, &mut stderr, &mut budget, &mut output)?;
        if flush_limit && reason.is_none() {
            reason = Some("output_limit");
        }
        Ok(Outcome {
            status: reason.unwrap_or(if status.success() {
                "completed"
            } else {
                "failed"
            }),
            exit_code: status.code(),
            signal: status.signal().map(signal_name),
            error: None,
        })
    })();
    if owned.active() {
        // Cleanup failures must remain visible on early IO/binding/callback
        // errors too. Drop alone cannot return BUILD_CLEANUP_PENDING.
        if let Err(error) = owned.stop_and_wait() {
            return Err(startup.failure(error));
        }
    }
    result.map_err(|error| startup.failure(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, io::Write, os::fd::FromRawFd};

    #[test]
    fn prestart_decoder_tail_is_private_and_shares_the_output_budget() {
        fn pipe() -> (File, File) {
            let mut descriptors = [-1; 2];
            assert_eq!(
                unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
                0
            );
            unsafe {
                (
                    File::from_raw_fd(descriptors[0]),
                    File::from_raw_fd(descriptors[1]),
                )
            }
        }
        for limit in [1, 64] {
            let (read, mut write) = pipe();
            let (empty, _held) = pipe();
            let mut stdout = Stream::new(read, "stdout").unwrap();
            let mut stderr = Stream::new(empty, "stderr").unwrap();
            let mut budget = OutputBudget {
                raw_bytes: 0,
                text_bytes: 0,
                limit,
            };
            let mut startup = StartupOutput::default();
            write.write_all(&[0xe4]).unwrap();
            // A live writer leaves this real decoder carrying half a UTF-8
            // character at cancellation, rather than finishing it on pipe EOF.
            stdout
                .drain(&mut budget, &mut |stream, text| {
                    startup.record(stream, text)
                })
                .unwrap();
            assert_eq!(budget.raw_bytes, 1);
            assert_eq!(budget.text_bytes, 0);
            let limited = startup
                .finish(false, &mut stdout, &mut stderr, &mut budget, &mut |_, _| {
                    panic!("pre-start UTF-8 tail emitted a public output frame")
                })
                .unwrap();
            assert_eq!(limited, limit == 1);
            assert!(budget.text_bytes <= limit);
            assert_eq!(startup.0[0], if limit == 1 { "" } else { "\u{fffd}" });
        }
    }
}
