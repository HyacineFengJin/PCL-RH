//! The only reaper of the group leader is this owner. WNOWAIT reserves its PID
//! until group cleanup, so no new process group can reuse the target PGID.
use std::{
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
};
pub(super) struct Owner {
    child: Child,
    result: Option<ExitStatus>,
}
pub(super) struct Output(File);
impl Read for Output {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}
fn output<T: std::os::fd::IntoRawFd>(pipe: T) -> io::Result<Output> {
    let file = unsafe { File::from_raw_fd(pipe.into_raw_fd()) };
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Output(file))
}
// All heap work and environment reads happen before fork. execve, fchdir and
// the other child-side syscalls below are async-signal-safe; there is no shell
// fallback for a plain-text executable and no formatting in pre_exec.
struct Image {
    program: CString,
    _arguments: Vec<CString>,
    _environment: Vec<CString>,
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
}
// Every pointer addresses an immutable CString owned by this image. Moving the
// image cannot move the allocation. pre_exec borrows it only until exec/failure.
unsafe impl Send for Image {}
unsafe impl Sync for Image {}
impl Image {
    fn new(program: &Path, args: &[OsString]) -> Result<Self, String> {
        if !program.is_absolute() || args.len() > 4096 {
            return Err("游戏启动映像或参数无效".into());
        }
        let program =
            CString::new(program.as_os_str().as_bytes()).map_err(|_| "游戏启动映像无效")?;
        let mut arguments = vec![program.clone()];
        let mut size = program.as_bytes().len();
        for argument in args {
            size = size
                .checked_add(argument.as_bytes().len())
                .ok_or("游戏参数过大")?;
            if size > 2 * 1024 * 1024 {
                return Err("游戏参数过大".into());
            }
            arguments.push(CString::new(argument.as_bytes()).map_err(|_| "游戏参数无效")?);
        }
        let mut environment = Vec::new();
        let mut size = 0usize;
        for (key, value) in std::env::vars_os() {
            let mut entry = key.as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            size = size.checked_add(entry.len() + 1).ok_or("游戏环境过大")?;
            if size > 2 * 1024 * 1024 {
                return Err("游戏环境过大".into());
            }
            environment.push(CString::new(entry).map_err(|_| "游戏环境无效")?);
        }
        let mut argv = arguments
            .iter()
            .map(|entry| entry.as_ptr())
            .collect::<Vec<_>>();
        let mut envp = environment
            .iter()
            .map(|entry| entry.as_ptr())
            .collect::<Vec<_>>();
        argv.push(std::ptr::null());
        envp.push(std::ptr::null());
        Ok(Self {
            program,
            _arguments: arguments,
            _environment: environment,
            argv,
            envp,
        })
    }
    fn execute(&self) -> io::Result<()> {
        unsafe {
            libc::execve(
                self.program.as_ptr(),
                self.argv.as_ptr(),
                self.envp.as_ptr(),
            );
        }
        Err(io::Error::last_os_error())
    }
}
pub(super) fn spawn(program: &Path, args: &[OsString], cwd: &Path) -> Result<Owner, String> {
    if !cwd.is_absolute() {
        return Err("游戏工作目录无效".into());
    }
    let directory = File::open(cwd).map_err(|_| "无法打开游戏工作目录")?;
    spawn_in_directory(program, args, &directory)
}
pub(super) fn spawn_in_directory(
    program: &Path,
    args: &[OsString],
    cwd: &File,
) -> Result<Owner, String> {
    if !cwd.metadata().map_err(|_| "无法检查游戏工作目录")?.is_dir() {
        return Err("游戏工作目录不是目录".into());
    }
    let image = Image::new(program, args)?;
    let directory = cwd.as_raw_fd();
    #[cfg(target_os = "linux")]
    let supervisor = std::process::id();
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(directory) < 0 {
                return Err(io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            {
                // PDEATHSIG follows the spawning thread on Linux. The detached
                // monitor spawns on its main thread and keeps that thread alive
                // until wait/cleanup; spawning on a short-lived worker would
                // violate this ownership contract.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != supervisor as i32 {
                    return Err(io::Error::from_raw_os_error(libc::ECANCELED));
                }
            }
            image.execute()
        });
    }
    Ok(Owner {
        child: command.spawn().map_err(|_| "无法执行游戏启动映像")?,
        result: None,
    })
}
impl Owner {
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn take_stdout(&mut self) -> Option<Output> {
        self.child.stdout.take().and_then(|pipe| output(pipe).ok())
    }
    pub fn take_stderr(&mut self) -> Option<Output> {
        self.child.stderr.take().and_then(|pipe| output(pipe).ok())
    }
    pub fn wait(&mut self) -> Result<ExitStatus, String> {
        if let Some(result) = self.result {
            return Ok(result);
        }
        loop {
            let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.id(),
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if result == 0 {
                break;
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err("游戏监控无法确认进程结束".into());
            }
        }
        unsafe {
            libc::kill(-(self.id() as i32), libc::SIGKILL);
        }
        let result = self.child.wait().map_err(|_| "游戏监控无法确认进程结束")?;
        self.result = Some(result);
        Ok(result)
    }
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, String> {
        if let Some(result) = self.result {
            return Ok(Some(result));
        }
        loop {
            let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.id(),
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            if result == 0 {
                // macOS leaves siginfo untouched for a still-running child;
                // initializing it to zero makes both native contracts safe.
                return if unsafe { information.assume_init().si_pid() } == 0 {
                    Ok(None)
                } else {
                    self.wait().map(Some)
                };
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err("游戏监控无法确认进程状态".into());
            }
        }
    }
    pub fn terminate(&mut self) -> Result<(), String> {
        if self.result.is_some() {
            return Ok(());
        }
        // Never call std::Child::try_wait: it would reap before group cleanup.
        let group = unsafe { libc::kill(-(self.id() as i32), libc::SIGKILL) };
        let group_error = io::Error::last_os_error();
        let root = self.child.kill();
        if group < 0 && group_error.raw_os_error() != Some(libc::ESRCH) && root.is_err() {
            return Err("未能停止拥有的游戏进程".into());
        }
        self.wait().map(|_| ())
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}
