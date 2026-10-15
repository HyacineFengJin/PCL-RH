use super::state::MonitorStatus;
use super::*;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = crate::launcher_local::filesystem::fixture_path(&format!(
            "monitor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("project")).unwrap();
        fs::create_dir(path.join("root")).unwrap();
        Self(path)
    }
    fn java(&self, text: &str) -> PathBuf {
        let path = self.0.join("fake-java");
        fs::write(&path, format!("#!/bin/sh\n{text}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn request(&self, java: PathBuf) -> protocol::MonitorRequest {
        protocol::MonitorRequest::new(
            self.0.join("project"),
            "root-generic".into(),
            self.0.join("root"),
            "generic-instance".into(),
            java,
            vec!["fake-access-token".into()],
            self.0.join("root"),
            self.0.join("root/.pcl-linux/logs/generic.log"),
            vec!["fake-access-token".into()],
        )
        .unwrap()
    }
    fn helper(&self) -> PathBuf {
        static HELPER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        HELPER.get_or_init(|| {
            let repo = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().find(|path|
                path.join("crates/core/Cargo.toml").is_file()
                    && path.join("apps/desktop/src-tauri/Cargo.toml").is_file()).unwrap();
            let workspace = repo.join("work/launcher-monitor-helper");
            fs::create_dir_all(&workspace).unwrap();
            fs::write(workspace.join("Cargo.toml"),r#"[package]
name = "launcher-monitor-test-helper"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
libc = "0.2"
[[bin]]
name = "launcher-monitor-test-helper"
path = "main.rs"
"#).unwrap();
            fs::copy(repo.join("Cargo.lock"),workspace.join("Cargo.lock")).unwrap();
            fs::write(workspace.join("main.rs"),format!(
                "#[allow(dead_code,unused_imports)]\n#[path = {:?}] mod launcher_local;\n#[allow(dead_code,unused_imports)]\n#[path = {:?}] mod launcher_game_monitor;\nfn main() {{ if let Some(code) = launcher_game_monitor::dispatch_cli() {{ std::process::exit(code); }} std::process::exit(2); }}\n",
                repo.join("apps/desktop/src-tauri/src/launcher_local.rs"),
                repo.join("apps/desktop/src-tauri/src/launcher_game_monitor.rs"))).unwrap();
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(option_env!("CARGO").unwrap_or("cargo"));
            command.args(["build","--offline","--quiet","--manifest-path"])
                .arg(workspace.join("Cargo.toml")).current_dir(&workspace)
                .env("CARGO_TARGET_DIR",workspace.join("target"))
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).process_group(0);
            let mut child = command.spawn().expect("start isolated helper compiler");
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(),"isolated monitor helper must compile offline"); break;
                }
                if Instant::now() >= deadline { process::kill_owned(&mut child); panic!("isolated helper build deadline"); }
                std::thread::sleep(Duration::from_millis(20));
            }
            workspace.join("target/debug/launcher-monitor-test-helper")
        }).clone()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wait_idle(project: &Path) -> MonitorStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = read_status(project).unwrap() {
            if !status.busy {
                return status;
            }
        }
        assert!(Instant::now() < deadline, "supervisor did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn redactor_covers_every_boundary_and_overlapping_prefix() {
    let source = b"before fake-access-token after fake-access-token tail";
    for split in 0..=source.len() {
        let mut redactor =
            output::Redactor::new(vec!["fake-access-token".into(), "fake-access".into()]).unwrap();
        let mut safe = redactor.feed(&source[..split], false);
        safe.extend(redactor.feed(&source[split..], true));
        assert_eq!(safe, b"before [REDACTED] after [REDACTED] tail");
    }
}
#[test]
fn huge_unterminated_lines_are_streamed_with_bounded_pending_and_no_credentials() {
    let f = Fixture::new();
    let path = f.0.join("output.log");
    let log = Arc::new(Mutex::new(fs::File::create(&path).unwrap()));
    let mut bytes = vec![b'x'; 2 * 1024 * 1024 + 8186];
    bytes.extend(b"fake-access-token");
    bytes.extend(vec![b'y'; 2 * 1024 * 1024]);
    output::stream(
        std::io::Cursor::new(bytes),
        log,
        output::Redactor::new(vec!["fake-access-token".into()]).unwrap(),
        None,
    )
    .unwrap();
    let saved = fs::read(path).unwrap();
    assert!(saved.windows(10).any(|window| window == b"[REDACTED]"));
    assert!(!saved
        .windows(17)
        .any(|window| window == b"fake-access-token"));
    assert!(saved.len() > 4 * 1024 * 1024);
    let mut redactor = output::Redactor::new(vec!["a".repeat(16 * 1024)]).unwrap();
    for _ in 0..64 {
        let _ = redactor.feed(&[b'x'; 8192], false);
    }
    assert!(redactor.pending_len() < 16 * 1024);
}
#[test]
fn helper_handoff_continues_logs_and_reopen_detects_owned_root() {
    let f = Fixture::new();
    let java = f.java("printf 'before fake-access-'; sleep 0.05; printf 'token after\\n'; printf 'err fake-access-token\\n' >&2; sleep 0.5");
    assert!(read_status(&f.0.join("project")).unwrap().is_none());
    assert!(!f.0.join("project/.pcl-rust").exists());
    let started = protocol::spawn_with_helper(&f.helper(), f.request(java)).unwrap();
    let status = read_status(&f.0.join("project")).unwrap().unwrap();
    assert!(status.busy && status.game_running && status.supervisor_running);
    assert_eq!(status.pid, Some(started.pid));
    assert_eq!(status.root_id, "root-generic");
    assert_eq!(status.instance_id.as_deref(), Some("generic-instance"));
    assert_eq!(status.session_id, started.session_id);
    let marker =
        fs::read_to_string(f.0.join("project/.pcl-rust/game-monitor/active.json")).unwrap();
    assert!(!marker.contains("fake-access-token"));
    assert!(!marker.contains("redactions") && !marker.contains("args"));
    let cmdline = fs::read(format!("/proc/{}/cmdline", started.monitor_pid)).unwrap();
    assert!(!cmdline
        .windows(17)
        .any(|window| window == b"fake-access-token"));
    let ended = wait_idle(&f.0.join("project"));
    assert_eq!(ended.exit_code, Some(0));
    assert_eq!(ended.session_id, started.session_id);
    assert_eq!(ended.instance_id, status.instance_id);
    let log = fs::read_to_string(started.log_path).unwrap();
    assert!(!log.contains("fake-access-token"));
    assert!(log.contains("before [REDACTED] after\n"));
    assert!(log.contains("err [REDACTED]\n"));
}
#[test]
fn stop_requires_fresh_revision_and_process_identity() {
    let f = Fixture::new();
    let java = f.java("exec sleep 3");
    protocol::spawn_with_helper(&f.helper(), f.request(java)).unwrap();
    let status = read_status(&f.0.join("project")).unwrap().unwrap();
    assert!(stop_monitor(&f.0.join("project"), "stale").is_err());
    assert!(
        read_status(&f.0.join("project"))
            .unwrap()
            .unwrap()
            .game_running
    );
    stop_monitor(&f.0.join("project"), &status.revision).unwrap();
    let ended = wait_idle(&f.0.join("project"));
    assert!(!ended.game_running);
    let mut wrong = process::identity(std::process::id()).unwrap().unwrap();
    wrong.start_time += 1;
    assert!(process::signal(&wrong, libc::SIGKILL).is_err());
}
#[test]
fn duplicate_handoff_and_unknown_markers_are_retained() {
    let f = Fixture::new();
    let java = f.java("exec sleep 3");
    protocol::spawn_with_helper(&f.helper(), f.request(java.clone())).unwrap();
    assert!(protocol::spawn_with_helper(&f.helper(), f.request(java)).is_err());
    let status = read_status(&f.0.join("project")).unwrap().unwrap();
    stop_monitor(&f.0.join("project"), &status.revision).unwrap();
    wait_idle(&f.0.join("project"));
    let marker = f.0.join("project/.pcl-rust/game-monitor/active.json");
    fs::write(&marker, b"unrecognized private data").unwrap();
    assert!(read_status(&f.0.join("project")).is_err());
    assert!(protocol::spawn_with_helper(&f.helper(), f.request(f.java("exit 0"))).is_err());
    assert_eq!(fs::read(marker).unwrap(), b"unrecognized private data");
}
#[test]
fn arbitrary_log_path_and_symlink_log_refuse_before_game_execution() {
    let f = Fixture::new();
    let java = f.java("touch must-not-run; exec sleep 1");
    let mut request = f.request(java.clone());
    request.log_path = f.0.join("project/.pcl-rust/accounts.json");
    assert!(protocol::spawn_with_helper(&f.helper(), request).is_err());
    fs::create_dir_all(f.0.join("root/.pcl-linux/logs")).unwrap();
    let retained = f.0.join("retained");
    fs::write(&retained, b"keep").unwrap();
    std::os::unix::fs::symlink(&retained, f.0.join("root/.pcl-linux/logs/generic.log")).unwrap();
    assert!(protocol::spawn_with_helper(&f.helper(), f.request(java)).is_err());
    assert_eq!(fs::read(retained).unwrap(), b"keep");
    assert!(!f.0.join("root/must-not-run").exists());
}

fn wire(request: protocol::MonitorRequest) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"schema_version":2,"project":request.project,
        "root_id":request.root_id,"root_path":request.root_path,"java":request.java,
        "instance_id":request.instance_id,
        "args":request.args,"game_dir":request.game_dir,"log_path":request.log_path,
        "redactions":request.redactions}),
    )
    .unwrap()
}
#[test]
fn pipe_eof_before_commit_kills_unconfirmed_game_and_releases_marker() {
    use std::io::{BufRead, Write};
    let f = Fixture::new();
    let java = f.java("exec sleep 3");
    let mut helper = std::process::Command::new(f.helper())
        .arg("--game-monitor")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut input = helper.stdin.take().unwrap();
    let bytes = wire(f.request(java));
    input
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    input.write_all(&bytes).unwrap();
    let mut reply = String::new();
    std::io::BufReader::new(helper.stdout.take().unwrap())
        .read_line(&mut reply)
        .unwrap();
    let pid = reply.split_whitespace().nth(1).unwrap().parse().unwrap();
    let identity = process::identity(pid).unwrap().unwrap();
    drop(input);
    assert!(!helper.wait().unwrap().success());
    assert!(!process::alive(&identity).unwrap());
    assert!(!read_status(&f.0.join("project")).unwrap().unwrap().busy);
}
#[test]
fn stalled_helper_input_is_deadline_bounded_and_cleaned_up() {
    let f = Fixture::new();
    let helper = f.0.join("stalled-helper");
    fs::write(&helper, "#!/bin/sh\nexec sleep 5\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut request = f.request(f.java("exit 0"));
    request.args = vec!["x".repeat(900 * 1024)];
    let before = Instant::now();
    assert!(protocol::spawn_test_deadline(&helper, request, Duration::from_millis(100)).is_err());
    assert!(before.elapsed() < Duration::from_secs(2));
    assert!(!f.0.join("project/.pcl-rust").exists());
}
#[test]
fn output_limit_drains_without_overwriting_or_growing_existing_log() {
    let f = Fixture::new();
    let path = f.0.join("output.log");
    let file = fs::File::create(&path).unwrap();
    file.set_len(output::LOG_LIMIT - 3).unwrap();
    let log = Arc::new(Mutex::new(file));
    output::stream(
        std::io::Cursor::new(vec![b'x'; 1024 * 1024]),
        log,
        output::Redactor::new(vec![]).unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(fs::metadata(path).unwrap().len(), output::LOG_LIMIT - 3);
}
#[test]
fn invalid_utf8_and_split_multibyte_text_are_normalized_with_bounded_frames() {
    let f = Fixture::new();
    let path = f.0.join("unicode.log");
    let log = Arc::new(Mutex::new(fs::File::create(&path).unwrap()));
    let mut bytes = vec![b'x'; 8191];
    bytes.extend("安全".as_bytes());
    bytes.push(255);
    bytes.extend("文本".repeat(12000).as_bytes());
    let expected = String::from_utf8_lossy(&bytes).into_owned();
    output::stream(
        std::io::Cursor::new(bytes),
        log,
        output::Redactor::new(vec![]).unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), expected);
}
#[test]
fn marker_symlink_is_retained_and_log_history_is_appended() {
    let f = Fixture::new();
    let marker_scope =
        crate::launcher_local::filesystem::Scope::create(&f.0.join("project"), "game-monitor")
            .unwrap();
    let retained = f.0.join("retained.json");
    fs::write(&retained, b"keep private").unwrap();
    std::os::unix::fs::symlink(&retained, marker_scope.path().join("active.json")).unwrap();
    assert!(read_status(&f.0.join("project")).is_err());
    assert!(protocol::spawn_with_helper(&f.helper(), f.request(f.java("exit 0"))).is_err());
    assert_eq!(fs::read(retained).unwrap(), b"keep private");
    fs::remove_file(marker_scope.path().join("active.json")).unwrap();
    let log = f.0.join("root/.pcl-linux/logs/generic.log");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(&log, b"prior history\n").unwrap();
    protocol::spawn_with_helper(
        &f.helper(),
        f.request(f.java("printf 'fake-access-token\\n'; sleep 0.2")),
    )
    .unwrap();
    wait_idle(&f.0.join("project"));
    assert_eq!(fs::read(log).unwrap(), b"prior history\n[REDACTED]\n");
}

#[test]
fn linked_log_and_symlink_working_directory_refuse_game_execution() {
    let f = Fixture::new();
    let java = f.java("touch must-not-run; exec sleep 1");
    let log = f.0.join("root/.pcl-linux/logs/generic.log");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    let retained = f.0.join("retained.log");
    fs::write(&retained, b"retain original").unwrap();
    fs::hard_link(&retained, &log).unwrap();
    assert!(protocol::spawn_with_helper(&f.helper(), f.request(java.clone())).is_err());
    assert_eq!(fs::read(&retained).unwrap(), b"retain original");
    fs::remove_file(log).unwrap();
    let external = f.0.join("project/external");
    fs::create_dir(&external).unwrap();
    let linked = f.0.join("root/linked");
    std::os::unix::fs::symlink(&external, &linked).unwrap();
    let mut request = f.request(java);
    request.game_dir = linked;
    assert!(protocol::spawn_with_helper(&f.helper(), request).is_err());
    assert!(!external.join("must-not-run").exists());
    assert!(!f.0.join("root/must-not-run").exists());
}
#[test]
fn oversized_or_unknown_wire_requests_exit_without_creating_marker() {
    use std::io::Write;
    let f = Fixture::new();
    for body in [
        None,
        Some(br#"{"schema_version":1,"token":"fake"}"#.to_vec()),
    ] {
        let mut helper = std::process::Command::new(f.helper())
            .arg("--game-monitor")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut input = helper.stdin.take().unwrap();
        let length = body.as_ref().map(Vec::len).unwrap_or(2 * 1024 * 1024 + 1) as u32;
        input.write_all(&length.to_be_bytes()).unwrap();
        if let Some(body) = body {
            input.write_all(&body).unwrap();
        }
        drop(input);
        assert!(!helper.wait().unwrap().success());
        assert!(!f.0.join("project/.pcl-rust").exists());
    }
}

#[test]
fn marker_phase_exchange_during_fd_capture_and_final_recheck_reads_fresh_state() {
    use std::os::unix::process::CommandExt;
    for point in [state::ReadPoint::Opened, state::ReadPoint::BeforeRecheck] {
        let f = Fixture::new();
        let project = f.0.join("project");
        let root = f.0.join("root");
        let mut session = state::Session::start(
            &project,
            "root-generic".into(),
            root.clone(),
            "generic-instance".into(),
            root.join(".pcl-linux/logs/generic.log"),
        )
        .unwrap();
        // Only an independent process identity is needed here. A stable system
        // executable avoids ETXTBSY when parallel forks inherit a fixture writer
        // FD while another test creates its temporary script.
        let mut child = std::process::Command::new("/usr/bin/sleep")
            .arg("3")
            .process_group(0)
            .spawn()
            .unwrap();
        session.spawned(child.id()).unwrap();
        let running = read_status(&project).unwrap().unwrap();
        let mut exchanged = false;
        let ended = state::read_status_observed(&project, &mut |reached| {
            if reached == point && !exchanged {
                exchanged = true;
                process::kill_owned(&mut child);
                session.finished(true, Some(0)).unwrap();
            }
        })
        .unwrap()
        .unwrap();
        assert!(exchanged);
        assert_eq!(ended.phase, "exited");
        assert_eq!(ended.exit_code, Some(0));
        assert_eq!(ended.session_id, running.session_id);
        assert_eq!(ended.pid, running.pid);
        assert_eq!(ended.root_id, running.root_id);
        assert_eq!(ended.log_path, running.log_path);
        assert_ne!(ended.revision, running.revision);
        assert!(!ended.game_running);
    }
}

#[test]
fn marker_retry_does_not_hide_inplace_unknown_different_session_or_binding_edits() {
    use std::os::unix::process::CommandExt;
    for case in 0..7 {
        let f = Fixture::new();
        let project = f.0.join("project");
        let root = f.0.join("root");
        let _session = state::Session::start(
            &project,
            "root-generic".into(),
            root.clone(),
            "generic-instance".into(),
            root.join(".pcl-linux/logs/generic.log"),
        )
        .unwrap();
        let path = project.join(".pcl-rust/game-monitor/active.json");
        let mut replacement: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        replacement["phase"] = "failed".into();
        let mut other = std::process::Command::new("/usr/bin/sleep")
            .arg("3")
            .process_group(0)
            .spawn()
            .unwrap();
        if case == 2 {
            replacement["monitor"] =
                serde_json::to_value(process::identity(other.id()).unwrap().unwrap()).unwrap();
        }
        if case == 3 {
            replacement["root_id"] = "different-root".into();
        }
        if case == 4 {
            replacement["phase"] = "starting".into();
        }
        if case == 5 {
            replacement["instance_id"] = "different-instance".into();
        }
        if case == 6 {
            replacement["schema_version"] = serde_json::json!(1);
            replacement.as_object_mut().unwrap().remove("instance_id");
        }
        let bytes = if case == 1 {
            b"unknown retained content".to_vec()
        } else {
            serde_json::to_vec(&replacement).unwrap()
        };
        let mut changed = false;
        let result = state::read_status_observed(&project, &mut |point| {
            if point == state::ReadPoint::BeforeRecheck && !changed {
                changed = true;
                if case == 0 {
                    // Same-inode edits can resemble a valid phase transition;
                    // only the helper's fresh-inode publication may be retried.
                    fs::write(&path, &bytes).unwrap();
                } else {
                    let stage = path.with_file_name("foreign-stage.json");
                    fs::write(&stage, &bytes).unwrap();
                    fs::rename(stage, &path).unwrap();
                }
            }
        });
        process::kill_owned(&mut other);
        assert!(changed && result.is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn marker_absence_under_owned_lifetime_lock_stays_conservatively_busy() {
    let f = Fixture::new();
    let project = f.0.join("project");
    let scope = crate::launcher_local::filesystem::Scope::create(&project, "game-monitor").unwrap();
    let folder = scope.folder.as_ref().unwrap();
    let guard = folder.lock(".monitor.lock").unwrap();
    assert!(read_status(&project).is_err());
    assert!(!scope.path().join("active.json").exists());
    drop(guard);
    assert!(read_status(&project).unwrap().is_none());
}

/// A completed marker copied during a project move may still name the old
/// compatibility alias. Reading it must preserve bytes and all process guards.
fn relocation_marker(f: &Fixture, recorded: &Path, phase: &str) -> Vec<u8> {
    let mut monitor = process::identity(std::process::id()).unwrap().unwrap();
    // Keep the valid boot identity but use absent PIDs: these fixtures never
    // signal or launch a process, and cannot report a false running game.
    monitor.pid = i32::MAX as u32;
    monitor.start_time = 1;
    let mut game = monitor.clone();
    game.pid -= 1;
    serde_json::to_vec(&serde_json::json!({
        "schema_version":1,"project":recorded,"root_id":"root-generic",
        "root_path":f.0.join("root"),"log_path":f.0.join("root/.pcl-linux/logs/generic.log"),
        "monitor":monitor,"game":game,"phase":phase,"started_at":1,"exit_code":0
    }))
    .unwrap()
}
#[test]
fn relocation_alias_accepts_same_project_without_rewriting_marker() {
    let f = Fixture::new();
    let current = f.0.join("project");
    let alias = f.0.join("former-project");
    std::os::unix::fs::symlink(&current, &alias).unwrap();
    let scope = crate::launcher_local::filesystem::Scope::create(&current, "game-monitor").unwrap();
    let marker = scope.path().join("active.json");
    let bytes = relocation_marker(&f, &alias, "exited");
    fs::write(&marker, &bytes).unwrap();
    let status = read_status(&current).unwrap().unwrap();
    assert_eq!(status.phase, "exited");
    assert!(!status.busy);
    assert_eq!(fs::read(marker).unwrap(), bytes);
}
#[test]
fn relocation_alias_keeps_live_process_and_future_schema_guards() {
    let f = Fixture::new();
    let current = f.0.join("project");
    let alias = f.0.join("former-project");
    std::os::unix::fs::symlink(&current, &alias).unwrap();
    let scope = crate::launcher_local::filesystem::Scope::create(&current, "game-monitor").unwrap();
    let marker = scope.path().join("active.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&relocation_marker(&f, &alias, "running")).unwrap();
    value["monitor"] =
        serde_json::to_value(process::identity(std::process::id()).unwrap().unwrap()).unwrap();
    fs::write(&marker, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(read_status(&current).unwrap().unwrap().busy);
    value["schema_version"] = serde_json::json!(3);
    let bytes = serde_json::to_vec(&value).unwrap();
    fs::write(&marker, &bytes).unwrap();
    assert!(read_status(&current).is_err());
    assert_eq!(fs::read(marker).unwrap(), bytes);
}
#[test]
fn relocation_alias_to_another_project_or_missing_path_is_rejected() {
    let f = Fixture::new();
    let current = f.0.join("project");
    let scope = crate::launcher_local::filesystem::Scope::create(&current, "game-monitor").unwrap();
    let marker = scope.path().join("active.json");
    let alias = f.0.join("foreign-project");
    std::os::unix::fs::symlink(f.0.join("root"), &alias).unwrap();
    for recorded in [alias, f.0.join("missing-project")] {
        let bytes = relocation_marker(&f, &recorded, "exited");
        fs::write(&marker, &bytes).unwrap();
        assert!(read_status(&current).is_err());
        assert_eq!(fs::read(&marker).unwrap(), bytes);
    }
}

#[test]
fn completed_marker_identity_is_explicit_for_v2_and_absent_for_unmodified_v1() {
    let f = Fixture::new();
    let project = f.0.join("project");
    let scope = crate::launcher_local::filesystem::Scope::create(&project, "game-monitor").unwrap();
    let path = scope.path().join("active.json");
    for instance in [None, Some("captured-instance")] {
        let mut marker: serde_json::Value =
            serde_json::from_slice(&relocation_marker(&f, &project, "failed")).unwrap();
        marker["exit_code"] = 2.into();
        if let Some(instance) = instance {
            marker["schema_version"] = 2.into();
            marker["instance_id"] = instance.into();
        }
        let bytes = serde_json::to_vec(&marker).unwrap();
        fs::write(&path, &bytes).unwrap();
        let status = read_status(&project).unwrap().unwrap();
        assert!(!status.busy);
        assert_eq!(status.phase, "failed");
        assert_eq!(status.instance_id.as_deref(), instance);
        assert_eq!(status.root_path, f.0.join("root").display().to_string());
        assert_eq!(
            status.log_path,
            f.0.join("root/.pcl-linux/logs/generic.log")
                .display()
                .to_string()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn unsupported_or_invalid_explicit_identity_is_retained_and_never_replaced_on_start() {
    let f = Fixture::new();
    let project = f.0.join("project");
    let root = f.0.join("root");
    let scope = crate::launcher_local::filesystem::Scope::create(&project, "game-monitor").unwrap();
    let path = scope.path().join("active.json");
    for (schema, instance) in [
        (2, None),
        (2, Some("")),
        (2, Some("..")),
        (2, Some("../foreign")),
        (2, Some("control\ncharacter")),
        (1, Some("unrecognized-extension")),
        (3, Some("future-instance")),
    ] {
        let mut marker: serde_json::Value =
            serde_json::from_slice(&relocation_marker(&f, &project, "failed")).unwrap();
        marker["schema_version"] = schema.into();
        if let Some(instance) = instance {
            marker["instance_id"] = instance.into();
        }
        let bytes = serde_json::to_vec(&marker).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(
            read_status(&project).is_err(),
            "schema {schema}, instance {instance:?}"
        );
        assert!(state::Session::start(
            &project,
            "root-generic".into(),
            root.clone(),
            "new-instance".into(),
            root.join(".pcl-linux/logs/new.log")
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn invalid_request_instance_refuses_before_any_helper_or_marker_is_created() {
    let f = Fixture::new();
    for instance in ["", "..", "../foreign", "control\ncharacter"] {
        let mut request = f.request(f.0.join("never-executed-java"));
        request.instance_id = instance.into();
        assert!(protocol::spawn_with_helper(&f.0.join("never-executed-helper"), request).is_err());
        assert!(!f.0.join("project/.pcl-rust").exists());
    }
}

#[test]
fn admitted_retirement_persists_without_inventing_legacy_identity_or_removing_the_log() {
    for instance in [None, Some("captured-instance")] {
        let f = Fixture::new();
        let project = f.0.join("project");
        let scope =
            crate::launcher_local::filesystem::Scope::create(&project, "game-monitor").unwrap();
        let path = scope.path().join("active.json");
        let mut marker: serde_json::Value =
            serde_json::from_slice(&relocation_marker(&f, &project, "failed")).unwrap();
        if let Some(instance) = instance {
            marker["schema_version"] = 2.into();
            marker["instance_id"] = instance.into();
        }
        fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
        let log = f.0.join("root/.pcl-linux/logs/generic.log");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, b"retained old log").unwrap();
        let previous = read_status(&project).unwrap().unwrap();
        retire_completed(&project, &previous.revision).unwrap();
        let retired = read_status(&project).unwrap().unwrap();
        assert_eq!(retired.phase, "retired");
        assert!(!retired.busy);
        assert_eq!(retired.session_id, previous.session_id);
        assert_eq!(retired.instance_id.as_deref(), instance);
        assert_eq!(retired.root_id, previous.root_id);
        assert_eq!(retired.root_path, previous.root_path);
        assert_eq!(retired.log_path, previous.log_path);
        assert_eq!(retired.exit_code, previous.exit_code);
        assert_ne!(retired.revision, previous.revision);
        assert_eq!(fs::read(&log).unwrap(), b"retained old log");
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], 2);
        assert_eq!(
            saved.get("instance_id").and_then(serde_json::Value::as_str),
            instance
        );
        // A repeated retirement is read-only, and a future admitted helper can
        // replace the tombstone with its own complete explicit identity.
        retire_completed(&project, &retired.revision).unwrap();
        assert_eq!(
            read_status(&project).unwrap().unwrap().revision,
            retired.revision
        );
        let _session = state::Session::start(
            &project,
            "root-generic".into(),
            f.0.join("root"),
            "next-instance".into(),
            f.0.join("root/.pcl-linux/logs/next.log"),
        )
        .unwrap();
        assert_eq!(
            read_status(&project)
                .unwrap()
                .unwrap()
                .instance_id
                .as_deref(),
            Some("next-instance")
        );
        assert_eq!(fs::read(&log).unwrap(), b"retained old log");
    }
}

#[test]
fn retirement_rejects_stale_live_unknown_and_lifetime_owned_records_without_touching_bytes() {
    let f = Fixture::new();
    let project = f.0.join("project");
    let scope = crate::launcher_local::filesystem::Scope::create(&project, "game-monitor").unwrap();
    let path = scope.path().join("active.json");
    let completed = relocation_marker(&f, &project, "failed");
    fs::write(&path, &completed).unwrap();
    let previous = read_status(&project).unwrap().unwrap();
    assert!(retire_completed(&project, "stale").is_err());
    assert_eq!(fs::read(&path).unwrap(), completed);
    {
        let _guard = scope
            .folder
            .as_ref()
            .unwrap()
            .lock(".monitor.lock")
            .unwrap();
        assert!(retire_completed(&project, &previous.revision).is_err());
        assert_eq!(fs::read(&path).unwrap(), completed);
    }
    let mut live: serde_json::Value = serde_json::from_slice(&completed).unwrap();
    live["monitor"] =
        serde_json::to_value(process::identity(std::process::id()).unwrap().unwrap()).unwrap();
    let live = serde_json::to_vec(&live).unwrap();
    fs::write(&path, &live).unwrap();
    let observed = read_status(&project).unwrap().unwrap();
    assert!(observed.busy);
    assert!(retire_completed(&project, &observed.revision).is_err());
    assert_eq!(fs::read(&path).unwrap(), live);
    let unknown = b"unknown retained format";
    fs::write(&path, unknown).unwrap();
    assert!(retire_completed(&project, &previous.revision).is_err());
    assert_eq!(fs::read(&path).unwrap(), unknown);
}

#[test]
fn retirement_publication_failure_and_external_replacement_leave_original_or_competing_bytes() {
    for replace in [false, true] {
        let f = Fixture::new();
        let project = f.0.join("project");
        let scope =
            crate::launcher_local::filesystem::Scope::create(&project, "game-monitor").unwrap();
        let path = scope.path().join("active.json");
        let original = relocation_marker(&f, &project, "failed");
        fs::write(&path, &original).unwrap();
        let previous = read_status(&project).unwrap().unwrap();
        let competing = b"external retained bytes";
        let result = state::retire_completed_observed(&project, &previous.revision, || {
            assert!(
                scope
                    .folder
                    .as_ref()
                    .unwrap()
                    .lock(".monitor.lock")
                    .is_err(),
                "publication must still own the helper lifetime lock"
            );
            if replace {
                let staged = path.with_file_name("external.json");
                fs::write(&staged, competing).unwrap();
                fs::rename(staged, &path).unwrap();
                Ok(())
            } else {
                Err("synthetic publication failure".into())
            }
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            if replace {
                competing.to_vec()
            } else {
                original
            }
        );
    }
}

#[test]
fn retirement_exchange_during_marker_read_is_a_recognized_identity_preserving_transition() {
    for instance in [None, Some("captured-instance")] {
        let f = Fixture::new();
        let project = f.0.join("project");
        let scope =
            crate::launcher_local::filesystem::Scope::create(&project, "game-monitor").unwrap();
        let path = scope.path().join("active.json");
        let mut marker: serde_json::Value =
            serde_json::from_slice(&relocation_marker(&f, &project, "failed")).unwrap();
        if let Some(instance) = instance {
            marker["schema_version"] = 2.into();
            marker["instance_id"] = instance.into();
        }
        fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
        let previous = read_status(&project).unwrap().unwrap();
        let mut exchanged = false;
        let current = state::read_status_observed(&project, &mut |point| {
            if point == state::ReadPoint::Opened && !exchanged {
                exchanged = true;
                retire_completed(&project, &previous.revision).unwrap();
            }
        })
        .unwrap()
        .unwrap();
        assert!(exchanged);
        assert_eq!(current.phase, "retired");
        assert_eq!(current.instance_id.as_deref(), instance);
        assert_eq!(current.session_id, previous.session_id);
    }
}
