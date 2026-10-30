//! Native Windows lease tests use only private, caller-created temporary data.
use super::*;
use std::{
    fs,
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

fn leaf(name: &str) -> Leaf {
    Leaf::new(name).unwrap()
}
fn fixture() -> (tempfile::TempDir, Directory) {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/native-read-lease-tests");
    fs::create_dir_all(&work).unwrap();
    let temporary = tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap();
    let parent = Directory::open_absolute(temporary.path()).unwrap();
    let directory = parent
        .create_private(&leaf("private"), Traversal::SameFilesystem)
        .unwrap();
    (temporary, directory)
}
fn write(directory: &Directory, name: &str, bytes: &[u8]) {
    let mut file = directory.create_file(&leaf(name)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
#[test]
fn read_lease_blocks_existing_and_new_writers_and_delete_but_allows_ordinary_readers() {
    let (temporary, directory) = fixture();
    write(&directory, "base", b"permanent");
    let path = temporary.path().join("private/base");
    let editor = fs::OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&path)
        .unwrap();
    assert!(directory.read_lease(&leaf("base"), false).is_err());
    drop(editor);
    let ordinary = directory
        .open_regular(&leaf("base"), Links::Single)
        .unwrap();
    let held = directory.read_lease(&leaf("base"), false).unwrap();
    assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
    assert!(fs::remove_file(&path).is_err());
    let mut reader = directory
        .open_regular(&leaf("base"), Links::Single)
        .unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"permanent");
    held.recheck().unwrap();
    assert!(held.remove_generated().is_err());
    assert_eq!(fs::read(&path).unwrap(), b"permanent");
    // Ordinary retained snapshots do not prevent an authorized DELETE lease.
    write(&directory, "generated", b"app payload");
    let generated = directory.read_lease(&leaf("generated"), true).unwrap();
    assert!(matches!(
        generated.remove_generated().unwrap(),
        WorkspaceRemoval::Removed
    ));
    assert!(!temporary.path().join("private/generated").exists());
    assert_eq!(fs::read(&path).unwrap(), b"permanent");
    assert_eq!(metadata(&ordinary).unwrap().kind, Kind::Regular);
}
#[test]
fn generated_removal_targets_exact_held_link_and_held_parent_after_rename() {
    let (temporary, directory) = fixture();
    write(&directory, "generated", b"old payload");
    let held = directory.read_lease(&leaf("generated"), true).unwrap();
    // A strong lease pins the child's name, so parent rename may be denied on
    // Windows. Either outcome must retain the original capability and leave a
    // distinct sibling's original untouched; no path fallback is allowed.
    let parent = Directory::open_absolute(temporary.path()).unwrap();
    let sibling = parent
        .create_private(&leaf("sibling"), Traversal::SameFilesystem)
        .unwrap();
    write(&sibling, "generated", b"user original");
    let rename = fs::rename(
        temporary.path().join("private"),
        temporary.path().join("held"),
    );
    held.recheck().unwrap();
    assert!(matches!(
        held.remove_generated().unwrap(),
        WorkspaceRemoval::Removed
    ));
    assert!(directory.entry(&leaf("generated")).unwrap().is_none());
    assert_eq!(
        fs::read(temporary.path().join("sibling/generated")).unwrap(),
        b"user original"
    );
    if rename.is_ok() {
        assert!(!temporary.path().join("held/generated").exists());
    }
}
#[test]
fn read_lease_refuses_hardlinks_directories_and_foreign_or_nonprivate_objects() {
    let (temporary, directory) = fixture();
    write(&directory, "single", b"retain");
    let path = temporary.path().join("private/single");
    fs::hard_link(&path, temporary.path().join("private/alias")).unwrap();
    assert!(directory.read_lease(&leaf("single"), false).is_err());
    assert!(directory.read_lease(&leaf("alias"), true).is_err());
    directory
        .create_private(&leaf("folder"), Traversal::SameFilesystem)
        .unwrap();
    assert!(directory.read_lease(&leaf("folder"), false).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"retain");
}

#[path = "../file_ops/junction_fixture.rs"]
mod junction_fixture;

// Adjust only freshly created synthetic entries. No production permissions or
// owner policy is loosened; TokenUser owner must survive this DACL-only edit.
fn broaden_fixture(path: &Path, is_directory: bool) {
    use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr};
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
            READ_CONTROL, WRITE_DAC,
        },
    };
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if is_directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        )
        .open(path)
        .unwrap();
    let before = metadata(&file).unwrap();
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
    let after = metadata(&file).unwrap();
    assert_eq!(before.identity, after.identity);
    match (&before.security, &after.security) {
        (Security::Windows { owner: left, .. }, Security::Windows { owner: right, .. }) => {
            assert_eq!(left, right)
        }
        _ => panic!("expected full native Windows security"),
    }
    after.require_owned().unwrap();
    assert!(after.require_private().is_err());
}
#[test]
fn read_lease_refuses_real_nonprivate_file_parent_and_junction_without_permission_repair() {
    let (temporary, directory) = fixture();
    write(&directory, "broad", b"keep broad");
    let broad = temporary.path().join("private/broad");
    broaden_fixture(&broad, false);
    let before = metadata(
        &directory
            .open_regular(&leaf("broad"), Links::Single)
            .unwrap(),
    )
    .unwrap();
    assert!(directory.read_lease(&leaf("broad"), false).is_err());
    assert_eq!(
        metadata(
            &directory
                .open_regular(&leaf("broad"), Links::Single)
                .unwrap()
        )
        .unwrap(),
        before
    );
    let outside = Directory::open_absolute(temporary.path())
        .unwrap()
        .create_private(&leaf("outside"), Traversal::SameFilesystem)
        .unwrap();
    write(&outside, "retain", b"outside original");
    junction_fixture::create(
        &temporary.path().join("outside"),
        &temporary.path().join("private/link"),
    );
    assert!(directory.read_lease(&leaf("link"), true).is_err());
    assert_eq!(
        fs::read(temporary.path().join("outside/retain")).unwrap(),
        b"outside original"
    );
    write(&directory, "private-file", b"private original");
    broaden_fixture(&temporary.path().join("private"), true);
    let before = directory.metadata().unwrap();
    assert!(directory.read_lease(&leaf("private-file"), false).is_err());
    assert_eq!(directory.metadata().unwrap(), before);
    assert_eq!(
        fs::read(temporary.path().join("private/private-file")).unwrap(),
        b"private original"
    );
}
