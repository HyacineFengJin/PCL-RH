//! Exercise the production desktop asset filesystem without linking Tauri or
//! fontconfig. Catalog command/UI/font discovery are separate integration work.
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_assets/filesystem.rs"]
mod filesystem;

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

#[cfg(windows)]
#[path = "../src/file_ops/junction_fixture.rs"]
mod junction_fixture;
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
    fs::create_dir(&path).unwrap();
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
    assert_eq!(directory.0.metadata().unwrap().security, before.security);
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
    fs::write(fixture.path().join("source"), b"source bytes").unwrap();
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
