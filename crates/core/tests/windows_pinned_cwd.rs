//! Real GameChild::spawn_in_directory regressions using only this executable
//! and disposable private fixtures. The ordinary libtest fixture entry points
//! return immediately outside a child fixture. An explicit opt-in CI test uses
//! the runner's JDK 21; no shell, GUI or game is started.
#![cfg(windows)]

#[path = "../src/file_ops/junction_fixture.rs"]
mod junction_fixture;

use pcl_core::{
    game_process::{self, GameChild},
    retained_fs::{self, Directory, Identity, Kind, Leaf, Traversal},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Storage::FileSystem::{
        MoveFileExW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_WRITE_ATTRIBUTES, MOVEFILE_REPLACE_EXISTING,
    },
    System::IO::DeviceIoControl,
};

const CONFIG: &str = ".pcl-test-pinned-cwd-fixture.json";
const WAIT: Duration = Duration::from_secs(10);
#[derive(Serialize, Deserialize)]
struct Config {
    control: PathBuf,
    expected: Identity,
    mode: String,
}
fn pause() {
    std::thread::sleep(Duration::from_millis(5));
}
fn wait(path: &Path) {
    let deadline = Instant::now() + WAIT;
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "fixture marker timeout: {}",
            path.display()
        );
        pause();
    }
}
fn go(control: &Path, name: &str) {
    fs::write(control.join(name), b"go").unwrap();
}
fn fixture_arguments(entry: &str) -> Vec<OsString> {
    ["--exact", entry, "--nocapture"]
        .into_iter()
        .map(Into::into)
        .collect()
}
fn dot() -> File {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(".")
        .unwrap()
}
fn configuration() -> Option<Config> {
    let bytes = match fs::read(CONFIG) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => panic!("fixture config: {error}"),
    };
    Some(serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn fixture_process() {
    let Some(config) = configuration() else {
        return;
    };
    let stamp = retained_fs::metadata(&dot()).unwrap();
    assert_eq!(stamp.identity, config.expected);
    assert_eq!(stamp.kind, Kind::Directory);
    assert!(stamp.reparse_tag.is_none());
    fs::write(
        config.control.join("child-ready"),
        std::process::id().to_string(),
    )
    .unwrap();
    wait(&config.control.join("go"));
    assert_eq!(fs::read("source.txt").unwrap(), b"literal cwd input");
    fs::create_dir("relative-directory").unwrap();
    let mut created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("relative-created")
        .unwrap();
    created.write_all(b"literal relative create").unwrap();
    created.sync_all().unwrap();
    fs::write("relative-saved", b"old value").unwrap();
    let mut temporary = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("relative-save.tmp")
        .unwrap();
    temporary.write_all(b"atomic relative save").unwrap();
    temporary.sync_all().unwrap();
    drop(temporary);
    let from: Vec<u16> = "relative-save.tmp".encode_utf16().chain(Some(0)).collect();
    let to: Vec<u16> = "relative-saved".encode_utf16().chain(Some(0)).collect();
    assert_ne!(
        unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) },
        0,
        "atomic relative save: {}",
        io::Error::last_os_error()
    );
    fs::write(config.control.join("io-complete"), b"complete").unwrap();
    if config.mode == "hang" {
        wait(&config.control.join("finish"));
    }
}

#[test]
fn monitor_process() {
    let Some(config) = configuration() else {
        return;
    };
    let input = Directory::open_absolute(&env::current_dir().unwrap()).unwrap();
    let mut child = GameChild::spawn_in_directory(
        &env::current_exe().unwrap(),
        &fixture_arguments("fixture_process"),
        input.file(),
    )
    .unwrap();
    fs::write(
        config.control.join("monitor-ready"),
        std::process::id().to_string(),
    )
    .unwrap();
    wait(&config.control.join("monitor-stop"));
    child.terminate().unwrap();
    assert!(child.try_wait().unwrap().is_some());
}

#[test]
fn gui_process() {
    let Some(config) = configuration() else {
        return;
    };
    // This launcher owns no GameChild/CwdLease. Only its detached monitor does.
    let _monitor = Command::new(env::current_exe().unwrap())
        .args(fixture_arguments("monitor_process"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait(&config.control.join("monitor-ready"));
    wait(&config.control.join("child-ready"));
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    tree: PathBuf,
    outside: PathBuf,
    control: PathBuf,
    input: File,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temporary.path()).unwrap();
        let tree = base.join("tree");
        let root = tree.join("cwd");
        let outside = base.join("outside");
        let control = base.join("control");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(outside.join("cwd")).unwrap();
        fs::create_dir(&control).unwrap();
        fs::write(root.join("source.txt"), b"literal cwd input").unwrap();
        fs::write(outside.join("sentinel"), b"outside sentinel").unwrap();
        for name in [
            "source.txt",
            "relative-created",
            "relative-saved",
            "relative-save.tmp",
        ] {
            fs::write(outside.join("cwd").join(name), b"outside unchanged").unwrap();
        }
        let input = Directory::open_absolute(&root)
            .unwrap()
            .try_clone_file()
            .unwrap();
        let config = Config {
            control: control.clone(),
            expected: retained_fs::metadata(&input).unwrap().identity,
            mode: mode.into(),
        };
        let encoded = serde_json::to_vec(&config).unwrap();
        fs::write(root.join(CONFIG), &encoded).unwrap();
        fs::write(outside.join("cwd").join(CONFIG), encoded).unwrap();
        Self {
            _temporary: temporary,
            root,
            tree,
            outside,
            control,
            input,
        }
    }
    fn spawn(&self) -> GameChild {
        GameChild::spawn_in_directory(
            &env::current_exe().unwrap(),
            &fixture_arguments("fixture_process"),
            &self.input,
        )
        .unwrap()
    }
    fn outside_hash(&self) -> String {
        fn hash(path: &Path, digest: &mut Sha256) {
            let stamp = fs::symlink_metadata(path).unwrap();
            assert!(!stamp.file_type().is_symlink());
            let mut entries = fs::read_dir(path)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                digest.update(
                    entry
                        .file_name()
                        .encode_wide()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>(),
                );
                if entry.file_type().unwrap().is_dir() {
                    digest.update(b"directory");
                    hash(&entry.path(), digest);
                } else {
                    digest.update(b"file");
                    digest.update(fs::read(entry.path()).unwrap());
                }
            }
        }
        let mut digest = Sha256::new();
        hash(&self.outside, &mut digest);
        format!("{:x}", digest.finalize())
    }
    fn anchor(&self) -> PathBuf {
        let anchors: Vec<_> = fs::read_dir(&self.root)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".pcl-rh-cwd-anchor-v1-")
            })
            .map(|entry| entry.path())
            .collect();
        assert_eq!(anchors.len(), 1);
        anchors.into_iter().next().unwrap()
    }
    fn assert_io(&self) {
        assert_eq!(
            fs::read(self.root.join("relative-created")).unwrap(),
            b"literal relative create"
        );
        assert_eq!(
            fs::read(self.root.join("relative-saved")).unwrap(),
            b"atomic relative save"
        );
        assert!(self.root.join("relative-directory").is_dir());
        assert!(!self.root.join("relative-save.tmp").exists());
    }
    fn attack(&self) {
        let blocked = |result: io::Result<()>| {
            let error = result.expect_err("cwd candidate mutation succeeded");
            assert!(
                matches!(error.raw_os_error(), Some(5 | 32 | 145)),
                "unexpected mutation rejection: {error}"
            );
        };
        blocked(fs::rename(
            &self.root,
            self.root.with_file_name("moved-cwd"),
        ));
        blocked(fs::rename(
            &self.tree,
            self.tree.with_file_name("moved-tree"),
        ));
        blocked(junction_fixture::set_existing(
            &self.outside.join("cwd"),
            &self.root,
        ));
        blocked(set_junction_attributes(
            &self.outside.join("cwd"),
            &self.root,
        ));
        blocked(junction_fixture::set_existing(&self.outside, &self.tree));
        blocked(set_junction_attributes(&self.outside, &self.tree));
        let anchor = self.anchor();
        let stamp = retained_fs::metadata(&File::open(&anchor).unwrap()).unwrap();
        stamp.require_private().unwrap();
        assert_eq!(stamp.links, 1);
        blocked(fs::remove_file(&anchor));
        blocked(fs::rename(&anchor, self.root.join("stolen-anchor")));
        let replacement = self.root.join("replacement-anchor");
        fs::write(&replacement, b"attacker bytes").unwrap();
        let from: Vec<u16> = replacement
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let to: Vec<u16> = anchor.as_os_str().encode_wide().chain(Some(0)).collect();
        blocked(
            if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            },
        );
        assert_eq!(
            retained_fs::metadata(&File::open(&anchor).unwrap())
                .unwrap()
                .identity,
            stamp.identity
        );
    }
}

// The dangerous attributes-only access bypasses ordinary data-write sharing.
fn set_junction_attributes(target: &Path, directory: &Path) -> io::Result<()> {
    let target = fs::canonicalize(target)?;
    let units: Vec<u16> = target.as_os_str().encode_wide().collect();
    assert!(units.starts_with(&[92, 92, 63, 92]));
    let print = &units[4..];
    let substitute: Vec<u16> = [92, 63, 63, 92]
        .into_iter()
        .chain(print.iter().copied())
        .collect();
    let sub_bytes = substitute.len() * 2;
    let size = 16 + sub_bytes + 2 + print.len() * 2 + 2;
    let mut aligned = vec![0u32; size.div_ceil(4)];
    let bytes = unsafe { std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast::<u8>(), size) };
    bytes[..4].copy_from_slice(&0xA0000003u32.to_le_bytes());
    bytes[4..6].copy_from_slice(&((size - 8) as u16).to_le_bytes());
    bytes[10..12].copy_from_slice(&(sub_bytes as u16).to_le_bytes());
    bytes[12..14].copy_from_slice(&((sub_bytes + 2) as u16).to_le_bytes());
    bytes[14..16].copy_from_slice(&((print.len() * 2) as u16).to_le_bytes());
    for (index, unit) in substitute.iter().enumerate() {
        bytes[16 + index * 2..18 + index * 2].copy_from_slice(&unit.to_le_bytes());
    }
    for (index, unit) in print.iter().enumerate() {
        let offset = 18 + sub_bytes + index * 2;
        bytes[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
    }
    let file = fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory)?;
    let mut returned = 0;
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            0x000900a4,
            aligned.as_ptr().cast(),
            size as u32,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[test]
fn created_directory_delete_access_requires_identity_checked_read_caps() {
    let temporary = tempfile::tempdir().unwrap();
    let parent = Directory::open_absolute(&fs::canonicalize(temporary.path()).unwrap()).unwrap();
    let (workspace_name, created_workspace) = parent.create_private_workspace().unwrap();
    let workspace_identity = created_workspace.identity().unwrap();
    let workspace = parent
        .child(&workspace_name, Traversal::SameFilesystem)
        .unwrap();
    assert_eq!(workspace.identity().unwrap(), workspace_identity);
    workspace.metadata().unwrap().require_private().unwrap();
    parent
        .recheck(&workspace_name, &workspace_identity)
        .unwrap();

    // Build descendants through the fresh read cap, so they cannot keep the
    // original workspace's DELETE access alive through Directory::_parent.
    let game_name = Leaf::new("game").unwrap();
    let created_game = workspace
        .create_private(&game_name, Traversal::SameFilesystem)
        .unwrap();
    let game_identity = created_game.identity().unwrap();
    let game = workspace
        .child(&game_name, Traversal::SameFilesystem)
        .unwrap();
    assert_eq!(game.identity().unwrap(), game_identity);
    game.metadata().unwrap().require_private().unwrap();
    workspace.recheck(&game_name, &game_identity).unwrap();
    let image = env::current_exe().unwrap();
    let arguments = [OsString::from("--list")];
    let spawn = || GameChild::spawn_in_directory(&image, &arguments, game.file());

    // Both creation FILE_OBJECTs request DELETE. A read-only cwd input does
    // not make them compatible with the launcher's retained NoDelete graph.
    assert!(
        spawn().is_err(),
        "creation caps bypassed NoDelete admission"
    );
    parent
        .recheck(&workspace_name, &workspace_identity)
        .unwrap();
    drop(created_workspace);
    workspace.metadata().unwrap().require_private().unwrap();
    assert!(
        spawn().is_err(),
        "game creation cap bypassed NoDelete admission"
    );
    workspace.recheck(&game_name, &game_identity).unwrap();
    drop(created_game);
    game.metadata().unwrap().require_private().unwrap();

    let mut child = spawn().expect("identity-checked read caps must permit pinned cwd startup");
    let stdout = child.take_stdout().unwrap();
    let stderr = child.take_stderr().unwrap();
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "native --list child did not exit"
        );
        pause();
    };
    assert_eq!(status.code(), Some(0));
    assert_eq!(child.wait().unwrap(), status);
    // GameOutput stays nonblocking after process exit: an overlapped read
    // must complete before its buffered bytes or EOF can be observed.
    let drain = |mut stream: game_process::GameOutput| {
        let deadline = Instant::now() + WAIT;
        let mut bytes = Vec::new();
        let mut block = [0u8; 1024];
        loop {
            match stream.read(&mut block) {
                Ok(0) => break,
                Ok(count) => bytes.extend_from_slice(&block[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => pause(),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => panic!("native --list output read failed: {error}"),
            }
            assert!(
                bytes.len() <= 64 * 1024,
                "native --list output exceeded bound"
            );
            assert!(
                Instant::now() < deadline,
                "native --list output did not reach EOF"
            );
        }
        String::from_utf8(bytes).unwrap()
    };
    let listed = drain(stdout);
    assert!(listed
        .contains("created_directory_delete_access_requires_identity_checked_read_caps: test"));
    let errors = drain(stderr);
    assert!(errors.is_empty(), "native --list stderr: {errors}");
    parent
        .recheck(&workspace_name, &workspace_identity)
        .unwrap();
    workspace.recheck(&game_name, &game_identity).unwrap();
    assert_eq!(game.identity().unwrap(), game_identity);
    workspace.metadata().unwrap().require_private().unwrap();
    game.metadata().unwrap().require_private().unwrap();
}

#[test]
fn java21_pinned_volume_guid_cwd_requires_java_user_dir() {
    let Some(java_home) = env::var_os("PCL_TEST_WINDOWS_JAVA21_HOME") else {
        eprintln!("JDK 21 opt-in absent; this invocation is not real JVM validation");
        return;
    };
    fn private_child(parent: &Directory, name: &Leaf) -> Directory {
        let created = parent
            .create_private(name, Traversal::SameFilesystem)
            .unwrap();
        let expected = created.identity().unwrap();
        let read = parent.child(name, Traversal::SameFilesystem).unwrap();
        assert_eq!(read.identity().unwrap(), expected);
        read.metadata().unwrap().require_private().unwrap();
        parent.recheck(name, &expected).unwrap();
        drop(created);
        read
    }
    fn collect(
        mut child: GameChild,
        mut while_running: impl FnMut(),
    ) -> (std::process::ExitStatus, String, String) {
        let mut streams = [child.take_stdout().unwrap(), child.take_stderr().unwrap()];
        let mut bytes = [Vec::new(), Vec::new()];
        let mut eof = [false; 2];
        let mut status = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            for index in 0..2 {
                if eof[index] {
                    continue;
                }
                let mut block = [0u8; 4096];
                match streams[index].read(&mut block) {
                    Ok(0) => eof[index] = true,
                    Ok(count) => bytes[index].extend_from_slice(&block[..count]),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => panic!("JDK fixture output read: {error}"),
                }
                assert!(
                    bytes[index].len() <= 64 * 1024,
                    "JDK fixture output exceeded 64 KiB"
                );
            }
            while_running();
            if status.is_none() {
                status = child.try_wait().unwrap();
            }
            if let Some(status) = status {
                if eof.into_iter().all(|done| done) {
                    assert_eq!(child.wait().unwrap(), status);
                    let [stdout, stderr] = bytes.map(|value| String::from_utf8(value).unwrap());
                    return (status, stdout, stderr);
                }
            }
            if Instant::now() >= deadline {
                let cleanup = Instant::now();
                child.terminate().unwrap();
                assert!(
                    cleanup.elapsed() < Duration::from_secs(2),
                    "JDK fixture cleanup exceeded deadline"
                );
                panic!(
                    "JDK fixture process/output deadline; stdout={} stderr={}",
                    String::from_utf8_lossy(&bytes[0]),
                    String::from_utf8_lossy(&bytes[1])
                );
            }
            pause();
        }
    }
    fn anchors(path: &Path) -> Vec<PathBuf> {
        fs::read_dir(path)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".pcl-rh-cwd-anchor-v1-")
            })
            .map(|entry| entry.path())
            .collect()
    }
    let java_home = PathBuf::from(java_home);
    let java = pcl_core::java::invocation_path(&java_home.join("bin/java.exe")).unwrap();
    let javac = pcl_core::java::invocation_path(&java_home.join("bin/javac.exe")).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temporary.path()).unwrap();
    let parent = Directory::open_absolute(&base).unwrap();
    let (name, created) = parent.create_private_workspace().unwrap();
    let workspace_identity = created.identity().unwrap();
    let workspace = parent.child(&name, Traversal::SameFilesystem).unwrap();
    assert_eq!(workspace.identity().unwrap(), workspace_identity);
    parent.recheck(&name, &workspace_identity).unwrap();
    workspace.metadata().unwrap().require_private().unwrap();
    drop(created);
    let workspace_path = base.join(name.as_os_str());
    let compile_cwd = pcl_core::java::invocation_path(&workspace_path).unwrap();
    let (version_status, version_stdout, version_stderr) = collect(
        GameChild::spawn(
            &java,
            &["-XshowSettings:properties".into(), "-version".into()],
            &compile_cwd,
        )
        .unwrap(),
        || {},
    );
    println!("JDK native version/property probe:\n{version_stdout}{version_stderr}");
    println!("Windows launcher ACP: {}", unsafe {
        windows_sys::Win32::Globalization::GetACP()
    });
    assert!(
        version_status.success(),
        "JDK 21 version/property probe failed"
    );
    let game_name = Leaf::new("游戏 cwd").unwrap();
    let game = private_child(&workspace, &game_name);
    let _control = private_child(&workspace, &Leaf::new("control").unwrap());
    let _outside = private_child(&workspace, &Leaf::new("outside").unwrap());
    let game_identity = game.identity().unwrap();
    let game_path = workspace_path.join(game_name.as_os_str());
    let java_cwd = pcl_core::java::invocation_path(&game_path)
        .expect("Chinese cwd needs a lossless code-page spelling or an existing same-object 8.3 alias; do not enable 8.3 or replace this fixture with an ASCII directory");
    assert_eq!(
        java_cwd.canonicalize().unwrap(),
        game_path.canonicalize().unwrap()
    );
    assert_eq!(
        Directory::open_absolute(&java_cwd)
            .unwrap()
            .identity()
            .unwrap(),
        game_identity
    );
    let control = pcl_core::java::invocation_path(&workspace_path.join("control")).unwrap();
    let ready = control.join("java-ready");
    let finish = control.join("java-finish");
    let source = control.join("PinnedCwd.java");
    fs::write(game_path.join("source.txt"), b"literal cwd input").unwrap();
    fs::write(
        &source,
        r#"
import java.nio.file.*;
public final class PinnedCwd {
    public static void main(String[] args) throws Exception {
        if (Runtime.version().feature() != 21) throw new AssertionError("JDK 21 required");
        Path cwd = Paths.get(".").toRealPath();
        if (!Files.isSameFile(cwd, Path.of(args[0]))) throw new AssertionError("different cwd");
        if (!Files.readString(Path.of("source.txt")).equals("literal cwd input"))
            throw new AssertionError("relative read escaped cwd");
        Files.writeString(Path.of("java-created"), "relative Java write");
        Files.writeString(Path.of(args[1]), "ready");
        while (!Files.exists(Path.of(args[2]))) Thread.sleep(5);
        System.out.println("java.base@" + Runtime.version().feature());
        System.out.println("same-cwd:true");
    }
}
"#,
    )
    .unwrap();
    let compile_args = [
        OsString::from("--release"),
        "21".into(),
        "-encoding".into(),
        "UTF-8".into(),
        "-d".into(),
        control.as_os_str().to_owned(),
        source.into_os_string(),
    ];
    let (status, stdout, stderr) = collect(
        GameChild::spawn(&javac, &compile_args, &compile_cwd).unwrap(),
        || {},
    );
    assert!(
        status.success(),
        "JDK fixture javac failed: {stdout}{stderr}"
    );
    let arguments = [
        OsString::from("-Dsun.java.launcher.diag=true"),
        "-cp".into(),
        control.as_os_str().to_owned(),
        "PinnedCwd".into(),
        java_cwd.as_os_str().to_owned(),
        ready.as_os_str().to_owned(),
        finish.as_os_str().to_owned(),
    ];
    let (status, stdout, stderr) = collect(
        GameChild::spawn_in_directory(&java, &arguments, game.file()).unwrap(),
        || {},
    );
    println!(
        "GUID cwd negative probe (exit={:?}):\n{stdout}{stderr}",
        status.code()
    );
    assert_eq!(
        status.code(),
        Some(1),
        "unmodified GUID cwd unexpectedly worked: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("InvalidPathException")
            && stderr.contains("Long path prefix can only be used with an absolute path"),
        "JDK did not reproduce Volume GUID default-directory failure: {stdout}{stderr}"
    );
    assert!(!ready.exists(), "negative JVM entered the Java fixture");
    let failed_anchors = anchors(&game_path);
    assert_eq!(failed_anchors.len(), 1);
    fs::remove_file(&failed_anchors[0]).unwrap();

    let mut fixed_arguments = vec![OsString::from(format!("-Duser.dir={}", java_cwd.display()))];
    fixed_arguments.extend(arguments);
    let child = GameChild::spawn_in_directory(&java, &fixed_arguments, game.file()).unwrap();
    let pid = child.id();
    let mut owned_identity = None;
    let mut checked = false;
    let (status, stdout, stderr) = collect(child, || {
        if checked || !ready.is_file() {
            return;
        }
        let identity = game_process::identity(pid)
            .unwrap()
            .expect("JDK fixture must still be alive");
        assert!(game_process::alive(&identity).unwrap());
        let blocked = |result: io::Result<()>| {
            let error = result.expect_err("JVM cwd mutation succeeded while owned Job was live");
            assert!(
                matches!(error.raw_os_error(), Some(5 | 32 | 145)),
                "unexpected rejection: {error}"
            );
        };
        blocked(fs::rename(&game_path, workspace_path.join("moved-cwd")));
        blocked(fs::rename(&workspace_path, base.join("moved-workspace")));
        blocked(junction_fixture::set_existing(
            &workspace_path.join("outside"),
            &game_path,
        ));
        blocked(set_junction_attributes(
            &workspace_path.join("outside"),
            &game_path,
        ));
        let protected = anchors(&game_path);
        assert_eq!(protected.len(), 1);
        blocked(fs::remove_file(&protected[0]));
        blocked(fs::rename(&protected[0], game_path.join("stolen-anchor")));
        assert_eq!(game.identity().unwrap(), game_identity);
        workspace.recheck(&game_name, &game_identity).unwrap();
        game.metadata().unwrap().require_private().unwrap();
        owned_identity = Some(identity);
        checked = true;
        fs::write(&finish, b"finish").unwrap();
    });
    println!(
        "Chinese cwd positive probe (exit={:?}):\n{stdout}{stderr}",
        status.code()
    );
    assert!(
        checked,
        "real JDK never completed its relative IO fixture: {stdout}{stderr}"
    );
    assert_eq!(status.code(), Some(0), "fixed JVM failed: {stdout}{stderr}");
    assert!(
        stdout.lines().any(|line| line == "java.base@21"),
        "not the required JDK 21: {stdout}"
    );
    assert!(stdout.lines().any(|line| line == "same-cwd:true"));
    assert!(stderr.is_empty(), "fixed JDK stderr: {stderr}");
    assert!(
        !game_process::alive(&owned_identity.unwrap()).unwrap(),
        "owned JVM survived retirement"
    );
    assert_eq!(
        fs::read(game_path.join("java-created")).unwrap(),
        b"relative Java write"
    );
    assert_eq!(game.identity().unwrap(), game_identity);
    assert_eq!(
        java_cwd.canonicalize().unwrap(),
        game_path.canonicalize().unwrap()
    );
    workspace.recheck(&game_name, &game_identity).unwrap();
    parent.recheck(&name, &workspace_identity).unwrap();
    game.metadata().unwrap().require_private().unwrap();
    for anchor in anchors(&game_path) {
        fs::remove_file(anchor).unwrap();
    }
}

#[test]
fn pinned_relative_io_normal_exit_and_mutation_rejection() {
    let fixture = Fixture::new("exit");
    let before = fixture.outside_hash();
    let mut child = fixture.spawn();
    wait(&fixture.control.join("child-ready"));
    fixture.attack();
    go(&fixture.control, "go");
    assert!(child.wait().unwrap().success());
    fixture.assert_io();
    assert_eq!(fixture.outside_hash(), before);
    // The small owned anchor remains, but its NoDelete lease is gone only
    // after GameChild confirms leader exit and the entire Job is empty.
    let anchor = fixture.anchor();
    fs::remove_file(anchor).unwrap();
    // The API borrows the caller's input; it must not close that caller-owned
    // handle. Windows also refuses moving an ancestor of this open directory.
    // Retire the independent fixture input before testing released graph locks.
    drop(fixture.input);
    fs::rename(&fixture.tree, fixture.tree.with_file_name("completed-tree")).unwrap();
}

#[test]
fn pinned_stop_keeps_graph_until_owned_job_is_empty() {
    let fixture = Fixture::new("hang");
    let before = fixture.outside_hash();
    let mut child = fixture.spawn();
    wait(&fixture.control.join("child-ready"));
    go(&fixture.control, "go");
    wait(&fixture.control.join("io-complete"));
    fixture.attack();
    child.terminate().unwrap();
    assert!(child.try_wait().unwrap().is_some());
    fixture.assert_io();
    assert_eq!(fixture.outside_hash(), before);
    fs::remove_file(fixture.anchor()).unwrap();
    drop(fixture.input);
    fs::rename(&fixture.tree, fixture.tree.with_file_name("stopped-tree")).unwrap();
}

#[test]
fn literal_reparse_input_is_rejected_before_child_or_anchor_creation() {
    let fixture = Fixture::new("exit");
    let literal = fixture.root.with_file_name("literal-junction");
    junction_fixture::create(&fixture.outside.join("cwd"), &literal);
    let input = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&literal)
        .unwrap();
    let before = fixture.outside_hash();
    assert!(GameChild::spawn_in_directory(
        &env::current_exe().unwrap(),
        &fixture_arguments("fixture_process"),
        &input
    )
    .is_err());
    assert!(!fixture.control.join("child-ready").exists());
    assert_eq!(fixture.outside_hash(), before);
    assert!(!fs::read_dir(&fixture.root)
        .unwrap()
        .map(Result::unwrap)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".pcl-rh-cwd-anchor-v1-")));
}

struct External(Child);
impl Drop for External {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn gui_exit_does_not_release_monitor_owned_cwd_lease() {
    let fixture = Fixture::new("hang");
    let before = fixture.outside_hash();
    let mut gui = External(
        Command::new(env::current_exe().unwrap())
            .args(fixture_arguments("gui_process"))
            .current_dir(&fixture.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = gui.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "synthetic GUI did not exit");
        pause();
    }
    wait(&fixture.control.join("child-ready"));
    let pid = fs::read_to_string(fixture.control.join("child-ready"))
        .unwrap()
        .parse()
        .unwrap();
    let identity = game_process::identity(pid).unwrap().unwrap();
    assert!(game_process::alive(&identity).unwrap());
    fixture.attack();
    go(&fixture.control, "go");
    wait(&fixture.control.join("io-complete"));
    go(&fixture.control, "monitor-stop");
    let deadline = Instant::now() + WAIT;
    while game_process::alive(&identity).unwrap() {
        assert!(
            Instant::now() < deadline,
            "monitor did not clean its own Job"
        );
        pause();
    }
    fixture.assert_io();
    assert_eq!(fixture.outside_hash(), before);
}
