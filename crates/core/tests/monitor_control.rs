//! The actual monitor stop-control adapter, using synthetic identities and
//! private repository fixtures. No process is launched or signalled.
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_game_monitor/control.rs"]
mod control;
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local.rs"]
mod launcher_local;

use launcher_local::filesystem::{fixture_path, Dir, Scope};
use pcl_core::game_process::Identity;
use std::{fs, path::PathBuf};

struct Fixture {
    _directory: tempfile::TempDir,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = fixture_path("monitor-control");
        fs::create_dir_all(&base).unwrap();
        let temp = tempfile::tempdir_in(fs::canonicalize(base).unwrap()).unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        Self {
            _directory: temp,
            project,
        }
    }
    fn scope(&self) -> Scope {
        Scope::create(&self.project, "game-monitor").unwrap()
    }
}
fn identity(pid: u32, start_time: u64) -> Identity {
    Identity {
        pid,
        start_time,
        boot_id: "12345678-1234-1234-1234-123456789abc".into(),
    }
}
fn pair() -> (Identity, Identity) {
    (identity(101, 10), identity(102, 11))
}
fn request_file(scope: &Scope) -> PathBuf {
    let name = scope
        .folder
        .as_ref()
        .unwrap()
        .names()
        .unwrap()
        .into_iter()
        .find(|name| name.starts_with(".stop-"))
        .unwrap();
    scope.path().join(name)
}
#[test]
fn exact_session_request_is_bounded_idempotent_and_contains_only_identities() {
    let fixture = Fixture::new();
    let scope = fixture.scope();
    let (monitor, game) = pair();
    let controller = control::Controller::for_session(&scope, &monitor, &game).unwrap();
    assert!(!controller.stop_requested().unwrap());
    control::request(&scope, &monitor, &game).unwrap();
    let path = request_file(&scope);
    let original = fs::read(&path).unwrap();
    let names = scope.folder.as_ref().unwrap().names().unwrap();
    assert!(original.len() <= 1024);
    let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["monitor"], serde_json::to_value(&monitor).unwrap());
    assert_eq!(value["game"], serde_json::to_value(&game).unwrap());
    assert!(controller.stop_requested().unwrap());
    control::request(&scope, &monitor, &game).unwrap();
    assert_eq!(names, scope.folder.as_ref().unwrap().names().unwrap());
    assert_eq!(original, fs::read(path).unwrap());
}
#[test]
fn previous_session_cannot_stop_a_new_monitor_and_wrong_game_is_refused() {
    let fixture = Fixture::new();
    let scope = fixture.scope();
    let (monitor, game) = pair();
    control::request(&scope, &monitor, &game).unwrap();
    let next_monitor = identity(monitor.pid, monitor.start_time + 1);
    let next = control::Controller::for_session(&scope, &next_monitor, &game).unwrap();
    assert!(!next.stop_requested().unwrap());
    let wrong_game = identity(game.pid, game.start_time + 1);
    assert!(control::request(&scope, &monitor, &wrong_game).is_err());
    let wrong = control::Controller::for_session(&scope, &monitor, &wrong_game).unwrap();
    assert!(wrong.stop_requested().is_err());
    assert!(control::Controller::for_session(&scope, &monitor, &monitor).is_err());
    assert!(control::request(&scope, &identity(0, 0), &game).is_err());
    let original = control::Controller::for_session(&scope, &monitor, &game).unwrap();
    assert!(original.stop_requested().unwrap());
}
#[test]
fn malformed_unknown_and_oversized_documents_are_retained_without_stop() {
    let fixture = Fixture::new();
    let scope = fixture.scope();
    let (monitor, game) = pair();
    control::request(&scope, &monitor, &game).unwrap();
    let controller = control::Controller::for_session(&scope, &monitor, &game).unwrap();
    let path = request_file(&scope);
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut unknown = original.clone();
    unknown["command"] = "stop-any-pid".into();
    let mut old = original.clone();
    old["schema_version"] = 0.into();
    for bytes in [
        b"malformed".to_vec(),
        serde_json::to_vec(&unknown).unwrap(),
        serde_json::to_vec(&old).unwrap(),
        vec![b'x'; 1025],
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(controller.stop_requested().is_err());
        assert!(control::request(&scope, &monitor, &game).is_err());
        assert_eq!(bytes, fs::read(&path).unwrap());
    }
}
#[test]
fn replaced_private_scope_is_rejected_and_outside_remains_untouched() {
    let fixture = Fixture::new();
    let scope = fixture.scope();
    let (monitor, game) = pair();
    let controller = control::Controller::for_session(&scope, &monitor, &game).unwrap();
    let old = scope.path();
    let retained = old.with_file_name("held-game-monitor");
    fs::rename(&old, &retained).unwrap();
    Dir::absolute(old.parent().unwrap())
        .unwrap()
        .create("game-monitor")
        .unwrap();
    assert!(controller.stop_requested().is_err());
    assert!(control::request(&scope, &monitor, &game).is_err());
    assert_eq!(fs::read_dir(old).unwrap().count(), 0);
    assert_eq!(fs::read_dir(retained).unwrap().count(), 0);
}
#[test]
fn absent_or_wrong_scope_reads_create_no_state() {
    let fixture = Fixture::new();
    let scope = Scope::open(&fixture.project, "game-monitor").unwrap();
    let (monitor, game) = pair();
    assert!(control::Controller::for_session(&scope, &monitor, &game).is_err());
    assert!(control::request(&scope, &monitor, &game).is_err());
    assert!(!fixture.project.join(".pcl-rust").exists());
    let scope = Scope::create(&fixture.project, "other-service").unwrap();
    let before = fs::read_dir(scope.path()).unwrap().count();
    assert!(control::Controller::for_session(&scope, &monitor, &game).is_err());
    assert!(control::request(&scope, &monitor, &game).is_err());
    assert_eq!(before, fs::read_dir(scope.path()).unwrap().count());
}
#[test]
fn request_hardlink_alias_is_refused_and_foreign_bytes_are_preserved() {
    let fixture = Fixture::new();
    let scope = fixture.scope();
    let (monitor, game) = pair();
    control::request(&scope, &monitor, &game).unwrap();
    let controller = control::Controller::for_session(&scope, &monitor, &game).unwrap();
    let path = request_file(&scope);
    let foreign = fixture.project.join("foreign");
    fs::write(&foreign, b"preserve foreign").unwrap();
    fs::remove_file(&path).unwrap();
    fs::hard_link(&foreign, &path).unwrap();
    assert!(controller.stop_requested().is_err());
    assert!(control::request(&scope, &monitor, &game).is_err());
    assert_eq!(fs::read(&foreign).unwrap(), b"preserve foreign");
}
#[cfg(unix)]
#[test]
fn symlink_request_never_reads_or_changes_its_target() {
    let fixture = Fixture::new();
    let scope = fixture.scope();
    let (monitor, game) = pair();
    control::request(&scope, &monitor, &game).unwrap();
    let controller = control::Controller::for_session(&scope, &monitor, &game).unwrap();
    let path = request_file(&scope);
    let foreign = fixture.project.join("foreign");
    fs::write(&foreign, b"preserve foreign").unwrap();
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&foreign, &path).unwrap();
    assert!(controller.stop_requested().is_err());
    assert!(control::request(&scope, &monitor, &game).is_err());
    assert_eq!(fs::read(&foreign).unwrap(), b"preserve foreign");
}
