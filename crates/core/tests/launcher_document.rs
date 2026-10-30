//! Exercise the production fixed-document CAS port without Tauri or real data.
#[cfg(windows)]
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
#[cfg(windows)]
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_document.rs"]
mod launcher_document;

use launcher_document as document;
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use std::{fs, path::PathBuf};

struct Fixture {
    _temp: tempfile::TempDir,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let parent = Directory::open_absolute(&root).unwrap();
        let project = parent
            .create_private(&Leaf::new("project").unwrap(), Traversal::AllowMounts)
            .unwrap();
        drop(project);
        Self {
            _temp: temp,
            path: root.join("project"),
        }
    }
    fn open(&self) -> document::Directory {
        document::Directory::open_preferences(&self.path).unwrap()
    }
    fn write(&self, bytes: &[u8]) -> document::Snapshot {
        let project = self.open();
        let mut expected = document::snapshot(&project).unwrap().0;
        let storage = document::prepare_directory(&project, &mut expected).unwrap();
        let _lock = storage.write_lock().unwrap();
        document::persist(
            &project,
            &self.path,
            &storage,
            &expected,
            bytes,
            ".test-stage",
            || Ok(()),
        )
        .unwrap()
    }
}

#[test]
fn read_only_missing_document_creates_nothing_and_fixed_stores_are_independent() {
    let f = Fixture::new();
    assert!(!document::snapshot(&f.open()).unwrap().0.has_file());
    assert!(fs::read_dir(&f.path).unwrap().next().is_none());
    f.write(br#"{"schema_version":1,"preferences":{}}"#);
    let favorites = document::Directory::open_favorites(&f.path).unwrap();
    let mut expected = document::snapshot(&favorites).unwrap().0;
    assert!(!expected.has_file());
    let storage = document::prepare_directory(&favorites, &mut expected).unwrap();
    let _lock = storage.write_lock().unwrap();
    document::persist(
        &favorites,
        &f.path,
        &storage,
        &expected,
        b"favorites",
        ".favorites-stage",
        || Ok(()),
    )
    .unwrap();
    assert_eq!(
        document::snapshot(&f.open()).unwrap().1.unwrap(),
        br#"{"schema_version":1,"preferences":{}}"#
    );
    assert_eq!(
        document::snapshot(&favorites).unwrap().1.unwrap(),
        b"favorites"
    );
}

#[test]
fn repeated_save_reopens_actual_current_bytes_and_rejects_stale_snapshot() {
    let f = Fixture::new();
    let old = f.write(b"first");
    let current = f.write(b"second");
    assert_ne!(old, current);
    let project = f.open();
    assert_eq!(document::snapshot(&project).unwrap().1.unwrap(), b"second");
    let storage = project.ensure_storage().unwrap();
    let _lock = storage.write_lock().unwrap();
    assert!(document::persist(
        &project,
        &f.path,
        &storage,
        &old,
        b"stale",
        ".stale",
        || Ok(())
    )
    .is_err());
    assert_eq!(document::snapshot(&project).unwrap().1.unwrap(), b"second");
    // Unix keeps its original public physical JSON contract. Windows preserves
    // the first physical version as a permanent fence for logical revisions.
    #[cfg(unix)]
    assert_eq!(
        fs::read(f.path.join(".pcl-rust/launcher-preferences.json")).unwrap(),
        b"second"
    );
    #[cfg(windows)]
    assert_eq!(
        fs::read(f.path.join(".pcl-rust/launcher-preferences.json")).unwrap(),
        b"first"
    );
}

#[test]
fn final_save_race_preserves_outside_edit() {
    let f = Fixture::new();
    let expected = f.write(b"original");
    let project = f.open();
    let storage = project.ensure_storage().unwrap();
    let _lock = storage.write_lock().unwrap();
    let path = f.path.join(".pcl-rust/launcher-preferences.json");
    assert!(document::persist(
        &project,
        &f.path,
        &storage,
        &expected,
        b"candidate",
        ".race",
        || { fs::write(&path, b"external editor").map_err(|e| e.to_string()) }
    )
    .is_err());
    assert_eq!(fs::read(path).unwrap(), b"external editor");
}

#[test]
fn writer_lock_is_nonblocking_and_released_before_directory_handles() {
    let f = Fixture::new();
    let storage = f.open().ensure_storage().unwrap();
    let held = storage.write_lock().unwrap();
    assert!(storage.write_lock().is_err());
    drop(held);
    let _next = storage.write_lock().unwrap();
}

#[test]
fn export_publishes_exact_bytes_without_replacing_existing_target() {
    let f = Fixture::new();
    let target = f.open();
    target
        .publish_anonymous(&f.path, "export.json", b"export bytes")
        .unwrap();
    assert_eq!(
        fs::read(f.path.join("export.json")).unwrap(),
        b"export bytes"
    );
    assert!(target
        .publish_anonymous(&f.path, "export.json", b"replacement")
        .is_err());
    assert_eq!(
        fs::read(f.path.join("export.json")).unwrap(),
        b"export bytes"
    );
    assert!(target
        .publish_anonymous(&f.path, "../escape.json", b"escape")
        .is_err());
    assert!(!f.path.parent().unwrap().join("escape.json").exists());
}

#[test]
fn input_limit_and_project_replacement_reject_before_publication() {
    let f = Fixture::new();
    let expected = f.write(b"original");
    let project = f.open();
    let storage = project.ensure_storage().unwrap();
    assert!(document::persist(
        &project,
        &f.path,
        &storage,
        &expected,
        &vec![0; 65537],
        ".too-large",
        || panic!("limit must reject first")
    )
    .is_err());
    #[cfg(unix)]
    {
        fs::rename(&f.path, f.path.with_file_name("retained-project")).unwrap();
        fs::create_dir(&f.path).unwrap();
        assert!(document::verify(&project, &f.path, &expected).is_err());
        assert_eq!(
            storage
                .read_bytes("launcher-preferences.json")
                .unwrap()
                .unwrap(),
            b"original"
        );
    }
}

#[cfg(unix)]
#[test]
fn literal_symlink_and_hard_link_documents_are_rejected_without_changing_targets() {
    let f = Fixture::new();
    let storage = f.open().ensure_storage().unwrap();
    let outside = f.path.join("outside");
    fs::write(&outside, b"outside bytes").unwrap();
    let document_path = f.path.join(".pcl-rust/launcher-preferences.json");
    std::os::unix::fs::symlink(&outside, &document_path).unwrap();
    assert!(document::snapshot(&f.open()).is_err());
    fs::remove_file(&document_path).unwrap();
    fs::hard_link(&outside, &document_path).unwrap();
    assert!(document::snapshot(&f.open()).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"outside bytes");
    drop(storage);
}

#[cfg(unix)]
#[test]
fn export_rejects_substituted_visible_parent_and_writes_no_backup() {
    let f = Fixture::new();
    let target = f.open();
    let retained = f.path.with_file_name("retained-project");
    fs::rename(&f.path, &retained).unwrap();
    fs::create_dir(&f.path).unwrap();
    assert!(target
        .publish_anonymous(&f.path, "export.json", b"export bytes")
        .is_err());
    assert!(!f.path.join("export.json").exists());
    assert!(!retained.join("export.json").exists());
}

#[cfg(windows)]
#[test]
fn chosen_managed_basename_reads_physical_file_instead_of_current_logical_revision() {
    let f = Fixture::new();
    f.write(b"physical selected bytes");
    f.write(b"logical application bytes");
    let project = f.open();
    let storage = project.ensure_storage().unwrap();
    assert_eq!(
        storage
            .read_bytes("launcher-preferences.json")
            .unwrap()
            .unwrap(),
        b"physical selected bytes"
    );
    assert_eq!(
        document::snapshot(&project).unwrap().1.unwrap(),
        b"logical application bytes"
    );
}

#[cfg(windows)]
#[test]
fn chosen_physical_backup_allows_broad_parent_and_file_without_permission_repair() {
    use std::io::Write;
    let f = Fixture::new();
    let parent = Directory::open_absolute(&f.path).unwrap();
    let name = Leaf::new("launcher-preferences.json").unwrap();
    let mut file = parent.create_file(&name).unwrap();
    file.write_all(b"selected backup").unwrap();
    file.sync_all().unwrap();
    drop(file);
    broaden_fixture_dacl(&f.path.join("launcher-preferences.json"), false);
    broaden_fixture_dacl(&f.path, true);
    let before_parent = parent.metadata().unwrap();
    let before_file = parent.entry(&name).unwrap().unwrap();
    assert!(before_parent.require_private().is_err());
    assert!(before_file.require_private().is_err());
    assert_eq!(
        f.open()
            .read_bytes("launcher-preferences.json")
            .unwrap()
            .unwrap(),
        b"selected backup"
    );
    assert_eq!(parent.metadata().unwrap(), before_parent);
    assert_eq!(parent.entry(&name).unwrap().unwrap(), before_file);
    assert!(parent
        .entry(&Leaf::new(".local-versions-launcher-preferences.json").unwrap())
        .unwrap()
        .is_none());
}

// DACL-only edits apply exclusively to new synthetic fixtures, preserving the
// native TokenUser owner. No production admission policy is loosened.
#[cfg(windows)]
fn broaden_fixture_dacl(path: &std::path::Path, directory: bool) {
    use std::{
        ffi::c_void,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SetSecurityInfo,
                SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, WRITE_DAC,
        },
    };
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        )
        .open(path)
        .unwrap();
    let before = pcl_core::retained_fs::metadata(&file).unwrap();
    struct Descriptor(*mut c_void);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    let sddl: Vec<u16> = "D:P(A;;FA;;;WD)".encode_utf16().chain(Some(0)).collect();
    let mut descriptor = Descriptor(ptr::null_mut());
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor.0,
                ptr::null_mut(),
            )
        },
        0
    );
    let mut present = 0;
    let mut dacl = ptr::null_mut();
    let mut defaulted = 0;
    assert_ne!(
        unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) },
        0
    );
    assert_ne!(present, 0);
    assert!(!dacl.is_null());
    assert_eq!(
        unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        },
        0
    );
    let after = pcl_core::retained_fs::metadata(&file).unwrap();
    assert_eq!(before.identity, after.identity);
    match (&before.security, &after.security) {
        (
            pcl_core::retained_fs::Security::Windows { owner: left, .. },
            pcl_core::retained_fs::Security::Windows { owner: right, .. },
        ) => assert_eq!(left, right),
        _ => panic!("expected Windows native security"),
    }
    after.require_owned().unwrap();
    assert!(after.require_private().is_err());
}
