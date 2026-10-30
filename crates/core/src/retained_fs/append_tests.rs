//! Private native filesystem fixtures. No shell, privilege or user data needed.
use super::*;
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
};

fn fixture() -> tempfile::TempDir {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/native-append-tests");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap()
}
fn leaf(name: &str) -> Leaf {
    Leaf::new(name).unwrap()
}
fn directory(path: &Path) -> Directory {
    Directory::open_absolute(path).unwrap()
}
fn bytes(directory: &Directory, name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    directory
        .open_regular(&leaf(name), Links::Single)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}
#[cfg(windows)]
#[path = "../file_ops/junction_fixture.rs"]
mod junction_fixture;
fn directory_link(target: &Path, path: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path).unwrap();
    #[cfg(windows)]
    junction_fixture::create(target, path);
}

#[test]
fn append_creates_privately_keeps_existing_bytes_and_ignores_seek_position() {
    let fixture = fixture();
    let root = directory(fixture.path());
    let mut log = root.open_append(&leaf("输出.log"), 64).unwrap();
    let created = metadata(&log).unwrap();
    created.require_private().unwrap();
    assert_eq!(created.links, 1);
    assert_eq!(created.kind, Kind::Regular);
    assert_eq!(created.len, 0);
    log.write_all(b"original").unwrap();
    log.sync_all().unwrap();
    drop(log);
    let mut log = root.open_append(&leaf("输出.log"), 8).unwrap();
    assert!(metadata(&log).unwrap().same_object(&created));
    log.seek(SeekFrom::Start(0)).unwrap();
    log.write_all(b" appended").unwrap();
    log.sync_all().unwrap();
    drop(log);
    assert_eq!(bytes(&root, "输出.log"), b"original appended");
    assert_eq!(
        root.names(10).unwrap(),
        vec![std::ffi::OsString::from("输出.log")]
    );
    assert!(root.open_append(&leaf("输出.log"), 16).is_err());
    assert_eq!(bytes(&root, "输出.log"), b"original appended");
    assert!(root.open_append(&leaf("输出.log"), 17).is_ok());
}

#[test]
fn append_refuses_links_multiple_hardlinks_directories_and_oversized_eof() {
    let fixture = fixture();
    let root = directory(fixture.path());
    let outside = root
        .create_private(&leaf("outside"), Traversal::SameFilesystem)
        .unwrap();
    let mut original = outside.open_append(&leaf("retained.log"), 16).unwrap();
    original.write_all(b"outside").unwrap();
    drop(original);
    directory_link(
        &fixture.path().join("outside"),
        &fixture.path().join("link.log"),
    );
    assert!(root.open_append(&leaf("link.log"), 16).is_err());
    assert_eq!(bytes(&outside, "retained.log"), b"outside");
    let mut log = root.open_append(&leaf("original.log"), 16).unwrap();
    log.write_all(b"keep").unwrap();
    drop(log);
    fs::hard_link(
        fixture.path().join("original.log"),
        fixture.path().join("alias.log"),
    )
    .unwrap();
    assert!(root.open_append(&leaf("original.log"), 16).is_err());
    assert!(root.open_append(&leaf("alias.log"), 16).is_err());
    assert_eq!(
        fs::read(fixture.path().join("original.log")).unwrap(),
        b"keep"
    );
    root.create_private(&leaf("directory.log"), Traversal::SameFilesystem)
        .unwrap();
    assert!(root.open_append(&leaf("directory.log"), 16).is_err());
    assert!(root.open_append(&leaf("outside"), 16).is_err());
}

#[test]
#[cfg(unix)]
fn append_refuses_nonprivate_files_file_symlinks_and_fifo_without_blocking() {
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{symlink, PermissionsExt},
    };
    let fixture = fixture();
    let root = directory(fixture.path());
    let mut file = root.open_append(&leaf("original.log"), 16).unwrap();
    file.write_all(b"retained").unwrap();
    drop(file);
    symlink(
        fixture.path().join("original.log"),
        fixture.path().join("alias.log"),
    )
    .unwrap();
    assert!(root.open_append(&leaf("alias.log"), 16).is_err());
    fs::set_permissions(
        fixture.path().join("original.log"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(root.open_append(&leaf("original.log"), 16).is_err());
    assert_eq!(
        fs::read(fixture.path().join("original.log")).unwrap(),
        b"retained"
    );
    let fifo =
        std::ffi::CString::new(fixture.path().join("fifo.log").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let start = std::time::Instant::now();
    assert!(root.open_append(&leaf("fifo.log"), 16).is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn append_uses_held_parent_after_path_is_renamed_and_replaced_with_a_link() {
    let fixture = fixture();
    let root = directory(fixture.path());
    let parent = root
        .create_private(&leaf("parent"), Traversal::SameFilesystem)
        .unwrap();
    let outside = root
        .create_private(&leaf("outside"), Traversal::SameFilesystem)
        .unwrap();
    let mut log = parent.open_append(&leaf("game.log"), 16).unwrap();
    log.write_all(b"first").unwrap();
    drop(log);
    let mut sentinel = outside.open_append(&leaf("game.log"), 16).unwrap();
    sentinel.write_all(b"outside").unwrap();
    drop(sentinel);
    fs::rename(fixture.path().join("parent"), fixture.path().join("moved")).unwrap();
    directory_link(
        &fixture.path().join("outside"),
        &fixture.path().join("parent"),
    );
    let mut log = parent.open_append(&leaf("game.log"), 16).unwrap();
    log.write_all(b" second").unwrap();
    drop(log);
    assert_eq!(bytes(&parent, "game.log"), b"first second");
    assert_eq!(bytes(&outside, "game.log"), b"outside");
    assert_eq!(
        fs::read(fixture.path().join("moved/game.log")).unwrap(),
        b"first second"
    );
    assert!(root.child(&leaf("parent"), Traversal::AllowMounts).is_err());
}

#[test]
fn shared_probe_detects_only_exclusive_locks_and_releases_its_own_shared_lease() {
    let fixture = fixture();
    let root = directory(fixture.path());
    // Public probe accepts a held existing file and never creates a filename.
    assert!(root.entry(&leaf("missing.lock")).unwrap().is_none());
    let exclusive = root.lock(&leaf("monitor.lock")).unwrap();
    let probe = root
        .open_regular(&leaf("monitor.lock"), Links::Single)
        .unwrap();
    let duplicate = probe.try_clone().unwrap();
    assert!(shared_lock_is_held(&probe).unwrap());
    drop(exclusive);
    assert!(!shared_lock_is_held(&probe).unwrap());
    // Prove a shared reader is compatible; an exclusive probe would report
    // this reader busy and would violate the monitor admission protocol.
    let reader = root
        .open_regular(&leaf("monitor.lock"), Links::Single)
        .unwrap();
    assert!(native::try_shared_lock(&reader).unwrap());
    assert!(!shared_lock_is_held(&probe).unwrap());
    assert!(root.lock(&leaf("monitor.lock")).is_err());
    native::unlock_checked(&reader).unwrap();
    // The duplicate remains live, so success proves an explicit unlock rather
    // than relying on close to release the previous successful shared probe.
    let exclusive = root.lock(&leaf("monitor.lock")).unwrap();
    assert!(shared_lock_is_held(&probe).unwrap());
    drop(exclusive);
    drop(duplicate);
    assert!(!shared_lock_is_held(&probe).unwrap());
    assert_eq!(
        root.names(10).unwrap(),
        vec![std::ffi::OsString::from("monitor.lock")]
    );
}

#[test]
fn pure_absolute_validation_rejects_dot_parent_relative_and_device_namespaces() {
    #[cfg(unix)]
    {
        for path in ["/", "/tmp/fixture logs/game.log"] {
            validate_absolute_path(Path::new(path)).unwrap();
        }
        for path in [
            "relative",
            "./relative",
            "/tmp/./game.log",
            "/tmp/../game.log",
        ] {
            assert!(validate_absolute_path(Path::new(path)).is_err(), "{path}");
        }
    }
    #[cfg(windows)]
    {
        for path in [
            r"C:\",
            r"C:\Games\实例",
            r"\\server\share\Games",
            r"\\?\C:\Games",
            r"\\?\UNC\server\share\Games",
        ] {
            validate_absolute_path(Path::new(path)).unwrap();
        }
        for path in [
            r"C:relative",
            r"\rooted",
            r"C:\Games\.\log",
            r"C:\Games\..\log",
            r"\\.\pipe\named",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1",
        ] {
            assert!(validate_absolute_path(Path::new(path)).is_err(), "{path}");
        }
    }
}
