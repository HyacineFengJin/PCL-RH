use crate::{launcher_game_monitor as monitor, transport_port};
use pcl_core::{
    game_process,
    retained_fs::{Directory, Leaf, Traversal},
};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Write,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
const WAIT: Duration = Duration::from_secs(10);
const SECRET: &str = "synthetic-monitor-secret";
fn pause() {
    std::thread::sleep(Duration::from_millis(10));
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "native monitor fixture deadline");
        pause();
    }
}
fn private_write(path: &Path, bytes: &[u8]) {
    let parent = Directory::open_absolute(path.parent().unwrap()).unwrap();
    let name = Leaf::new(path.file_name().unwrap()).unwrap();
    let mut file = parent.create_file(&name).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
fn cwd_identity() -> pcl_core::retained_fs::Identity {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x02000000)
        .open(".")
        .unwrap();
    Directory::from_file(file).unwrap().identity().unwrap()
}
fn game() -> i32 {
    let mode = env::args().nth(2).unwrap();
    assert_eq!(env::args().nth(3).as_deref(), Some(SECRET));
    fs::write(
        "cwd.identity.json",
        serde_json::to_vec(&cwd_identity()).unwrap(),
    )
    .unwrap();
    print!("synthetic stdout synthetic-monitor-");
    std::io::stdout().flush().unwrap();
    pause();
    println!("secret ready");
    eprintln!("synthetic stderr {SECRET}");
    let start = Instant::now();
    let mut counter = 0;
    while start.elapsed() < Duration::from_secs(20) {
        if mode == "normal" && Path::new("normal-exit").is_file() {
            break;
        }
        counter += 1;
        fs::write("heartbeat", counter.to_string()).unwrap();
        pause();
    }
    0
}
#[derive(Serialize, Deserialize)]
struct Started {
    pid: u32,
    monitor_pid: u32,
    revision: String,
}
fn request(base: &Path, mode: &str) -> monitor::MonitorRequest {
    monitor::MonitorRequest::new(
        base.join("project"),
        "synthetic-root".into(),
        base.join("root"),
        "synthetic-instance".into(),
        env::current_exe().unwrap(),
        vec!["--synthetic-game".into(), mode.into(), SECRET.into()],
        base.join("root"),
        base.join("root/.pcl-linux/logs/fixture.log"),
        vec![SECRET.into()],
    )
    .unwrap()
}
fn gui(base: &Path) -> i32 {
    let started = monitor::spawn_monitor(request(base, "live")).unwrap();
    let status = monitor::read_status(&base.join("project"))
        .unwrap()
        .unwrap();
    private_write(
        &base.join("started.json"),
        &serde_json::to_vec(&Started {
            pid: started.pid,
            monitor_pid: started.monitor_pid,
            revision: status.revision,
        })
        .unwrap(),
    );
    0
}
pub(super) fn dispatch_fixture() -> Option<i32> {
    match env::args().nth(1).as_deref() {
        Some("--synthetic-game") => Some(game()),
        Some("--synthetic-gui") => Some(gui(Path::new(&env::args().nth(2).unwrap()))),
        _ => None,
    }
}
pub(super) fn dispatch_transport_fixture() -> Option<i32> {
    if env::args().nth(1).as_deref() != Some("--game-monitor") {
        return None;
    }
    match env::var("PCL_MONITOR_FIXTURE_MODE").ok().as_deref() {
        Some("hang") => {
            std::thread::sleep(Duration::from_secs(20));
            Some(0)
        }
        Some("inheritance") => {
            let raw = env::var("PCL_MONITOR_FIXTURE_HANDLE")
                .unwrap()
                .parse::<usize>()
                .unwrap() as HANDLE;
            let expected: pcl_core::retained_fs::Identity =
                serde_json::from_str(&env::var("PCL_MONITOR_FIXTURE_ID").unwrap()).unwrap();
            // Query the raw numeric canary without inventing Rust ownership
            // of a possibly absent or recycled handle in this child.
            use windows_sys::Win32::Storage::FileSystem::{
                FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
            };
            let mut info = FILE_ID_INFO::default();
            let found = unsafe {
                GetFileInformationByHandleEx(
                    raw,
                    FileIdInfo,
                    (&mut info as *mut FILE_ID_INFO).cast(),
                    std::mem::size_of::<FILE_ID_INFO>() as u32,
                )
            };
            let leaked = found != 0
                && pcl_core::retained_fs::Identity::Windows {
                    volume: info.VolumeSerialNumber,
                    file: info.FileId.Identifier,
                } == expected;
            println!("{}", if leaked { "LEAKED" } else { "ISOLATED" });
            Some(0)
        }
        _ => None,
    }
}
struct Fixture {
    _temporary: tempfile::TempDir,
    base: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let root = Directory::open_absolute(&base).unwrap();
        root.create_private(&Leaf::new("project").unwrap(), Traversal::AllowMounts)
            .unwrap();
        root.create_private(&Leaf::new("root").unwrap(), Traversal::AllowMounts)
            .unwrap();
        Self {
            _temporary: temporary,
            base,
        }
    }
    fn project(&self) -> PathBuf {
        self.base.join("project")
    }
    fn heartbeat(&self) -> Vec<u8> {
        fs::read(self.base.join("root/heartbeat")).unwrap_or_default()
    }
    fn stopped(&self) {
        wait_for(|| {
            monitor::read_status(&self.project())
                .ok()
                .flatten()
                .is_some_and(|s| !s.game_running && !s.supervisor_running)
        });
    }
}
struct External(Child);
impl Drop for External {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn request_bytes(base: &Path) -> Vec<u8> {
    let request = request(base, "live");
    serde_json::to_vec(&serde_json::json!({"schema_version":2,"project":request.project,"root_id":request.root_id,"root_path":request.root_path,"instance_id":request.instance_id,"java":request.java,"args":request.args,"game_dir":request.game_dir,"log_path":request.log_path,"redactions":request.redactions})).unwrap()
}
pub(super) fn run() {
    // Real production READY/COMMIT/ACK -> normal exit, relative IO and redaction.
    let fixture = Fixture::new();
    let started = monitor::spawn_monitor(request(&fixture.base, "normal")).unwrap();
    assert_ne!(started.pid, started.monitor_pid);
    fs::write(
        fixture.base.join("root/normal-exit"),
        b"exit after actual ACK",
    )
    .unwrap();
    fixture.stopped();
    let expected = Directory::open_absolute(&fixture.base.join("root"))
        .unwrap()
        .identity()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<pcl_core::retained_fs::Identity>(
            &fs::read(fixture.base.join("root/cwd.identity.json")).unwrap()
        )
        .unwrap(),
        expected
    );
    let log = fs::read(fixture.base.join("root/.pcl-linux/logs/fixture.log")).unwrap();
    assert!(!String::from_utf8_lossy(&log).contains(SECRET));
    assert!(String::from_utf8_lossy(&log).contains("synthetic stdout"));
    println!("windows monitor: normal handshake/relative IO/redaction passed");
    // A separate synthetic GUI exits; its monitor/game remain alive and the
    // reopened launcher stops only this session through its private request.
    let fixture = Fixture::new();
    let mut gui = External(
        Command::new(env::current_exe().unwrap())
            .arg("--synthetic-gui")
            .arg(&fixture.base)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| fixture.base.join("started.json").is_file());
    wait_for(|| gui.0.try_wait().unwrap().is_some());
    let started: Started =
        serde_json::from_slice(&fs::read(fixture.base.join("started.json")).unwrap()).unwrap();
    let game = game_process::identity(started.pid).unwrap().unwrap();
    let supervisor = game_process::identity(started.monitor_pid)
        .unwrap()
        .unwrap();
    let before = fixture.heartbeat();
    wait_for(|| !fixture.heartbeat().is_empty() && fixture.heartbeat() != before);
    assert!(game_process::alive(&game).unwrap());
    assert!(game_process::alive(&supervisor).unwrap());
    let status = monitor::read_status(&fixture.project()).unwrap().unwrap();
    monitor::stop_monitor(&fixture.project(), &status.revision).unwrap();
    fixture.stopped();
    assert!(!game_process::alive(&game).unwrap());
    println!("windows monitor: GUI exit ownership/session stop passed");
    // Drop the real transport after READY without COMMIT. The real helper must
    // clean its original GameChild/Job, not leave an unconfirmed game running.
    let fixture = Fixture::new();
    let (mut child, mut io) = transport_port::spawn(&env::current_exe().unwrap()).unwrap();
    let deadline = Instant::now() + WAIT;
    let bytes = request_bytes(&fixture.base);
    io.write(&(bytes.len() as u32).to_be_bytes(), deadline)
        .unwrap();
    io.write(&bytes, deadline).unwrap();
    let ready = io.read_line(deadline).unwrap();
    let pid = ready.split_whitespace().nth(1).unwrap().parse().unwrap();
    let game = game_process::identity(pid).unwrap().unwrap();
    drop(io);
    wait_for(|| child.try_wait().unwrap().is_some());
    assert!(!game_process::alive(&game).unwrap());
    println!("windows monitor: missing COMMIT cleans owned game passed");
    // A non-reading/non-writing synthetic peer exercises both real synchronous
    // pipe directions and bounded cancellation; no thread/buffer is freed live.
    for write in [true, false] {
        env::set_var("PCL_MONITOR_FIXTURE_MODE", "hang");
        let (mut child, mut io) = transport_port::spawn(&env::current_exe().unwrap()).unwrap();
        env::remove_var("PCL_MONITOR_FIXTURE_MODE");
        let start = Instant::now();
        let deadline = start + Duration::from_millis(100);
        let result = if write {
            io.write(&vec![42; 128 * 1024], deadline)
        } else {
            io.read_line(deadline).map(|_| ())
        };
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        drop(io);
        child.kill_owned();
    }
    println!("windows monitor: read/write deadlines passed");
    // Prove that an explicitly inheritable unrelated file is excluded by the
    // actual core HANDLE_LIST, even when its numeric handle is known by helper.
    let fixture = Fixture::new();
    let root = Directory::open_absolute(&fixture.base).unwrap();
    let file = root.create_file(&Leaf::new("canary").unwrap()).unwrap();
    let id = pcl_core::retained_fs::metadata(&file).unwrap().identity;
    assert_ne!(
        unsafe {
            SetHandleInformation(
                file.as_raw_handle(),
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            )
        },
        0
    );
    env::set_var("PCL_MONITOR_FIXTURE_MODE", "inheritance");
    env::set_var(
        "PCL_MONITOR_FIXTURE_HANDLE",
        (file.as_raw_handle() as usize).to_string(),
    );
    env::set_var(
        "PCL_MONITOR_FIXTURE_ID",
        serde_json::to_string(&id).unwrap(),
    );
    let (mut child, mut io) = transport_port::spawn(&env::current_exe().unwrap()).unwrap();
    for key in [
        "PCL_MONITOR_FIXTURE_MODE",
        "PCL_MONITOR_FIXTURE_HANDLE",
        "PCL_MONITOR_FIXTURE_ID",
    ] {
        env::remove_var(key);
    }
    unsafe {
        SetHandleInformation(file.as_raw_handle(), HANDLE_FLAG_INHERIT, 0);
    }
    assert_eq!(io.read_line(Instant::now() + WAIT).unwrap(), "ISOLATED");
    drop(io);
    child.wait().unwrap();
    println!("windows monitor: exact stdio inheritance passed");
}
