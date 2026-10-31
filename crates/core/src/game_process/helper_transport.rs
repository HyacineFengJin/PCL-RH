//! Same-binary monitor bootstrap, separate from GameChild ownership. Only the
//! three explicit stdio handles are inherited. The GUI holds a process HANDLE,
//! never the monitor's game Job; dropping a committed/reaped wrapper never kills
//! the monitor. Deadline/cancellation and before-commit cleanup belong to caller.
use super::platform::{Attributes, Handle};
use std::{
    fs::File,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::*,
    System::{Pipes::CreatePipe, Threading::*},
};
type Result<T> = std::result::Result<T, String>;
fn error() -> String {
    format!("无法建立独立游戏监控传输：{}", io::Error::last_os_error())
}
fn pipe() -> Result<(File, File)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: 0,
    };
    let (mut read, mut write) = (ptr::null_mut(), ptr::null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 8192) } == 0 {
        return Err(error());
    }
    // CreatePipe returned two fresh owned synchronous handles. The parent
    // sides stay non-inheritable; only the three selected child sides are set.
    Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
}
fn inherit(file: &File) -> Result<()> {
    if unsafe {
        SetHandleInformation(
            file.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    } == 0
    {
        Err(error())
    } else {
        Ok(())
    }
}
pub struct OwnedHelper {
    process: OwnedHandle,
    pid: u32,
}
impl OwnedHelper {
    pub fn id(&self) -> u32 {
        self.pid
    }
    pub fn try_wait(&mut self) -> Result<Option<()>> {
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(Some(())),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(error()),
        }
    }
    pub fn wait(&mut self) -> Result<()> {
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), INFINITE) } == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(error())
        }
    }
    /// Only the unreleased process object returned by this spawn is stopped;
    /// no PID lookup or unrelated process can enter this path. Waiting is bounded.
    pub fn kill_owned(&mut self) {
        if self.try_wait().ok().flatten().is_none() {
            unsafe {
                TerminateProcess(self.process.as_raw_handle(), 2);
            }
        }
        let _ = unsafe {
            WaitForSingleObject(
                self.process.as_raw_handle(),
                Duration::from_millis(500).as_millis() as u32,
            )
        };
    }
}
pub struct Pipes {
    pub child: OwnedHelper,
    pub input: File,
    pub output: File,
}
/// Execute one exact executable with one fixed helper flag and synchronous
/// anonymous stdin/stdout. No shell/argv source data, Job, game cwd or ambient
/// handle inheritance is introduced by this bootstrap.
pub fn spawn(executable: &Path) -> Result<Pipes> {
    if !executable.is_absolute() {
        return Err("游戏监控映像必须是绝对路径".into());
    }
    let mut image = executable.as_os_str().encode_wide().collect::<Vec<_>>();
    if image.is_empty()
        || image.contains(&0)
        || image.contains(&(b'"' as u16))
        || image.len() > 30000
    {
        return Err("游戏监控映像无效".into());
    }
    let mut command = vec![b'"' as u16];
    command.extend(&image);
    command.extend("\" --game-monitor".encode_utf16());
    command.push(0);
    image.push(0);
    let (child_input, parent_input) = pipe()?;
    let (parent_output, child_output) = pipe()?;
    let nul = [b'N' as u16, b'U' as u16, b'L' as u16, 0];
    let stderr = Handle::take(
        unsafe {
            CreateFileW(
                nul.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        },
        "无法关闭游戏监控错误输出",
    )?;
    inherit(&child_input)?;
    inherit(&child_output)?;
    if unsafe { SetHandleInformation(stderr.0, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
        return Err(error());
    }
    let mut attributes = Attributes::new([
        child_input.as_raw_handle(),
        child_output.as_raw_handle(),
        stderr.0,
    ])?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = child_input.as_raw_handle();
    startup.StartupInfo.hStdOutput = child_output.as_raw_handle();
    startup.StartupInfo.hStdError = stderr.0;
    startup.lpAttributeList = attributes.pointer();
    let mut info = PROCESS_INFORMATION::default();
    if unsafe {
        CreateProcessW(
            image.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_SUSPENDED | DETACHED_PROCESS | EXTENDED_STARTUPINFO_PRESENT,
            ptr::null(),
            ptr::null(),
            &startup.StartupInfo,
            &mut info,
        )
    } == 0
    {
        return Err(error());
    }
    let mut child = OwnedHelper {
        process: unsafe { OwnedHandle::from_raw_handle(info.hProcess) },
        pid: info.dwProcessId,
    };
    let thread = Handle(info.hThread);
    drop(attributes);
    drop(child_input);
    drop(child_output);
    drop(stderr);
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        child.kill_owned();
        return Err(error());
    }
    Ok(Pipes {
        child,
        input: parent_input,
        output: parent_output,
    })
}
