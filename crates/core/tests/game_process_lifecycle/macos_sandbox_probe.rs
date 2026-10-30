//! Opt-in diagnostic only. Custom SBPL is unsupported for third-party use and
//! this measures existing-file write-open permission, never backend safety.
//! The parent is not sandboxed; the child never truncates or writes a canary.
use serde_json::{json, Value};
use std::{
    env,
    ffi::{CStr, CString},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const CHILD: &str = "--pcl-rh-seatbelt-write-open-child";
const ACTIVE: Duration = Duration::from_secs(6);
const TOTAL: Duration = Duration::from_secs(8);
const MAX_PROTOCOL: u64 = 64 * 1024;
const SENTINEL: &[u8] = b"outside canary must stay byte-for-byte unchanged\n";
const INSIDE: &[u8] = b"captured private root canary\n";

fn enabled() -> bool {
    env::var_os("PCL_RH_SEATBELT_PROBE").as_deref() == Some(std::ffi::OsStr::new("1"))
}
fn fail(class: &str, error: impl std::fmt::Display) -> Value {
    json!({"class":class,"detail":error.to_string()})
}
fn quoted(path: &Path) -> io::Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| io::Error::other("probe path is not UTF-8"))?;
    if text.chars().any(char::is_control) {
        return Err(io::Error::other("control character in probe path"));
    }
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
fn profile(root: &Path, filter: &str) -> io::Result<String> {
    let write = match filter {
        "subpath" => format!("(subpath {})", quoted(root)?),
        // Literal can cover this one existing canary, not a general output tree.
        "literal" => format!(
            "(literal {}) (literal {})",
            quoted(root)?,
            quoted(&root.join("canary"))?
        ),
        _ => return Err(io::Error::other("unknown probe filter")),
    };
    Ok(format!(
        "(version 1)\n(deny default)\n\
        (allow file-read-metadata)\n\
        (allow file-read-data file-map-executable\n\
          (subpath \"/System\") (subpath \"/usr/lib\") (subpath \"/usr/share\")\n\
          (literal \"/dev/null\") (literal \"/dev/random\") (literal \"/dev/urandom\"))\n\
        (allow process-exec (subpath \"/System\") (subpath \"/usr/lib\") (subpath \"/usr/bin\"))\n\
        (allow file-write* {write})\n"
    ))
}

type SandboxInit =
    unsafe extern "C" fn(*const libc::c_char, u64, *mut *mut libc::c_char) -> libc::c_int;
type SandboxFreeError = unsafe extern "C" fn(*mut libc::c_char);
struct Library(*mut libc::c_void);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            libc::dlclose(self.0);
        }
    }
}
fn loader_error() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "no dynamic-loader error text".into()
    } else {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}
fn install_profile(text: &str) -> Result<(), Value> {
    // Resolve dynamically so absent/deprecated SDK interfaces are preserved as
    // an explicit diagnostic result, rather than a successful fallback.
    let handle = unsafe {
        libc::dlopen(
            c"/usr/lib/libsandbox.dylib".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        )
    };
    if handle.is_null() {
        return Err(fail("dlopen_failed", loader_error()));
    }
    let _library = Library(handle);
    let init = unsafe { libc::dlsym(handle, c"sandbox_init".as_ptr()) };
    if init.is_null() {
        return Err(fail("sandbox_init_symbol_missing", loader_error()));
    }
    let free = unsafe { libc::dlsym(handle, c"sandbox_free_error".as_ptr()) };
    if free.is_null() {
        return Err(fail("sandbox_free_error_symbol_missing", loader_error()));
    }
    let init: SandboxInit = unsafe { std::mem::transmute(init) };
    let free: SandboxFreeError = unsafe { std::mem::transmute(free) };
    let text = CString::new(text).map_err(|error| fail("profile_encoding", error))?;
    let mut errorbuf = std::ptr::null_mut();
    let result = unsafe { init(text.as_ptr(), 0, &mut errorbuf) };
    let errno = io::Error::last_os_error().raw_os_error();
    let detail = if errorbuf.is_null() {
        None
    } else {
        let text = unsafe { CStr::from_ptr(errorbuf) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            free(errorbuf);
        }
        Some(text)
    };
    if result != 0 {
        return Err(json!({"class":"sandbox_init_failed","return_code":result,
        "errorbuf":detail,"errno_snapshot_not_api_contract":errno}));
    }
    Ok(())
}
fn write_open(path: &Path) -> Value {
    let path_c = match CString::new(path.as_os_str().as_bytes()) {
        Ok(path) => path,
        Err(error) => return json!({"allowed":false,"error":fail("invalid_path",error)}),
    };
    // No O_CREAT/O_TRUNC/O_APPEND and no write(), truncate(), or mmap(). The
    // existing regular canary is created and later verified by the parent.
    let fd = unsafe {
        libc::open(
            path_c.as_ptr(),
            libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd >= 0 {
        drop(unsafe { File::from_raw_fd(fd) });
        json!({"allowed":true,"errno":null})
    } else {
        let error = io::Error::last_os_error();
        json!({"allowed":false,"errno":error.raw_os_error(),"error_kind":format!("{:?}",error.kind()),"detail":error.to_string()})
    }
}
fn observations(root: &Path, outside: &Path) -> Value {
    json!({"relative_captured_cwd":write_open(Path::new("canary")),
        "absolute_old_root":write_open(&root.join("canary")),
        "absolute_outside":write_open(&outside.join("canary"))})
}
fn send(value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n")?;
    out.flush()
}
fn child(root: &Path, outside: &Path, filter: &str) -> io::Result<i32> {
    let root_fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    let identity = root_fd.metadata()?;
    if unsafe { libc::fchdir(root_fd.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let text = profile(root, filter)?;
    if let Err(error) = install_profile(&text) {
        send(&json!({"event":"api_error","error":error}))?;
        return Ok(2);
    }
    send(
        &json!({"event":"ready","sandbox_applied":true,"captured_identity":{"device":identity.dev(),"inode":identity.ino()},
        "baseline":observations(root,outside)}),
    )?;
    let mut go = [0];
    io::stdin().read_exact(&mut go)?;
    if go != [b'G'] {
        return Err(io::Error::other("invalid parent release"));
    }
    send(&json!({"event":"complete","after":observations(root,outside)}))?;
    // Keep the captured FD alive through both measurements; never turn its
    // pathname into new write authority or execute a Java payload here.
    drop(root_fd);
    Ok(0)
}

pub(super) fn dispatch() -> Option<i32> {
    if !enabled() {
        return None;
    }
    let args: Vec<_> = env::args_os().collect();
    if args
        .get(1)
        .is_none_or(|arg| arg.as_os_str() != std::ffi::OsStr::new(CHILD))
    {
        return None;
    }
    if args.len() != 5 {
        let _ = send(
            &json!({"event":"child_error","error":fail("invalid_arguments","expected root/outside/filter")}),
        );
        return Some(2);
    }
    let result = args[4]
        .to_str()
        .ok_or_else(|| io::Error::other("invalid filter"))
        .and_then(|filter| child(Path::new(&args[2]), Path::new(&args[3]), filter));
    Some(match result {
        Ok(code) => code,
        Err(error) => {
            let _ = send(&json!({"event":"child_error","error":fail("child_io",error)}));
            2
        }
    })
}

fn make_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}
fn canary(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
struct Owned {
    child: Child,
    reaped: bool,
    deadline: Instant,
}
impl Owned {
    fn reap(&mut self, deadline: Instant) -> io::Result<std::process::ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.reaped = true;
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "probe child deadline",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn kill_group(&mut self) {
        if !self.reaped {
            // The leader is our unreaped direct child, so its PID cannot have
            // been reused. Only this fixture's independently created group.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill_group();
            let _ = self.reap(self.deadline);
        }
    }
}
fn receive(rx: &mpsc::Receiver<Result<Value, String>>, deadline: Instant) -> io::Result<Value> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(remaining)
        .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error.to_string()))?
        .map_err(io::Error::other)
}
fn allowed(observed: &Value, key: &str) -> bool {
    observed[key]["allowed"].as_bool() == Some(true)
}

fn run_case(base: &Path, scenario: &str, filter: &str) -> io::Result<Value> {
    let started = Instant::now();
    let case = base.join(format!("{filter}-{scenario}"));
    make_dir(&case)?;
    let root = case.join("ancestor/root");
    let outside = case.join("outside");
    make_dir(&root)?;
    make_dir(&outside.join("root"))?;
    canary(&root.join("canary"), INSIDE)?;
    canary(&outside.join("canary"), SENTINEL)?;
    canary(&outside.join("root/canary"), SENTINEL)?;
    let initial = root.metadata()?;
    fs::write(case.join("profile.sb"), profile(&root, filter)?)?;
    let deadline = started + ACTIVE;
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "fixture setup exceeded active deadline",
        ));
    }
    let mut command = Command::new(env::current_exe()?);
    command
        .args([
            std::ffi::OsStr::new(CHILD),
            root.as_os_str(),
            outside.as_os_str(),
            std::ffi::OsStr::new(filter),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut owned = Owned {
        child: command.spawn()?,
        reaped: false,
        deadline: started + TOTAL,
    };
    let stdout = owned
        .child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout"))?;
    let stderr = owned
        .child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr"))?;
    let (tx, rx) = mpsc::sync_channel(4);
    let stdout_reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout.take(MAX_PROTOCOL + 1));
        for _ in 0..3 {
            let mut line = String::new();
            let result = match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => serde_json::from_str(&line)
                    .map_err(|error| format!("invalid JSON: {error}: {line}")),
                Err(error) => Err(error.to_string()),
            };
            if tx.send(result).is_err() {
                break;
            }
        }
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr.take(16 * 1024 + 1).read_to_end(&mut bytes);
        (bytes, result.err().map(|error| error.to_string()))
    });
    let mut report = json!({"scenario":scenario,"filter":filter,"root":root,"outside":outside,
        "captured_identity":{"device":initial.dev(),"inode":initial.ino()},"unsupported_custom_sbpl":true,
        "production_backend_enabled":false,"measures_existing_write_open_only":true,"unsafe":false});
    let exchange = (|| -> io::Result<()> {
        let first = receive(&rx, deadline)?;
        report["child_first"] = first.clone();
        if first["event"] != "ready" {
            return Err(io::Error::other(
                "child did not apply sandbox; see recorded error class",
            ));
        }
        let captured_matches = first["captured_identity"]["device"].as_u64() == Some(initial.dev())
            && first["captured_identity"]["inode"].as_u64() == Some(initial.ino());
        report["captured_identity_matches"] = json!(captured_matches);
        if !captured_matches {
            return Err(io::Error::other(
                "child did not retain the expected original root object",
            ));
        }
        let baseline = &first["baseline"];
        report["baseline_inside_allowed"] = json!(
            allowed(baseline, "relative_captured_cwd") && allowed(baseline, "absolute_old_root")
        );
        if allowed(baseline, "absolute_outside") {
            report["unsafe"] = json!(true);
        }
        match scenario {
            "baseline" => {}
            "root-symlink" => {
                fs::rename(&root, case.join("saved-root"))?;
                std::os::unix::fs::symlink(&outside, &root)?;
            }
            "ancestor-plain" => {
                fs::rename(case.join("ancestor"), case.join("saved-ancestor"))?;
                make_dir(&root)?;
                canary(&root.join("canary"), SENTINEL)?;
            }
            "ancestor-symlink" => {
                fs::rename(case.join("ancestor"), case.join("saved-ancestor"))?;
                std::os::unix::fs::symlink(&outside, case.join("ancestor"))?;
            }
            _ => return Err(io::Error::other("unknown retarget scenario")),
        }
        let current = root.metadata()?;
        report["old_path_identity_after"] = json!({"device":current.dev(),"inode":current.ino()});
        let is_retarget = scenario != "baseline";
        if is_retarget && (current.dev(), current.ino()) == (initial.dev(), initial.ino()) {
            return Err(io::Error::other(
                "retarget did not replace the actual old-path object",
            ));
        }
        owned
            .child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing control pipe"))?
            .write_all(b"G")?;
        let last = receive(&rx, deadline)?;
        report["child_last"] = last.clone();
        if last["event"] != "complete" {
            return Err(io::Error::other("child did not complete write-open probes"));
        }
        if allowed(&last["after"], "absolute_outside")
            || (is_retarget && allowed(&last["after"], "absolute_old_root"))
        {
            report["unsafe"] = json!(true);
        }
        Ok(())
    })();
    if let Err(error) = exchange {
        report["diagnostic_error"] = fail("protocol_or_observer", error);
    }
    let status = match owned.reap(deadline) {
        Ok(status) => Some(status),
        Err(error) => {
            report["wait_error"] = fail("active_deadline", error);
            owned.kill_group();
            match owned.reap(started + TOTAL) {
                Ok(status) => Some(status),
                Err(error) => {
                    report["cleanup_error"] = fail("group_cleanup_unconfirmed", error);
                    None
                }
            }
        }
    };
    if let Some(status) = status {
        report["exit_status"] = json!(status.to_string());
        report["exit_success"] = json!(status.success());
        let _ = stdout_reader.join();
        let (stderr, error) = stderr_reader
            .join()
            .map_err(|_| io::Error::other("stderr reader panicked"))?;
        report["stderr"] = json!(String::from_utf8_lossy(&stderr));
        if let Some(error) = error {
            report["stderr_error"] = json!(error);
        }
    }
    let direct = fs::read(outside.join("canary"))?;
    let nested = fs::read(outside.join("root/canary"))?;
    let replacement_unchanged =
        scenario != "ancestor-plain" || fs::read(root.join("canary"))? == SENTINEL;
    let unchanged = direct == SENTINEL && nested == SENTINEL && replacement_unchanged;
    let original_root = match scenario {
        "root-symlink" => case.join("saved-root"),
        "ancestor-plain" | "ancestor-symlink" => case.join("saved-ancestor/root"),
        _ => root.clone(),
    };
    let inside_unchanged = fs::read(original_root.join("canary"))? == INSIDE;
    report["captured_canary_bytes_unchanged"] = json!(inside_unchanged);
    if !inside_unchanged {
        report["diagnostic_error"] = fail(
            "unexpected_canary_mutation",
            "open-only probe changed captured canary bytes",
        );
    }
    report["outside_canary_bytes_unchanged"] = json!(unchanged);
    if !unchanged {
        report["unsafe"] = json!(true);
    }
    report["elapsed_ms"] = json!(started.elapsed().as_millis());
    fs::write(
        case.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

pub(super) fn run_if_enabled() -> io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../work/macos-seatbelt-diagnostic-2026-10-30");
    make_dir(&work)?;
    let work = fs::canonicalize(work)?;
    // Keep the entire private diagnostic fixture and profiles as evidence on
    // every outcome, including unsupported SPI, timeout and unsafe permission.
    let fixture = tempfile::Builder::new()
        .prefix("probe-")
        .tempdir_in(&work)?
        .keep();
    let mut cases = Vec::new();
    for filter in ["subpath", "literal"] {
        for scenario in [
            "baseline",
            "root-symlink",
            "ancestor-plain",
            "ancestor-symlink",
        ] {
            match run_case(&fixture,scenario,filter) {
                Ok(case)=>cases.push(case),
                Err(error)=>cases.push(json!({"scenario":scenario,"filter":filter,"diagnostic_error":fail("parent_setup_or_record",error),"unsafe":false})),
            }
        }
    }
    let unsafe_ = cases.iter().any(|case| case["unsafe"] == true);
    let inconclusive = cases.iter().any(|case| {
        case.get("diagnostic_error").is_some()
            || case.get("cleanup_error").is_some()
            || case["baseline_inside_allowed"] != true
            || case["exit_success"] != true
    });
    let outcome = if unsafe_ {
        "UNSAFE_OUTSIDE_WRITE_OPEN_ALLOWED"
    } else if inconclusive {
        "INCONCLUSIVE"
    } else {
        "NO_COUNTEREXAMPLE_IN_THIS_OPEN_ONLY_PROBE"
    };
    let report = json!({"probe":"macOS unsupported custom Seatbelt diagnostic","architecture":env::consts::ARCH,
        "production_backend_enabled":false,"profile_success_is_not_backend_safety":true,"open_only_no_canary_write":true,
        "does_not_test":["file creation","data writes","mmap","arbitrary native payload","JDK processors","all supported OS versions"],
        "fixture":fixture,"outcome":outcome,"unsafe":unsafe_,"inconclusive":inconclusive,"cases":cases});
    let path = fixture.join("summary.json");
    fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "Seatbelt diagnostic ({outcome}); retained evidence: {}",
        path.display()
    );
    if unsafe_ {
        return Err(io::Error::other(format!(
            "UNSAFE: outside write-open permission observed; evidence {}",
            path.display()
        )));
    }
    if inconclusive {
        return Err(io::Error::other(format!(
            "Seatbelt diagnostic inconclusive; evidence {}",
            path.display()
        )));
    }
    Ok(())
}
