use crate::{
    control_port, launcher_game_monitor as monitor,
    launcher_local::filesystem::{Dir, Scope},
};
use pcl_core::{
    game_process::{self, Identity},
    retained_fs::{Directory, Identity as FileIdentity},
};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(10);
const GAME_LIFETIME: Duration = Duration::from_secs(20);
const SECRET: &str = "detached-fixture-secret";
const HISTORY: &[u8] = b"historical log bytes remain intact\n";
fn pause() {
    std::thread::sleep(Duration::from_millis(10));
}
fn write_private(path: impl AsRef<Path>, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
fn cwd_identity() -> FileIdentity {
    Directory::from_file(fs::File::open(".").unwrap())
        .unwrap()
        .identity()
        .unwrap()
}
fn synthetic_game() -> i32 {
    assert_eq!(env::args().nth(2).as_deref(), Some(SECRET));
    write_private(
        "cwd.identity.json",
        &serde_json::to_vec(&cwd_identity()).unwrap(),
    );
    // Exercise the production streaming redactor across a real pipe boundary.
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    output
        .write_all(b"synthetic stdout detached-fixture-")
        .unwrap();
    output.flush().unwrap();
    std::thread::sleep(Duration::from_millis(15));
    output.write_all(b"secret ready\n").unwrap();
    output.flush().unwrap();
    drop(output);
    eprintln!("synthetic stderr {SECRET} ready");
    let started = Instant::now();
    let mut count = 0u64;
    // A failing control implementation cannot strand an endless fixture process.
    // The monitor remains sole child owner; the harness never bare-PID-kills it.
    while started.elapsed() < GAME_LIFETIME {
        count += 1;
        write_private("heartbeat", count.to_string().as_bytes());
        if Path::new("write-after-rename").is_file() {
            write_private(
                "after-rename.identity.json",
                &serde_json::to_vec(&cwd_identity()).unwrap(),
            );
            println!("relative cwd survives parent rename");
        }
        println!("synthetic heartbeat {count}");
        std::thread::sleep(Duration::from_millis(30));
    }
    0
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Started {
    monitor_pid: u32,
    game_pid: u32,
    session: String,
    log_start: u64,
}
fn request(base: &Path) -> monitor::MonitorRequest {
    monitor::MonitorRequest::new(
        base.join("project"),
        "synthetic-root".into(),
        base.join("root"),
        "synthetic-instance".into(),
        env::current_exe().unwrap().canonicalize().unwrap(),
        vec!["--synthetic-game".into(), SECRET.into()],
        base.join("root"),
        base.join("root/.pcl-linux/logs/fixture.log"),
        vec![SECRET.into()],
    )
    .unwrap()
}
fn gui(base: &Path) -> i32 {
    match monitor::spawn_monitor(request(base)) {
        Ok(started) => {
            write_private(
                base.join("gui.started.json"),
                &serde_json::to_vec(&Started {
                    monitor_pid: started.monitor_pid,
                    game_pid: started.pid,
                    session: started.session_id,
                    log_start: started.log_start,
                })
                .unwrap(),
            );
            // Exiting terminates all GUI/reaper threads. The production helper
            // owns its independent session, GameChild, output readers and lease.
            0
        }
        Err(error) => {
            write_private(base.join("gui.error"), error.as_bytes());
            2
        }
    }
}
pub(super) fn dispatch_fixture() -> Option<i32> {
    let arguments = env::args_os().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|arg| arg == "--synthetic-game")
    {
        return Some(synthetic_game());
    }
    if arguments.get(1).is_some_and(|arg| arg == "--synthetic-gui") {
        assert_eq!(arguments.len(), 3);
        return Some(gui(Path::new(&arguments[2])));
    }
    None
}

struct Fixture {
    _temporary: tempfile::TempDir,
    base: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|p| p.join("Cargo.lock").is_file() && p.join("apps/desktop/src-tauri").is_dir())
            .unwrap();
        let work = repository.join("work/detached-monitor-lifecycle");
        fs::create_dir_all(&work).unwrap();
        let temporary = tempfile::tempdir_in(work.canonicalize().unwrap()).unwrap();
        let base = temporary.path().to_owned();
        let root = Dir::absolute(&base).unwrap();
        root.create("project").unwrap();
        let game_root = root.create("root").unwrap();
        let owner = game_root.create(".pcl-linux").unwrap();
        let logs = owner.create("logs").unwrap();
        logs.write_new("fixture.log", HISTORY).unwrap();
        Self {
            _temporary: temporary,
            base,
        }
    }
    fn project(&self) -> PathBuf {
        self.base.join("project")
    }
    fn log(&self) -> PathBuf {
        self.base.join("root/.pcl-linux/logs/fixture.log")
    }
    fn scope(&self) -> Scope {
        Scope::open(&self.project(), "game-monitor").unwrap()
    }
    fn launch_gui(&self) -> OwnedGui {
        OwnedGui(
            Command::new(env::current_exe().unwrap())
                .arg("--synthetic-gui")
                .arg(&self.base)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }
}
struct OwnedGui(Child);
impl OwnedGui {
    fn wait_success(&mut self, fixture: &Fixture) {
        let until = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "synthetic GUI failed: {}",
                    fs::read_to_string(fixture.base.join("gui.error")).unwrap_or_default()
                );
                return;
            }
            assert!(
                Instant::now() < until,
                "synthetic GUI did not finish handoff"
            );
            pause();
        }
    }
}
impl Drop for OwnedGui {
    fn drop(&mut self) {
        // This exact unreaped direct child belongs to the fixture. A completed
        // child is never signalled; no process-group or external PID is used.
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
#[derive(Deserialize)]
struct MarkerIdentities {
    monitor: Identity,
    game: Option<Identity>,
}
struct Attempt {
    project: PathBuf,
    monitor: Identity,
    game: Identity,
}
impl Attempt {
    fn capture(fixture: &Fixture) -> Self {
        let identities: MarkerIdentities = serde_json::from_slice(
            &fs::read(fixture.project().join(".pcl-rust/game-monitor/active.json")).unwrap(),
        )
        .unwrap();
        let game = identities
            .game
            .expect("production marker must bind the launched game");
        assert!(identities.monitor.valid() && game.valid());
        assert_ne!(identities.monitor.pid, game.pid);
        Self {
            project: fixture.project(),
            monitor: identities.monitor,
            game,
        }
    }
    fn request_stop(&self) {
        let scope = Scope::open(&self.project, "game-monitor").unwrap();
        control_port::request(&scope, &self.monitor, &self.game).unwrap();
    }
    fn assert_alive(&self) {
        assert!(
            game_process::alive(&self.monitor).unwrap(),
            "detached monitor stopped prematurely"
        );
        assert!(
            game_process::alive(&self.game).unwrap(),
            "synthetic game stopped prematurely"
        );
    }
    fn wait_idle(&self) {
        let until = Instant::now() + WAIT;
        let mut last_error = String::new();
        loop {
            match monitor::read_status(&self.project) {
                Ok(Some(status)) if !status.busy => {
                    assert!(!status.game_running && !status.supervisor_running);
                    assert_eq!(
                        status.phase, "failed",
                        "synthetic game must be stopped by its owner before its self-deadline"
                    );
                    assert!(!game_process::alive(&self.game).unwrap());
                    assert!(!game_process::alive(&self.monitor).unwrap());
                    return;
                }
                Ok(_) => {}
                Err(error) => last_error = error,
            }
            assert!(
                Instant::now() < until,
                "production monitor did not finish: {last_error}"
            );
            pause();
        }
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if let Ok(scope) = Scope::open(&self.project, "game-monitor") {
            let _ = control_port::request(&scope, &self.monitor, &self.game);
        }
        // The finite synthetic game remains its monitor's owned child. Even if
        // stop-control fails, allow that exact fixture to reach its self-bound
        // and be reaped; never substitute check-identity-then-kill on macOS.
        let until = Instant::now() + GAME_LIFETIME + Duration::from_secs(3);
        while Instant::now() < until {
            if !game_process::alive(&self.game).unwrap_or(false)
                && !game_process::alive(&self.monitor).unwrap_or(false)
            {
                break;
            }
            pause();
        }
    }
}
fn wait_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    let until = Instant::now() + WAIT;
    loop {
        if let Ok(bytes) = fs::read(path) {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                return value;
            }
        }
        assert!(
            Instant::now() < until,
            "fixture evidence missing: {}",
            path.display()
        );
        pause();
    }
}
fn heartbeat(path: &Path) -> u64 {
    let until = Instant::now() + WAIT;
    loop {
        if let Ok(bytes) = fs::read_to_string(path) {
            if let Ok(value) = bytes.parse() {
                return value;
            }
        }
        assert!(Instant::now() < until, "synthetic heartbeat missing");
        pause();
    }
}
fn wait_heartbeat_after(path: &Path, before: u64) {
    let until = Instant::now() + WAIT;
    loop {
        if heartbeat(path) > before {
            return;
        }
        assert!(
            Instant::now() < until,
            "synthetic game no longer makes progress"
        );
        pause();
    }
}
fn assert_redacted_log(path: &Path) {
    let log = fs::read_to_string(path).unwrap();
    assert!(log.as_bytes().starts_with(HISTORY));
    assert!(log.contains("synthetic stdout [REDACTED] ready"));
    assert!(log.contains("synthetic stderr [REDACTED] ready"));
    assert!(!log.contains(SECRET));
}
fn handoff_pins_cwd_and_append_log_until_owned_control_stop() {
    let fixture = Fixture::new();
    let expected_directory = Directory::open_absolute(&fixture.base.join("root"))
        .unwrap()
        .identity()
        .unwrap();
    let started = monitor::spawn_monitor(request(&fixture.base))
        .expect("production READY/COMMIT/ACK handoff");
    let attempt = Attempt::capture(&fixture);
    attempt.assert_alive();
    let status = monitor::read_status(&fixture.project()).unwrap().unwrap();
    assert_eq!(status.pid, Some(started.pid));
    assert_eq!(status.monitor_pid, started.monitor_pid);
    assert_eq!(status.session_id, started.session_id);
    assert_eq!(status.log_start, Some(HISTORY.len() as u64));
    assert_eq!(started.log_start, HISTORY.len() as u64);
    assert_eq!(status.instance_id.as_deref(), Some("synthetic-instance"));
    let identity: FileIdentity = wait_json(&fixture.base.join("root/cwd.identity.json"));
    assert_eq!(identity, expected_directory);
    let held = fixture.base.join("held-root");
    fs::rename(fixture.base.join("root"), &held).unwrap();
    Dir::absolute(&fixture.base)
        .unwrap()
        .create("root")
        .unwrap();
    write_private(held.join("write-after-rename"), b"go");
    let identity: FileIdentity = wait_json(&held.join("after-rename.identity.json"));
    assert_eq!(identity, expected_directory);
    assert_eq!(fs::read_dir(fixture.base.join("root")).unwrap().count(), 0);
    attempt.request_stop();
    attempt.wait_idle();
    assert_redacted_log(&held.join(".pcl-linux/logs/fixture.log"));
}
fn gui_exit_and_stale_requests_do_not_transfer_child_ownership() {
    let fixture = Fixture::new();
    let mut gui = fixture.launch_gui();
    gui.wait_success(&fixture);
    let started: Started = wait_json(&fixture.base.join("gui.started.json"));
    let attempt = Attempt::capture(&fixture);
    assert_eq!(attempt.monitor.pid, started.monitor_pid);
    assert_eq!(attempt.game.pid, started.game_pid);
    assert_eq!(started.log_start, HISTORY.len() as u64);
    let status = monitor::read_status(&fixture.project()).unwrap().unwrap();
    assert_eq!(status.session_id, started.session);
    assert_eq!(status.log_start, Some(started.log_start));
    assert!(status.busy && status.supervisor_running && status.game_running);
    attempt.assert_alive();
    let heartbeat_path = fixture.base.join("root/heartbeat");
    let before = heartbeat(&heartbeat_path);
    std::thread::sleep(Duration::from_millis(300));
    wait_heartbeat_after(&heartbeat_path, before);
    attempt.assert_alive();
    let scope = fixture.scope();
    let mut old_monitor = attempt.monitor.clone();
    old_monitor.start_time = if old_monitor.start_time > 1 {
        old_monitor.start_time - 1
    } else {
        old_monitor.start_time + 1
    };
    control_port::request(&scope, &old_monitor, &attempt.game).unwrap();
    let mut wrong_game = attempt.game.clone();
    wrong_game.start_time += 1;
    control_port::request(&scope, &attempt.monitor, &wrong_game).unwrap();
    let before = heartbeat(&heartbeat_path);
    // The production monitor polls every 200ms. Two intervals plus continued
    // heartbeats prove neither wrong document acted on its owned GameChild.
    std::thread::sleep(Duration::from_millis(600));
    wait_heartbeat_after(&heartbeat_path, before);
    attempt.assert_alive();
    assert!(
        control_port::request(&scope, &attempt.monitor, &attempt.game).is_err(),
        "wrong-game request at the same session key must not be overwritten"
    );
    let current = scope
        .folder
        .as_ref()
        .unwrap()
        .names()
        .unwrap()
        .into_iter()
        .filter(|name| name.starts_with(".stop-"))
        .find(|name| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(scope.path().join(name)).unwrap()).unwrap();
            value["monitor"] == serde_json::to_value(&attempt.monitor).unwrap()
        })
        .unwrap();
    // Preserve the intentionally poisoned test request as evidence; use a new
    // name so a correct immutable request can be sent without deleting bytes.
    // This is only synthetic fixture setup, never production recovery policy.
    fs::rename(
        scope.path().join(current),
        scope.path().join("preserved-wrong-game.json"),
    )
    .unwrap();
    attempt.request_stop();
    attempt.wait_idle();
    assert!(scope.path().join("preserved-wrong-game.json").is_file());
    assert_redacted_log(&fixture.log());
    let marker =
        fs::read_to_string(fixture.project().join(".pcl-rust/game-monitor/active.json")).unwrap();
    assert!(!marker.contains(SECRET));
    assert!(!marker.contains("redactions") && !marker.contains("args"));
}
pub(super) fn run() {
    handoff_pins_cwd_and_append_log_until_owned_control_stop();
    println!("ok: Unix production monitor READY/COMMIT/ACK, held cwd/log, owner stop");
    gui_exit_and_stale_requests_do_not_transfer_child_ownership();
    println!(
        "ok: Unix GUI exit preserves monitor/game; stale/wrong identity requests cannot stop child"
    );
}
