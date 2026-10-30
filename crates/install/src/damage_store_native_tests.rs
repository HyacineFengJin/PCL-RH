//! The staging registry records new payloads; it must not become authority for
//! legacy business recovery. Exercise the actual journal on every native host.
use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let work =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/native-business-recovery-tests");
    fs::create_dir_all(&work).unwrap();
    let temporary = tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap();
    let root = fs::canonicalize(temporary.path()).unwrap();
    (temporary, root)
}

fn open_with_area(root: &Path, create: bool) -> Store {
    let directory = Directory::open_absolute(root).unwrap();
    let area = staging::Area::open(&directory, true).unwrap();
    // Linux normally uses anonymous per-parent stages. Explicitly admit the
    // same separate Area used by the native named-stage production backend.
    Store::open_prepared(
        directory.try_clone_file().unwrap(),
        root,
        create,
        true,
        Some(area),
    )
    .unwrap()
    .unwrap()
}

fn staged(root: &Path) -> (Store, Operation, Intent) {
    fs::create_dir(root.join("libraries")).unwrap();
    fs::write(root.join("libraries/native.jar"), b"original user bytes").unwrap();
    let mut storage = open_with_area(root, true);
    let operation = storage.begin("d-a-b-c", "fixture").unwrap();
    let parent = storage
        .filesystem
        .parent("libraries/native.jar", false)
        .unwrap()
        .unwrap();
    let original = fingerprint(&mut regular(&parent, "native.jar").unwrap(), &|| Ok(())).unwrap();
    for (name, bytes) in [
        ("backup-0", b"original user bytes".as_slice()),
        ("incoming-0", b"verified replacement".as_slice()),
    ] {
        let mut stage = RepairStage::new(
            operation.directory(),
            name,
            bytes.len() as u64,
            operation.staging_area(),
        )
        .unwrap();
        stage.file_mut().write_all(bytes).unwrap();
        assert!(stage.publish().unwrap());
    }
    let backup = fingerprint(
        &mut regular(&operation.directory, "backup-0").unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let incoming = fingerprint(
        &mut regular(&operation.directory, "incoming-0").unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let intent = operation
        .prepare(
            0,
            "libraries/native.jar",
            &storage.filesystem,
            original,
            incoming,
            backup,
        )
        .unwrap();
    (storage, operation, intent)
}

fn corrupt_registry(root: &Path) -> Vec<u8> {
    let path = root.join(".pcl-linux/dependency-staging/registry.log");
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 1;
    fs::write(&path, &bytes).unwrap();
    bytes
}

#[test]
fn corrupt_staging_registry_cannot_block_verified_business_rollback() {
    for stop in [1, 2] {
        let (_temporary, root) = fixture();
        let (mut storage, operation, intent) = staged(&root);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert!(publish(&mut storage, &operation, &intent, &|| {
            if calls.fetch_add(1, Ordering::Relaxed) + 1 == stop {
                Err("fixture power interruption".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        drop(operation);
        drop(storage);
        let corrupt = corrupt_registry(&root);
        let mut storage = open_with_area(&root, false);
        let area = storage.staging.as_ref().unwrap();
        assert!(!area.report().unwrap().writable());
        assert!(area
            .allocate(
                area.directory(),
                &Leaf::new("forbidden-new-payload").unwrap(),
                1024,
            )
            .is_err());
        let mut operations = storage.operations().unwrap();
        assert_eq!(operations.len(), 1);
        let operation = operations.pop().unwrap();
        recover(&mut storage, &operation, &|| Ok(())).unwrap();
        assert!(operation.done().unwrap());
        assert_eq!(
            fs::read(root.join("libraries/native.jar")).unwrap(),
            b"original user bytes"
        );
        assert_eq!(
            read(&operation.directory, "backup-0", 1024).unwrap(),
            b"original user bytes"
        );
        assert_eq!(
            read(&operation.directory, "incoming-0", 1024).unwrap(),
            b"verified replacement"
        );
        let intent_bytes = read(&operation.directory, "intent-0.json", MAX_RECORD_BYTES).unwrap();
        assert_eq!(
            read(&operation.directory, "rollback-0", 128).unwrap(),
            sha256(&intent_bytes).as_bytes()
        );
        let mut visible = names(&operation.directory, 16).unwrap();
        visible.sort();
        assert_eq!(
            visible,
            [
                "backup-0",
                "done",
                "header.json",
                "incoming-0",
                "intent-0.json",
                "rollback-0"
            ]
        );
        assert!(storage.operations().unwrap().is_empty());
        drop(operation);
        drop(storage);
        // Windows' registry byte lock also excludes an unrelated read handle.
        // Inspect the retained log after its owning lock has been released.
        assert_eq!(
            fs::read(root.join(".pcl-linux/dependency-staging/registry.log")).unwrap(),
            corrupt
        );
    }
}

#[test]
fn corrupt_registry_does_not_authorize_tampered_business_intent() {
    let (_temporary, root) = fixture();
    let (mut storage, operation, intent) = staged(&root);
    assert!(publish(&mut storage, &operation, &intent, &|| {
        Err("fixture stop before replacement".into())
    })
    .is_err());
    drop(operation);
    drop(storage);
    corrupt_registry(&root);
    let operation_path = root.join(STORE).join("d-a-b-c");
    fs::write(operation_path.join("intent-0.json"), b"not an intent").unwrap();
    let mut storage = open_with_area(&root, false);
    let operation = storage.operations().unwrap().pop().unwrap();
    assert!(recover(&mut storage, &operation, &|| Ok(())).is_err());
    assert!(!operation.done().unwrap());
    for name in ["backup-0", "original-0"] {
        assert_eq!(
            read(&operation.directory, name, 1024).unwrap(),
            b"original user bytes"
        );
    }
    assert_eq!(
        read(&operation.directory, "incoming-0", 1024).unwrap(),
        b"verified replacement"
    );
    assert!(!root.join("libraries/native.jar").exists());
}

#[test]
fn empty_pre_header_operation_recovers_without_writable_staging_registry() {
    let (_temporary, root) = fixture();
    let mut storage = open_with_area(&root, true);
    storage
        .filesystem
        .parent(&format!("{STORE}/d-a-b-c/header.json"), true)
        .unwrap()
        .unwrap();
    drop(storage);
    let corrupt = corrupt_registry(&root);
    let mut storage = open_with_area(&root, false);
    let operation = storage.operations().unwrap().pop().unwrap();
    assert!(operation.missing_header);
    recover(&mut storage, &operation, &|| Ok(())).unwrap();
    assert_eq!(
        names(&operation.directory, 8).unwrap(),
        ["done", "header.json"]
    );
    assert!(storage.operations().unwrap().is_empty());
    drop(operation);
    drop(storage);
    assert_eq!(
        fs::read(root.join(".pcl-linux/dependency-staging/registry.log")).unwrap(),
        corrupt
    );
}
