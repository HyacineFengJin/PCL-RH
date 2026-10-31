//! Linux-only bwrap startup handshake and process retirement. A gate keeps
//! author code stopped until the real namespace init has an owned pidfd.
use super::super::{protocol::nonblocking, Failure};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    ops::{Deref, DerefMut},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    process::{Child, Command, ExitStatus},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

const INFO_BYTES: usize = 8192;
const OWNERS: usize = 16;
static OWNED: AtomicUsize = AtomicUsize::new(0);

fn pipe() -> io::Result<(File, File)> {
    let mut descriptors = [-1; 2];
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    })
}

pub(crate) struct Launch {
    command: Command,
    info: File,
    info_child: File,
    gate: File,
    gate_child: File,
}
impl Launch {
    pub(crate) fn new(mut command: Command) -> Result<Self, Failure> {
        let (info, info_child) =
            pipe().map_err(|e| Failure::io("Cannot create sandbox identity pipe", e))?;
        let (gate_child, gate) =
            pipe().map_err(|e| Failure::io("Cannot create sandbox startup gate", e))?;
        nonblocking(info.as_raw_fd())?;
        nonblocking(gate.as_raw_fd())?;
        // --userns-block-fd is incompatible with --disable-userns. The later
        // --block-fd is supported by bwrap >=0.8, before setsid/fork/author exec.
        // The reported init can fail during setup, so binding also checks its
        // held /proc identity and real parent before author execution is allowed.
        command
            .arg("--info-fd")
            .arg(info_child.as_raw_fd().to_string());
        command
            .arg("--block-fd")
            .arg(gate_child.as_raw_fd().to_string());
        Ok(Self {
            command,
            info,
            info_child,
            gate,
            gate_child,
        })
    }
    pub(crate) fn child_fds(&self) -> [RawFd; 2] {
        [self.info_child.as_raw_fd(), self.gate_child.as_raw_fd()]
    }
}
impl Deref for Launch {
    type Target = Command;
    fn deref(&self) -> &Command {
        &self.command
    }
}
impl DerefMut for Launch {
    fn deref_mut(&mut self) -> &mut Command {
        &mut self.command
    }
}

pub(crate) struct PidFd(File);
impl PidFd {
    pub(crate) fn open(pid: u32) -> io::Result<Self> {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(unsafe { File::from_raw_fd(descriptor as RawFd) }))
    }
    pub(crate) fn exited(&self) -> io::Result<bool> {
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            if unsafe { libc::poll(&mut poll, 1, 0) } >= 0 {
                if poll.revents & libc::POLLNVAL != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Invalid owned pidfd",
                    ));
                }
                return Ok(poll.revents & (libc::POLLIN | libc::POLLHUP) != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    fn kill(&self) -> io::Result<()> {
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } == 0
        {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
    fn reported_pid(&self) -> io::Result<u32> {
        let text = fs::read_to_string(format!("/proc/self/fdinfo/{}", self.0.as_raw_fd()))?;
        text.lines()
            .find_map(|line| line.strip_prefix("Pid:")?.trim().parse().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Owned pidfd no longer has a live PID",
                )
            })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) state: String,
    pub(crate) parent: u32,
    pub(crate) group: u32,
    pub(crate) session: u32,
    pub(crate) start: u64,
    pub(crate) namespace_pids: Vec<u32>,
    pub(crate) uids: Vec<u32>,
}
impl Identity {
    pub(crate) fn read(directory: &File) -> io::Result<Self> {
        fn text(directory: &File, name: &str) -> io::Result<String> {
            let mut bytes = Vec::new();
            File::open(format!("/proc/self/fd/{}/{name}", directory.as_raw_fd()))?
                .take((INFO_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > INFO_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Process identity exceeds its bound",
                ));
            }
            String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid process identity");
        let stat = text(directory, "stat")?;
        let fields: Vec<_> = stat[stat.rfind(')').ok_or_else(invalid)? + 1..]
            .split_whitespace()
            .collect();
        let status = text(directory, "status")?;
        let numbers = |prefix: &str| -> io::Result<Vec<u32>> {
            status
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
                .ok_or_else(invalid)?
                .split_whitespace()
                .map(|value| value.parse().map_err(|_| invalid()))
                .collect()
        };
        Ok(Self {
            state: fields.first().ok_or_else(invalid)?.to_string(),
            parent: fields
                .get(1)
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())?,
            group: fields
                .get(2)
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())?,
            session: fields
                .get(3)
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())?,
            start: fields
                .get(19)
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())?,
            namespace_pids: numbers("NSpid:")?,
            uids: numbers("Uid:")?,
        })
    }
    fn init_of(&self, parent: u32, pid: u32) -> bool {
        self.parent == parent
            && self.namespace_pids.first() == Some(&pid)
            && self.namespace_pids.len() >= 2
            && self.namespace_pids.last() == Some(&1)
            && !matches!(self.state.as_str(), "Z" | "X")
    }
}

struct Slot;
impl Slot {
    fn acquire() -> Result<Self, Failure> {
        let mut count = OWNED.load(Ordering::SeqCst);
        loop {
            if count >= OWNERS {
                return Err(pending());
            }
            match OWNED.compare_exchange_weak(count, count + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Ok(Self),
                Err(actual) => count = actual,
            }
        }
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        OWNED.fetch_sub(1, Ordering::SeqCst);
    }
}
fn pending() -> Failure {
    Failure::new(
        "BUILD_CLEANUP_PENDING",
        "Build process was stopped but kernel cleanup is not confirmed",
    )
}

struct Running {
    child: Option<Child>,
    leader: u32,
    status: Option<ExitStatus>,
    info: File,
    bytes: Vec<u8>,
    info_eof: bool,
    init: Option<PidFd>,
    gate: Option<File>,
    released: bool,
    signalled: bool,
    _slot: Slot,
}
pub(super) struct Owned(Option<Running>);
impl Owned {
    pub(super) fn active(&self) -> bool {
        self.0.is_some()
    }
    pub(super) fn spawn(mut launch: Launch) -> Result<Self, Failure> {
        let slot = Slot::acquire()?;
        let child = launch
            .command
            .spawn()
            .map_err(|e| Failure::io("Cannot launch required build sandbox", e))?;
        // The parent must not keep the info writer: EOF belongs to bwrap.
        drop(launch.info_child);
        drop(launch.gate_child);
        let leader = child.id();
        Ok(Self(Some(Running {
            child: Some(child),
            leader,
            status: None,
            info: launch.info,
            bytes: Vec::new(),
            info_eof: false,
            init: None,
            gate: Some(launch.gate),
            released: false,
            signalled: false,
            _slot: slot,
        })))
    }
    pub(super) fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap().child.as_mut().unwrap()
    }
    pub(super) fn exited(&mut self) -> Result<bool, Failure> {
        self.0
            .as_mut()
            .unwrap()
            .leader_exited()
            .map_err(|e| Failure::io("Cannot observe build process exit", e))
    }
    pub(super) fn bind(&mut self) -> Result<bool, Failure> {
        self.0.as_mut().unwrap().bind()
    }
    pub(super) fn release(&mut self) -> Result<(), Failure> {
        let running = self.0.as_mut().unwrap();
        if running
            .init
            .as_ref()
            .ok_or_else(|| Failure::new("BUILD_SANDBOX_IDENTITY", "Sandbox identity is not bound"))?
            .exited()
            .map_err(|e| Failure::io("Cannot inspect sandbox identity", e))?
        {
            return Err(Failure::new(
                "BUILD_SANDBOX_IDENTITY",
                "Sandbox init exited before execution",
            ));
        }
        running
            .gate
            .as_mut()
            .unwrap()
            .write_all(&[1])
            .map_err(|e| Failure::io("Cannot release sandbox startup gate", e))?;
        running.released = true;
        drop(running.gate.take());
        Ok(())
    }
    pub(super) fn stop_and_wait(&mut self) -> Result<ExitStatus, Failure> {
        let mut running = self.0.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        // Binding is attempted while the leader is still held. If startup has
        // failed, the unreleased init is still in the original owned PGID.
        let _ = running.bind();
        let kill_error = running.signal().err();
        loop {
            match running.retired() {
                Ok(Some(status)) => {
                    return kill_error.map_or(Ok(status), |e| {
                        Err(Failure::io("Cannot stop sandbox init", e))
                    })
                }
                Ok(None) => {}
                Err(_) => break,
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        retire(running);
        Err(pending())
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.is_some() {
            let _ = self.stop_and_wait();
        }
    }
}

impl Running {
    fn bind(&mut self) -> Result<bool, Failure> {
        if self.init.is_some() {
            return Ok(true);
        }
        let mut bytes = [0u8; 1024];
        // A noisy/interrupted startup pipe must return to run's one absolute
        // deadline and ownership checks instead of extending initialization.
        for _ in 0..16 {
            if self.info_eof {
                break;
            }
            match self.info.read(&mut bytes) {
                Ok(0) => self.info_eof = true,
                Ok(length) => {
                    if self.bytes.len() + length > INFO_BYTES {
                        return Err(Failure::new(
                            "BUILD_SANDBOX_IDENTITY",
                            "Sandbox identity exceeds its bound",
                        ));
                    }
                    self.bytes.extend_from_slice(&bytes[..length]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(Failure::io("Cannot read sandbox identity", error)),
            }
        }
        if !self.info_eof {
            return Ok(false);
        }
        let invalid = || {
            Failure::new(
                "BUILD_SANDBOX_IDENTITY",
                "Sandbox init identity could not be confirmed",
            )
        };
        let value: serde_json::Value =
            serde_json::from_slice(&self.bytes).map_err(|_| invalid())?;
        let pid = value
            .get("child-pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
            .ok_or_else(invalid)?;
        // Hold the proc directory across pidfd_open and both identity reads.
        // bwrap only forks one init, and has not released this startup gate;
        // a recycled PID cannot become another child of the unreaped leader.
        let directory = File::open(format!("/proc/{pid}")).map_err(|_| invalid())?;
        let before = Identity::read(&directory).map_err(|_| invalid())?;
        if !before.init_of(self.leader, pid) {
            return Err(invalid());
        }
        let init =
            PidFd::open(pid).map_err(|e| Failure::io("Cannot retain sandbox init pidfd", e))?;
        let after = Identity::read(&directory).map_err(|_| invalid())?;
        if !after.init_of(self.leader, pid)
            || before.start != after.start
            || before.namespace_pids != after.namespace_pids
            || init.reported_pid().map_err(|_| invalid())? != pid
            || init.exited().map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        self.init = Some(init);
        Ok(true)
    }
    fn leader_exited(&mut self) -> io::Result<bool> {
        if self.status.is_some() {
            return Ok(true);
        }
        loop {
            let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.leader,
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } == 0
            {
                return Ok(unsafe { information.assume_init().si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    fn signal(&mut self) -> io::Result<()> {
        if self.signalled {
            return Ok(());
        }
        self.signalled = true;
        // Only this still-unreaped Child reserves the PGID. Never signal the
        // numeric group again after reaping, nor signal a reported numeric PID.
        unsafe {
            libc::kill(-(self.leader as i32), libc::SIGKILL);
        }
        let _ = self.child.as_mut().unwrap().kill();
        // Preserve the owned leader's SIGKILL wire status: killing init first
        // could let bwrap report its child's numeric 137 exit before our kill.
        self.init.as_ref().map_or(Ok(()), PidFd::kill)
    }
    fn retired(&mut self) -> io::Result<Option<ExitStatus>> {
        if !self.leader_exited()? {
            return Ok(None);
        }
        if let Some(init) = &self.init {
            if !init.exited()? {
                return Ok(None);
            }
        } else if self.released {
            // No author may ever have run without an init pidfd.
            return Ok(None);
        }
        if self.status.is_none() {
            self.status = Some(self.child.take().unwrap().wait()?);
        }
        if self.init.is_none() {
            // Startup failed before binding. No code crossed the gate or
            // setsid, so all setup processes were in the signalled PGID.
            // This is observation only after reap; PGID reuse cannot authorize
            // another signal. A still-present/reused group fails closed.
            let result = unsafe { libc::kill(-(self.leader as i32), 0) };
            if result == 0 {
                return Ok(None);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        // Keep the gate writer until namespace retirement; dropping it earlier
        // would turn a startup error into EOF permission to execute author code.
        drop(self.gate.take());
        Ok(self.status)
    }
}

struct RetireQueue {
    owners: Mutex<Vec<Running>>,
    wake: Condvar,
}
fn retire(running: Running) {
    static QUEUE: OnceLock<Arc<RetireQueue>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let queue = Arc::new(RetireQueue {
            owners: Mutex::new(Vec::with_capacity(OWNERS)),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&queue);
        // One process-local worker, at most OWNERS reserved process trees.
        // main uses process::exit: pending remains an explicit failure, never a
        // claim that a Rust thread survives helper exit or owns an external lease.
        let _ = std::thread::Builder::new()
            .name("code-build-retire".into())
            .spawn(move || loop {
                let mut owners = worker.owners.lock().unwrap_or_else(|e| e.into_inner());
                owners.retain_mut(|owner| !matches!(owner.retired(), Ok(Some(_))));
                let interval = if owners.is_empty() {
                    Duration::from_secs(60)
                } else {
                    Duration::from_millis(10)
                };
                drop(
                    worker
                        .wake
                        .wait_timeout(owners, interval)
                        .unwrap_or_else(|e| e.into_inner()),
                );
            });
        queue
    });
    queue
        .owners
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(running);
    queue.wake.notify_one();
}
