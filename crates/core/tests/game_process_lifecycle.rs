//! Same-executable, private-directory process fixtures. No real Java/game,
//! shell, desktop, account or system service is used by this executable.
use pcl_core::game_process::{self, GameChild, GameOutput};
use std::{
    env,
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
#[cfg(target_os = "macos")]
#[path = "game_process_lifecycle/macos_sandbox_probe.rs"]
mod macos_sandbox_probe;
#[cfg(windows)]
#[path = "game_process_lifecycle/windows_cwd_probe.rs"]
mod windows_cwd_probe;

const WAIT: Duration = Duration::from_secs(10);
fn pause() {
    std::thread::sleep(Duration::from_millis(5));
}
fn hang() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(30));
    }
}
fn mark(folder: &Path, name: &str) {
    fs::write(folder.join(name), std::process::id().to_string()).unwrap();
}
fn wait_file(folder: &Path, name: &str) -> u32 {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(value) = fs::read_to_string(folder.join(name)) {
            if let Ok(pid) = value.parse() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not become ready: {name}"
        );
        pause();
    }
}
fn args(mode: &str, folder: &Path) -> Vec<OsString> {
    vec![
        "--fixture".into(),
        mode.into(),
        folder.as_os_str().to_owned(),
    ]
}
fn spawn(mode: &str, folder: &Path) -> GameChild {
    GameChild::spawn(&env::current_exe().unwrap(), &args(mode, folder), folder).unwrap()
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn external(mode: &str, folder: &Path) -> Owned {
    Owned(
        Command::new(env::current_exe().unwrap())
            .args(args(mode, folder))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn wait_exit(child: &mut Child) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "fixture exit deadline");
        pause();
    }
}
fn wait_gone(identity: &game_process::Identity) {
    let deadline = Instant::now() + WAIT;
    while game_process::alive(identity).unwrap() {
        assert!(Instant::now() < deadline, "owned fixture survived cleanup");
        pause();
    }
}
fn drain(mut output: GameOutput) -> Vec<u8> {
    let mut bytes = Vec::new();
    let deadline = Instant::now() + WAIT;
    let mut block = [0; 8192];
    loop {
        match output.read(&mut block) {
            Ok(0) => return bytes,
            Ok(count) => bytes.extend_from_slice(&block[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => pause(),
            Err(error) => panic!("fixture output read: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "fixture pipe retained after cleanup"
        );
    }
}
fn fixture(mode: &str, folder: &Path, tail: &[OsString]) -> ! {
    match mode {
        "normal" => {
            assert_eq!(
                env::current_dir().unwrap().canonicalize().unwrap(),
                folder.canonicalize().unwrap()
            );
            let mut byte = [0];
            assert_eq!(std::io::stdin().read(&mut byte).unwrap(), 0);
            fs::write(folder.join("argv.json"), serde_json::to_vec(tail).unwrap()).unwrap();
            println!("fixture stdout");
            eprintln!("fixture stderr");
            std::process::exit(7);
        }
        "hang" => {
            mark(folder, "root.pid");
            hang();
        }
        "descendant" => {
            mark(folder, "descendant.pid");
            hang();
        }
        #[cfg(windows)]
        "io-probe" => {
            windows_output_probe(folder);
            std::process::exit(0);
        }
        #[cfg(windows)]
        "flood-output" => {
            use std::io::Write;
            let stdout = std::thread::spawn(|| {
                std::io::stdout()
                    .write_all(&vec![b'x'; 256 * 1024])
                    .unwrap();
                std::io::stdout().flush().unwrap();
            });
            std::io::stderr()
                .write_all(&vec![b'y'; 256 * 1024])
                .unwrap();
            std::io::stderr().flush().unwrap();
            stdout.join().unwrap();
            std::process::exit(0);
        }
        "exit-with-child" | "hang-with-child" => {
            let _child = Command::new(env::current_exe().unwrap())
                .args(args("descendant", folder))
                .spawn()
                .unwrap();
            wait_file(folder, "descendant.pid");
            mark(folder, "root.pid");
            if mode == "exit-with-child" {
                while !folder.join("exit-root").exists() {
                    pause();
                }
                std::process::exit(0);
            }
            hang();
        }
        "monitor" | "owner-loss" => {
            let mut game = spawn(
                if cfg!(windows) && mode == "owner-loss" {
                    "hang-with-child"
                } else {
                    "hang"
                },
                folder,
            );
            mark(folder, "monitor.pid");
            wait_file(folder, "root.pid");
            while !folder.join("release-monitor").exists() {
                pause();
            }
            game.terminate().unwrap();
            std::process::exit(0);
        }
        "gui" => {
            let _monitor = Command::new(env::current_exe().unwrap())
                .args(args("monitor", folder))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            wait_file(folder, "root.pid");
            std::process::exit(0);
        }
        _ => std::process::exit(2),
    }
}
fn private_folder() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("work/game-process-fixtures");
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("case-")
        .tempdir_in(root)
        .unwrap()
}
fn normal_and_literal_argv() {
    let folder = private_folder();
    let image = folder.path().join(if cfg!(windows) {
        "游戏 ' & $(literal).exe"
    } else {
        "游戏 ' & $(literal)"
    });
    fs::copy(env::current_exe().unwrap(), &image).unwrap();
    let tail = [
        "",
        "space here",
        "quote\"here",
        "tail\\",
        "slashes\\\\\"quote",
        "& $(never-run);",
        "😀中文",
    ]
    .map(OsString::from);
    let mut arguments = args("normal", folder.path());
    arguments.extend(tail.clone());
    let mut child = GameChild::spawn(&image, &arguments, folder.path()).unwrap();
    let stdout = child.take_stdout().unwrap();
    let stderr = child.take_stderr().unwrap();
    let result = child.wait().unwrap();
    assert_eq!(result.code(), Some(7));
    assert_eq!(child.wait().unwrap(), result);
    assert_eq!(child.try_wait().unwrap(), Some(result));
    assert_eq!(drain(stdout), b"fixture stdout\n");
    assert_eq!(
        String::from_utf8(drain(stderr)).unwrap().trim(),
        "fixture stderr"
    );
    let actual: Vec<OsString> =
        serde_json::from_slice(&fs::read(folder.path().join("argv.json")).unwrap()).unwrap();
    assert_eq!(actual, tail);
    assert!(!folder.path().join("never-run").exists());
}
fn identity_and_owned_stop() {
    let folder = private_folder();
    let other_folder = private_folder();
    let mut child = spawn("hang", folder.path());
    let mut other = spawn("hang", other_folder.path());
    wait_file(folder.path(), "root.pid");
    wait_file(other_folder.path(), "root.pid");
    let identity = game_process::identity(child.id()).unwrap().unwrap();
    assert!(child.try_wait().unwrap().is_none());
    assert!(identity.valid());
    assert!(game_process::alive(&identity).unwrap());
    assert_eq!(
        identity,
        game_process::identity(child.id()).unwrap().unwrap()
    );
    let mut wrong = identity.clone();
    wrong.start_time += 1;
    assert!(!game_process::alive(&wrong).unwrap());
    #[cfg(any(target_os = "linux", windows))]
    {
        assert!(game_process::terminate(&wrong).is_err());
        assert!(game_process::alive(&identity).unwrap());
    }
    child.terminate().unwrap();
    wait_gone(&identity);
    assert!(game_process::identity(other.id()).unwrap().is_some());
    other.terminate().unwrap();
    assert!(game_process::identity(0).is_err());
    assert!(game_process::identity(u32::MAX).is_err());
}
fn exit_cleans_owned_descendants() {
    let folder = private_folder();
    let mut child = spawn("exit-with-child", folder.path());
    wait_file(folder.path(), "root.pid");
    let descendant = game_process::identity(wait_file(folder.path(), "descendant.pid"))
        .unwrap()
        .unwrap();
    fs::write(folder.path().join("exit-root"), b"").unwrap();
    assert!(child.wait().unwrap().success());
    wait_gone(&descendant);
    assert!(drain(child.take_stdout().unwrap()).is_empty());
}
fn drop_cleans_owned_descendants() {
    let folder = private_folder();
    let child = spawn("hang-with-child", folder.path());
    let root = game_process::identity(wait_file(folder.path(), "root.pid"))
        .unwrap()
        .unwrap();
    let descendant = game_process::identity(wait_file(folder.path(), "descendant.pid"))
        .unwrap()
        .unwrap();
    drop(child);
    wait_gone(&root);
    wait_gone(&descendant);
}
fn failure_keeps_launch_details_private() {
    let folder = private_folder();
    let secret = "fixture-private-token";
    let missing = folder.path().join(format!("missing-{secret}"));
    let error = GameChild::spawn(&missing, &[secret.into()], folder.path())
        .err()
        .expect("missing image must fail");
    assert!(!error.contains(secret));
    assert!(!error.contains(&folder.path().display().to_string()));
    let invalid = OsString::from(format!("{secret}\0suffix"));
    let error = GameChild::spawn(&env::current_exe().unwrap(), &[invalid], folder.path())
        .err()
        .expect("NUL argument must fail before spawn");
    assert!(!error.contains(secret));
    assert!(fs::read_dir(folder.path()).unwrap().next().is_none());
}
#[cfg(windows)]
fn windows_output_probe(folder: &Path) {
    let idle = folder.join("idle");
    fs::create_dir(&idle).unwrap();
    let mut child = spawn("hang-with-child", &idle);
    wait_file(&idle, "root.pid");
    let descendant = game_process::identity(wait_file(&idle, "descendant.pid"))
        .unwrap()
        .unwrap();
    let mut stdout = child.take_stdout().unwrap();
    let mut stderr = child.take_stderr().unwrap();
    let start = Instant::now();
    for stream in [&mut stdout, &mut stderr] {
        assert_eq!(stream.read(&mut []).unwrap(), 0);
        for _ in 0..128 {
            assert_eq!(
                stream.read(&mut [0; 1]).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "idle output poll blocked"
    );
    // Pending reads were initiated on this thread. Drop on different threads
    // exercises CancelIoEx rather than thread-local CancelIo; live descendants
    // keep both writers open during cancellation.
    let stdout = std::thread::spawn(move || {
        let start = Instant::now();
        drop(stdout);
        assert!(start.elapsed() < Duration::from_secs(1));
    });
    let stderr = std::thread::spawn(move || {
        let start = Instant::now();
        drop(stderr);
        assert!(start.elapsed() < Duration::from_secs(1));
    });
    stdout.join().unwrap();
    stderr.join().unwrap();
    assert!(game_process::alive(&descendant).unwrap());
    child.terminate().unwrap();
    wait_gone(&descendant);

    let retained = folder.join("retained");
    fs::create_dir(&retained).unwrap();
    let mut child = spawn("exit-with-child", &retained);
    let root = game_process::identity(wait_file(&retained, "root.pid"))
        .unwrap()
        .unwrap();
    let descendant = game_process::identity(wait_file(&retained, "descendant.pid"))
        .unwrap()
        .unwrap();
    let mut stdout = child.take_stdout().unwrap();
    let mut stderr = child.take_stderr().unwrap();
    assert_eq!(
        stdout.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        stderr.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    fs::write(retained.join("exit-root"), b"").unwrap();
    wait_gone(&root);
    assert!(game_process::alive(&descendant).unwrap());
    assert_eq!(
        stdout.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(child.wait().unwrap().success());
    wait_gone(&descendant);
    assert!(drain(stdout).is_empty());
    assert!(drain(stderr).is_empty());

    let flood = folder.join("flood");
    fs::create_dir(&flood).unwrap();
    let mut child = spawn("flood-output", &flood);
    let stdout = child.take_stdout().unwrap();
    let stderr = child.take_stderr().unwrap();
    let collect = |mut stream: GameOutput, expected: u8| {
        let deadline = Instant::now() + WAIT;
        let mut block = [0u8; 137];
        let mut total = 0;
        loop {
            match stream.read(&mut block) {
                Ok(0) => break,
                Ok(count) => {
                    assert!(block[..count].iter().all(|byte| *byte == expected));
                    total += count;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => pause(),
                Err(error) => panic!("overlapped output read failed: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "overlapped output flood deadline"
            );
        }
        assert_eq!(total, 256 * 1024);
    };
    let stdout = std::thread::spawn(move || collect(stdout, b'x'));
    let stderr = std::thread::spawn(move || collect(stderr, b'y'));
    assert!(child.wait().unwrap().success());
    stdout.join().unwrap();
    stderr.join().unwrap();
}
#[cfg(windows)]
fn windows_overlapped_output() {
    let folder = private_folder();
    // An outer owned process enforces the probe deadline even if a regression
    // reintroduces a synchronous Read/Drop. Killing it closes its private Jobs.
    let mut probe = Owned(
        Command::new(env::current_exe().unwrap())
            .args(args("io-probe", folder.path()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = probe.0.try_wait().unwrap() {
            let mut diagnostic = String::new();
            probe
                .0
                .stderr
                .take()
                .unwrap()
                .take(16 * 1024)
                .read_to_string(&mut diagnostic)
                .unwrap();
            assert!(
                status.success(),
                "overlapped fixture failed ({status}): {diagnostic}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "overlapped fixture outer deadline"
        );
        pause();
    }
}
fn monitor_survives_gui_exit() {
    let folder = private_folder();
    // A failed assertion must still let the detached fixture clean its owned
    // game. This path never signals a monitor found only by a persisted PID.
    struct Release<'a>(&'a Path);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            let _ = fs::write(self.0.join("release-monitor"), b"");
        }
    }
    let _release = Release(folder.path());
    let mut gui = external("gui", folder.path());
    wait_exit(&mut gui.0);
    let root = game_process::identity(wait_file(folder.path(), "root.pid"))
        .unwrap()
        .unwrap();
    let monitor = game_process::identity(wait_file(folder.path(), "monitor.pid"))
        .unwrap()
        .unwrap();
    assert!(game_process::alive(&root).unwrap());
    assert!(game_process::alive(&monitor).unwrap());
    fs::write(folder.path().join("release-monitor"), b"").unwrap();
    wait_gone(&root);
    wait_gone(&monitor);
}
#[cfg(any(target_os = "linux", windows))]
fn owner_loss_cleans_kernel_owned_processes() {
    let folder = private_folder();
    let mut monitor = external("owner-loss", folder.path());
    let root = game_process::identity(wait_file(folder.path(), "root.pid"))
        .unwrap()
        .unwrap();
    #[cfg(windows)]
    let descendant = game_process::identity(wait_file(folder.path(), "descendant.pid"))
        .unwrap()
        .unwrap();
    monitor.0.kill().unwrap();
    monitor.0.wait().unwrap();
    wait_gone(&root);
    #[cfg(windows)]
    wait_gone(&descendant);
}
#[cfg(unix)]
fn pinned_directory_and_no_shell_fallback() {
    use std::os::unix::fs::PermissionsExt;
    let folder = private_folder();
    let original = folder.path().join("original");
    fs::create_dir(&original).unwrap();
    let directory = fs::File::open(&original).unwrap();
    let moved = folder.path().join("moved");
    fs::rename(&original, &moved).unwrap();
    fs::create_dir(&original).unwrap();
    let mut child = GameChild::spawn_in_directory(
        &env::current_exe().unwrap(),
        &args("normal", &moved),
        &directory,
    )
    .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(7));
    assert!(moved.join("argv.json").exists());
    assert!(!original.join("argv.json").exists());
    let text = folder.path().join("plain text");
    fs::write(&text, "touch implicit-shell-side-effect\n").unwrap();
    fs::set_permissions(&text, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(GameChild::spawn(&text, &[], folder.path()).is_err());
    assert!(!folder.path().join("implicit-shell-side-effect").exists());
}
fn main() {
    #[cfg(target_os = "macos")]
    if let Some(code) = macos_sandbox_probe::dispatch() {
        std::process::exit(code);
    }
    #[cfg(windows)]
    if let Some(code) = windows_cwd_probe::dispatch() {
        std::process::exit(code);
    }
    let arguments = env::args_os().collect::<Vec<_>>();
    if arguments.get(1).is_some_and(|value| value == "--fixture") {
        fixture(
            arguments[2].to_str().unwrap(),
            &PathBuf::from(&arguments[3]),
            &arguments[4..],
        );
    }
    for (name, check) in [
        (
            "literal argv, outputs and exit",
            normal_and_literal_argv as fn(),
        ),
        (
            "stable identity and owned cancellation",
            identity_and_owned_stop,
        ),
        (
            "leader exit cleans descendants",
            exit_cleans_owned_descendants,
        ),
        (
            "owner drop cleans descendants",
            drop_cleans_owned_descendants,
        ),
        (
            "failed launch keeps details private",
            failure_keeps_launch_details_private,
        ),
        (
            "detached monitor survives GUI exit",
            monitor_survives_gui_exit,
        ),
    ] {
        check();
        println!("ok: {name}");
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        owner_loss_cleans_kernel_owned_processes();
        println!("ok: kernel owner-loss cleanup");
    }
    #[cfg(unix)]
    {
        pinned_directory_and_no_shell_fallback();
        println!("ok: pinned cwd and no implicit shell");
    }
    #[cfg(windows)]
    {
        windows_overlapped_output();
        println!("ok: overlapped idle, pending cancellation, retained writers and dual flood");
        windows_cwd_probe::run_if_enabled().expect("Windows cwd diagnostic fixture failed");
    }
    #[cfg(target_os = "macos")]
    macos_sandbox_probe::run_if_enabled().expect("macOS sandbox diagnostic fixture failed");
}
