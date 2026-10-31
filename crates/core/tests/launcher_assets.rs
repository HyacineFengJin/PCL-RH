//! Exercise the production desktop asset filesystem without linking Tauri or
//! fontconfig. Catalog command/UI/font discovery are separate integration work.
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_assets/filesystem.rs"]
mod filesystem;
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_assets/response.rs"]
mod response;

use filesystem::{Dir, Scope, Snapshot};
use pcl_core::retained_fs::{Identity, Leaf};
use std::{
    fs,
    io::Read,
    path::Path,
    sync::{Arc, Barrier},
};

fn fixture() -> tempfile::TempDir {
    let work =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/launcher-assets-native-tests");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap()
}

#[derive(Default)]
struct BodyIo {
    reads: usize,
    bytes: usize,
    seeks: usize,
}
struct CountedFile<'a> {
    file: &'a mut fs::File,
    io: BodyIo,
}
impl std::io::Read for CountedFile<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.io.reads += 1;
        let count = self.file.read(bytes)?;
        self.io.bytes += count;
        Ok(count)
    }
}
impl std::io::Seek for CountedFile<'_> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.io.seeks += 1;
        std::io::Seek::seek(self.file, position)
    }
}

fn media_binding<'a>(
    name: &'a str,
    stamp: &'a filesystem::Stamp,
    root: &'a Identity,
    owner: &'a Identity,
    folder: &'a Identity,
) -> response::Binding<'a> {
    response::Binding {
        name,
        stamp,
        root,
        owner,
        folder,
    }
}

#[test]
fn head_of_a_real_64mib_sparse_media_has_zero_body_io_and_preserves_range_lengths() {
    use std::io::{Seek, Write};
    let fixture = fixture();
    let scope = Scope::create(fixture.path(), "backgrounds").unwrap();
    let folder = scope.folder.as_ref().unwrap();
    folder.write_new("large.mp4", b"").unwrap();
    let path = scope.path().join("large.mp4");
    let mut output = fs::OpenOptions::new().write(true).open(&path).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // FSCTL_SET_SPARSE from the locked Windows SDK binding (590020).
        // Mark only this new synthetic file; no user path/system policy changes.
        let mut returned = 0;
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::IO::DeviceIoControl(
                    output.as_raw_handle(),
                    590020,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            },
            0
        );
    }
    output.set_len(response::MAX_MEDIA_BYTES).unwrap();
    output
        .write_all(b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isommp42")
        .unwrap();
    drop(output);
    // The full content hash is the existing catalog admission, outside the
    // HEAD-body measurement. It uses the production fixed streaming buffer.
    let snapshot = Snapshot::open(folder, "large.mp4", response::MAX_MEDIA_BYTES).unwrap();
    let root_id = scope.root.identity().unwrap();
    let owner_id = scope.owner.as_ref().unwrap().identity().unwrap();
    let folder_id = folder.identity().unwrap();
    for (range, expected_start, expected_length, partial) in [
        (None, 0, response::MAX_MEDIA_BYTES, false),
        (Some("bytes=7-11"), 7, 5, true),
        (Some("bytes=0-"), 0, response::MAX_RANGE_BYTES, true),
    ] {
        let mut authorized = response::authorize(
            &scope,
            media_binding(
                "large.mp4",
                &snapshot.stamp,
                &root_id,
                &owner_id,
                &folder_id,
            ),
            range,
        )
        .unwrap();
        assert_eq!(authorized.file.stream_position().unwrap(), 0);
        let span = authorized.span;
        let mut counted = CountedFile {
            file: &mut authorized.file,
            io: BodyIo::default(),
        };
        let bytes = response::read_body(&mut counted, span, response::Method::Head).unwrap();
        assert_eq!(
            (counted.io.reads, counted.io.bytes, counted.io.seeks),
            (0, 0, 0)
        );
        assert_eq!(bytes.capacity(), 0);
        assert_eq!(counted.file.stream_position().unwrap(), 0);
        drop(counted);
        let head = authorized.respond(response::Method::Head).unwrap();
        assert!(head.bytes.is_empty());
        assert_eq!(head.bytes.capacity(), 0);
        assert_eq!(authorized.file.stream_position().unwrap(), 0);
        assert_eq!(head.span.start, expected_start);
        assert_eq!(head.span.content_length(), expected_length);
        assert_eq!(head.span.total, response::MAX_MEDIA_BYTES);
        assert_eq!(head.span.partial, partial);
    }
    // A valid GET subrange still reads the exact admitted source bytes.
    let mut authorized = response::authorize(
        &scope,
        media_binding(
            "large.mp4",
            &snapshot.stamp,
            &root_id,
            &owner_id,
            &folder_id,
        ),
        Some("bytes=4-7"),
    )
    .unwrap();
    let get = authorized.respond(response::Method::Get).unwrap();
    assert_eq!(get.bytes, b"ftyp");
    assert_eq!(get.span.content_length(), 4);
}

#[test]
fn media_head_keeps_missing_changed_range_and_directory_identity_rejections() {
    let fixture = fixture();
    let scope = Scope::create(fixture.path(), "titles").unwrap();
    let folder = scope.folder.as_ref().unwrap();
    folder.write_new("title", b"catalog bytes").unwrap();
    let snapshot = Snapshot::open(folder, "title", 1024).unwrap();
    let root_id = scope.root.identity().unwrap();
    let owner_id = scope.owner.as_ref().unwrap().identity().unwrap();
    let folder_id = folder.identity().unwrap();
    let unauthorized = Scope::create(fixture.path(), "music").unwrap();
    assert!(response::authorize(
        &unauthorized,
        media_binding("title", &snapshot.stamp, &root_id, &owner_id, &folder_id),
        None
    )
    .is_err());
    assert!(response::authorize(
        &scope,
        media_binding("missing", &snapshot.stamp, &root_id, &owner_id, &folder_id),
        None
    )
    .is_err());
    for range in ["bytes=99-", "bytes=3-1", "bytes=0-2,4-6", "items=0-3"] {
        assert!(response::authorize(
            &scope,
            media_binding("title", &snapshot.stamp, &root_id, &owner_id, &folder_id),
            Some(range)
        )
        .is_err());
    }
    let mut get = response::authorize(
        &scope,
        media_binding("title", &snapshot.stamp, &root_id, &owner_id, &folder_id),
        None,
    )
    .unwrap();
    assert_eq!(
        get.respond(response::Method::Get).unwrap().bytes,
        b"catalog bytes"
    );
    fs::OpenOptions::new()
        .write(true)
        .open(scope.path().join("title"))
        .unwrap()
        .set_len(2)
        .unwrap();
    assert!(response::authorize(
        &scope,
        media_binding("title", &snapshot.stamp, &root_id, &owner_id, &folder_id),
        None
    )
    .is_err());
    let changed = Snapshot::open(folder, "title", 1024).unwrap();
    let mut head = response::authorize(
        &scope,
        media_binding("title", &changed.stamp, &root_id, &owner_id, &folder_id),
        None,
    )
    .unwrap();
    fs::rename(scope.path(), fixture.path().join("retained-titles")).unwrap();
    fs::create_dir(scope.path()).unwrap();
    assert!(head.respond(response::Method::Head).is_err());
    let rebound = Scope::open(fixture.path(), "titles").unwrap();
    assert!(response::authorize(
        &rebound,
        media_binding("title", &changed.stamp, &root_id, &owner_id, &folder_id),
        None
    )
    .is_err());
}

#[cfg(windows)]
#[path = "../src/file_ops/junction_fixture.rs"]
mod junction_fixture;
/// Widen only this newly created synthetic directory's DACL. Its native
/// TokenUser owner is retained, including under an elevated GHA token whose
/// ordinary std::fs creations may instead default to BuiltinAdministrators.
#[cfg(windows)]
fn broad_fixture_dacl(path: &Path, directory: &pcl_core::retained_fs::Directory) {
    use std::{
        ffi::c_void,
        fs::File,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle},
        },
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{LocalFree, INVALID_HANDLE_VALUE},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
        },
    };
    struct Local(*mut c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
    }
    let admitted = directory.metadata().unwrap();
    admitted.require_private().unwrap();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            WRITE_DAC | READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    assert_ne!(
        raw,
        INVALID_HANDLE_VALUE,
        "open synthetic WRITE_DAC handle: {}",
        std::io::Error::last_os_error()
    );
    let file = unsafe { File::from_raw_handle(raw) };
    let opened = pcl_core::retained_fs::metadata(&file).unwrap();
    // Complete native volume/FileId128, never a truncated inode or pathname
    // comparison, binds the privileged test mutation to this exact fixture.
    assert_eq!(opened.identity, admitted.identity);
    opened.require_owned().unwrap();
    let mut owner = ptr::null_mut();
    let mut original = Local(ptr::null_mut());
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut original.0,
        )
    };
    assert_eq!(status, 0, "query synthetic directory owner");
    assert!(!owner.is_null() && !original.0.is_null());
    let mut sid_pointer = ptr::null_mut();
    let converted = unsafe { ConvertSidToStringSidW(owner, &mut sid_pointer) };
    let sid_allocation = Local(sid_pointer.cast());
    assert_ne!(
        converted,
        0,
        "convert actual fixture owner SID: {}",
        std::io::Error::last_os_error()
    );
    assert!(!sid_allocation.0.is_null());
    let mut sid_units = Vec::new();
    // Native SIDs are bounded to 15 subauthorities; the converted, terminated
    // S-R-I-S... string fits well within this explicit upper boundary.
    for index in 0..256 {
        let unit = unsafe { *sid_pointer.add(index) };
        if unit == 0 {
            break;
        }
        sid_units.push(unit);
    }
    assert!(sid_units.len() < 256);
    let sid = String::from_utf16(&sid_units).unwrap();
    let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FR;;;WD)")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = Local(ptr::null_mut());
    let mut length = 0u32;
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor.0,
                &mut length,
            )
        },
        0,
        "build actual broad fixture DACL: {}",
        std::io::Error::last_os_error()
    );
    assert!(length > 0);
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = ptr::null_mut();
    assert_ne!(
        unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) },
        0
    );
    assert_ne!(present, 0);
    assert!(
        !dacl.is_null(),
        "never substitute an unrestricted NULL DACL"
    );
    // OWNER_SECURITY_INFORMATION is deliberately absent. This adjusts only the
    // new empty fixture's DACL, not its owner or any production admission rule.
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
    let changed = directory.metadata().unwrap();
    assert_eq!(changed.identity, admitted.identity);
    changed.require_owned().unwrap();
    assert!(changed.require_private().is_err());
    match (&admitted.security, &changed.security) {
        (
            pcl_core::retained_fs::Security::Windows { owner: before, .. },
            pcl_core::retained_fs::Security::Windows { owner: after, .. },
        ) => assert_eq!(before, after),
        _ => panic!("Windows fixture must retain native security evidence"),
    }
    assert_ne!(changed.security, admitted.security);
}

fn directory_link(target: &Path, path: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path).unwrap();
    #[cfg(windows)]
    junction_fixture::create(target, path);
}

#[test]
fn read_only_scope_does_not_create_and_explicit_creation_publishes_real_bytes() {
    let fixture = fixture();
    assert!(Scope::open(fixture.path(), "titles")
        .unwrap()
        .owner
        .is_none());
    assert!(!fixture.path().join(".pcl-rust").exists());
    let scope = Scope::create(fixture.path(), "titles").unwrap();
    let folder = scope.folder.as_ref().unwrap();
    folder.write_new("标题.bin", b"native asset bytes").unwrap();
    let mut snapshot = Snapshot::open(folder, "标题.bin", 1024).unwrap();
    assert_eq!(
        snapshot.bytes(folder, "标题.bin").unwrap(),
        b"native asset bytes"
    );
    assert_eq!(folder.names().unwrap(), ["标题.bin"]);
    scope.recheck().unwrap();
    match folder.identity().unwrap() {
        #[cfg(unix)]
        Identity::Unix { device, inode } => assert!(device != 0 && inode != 0),
        #[cfg(windows)]
        Identity::Windows { file, .. } => assert_ne!(file, [0; 16]),
        _ => panic!("A native identity must retain its platform"),
    }
    #[cfg(windows)]
    {
        let stamp = serde_json::to_value(&snapshot.stamp).unwrap();
        assert!(stamp.get("device").is_none() && stamp.get("inode").is_none());
        assert_eq!(stamp["identity"]["platform"], "windows");
        assert_eq!(stamp["identity"]["file"].as_array().unwrap().len(), 16);
        assert!(stamp["security"]["descriptor"].as_array().unwrap().len() > 0);
    }
}

#[test]
fn collision_never_changes_original_bytes_or_open_original_identity() {
    let fixture = fixture();
    let directory = Dir::absolute(fixture.path()).unwrap();
    directory.write_new("existing", b"original bytes").unwrap();
    let mut original = directory.file("existing").unwrap().unwrap();
    let stamp = pcl_core::retained_fs::metadata(&original).unwrap();
    let error = directory
        .write_new("existing", b"replacement bytes")
        .unwrap_err();
    assert!(error.contains("目标文件必须不存在"));
    assert_eq!(
        fs::read(fixture.path().join("existing")).unwrap(),
        b"original bytes"
    );
    let mut bytes = Vec::new();
    original.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original bytes");
    assert_eq!(pcl_core::retained_fs::metadata(&original).unwrap(), stamp);
    #[cfg(any(windows, target_os = "macos"))]
    {
        use pcl_core::retained_fs::{Links, Traversal};
        assert!(error.contains(".pcl-stage-"));
        let residue = directory
            .names()
            .unwrap()
            .into_iter()
            .find(|name| name != "existing")
            .unwrap();
        let (parent, lease) = if residue.ends_with(".lease") {
            (directory.0.clone(), residue)
        } else {
            assert!(error.contains(&residue));
            let workspace = directory
                .0
                .child(&Leaf::new(residue).unwrap(), Traversal::SameFilesystem)
                .unwrap();
            workspace.metadata().unwrap().require_private().unwrap();
            let names = workspace.names(2).unwrap();
            assert_eq!(names.len(), 1);
            (workspace, names[0].to_str().unwrap().to_owned())
        };
        let mut retained = parent
            .open_regular(&Leaf::new(lease).unwrap(), Links::Single)
            .unwrap();
        let mut bytes = Vec::new();
        retained.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"replacement bytes");
    }
}

#[test]
fn legacy_owned_catalog_permissions_are_preserved_during_new_file_publication() {
    let fixture = fixture();
    let path = fixture.path().join("legacy-catalog");
    #[cfg(unix)]
    fs::create_dir(&path).unwrap();
    #[cfg(windows)]
    {
        use pcl_core::retained_fs::{Directory, Traversal};
        let root = Directory::open_absolute(fixture.path()).unwrap();
        let directory = root
            .create_private(
                &Leaf::new("legacy-catalog").unwrap(),
                Traversal::SameFilesystem,
            )
            .unwrap();
        broad_fixture_dacl(&path, &directory);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Only this synthetic fixture's permissions are set; production never
        // repairs/chmods a user's legacy catalog or weakens Stage admission.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let directory = Dir::absolute(&path).unwrap();
    let before = directory.0.metadata().unwrap();
    before.require_owned().unwrap();
    directory
        .write_new("asset", b"legacy catalog import")
        .unwrap();
    let after = directory.0.metadata().unwrap();
    assert_eq!(after.identity, before.identity);
    assert_eq!(after.security, before.security);
    #[cfg(windows)]
    assert!(
        before.require_private().is_err(),
        "the actual broad DACL must remain broad"
    );
    assert_eq!(directory.names().unwrap(), ["asset"]);
    assert_eq!(
        fs::read(path.join("asset")).unwrap(),
        b"legacy catalog import"
    );
    let mut snapshot = Snapshot::open(&directory, "asset", 1024).unwrap();
    assert_eq!(
        snapshot.bytes(&directory, "asset").unwrap(),
        b"legacy catalog import"
    );
}

#[test]
fn retained_parent_rename_ignores_recreated_paths_and_junctions() {
    for link in [false, true] {
        let fixture = fixture();
        let original = fixture.path().join("original");
        let moved = fixture.path().join("moved");
        let outside = fixture.path().join("outside");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"outside bytes").unwrap();
        let directory = Dir::absolute(&original).unwrap();
        fs::rename(&original, &moved).unwrap();
        if link {
            directory_link(&outside, &original);
            assert!(Dir::absolute(&original).is_err());
        } else {
            fs::create_dir(&original).unwrap();
        }
        directory
            .write_new("new-asset", b"held parent bytes")
            .unwrap();
        let mut snapshot = Snapshot::open(&directory, "new-asset", 1024).unwrap();
        assert_eq!(
            snapshot.bytes(&directory, "new-asset").unwrap(),
            b"held parent bytes"
        );
        assert_eq!(
            fs::read(moved.join("new-asset")).unwrap(),
            b"held parent bytes"
        );
        assert!(!original.join("new-asset").exists());
        assert_eq!(
            fs::read(outside.join("sentinel")).unwrap(),
            b"outside bytes"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    }
}

#[test]
fn snapshots_reject_changed_sources_hard_links_and_directory_links() {
    let fixture = fixture();
    let directory = Dir::absolute(fixture.path()).unwrap();
    directory.write_new("source", b"source bytes").unwrap();
    let snapshot = Snapshot::open(&directory, "source", 1024).unwrap();
    fs::write(fixture.path().join("source"), b"new source bytes").unwrap();
    assert!(snapshot.recheck(&directory, "source").is_err());
    fs::hard_link(fixture.path().join("source"), fixture.path().join("alias")).unwrap();
    assert!(directory.file("source").is_err());
    assert!(directory.file("alias").is_err());
    let outside = fixture.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"outside").unwrap();
    directory_link(&outside, &fixture.path().join("linked"));
    assert!(directory.optional("linked").is_err());
    assert!(directory.create("linked").is_err());
    assert!(directory.file("linked").is_err());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("sentinel"), fixture.path().join("file-link"))
            .unwrap();
        assert!(directory.file("file-link").is_err());
    }
}

#[test]
fn scope_recheck_detects_a_replaced_catalog_directory() {
    let fixture = fixture();
    let scope = Scope::create(fixture.path(), "titles").unwrap();
    let old_path = fixture.path().join(".pcl-rust/titles");
    fs::rename(&old_path, fixture.path().join("retained-titles")).unwrap();
    fs::create_dir(&old_path).unwrap();
    assert!(scope.recheck().is_err());
    scope
        .folder
        .as_ref()
        .unwrap()
        .write_new("held", b"retained")
        .unwrap();
    assert_eq!(
        fs::read(fixture.path().join("retained-titles/held")).unwrap(),
        b"retained"
    );
    assert!(!old_path.join("held").exists());
}

#[test]
fn concurrent_no_replace_has_one_winner_and_keeps_its_complete_payload() {
    let fixture = fixture();
    let directory = Arc::new(Dir::absolute(fixture.path()).unwrap());
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = Vec::new();
    for byte in [b'a', b'b'] {
        let directory = directory.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            directory.write_new("winner", &[byte; 8192]).is_ok()
        }));
    }
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    let bytes = fs::read(fixture.path().join("winner")).unwrap();
    assert_eq!(bytes.len(), 8192);
    assert!(bytes == vec![b'a'; 8192] || bytes == vec![b'b'; 8192]);
    let actual = directory
        .0
        .entry(&Leaf::new("winner").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(actual.links, 1);
}

#[cfg(unix)]
#[test]
fn unix_stamp_and_revision_preserve_the_legacy_serialized_bytes() {
    use serde::Serialize;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    #[derive(Serialize)]
    struct LegacyStamp {
        device: u64,
        inode: u64,
        mode: u32,
        owner: u32,
        links: u64,
        bytes: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }
    let fixture = fixture();
    let path = fixture.path().join("wire-stamp");
    fs::write(&path, b"legacy bytes").unwrap();
    let (parent, name, snapshot) = filesystem::source(&path, 1024).unwrap();
    snapshot.recheck(&parent, &name).unwrap();
    let m = snapshot.file.metadata().unwrap();
    let old = LegacyStamp {
        device: m.dev(),
        inode: m.ino(),
        mode: m.mode(),
        owner: m.uid(),
        links: m.nlink(),
        bytes: m.len(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
    };
    assert_eq!(
        serde_json::to_vec(&snapshot.stamp).unwrap(),
        serde_json::to_vec(&old).unwrap()
    );
    assert_eq!(
        filesystem::revision(&path, &snapshot).unwrap(),
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&path, &old, &snapshot.digest)).unwrap())
        )
    );
}
