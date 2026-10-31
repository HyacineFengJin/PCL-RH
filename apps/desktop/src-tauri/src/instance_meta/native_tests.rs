//! Synthetic offline scopes only: these tests exercise the actual MetadataStore
//! and read the current logical document, never the immutable Windows base.
use super::*;
use pcl_core::retained_fs::{Directory as NativeDirectory, Leaf, Traversal};
use std::{fs, path::PathBuf};

struct Fixture {
    _temp: tempfile::TempDir,
    project: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let parent = NativeDirectory::open_absolute(&base).unwrap();
        drop(
            parent
                .create_private(&Leaf::new("project").unwrap(), Traversal::AllowMounts)
                .unwrap(),
        );
        let project = base.join("project");
        let root = base.join("offline-game");
        Self {
            _temp: temp,
            project,
            root,
        }
    }
    fn store(&self) -> MetadataStore {
        MetadataStore::load(&self.project)
    }
    fn patch(&self, store: &MetadataStore, id: &str, patch: MetadataPatch) -> MetaView {
        let old = store.get("root-a", &self.root, id);
        store
            .patch("root-a", &self.root, id, &old.revision, patch)
            .unwrap()
    }
    fn bytes(&self) -> Vec<u8> {
        disk_snapshot(&Directory::open(&self.project).unwrap())
            .unwrap()
            .1
            .unwrap()
    }
    fn physical(&self) -> PathBuf {
        self.project.join(".pcl-rust").join(FILE_NAME)
    }
    fn storage(&self) -> NativeDirectory {
        let project = NativeDirectory::open_absolute(&self.project).unwrap();
        let leaf = Leaf::new(".pcl-rust").unwrap();
        match project.child(&leaf, Traversal::AllowMounts) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => project
                .create_private(&leaf, Traversal::AllowMounts)
                .unwrap(),
            Err(error) => panic!("{error}"),
        }
    }
}
fn description(value: &str) -> MetadataPatch {
    MetadataPatch {
        description: Some(value.into()),
        ..Default::default()
    }
}

#[test]
fn native_metadata_missing_read_is_empty_and_repeated_patches_reopen_logical_document() {
    let f = Fixture::new();
    let store = f.store();
    assert!(store.warning().is_none());
    assert_eq!(fs::read_dir(&f.project).unwrap().count(), 0);
    let initial = store.get("root-a", &f.root, "Example");
    let first = f.patch(&store, "Example", description("  第一行\r\n第二行  "));
    let base = fs::read(f.physical()).unwrap();
    let second = f.patch(
        &store,
        "Example",
        MetadataPatch {
            favorite: Some(true),
            icon: Some(Icon::Steve),
            category: Some(Category::Fabric),
            ..Default::default()
        },
    );
    assert_eq!(second.description, "第一行\n第二行");
    assert!(second.favorite);
    assert_ne!(first.revision, second.revision);
    assert_ne!(initial.revision, first.revision);
    assert_eq!(f.store().get("root-a", &f.root, "Example"), second);
    let data: Persisted = serde_json::from_slice(&f.bytes()).unwrap();
    assert_eq!(data.entries[0].revision, 2);
    assert_eq!(data.entries[0].metadata, second.metadata());
    #[cfg(windows)]
    assert_eq!(fs::read(f.physical()).unwrap(), base);
    #[cfg(unix)]
    assert_ne!(fs::read(f.physical()).unwrap(), base);
}

#[test]
fn native_metadata_stale_scopes_and_default_generation_never_reuse_revisions() {
    let f = Fixture::new();
    let store = f.store();
    let default = store.get("root-a", &f.root, "Example");
    let first = f.patch(&store, "Example", description("saved"));
    assert!(store
        .patch(
            "root-a",
            &f.root,
            "Example",
            &default.revision,
            description("stale")
        )
        .is_err());
    for (root_id, path, id) in [
        ("root-b", f.root.clone(), "Example"),
        ("root-a", f.root.with_file_name("other"), "Example"),
        ("root-a", f.root.clone(), "Other"),
    ] {
        assert!(store
            .patch(
                root_id,
                &path,
                id,
                &first.revision,
                description("wrong scope")
            )
            .is_err());
    }
    let cleared = f.patch(&store, "Example", description(""));
    assert_eq!(cleared.metadata(), Metadata::default());
    assert_ne!(cleared.revision, default.revision);
    assert_ne!(cleared.revision, first.revision);
    assert_eq!(f.store().get("root-a", &f.root, "Example"), cleared);
}

#[test]
fn native_metadata_external_physical_edit_is_preserved_and_blocks_logical_writes() {
    let f = Fixture::new();
    let store = f.store();
    f.patch(&store, "Example", description("base"));
    let old = f.patch(&store, "Example", description("current logical"));
    fs::write(f.physical(), b"external original edit").unwrap();
    assert!(store
        .patch(
            "root-a",
            &f.root,
            "Example",
            &old.revision,
            description("must fail")
        )
        .is_err());
    assert_eq!(store.get("root-a", &f.root, "Example"), old);
    assert!(store.warning().is_some());
    assert_eq!(fs::read(f.physical()).unwrap(), b"external original edit");
}

#[test]
fn native_metadata_rename_bytes_and_refresh_keep_order_scope_and_invalidate_old_names() {
    let f = Fixture::new();
    let store = f.store();
    f.patch(&store, "Unrelated", description("keep"));
    let old = f.patch(&store, "Old", description("move this"));
    let original = f.bytes();
    let renamed = rename_bytes(&original, "root-a", &f.root, "Old", "New").unwrap();
    let before: Persisted = serde_json::from_slice(&original).unwrap();
    let after: Persisted = serde_json::from_slice(&renamed).unwrap();
    assert_eq!(after.entries[0].instance_id, before.entries[0].instance_id);
    assert_eq!(after.entries[0].metadata, before.entries[0].metadata);
    assert_eq!(after.entries[1].instance_id, "Old");
    assert_eq!(after.entries[1].metadata, Metadata::default());
    assert_eq!(after.entries[2].instance_id, "New");
    assert_eq!(after.entries[2].metadata, old.metadata());
    // Model the reference writer with the same production fixed-document port.
    #[cfg(not(target_os = "linux"))]
    {
        let directory = Directory::open(&f.project)
            .unwrap()
            .storage()
            .unwrap()
            .unwrap();
        let (expected, _) = directory.read_file().unwrap().unwrap();
        directory.replace(Some(&expected), &renamed).unwrap();
    }
    #[cfg(target_os = "linux")]
    fs::write(f.physical(), renamed).unwrap();
    store.refresh_after_rename().unwrap();
    let moved = store.get("root-a", &f.root, "New");
    assert_eq!(moved.metadata(), old.metadata());
    assert_ne!(moved.revision, old.revision);
    assert_ne!(store.get("root-a", &f.root, "Old").revision, old.revision);
    assert!(store
        .ensure_new_instance_name("root-a", &f.root, "Old")
        .is_err());
}

#[test]
fn native_metadata_nonblocking_lock_and_failed_save_preserve_disk_and_memory() {
    let f = Fixture::new();
    let store = f.store();
    let old = f.patch(&store, "Example", description("before"));
    let original = f.bytes();
    let directory = Directory::open(&f.project)
        .unwrap()
        .storage()
        .unwrap()
        .unwrap();
    let lock = directory.write_lock().unwrap();
    assert!(store
        .patch(
            "root-a",
            &f.root,
            "Example",
            &old.revision,
            description("blocked")
        )
        .is_err());
    drop(lock);
    assert_eq!(f.bytes(), original);
    assert_eq!(store.get("root-a", &f.root, "Example"), old);
    store.lock().fail_before_replace = true;
    assert!(store
        .patch(
            "root-a",
            &f.root,
            "Example",
            &old.revision,
            description("injected failure")
        )
        .is_err());
    assert_eq!(f.bytes(), original);
    assert_eq!(store.get("root-a", &f.root, "Example"), old);
    assert!(store.warning().is_none());
    store.lock().fail_before_replace = false;
    f.patch(&store, "Example", description("retry"));
}

#[test]
fn native_metadata_future_corrupt_and_oversized_documents_are_read_only() {
    for bytes in [
        b"invalid json".to_vec(),
        br#"{"schema_version":2,"entries":[]}"#.to_vec(),
        vec![b' '; MAX_FILE_BYTES + 1],
    ] {
        let f = Fixture::new();
        let storage = f.storage();
        let mut file = storage.create_file(&Leaf::new(FILE_NAME).unwrap()).unwrap();
        use std::io::Write;
        file.write_all(&bytes).unwrap();
        drop(file);
        let store = f.store();
        let old = store.get("root-a", &f.root, "Example");
        assert!(store.warning().is_some());
        assert!(store
            .patch(
                "root-a",
                &f.root,
                "Example",
                &old.revision,
                description("blocked")
            )
            .is_err());
        assert_eq!(fs::read(f.physical()).unwrap(), bytes);
    }
}

#[test]
fn native_metadata_competing_stores_have_exactly_one_winner_and_preserve_other_fields() {
    let f = Fixture::new();
    let initial = f.store();
    f.patch(
        &initial,
        "Example",
        MetadataPatch {
            description: Some("base".into()),
            icon: Some(Icon::Steve),
            category: Some(Category::Fabric),
            ..Default::default()
        },
    );
    let first = f.store();
    let second = f.store();
    let old = first.get("root-a", &f.root, "Example");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    first.lock().before_write_lock = Some(barrier.clone());
    second.lock().before_write_lock = Some(barrier);
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            first.patch(
                "root-a",
                &f.root,
                "Example",
                &old.revision,
                description("writer one"),
            )
        });
        let two = scope.spawn(|| {
            second.patch(
                "root-a",
                &f.root,
                "Example",
                &old.revision,
                MetadataPatch {
                    favorite: Some(true),
                    ..Default::default()
                },
            )
        });
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_ne!(one.is_ok(), two.is_ok());
    let winner = one.as_ref().ok().or_else(|| two.as_ref().ok()).unwrap();
    assert_eq!(&f.store().get("root-a", &f.root, "Example"), winner);
    assert_eq!(winner.icon, Icon::Steve);
    assert_eq!(winner.category, Category::Fabric);
}

#[test]
fn native_metadata_large_document_allows_successive_full_size_logical_revisions() {
    let f = Fixture::new();
    let store = f.store();
    let mut data = Persisted::default();
    let description = "😀".repeat(MAX_DESCRIPTION_CHARS);
    for index in 0..300 {
        data.entries.push(Record {
            root_id: "root-a".into(),
            root_path: f.root.to_str().unwrap().into(),
            instance_id: format!("Example-{index}"),
            revision: 1,
            metadata: Metadata {
                description: description.clone(),
                ..Default::default()
            },
        });
    }
    let size = serde_json::to_vec_pretty(&data).unwrap().len();
    assert!(size > 4 * 1024 * 1024 && size < MAX_FILE_BYTES);
    for generation in 1..=3 {
        for record in &mut data.entries {
            record.revision = generation;
        }
        store.commit(&mut store.lock(), data.clone()).unwrap();
        let reopened: Persisted = serde_json::from_slice(&f.bytes()).unwrap();
        assert_eq!(reopened.entries.len(), 300);
        assert!(reopened
            .entries
            .iter()
            .all(|record| record.revision == generation));
    }
    assert!(f.store().warning().is_none());
}

#[test]
fn native_metadata_pending_reference_marker_blocks_writes_without_changing_document() {
    for name in [
        "instance-rename-pending.json",
        "instance-delete-pending.json",
    ] {
        let f = Fixture::new();
        let store = f.store();
        let old = f.patch(&store, "Example", description("kept"));
        let original = f.bytes();
        let mut file = f.storage().create_file(&Leaf::new(name).unwrap()).unwrap();
        use std::io::Write;
        file.write_all(b"unknown pending evidence").unwrap();
        drop(file);
        assert!(store
            .patch(
                "root-a",
                &f.root,
                "Example",
                &old.revision,
                description("blocked")
            )
            .is_err());
        assert_eq!(f.bytes(), original);
        assert_eq!(store.get("root-a", &f.root, "Example"), old);
        assert_eq!(
            fs::read(f.project.join(".pcl-rust").join(name)).unwrap(),
            b"unknown pending evidence"
        );
    }
}
