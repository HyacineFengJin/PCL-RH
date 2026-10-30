//! Opt-in Windows cwd inheritance diagnostics, not a production spawn API.
//! Only dedicated same-binary worker processes change cwd. All evidence and
//! mutations stay in retained private fixtures; normal lifecycle is unchanged.

#[path = "../../src/file_ops/junction_fixture.rs"]
mod junction_fixture;

use pcl_core::retained_fs::{self, Directory, Identity, Kind, Leaf, Links};
use serde_json::{json, Value};
use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, Read},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
            InitializeProcThreadAttributeList, ResumeThread, TerminateProcess,
            UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
            EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        },
    },
};

fn dot() -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(".")
}
fn save(path: &Path, value: Value) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    fs::rename(temporary, path)
}
fn load(path: &Path) -> io::Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn new_job() -> io::Result<OwnedHandle> {
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(owned)
}
fn job(child: &Child) -> io::Result<OwnedHandle> {
    let owned = new_job()?;
    if unsafe { AssignProcessToJobObject(owned.as_raw_handle(), child.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(owned)
}

// Match the production process's three explicit standard-handle entries and
// attribute lifetime/alignment. NUL replaces output transport here because CWD
// diagnostics use absolute fixture files, not the production output protocol.
#[repr(C, align(16))]
#[derive(Clone)]
struct AttributeBlock([u8; 16]);
struct Attributes {
    storage: Vec<AttributeBlock>,
    handles: Box<[HANDLE; 3]>,
}
impl Attributes {
    fn new(handles: [HANDLE; 3]) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut bytes);
        }
        if bytes == 0 || bytes > 64 * 1024 {
            return Err(io::Error::other("invalid attribute-list size"));
        }
        let mut storage = vec![AttributeBlock([0; 16]); bytes.div_ceil(16)];
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 1, 0, &mut bytes)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = Self {
            storage,
            handles: Box::new(handles),
        };
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                attributes.handles.as_ptr().cast(),
                std::mem::size_of::<[HANDLE; 3]>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
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
fn nul(access: u32) -> io::Result<OwnedHandle> {
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let name = [b'N' as u16, b'U' as u16, b'L' as u16, 0];
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}
fn quote(output: &mut Vec<u16>, text: &OsStr) {
    output.push(b'"' as u16);
    let mut slashes = 0;
    for unit in text.encode_wide() {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        if unit == b'"' as u16 {
            output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
        } else {
            output.extend(std::iter::repeat_n(b'\\' as u16, slashes));
        }
        output.push(unit);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
}
struct NativeChild {
    process: OwnedHandle,
    thread: OwnedHandle,
    job: OwnedHandle,
    assigned: bool,
}
impl Drop for NativeChild {
    fn drop(&mut self) {
        // This guard owns exactly this diagnostic child/job, never a bare PID.
        unsafe {
            if self.assigned {
                TerminateJobObject(self.job.as_raw_handle(), 2);
            } else {
                TerminateProcess(self.process.as_raw_handle(), 2);
            }
            WaitForSingleObject(self.process.as_raw_handle(), 1000);
        }
    }
}
fn spawn_restricted(control: &Path) -> io::Result<Option<i32>> {
    let exe = env::current_exe()?;
    let mut image: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut command = Vec::new();
    for argument in [
        exe.as_os_str(),
        OsStr::new("--cwd-probe-child"),
        control.as_os_str(),
    ] {
        if !command.is_empty() {
            command.push(b' ' as u16);
        }
        quote(&mut command, argument);
    }
    if command.len() >= 32767 || image[..image.len() - 1].contains(&0) {
        return Err(io::Error::other("invalid diagnostic command line"));
    }
    command.push(0);
    let job = new_job()?;
    let input = nul(GENERIC_READ)?;
    let output = nul(GENERIC_WRITE)?;
    let error = nul(GENERIC_WRITE)?;
    let mut attributes = Attributes::new([
        input.as_raw_handle(),
        output.as_raw_handle(),
        error.as_raw_handle(),
    ])?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = input.as_raw_handle();
    startup.StartupInfo.hStdOutput = output.as_raw_handle();
    startup.StartupInfo.hStdError = error.as_raw_handle();
    startup.lpAttributeList = attributes.pointer();
    let mut information = PROCESS_INFORMATION::default();
    if unsafe {
        CreateProcessW(
            image.as_mut_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null(),
            std::ptr::null(),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut child = NativeChild {
        process: unsafe { OwnedHandle::from_raw_handle(information.hProcess) },
        thread: unsafe { OwnedHandle::from_raw_handle(information.hThread) },
        job,
        assigned: false,
    };
    save(
        &control.join("native-phase.json"),
        json!({"phase":"suspended-created"}),
    )?;
    drop(attributes);
    drop(input);
    drop(output);
    drop(error);
    if unsafe { AssignProcessToJobObject(child.job.as_raw_handle(), child.process.as_raw_handle()) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    child.assigned = true;
    save(
        &control.join("native-phase.json"),
        json!({"phase":"private-job-assigned"}),
    )?;
    if unsafe { ResumeThread(child.thread.as_raw_handle()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    save(
        &control.join("native-phase.json"),
        json!({"phase":"resumed"}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = unsafe { WaitForSingleObject(child.process.as_raw_handle(), 0) };
        if status == WAIT_OBJECT_0 {
            let mut code = 0;
            if unsafe { GetExitCodeProcess(child.process.as_raw_handle(), &mut code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(Some(code as i32));
        }
        if status == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        if status != WAIT_TIMEOUT || Instant::now() >= deadline {
            return Err(io::Error::other("restricted child timeout"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}
fn wait_marker(child: &mut Child, path: &Path) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if path.is_file() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "worker exited before marker: {status}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("worker marker timeout"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}
fn wait_child(child: &mut Child) -> io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("child timeout"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

// Runs in its own same-binary process before any application-created threads.
fn worker(root: &Path, control: &Path, variant: &str) -> io::Result<()> {
    let held = Directory::open_absolute(root)?;
    let expected = held.identity()?;
    env::set_current_dir(root)?;
    let initial = retained_fs::metadata(&dot()?)?;
    if initial.identity != expected
        || initial.kind != Kind::Directory
        || initial.reparse_tag.is_some()
    {
        return Err(io::Error::other(
            "SetCurrentDirectory did not select the held literal root",
        ));
    }
    save(
        &control.join("ready.json"),
        json!({"status":"validated", "expected":expected, "dot":initial}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while !control.join("continue").is_file() {
        if Instant::now() >= deadline {
            return Err(io::Error::other("observer timeout"));
        }
        thread::sleep(Duration::from_millis(5));
    }
    // Intentionally NO second check here: mutate after the last validation to
    // investigate whether default child cwd actually preserves native binding.
    // std's Windows backend maps absent Command.current_dir to a NULL dirp.
    let exit_code = if variant == "restricted-handle-list-suspended" {
        spawn_restricted(control)?
    } else {
        let mut child = Command::new(env::current_exe()?)
            .arg("--cwd-probe-child")
            .arg(control)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_child(&mut child)?.code()
    };
    save(
        &control.join("worker.json"),
        json!({"child_exit":exit_code,
        "held_after":held.metadata()?, "child":load(&control.join("child.json"))?}),
    )
}

fn child(control: &Path, expected: Identity) -> io::Result<()> {
    let actual = retained_fs::metadata(&dot()?)?;
    let accepted = actual.identity == expected
        && actual.kind == Kind::Directory
        && actual.reparse_tag.is_none();
    if !accepted {
        // This guard protects only the diagnostic fixture. Java would already
        // have executed before an equivalent guard; this is a counterexample,
        // NOT a successful production fail-before-spawn result.
        return save(
            &control.join("child.json"),
            json!({"status":"wrong-or-reparse-child-cwd", "dot":actual}),
        );
    }
    // CREATE_NEW plus an existing same-name outside canary prevents this probe
    // from truncating outside if relative-leaf resolution differs from '.'.
    // A collision here is a probe failure, never accepted as correct binding.
    use std::io::Write;
    let write = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open("relative-marker")?;
        file.write_all(b"child relative bytes")?;
        file.sync_all()
    })();
    save(
        &control.join("child.json"),
        json!({"status":if write.is_ok(){"held-root-relative-write"}else{"relative-write-refused"},
        "dot":actual, "write_error":write.err().map(|e|e.to_string())}),
    )
}

pub(super) fn run_if_enabled() -> io::Result<()> {
    if env::var_os("PCL_RH_CWD_PROBE").is_none_or(|value| value != "1") {
        return Ok(());
    }
    let base =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/windows-inherited-cwd-2026-10-30");
    fs::create_dir_all(&base)?;
    let exe = env::current_exe()?;
    for variant in ["std-default-null-cwd", "restricted-handle-list-suspended"] {
        for case in ["baseline", "cwd-junction", "ancestor-replace"] {
            let fixture = tempfile::tempdir_in(fs::canonicalize(&base)?)?.keep();
            let tree = fixture.join("tree");
            let root = tree.join("leaf");
            let outside = fixture.join("outside");
            let outside_leaf = outside.join("leaf");
            let control = fixture.join("control");
            fs::create_dir_all(&root)?;
            fs::create_dir_all(&outside_leaf)?;
            fs::create_dir(&control)?;
            fs::write(
                outside_leaf.join("relative-marker"),
                b"outside canary must stay unchanged",
            )?;
            fs::write(outside.join("sentinel"), b"outside sentinel")?;
            let held = Directory::open_absolute(&root)?;
            let expected = held.identity()?;
            let mut process = Command::new(&exe)
                .arg("--cwd-probe-worker")
                .arg(&root)
                .arg(&control)
                .arg(variant)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            let _job = match job(&process) {
                Ok(job) => job,
                Err(error) => {
                    let _ = process.kill();
                    return Err(error);
                }
            };
            wait_marker(&mut process, &control.join("ready.json"))?;
            let ready = load(&control.join("ready.json"))?;
            if ready["status"] != "validated" {
                println!(
                    "{}",
                    json!({"diagnostic_only":true,"variant":variant,"case":case,"fixture":fixture,"classification":"rejected-before-child-spawn","initial_rejection":ready})
                );
                continue;
            }
            let mutation = match case {
                "cwd-junction" => junction_fixture::set_existing(&outside_leaf, &root),
                "ancestor-replace" => fs::rename(&tree, fixture.join("moved-tree")).map(|()| {
                    junction_fixture::create(&outside, &tree);
                }),
                _ => Ok(()),
            };
            fs::write(control.join("continue"), b"go")?;
            let status = wait_child(&mut process)?;
            let worker = load(&control.join("worker.json")).unwrap_or_else(|error| {
                json!({"status":"worker-error-after-validation", "worker_result_error":error.to_string(),
                    "worker_error":load(&control.join("worker-error.json")).ok(),
                    "native_phase":load(&control.join("native-phase.json")).ok(),
                    "child":load(&control.join("child.json")).ok()})
            });
            let restored = match case {
                "cwd-junction" if mutation.is_ok() => junction_fixture::clear_existing(&root),
                "ancestor-replace" if mutation.is_ok() => junction_fixture::clear_existing(&tree),
                _ => Ok(()),
            };
            let marker = held
                .open_regular(&Leaf::new("relative-marker")?, Links::Single)
                .map(|mut file| {
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes).map(|_| bytes)
                });
            let outside_unchanged = fs::read(outside_leaf.join("relative-marker"))?
                == b"outside canary must stay unchanged"
                && fs::read(outside.join("sentinel"))? == b"outside sentinel"
                && fs::read_dir(&outside_leaf)?.count() == 1
                && fs::read_dir(&outside)?.count() == 2;
            let bound = worker["child"]["status"] == "held-root-relative-write"
                && marker
                    .as_ref()
                    .ok()
                    .and_then(|value| value.as_ref().ok())
                    .is_some_and(|bytes| bytes == b"child relative bytes");
            let classification = if worker["status"] == "worker-error-after-validation" {
                "worker-error-after-validation"
            } else if case == "baseline" {
                if bound {
                    "baseline-observed"
                } else {
                    "baseline-failed"
                }
            } else if mutation.is_err() {
                "mutation-blocked-no-inheritance-evidence"
            } else if bound {
                "binding-observed-on-this-host"
            } else if worker["child"]["status"] == "wrong-or-reparse-child-cwd" {
                "counterexample-child-cwd"
            } else {
                "unresolved-relative-write-or-startup"
            };
            let result = json!({"diagnostic_only":true,"variant":variant,"case":case,"fixture":fixture,"expected":expected,
            "classification":classification,
            "mutation_succeeded":mutation.is_ok(),"mutation_error":mutation.err().map(|e|e.to_string()),
            "worker_exit":status.code(),"result":worker,"outside_unchanged":outside_unchanged,
            "held_root_marker":marker.ok().and_then(Result::ok),"junction_clear_error":restored.err().map(|e|e.to_string())});
            save(&control.join("result.json"), result.clone())?;
            println!("{result}");
            if !outside_unchanged {
                return Err(io::Error::other("outside changed"));
            }
        }
    }
    // Diagnostic process success does not mean the candidate was verified;
    // classify the emitted records using RESEARCH.md, especially mutation and
    // child identity. A blocked mutation is not coverage of that attack case.
    println!(
        "{}",
        json!({"diagnostic_completed":true,"production_implementation_changed":false,"pinned_cwd_claim":false})
    );
    Ok(())
}

/// Must run before the enclosing lifecycle executable dispatches any fixture.
pub(super) fn dispatch() -> Option<i32> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let worker_mode = args.first().is_some_and(|arg| arg == "--cwd-probe-worker");
    let child_mode = args.first().is_some_and(|arg| arg == "--cwd-probe-child");
    if !worker_mode && !child_mode {
        return None;
    }
    if env::var_os("PCL_RH_CWD_PROBE").is_none_or(|value| value != "1") {
        return Some(2);
    }
    let result = if worker_mode && args.len() == 4 {
        let control = PathBuf::from(&args[2]);
        let result = worker(Path::new(&args[1]), &control, &args[3].to_string_lossy());
        if let Err(error) = &result {
            // An initial rejection is written only if the validated gate was
            // never emitted. Do not relabel a later startup/IO failure as safe.
            let target = if control.join("ready.json").exists() {
                "worker-error.json"
            } else {
                "ready.json"
            };
            let _ = save(
                &control.join(target),
                json!({"status":"worker-error-or-initial-rejection", "error":error.to_string()}),
            );
        }
        result
    } else if child_mode && args.len() == 2 {
        let control = PathBuf::from(&args[1]);
        let result = (|| {
            let expected =
                serde_json::from_value(load(&control.join("ready.json"))?["expected"].clone())?;
            child(&control, expected)
        })();
        if let Err(error) = &result {
            let _ = save(
                &control.join("child.json"),
                json!({"status":"child-observation-failed", "error":error.to_string(),"native_error":error.raw_os_error()}),
            );
        }
        result
    } else {
        Err(io::Error::other("invalid cwd probe dispatch"))
    };
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("cwd probe error: {error}");
            2
        }
    })
}
