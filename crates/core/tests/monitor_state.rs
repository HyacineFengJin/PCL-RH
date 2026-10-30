//! Actual desktop monitor state/log adapters on native hosts, without a GUI,
//! JVM, account or user directory. Process ownership is covered separately by
//! the native executable lifecycle fixture.
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor/control.rs"]
mod control;
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local.rs"]
mod launcher_local;
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor/output.rs"]
mod output;
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor/process.rs"]
mod process;
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor/state.rs"]
mod state;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/native-monitor-state");
    fs::create_dir_all(&work).unwrap();
    let temporary = tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap();
    let root = fs::canonicalize(temporary.path()).unwrap();
    (temporary, root)
}

#[test]
fn absent_monitor_state_is_read_only() {
    let (_temporary, project) = fixture();
    assert!(state::read_status(&project).unwrap().is_none());
    assert!(!project.join(".pcl-rust").exists());
}

#[test]
fn initial_session_keeps_its_native_lifetime_lock_and_exact_binding() {
    let (_temporary, project) = fixture();
    let game_root = project.join("game");
    fs::create_dir(&game_root).unwrap();
    let log = game_root.join(".pcl-linux/logs/fixture.log");
    let _session = state::Session::start(
        &project,
        "native-root".into(),
        game_root.clone(),
        "native-instance".into(),
        log.clone(),
    )
    .unwrap();
    let status = state::read_status(&project).unwrap().unwrap();
    assert_eq!(status.root_id, "native-root");
    assert_eq!(status.instance_id.as_deref(), Some("native-instance"));
    assert_eq!(status.monitor_pid, std::process::id());
    assert!(status.supervisor_running);
    assert_eq!(status.phase, "starting");
    assert!(status.pid.is_none());
    assert!(state::Session::start(
        &project,
        "native-root".into(),
        game_root,
        "native-instance".into(),
        log,
    )
    .is_err());
}

#[test]
fn launch_log_is_appended_without_replacing_its_existing_inode() {
    let (_temporary, root) = fixture();
    let path = root.join(".pcl-linux/logs/fixture.log");
    let mut log = state::open_launch_log(&root, &path).unwrap();
    log.write_all(b"older output\n").unwrap();
    log.sync_all().unwrap();
    let original = pcl_core::retained_fs::metadata(&log).unwrap().identity;
    drop(log);
    let mut log = state::open_launch_log(&root, &path).unwrap();
    assert_eq!(
        pcl_core::retained_fs::metadata(&log).unwrap().identity,
        original
    );
    log.write_all(b"new output\n").unwrap();
    log.sync_all().unwrap();
    drop(log);
    assert_eq!(fs::read(path).unwrap(), b"older output\nnew output\n");
    assert!(state::open_launch_log(&root, &root.join("outside.log")).is_err());
    assert!(!root.join("outside.log").exists());
}
