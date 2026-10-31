//! Only actual host modules, with trusted isolated synthetic pipe children.
#[path = "host_files.rs"]
mod host_files;
#[path = "host_io.rs"]
mod host_io;
#[path = "host_process.rs"]
mod host_process;
#[path = "host_session.rs"]
mod host_session;

#[cfg(test)]
mod tests {
    use super::{
        host_files::Lease,
        host_io::Channel,
        host_process::{self, Lifecycle, Process},
        host_session::Session,
    };
    use serde_json::json;
    use std::{
        fs,
        path::PathBuf,
        process::{Command, Stdio},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };
    struct Fixture {
        root: PathBuf,
        _temporary: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().canonicalize().unwrap();
            fs::create_dir(root.join("experimental")).unwrap();
            fs::write(
                root.join("experimental/host.mjs"),
                b"// isolated trusted fixture\n",
            )
            .unwrap();
            Self {
                root,
                _temporary: temporary,
            }
        }
        fn lease(&self) -> Arc<Lease> {
            Arc::new(Lease::open(&self.root).unwrap())
        }
    }
    fn lifecycle() -> Lifecycle {
        Lifecycle {
            retiring: Arc::new(AtomicUsize::new(0)),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }
    fn child(mode: &str) -> Command {
        #[cfg(unix)]
        {
            let script = match mode {
                "reply" => {
                    r#"while IFS= read -r request; do printf '{"ok":true,"result":true}\n'; case "$request" in *'"operation":"shutdown"'*) exit 0;; esac; done"#
                }
                "slow" => {
                    r#"IFS= read -r request; printf '{"ok":true,"result":true}\n'; IFS= read -r request; while :; do printf x; sleep 0.04; done"#
                }
                "large" => {
                    r#"IFS= read -r request; i=0; while [ "$i" -lt 5000 ]; do printf x; i=$((i+1)); done; printf '\n'; exec /bin/sleep 5"#
                }
                "eof" => "IFS= read -r request; exit 0",
                _ => "exec /bin/sleep 10",
            };
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(script)
                .env_clear()
                .env("PATH", "/usr/bin:/bin");
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            command
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            use windows_sys::Win32::{
                System::Com::CoTaskMemFree,
                UI::Shell::{FOLDERID_Windows, SHGetKnownFolderPath},
            };
            let mut raw = std::ptr::null_mut();
            assert!(
                unsafe {
                    SHGetKnownFolderPath(&FOLDERID_Windows, 0, std::ptr::null_mut(), &mut raw)
                } >= 0
            );
            let mut length = 0;
            while length < 32768 && unsafe { *raw.add(length) } != 0 {
                length += 1;
            }
            assert!(length < 32768);
            let root = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
                std::slice::from_raw_parts(raw, length)
            }));
            unsafe { CoTaskMemFree(raw.cast()) };
            let script = match mode {
                "reply" => {
                    r#"while($null -ne ($line=[Console]::ReadLine())) { [Console]::WriteLine('{"ok":true,"result":true}'); if($line.Contains('"operation":"shutdown"')) { break } }"#
                }
                "slow" => {
                    r#"$line=[Console]::ReadLine(); [Console]::WriteLine('{"ok":true,"result":true}'); $line=[Console]::ReadLine(); while($true) { [Console]::Write('x'); Start-Sleep -Milliseconds 40 }"#
                }
                "large" => {
                    r#"$line=[Console]::ReadLine(); [Console]::WriteLine(('x' * 5000)); Start-Sleep -Seconds 5"#
                }
                "eof" => "$line=[Console]::ReadLine(); exit 0",
                _ => "Start-Sleep -Seconds 10",
            };
            let mut command =
                Command::new(root.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ]);
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            command
        }
    }
    #[test]
    fn private_lock_excludes_another_live_host_and_releases_without_repair() {
        let fixture = Fixture::new();
        let lease = fixture.lease();
        assert!(Lease::open(&fixture.root).is_err());
        lease.recheck().unwrap();
        drop(lease);
        Lease::open(&fixture.root).unwrap().recheck().unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn safe_existing_0755_app_container_creates_private_host_data_without_chmod() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let fixture = Fixture::new();
        let app = fixture.root.join(".pcl-rust");
        let data = app.join("experimental");
        fs::create_dir(&app).unwrap();
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!data.exists());
        let lease = fixture.lease();
        lease.recheck().unwrap();
        assert_eq!(fs::metadata(&app).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::metadata(&data).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(data.join("host.lock")).unwrap().mode() & 0o777,
            0o600
        );
    }
    #[cfg(unix)]
    #[test]
    fn foreign_writable_app_container_is_retained_without_creating_host_data() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let fixture = Fixture::new();
        let app = fixture.root.join(".pcl-rust");
        fs::create_dir(&app).unwrap();
        for mode in [0o775, 0o777] {
            fs::set_permissions(&app, fs::Permissions::from_mode(mode)).unwrap();
            assert!(Lease::open(&fixture.root).is_err());
            assert!(!app.join("experimental").exists());
            assert_eq!(fs::metadata(&app).unwrap().mode() & 0o777, mode);
        }
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
        let lease = fixture.lease();
        fs::set_permissions(&app, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(lease.recheck().is_err());
        assert_eq!(fs::metadata(&app).unwrap().mode() & 0o777, 0o777);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn public_existing_host_data_stays_rejected_even_inside_a_safe_app_container() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let fixture = Fixture::new();
        let app = fixture.root.join(".pcl-rust");
        let data = app.join("experimental");
        fs::create_dir_all(&data).unwrap();
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Lease::open(&fixture.root).is_err());
        assert!(!data.join("host.lock").exists());
        assert_eq!(fs::metadata(&app).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::metadata(&data).unwrap().mode() & 0o777, 0o755);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn existing_linux_0755_data_directories_keep_their_original_modes() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let fixture = Fixture::new();
        let app = fixture.root.join(".pcl-rust");
        let data = app.join("experimental");
        fs::create_dir_all(&data).unwrap();
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
        fixture.lease().recheck().unwrap();
        assert_eq!(fs::metadata(&app).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::metadata(&data).unwrap().mode() & 0o777, 0o755);
        assert_eq!(
            fs::metadata(data.join("host.lock")).unwrap().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn session_uses_actual_fixed_wire_and_preserves_a_rejected_request_owner() {
        let fixture = Fixture::new();
        let lifecycle = lifecycle();
        let process = Process::spawn(child("reply"), fixture.lease(), lifecycle.clone()).unwrap();
        let mut session = Session::from_process(process).unwrap();
        assert!(session
            .call("status", json!({"large":"x".repeat(128_000)}))
            .is_err());
        assert!(!session.broken);
        assert_eq!(session.call("status", json!({})).unwrap(), true);
        assert_eq!(
            session
                .call("extensions_settings", json!({"values":"界".repeat(64_000)}))
                .unwrap(),
            true
        );
        assert!(session.retire(true));
        assert_eq!(lifecycle.retiring.load(Ordering::Acquire), 0);
        assert!(lifecycle.stopped.load(Ordering::Acquire));
        Lease::open(&fixture.root).unwrap();
    }
    #[test]
    fn slow_chunks_share_one_absolute_deadline_and_drop_is_bounded() {
        let fixture = Fixture::new();
        let lifecycle = lifecycle();
        let mut process =
            Process::spawn(child("slow"), fixture.lease(), lifecycle.clone()).unwrap();
        let mut channel = Channel::open(&mut process).unwrap();
        let ready = channel
            .request(b"{}", 1024, Instant::now() + Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&ready).unwrap()["ok"],
            true
        );
        let started = Instant::now();
        let error = channel
            .request(b"{}", 1_000_000, started + Duration::from_millis(150))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        let completed = process.completion();
        let dropping = Instant::now();
        drop(channel);
        drop(process);
        assert!(dropping.elapsed() < Duration::from_millis(150));
        assert!(host_process::wait_retired(
            &completed,
            Instant::now() + Duration::from_secs(3)
        ));
        Lease::open(&fixture.root).unwrap();
    }
    #[test]
    fn reply_limit_failure_retires_the_actual_pipe_workers_and_child() {
        let fixture = Fixture::new();
        let mut process = Process::spawn(child("large"), fixture.lease(), lifecycle()).unwrap();
        let mut channel = Channel::open(&mut process).unwrap();
        let error = channel
            .request(b"{}", 128, Instant::now() + Duration::from_secs(5))
            .unwrap_err();
        assert!(error.to_string().contains("bound"), "{error}");
        let completed = process.completion();
        drop(channel);
        drop(process);
        assert!(host_process::wait_retired(
            &completed,
            Instant::now() + Duration::from_secs(3)
        ));
        Lease::open(&fixture.root).unwrap();
    }
    #[test]
    fn unread_request_has_a_write_deadline_and_eof_marks_the_session_broken() {
        let fixture = Fixture::new();
        let mut process = Process::spawn(child("idle"), fixture.lease(), lifecycle()).unwrap();
        let mut channel = Channel::open(&mut process).unwrap();
        let started = Instant::now();
        let error = channel
            .request(
                &vec![b'x'; 4 * 1024 * 1024],
                128,
                started + Duration::from_millis(150),
            )
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        let completed = process.completion();
        drop(channel);
        drop(process);
        assert!(host_process::wait_retired(
            &completed,
            Instant::now() + Duration::from_secs(3)
        ));
        let process = Process::spawn(child("eof"), fixture.lease(), lifecycle()).unwrap();
        let mut session = Session::from_process(process).unwrap();
        assert!(session.call("status", json!({})).is_err());
        assert!(session.broken);
        assert!(session.retire(false));
        Lease::open(&fixture.root).unwrap();
    }
    #[test]
    fn lease_and_stopped_wait_for_actual_pipe_close_after_child_exit() {
        let fixture = Fixture::new();
        let lifecycle = lifecycle();
        let mut process =
            Process::spawn(child("idle"), fixture.lease(), lifecycle.clone()).unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        process.track_pipe(closed.clone());
        let completed = process.completion();
        drop(process);
        assert!(!host_process::wait_retired(
            &completed,
            Instant::now() + Duration::from_millis(100)
        ));
        assert_eq!(lifecycle.retiring.load(Ordering::Acquire), 1);
        assert!(!lifecycle.stopped.load(Ordering::Acquire));
        assert!(Lease::open(&fixture.root).is_err());
        closed.store(true, Ordering::Release);
        assert!(host_process::wait_retired(
            &completed,
            Instant::now() + Duration::from_secs(3)
        ));
        assert_eq!(lifecycle.retiring.load(Ordering::Acquire), 0);
        assert!(lifecycle.stopped.load(Ordering::Acquire));
        Lease::open(&fixture.root).unwrap();
    }
    #[test]
    fn host_script_replacement_is_observed_before_a_new_rpc_owner() {
        let fixture = Fixture::new();
        let lease = fixture.lease();
        let script = fixture.root.join("experimental/host.mjs");
        fs::rename(&script, fixture.root.join("experimental/previous.mjs")).unwrap();
        fs::write(&script, b"// unrelated replacement\n").unwrap();
        assert!(lease.recheck().is_err());
        assert!(fixture.root.join("experimental/previous.mjs").exists());
    }
    #[test]
    fn system_node_candidates_are_fixed_absolute_installation_paths() {
        let (paths, _) = host_process::candidates().unwrap();
        assert!(!paths.is_empty());
        assert!(paths.iter().all(|path| path.is_absolute()));
        #[cfg(target_os = "linux")]
        assert_eq!(paths, [PathBuf::from("/usr/bin/node")]);
        #[cfg(target_os = "macos")]
        assert_eq!(
            paths,
            [
                PathBuf::from("/opt/homebrew/bin/node"),
                PathBuf::from("/usr/local/bin/node")
            ]
        );
        #[cfg(windows)]
        assert!(paths.iter().all(|path| path.ends_with("nodejs/node.exe")));
    }
    #[test]
    fn node_version_floor_is_exact_and_never_accepts_a_command_string() {
        for value in [b"v22.19.0\n".as_slice(), b"v24.0.1\r\n"] {
            assert!(host_process::supported_version(value));
        }
        for value in [
            b"v22.18.9".as_slice(),
            b"v20.99.0",
            b"v22.19.0;run",
            b"node v24.0.0",
            b"v24.0",
            b"v24.0.0.extra",
        ] {
            assert!(!host_process::supported_version(value));
        }
    }
}
