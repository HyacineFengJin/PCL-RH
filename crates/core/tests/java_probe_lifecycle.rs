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
const MODES: &[&str] = &[
    "normal",
    "malformed",
    "nonzero",
    "wrong-arch",
    "stdout-flood",
    "stderr-flood",
    "hang",
    "exit-with-child",
    "hang-with-child",
    #[cfg(windows)]
    "win32-argv0",
    #[cfg(windows)]
    "win32-plan",
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
        #[cfg(windows)]
        "win32-argv0" | "win32-plan" => {
            // Model the OpenJDK runtime-image lookup failure, independently of
            // CreateProcessW's ability to load an extended-path PE image.
            let argv0 = env::args_os().next().unwrap();
            fs::write(report("argv0.txt"), argv0.to_str().unwrap()).unwrap();
            if argv0.to_string_lossy().starts_with(r"\\?\") {
                eprintln!("fixture rejects verbatim JVM argv[0]");
                std::process::exit(1);
            }
            valid_metadata(false);
        }
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

#[cfg(windows)]
fn windows_launch_plan(directory: &Path, image: &Path) {
    use pcl_core::java::{invocation_path, JavaSelection};
    use serde_json::json;
    let root = directory.join("游戏根 & space");
    let id = "windows_paths";
    let version = root.join("versions").join(id);
    let library = root.join("libraries/example/fixture/1.0/fixture-1.0.jar");
    let logging = root.join("assets/log_configs/fixture.xml");
    fs::create_dir_all(&version).unwrap();
    fs::create_dir_all(library.parent().unwrap()).unwrap();
    fs::create_dir_all(logging.parent().unwrap()).unwrap();
    fs::write(
        version.join(format!("{id}.jar")),
        b"never executed client fixture",
    )
    .unwrap();
    fs::write(&library, b"never executed library fixture").unwrap();
    fs::write(&logging, b"<Configuration/>").unwrap();
    let literal = "完整 中文 argument & 'quoted' with spaces";
    let metadata = json!({
        "id": id, "mainClass": "NeverExecutedFixture", "javaVersion": {"majorVersion": 21},
        "libraries": [{"name":"example:fixture:1.0","downloads":{"artifact":{
            "path":"example/fixture/1.0/fixture-1.0.jar"}}}],
        "logging":{"client":{"file":{"id":"fixture.xml"},"argument":"-Dfixture.logging=${path}"}},
        "arguments":{
            "jvm":["-Dfixture.game=${game_directory}","-Dfixture.assets=${assets_root}",
                   "-Dfixture.natives=${natives_directory}","-Dfixture.libraries=${library_directory}",
                   "-Dfixture.legacy=${game_assets}","-cp","${classpath}"],
            "game":["--gameDir","${game_directory}","--assetsDir","${assets_root}",
                    "--username","${auth_player_name}","--fixtureLiteral",literal]
        }
    });
    fs::write(version.join(format!("{id}.json")), metadata.to_string()).unwrap();
    let canonical = root.canonicalize().unwrap();
    let selected = JavaSelection::Manual {
        path: image.canonicalize().unwrap().display().to_string(),
    };
    let plan = pcl_core::build_launch_plan_with_java(
        &canonical,
        directory,
        id,
        "Fixture_Player",
        2,
        &selected,
        &[],
    )
    .unwrap();
    assert!(!plan.java.to_string_lossy().starts_with(r"\\?\"));
    assert_eq!(
        plan.java.canonicalize().unwrap(),
        image.canonicalize().unwrap()
    );
    assert!(plan.args.iter().all(|arg| !arg.contains(r"\\?\")));
    assert_eq!(plan.game_dir, canonical);
    assert!(plan.game_dir.to_string_lossy().starts_with(r"\\?\"));
    assert!(plan.log_path.starts_with(&canonical));
    assert!(plan.log_path.to_string_lossy().starts_with(r"\\?\"));
    let argument = |prefix: &str| {
        plan.args
            .iter()
            .find_map(|value| value.strip_prefix(prefix))
            .unwrap()
    };
    let java_root = invocation_path(&canonical).unwrap();
    assert_eq!(Path::new(argument("-Duser.dir=")), java_root);
    assert_eq!(
        Path::new(argument("-Duser.dir=")).canonicalize().unwrap(),
        plan.game_dir
    );
    assert_eq!(Path::new(argument("-Dfixture.game=")), java_root);
    assert_eq!(
        Path::new(argument("-Dfixture.assets=")),
        java_root.join("assets")
    );
    assert_eq!(
        Path::new(argument("-Dfixture.libraries=")),
        java_root.join("libraries")
    );
    assert_eq!(
        Path::new(argument("-Dfixture.logging="))
            .canonicalize()
            .unwrap(),
        logging.canonicalize().unwrap()
    );
    let natives = canonical.join(".pcl-linux/natives").join(id);
    assert_eq!(
        Path::new(argument("-Dfixture.natives="))
            .canonicalize()
            .unwrap(),
        natives.canonicalize().unwrap()
    );
    assert_eq!(
        Path::new(argument("-Dfixture.legacy=")),
        java_root.join("assets/virtual/legacy")
    );
    assert!(!canonical.join("assets/virtual/legacy").exists());
    let classpath = &plan.args[plan.args.iter().position(|arg| arg == "-cp").unwrap() + 1];
    let entries: Vec<_> = env::split_paths(classpath).collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].canonicalize().unwrap(),
        library.canonicalize().unwrap()
    );
    assert_eq!(
        entries[1].canonicalize().unwrap(),
        version.join(format!("{id}.jar")).canonicalize().unwrap()
    );
    assert!(plan
        .args
        .windows(2)
        .any(|args| args == ["--username", "Fixture_Player"]));
    assert!(plan
        .args
        .windows(2)
        .any(|args| args == ["--fixtureLiteral", literal]));
    assert!(plan
        .args
        .windows(2)
        .any(|args| args[0] == "--gameDir" && Path::new(&args[1]) == java_root));
    let explicit = pcl_core::build_launch_plan_with_options(
        &canonical,
        directory,
        id,
        "Fixture_Player",
        2,
        &selected,
        &[],
        &pcl_core::LaunchOptions {
            jvm_args: "-Duser.dir=explicit-user-choice".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let user_dirs = explicit
        .args
        .iter()
        .filter_map(|arg| arg.strip_prefix("-Duser.dir="))
        .collect::<Vec<_>>();
    assert_eq!(
        user_dirs,
        [java_root.to_str().unwrap(), "explicit-user-choice"]
    );
    assert_eq!(explicit.game_dir, plan.game_dir);
    // Only the fake JVM's fixed property query ran. No jar or game is started.
    assert_stopped(directory, "root.pid");
}

fn case(mode: &str) {
    let directory = PathBuf::from(env::var_os(REPORT).unwrap());
    let image = directory.join("java space & '$(literal)%'.exe");
    fs::copy(env::current_exe().unwrap(), &image).unwrap();
    #[cfg(windows)]
    if mode == "win32-plan" {
        windows_launch_plan(&directory, &image);
        return;
    }
    let started = Instant::now();
    let probe_image = fs::canonicalize(&image).unwrap();
    let result = pcl_core::java::inspect(&probe_image);
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "probe exceeded its bounded runtime/cleanup"
    );
    match mode {
        "normal" | "exit-with-child" | "win32-argv0" => {
            let runtime = result.unwrap();
            assert_eq!(runtime.major, 21);
            assert_eq!(runtime.vendor, "Fixture JVM");
            let callable = PathBuf::from(&runtime.path);
            assert_eq!(fs::canonicalize(&callable).unwrap(), probe_image);
            #[cfg(unix)]
            assert_eq!(callable, probe_image);
            #[cfg(windows)]
            {
                assert!(callable.is_absolute());
                assert!(!runtime.path.starts_with(r"\\?\"));
            }
            let selected = pcl_core::java::select_from_catalog(
                &pcl_core::java::JavaCatalog {
                    runtimes: vec![runtime],
                    unavailable: vec![],
                },
                21,
                true,
            )
            .unwrap();
            assert_eq!(selected, callable);
            #[cfg(windows)]
            if mode == "win32-argv0" {
                assert!(probe_image.to_string_lossy().starts_with(r"\\?\"));
                assert_eq!(
                    fs::read_to_string(report("argv0.txt")).unwrap(),
                    selected.to_str().unwrap()
                );
                // The returned catalog path is also the later game-launch
                // executable. A probe-only argv[0] workaround is insufficient.
                let mut command = fixture_command(&selected);
                command
                    .args(["-XshowSettings:properties", "-version"])
                    .stdin(Stdio::null());
                for name in JVM_OPTIONS {
                    command.env_remove(name);
                }
                assert!(command.output().unwrap().status.success());
            }
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
    #[cfg(not(windows))]
    {
        let untouched = Path::new("missing relative invocation fixture");
        assert_eq!(
            pcl_core::java::invocation_path(untouched).unwrap(),
            untouched
        );
    }
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
    for &mode in MODES {
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
