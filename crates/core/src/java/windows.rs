//! Probe only the selected image with fixed JVM arguments. A suspended start
//! closes the race between process creation and job assignment. The private job
//! owns descendants before any application code runs and kills on owner loss;
//! this is lifecycle management, not a sandbox for arbitrary executable code.
use super::*;
use std::{io, mem::size_of, os::windows::ffi::OsStrExt, ptr};
use windows_sys::Win32::{
    Foundation::*,
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::*,
    System::{Environment::*, JobObjects::*, Pipes::*, Threading::*},
};

const CLEANUP_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_ENV_UNITS: usize = 1024 * 1024;
const STOP_CODE: u32 = 1;

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
    // Never include command lines, environment contents or JVM output.
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
        return Err(win_error("无法建立 Java 检测输出管道"));
    }
    let pipe = Pipe {
        read: Handle(read),
        write: Handle(write),
    };
    if unsafe { SetHandleInformation(pipe.read.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(win_error("无法限制 Java 检测句柄继承"));
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
            return Err("Java 检测句柄属性大小无效".into());
        }
        let mut storage = vec![AttributeBlock([0; 16]); bytes.div_ceil(16)];
        let pointer = storage.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(pointer, 1, 0, &mut bytes) } == 0 {
            return Err(win_error("无法初始化 Java 检测句柄属性"));
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
            return Err(win_error("无法限制 Java 检测继承句柄"));
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

fn jvm_option(entry: &[u16]) -> bool {
    let end = entry
        .iter()
        .position(|unit| *unit == b'=' as u16)
        .unwrap_or(entry.len());
    ["JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS"]
        .iter()
        .any(|name| {
            end == name.len()
                && entry[..end]
                    .iter()
                    .zip(name.bytes())
                    .all(|(unit, byte)| *unit <= 127 && (*unit as u8).eq_ignore_ascii_case(&byte))
        })
}
fn environment() -> Result<Vec<u16>> {
    struct Environment(*mut u16);
    impl Drop for Environment {
        fn drop(&mut self) {
            unsafe {
                FreeEnvironmentStringsW(self.0);
            }
        }
    }
    let raw = unsafe { GetEnvironmentStringsW() };
    if raw.is_null() {
        return Err(win_error("无法读取 Java 检测环境"));
    }
    let source = Environment(raw);
    let (mut offset, mut output) = (0, Vec::new());
    // Preserve the OS block's ordering, Unicode and hidden drive entries. Only
    // the three JVM injection variables are filtered, case-insensitively.
    while offset < MAX_ENV_UNITS {
        let start = offset;
        while offset < MAX_ENV_UNITS && unsafe { *source.0.add(offset) } != 0 {
            offset += 1;
        }
        if offset == MAX_ENV_UNITS {
            break;
        }
        if offset == start {
            if output.is_empty() {
                output.push(0);
            }
            output.push(0);
            return Ok(output);
        }
        let entry = unsafe { std::slice::from_raw_parts(source.0.add(start), offset - start) };
        if !jvm_option(entry) {
            output.extend_from_slice(entry);
            output.push(0);
        }
        offset += 1;
    }
    Err("Java 检测环境超过大小上限".into())
}

struct ChildOwner {
    job: Handle,
    process: Handle,
    thread: Handle,
    assigned: bool,
    cleanup_attempted: bool,
}
impl ChildOwner {
    fn exit_code(&self) -> Result<Option<u32>> {
        match unsafe { WaitForSingleObject(self.process.0, 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(self.process.0, &mut code) } == 0 {
                    return Err(win_error("无法读取 Java 检测退出状态"));
                }
                Ok(Some(code))
            }
            _ => Err(win_error("无法等待 Java 检测进程")),
        }
    }
    fn terminate(&self) -> Result<()> {
        if self.assigned {
            if unsafe { TerminateJobObject(self.job.0, STOP_CODE) } == 0 {
                return Err(win_error("无法结束 Java 检测进程组"));
            }
        } else if self.exit_code()?.is_none()
            && unsafe { TerminateProcess(self.process.0, STOP_CODE) } == 0
        {
            return Err(win_error("无法结束挂起的 Java 检测进程"));
        }
        Ok(())
    }
    fn cleanup(&mut self) -> Result<()> {
        self.cleanup_attempted = true;
        self.terminate()?;
        let until = Instant::now() + CLEANUP_TIMEOUT;
        loop {
            let root_exited = self.exit_code()?.is_some();
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if self.assigned
                && unsafe {
                    QueryInformationJobObject(
                        self.job.0,
                        JobObjectBasicAccountingInformation,
                        (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                        ptr::null_mut(),
                    )
                } == 0
            {
                return Err(win_error("无法检查 Java 检测子进程清理状态"));
            }
            if root_exited && accounting.ActiveProcesses == 0 {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err("Java 检测进程未能在清理时限内退出".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.cleanup();
        }
        // Closing the non-inherited job is the final kill-on-close backstop,
        // including owner crashes and a kernel-delayed termination. Handles
        // always close; a cleanup timeout is an error, never a successful probe.
    }
}

fn spawn(path: &Path, deadline: Instant) -> Result<(ChildOwner, Handle, Handle)> {
    let mut image: Vec<_> = path.as_os_str().encode_wide().collect();
    if image.is_empty()
        || image.contains(&0)
        || image.contains(&(b'"' as u16))
        || image.len() > 32_000
    {
        return Err("Java 可执行文件路径无效或过长".into());
    }
    let mut command = vec![b'"' as u16];
    command.extend_from_slice(&image);
    command.extend("\" -XshowSettings:properties -version\0".encode_utf16());
    image.push(0);
    let environment = environment()?;
    let job = Handle::take(
        unsafe { CreateJobObjectW(ptr::null(), ptr::null()) },
        "无法建立 Java 检测进程组",
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
        return Err(win_error("无法设置 Java 检测进程组清理"));
    }
    let Pipe {
        read: out,
        write: out_writer,
    } = pipe()?;
    let Pipe {
        read: err,
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
        "无法关闭 Java 检测输入",
    )?;
    let mut attributes = Attributes::new([input.0, out_writer.0, err_writer.0])?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = input.0;
    startup.StartupInfo.hStdOutput = out_writer.0;
    startup.StartupInfo.hStdError = err_writer.0;
    startup.lpAttributeList = attributes.pointer();
    if Instant::now() >= deadline {
        return Err("Java 检测超时".into());
    }
    let mut information = PROCESS_INFORMATION::default();
    // An explicit application name never invokes cmd.exe or a batch fallback.
    // The only inherited handles are NUL/stdout/stderr, not launcher services.
    if unsafe {
        CreateProcessW(
            image.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_ptr().cast(),
            ptr::null(),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(win_error("无法执行 Java"));
    }
    let mut child = ChildOwner {
        job,
        process: Handle(information.hProcess),
        thread: Handle(information.hThread),
        assigned: false,
        cleanup_attempted: false,
    };
    drop(attributes);
    drop(input);
    drop(out_writer);
    drop(err_writer);
    // On assignment failure the still-suspended image is terminated by Drop;
    // there is deliberately no unowned, unbounded fallback launch.
    if unsafe { AssignProcessToJobObject(child.job.0, child.process.0) } == 0 {
        return Err(win_error("无法绑定 Java 检测进程组"));
    }
    child.assigned = true;
    if Instant::now() >= deadline {
        return Err("Java 检测超时".into());
    }
    if unsafe { ResumeThread(child.thread.0) } == u32::MAX {
        return Err(win_error("无法运行 Java 检测进程"));
    }
    Ok((child, out, err))
}

fn drain(pipe: &Handle, output: &mut Vec<u8>, ended: &mut bool) -> Result<()> {
    if *ended {
        return Ok(());
    }
    loop {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                pipe.0,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut available,
                ptr::null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(),Some(code) if code==ERROR_BROKEN_PIPE as i32 || code==ERROR_PIPE_NOT_CONNECTED as i32)
            {
                *ended = true;
                return Ok(());
            }
            return Err(format!("Java 检测输出读取失败：{error}"));
        }
        if available == 0 {
            return Ok(());
        }
        let count = available.min(4096);
        if output.len() + count as usize > OUTPUT_LIMIT {
            return Err("Java 检测输出超过 64 KiB 限制".into());
        }
        let mut bytes = [0; 4096];
        let mut read = 0;
        if unsafe {
            ReadFile(
                pipe.0,
                bytes.as_mut_ptr(),
                count,
                &mut read,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(win_error("Java 检测输出读取失败"));
        }
        if read == 0 {
            *ended = true;
            return Ok(());
        }
        output.extend_from_slice(&bytes[..read as usize]);
    }
}
pub(super) fn probe(path: &Path, deadline: Instant) -> Result<String> {
    if Instant::now() >= deadline {
        return Err("Java 检测超时".into());
    }
    let (mut child, out, err) = spawn(path, deadline)?;
    let result = (|| {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let (mut out_end, mut err_end) = (false, false);
        let mut status = None;
        let mut drain_deadline = deadline;
        loop {
            drain(&out, &mut stdout, &mut out_end)?;
            drain(&err, &mut stderr, &mut err_end)?;
            if status.is_none() {
                status = child.exit_code()?;
                if status.is_some() {
                    child.terminate()?;
                    drain_deadline = deadline.min(Instant::now() + Duration::from_millis(100));
                }
            }
            if let Some(code) = status {
                if out_end && err_end {
                    if code != 0 {
                        return Err(format!("Java 检测进程异常退出：{code}"));
                    }
                    return Ok(format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&stdout),
                        String::from_utf8_lossy(&stderr)
                    ));
                }
            }
            if Instant::now() >= drain_deadline {
                return Err(if status.is_some() {
                    "Java 检测进程退出后输出管道未关闭"
                } else {
                    "Java 检测超时（最长 2 秒）"
                }
                .into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    match (result, child.cleanup()) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(format!("{error}；{cleanup}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_three_jvm_injection_names_are_filtered() {
        for entry in [
            "JAVA_TOOL_OPTIONS=secret",
            "java_tool_options=secret",
            "_Java_Options=secret",
            "Jdk_Java_Options=secret",
        ] {
            assert!(jvm_option(&entry.encode_utf16().collect::<Vec<_>>()));
        }
        for entry in [
            "JAVA_HOME=retained",
            "=C:=C:\\retained",
            "JAVA_TOOL_OPTIONS_EXTRA=retained",
            "OTHER=JAVA_TOOL_OPTIONS",
        ] {
            assert!(!jvm_option(&entry.encode_utf16().collect::<Vec<_>>()));
        }
    }
}
