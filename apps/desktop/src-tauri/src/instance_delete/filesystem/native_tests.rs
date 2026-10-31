use super::*;
use std::fs;
struct Fixture {
    _temp: tempfile::TempDir,
    path: PathBuf,
    root: Dir,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(temp.path()).unwrap();
        let root = Dir::open(&path).unwrap();
        Self {
            _temp: temp,
            path,
            root,
        }
    }
    fn write(dir: &Dir, name: &str, bytes: &[u8]) {
        native(dir.0.create_file(&leaf(name).unwrap()))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
}
#[test]
fn native_scan_move_and_restore_preserve_full_tree_and_all_original_bytes() {
    let f = Fixture::new();
    let versions = f.root.create("versions").unwrap();
    let store = f.root.create("recovery").unwrap();
    let instance = versions.create("example").unwrap();
    let saves = instance.create("saves").unwrap();
    Fixture::write(&instance, "example.json", b"synthetic version");
    Fixture::write(&saves, "world.dat", b"irreplaceable synthetic world");
    let before = scan(&instance, None).unwrap();
    assert_eq!(before.files, 2);
    rename_new(&versions, "example", &store, "instance").unwrap();
    let moved = store.child("instance").unwrap();
    assert!(before.moved_matches(&scan(&moved, None).unwrap()));
    assert!(!f.path.join("versions/example").exists());
    rename_new(&store, "instance", &versions, "example").unwrap();
    assert!(before.moved_matches(&scan(&versions.child("example").unwrap(), None).unwrap()));
    assert_eq!(
        fs::read(f.path.join("versions/example/saves/world.dat")).unwrap(),
        b"irreplaceable synthetic world"
    );
}
#[test]
fn native_move_never_replaces_an_occupied_recovery_destination() {
    let f = Fixture::new();
    let versions = f.root.create("versions").unwrap();
    let store = f.root.create("recovery").unwrap();
    let instance = versions.create("example").unwrap();
    let occupied = store.create("instance").unwrap();
    Fixture::write(&instance, "original", b"original instance");
    Fixture::write(&occupied, "original", b"existing recovery");
    assert!(rename_new(&versions, "example", &store, "instance").is_err());
    assert_eq!(
        fs::read(f.path.join("versions/example/original")).unwrap(),
        b"original instance"
    );
    assert_eq!(
        fs::read(f.path.join("recovery/instance/original")).unwrap(),
        b"existing recovery"
    );
}
#[test]
fn native_scan_rejects_hardlinks_and_cancellation_without_mutation() {
    let f = Fixture::new();
    let instance = f.root.create("instance").unwrap();
    Fixture::write(&instance, "original", b"original");
    fs::hard_link(
        f.path.join("instance/original"),
        f.path.join("instance/alias"),
    )
    .unwrap();
    assert!(scan(&instance, None).is_err());
    fs::remove_file(f.path.join("instance/alias")).unwrap();
    assert!(scan(&instance, Some(&AtomicBool::new(true))).is_err());
    assert_eq!(
        fs::read(f.path.join("instance/original")).unwrap(),
        b"original"
    );
}
#[test]
fn native_record_stage_publishes_without_replacement_and_rejects_changed_cleanup() {
    let f = Fixture::new();
    let operation = f.root.create("operation").unwrap();
    let mut stage = operation.anonymous().unwrap();
    let key = anonymous_key(&stage).unwrap();
    stage.write_all(b"journal bytes").unwrap();
    stage.sync_all().unwrap();
    operation.link(&stage, "journal.json").unwrap();
    let (token, bytes) = read_bytes(&operation, "journal.json", MAX_JOURNAL).unwrap();
    assert_eq!(token.stamp.key, key);
    assert_eq!(bytes, b"journal bytes");
    assert_eq!(operation.names().unwrap(), ["journal.json"]);
    fs::write(f.path.join("operation/journal.json"), b"changed externally").unwrap();
    assert!(operation.unlink_file("journal.json", &token).is_err());
    assert_eq!(
        fs::read(f.path.join("operation/journal.json")).unwrap(),
        b"changed externally"
    );
    // Arbitrary game files cannot be enrolled in generated-record cleanup.
    Fixture::write(&operation, "user-world.dat", b"user original");
    let token = read_bytes(&operation, "user-world.dat", MAX_JOURNAL)
        .unwrap()
        .0;
    assert!(operation.unlink_file("user-world.dat", &token).is_err());
    assert_eq!(
        fs::read(f.path.join("operation/user-world.dat")).unwrap(),
        b"user original"
    );
}
#[cfg(unix)]
#[test]
fn native_unix_exchange_retains_both_records_and_exact_cleanup_token() {
    let f = Fixture::new();
    let operation = f.root.create("operation").unwrap();
    Fixture::write(&operation, "journal.json", b"old generation");
    let original = read_bytes(&operation, "journal.json", MAX_JOURNAL)
        .unwrap()
        .0;
    let mut stage = operation.anonymous().unwrap();
    stage.write_all(b"next generation").unwrap();
    operation.link(&stage, "journal-fixture.next").unwrap();
    operation
        .exchange("journal-fixture.next", "journal.json", "fixture exchange")
        .unwrap();
    let displaced = read_bytes(&operation, "journal-fixture.next", MAX_JOURNAL)
        .unwrap()
        .0;
    assert!(original.moved_matches(&displaced));
    assert_eq!(
        read_bytes(&operation, "journal.json", MAX_JOURNAL)
            .unwrap()
            .1,
        b"next generation"
    );
    operation
        .unlink_file("journal-fixture.next", &displaced)
        .unwrap();
    assert_eq!(operation.names().unwrap(), ["journal.json"]);
}
#[test]
fn native_operation_lock_unlocks_even_when_a_duplicate_handle_survives() {
    let f = Fixture::new();
    let store = f.root.create("recovery").unwrap();
    let owner = lock(&store).unwrap();
    let duplicate = owner.duplicate_descriptor().unwrap();
    assert!(lock(&store).is_err());
    drop(owner);
    let replacement = lock(&store).unwrap();
    drop(duplicate);
    drop(replacement);
}
#[test]
fn native_empty_setup_cleanup_preserves_replacements_and_unknown_entries() {
    let f = Fixture::new();
    let setup = f.root.create("setup").unwrap();
    let key = setup.key().unwrap();
    Fixture::write(&setup, "unknown", b"unknown evidence");
    assert!(f.root.remove_empty("setup", &key).is_err());
    assert_eq!(
        fs::read(f.path.join("setup/unknown")).unwrap(),
        b"unknown evidence"
    );
    fs::remove_file(f.path.join("setup/unknown")).unwrap();
    f.root.remove_empty("setup", &key).unwrap();
    assert!(!f.path.join("setup").exists());
}
#[cfg(unix)]
#[test]
fn native_bound_child_refuses_symlinks_and_uses_original_held_parent() {
    let f = Fixture::new();
    let original = f.root.create("original").unwrap();
    Fixture::write(&original, "kept", b"held original");
    fs::rename(f.path.join("original"), f.path.join("held")).unwrap();
    std::os::unix::fs::symlink(f.path.join("held"), f.path.join("original")).unwrap();
    assert!(f.root.child("original").is_err());
    assert_eq!(
        read_bytes(&original, "kept", MAX_JOURNAL).unwrap().1,
        b"held original"
    );
}
#[cfg(windows)]
#[test]
fn windows_transaction_preflight_fails_before_any_filesystem_side_effect() {
    let f = Fixture::new();
    assert!(require_mutation_support().unwrap_err().contains("原实例"));
    assert!(f.root.names().unwrap().is_empty());
}
