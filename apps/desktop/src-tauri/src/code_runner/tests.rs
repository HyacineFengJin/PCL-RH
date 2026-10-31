//! Synthetic-only native tests. The Gradle wrapper fixture is a locally built
//! Java program, not a downloaded distribution or a user's project/build.
use super::*;
use std::{
    cell::Cell,
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{symlink, PermissionsExt},
    },
    process::Command,
    time::Instant,
};

struct Fixture(PathBuf, #[allow(dead_code)] tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("attempt");
        fs::create_dir_all(root.join("source")).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(root).unwrap(), temporary)
    }
    fn request(&self, recipe: Recipe, target: Target) -> Request {
        Request {
            schema_version: 1,
            run_id: "12345678-1234-4234-8234-123456789abc".into(),
            root: self.0.clone(),
            recipe,
            target,
            network: false,
            timeout_ms: 10_000,
            max_output_bytes: 64 * 1024,
        }
    }
    fn java(&self, name: &str, code: &str) {
        fs::write(self.0.join("source").join(name), code).unwrap();
    }
    fn wrapper(&self) {
        let dev = self.0.join("fixture-tools");
        fs::create_dir(&dev).unwrap();
        let source = dev.join("GradleWrapperMain.java");
        let hidden = self.0.join("outside-marker");
        fs::write(&hidden, b"synthetic private marker").unwrap();
        let code = r#"package org.gradle.wrapper;
import java.io.*;
import java.nio.file.*;
public class GradleWrapperMain {
  public static void main(String[] args) throws Exception {
    if (System.getenv("PCL_RUNNER_SECRET") != null || System.getenv("DISPLAY") != null) throw new Exception("ambient environment leaked");
    if (!System.getProperty("user.home").equals("/home/build")) throw new Exception("wrong private home");
    if (Files.exists(Path.of("OUTSIDE_MARKER"))) throw new Exception("host attempt parent leaked");
    try (var fds = Files.list(Path.of("/proc/self/fd"))) {
      for (Path fd : fds.toList()) {
        try { if (Files.isDirectory(fd) && Files.exists(fd.resolve("../outside-marker"))) throw new Exception("host directory descriptor leaked"); }
        catch (java.nio.file.NoSuchFileException ignored) {}
      }
    }
    String target = args[args.length - 1];
    if (!args[0].equals("--no-daemon") || !args[1].equals("--console=plain") || !args[2].equals("--offline")) throw new Exception("wrong fixed arguments");
    if (target.equals("classes")) { byte[] bytes = new byte[100000]; java.util.Arrays.fill(bytes, (byte)0xff); System.out.write(bytes); System.out.flush(); }
    else if (target.equals("test")) {
      new ProcessBuilder("/usr/bin/setsid", "/usr/bin/sleep", "60").inheritIO().start();
      System.out.println("DESCENDANT_READY"); System.out.flush(); Thread.sleep(60000);
    } else { System.out.println("PRIVATE_SANDBOX_READY"); System.out.flush(); Thread.sleep(60000); }
  }
}"#.replace("OUTSIDE_MARKER", &hidden.to_string_lossy());
        fs::write(&source, code).unwrap();
        let classes = dev.join("classes");
        fs::create_dir(&classes).unwrap();
        let result = Command::new("/usr/bin/javac")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .args(["-proc:none", "-encoding", "UTF-8", "-d"])
            .arg(&classes)
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "fixture compiler failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let wrapper = self.0.join("source/gradle/wrapper");
        fs::create_dir_all(&wrapper).unwrap();
        let result = Command::new("/usr/bin/jar")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args(["--create", "--file"])
            .arg(wrapper.join("gradle-wrapper.jar"))
            .arg("-C")
            .arg(classes)
            .arg(".")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "fixture jar failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
fn pipe() -> (File, File) {
    let mut descriptors = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    }
}
fn run(
    command: Command,
    input: &mut protocol::Input,
    request: &Request,
    started: impl FnMut(u32) -> Result<(), Failure>,
    output: impl FnMut(&str, &str) -> Result<(), Failure>,
) -> Outcome {
    process::run(
        command,
        input,
        Duration::from_millis(request.timeout_ms),
        request.max_output_bytes,
        started,
        output,
    )
    .unwrap()
}
fn assert_resolved_recipe_tool(command: &Command, tool: &str) {
    let arguments: Vec<_> = command.get_args().collect();
    let separator = arguments
        .iter()
        .rposition(|arg| *arg == std::ffi::OsStr::new("--"))
        .unwrap();
    let expected = fs::canonicalize(tool).unwrap();
    assert!(expected.starts_with("/usr"));
    assert_eq!(arguments[separator + 1], expected.as_os_str());
}
fn descendants(pid: u32) -> Vec<u32> {
    let mut result = Vec::new();
    let mut pending = vec![pid];
    while let Some(parent) = pending.pop() {
        let text = fs::read_to_string(format!("/proc/{parent}/task/{parent}/children"))
            .unwrap_or_default();
        for child in text
            .split_whitespace()
            .filter_map(|v| v.parse::<u32>().ok())
        {
            result.push(child);
            pending.push(child);
        }
    }
    result
}
fn live(pid: u32) -> bool {
    let Ok(text) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    text.rfind(')')
        .and_then(|end| text[end + 1..].split_whitespace().next())
        .is_some_and(|state| !matches!(state, "Z" | "X"))
}

#[test]
fn request_rejects_unknown_fields_targets_and_unbounded_limits() {
    let fixture = Fixture::new();
    let mut request = fixture.request(Recipe::Javac, Target::Classes);
    assert!(request.validate().is_ok());
    request.target = Target::Test;
    assert!(request.validate().is_err());
    request.target = Target::Classes;
    request.max_output_bytes = MAX_OUTPUT + 1;
    assert!(request.validate().is_err());
    request.max_output_bytes = 10;
    request.timeout_ms = MAX_TIMEOUT + 1;
    assert!(request.validate().is_err());
    let bad = json!({"schemaVersion":1,"runId":"12345678-1234-4234-8234-123456789abc","root":fixture.0,"recipe":"javac","target":"classes","network":false,"timeoutMs":100,"maxOutputBytes":100,"executable":"/usr/bin/sh"});
    assert!(serde_json::from_value::<Request>(bad).is_err());
}

#[test]
fn source_rejects_links_hardlinks_and_output_aliases() {
    let fixture = Fixture::new();
    fixture.java("Safe.java", "class Safe {}");
    symlink("Safe.java", fixture.0.join("source/Alias.java")).unwrap();
    assert!(sandbox::Prepared::new(&fixture.request(Recipe::Javac, Target::Classes)).is_err());
    fs::remove_file(fixture.0.join("source/Alias.java")).unwrap();
    fs::hard_link(
        fixture.0.join("source/Safe.java"),
        fixture.0.join("source/Alias.java"),
    )
    .unwrap();
    assert!(sandbox::Prepared::new(&fixture.request(Recipe::Javac, Target::Classes)).is_err());
    fs::remove_file(fixture.0.join("source/Alias.java")).unwrap();
    fs::create_dir(fixture.0.join("source/.rh-build")).unwrap();
    assert!(sandbox::Prepared::new(&fixture.request(Recipe::Javac, Target::Classes)).is_err());
}

#[test]
fn incremental_utf8_handles_boundaries_invalid_bytes_and_terminal_tail() {
    let mut decoder = protocol::Utf8::default();
    assert_eq!(decoder.decode(&[0xe4, 0xbd], false), "");
    assert_eq!(decoder.decode(&[0xa0, b' ', 0xff], false), "你 �");
    assert_eq!(decoder.decode(&[0xf0, 0x9f, 0x98], false), "");
    assert_eq!(decoder.decode(&[0x80], false), "😀");
    assert_eq!(decoder.decode(&[0xe4], false), "");
    assert_eq!(decoder.decode(&[], true), "�");
}

#[test]
fn ndjson_has_identity_and_cancel_or_eof_closes_ownership() {
    let (mut reader, writer_fd) = pipe();
    let mut writer = protocol::Writer::new(writer_fd.as_raw_fd()).unwrap();
    writer.set_run_id("12345678-1234-4234-8234-123456789abc");
    writer
        .event(&json!({"type":"output","stream":"stdout","text":"你\n😀"}))
        .unwrap();
    drop(writer_fd);
    let mut text = String::new();
    reader.read_to_string(&mut text).unwrap();
    assert_eq!(text.lines().count(), 1);
    let event: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(event["schemaVersion"], 1);
    assert_eq!(event["text"], "你\n😀");
    assert_eq!(event["runId"], "12345678-1234-4234-8234-123456789abc");
    let (read, mut write) = pipe();
    let mut input = protocol::Input::new(read.as_raw_fd()).unwrap();
    write.write_all(b"{\"type\":\"cancel\"}\n").unwrap();
    assert!(input.cancelled().unwrap());
    let (read, write) = pipe();
    let mut input = protocol::Input::new(read.as_raw_fd()).unwrap();
    drop(write);
    assert!(input.cancelled().unwrap());
}

#[test]
fn bubblewrap_preflight_and_actual_javac_compile_private_copy() {
    let fixture = Fixture::new();
    fixture.java("Safe.java", "class Safe { String greeting = \"你好😀\"; }");
    fixture.java("@options.java", "class ArgumentFileName {}");
    fixture.java("-options.java", "class OptionName {}");
    let request = fixture.request(Recipe::Javac, Target::Classes);
    let prepared = sandbox::Prepared::new(&request).unwrap();
    assert_resolved_recipe_tool(
        &prepared.command(&request, false).unwrap(),
        "/usr/bin/javac",
    );
    let (read, _write) = pipe();
    let mut input = protocol::Input::new(read.as_raw_fd()).unwrap();
    let mut diagnostics = String::new();
    let probe = run(
        prepared.command(&request, true).unwrap(),
        &mut input,
        &request,
        |_| Ok(()),
        |_, text| {
            diagnostics.push_str(text);
            Ok(())
        },
    );
    assert_eq!(
        probe.status, "completed",
        "sandbox unavailable: {diagnostics}"
    );
    let result = run(
        prepared.command(&request, false).unwrap(),
        &mut input,
        &request,
        |_| Ok(()),
        |_, text| {
            diagnostics.push_str(text);
            Ok(())
        },
    );
    assert_eq!(result.status, "completed", "javac failed: {diagnostics}");
    assert_eq!(result.exit_code, Some(0));
    for name in ["Safe", "ArgumentFileName", "OptionName"] {
        assert!(fixture
            .0
            .join(format!("source/.rh-build/classes/{name}.class"))
            .is_file());
    }
}

#[test]
fn wrapper_is_private_and_output_limit_stops_the_owned_namespace() {
    let fixture = Fixture::new();
    fixture.wrapper();
    let mut request = fixture.request(Recipe::Gradle, Target::Classes);
    request.max_output_bytes = 1024;
    let prepared = sandbox::Prepared::new(&request).unwrap();
    let (read, _write) = pipe();
    let mut input = protocol::Input::new(read.as_raw_fd()).unwrap();
    let mut command = prepared.command(&request, false).unwrap();
    assert_resolved_recipe_tool(&command, "/usr/bin/java");
    command
        .env("PCL_RUNNER_SECRET", "synthetic-only")
        .env("DISPLAY", "synthetic-display");
    let mut text = String::new();
    let pid = Cell::new(0);
    let result = run(
        command,
        &mut input,
        &request,
        |value| {
            pid.set(value);
            Ok(())
        },
        |_, value| {
            text.push_str(value);
            Ok(())
        },
    );
    assert_eq!(
        result.status, "output_limit",
        "private wrapper failed: {text}"
    );
    assert!(text.len() <= 1024);
    assert!(text.contains('\u{fffd}'));
    assert!(!live(pid.get()));
}

#[test]
fn cancellation_kills_descendants_that_change_session_before_reaping() {
    let fixture = Fixture::new();
    fixture.wrapper();
    let request = fixture.request(Recipe::Gradle, Target::Test);
    let prepared = sandbox::Prepared::new(&request).unwrap();
    let (read, mut write) = pipe();
    let mut input = protocol::Input::new(read.as_raw_fd()).unwrap();
    let pid = Cell::new(0);
    let mut children = Vec::new();
    let mut diagnostics = String::new();
    let started_at = Instant::now();
    let result = run(
        prepared.command(&request, false).unwrap(),
        &mut input,
        &request,
        |value| {
            pid.set(value);
            Ok(())
        },
        |_, text| {
            diagnostics.push_str(text);
            if diagnostics.contains("DESCENDANT_READY") && children.is_empty() {
                children = descendants(pid.get());
                write.write_all(b"{\"type\":\"cancel\"}\n").unwrap();
            }
            Ok(())
        },
    );
    assert_eq!(result.status, "cancelled", "wrapper failed: {diagnostics}");
    // The actual killed namespace's wire result must match Node/UI's signal
    // string contract, not Rust's integer ExitStatusExt representation.
    assert_eq!(result.finished()["signal"], "SIGKILL");
    assert!(!children.is_empty());
    assert!(started_at.elapsed() < Duration::from_secs(3));
    assert!(!live(pid.get()));
    for child in children {
        assert!(!live(child), "owned descendant {child} is still alive");
    }
}

#[test]
fn timeout_and_closed_input_complete_without_waiting_for_child_pipes() {
    let fixture = Fixture::new();
    fixture.wrapper();
    let mut request = fixture.request(Recipe::Gradle, Target::Build);
    request.timeout_ms = 150;
    let prepared = sandbox::Prepared::new(&request).unwrap();
    let (read, _write) = pipe();
    let mut input = protocol::Input::new(read.as_raw_fd()).unwrap();
    let started_at = Instant::now();
    let result = run(
        prepared.command(&request, false).unwrap(),
        &mut input,
        &request,
        |_| Ok(()),
        |_, _| Ok(()),
    );
    assert_eq!(result.status, "timed_out");
    assert!(started_at.elapsed() < Duration::from_secs(2));
    let (read, write) = pipe();
    let mut input = protocol::Input::new(read.as_raw_fd()).unwrap();
    drop(write);
    let result = run(
        prepared.command(&request, false).unwrap(),
        &mut input,
        &request,
        |_| panic!("closed owner started execution"),
        |_, _| Ok(()),
    );
    assert_eq!(result.status, "cancelled");
}
