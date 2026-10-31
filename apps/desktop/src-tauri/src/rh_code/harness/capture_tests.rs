//! Actual source capture and immutable provenance on every native filesystem.
use super::*;
use std::fs;
struct Fixture {
    _temporary: tempfile::TempDir,
    base: std::path::PathBuf,
    project: Project,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let folder = base.join("source-project");
        fs::create_dir(&folder).unwrap();
        Self {
            _temporary: temporary,
            base,
            project: Project {
                id: "a".repeat(24),
                name: "Capture fixture".into(),
                folder: folder.to_str().unwrap().into(),
                updated_at: 0,
                targets: vec![],
                conversations: vec![],
                layout: json!({}),
                instructions: String::new(),
                build_policy: Default::default(),
            },
        }
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = Path::new(&self.project.folder).join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn storage(&self) -> std::path::PathBuf {
        self.base.join("owned-store")
    }
    fn bind(&self, snapshot: &Value) -> Result<(), String> {
        bind(
            &self.storage(),
            &self.project,
            "captured-job",
            "conversation",
            snapshot,
        )
    }
    fn bound(&self) -> Binding {
        load_binding(&store(&self.storage()).unwrap(), "captured-job").unwrap()
    }
}
#[test]
fn source_capture_keeps_host_manifest_and_real_native_root_identity() {
    let f = Fixture::new();
    f.write("z.txt", b"last");
    f.write("a/中.rs", b"first");
    f.write("binary.bin", &[0xff, 0]);
    f.write(".private/secret.txt", b"secret");
    f.write("build/output.txt", b"generated");
    let captured = capture(&f.project).unwrap();
    let entries: Vec<ManifestEntry> =
        serde_json::from_value(captured["sourceManifest"].clone()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, "a/中.rs");
    assert_eq!(entries[1].path, "z.txt");
    assert_eq!(
        captured["sourceFingerprint"],
        manifest_fingerprint(&entries).unwrap()
    );
    assert_eq!(captured["sourceOmissionCount"], 1);
    assert!(!serde_json::to_string(&captured).unwrap().contains("secret"));
    let actual = pcl_core::retained_fs::Directory::open_absolute(Path::new(&f.project.folder))
        .unwrap()
        .identity()
        .unwrap();
    #[cfg(unix)]
    match actual {
        pcl_core::retained_fs::Identity::Unix { device, inode } => {
            assert_eq!(
                captured["rootIdentity"],
                json!({"device":device,"inode":inode})
            );
        }
        _ => panic!("expected actual Unix identity"),
    }
    #[cfg(windows)]
    {
        assert!(
            matches!(actual, pcl_core::retained_fs::Identity::Windows { file, .. } if file != [0;16])
        );
        assert_eq!(
            captured["rootIdentity"],
            serde_json::to_value(actual).unwrap()
        );
        assert!(captured["rootIdentity"].get("inode").is_none());
    }
}
#[test]
fn immutable_binding_reopens_with_the_same_manifest_and_platform_schema() {
    let f = Fixture::new();
    f.write("Main.java", b"class Main {}");
    let captured = capture(&f.project).unwrap();
    f.bind(&captured).unwrap();
    f.bind(&captured).unwrap();
    let bound = f.bound();
    assert_eq!(bound.schema_version, BINDING_SCHEMA);
    assert_eq!(bound.source_fingerprint, captured["sourceFingerprint"]);
    assert_eq!(
        bound.root_identity,
        root(&f.project).unwrap().key().unwrap()
    );
    let mut foreign = serde_json::to_value(&bound).unwrap();
    foreign["schemaVersion"] = json!(if BINDING_SCHEMA == 1 { 2 } else { 1 });
    assert!(parse_binding(serde_json::from_value(foreign).unwrap()).is_err());
}
#[test]
fn binding_cannot_replace_an_existing_job_with_other_snapshot_bytes() {
    let f = Fixture::new();
    f.write("Main.java", b"first");
    let first = capture(&f.project).unwrap();
    f.bind(&first).unwrap();
    f.write("Main.java", b"second");
    let second = capture(&f.project).unwrap();
    assert!(f.bind(&second).is_err());
    assert_eq!(f.bound().source_fingerprint, first["sourceFingerprint"]);
    assert_eq!(
        fs::read(Path::new(&f.project.folder).join("Main.java")).unwrap(),
        b"second"
    );
}
#[test]
fn changed_root_refuses_binding_before_creating_a_store() {
    let f = Fixture::new();
    f.write("Main.java", b"source");
    let captured = capture(&f.project).unwrap();
    fs::rename(&f.project.folder, f.base.join("retained-source")).unwrap();
    fs::create_dir(&f.project.folder).unwrap();
    assert!(f.bind(&captured).is_err());
    assert!(!f.storage().exists());
    assert_eq!(
        fs::read(f.base.join("retained-source/Main.java")).unwrap(),
        b"source"
    );
}
#[test]
fn modified_physical_binding_is_preserved_and_never_overwritten() {
    let f = Fixture::new();
    f.write("Main.java", b"source");
    let captured = capture(&f.project).unwrap();
    f.bind(&captured).unwrap();
    let binding = f
        .storage()
        .join("harness-bindings")
        .join(binding_name("captured-job"));
    fs::write(&binding, b"external binding edit").unwrap();
    assert!(f.bind(&captured).is_err());
    assert_eq!(fs::read(binding).unwrap(), b"external binding edit");
}
#[test]
fn binary_build_inputs_are_opt_in_and_not_source_context() {
    let f = Fixture::new();
    f.write("Main.java", b"class Main {}");
    f.write(
        "src/main/resources/texture.png",
        &[0x89, b'P', b'N', b'G', 0],
    );
    let ordinary = capture(&f.project).unwrap();
    assert!(ordinary.get("buildFiles").is_none());
    let build = capture_with_build(&f.project, true).unwrap();
    assert_eq!(ordinary["sourceFingerprint"], build["sourceFingerprint"]);
    assert_eq!(
        build["buildFiles"][0]["path"],
        "src/main/resources/texture.png"
    );
    assert_eq!(build["sourceFiles"].as_array().unwrap().len(), 1);
}
#[test]
fn linked_or_oversized_files_are_omitted_without_changing_them() {
    let f = Fixture::new();
    f.write("original.txt", b"same");
    fs::hard_link(
        Path::new(&f.project.folder).join("original.txt"),
        Path::new(&f.project.folder).join("alias.txt"),
    )
    .unwrap();
    f.write("oversized.txt", &vec![b'x'; MAX_FILE + 1]);
    let captured = capture(&f.project).unwrap();
    assert_eq!(captured["sourceFiles"].as_array().unwrap().len(), 0);
    assert_eq!(captured["sourceOmissionCount"], 3);
    assert_eq!(
        fs::read(Path::new(&f.project.folder).join("original.txt")).unwrap(),
        b"same"
    );
}
#[cfg(target_os = "linux")]
#[test]
fn retained_read_adapter_and_document_bindings_run_on_the_host_filesystem() {
    let f = Fixture::new();
    f.write("Main.java", b"source");
    let directory =
        super::native_read_adapter::Dir::absolute(Path::new(&f.project.folder), false).unwrap();
    let retained = directory.child("").unwrap();
    assert_eq!(retained.names().unwrap(), vec!["Main.java"]);
    assert!(matches!(
        retained.facts("Main.java").unwrap().unwrap().kind,
        NodeKind::Regular
    ));
    let (state, bytes) = directory.read("Main.java", MAX_FILE).unwrap();
    assert_eq!(state.sha256, hash(b"source"));
    assert_eq!(bytes, b"source");
    assert_eq!(
        directory.key().unwrap(),
        root(&f.project).unwrap().key().unwrap()
    );
    let storage = super::native_read_adapter::Dir::absolute(&f.storage(), true).unwrap();
    assert!(storage.optional_child("absent").unwrap().is_none());
    let bindings = storage.ensure_child("harness-bindings").unwrap();
    bindings.immutable("job.json", b"immutable").unwrap();
    bindings.immutable("job.json", b"immutable").unwrap();
    assert!(bindings.immutable("job.json", b"different").is_err());
    assert_eq!(bindings.read_binding("job.json").unwrap(), b"immutable");
}
#[cfg(not(target_os = "linux"))]
#[test]
fn unavailable_publication_refuses_before_any_source_or_store_mutation() {
    let f = Fixture::new();
    f.write("Main.java", b"source");
    assert!(apply(&f.storage(), &f.project, &Value::Null, &Value::Null)
        .unwrap_err()
        .starts_with("HARNESS_APPLY_UNAVAILABLE"));
    assert!(save_assets(&f.storage(), &f.project, &[])
        .unwrap_err()
        .starts_with("HARNESS_APPLY_UNAVAILABLE"));
    assert!(!f.storage().exists());
    assert_eq!(
        fs::read(Path::new(&f.project.folder).join("Main.java")).unwrap(),
        b"source"
    );
}
#[cfg(not(target_os = "linux"))]
#[test]
fn native_recovery_only_reports_ready_after_inspecting_empty_state() {
    let f = Fixture::new();
    assert_eq!(
        recover(&f.storage(), &f.project).unwrap()["status"],
        "ready"
    );
    let storage = store(&f.storage()).unwrap();
    storage
        .ensure_child("harness-transactions")
        .unwrap()
        .ensure_child("retained-orphan")
        .unwrap();
    assert!(recover(&f.storage(), &f.project).is_err());
    assert!(f
        .storage()
        .join("harness-transactions/retained-orphan")
        .is_dir());
}
