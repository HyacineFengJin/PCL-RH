//! Only the detached monitor owns the Job. GUI exit never closes it. A
//! suspended image is assigned before it runs; monitor death closes the private
//! non-inherited Job and cleans only this game tree.
use super::Identity;
use std::{
    cell::UnsafeCell,
    ffi::OsString,
    io::{self, Read},
    mem::size_of,
    os::windows::{ffi::OsStrExt, process::ExitStatusExt},
    path::Path,
    process::ExitStatus,
    ptr,
    sync::{
        mpsc::{self, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{
        Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
        SECURITY_ATTRIBUTES,
    },
    Storage::FileSystem::*,
    System::{
        JobObjects::*,
        Pipes::*,
        Threading::*,
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
    },
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
const OUTPUT_BLOCK: usize = 8192;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
type Retire = SyncSender<Box<IoState>>;

struct Pipe {
    read: Output,
    write: Handle,
}
fn pipe_name() -> Result<Vec<u16>> {
    let mut random = [0u8; 16];
    if unsafe {
        BCryptGenRandom(
            ptr::null_mut(),
            random.as_mut_ptr(),
            random.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    } < 0
    {
        return Err("无法创建游戏输出管道身份".into());
    }
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(
        format!(r"\\.\pipe\pcl-rh-game-{}-{suffix}", std::process::id())
            .encode_utf16()
            .chain(Some(0))
            .collect(),
    )
}
fn pipe(retire: Retire) -> Result<Pipe> {
    let name = pipe_name()?;
    pipe_named(&name, retire)
}
fn pipe_named(name: &[u16], retire: Retire) -> Result<Pipe> {
    let descriptor = crate::retained_fs::windows_security::private_descriptor(false)
        .map_err(|_| "无法设置私有游戏输出管道权限")?;
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr().cast_mut().cast(),
        bInheritHandle: 0,
    };
    let handle = Handle::take(
        unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                OUTPUT_BLOCK as u32,
                0,
                &security,
            )
        },
        "无法建立私有游戏输出管道",
    )?;
    let event = Handle::take(
        unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) },
        "无法建立游戏输出完成事件",
    )?;
    let mut read = Output::new(handle, event, retire);
    let state = read.state.as_mut().unwrap();
    state.reset().map_err(|_| "无法准备游戏输出连接")?;
    // Connection owns the same stable OVERLAPPED as later reads. A local writer
    // is connected and its PID checked before a child can inherit any handle.
    state.pending = true;
    if unsafe { ConnectNamedPipe(state.handle.0, state.overlapped()) } != 0 {
        state.pending = false;
        return Err("游戏输出管道在建立前已连接".into());
    }
    if io::Error::last_os_error().raw_os_error() != Some(ERROR_IO_PENDING as i32) {
        state.pending = false;
        return Err("无法开始私有游戏输出连接".into());
    }
    let writer = Handle::take(
        unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                &inheritable(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        },
        "无法连接私有游戏输出写端",
    )?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match state.complete() {
            Ok(Some(_)) => break,
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("游戏输出连接确认超时".into());
                }
                if unsafe {
                    WaitForSingleObject(state.event.0, remaining.as_millis().clamp(1, 10) as u32)
                } == WAIT_FAILED
                {
                    return Err("无法确认私有游戏输出连接".into());
                }
            }
            Err(_) => return Err("无法完成私有游戏输出连接".into()),
        }
    }
    let mut client = 0;
    if unsafe { GetNamedPipeClientProcessId(state.handle.0, &mut client) } == 0
        || client != std::process::id()
    {
        return Err("游戏输出写端身份不匹配".into());
    }
    // No DisconnectNamedPipe or second Connect is exposed. The sole connected
    // client is this writer; max-instances=1 rejects every subsequent opener.
    Ok(Pipe {
        read,
        write: writer,
    })
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

// Native I/O lives in an independent shared allocation. UnsafeCell permits
// the kernel to mutate it after a call returns; Arc avoids unique-owner retags
// of that allocation when the mutable IoState control object moves/is borrowed.
// Pending operations use only raw field pointers, never &mut IoMemory/fields.
struct IoMemory {
    overlapped: OVERLAPPED,
    buffer: [u8; OUTPUT_BLOCK],
}
struct IoState {
    handle: Handle,
    event: Handle,
    native: Arc<UnsafeCell<IoMemory>>,
    pending: bool,
    start: usize,
    end: usize,
    eof: bool,
}
// One Rust owner controls the operation; the shared native allocation is never
// cloned or mutated through Arc::get_mut/make_mut. Moving control between reader
// and retirement threads preserves its native pointers. Kernel writes remain
// within UnsafeCell, and buffer bytes are borrowed only after completion.
unsafe impl Send for IoState {}
impl IoState {
    fn overlapped(&self) -> *mut OVERLAPPED {
        unsafe { ptr::addr_of_mut!((*self.native.get()).overlapped) }
    }
    fn buffer(&self) -> *mut u8 {
        unsafe { ptr::addr_of_mut!((*self.native.get()).buffer).cast() }
    }
    fn reset(&mut self) -> io::Result<()> {
        debug_assert!(!self.pending);
        if unsafe { ResetEvent(self.event.0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // Only terminal operations can reset/reuse these bytes. Even here use
        // raw writes, so no unique native reference can outlive submission.
        unsafe {
            self.overlapped().write(OVERLAPPED::default());
            ptr::addr_of_mut!((*self.overlapped()).hEvent).write(self.event.0);
        }
        Ok(())
    }
    fn complete(&mut self) -> io::Result<Option<usize>> {
        debug_assert!(self.pending);
        let mut count = 0;
        if unsafe { GetOverlappedResult(self.handle.0, self.overlapped(), &mut count, 0) } != 0 {
            self.pending = false;
            return Ok(Some(count as usize));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
            return Ok(None);
        }
        // Every other operation result is terminal, including cancellation and
        // a writer closing. CancelIoEx alone is never treated as completion.
        self.pending = false;
        Err(error)
    }
}
// Started before any pipe operation. There are at most two senders/retirements
// per spawn, and the worker runs until every sender is gone and every native
// operation is terminal. Slow kernel cancellation therefore retains at most two
// owned buffers/OVERLAPPEDs/handles here, without blocking the monitor or reader.
fn retirement() -> Result<Retire> {
    let (sender, receiver) = mpsc::sync_channel::<Box<IoState>>(2);
    let mut pending = Vec::<Box<IoState>>::with_capacity(2);
    std::thread::Builder::new()
        .name("pcl-game-io-retire".into())
        .spawn(move || {
            let mut connected = true;
            loop {
                if connected {
                    match receiver.recv_timeout(Duration::from_millis(10)) {
                        Ok(state) => pending.push(state),
                        Err(mpsc::RecvTimeoutError::Disconnected) => connected = false,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                } else if !pending.is_empty() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                pending.retain_mut(|state| matches!(state.complete(), Ok(None)));
                if !connected && pending.is_empty() {
                    break;
                }
            }
        })
        .map_err(|_| "无法建立游戏输出清理所有者")?;
    Ok(sender)
}

pub(super) struct Output {
    state: Option<Box<IoState>>,
    retire: Retire,
}
impl Output {
    fn new(handle: Handle, event: Handle, retire: Retire) -> Self {
        Self {
            state: Some(Box::new(IoState {
                handle,
                event,
                native: Arc::new(UnsafeCell::new(IoMemory {
                    overlapped: OVERLAPPED::default(),
                    buffer: [0; OUTPUT_BLOCK],
                })),
                pending: false,
                start: 0,
                end: 0,
                eof: false,
            })),
            retire,
        }
    }
}
impl Read for Output {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let state = self.state.as_mut().unwrap();
        if state.start != state.end {
            let size = buffer.len().min(state.end - state.start);
            buffer[..size].copy_from_slice(
                &unsafe { std::slice::from_raw_parts(state.buffer(), OUTPUT_BLOCK) }
                    [state.start..state.start + size],
            );
            state.start += size;
            return Ok(size);
        }
        if state.eof {
            return Ok(0);
        }
        if !state.pending {
            state.reset()?;
            state.pending = true;
            if unsafe {
                ReadFile(
                    state.handle.0,
                    state.buffer(),
                    OUTPUT_BLOCK as u32,
                    ptr::null_mut(),
                    state.overlapped(),
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    state.pending = false;
                    if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32)
                    {
                        state.eof = true;
                        return Ok(0);
                    }
                    return Err(error);
                }
            }
        }
        match state.complete() {
            Ok(None) => Err(io::ErrorKind::WouldBlock.into()),
            Ok(Some(count)) => {
                if count > OUTPUT_BLOCK {
                    return Err(io::Error::other("Invalid game pipe read length"));
                }
                state.start = 0;
                state.end = count;
                if count == 0 {
                    state.eof = true;
                    return Ok(0);
                }
                let size = buffer.len().min(count);
                buffer[..size].copy_from_slice(
                    &unsafe { std::slice::from_raw_parts(state.buffer(), OUTPUT_BLOCK) }[..size],
                );
                state.start = size;
                Ok(size)
            }
            Err(error) if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32) =>
            {
                state.eof = true;
                Ok(0)
            }
            Err(error) => Err(error),
        }
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        let Some(mut state) = self.state.take() else {
            return;
        };
        if !state.pending {
            return;
        }
        unsafe {
            CancelIoEx(state.handle.0, state.overlapped());
        }
        if !matches!(state.complete(), Ok(None)) {
            return;
        }
        if let Err(error) = self.retire.try_send(state) {
            // No sender can retire twice, and the worker cannot exit while a
            // sender exists. If that invariant ever fails, retain these owned
            // allocations rather than freeing memory the kernel may still use.
            // Each spawn can reach this safety fallback at most twice. Drop
            // must never panic, terminate the host, or print launch details.
            let (mpsc::TrySendError::Full(state) | mpsc::TrySendError::Disconnected(state)) = error;
            std::mem::forget(state);
        }
    }
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
    let retire = retirement()?;
    let Pipe {
        read: stdout,
        write: out_writer,
    } = pipe(retire.clone())?;
    let Pipe {
        read: stderr,
        write: err_writer,
    } = pipe(retire)?;
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
        stdout: Some(stdout),
        stderr: Some(stderr),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_output_has_one_client_and_no_inherited_reader() {
        let name = pipe_name().unwrap();
        let pipe = pipe_named(&name, retirement().unwrap()).unwrap();
        let state = pipe.read.state.as_ref().unwrap();
        for handle in [state.handle.0, state.event.0] {
            let mut flags = 0;
            assert_ne!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
            assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        }
        let mut flags = 0;
        assert_ne!(unsafe { GetHandleInformation(pipe.write.0, &mut flags) }, 0);
        assert_ne!(flags & HANDLE_FLAG_INHERIT, 0);

        let unexpected = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if unexpected != INVALID_HANDLE_VALUE && !unexpected.is_null() {
            drop(Handle(unexpected));
            panic!("private game output accepted a second client");
        }
        let unexpected = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                OUTPUT_BLOCK as u32,
                0,
                ptr::null(),
            )
        };
        if unexpected != INVALID_HANDLE_VALUE && !unexpected.is_null() {
            drop(Handle(unexpected));
            panic!("private game output name accepted a second server");
        }
    }
}
