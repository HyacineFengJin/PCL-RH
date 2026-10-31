//! Native temporary inputs and real OS pipes; no author build runs here.
use super::*;
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use std::{
    fs::{self, File},
    io::{Read, Write},
    sync::{Mutex, MutexGuard},
    time::Instant,
};
#[cfg(windows)]
#[path = "../../../../../crates/core/src/file_ops/junction_fixture.rs"]
mod junction_fixture;

// The Windows transport intentionally has a finite process-wide worker budget.
// Serialize these IO fixtures so test-runner parallelism does not consume it.
pub(in crate::code_runner) fn io_fixture() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap()
}
struct Fixture(PathBuf, #[allow(dead_code)] tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let parent = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let root = parent
            .create_private(&Leaf::new("attempt").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        for name in ["source", "home"] {
            root.create_private(&Leaf::new(name).unwrap(), Traversal::SameFilesystem)
                .unwrap();
        }
        Self(
            temporary.path().join("attempt").canonicalize().unwrap(),
            temporary,
        )
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        let path = self.0.join(path);
        let parent = Directory::open_absolute(path.parent().unwrap()).unwrap();
        let mut file = parent
            .create_file(&Leaf::new(path.file_name().unwrap()).unwrap())
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }
    fn request(&self) -> Value {
        json!({"schemaVersion":1,"runId":"12345678-1234-4234-8234-123456789abc",
            "root":self.0,"recipe":"javac","target":"classes","network":false,
            "timeoutMs":100,"maxOutputBytes":100})
    }
}
#[cfg(unix)]
pub(in crate::code_runner) fn pipe() -> (File, File) {
    use std::os::fd::FromRawFd;
    let mut descriptors = [-1; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    for descriptor in descriptors {
        assert_eq!(
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    }
}
#[cfg(windows)]
pub(in crate::code_runner) fn pipe() -> (File, File) {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Pipes::CreatePipe;
    let mut reader = std::ptr::null_mut();
    let mut writer = std::ptr::null_mut();
    assert_ne!(
        unsafe { CreatePipe(&mut reader, &mut writer, std::ptr::null(), 4096) },
        0
    );
    unsafe { (File::from_raw_handle(reader), File::from_raw_handle(writer)) }
}
fn pipe_input(file: &File) -> Input {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        Input::new(file.as_raw_fd()).unwrap()
    }
    #[cfg(windows)]
    {
        Input::new(file.try_clone().unwrap()).unwrap()
    }
}
pub(in crate::code_runner) fn writer(file: &File) -> Writer {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        Writer::new(file.as_raw_fd()).unwrap()
    }
    #[cfg(windows)]
    {
        Writer::new(file.try_clone().unwrap()).unwrap()
    }
}
fn poll_control(input: &mut Input) -> Result<bool, Failure> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match input.cancelled() {
            Ok(false) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            result => return result,
        }
    }
}

#[test]
fn request_schema_limits_and_fixed_recipe_targets_are_portable() {
    let fixture = Fixture::new();
    let valid = fixture.request();
    serde_json::from_value::<Request>(valid.clone())
        .unwrap()
        .validate()
        .unwrap();
    for (field, invalid) in [
        ("schemaVersion", json!(2)),
        ("runId", json!("arbitrary")),
        ("timeoutMs", json!(0)),
        ("timeoutMs", json!(MAX_TIMEOUT + 1)),
        ("maxOutputBytes", json!(0)),
        ("maxOutputBytes", json!(MAX_OUTPUT + 1)),
        ("target", json!("test")),
        ("root", json!("relative/attempt")),
    ] {
        let mut request = valid.clone();
        request[field] = invalid;
        assert!(
            serde_json::from_value::<Request>(request)
                .unwrap()
                .validate()
                .is_err(),
            "{field}"
        );
    }
    let mut unknown = valid;
    unknown["executable"] = json!("arbitrary");
    assert!(serde_json::from_value::<Request>(unknown).is_err());
}

#[test]
fn inventory_reads_source_and_home_with_native_identity_and_rechecks_changes() {
    let fixture = Fixture::new();
    fixture.put("source/@options.java", b"class A {}\n");
    fixture.put("source/-options.java", b"class B {}\n");
    fixture.put("home/cache", b"captured cache");
    let inventory = inputs::Inventory::capture(&fixture.0).unwrap();
    assert_eq!(
        inventory.java_sources,
        ["./-options.java", "./@options.java"]
    );
    assert_eq!(inventory.total_bytes, 36);
    inventory.recheck().unwrap();
    fs::write(fixture.0.join("home/cache"), b"changed cache bytes").unwrap();
    assert!(inventory.recheck().is_err());
    assert!(!fixture.0.join("source/.rh-build").exists());
}

#[test]
fn inventory_refuses_hardlinks_oversize_output_alias_and_directory_depth() {
    let fixture = Fixture::new();
    fixture.put("source/Safe.java", b"class Safe {}\n");
    fs::hard_link(
        fixture.0.join("source/Safe.java"),
        fixture.0.join("home/alias"),
    )
    .unwrap();
    assert!(inputs::Inventory::capture(&fixture.0).is_err());
    fs::remove_file(fixture.0.join("home/alias")).unwrap();
    let file = File::options()
        .write(true)
        .open(fixture.0.join("source/Safe.java"))
        .unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    assert!(inputs::Inventory::capture(&fixture.0).is_err());
    file.set_len(0).unwrap();
    drop(file);
    fs::create_dir(fixture.0.join("source/.rh-build")).unwrap();
    assert!(inputs::Inventory::capture(&fixture.0).is_err());
    fs::remove_dir(fixture.0.join("source/.rh-build")).unwrap();
    let mut directory = Directory::open_absolute(&fixture.0.join("home")).unwrap();
    for _ in 0..13 {
        directory = directory
            .create_private(&Leaf::new("nested").unwrap(), Traversal::SameFilesystem)
            .unwrap();
    }
    assert_eq!(
        inputs::Inventory::capture(&fixture.0).err().unwrap().code,
        "BUILD_INPUT_LIMIT"
    );
}

#[test]
fn inventory_refuses_directory_and_file_links_without_following_target() {
    let fixture = Fixture::new();
    fixture.put("source/Safe.java", b"class Safe {}\n");
    #[cfg(unix)]
    let file_link_created = {
        std::os::unix::fs::symlink(
            fixture.0.join("source/Safe.java"),
            fixture.0.join("home/link"),
        )
        .unwrap();
        true
    };
    #[cfg(windows)]
    let file_link_created = {
        match std::os::windows::fs::symlink_file(
            fixture.0.join("source/Safe.java"),
            fixture.0.join("home/link"),
        ) {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == Some(1314) => {
                eprintln!("Native file-symlink fixture unavailable (ERROR_PRIVILEGE_NOT_HELD=1314); unprivileged hardlink and junction cases still run");
                false
            }
            Err(error) => panic!("unexpected file-symlink fixture error: {error}"),
        }
    };
    if file_link_created {
        assert!(fs::symlink_metadata(fixture.0.join("home/link"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(inputs::Inventory::capture(&fixture.0).is_err());
        fs::remove_file(fixture.0.join("home/link")).unwrap();
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(fixture.0.join("source"), fixture.0.join("home/link")).unwrap();
    }
    #[cfg(windows)]
    {
        junction_fixture::create(&fixture.0.join("source"), &fixture.0.join("home/link"));
    }
    assert!(inputs::Inventory::capture(&fixture.0).is_err());
    assert_eq!(
        fs::read(fixture.0.join("source/Safe.java")).unwrap(),
        b"class Safe {}\n"
    );
}

#[test]
fn framing_has_identity_utf8_and_keeps_request_and_control_separate() {
    let _guard = io_fixture();
    let (read, mut write) = pipe();
    let mut input = pipe_input(&read);
    write
        .write_all(b"{\"schemaVersion\":1}\n{\"type\":\"cancel\"}\n")
        .unwrap();
    let request = input.request(Duration::from_secs(1)).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&request).unwrap(),
        json!({"schemaVersion":1})
    );
    assert!(poll_control(&mut input).unwrap());
    let (mut read, output) = pipe();
    let mut writer = writer(&output);
    writer.set_run_id("12345678-1234-4234-8234-123456789abc");
    writer
        .event(&json!({"type":"output","stream":"stdout","text":"你\n😀"}))
        .unwrap();
    drop(writer);
    drop(output);
    let mut bytes = String::new();
    read.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes.lines().count(), 1);
    let event: Value = serde_json::from_str(&bytes).unwrap();
    assert_eq!(event["schemaVersion"], 1);
    assert_eq!(event["runId"], "12345678-1234-4234-8234-123456789abc");
    assert_eq!(event["text"], "你\n😀");
}

#[test]
fn control_refuses_unknown_json_and_owner_eof_cancels() {
    let _guard = io_fixture();
    for message in [
        b"{\"type\":\"continue\"}\n".as_slice(),
        b"{\"type\":\"cancel\",\"extra\":true}\n",
        b"\xff\n",
    ] {
        let (read, mut write) = pipe();
        let mut input = pipe_input(&read);
        write.write_all(message).unwrap();
        assert_eq!(
            poll_control(&mut input).unwrap_err().code,
            "INVALID_BUILD_CONTROL"
        );
    }
    let (read, write) = pipe();
    let mut input = pipe_input(&read);
    drop(write);
    assert!(poll_control(&mut input).unwrap());
}

#[test]
fn request_deadline_and_transport_limits_are_bounded() {
    let _guard = io_fixture();
    let (read, _write) = pipe();
    let mut input = pipe_input(&read);
    let start = Instant::now();
    assert_eq!(
        input.request(Duration::from_millis(30)).unwrap_err().code,
        "BUILD_REQUEST_TIMEOUT"
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    let (read, mut write) = pipe();
    let mut input = pipe_input(&read);
    // Fragmented input crosses the total request bound, independently of a newline.
    let sender = std::thread::spawn(move || {
        let _ = write.write_all(&vec![b'x'; 17 * 1024]);
    });
    assert_eq!(
        input.request(Duration::from_secs(1)).unwrap_err().code,
        "BUILD_CONTROL_LIMIT"
    );
    drop(input);
    drop(read);
    sender.join().unwrap();
}

#[test]
fn output_deadline_does_not_starve_independent_input_or_allow_more_writes() {
    let _guard = io_fixture();
    let (_unread, output) = pipe();
    let writer = writer(&output);
    let (read, mut control) = pipe();
    let mut input = pipe_input(&read);
    control.write_all(b"{\"type\":\"cancel\"}\n").unwrap();
    let start = Instant::now();
    // Pipe capacity varies by host. Fill it with bounded events until the
    // actual reader backpressure reaches the per-call deadline.
    let event = json!({"type":"output","text":"x".repeat(60 * 1024)});
    let error = (0..64)
        .find_map(|_| writer.event(&event).err())
        .expect("pipe capacity must be bounded");
    assert_eq!(error.code, "BUILD_EVENT_TIMEOUT");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(poll_control(&mut input).unwrap());
    assert!(writer
        .event(&json!({"type":"finished","status":"blocked"}))
        .is_err());
}

#[test]
fn utf8_and_output_budget_preserve_complete_characters() {
    let mut utf8 = protocol::Utf8::default();
    assert_eq!(utf8.decode(&[0xe4, 0xbd], false), "");
    assert_eq!(utf8.decode(&[0xa0, 0xff], false), "你�");
    assert_eq!(utf8.decode(&[0xf0, 0x9f], false), "");
    assert_eq!(utf8.decode(&[], true), "�");
    let mut budget = protocol::OutputBudget {
        raw_bytes: 0,
        text_bytes: 0,
        limit: 5,
    };
    let mut text = String::new();
    assert!(budget
        .emit("stdout", "你😀", &mut |_, part| {
            text.push_str(part);
            Ok(())
        })
        .unwrap());
    assert_eq!(text, "你");
    assert_eq!(budget.text_bytes, 3);
}

#[test]
fn request_validation_and_actual_helper_receipt_bind_identity() {
    let _guard = io_fixture();
    let fixture = Fixture::new();
    let mut request = fixture.request();
    request["target"] = "test".into();
    let (read, mut owner) = pipe();
    let mut input = pipe_input(&read);
    let (mut output_read, output) = pipe();
    let mut writer = writer(&output);
    owner.write_all(format!("{request}\n").as_bytes()).unwrap();
    assert_eq!(serve(&mut input, &mut writer), 1);
    drop(writer);
    drop(output);
    let mut bytes = String::new();
    output_read.read_to_string(&mut bytes).unwrap();
    let event: Value = serde_json::from_str(&bytes).unwrap();
    assert_eq!(event["type"], "finished");
    assert_eq!(event["status"], "blocked");
    assert_eq!(event["runId"], request["runId"]);
    assert_eq!(event["error"]["code"], "INVALID_BUILD_REQUEST");
    assert!(!fixture.0.join("source/.rh-build").exists());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn native_actual_helper_reads_inputs_then_refuses_before_output_or_started() {
    let _guard = io_fixture();
    let fixture = Fixture::new();
    fixture.put("source/Safe.java", b"class Safe {}\n");
    let request = fixture.request();
    let source = fs::read(fixture.0.join("source/Safe.java")).unwrap();
    let (read, mut owner) = pipe();
    let mut input = pipe_input(&read);
    let (mut output_read, output) = pipe();
    let mut writer = writer(&output);
    owner.write_all(format!("{request}\n").as_bytes()).unwrap();
    assert_eq!(serve(&mut input, &mut writer), 1);
    drop(writer);
    drop(output);
    let mut text = String::new();
    output_read.read_to_string(&mut text).unwrap();
    assert_eq!(text.lines().count(), 1);
    let event: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(event["type"], "finished");
    assert_eq!(event["status"], "blocked");
    assert_eq!(event["runId"], request["runId"]);
    assert_eq!(event["error"]["code"], "BUILD_SANDBOX_UNAVAILABLE");
    assert_eq!(
        fs::read(fixture.0.join("source/Safe.java")).unwrap(),
        source
    );
    assert!(!fixture.0.join("source/.rh-build").exists());
    assert_eq!(fs::read_dir(fixture.0.join("home")).unwrap().count(), 0);
}
