//! One owned bwrap leader reserves its PID until the namespace/group is stopped.
//! waitid(WNOWAIT) observes exit without the PID/PGID reuse race from try_wait.
use super::{
    protocol::{nonblocking, Input, Utf8},
    Failure, Outcome,
};
use std::{
    io::{self, Read},
    os::{fd::AsRawFd, unix::process::ExitStatusExt},
    process::{Child, Command},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

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

struct Owned(Option<Child>);
impl Owned {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }
    fn exited(&mut self) -> Result<bool, Failure> {
        loop {
            let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child().id(),
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(unsafe { information.assume_init().si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(Failure::io("Cannot observe build process exit", error));
            }
        }
    }
    fn stop_and_wait(&mut self) -> Result<std::process::ExitStatus, Failure> {
        let child = self.child();
        // The unreaped leader pins its process-group ID. Killing bwrap's PID
        // namespace init also stops descendants that created another session.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !self.exited()? {
            if Instant::now() >= deadline {
                let mut child = self.0.take().unwrap();
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Err(Failure::new(
                    "BUILD_CLEANUP_PENDING",
                    "Build process is killed but kernel cleanup is still pending",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.0
            .take()
            .unwrap()
            .wait()
            .map_err(|e| Failure::io("Cannot reap owned build process", e))
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.is_some() {
            let _ = self.stop_and_wait();
        }
    }
}

struct OutputBudget {
    raw_bytes: usize,
    text_bytes: usize,
    limit: usize,
}
impl OutputBudget {
    fn emit(
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
struct Stream<R> {
    reader: R,
    utf8: Utf8,
    name: &'static str,
    eof: bool,
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
    mut command: Command,
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
    let child = command
        .spawn()
        .map_err(|e| Failure::io("Cannot launch required build sandbox", e))?;
    let mut owned = Owned(Some(child));
    started(owned.child().id())?;
    let mut stdout =
        Stream::new(
            owned.child().stdout.take().ok_or_else(|| {
                Failure::new("BUILD_PIPE_UNAVAILABLE", "Build stdout is unavailable")
            })?,
            "stdout",
        )?;
    let mut stderr =
        Stream::new(
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
    let reason = loop {
        if input.cancelled()? {
            break Some("cancelled");
        }
        if Instant::now() >= deadline {
            break Some("timed_out");
        }
        if stdout.drain(&mut budget, &mut output)? || stderr.drain(&mut budget, &mut output)? {
            break Some("output_limit");
        }
        if owned.exited()? {
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
            if stdout.drain(&mut budget, &mut output)? || stderr.drain(&mut budget, &mut output)? {
                reason = Some("output_limit");
                break;
            }
            if budget.raw_bytes == before {
                break;
            }
        }
    }
    let flush_limit =
        stdout.finish(&mut budget, &mut output)? | stderr.finish(&mut budget, &mut output)?;
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
}
