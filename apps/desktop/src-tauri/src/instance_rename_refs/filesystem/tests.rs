use super::*;
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use std::fs;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Dir) {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().canonicalize().unwrap();
    let parent = Directory::open_absolute(&path).unwrap();
    let owned = parent
        .create_private(&Leaf::new("references").unwrap(), Traversal::AllowMounts)
        .unwrap();
    drop(owned);
    let path = path.join("references");
    let dir = Dir::open(&path).unwrap();
    (temporary, path, dir)
}
#[test]
fn production_references_create_replace_remove_reopen_and_reactivate() {
    let (_temporary, path, dir) = fixture();
    let original = dir.snapshot("preset.json", 65536).unwrap().0;
    assert!(!original.file_exists());
    assert!(fs::read_dir(&path).unwrap().next().is_none());
    dir.update_checked("preset.json", &original, Some(b"original"), 65536)
        .unwrap();
    let first = dir.snapshot("preset.json", 65536).unwrap().0;
    dir.update_checked("preset.json", &first, Some(b"new bytes"), 65536)
        .unwrap();
    assert!(dir
        .update_checked("preset.json", &first, Some(b"stale"), 65536)
        .is_err());
    let current = dir.snapshot("preset.json", 65536).unwrap().0;
    dir.update_checked("preset.json", &current, None, 65536)
        .unwrap();
    let reopened = Dir::open(&path).unwrap();
    let (absent, bytes) = reopened.snapshot("preset.json", 65536).unwrap();
    assert_eq!(bytes, None);
    #[cfg(windows)]
    {
        assert_eq!(absent.logical.as_ref().unwrap().presence, "tombstone");
        assert!(!original.compatible(&absent));
        assert!(!DiskSnapshot::default().compatible(&absent));
        assert_eq!(fs::read(path.join("preset.json")).unwrap(), b"original");
    }
    reopened
        .update_checked("preset.json", &absent, Some(b"reactivated"), 65536)
        .unwrap();
    assert_eq!(
        reopened.snapshot("preset.json", 65536).unwrap().1.unwrap(),
        b"reactivated"
    );
    #[cfg(windows)]
    assert!(reopened
        .update_checked("preset.json", &absent, None, 65536)
        .is_err());
}
#[test]
fn generated_marker_identity_content_and_unknown_siblings_are_preserved() {
    let (_temporary, path, dir) = fixture();
    dir.publish_marker(|identity| serde_json::to_vec(&identity).map_err(|e| e.to_string()))
        .unwrap();
    let (token, bytes) = dir.read(MARKER, PRESET_LIMIT).unwrap().unwrap();
    let identity: Identity = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(identity, token.stamp.identity);
    dir.write_backup("unknown-keep.json", b"keep", PRESET_LIMIT)
        .unwrap();
    fs::rename(path.join(MARKER), path.join("preserved-marker.json")).unwrap();
    dir.write_backup(MARKER, &bytes, PRESET_LIMIT).unwrap();
    assert!(dir.clear_marker(&token, &bytes).is_err());
    assert_eq!(fs::read(path.join(MARKER)).unwrap(), bytes);
    let (replacement, actual) = dir.read(MARKER, PRESET_LIMIT).unwrap().unwrap();
    dir.clear_marker(&replacement, &actual).unwrap();
    assert!(!path.join(MARKER).exists());
    assert_eq!(fs::read(path.join("unknown-keep.json")).unwrap(), b"keep");
    assert_eq!(fs::read(path.join("preserved-marker.json")).unwrap(), bytes);
}
#[test]
fn marker_edits_and_hardlinked_documents_are_refused() {
    let (_temporary, path, dir) = fixture();
    dir.publish_marker(|_| Ok(b"original marker".to_vec()))
        .unwrap();
    let (token, bytes) = dir.read(MARKER, PRESET_LIMIT).unwrap().unwrap();
    fs::write(path.join(MARKER), b"external edit").unwrap();
    assert!(dir.clear_marker(&token, &bytes).is_err());
    assert_eq!(fs::read(path.join(MARKER)).unwrap(), b"external edit");
    dir.write_backup("document.json", b"original", PRESET_LIMIT)
        .unwrap();
    fs::hard_link(path.join("document.json"), path.join("alias.json")).unwrap();
    assert!(dir.snapshot("document.json", PRESET_LIMIT).is_err());
    assert_eq!(fs::read(path.join("alias.json")).unwrap(), b"original");
}
#[test]
fn reference_locks_are_shared_by_reopened_capabilities() {
    let (_temporary, path, dir) = fixture();
    let first = dir.lock(".settings.lock").unwrap();
    let reopened = Dir::open(&path).unwrap();
    assert!(reopened.lock(".settings.lock").is_err());
    drop(first);
    reopened.lock(".settings.lock").unwrap();
}
#[cfg(unix)]
#[test]
fn original_unix_snapshot_wire_is_byte_identical() {
    use std::os::unix::fs::MetadataExt;
    let (_temporary, path, dir) = fixture();
    dir.write_backup("document.json", b"original", PRESET_LIMIT)
        .unwrap();
    let m = fs::metadata(path.join("document.json")).unwrap();
    let snapshot = dir.snapshot("document.json", PRESET_LIMIT).unwrap().0;
    let d = fs::metadata(&path).unwrap();
    let expected = format!(
        "{{\"directory\":{{\"device\":{},\"inode\":{}}},\"file\":{{\"stamp\":{{\"identity\":{{\"device\":{},\"inode\":{}}},\"bytes\":{},\"mode\":{},\"links\":{},\"modified\":[{},{}],\"changed\":[{},{}]}},\"hash\":\"{}\"}}}}",
        d.dev(), d.ino(), m.dev(), m.ino(), m.len(), m.mode(), m.nlink(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec(), hash(b"original"));
    let encoded = serde_json::to_string(&snapshot).unwrap();
    assert_eq!(encoded, expected);
    assert_eq!(
        serde_json::to_string(&serde_json::from_str::<DiskSnapshot>(&encoded).unwrap()).unwrap(),
        expected
    );
}
