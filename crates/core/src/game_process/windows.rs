//! Only the detached monitor owns the Job. GUI exit never closes it. A
//! suspended image is assigned before it runs; monitor death closes the private
//! non-inherited Job and cleans only this game tree.
use super::Identity;
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read},
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::*,
    System::{JobObjects::*, Pipes::*, Threading::*},
};
type Result<T> = std::result::Result<T, String>;
struct Handle(HANDLE);
impl Handle {
    fn take(raw: HANDLE, context: &str) -> Result<Self> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(win_error(context))
        } else {
            Ok(Self(raw))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn win_error(context: &str) -> String {
    format!("{context}：{}", io::Error::last_os_error())
}
fn inheritable() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: 1,
    }
}
struct Pipe {
    read: Handle,
    write: Handle,
}
fn pipe() -> Result<Pipe> {
    let (mut read, mut write) = (ptr::null_mut(), ptr::null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, &inheritable(), 0) } == 0 {
        return Err(win_error("无法建立游戏监控输出管道"));
    }
    let pipe = Pipe {
        read: Handle(read),
        write: Handle(write),
    };
    if unsafe { SetHandleInformation(pipe.read.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(win_error("无法限制游戏监控句柄继承"));
    }
    Ok(pipe)
}

#[repr(C, align(16))]
#[derive(Clone)]
struct AttributeBlock([u8; 16]);
struct Attributes {
    storage: Vec<AttributeBlock>,
    // UpdateProcThreadAttribute retains this pointer through CreateProcessW.
    // Heap storage is stable even if this guard is moved by its constructor.
    _handles: Box<[HANDLE; 3]>,
}
impl Attributes {
    fn new(handles: [HANDLE; 3]) -> Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut bytes);
        }
        if bytes == 0 || bytes > 64 * 1024 {
            return Err("游戏监控句柄属性大小无效".into());
        }
        let mut storage = vec![AttributeBlock([0; 16]); bytes.div_ceil(16)];
        let pointer = storage.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(pointer, 1, 0, &mut bytes) } == 0 {
            return Err(win_error("无法初始化游戏监控句柄属性"));
        }
        let mut attributes = Self {
            storage,
            _handles: Box::new(handles),
        };
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                attributes._handles.as_ptr().cast(),
                size_of::<[HANDLE; 3]>(),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(win_error("无法限制游戏监控继承句柄"));
        }
        Ok(attributes)
    }
    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.pointer());
        }
    }
}

// The native boot GUID is independent of wall-clock adjustment. Creation time
// alone could collide with a pre-reboot marker after a clock change. The fixed
// structure is deliberately local: failure rejects observation/termination.
#[repr(C)]
struct BootEnvironment {
    identifier: windows_sys::core::GUID,
    firmware_type: u32,
    flags: u64,
}
#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        buffer: *mut core::ffi::c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}
fn boot_id() -> Result<String> {
    let mut boot = std::mem::MaybeUninit::<BootEnvironment>::zeroed();
    let mut returned = 0;
    if unsafe {
        NtQuerySystemInformation(
            90,
            boot.as_mut_ptr().cast(),
            size_of::<BootEnvironment>() as u32,
            &mut returned,
        )
    } < 0
        || returned as usize != size_of::<BootEnvironment>()
    {
        return Err("无法读取系统启动身份".into());
    }
    let guid = unsafe { boot.assume_init() }.identifier;
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    ))
}
fn process_identity(process: &Handle, pid: u32) -> Result<Option<Identity>> {
    match unsafe { WaitForSingleObject(process.0, 0) } {
        WAIT_OBJECT_0 => return Ok(None),
        WAIT_TIMEOUT => {}
        _ => return Err("无法确认监控进程状态".into()),
    }
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err("无法读取监控进程身份".into());
    }
    let value = Identity {
        pid,
        start_time: ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
        boot_id: boot_id()?,
    };
    if !value.valid() {
        return Err("监控进程身份无效".into());
    }
    Ok(Some(value))
}
pub(super) fn identity(pid: u32) -> Result<Option<Identity>> {
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        return if io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32)
        {
            Ok(None)
        } else {
            Err("无法连接监控进程身份".into())
        };
    }
    process_identity(&Handle(raw), pid)
}
pub(super) fn terminate(expected: &Identity) -> Result<()> {
    // Open the actual kernel object before querying. Every later action uses
    // this same handle, even if its PID is recycled after the query.
    let process = Handle::take(
        unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                0,
                expected.pid,
            )
        },
        "无法安全连接游戏进程",
    )?;
    if process_identity(&process, expected.pid)?.as_ref() != Some(expected) {
        return Err("游戏进程身份已变化，未发送停止信号".into());
    }
    if unsafe { TerminateProcess(process.0, 1) } == 0 {
        return Err("未能安全停止游戏进程".into());
    }
    Ok(())
}

pub(super) struct Output(File);
impl Read for Output {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                self.0.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut available,
                ptr::null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            return if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32)
            {
                Ok(0)
            } else {
                Err(error)
            };
        }
        if available == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let size = buffer.len().min(available as usize);
        self.0.read(&mut buffer[..size])
    }
}
fn output(handle: Handle) -> Output {
    let raw = handle.0;
    std::mem::forget(handle);
    Output(unsafe { File::from_raw_handle(raw) })
}
pub(super) struct Owner {
    job: Handle,
    process: Handle,
    thread: Handle,
    pid: u32,
    stdout: Option<Output>,
    stderr: Option<Output>,
    assigned: bool,
    result: Option<ExitStatus>,
}
// Win32 kernel handles may be used on another thread. They are private and not
// inherited; the owner keeps every handle alive through all calls.
unsafe impl Send for Owner {}
impl Owner {
    pub fn id(&self) -> u32 {
        self.pid
    }
    pub fn take_stdout(&mut self) -> Option<Output> {
        self.stdout.take()
    }
    pub fn take_stderr(&mut self) -> Option<Output> {
        self.stderr.take()
    }
    fn cleanup(&mut self) -> Result<()> {
        if self.assigned {
            if unsafe { TerminateJobObject(self.job.0, 1) } == 0 {
                return Err("未能停止游戏进程组".into());
            }
        } else if unsafe { TerminateProcess(self.process.0, 1) } == 0 {
            return Err("未能停止未启动的游戏进程".into());
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            let process_gone = unsafe { WaitForSingleObject(self.process.0, 0) } == WAIT_OBJECT_0;
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            let job_empty = !self.assigned
                || unsafe {
                    QueryInformationJobObject(
                        self.job.0,
                        JobObjectBasicAccountingInformation,
                        (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                        ptr::null_mut(),
                    )
                } != 0
                    && accounting.ActiveProcesses == 0;
            if process_gone && job_empty {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("游戏进程组未能在清理时限内退出".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn capture_exit(&mut self) -> Result<ExitStatus> {
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.process.0, &mut code) } == 0 {
            return Err("无法读取游戏退出状态".into());
        }
        let result = ExitStatus::from_raw(code);
        self.result = Some(result);
        Ok(result)
    }
    pub fn wait(&mut self) -> Result<ExitStatus> {
        if let Some(result) = self.result {
            return Ok(result);
        }
        if unsafe { WaitForSingleObject(self.process.0, INFINITE) } != WAIT_OBJECT_0 {
            return Err("无法等待游戏退出".into());
        }
        self.cleanup()?;
        self.capture_exit()
    }
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        if let Some(result) = self.result {
            return Ok(Some(result));
        }
        match unsafe { WaitForSingleObject(self.process.0, 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => self.wait().map(Some),
            _ => Err("无法确认游戏进程状态".into()),
        }
    }
    pub fn terminate(&mut self) -> Result<()> {
        if self.result.is_some() {
            return Ok(());
        }
        self.cleanup()?;
        self.capture_exit().map(|_| ())
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn wide(value: &std::ffi::OsStr) -> Result<Vec<u16>> {
    let units = value.encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        return Err("游戏启动字符串无效".into());
    }
    Ok(units)
}
// CRT argv quoting, including runs of backslashes before quotes/end. No cmd.exe
// is involved, so metacharacters are literal. A Java main receives these argv.
fn argument(output: &mut Vec<u16>, units: &[u16]) {
    output.push(b'"' as u16);
    let mut slashes = 0;
    for &unit in units {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(
            b'\\' as u16,
            if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        output.push(unit);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
}
pub(super) fn spawn(program: &Path, args: &[OsString], cwd: &Path) -> Result<Owner> {
    if !program.is_absolute() || !cwd.is_absolute() || args.len() > 4096 {
        return Err("游戏启动映像或工作目录无效".into());
    }
    let mut image = wide(program.as_os_str())?;
    let mut directory = wide(cwd.as_os_str())?;
    if image.is_empty() || image.contains(&(b'"' as u16)) {
        return Err("游戏启动映像无效".into());
    }
    let mut command = Vec::new();
    argument(&mut command, &image);
    for arg in args {
        command.push(b' ' as u16);
        argument(&mut command, &wide(arg)?);
    }
    if command.len() >= 32767 {
        return Err("游戏启动参数过长".into());
    }
    command.push(0);
    image.push(0);
    directory.push(0);
    let job = Handle::take(
        unsafe { CreateJobObjectW(ptr::null(), ptr::null()) },
        "无法建立游戏进程组",
    )?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(win_error("无法设置独立监控清理"));
    }
    let Pipe {
        read: stdout,
        write: out_writer,
    } = pipe()?;
    let Pipe {
        read: stderr,
        write: err_writer,
    } = pipe()?;
    let nul = [b'N' as u16, b'U' as u16, b'L' as u16, 0];
    let input = Handle::take(
        unsafe {
            CreateFileW(
                nul.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &inheritable(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        },
        "无法关闭游戏输入",
    )?;
    let mut attributes = Attributes::new([input.0, out_writer.0, err_writer.0])?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = input.0;
    startup.StartupInfo.hStdOutput = out_writer.0;
    startup.StartupInfo.hStdError = err_writer.0;
    startup.lpAttributeList = attributes.pointer();
    let mut information = PROCESS_INFORMATION::default();
    if unsafe {
        CreateProcessW(
            image.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
            ptr::null(),
            directory.as_ptr(),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(win_error("无法执行游戏启动映像"));
    }
    let mut child = Owner {
        job,
        process: Handle(information.hProcess),
        thread: Handle(information.hThread),
        pid: information.dwProcessId,
        stdout: Some(output(stdout)),
        stderr: Some(output(stderr)),
        assigned: false,
        result: None,
    };
    drop(attributes);
    drop(input);
    drop(out_writer);
    drop(err_writer);
    if unsafe { AssignProcessToJobObject(child.job.0, child.process.0) } == 0 {
        return Err(win_error("无法绑定游戏进程组"));
    }
    child.assigned = true;
    if unsafe { ResumeThread(child.thread.0) } == u32::MAX {
        return Err(win_error("无法运行游戏启动映像"));
    }
    Ok(child)
}
