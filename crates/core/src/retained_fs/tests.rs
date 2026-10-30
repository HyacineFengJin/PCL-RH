use super::*;
use std::{
    fs,
    io::{Read, Write},
};

fn fixture() -> tempfile::TempDir {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/retained-fs-tests");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap()
}
fn leaf(name: &str) -> Leaf {
    Leaf::new(name).unwrap()
}
fn dir(path: &Path) -> Directory {
    Directory::open_absolute(path).unwrap()
}

#[test]
fn leaf_rejects_paths_and_directory_admission_rejects_relative_roots() {
    for name in ["", ".", "..", "../file", "a/b", "a\\b", "a\0b"] {
        assert!(Leaf::new(name).is_err(), "{name:?}");
    }
    assert!(Directory::open_absolute(Path::new("relative")).is_err());
    assert!(Leaf::new("a normal 实例.jar").is_ok());
    #[cfg(windows)]
    for name in [
        "file:stream",
        "C:relative",
        "NUL",
        "CON.txt",
        "tail.",
        "tail ",
        "file?",
    ] {
        assert!(Leaf::new(name).is_err(), "{name:?}");
    }
}

#[test]
fn private_creation_read_and_native_identity_are_bound_to_actual_handles() {
    let fixture = fixture();
    let root = dir(fixture.path());
    let owned = root
        .create_private(&leaf("private"), Traversal::SameFilesystem)
        .unwrap();
    owned.metadata().unwrap().require_private().unwrap();
    let mut file = owned.create_file(&leaf("配置.json")).unwrap();
    file.write_all(b"complete bytes").unwrap();
    file.sync_all().unwrap();
    let written = metadata(&file).unwrap();
    written.require_private().unwrap();
    assert_eq!(written.kind, Kind::Regular);
    assert_eq!(written.links, 1);
    drop(file);
    let mut file = owned
        .open_regular(&leaf("配置.json"), Links::Single)
        .unwrap();
    let stamp = metadata(&file).unwrap();
    assert!(stamp.same_object(&written));
    assert_eq!(stamp.len, written.len);
    stamp.require_private().unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"complete bytes");
    assert_eq!(metadata(&file).unwrap(), stamp);
    owned.recheck(&leaf("配置.json"), &stamp.identity).unwrap();
    assert_eq!(
        owned.entry(&leaf("配置.json")).unwrap().unwrap().identity,
        stamp.identity
    );
    assert!(owned.entry(&leaf("absent")).unwrap().is_none());
    assert!(owned.create_file(&leaf("配置.json")).is_err());
    assert!(root
        .create_private(&leaf("private"), Traversal::SameFilesystem)
        .is_err());
    assert_eq!(
        fs::read(fixture.path().join("private/配置.json")).unwrap(),
        b"complete bytes"
    );
}

#[test]
fn renamed_parent_and_replacement_never_redirect_the_held_directory() {
    let fixture = fixture();
    fs::create_dir(fixture.path().join("parent")).unwrap();
    let held = dir(&fixture.path().join("parent"));
    let identity = held.identity().unwrap();
    fs::rename(
        fixture.path().join("parent"),
        fixture.path().join("detached"),
    )
    .unwrap();
    fs::create_dir(fixture.path().join("parent")).unwrap();
    fs::write(fixture.path().join("parent/untouched"), b"replacement").unwrap();
    let mut file = held.create_file(&leaf("owned")).unwrap();
    file.write_all(b"retained").unwrap();
    drop(file);
    assert_eq!(held.identity().unwrap(), identity);
    assert_eq!(
        fs::read(fixture.path().join("detached/owned")).unwrap(),
        b"retained"
    );
    assert!(!fixture.path().join("parent/owned").exists());
    assert_eq!(
        fs::read(fixture.path().join("parent/untouched")).unwrap(),
        b"replacement"
    );
    assert_ne!(
        dir(&fixture.path().join("parent")).identity().unwrap(),
        identity
    );
}

#[cfg(windows)]
#[path = "../file_ops/junction_fixture.rs"]
mod junction_fixture;
fn link(target: &Path, path: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path).unwrap();
    #[cfg(windows)]
    junction_fixture::create(target, path);
}
#[test]
fn links_are_metadata_only_and_cannot_become_directory_or_file_authority() {
    let fixture = fixture();
    fs::create_dir(fixture.path().join("outside")).unwrap();
    fs::write(fixture.path().join("outside/retained"), b"outside").unwrap();
    link(
        &fixture.path().join("outside"),
        &fixture.path().join("link"),
    );
    let root = dir(fixture.path());
    assert_eq!(root.entry(&leaf("link")).unwrap().unwrap().kind, Kind::Link);
    assert!(root.child(&leaf("link"), Traversal::AllowMounts).is_err());
    assert!(root.open_regular(&leaf("link"), Links::Any).is_err());
    assert!(Directory::open_absolute(&fixture.path().join("link")).is_err());
    assert!(root
        .create_private(&leaf("link"), Traversal::SameFilesystem)
        .is_err());
    assert_eq!(
        fs::read(fixture.path().join("outside/retained")).unwrap(),
        b"outside"
    );
}

#[cfg(windows)]
#[test]
fn held_directory_in_place_junction_conversion_cannot_redirect_children() {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};

    // This guard restores the directory even if a security assertion fails.
    // Otherwise successful operations against the literal directory behind a
    // junction could leave hidden fixture children during TempDir cleanup.
    struct RestoreJunction(Option<std::path::PathBuf>);
    impl Drop for RestoreJunction {
        fn drop(&mut self) {
            if let Some(path) = &self.0 {
                let _ = junction_fixture::clear_existing(path);
            }
        }
    }

    for private_creation in [false, true] {
        let fixture = fixture();
        let outside = fixture.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(outside.join("outside-child")).unwrap();
        fs::write(
            outside.join("outside-marker"),
            b"outside must stay unchanged",
        )
        .unwrap();
        fs::write(
            outside.join("outside-child/nested"),
            b"nested outside bytes",
        )
        .unwrap();

        // Prove that junction creation/removal works here without shell,
        // administrator rights, Developer Mode or a symlink privilege. Only
        // the second conversion is performed while Directory retains a handle.
        let control = fixture.path().join("unheld-control");
        junction_fixture::create(&outside, &control);
        junction_fixture::clear_existing(&control).unwrap();
        fs::remove_dir(&control).unwrap();

        let root = dir(fixture.path());
        let path = fixture.path().join("held");
        let held = if private_creation {
            root.create_private(&leaf("held"), Traversal::SameFilesystem)
                .unwrap()
        } else {
            fs::create_dir(&path).unwrap();
            root.child(&leaf("held"), Traversal::SameFilesystem)
                .unwrap()
        };
        let identity = held.identity().unwrap();
        let mut restore = RestoreJunction(None);
        match junction_fixture::set_existing(&outside, &path) {
            Ok(()) => {
                restore.0 = Some(path.clone());
                assert_eq!(
                    root.entry(&leaf("held")).unwrap().unwrap().kind,
                    Kind::Link,
                    "fixture did not attach junction metadata to the original directory"
                );
                assert_eq!(held.identity().unwrap(), identity);
            }
            Err(error) if matches!(error.raw_os_error(), Some(code) if code == ERROR_ACCESS_DENIED as i32 || code == ERROR_SHARING_VIOLATION as i32) =>
            {
                eprintln!(
                    "in-place junction conversion rejected by native handle policy (private={private_creation}): {error}"
                );
                assert_eq!(held.metadata().unwrap().kind, Kind::Directory);
            }
            Err(error) => panic!("unexpected in-place junction fixture error: {error}"),
        }

        // Opening a single final leaf with OPEN_REPARSE_POINT is insufficient
        // evidence on its own: the held parent object was changed in place.
        // Reads/enumeration must not expose the referent and writes must never
        // create entries in it. A native refusal is an acceptable safe result.
        let exposed_file = held
            .open_regular(&leaf("outside-marker"), Links::Any)
            .is_ok();
        let exposed_entry = held
            .entry(&leaf("outside-marker"))
            .is_ok_and(|entry| entry.is_some());
        let exposed_child = held
            .child(&leaf("outside-child"), Traversal::SameFilesystem)
            .is_ok();
        let exposed_names = held.names(32).is_ok_and(|names| {
            names.contains(&OsString::from("outside-marker"))
                || names.contains(&OsString::from("outside-child"))
        });
        let created_file = held.create_file(&leaf("created-through-held"));
        if let Ok(mut file) = created_file {
            file.write_all(b"held object write").unwrap();
        }
        let _created_directory =
            held.create_private(&leaf("directory-through-held"), Traversal::SameFilesystem);
        let lock = held.lock(&leaf("lock-through-held"));
        drop(lock);
        if let Ok(reopened) = held.reopen(Traversal::SameFilesystem) {
            assert_eq!(reopened.identity().unwrap(), identity);
            if let Ok(mut file) = reopened.create_file(&leaf("created-through-reopen")) {
                file.write_all(b"reopened object write").unwrap();
            }
        }

        let outside_names = dir(&outside).names(32).unwrap();
        assert_eq!(
            fs::read(outside.join("outside-marker")).unwrap(),
            b"outside must stay unchanged"
        );
        assert_eq!(
            fs::read(outside.join("outside-child/nested")).unwrap(),
            b"nested outside bytes"
        );
        assert_eq!(
            outside_names,
            vec![
                OsString::from("outside-child"),
                OsString::from("outside-marker")
            ],
            "held-parent operations wrote into the junction referent (private={private_creation})"
        );
        assert!(
            !(exposed_file || exposed_entry || exposed_child || exposed_names),
            "held-parent operations exposed the junction referent (private={private_creation}, file={exposed_file}, entry={exposed_entry}, child={exposed_child}, names={exposed_names})"
        );
        if restore.0.is_some() {
            junction_fixture::clear_existing(&path).unwrap();
            restore.0 = None;
        }
    }
}

#[test]
fn single_link_admission_and_identity_rechecks_reject_replacements() {
    let fixture = fixture();
    let root = dir(fixture.path());
    fs::write(fixture.path().join("file"), b"original").unwrap();
    let old = root.open_regular(&leaf("file"), Links::Single).unwrap();
    let id = metadata(&old).unwrap().identity;
    fs::hard_link(fixture.path().join("file"), fixture.path().join("alias")).unwrap();
    assert!(root.open_regular(&leaf("file"), Links::Single).is_err());
    assert!(root.open_regular(&leaf("file"), Links::Any).is_ok());
    fs::rename(fixture.path().join("file"), fixture.path().join("retained")).unwrap();
    fs::write(fixture.path().join("file"), b"replacement").unwrap();
    assert!(root.recheck(&leaf("file"), &id).is_err());
    assert_eq!(metadata(&old).unwrap().identity, id);
    assert_eq!(
        fs::read(fixture.path().join("retained")).unwrap(),
        b"original"
    );
}

#[test]
fn enumeration_is_bounded_independent_and_preserves_native_names() {
    let fixture = fixture();
    let root = dir(fixture.path());
    fs::write(fixture.path().join("a"), b"a").unwrap();
    fs::write(fixture.path().join("实例"), b"b").unwrap();
    assert!(root.names(1).is_err());
    assert!(root.names(0).is_err());
    let first = root.names(10).unwrap();
    let second = root.names(10).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let name = OsStr::from_bytes(b"native-\xff");
        fs::write(fixture.path().join(name), b"c").unwrap();
        assert!(root.names(10).unwrap().contains(&name.to_owned()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let name = OsString::from_wide(&[110, 45, 0xd800]);
        fs::write(fixture.path().join(&name), b"c").unwrap();
        assert!(root.names(10).unwrap().contains(&name));
    }
}

#[test]
fn lock_is_exclusive_and_explicitly_unlocked_while_a_duplicate_handle_survives() {
    let fixture = fixture();
    let root = dir(fixture.path());
    let lock = root.lock(&leaf("lease.lock")).unwrap();
    let duplicate = lock.file().try_clone().unwrap();
    assert!(root.lock(&leaf("lease.lock")).is_err());
    drop(lock);
    let next = root.lock(&leaf("lease.lock")).unwrap();
    assert!(root.lock(&leaf("lease.lock")).is_err());
    drop(next);
    drop(duplicate);
    assert!(root.lock(&leaf("lease.lock")).is_ok());
}

#[test]
fn reopens_same_filesystem_without_reconstructing_a_path() {
    let fixture = fixture();
    let root = dir(fixture.path());
    assert_eq!(
        root.reopen(Traversal::SameFilesystem)
            .unwrap()
            .identity()
            .unwrap(),
        root.identity().unwrap()
    );
    let clone = Directory::from_file(root.try_clone_file().unwrap()).unwrap();
    assert_eq!(clone.identity().unwrap(), root.identity().unwrap());
    #[cfg(unix)]
    root.sync().unwrap();
    // Windows directory flush capability is deliberately not upgraded to a
    // success if the actual filesystem rejects it. Publication must handle it.
    #[cfg(windows)]
    {
        let _actual_native_result = root.sync();
    }
}

#[cfg(unix)]
#[test]
fn existing_broad_permissions_are_rejected_without_repairing_them() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture();
    fs::create_dir(fixture.path().join("shared")).unwrap();
    fs::set_permissions(
        fixture.path().join("shared"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let shared = dir(&fixture.path().join("shared"));
    assert!(shared.metadata().unwrap().require_private().is_err());
    assert_eq!(
        fs::metadata(fixture.path().join("shared"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}
