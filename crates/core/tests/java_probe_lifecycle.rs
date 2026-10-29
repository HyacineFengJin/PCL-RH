//! Native executable fixtures for the public Java inspector. The test binary
//! acts as both supervisor and fake JVM, so Windows needs no batch shell or
//! installed Java. Every subprocess/file belongs to this private test tree.
use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const CASE: &str = "PCL_RH_TEST_JAVA_CASE";
const REPORT: &str = "PCL_RH_TEST_JAVA_REPORT";
const JVM_OPTIONS: [&str; 3] = ["JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS"];
const MODES: [&str; 9] = [
    "normal",
    "malformed",
    "nonzero",
    "wrong-arch",
    "stdout-flood",
    "stderr-flood",
    "hang",
    "exit-with-child",
    "hang-with-child",
];

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn pause() {
    std::thread::sleep(Duration::from_millis(5));
}
fn hang() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
fn fixture_command(image: &Path) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new(image);
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    Command::new(image)
}
fn report(name: &str) -> PathBuf {
    PathBuf::from(env::var_os(REPORT).expect("private fixture report")).join(name)
}
fn mark(name: &str) {
    fs::write(report(name), std::process::id().to_string()).unwrap();
}
fn valid_metadata(wrong_arch: bool) {
    let arch = if wrong_arch {
        if env::consts::ARCH == "aarch64" {
            "amd64"
        } else {
            "aarch64"
        }
    } else {
        env::consts::ARCH
    };
    eprintln!("java.specification.version = 21\njava.vendor = Fixture JVM\nos.arch = {arch}");
}
fn fake_jvm(args: &[std::ffi::OsString]) {
    mark("root.pid");
    assert_eq!(args, ["-XshowSettings:properties", "-version"]);
    for name in JVM_OPTIONS {
        assert!(
            env::var_os(name).is_none(),
            "JVM option variable was inherited"
        );
    }
    let mut input = [0; 1];
    assert_eq!(
        std::io::stdin().read(&mut input).unwrap(),
        0,
        "probe stdin is not closed"
    );
    let mode = env::var(CASE).unwrap();
    match mode.as_str() {
        "normal" => valid_metadata(false),
        "malformed" => eprintln!("synthetic non-Java output"),
        "nonzero" => {
            valid_metadata(false);
            std::process::exit(7);
        }
        "wrong-arch" => valid_metadata(true),
        "stdout-flood" | "stderr-flood" => {
            let stdout = std::io::stdout();
            let stderr = std::io::stderr();
            let mut output: Box<dyn Write> = if mode == "stdout-flood" {
                Box::new(stdout.lock())
            } else {
                Box::new(stderr.lock())
            };
            let bytes = [b'x'; 4096];
            while output.write_all(&bytes).is_ok() {}
        }
        "hang" => hang(),
        "exit-with-child" | "hang-with-child" | "owner-loss" => {
            // This child deliberately retains the JVM's stdout/stderr handles.
            // A detector that kills only its direct child cannot finish safely.
            let _descendant = fixture_command(&env::current_exe().unwrap())
                .arg("--descendant")
                .stdin(Stdio::null())
                .spawn()
                .unwrap();
            let until = Instant::now() + Duration::from_secs(1);
            while !report("descendant.pid").exists() {
                assert!(Instant::now() < until, "descendant did not start");
                pause();
            }
            valid_metadata(false);
            if mode != "exit-with-child" {
                hang();
            }
        }
        _ => panic!("unknown fixture mode"),
    }
}

#[cfg(target_os = "linux")]
fn running(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat.rsplit_once(") ").unwrap().1.chars().next().unwrap();
    !matches!(state, 'Z' | 'X')
}
#[cfg(all(unix, not(target_os = "linux")))]
fn running(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
#[cfg(windows)]
fn running(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_TIMEOUT},
        System::Threading::*,
    };
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return false;
    }
    let active = unsafe { WaitForSingleObject(process, 0) == WAIT_TIMEOUT };
    unsafe {
        CloseHandle(process);
    }
    active
}
fn assert_stopped(directory: &Path, name: &str) {
    let file = directory.join(name);
    let pid = fs::read_to_string(file)
        .expect("executed fixture PID")
        .parse::<u32>()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while running(pid) && Instant::now() < until {
        pause();
    }
    assert!(
        !running(pid),
        "owned fixture process survived cleanup: {name}"
    );
}
fn case(mode: &str) {
    let directory = PathBuf::from(env::var_os(REPORT).unwrap());
    let image = directory.join("java space & '$(literal)%'.exe");
    fs::copy(env::current_exe().unwrap(), &image).unwrap();
    let started = Instant::now();
    let result = pcl_core::java::inspect(&image);
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "probe exceeded its bounded runtime/cleanup"
    );
    match mode {
        "normal" | "exit-with-child" => {
            let runtime = result.unwrap();
            assert_eq!(runtime.major, 21);
            assert_eq!(runtime.vendor, "Fixture JVM");
            assert_eq!(Path::new(&runtime.path), fs::canonicalize(&image).unwrap());
            let selected = pcl_core::java::select_from_catalog(
                &pcl_core::java::JavaCatalog {
                    runtimes: vec![runtime],
                    unavailable: vec![],
                },
                21,
                true,
            )
            .unwrap();
            assert_eq!(selected, fs::canonicalize(&image).unwrap());
        }
        "malformed" => assert!(result.unwrap_err().contains("版本")),
        "nonzero" => assert!(result.unwrap_err().contains("退出")),
        "wrong-arch" => assert!(result.unwrap_err().contains("不兼容")),
        "stdout-flood" | "stderr-flood" => assert!(result.unwrap_err().contains("64 KiB")),
        "hang" | "hang-with-child" => assert!(result.unwrap_err().contains("超时")),
        "owner-loss" => panic!("owner-loss fixture was not interrupted before its deadline"),
        _ => panic!("unknown case"),
    }
    assert_stopped(&directory, "root.pid");
    if mode.ends_with("with-child") {
        assert_stopped(&directory, "descendant.pid");
    }
    assert!(!directory.join("injected").exists());
}
fn launch_case(image: &Path, mode: &str, directory: &Path) -> OwnedChild {
    let mut command = fixture_command(image);
    command
        .args(["--case", mode])
        .env(CASE, mode)
        .env(REPORT, directory)
        .stdin(Stdio::null());
    for name in JVM_OPTIONS {
        #[cfg(windows)]
        let name = match name {
            "JAVA_TOOL_OPTIONS" => "Java_Tool_Options",
            "_JAVA_OPTIONS" => "_Java_Options",
            _ => "Jdk_Java_Options",
        };
        command.env(name, "-javaagent:synthetic-forbidden-option");
    }
    OwnedChild(command.spawn().unwrap())
}
fn supervisor() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/java-probe-lifecycle-tests");
    fs::create_dir_all(&base).unwrap();
    let fixture = tempfile::tempdir_in(base).unwrap();
    let image = env::current_exe().unwrap();
    let mut bystander = OwnedChild(
        fixture_command(&image)
            .arg("--bystander")
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    for mode in MODES {
        let directory = fixture.path().join(mode);
        fs::create_dir(&directory).unwrap();
        let mut worker = launch_case(&image, mode, &directory);
        let until = Instant::now() + Duration::from_secs(12);
        let status = loop {
            if let Some(status) = worker.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < until, "case watchdog expired: {mode}");
            pause();
        };
        assert!(status.success(), "Java lifecycle case failed: {mode}");
        assert!(
            bystander.0.try_wait().unwrap().is_none(),
            "unrelated fixture was terminated"
        );
        println!("Java lifecycle: {mode} passed");
    }
    // A .cmd/text file is never routed through a shell by the probe.
    let invalid = fixture.path().join("not-java.cmd");
    fs::write(&invalid, "echo unexpected > injected\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&invalid, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Even a regression that invoked this script could only create its marker
    // in our fixture, never the repository or caller's current directory.
    let previous_directory = env::current_dir().unwrap();
    env::set_current_dir(fixture.path()).unwrap();
    let invalid_result = pcl_core::java::inspect(&invalid);
    env::set_current_dir(previous_directory).unwrap();
    assert!(invalid_result.is_err());
    assert!(!fixture.path().join("injected").exists());
    println!("Java lifecycle: shell fallback rejected");
    #[cfg(windows)]
    {
        let directory = fixture.path().join("owner-loss");
        fs::create_dir(&directory).unwrap();
        let mut worker = launch_case(&image, "owner-loss", &directory);
        let until = Instant::now() + Duration::from_secs(5);
        while !directory.join("descendant.pid").exists() {
            assert!(
                worker.0.try_wait().unwrap().is_none(),
                "fixture owner exited before its job was exercised"
            );
            assert!(Instant::now() < until, "owner-loss fixture did not start");
            pause();
        }
        worker.0.kill().unwrap();
        worker.0.wait().unwrap();
        assert_stopped(&directory, "root.pid");
        assert_stopped(&directory, "descendant.pid");
        assert!(bystander.0.try_wait().unwrap().is_none());
        println!("Java lifecycle: owner loss closes and kills the private job");
    }
}
fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--descendant") {
        mark("descendant.pid");
        hang();
    }
    if args.first().is_some_and(|arg| arg == "--bystander") {
        hang();
    }
    if args.first().is_some_and(|arg| arg == "--case") {
        case(&args[1].to_string_lossy());
        return;
    }
    if env::var_os(CASE).is_some() {
        fake_jvm(&args);
        return;
    }
    supervisor();
}
