//! Independent, opt-in measurement of a nonempty cwd plus NoDelete graph.
//! This is not a spawn API, sandbox, or a proof of production cwd safety.
//!
//! Primary references (reviewed 2026-10-31):
//! https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/4aeefef8-92c3-4abc-af7a-a610caf8a165
//! specifies WRITE_DATA *or* WRITE_ATTRIBUTES and DIRECTORY_NOT_EMPTY for
//! FSCTL_SET_REPARSE_POINT. Sharing alone does not block attributes access:
//! https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew
//! Hence both access masks have required empty-directory positive controls.

use super::{load, new_job, nul, quote, save, Attributes, NativeChild};
use pcl_core::retained_fs::{self, Directory, Identity, Kind, Leaf, Security, Traversal};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read, Write},
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr, thread,
    time::{Duration, Instant},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{
            RtlNtStatusToDosError, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
            OBJ_CASE_INSENSITIVE, UNICODE_STRING, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Storage::FileSystem::{
            CreateFileW, MoveFileExW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_WRITE_ATTRIBUTES,
            FILE_WRITE_DATA, MOVEFILE_REPLACE_EXISTING, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE,
        },
        System::{
            JobObjects::AssignProcessToJobObject,
            Threading::{
                CreateProcessW, GetExitCodeProcess, ResumeThread, WaitForSingleObject,
                CREATE_NO_WINDOW, CREATE_SUSPENDED, EXTENDED_STARTUPINFO_PRESENT,
                PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            },
            IO::{DeviceIoControl, IO_STATUS_BLOCK},
        },
    },
};

const SHARING: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const DIRECTORY_ACCESS: u32 =
    FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
const ANCHOR: &str = ".pcl-rh-private-cwd-anchor";
const CASES: [&str; 12] = [
    "baseline",
    "positive-empty-generic-write",
    "positive-empty-write-attributes",
    "before-spawn-generic-write",
    "before-spawn-write-attributes",
    "cwd-rename-fullgraph",
    "cwd-rename-leaf-only",
    "ancestor-rename-fullgraph",
    "ancestor-rename-leaf-only",
    "anchor-unlink-and-rename-replace",
    "suspended-fsctl",
    "child-ready-fsctl",
];

fn enabled() -> bool {
    env::var_os("PCL_RH_CWD_ANCHOR_PROBE").is_some_and(|value| value == "1")
}
fn stage(control: &Path, phase: &str) -> io::Result<()> {
    save(&control.join("phase.json"), json!({"phase":phase}))
}
fn error_value(error: &io::Error) -> Value {
    json!({"message":error.to_string(),"native_error":error.raw_os_error(),"kind":format!("{:?}",error.kind())})
}
fn result_value(result: &io::Result<()>) -> Value {
    match result {
        Ok(()) => json!({"succeeded":true}),
        Err(error) => json!({"succeeded":false,"error":error_value(error)}),
    }
}
fn units(path: &Path) -> io::Result<Vec<u16>> {
    let mut text: Vec<u16> = path.as_os_str().encode_wide().collect();
    if text.contains(&0) {
        return Err(io::Error::other("NUL in probe path"));
    }
    text.push(0);
    Ok(text)
}
fn open_path(path: &Path, access: u32, sharing: u32) -> io::Result<File> {
    let path = units(path)?;
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            sharing,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(raw) })
}

// Counted single-leaf NT opens, never a concatenated mutable ancestor path.
// A supplied SD is self-relative, aligned, and retained until NtCreateFile returns.
fn open_at(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    sharing: u32,
    descriptor: Option<&[u32]>,
) -> io::Result<File> {
    Leaf::new(name)?;
    let mut text: Vec<u16> = name.encode_wide().collect();
    let name = UNICODE_STRING {
        Length: (text.len() * 2) as u16,
        MaximumLength: (text.len() * 2) as u16,
        Buffer: text.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: descriptor.map_or(ptr::null(), |value| value.as_ptr().cast()),
        ..Default::default()
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut raw = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut raw,
            access | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            ptr::null(),
            0,
            sharing,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    Ok(unsafe { File::from_raw_handle(raw) })
}

struct Graph {
    held: Vec<File>,
    evidence: Vec<Value>,
}
impl Graph {
    fn open(path: &Path, leaf_only: bool) -> io::Result<Self> {
        let mut root = PathBuf::new();
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) if root.as_os_str().is_empty() => {
                    if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
                        return Err(io::Error::other("probe requires a local disk path"));
                    }
                    root.push(prefix.as_os_str());
                }
                Component::RootDir if names.is_empty() => root.push(component.as_os_str()),
                Component::Normal(name) => names.push(Leaf::new(name)?),
                _ => return Err(io::Error::other("invalid absolute graph path")),
            }
        }
        if !root.is_absolute() || names.is_empty() {
            return Err(io::Error::other(
                "probe graph must have a local absolute leaf",
            ));
        }
        let mut held = vec![open_path(&root, DIRECTORY_ACCESS, SHARING)?];
        let mut evidence: Vec<Value> = Vec::new();
        for index in 0..=names.len() {
            let stamp = retained_fs::metadata(held.last().unwrap())?;
            if stamp.kind != Kind::Directory || stamp.reparse_tag.is_some() {
                return Err(io::Error::other(
                    "graph contains a reparse point or non-directory",
                ));
            }
            if let Some(first) = evidence.first() {
                let identity: Identity =
                    serde_json::from_value(first["metadata"]["identity"].clone())?;
                if !matches!((&identity, &stamp.identity),
                    (Identity::Windows { volume: a, .. }, Identity::Windows { volume: b, .. }) if a == b)
                {
                    return Err(io::Error::other("graph crosses a volume"));
                }
            }
            evidence.push(json!({"depth":index,"metadata":stamp}));
            if let Some(name) = names.get(index) {
                held.push(open_at(
                    held.last().unwrap(),
                    name.as_os_str(),
                    DIRECTORY_ACCESS,
                    FILE_OPEN,
                    FILE_DIRECTORY_FILE,
                    SHARING,
                    None,
                )?);
            }
        }
        // Deliberate comparison case: release all ancestor protection but keep
        // exactly the same validated literal leaf handle and anchor.
        if leaf_only {
            let leaf = held.pop().unwrap();
            held.clear();
            held.push(leaf);
        }
        Ok(Self { held, evidence })
    }
    fn leaf(&self) -> &File {
        self.held.last().unwrap()
    }
}

fn create_anchor(graph: &Graph) -> io::Result<File> {
    let directory = retained_fs::metadata(graph.leaf())?;
    directory.require_private()?;
    let Security::Windows { descriptor, .. } = directory.security else {
        return Err(io::Error::other("missing Windows private descriptor"));
    };
    let mut aligned = vec![0u32; descriptor.len().div_ceil(4)];
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast::<u8>(), aligned.len() * 4)
    };
    bytes[..descriptor.len()].copy_from_slice(&descriptor);
    let mut file = open_at(
        graph.leaf(),
        OsStr::new(ANCHOR),
        FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        SHARING,
        Some(&aligned),
    )?;
    file.write_all(b"private nonempty cwd anchor\n")?;
    file.sync_all()?;
    let stamp = retained_fs::metadata(&file)?;
    stamp.require_private()?;
    if stamp.kind != Kind::Regular || stamp.links != 1 || stamp.reparse_tag.is_some() {
        return Err(io::Error::other(
            "anchor is not a private single-link regular file",
        ));
    }
    Ok(file)
}

// Deliberately request exactly one of the two relevant attack access masks.
// FILE_SHARE_DELETE on attacker opens cannot undo the defender's NoDelete lease.
fn set_junction(target: &Path, directory: &Path, access: u32) -> Value {
    let mut attack_phase = "prepare-junction-buffer";
    let result = (|| -> io::Result<()> {
        let target = fs::canonicalize(target)?;
        let target: Vec<u16> = target.as_os_str().encode_wide().collect();
        if !target.starts_with(&[92, 92, 63, 92]) || target.get(5) != Some(&58) {
            return Err(io::Error::other(
                "junction target is not an extended local disk path",
            ));
        }
        let print = &target[4..];
        let substitute: Vec<u16> = [92, 63, 63, 92]
            .into_iter()
            .chain(print.iter().copied())
            .collect();
        let substitute_bytes = substitute.len() * 2;
        let size = 16 + substitute_bytes + 2 + print.len() * 2 + 2;
        if size >= 16 * 1024 {
            return Err(io::Error::other("junction buffer too large"));
        }
        let mut aligned = vec![0u32; size.div_ceil(4)];
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast::<u8>(), size) };
        bytes[..4].copy_from_slice(&0xA0000003u32.to_le_bytes());
        bytes[4..6].copy_from_slice(&((size - 8) as u16).to_le_bytes());
        bytes[10..12].copy_from_slice(&(substitute_bytes as u16).to_le_bytes());
        bytes[12..14].copy_from_slice(&((substitute_bytes + 2) as u16).to_le_bytes());
        bytes[14..16].copy_from_slice(&((print.len() * 2) as u16).to_le_bytes());
        for (index, unit) in substitute.iter().enumerate() {
            bytes[16 + index * 2..18 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        for (index, unit) in print.iter().enumerate() {
            let offset = 18 + substitute_bytes + index * 2;
            bytes[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
        }
        attack_phase = "attack-open";
        let file = open_path(directory, access, SHARING | FILE_SHARE_DELETE)?;
        attack_phase = "attack-fsctl";
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
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    json!({"operation":"FSCTL_SET_REPARSE_POINT","access":access,"phase":attack_phase,"result":result_value(&result)})
}

fn spawn(arguments: &[&OsStr]) -> io::Result<NativeChild> {
    let exe = env::current_exe()?;
    let image = units(&exe)?;
    let mut command = Vec::new();
    quote(&mut command, exe.as_os_str());
    for argument in arguments {
        command.push(b' ' as u16);
        quote(&mut command, argument);
    }
    if command.len() >= 32767 {
        return Err(io::Error::other("probe command too long"));
    }
    command.push(0);
    let job = new_job()?;
    let input = nul(GENERIC_READ)?;
    let output = nul(GENERIC_WRITE)?;
    let error = nul(GENERIC_WRITE)?;
    let mut attributes = Attributes::new([
        input.as_raw_handle(),
        output.as_raw_handle(),
        error.as_raw_handle(),
    ])?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = input.as_raw_handle();
    startup.StartupInfo.hStdOutput = output.as_raw_handle();
    startup.StartupInfo.hStdError = error.as_raw_handle();
    startup.lpAttributeList = attributes.pointer();
    let mut information = PROCESS_INFORMATION::default();
    if unsafe {
        CreateProcessW(
            image.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
            ptr::null(),
            ptr::null(),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut child = NativeChild {
        process: unsafe {
            std::os::windows::io::OwnedHandle::from_raw_handle(information.hProcess)
        },
        thread: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(information.hThread) },
        job,
        assigned: false,
    };
    if unsafe { AssignProcessToJobObject(child.job.as_raw_handle(), child.process.as_raw_handle()) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    child.assigned = true;
    Ok(child)
}
fn resume(child: &NativeChild) -> io::Result<()> {
    if unsafe { ResumeThread(child.thread.as_raw_handle()) } == u32::MAX {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn exit_code(child: &NativeChild) -> io::Result<Option<u32>> {
    match unsafe { WaitForSingleObject(child.process.as_raw_handle(), 0) } {
        WAIT_TIMEOUT => Ok(None),
        WAIT_OBJECT_0 => {
            let mut code = 0;
            if unsafe { GetExitCodeProcess(child.process.as_raw_handle(), &mut code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(code))
        }
        WAIT_FAILED => Err(io::Error::last_os_error()),
        _ => Err(io::Error::other("unexpected process wait status")),
    }
}
fn wait_for(path: &Path, child: Option<&NativeChild>) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !path.is_file() {
        if let Some(child) = child {
            if let Some(code) = exit_code(child)? {
                return Err(io::Error::other(format!(
                    "process exited {code} before {}",
                    path.display()
                )));
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "marker timeout: {}",
                path.display()
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
fn wait_exit(child: &NativeChild) -> io::Result<u32> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(code) = exit_code(child)? {
            return Ok(code);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("probe process timeout"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn worker(root: &Path, control: &Path, case: &str) -> io::Result<()> {
    stage(control, "opening-graph")?;
    let graph = Graph::open(root, case.ends_with("leaf-only"))?;
    let expected = retained_fs::metadata(graph.leaf())?.identity;
    let positive = case.starts_with("positive-empty-");
    let anchor = if positive {
        None
    } else {
        Some(create_anchor(&graph)?)
    };
    env::set_current_dir(root)?;
    let dot = retained_fs::metadata(&super::dot()?)?;
    if dot.identity != expected || dot.kind != Kind::Directory || dot.reparse_tag.is_some() {
        return Err(io::Error::other(
            "initial cwd differs from literal held graph",
        ));
    }
    save(
        &control.join("ready.json"),
        json!({"expected":expected,"dot":dot,"graph":graph.evidence,
        "retained_directory_handles":graph.held.len(),"anchor":anchor.as_ref().map(retained_fs::metadata).transpose()?}),
    )?;
    stage(control, "ready-before-spawn")?;
    wait_for(&control.join("continue"), None)?;
    if positive {
        // Empty-directory positive controls intentionally do not launch a child
        // through the now-mutated cwd; their only purpose is attack viability.
        return save(
            &control.join("worker.json"),
            json!({"positive_control":true,"held_after":retained_fs::metadata(graph.leaf())?}),
        );
    }
    stage(control, "creating-suspended-child")?;
    let child = spawn(&[OsStr::new("--cwd-anchor-child"), control.as_os_str()])?;
    stage(control, "child-suspended-private-job-assigned")?;
    save(
        &control.join("suspended.json"),
        json!({"phase":"private-job-assigned-before-resume"}),
    )?;
    if case == "suspended-fsctl" {
        wait_for(&control.join("resume"), Some(&child))?;
    }
    resume(&child)?;
    stage(control, "child-resumed")?;
    wait_for(&control.join("child-ready.json"), Some(&child))?;
    if case == "child-ready-fsctl" {
        save(
            &control.join("child-window.json"),
            json!({"phase":"child-ready-before-relative-io"}),
        )?;
        wait_for(&control.join("child-go"), Some(&child))?;
    } else {
        fs::write(control.join("child-go"), b"go")?;
    }
    let code = wait_exit(&child)?;
    stage(control, "child-exited")?;
    save(
        &control.join("worker.json"),
        json!({"child_exit":code,
        "held_after":retained_fs::metadata(graph.leaf())?,
        "anchor_after":anchor.as_ref().map(retained_fs::metadata).transpose()?,
        "child":load(&control.join("child.json"))?}),
    )
}

fn child(control: &Path) -> io::Result<()> {
    let expected: Identity =
        serde_json::from_value(load(&control.join("ready.json"))?["expected"].clone())?;
    let initial = retained_fs::metadata(&super::dot()?)?;
    save(&control.join("child-ready.json"), json!({"dot":initial}))?;
    wait_for(&control.join("child-go"), None)?;
    let actual = retained_fs::metadata(&super::dot()?)?;
    if actual.identity != expected || actual.kind != Kind::Directory || actual.reparse_tag.is_some()
    {
        return save(
            &control.join("child.json"),
            json!({"status":"wrong-or-reparse-child-cwd","initial":initial,"dot":actual}),
        );
    }
    // These are ordinary application operations. No handle-relative IO here:
    // the candidate must preserve relative paths as used by the real program.
    let io_result = (|| -> io::Result<()> {
        fs::create_dir("child-directory")?;
        let mut marker = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open("child-create")?;
        marker.write_all(b"child relative create\n")?;
        marker.sync_all()?;
        let mut original = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open("child-save")?;
        original.write_all(b"old bytes\n")?;
        original.sync_all()?;
        drop(original);
        let mut temporary = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open("child-save.tmp")?;
        temporary.write_all(b"atomic saved bytes\n")?;
        temporary.sync_all()?;
        drop(temporary);
        let source = units(Path::new("child-save.tmp"))?;
        let target = units(Path::new("child-save"))?;
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_REPLACE_EXISTING) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if fs::read("child-create")? != b"child relative create\n"
            || fs::read("child-save")? != b"atomic saved bytes\n"
            || Path::new("child-save.tmp").exists()
        {
            return Err(io::Error::other("relative IO postcondition failed"));
        }
        Ok(())
    })();
    save(
        &control.join("child.json"),
        json!({"status":if io_result.is_ok(){"relative-io-observed"}else{"relative-io-failed"},
        "initial":initial,"dot":actual,"io":result_value(&io_result)}),
    )?;
    io_result
}

fn attacks(
    case: &str,
    root: &Path,
    tree: &Path,
    outside: &Path,
    fixture: &Path,
) -> io::Result<Vec<Value>> {
    let outside_leaf = outside.join("leaf");
    Ok(match case {
        "positive-empty-generic-write" | "before-spawn-generic-write" => {
            vec![set_junction(&outside_leaf, root, GENERIC_WRITE)]
        }
        "positive-empty-write-attributes" | "before-spawn-write-attributes" => {
            vec![set_junction(&outside_leaf, root, FILE_WRITE_ATTRIBUTES)]
        }
        "suspended-fsctl" | "child-ready-fsctl" => vec![
            set_junction(&outside_leaf, root, GENERIC_WRITE),
            set_junction(&outside_leaf, root, FILE_WRITE_ATTRIBUTES),
            set_junction(outside, tree, GENERIC_WRITE),
            set_junction(outside, tree, FILE_WRITE_ATTRIBUTES),
        ],
        "cwd-rename-fullgraph"
        | "cwd-rename-leaf-only"
        | "ancestor-rename-fullgraph"
        | "ancestor-rename-leaf-only" => {
            let source = if case.starts_with("cwd-") { root } else { tree };
            let destination = fixture.join("moved");
            let renamed = fs::rename(source, &destination);
            let mut result =
                vec![json!({"operation":"rename-directory","result":result_value(&renamed)})];
            if renamed.is_ok() {
                fs::create_dir(source)?;
                let target = if case.starts_with("cwd-") {
                    &outside_leaf
                } else {
                    outside
                };
                result.push(set_junction(target, source, FILE_WRITE_ATTRIBUTES));
            }
            result
        }
        "anchor-unlink-and-rename-replace" => {
            let unlink = fs::remove_file(root.join(ANCHOR));
            let rename = fs::rename(root.join(ANCHOR), root.join("stolen-anchor"));
            let replacement = root.join("anchor-replacement");
            fs::write(&replacement, b"replacement must not replace held anchor")?;
            let from = units(&replacement)?;
            let to = units(&root.join(ANCHOR))?;
            let replaced =
                if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) }
                    == 0
                {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                };
            vec![
                json!({"operation":"anchor-unlink","result":result_value(&unlink)}),
                json!({"operation":"anchor-rename","result":result_value(&rename)}),
                json!({"operation":"anchor-replace","result":result_value(&replaced)}),
            ]
        }
        _ => Vec::new(),
    })
}

// The entire retained outside tree is checked, including entry names, types,
// IDs, lengths and every file's SHA-256, not just two preselected canaries.
fn snapshot(root: &Path) -> io::Result<Value> {
    fn visit(root: &Path, path: &Path, output: &mut Vec<Value>) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::other("unexpected outside symlink"));
        }
        let file = open_path(
            path,
            FILE_READ_ATTRIBUTES | READ_CONTROL,
            SHARING | FILE_SHARE_DELETE,
        )?;
        let stamp = retained_fs::metadata(&file)?;
        if stamp.reparse_tag.is_some() {
            return Err(io::Error::other("unexpected outside reparse point"));
        }
        let hash = if metadata.is_file() {
            let mut digest = Sha256::new();
            let mut input = File::open(path)?;
            let mut buffer = [0u8; 8192];
            loop {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
            Some(format!("{:x}", digest.finalize()))
        } else {
            None
        };
        output.push(json!({"path":path.strip_prefix(root).map_err(io::Error::other)?.to_string_lossy(),
            "kind":stamp.kind,"identity":stamp.identity,"length":if metadata.is_file(){Some(stamp.len)}else{None},"sha256":hash}));
        if metadata.is_dir() {
            let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                visit(root, &entry.path(), output)?;
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    visit(root, root, &mut entries)?;
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&entries)?));
    Ok(json!({"sha256":hash,"entries":entries}))
}

fn verify_io(held: &Directory) -> io::Result<()> {
    for (name, expected) in [
        ("child-create", b"child relative create\n".as_slice()),
        ("child-save", b"atomic saved bytes\n".as_slice()),
    ] {
        let mut file = held.open_regular(&Leaf::new(name)?, retained_fs::Links::Single)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes != expected {
            return Err(io::Error::other("held-root bytes differ"));
        }
    }
    let directory = held.child(&Leaf::new("child-directory")?, Traversal::SameFilesystem)?;
    if directory.metadata()?.kind != Kind::Directory
        || held.entry(&Leaf::new("child-save.tmp")?)?.is_some()
    {
        return Err(io::Error::other(
            "held-root relative IO postcondition failed",
        ));
    }
    Ok(())
}

fn one_case(base: &Path, case: &str) -> io::Result<Value> {
    let fixture = tempfile::tempdir_in(base)?.keep();
    let control = fixture.join("control");
    fs::create_dir(&control)?;
    {
        let fixture_dir = Directory::open_absolute(&fixture)?;
        let tree_dir =
            fixture_dir.create_private(&Leaf::new("tree")?, Traversal::SameFilesystem)?;
        let _leaf = tree_dir.create_private(&Leaf::new("leaf")?, Traversal::SameFilesystem)?;
        // Native private creators own DELETE access. Release all creator
        // handles before asking the worker for a NoDelete sharing lease.
    }
    let tree = fixture.join("tree");
    let root = tree.join("leaf");
    let held = Directory::open_absolute(&root)?;
    let outside = fixture.join("outside");
    fs::create_dir_all(outside.join("leaf/child-directory"))?;
    fs::write(outside.join("sentinel"), b"outside sentinel")?;
    for name in ["child-create", "child-save", "child-save.tmp", ANCHOR] {
        fs::write(outside.join("leaf").join(name), b"outside canary unchanged")?;
    }
    let before = snapshot(&outside)?;
    save(&control.join("outside-before.json"), before.clone())?;
    // All later failures are captured with this case's fixture and phase; do
    // not short-circuit the suite and lose the other attack observations.
    let observation = (|| -> io::Result<Value> {
        let worker = spawn(&[
            OsStr::new("--cwd-anchor-worker"),
            root.as_os_str(),
            control.as_os_str(),
            OsStr::new(case),
        ])?;
        resume(&worker)?;
        wait_for(&control.join("ready.json"), Some(&worker))?;
        let ready = load(&control.join("ready.json"))?;
        let expected: Identity = serde_json::from_value(ready["expected"].clone())?;
        if expected != held.identity()? {
            return Err(io::Error::other(
                "worker graph differs from observer's complete file ID",
            ));
        }
        let mut mutations = Vec::new();
        if case != "suspended-fsctl" && case != "child-ready-fsctl" {
            mutations = attacks(case, &root, &tree, &outside, &fixture)?;
        }
        save(&control.join("attacks.json"), json!(mutations))?;
        fs::write(control.join("continue"), b"go")?;
        if case == "suspended-fsctl" {
            wait_for(&control.join("suspended.json"), Some(&worker))?;
            mutations = attacks(case, &root, &tree, &outside, &fixture)?;
            save(&control.join("attacks.json"), json!(mutations))?;
            save(
                &control.join("attack-window.json"),
                json!({"phase":"suspended-before-resume","attacks":mutations}),
            )?;
            fs::write(control.join("resume"), b"go")?;
        }
        if case == "child-ready-fsctl" {
            wait_for(&control.join("child-window.json"), Some(&worker))?;
            mutations = attacks(case, &root, &tree, &outside, &fixture)?;
            save(&control.join("attacks.json"), json!(mutations))?;
            save(
                &control.join("attack-window.json"),
                json!({"phase":"child-ready-before-relative-io","attacks":mutations}),
            )?;
            fs::write(control.join("child-go"), b"go")?;
        }
        save(&control.join("attacks.json"), json!(mutations))?;
        let code = wait_exit(&worker)?;
        let result = load(&control.join("worker.json"))?;
        let positive = case.starts_with("positive-empty-");
        let attack_succeeded = mutations
            .iter()
            .any(|value| value["result"]["succeeded"] == true);
        let blocked_as_expected = !mutations.is_empty()
            && mutations.iter().all(|value| {
                value["result"]["succeeded"] == false
                    && matches!(
                        value["result"]["error"]["native_error"].as_i64(),
                        Some(5 | 32 | 145)
                    )
            });
        let io_check = if positive { Ok(()) } else { verify_io(&held) };
        let bound = result["child"]["status"] == "relative-io-observed"
            && result["child_exit"] == 0
            && io_check.is_ok();
        let positive_confirmed = attack_succeeded
            && result["held_after"]["kind"] == "Link"
            && result["held_after"]["reparse_tag"] == 0xA0000003u32;
        let classification = if code != 0 {
            "worker-failed"
        } else if positive && positive_confirmed {
            "positive-control-observed"
        } else if positive {
            "positive-control-failed"
        } else if attack_succeeded {
            "counterexample-candidate-mutation-succeeded"
        } else if !bound {
            "relative-io-or-cwd-failed"
        } else if case == "baseline" {
            "baseline-observed"
        } else if blocked_as_expected {
            "candidate-block-observed-on-this-host"
        } else {
            "unknown-attack-rejection"
        };
        Ok(
            json!({"classification":classification,"ready":ready,"attacks":mutations,
            "worker_exit":code,"worker":result,"held_root_io":result_value(&io_check)}),
        )
    })();
    let after = snapshot(&outside);
    let unchanged = after.as_ref().is_ok_and(|value| *value == before);
    let mut result = match observation {
        Ok(result) => result,
        Err(error) => {
            json!({"classification":"inconclusive-stage-error","error":error_value(&error)})
        }
    };
    result["case"] = json!(case);
    result["fixture"] = json!(fixture);
    result["diagnostic_only"] = json!(true);
    result["phase"] = load(&control.join("phase.json")).unwrap_or(Value::Null);
    result["worker_error"] = load(&control.join("worker-error.json")).unwrap_or(Value::Null);
    result["child_error"] = load(&control.join("child-error.json")).unwrap_or(Value::Null);
    result["retained_attacks"] = load(&control.join("attacks.json")).unwrap_or(Value::Null);
    result["retained_child"] = load(&control.join("child.json")).unwrap_or(Value::Null);
    result["outside_before"] = before;
    result["outside_after"] = match after {
        Ok(value) => value,
        Err(error) => json!({"error":error_value(&error)}),
    };
    result["outside_unchanged"] = json!(unchanged);
    if !unchanged {
        result["classification"] = json!("outside-tree-changed-or-unreadable");
    }
    save(&control.join("result.json"), result.clone())?;
    Ok(result)
}

pub(super) fn run() -> io::Result<()> {
    let base =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/windows-cwd-anchor-2026-10-31");
    fs::create_dir_all(&base)?;
    let base = fs::canonicalize(base)?;
    let mut failed = 0;
    for case in CASES {
        let result = one_case(&base, case).unwrap_or_else(|error|
            json!({"case":case,"classification":"inconclusive-fixture-error","error":error_value(&error)}));
        if !matches!(
            result["classification"].as_str(),
            Some(
                "positive-control-observed"
                    | "baseline-observed"
                    | "candidate-block-observed-on-this-host"
            )
        ) {
            failed += 1;
        }
        println!("{result}");
    }
    let summary = json!({"diagnostic_completed":true,"cases":CASES.len(),"failed_or_inconclusive":failed,
        "production_implementation_changed":false,"pinned_cwd_claim":false,"safety_proof":false,
        "positive_controls_required":true});
    save(&base.join("summary.json"), summary.clone())?;
    println!("{summary}");
    if failed != 0 {
        return Err(io::Error::other(format!("cwd anchor candidate unresolved/rejected: {failed} failed or inconclusive cases; evidence {}", base.display())));
    }
    Ok(())
}

pub(super) fn dispatch() -> Option<i32> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    // This opt-in executable invocation measures only the twelve new cases.
    // The existing diagnostic and the ordinary lifecycle suite keep their own
    // independent entry paths when this environment flag is absent.
    if enabled() && args.is_empty() {
        return Some(match run() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("cwd anchor diagnostic: {error}");
                2
            }
        });
    }
    let worker_mode = args.first().is_some_and(|arg| arg == "--cwd-anchor-worker");
    let child_mode = args.first().is_some_and(|arg| arg == "--cwd-anchor-child");
    if !worker_mode && !child_mode {
        return None;
    }
    if !enabled() {
        return Some(2);
    }
    let control = if worker_mode {
        args.get(2)
    } else {
        args.get(1)
    }
    .map(PathBuf::from);
    let result = if worker_mode && args.len() == 4 {
        worker(
            Path::new(&args[1]),
            control.as_ref().unwrap(),
            &args[3].to_string_lossy(),
        )
    } else if child_mode && args.len() == 2 {
        child(control.as_ref().unwrap())
    } else {
        Err(io::Error::other("invalid cwd anchor dispatch"))
    };
    if let Err(error) = &result {
        if let Some(control) = control {
            let target = if worker_mode {
                "worker-error.json"
            } else {
                "child-error.json"
            };
            let _ = save(
                &control.join(target),
                json!({"error":error_value(error),"phase":load(&control.join("phase.json")).ok()}),
            );
        }
        eprintln!("cwd anchor diagnostic: {error}");
    }
    Some(if result.is_ok() { 0 } else { 2 })
}
