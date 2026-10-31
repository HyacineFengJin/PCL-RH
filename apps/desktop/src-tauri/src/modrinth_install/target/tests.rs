//! Real target graph and source lifetime regressions shared by desktop/wrapper.
use super::*;
use std::{fs, io::Write};
struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let parent = pcl_core::retained_fs::Directory::open_absolute(&base).unwrap();
        for name in ["game", "project"] {
            parent
                .create_private(
                    &pcl_core::retained_fs::Leaf::new(name).unwrap(),
                    pcl_core::retained_fs::Traversal::SameFilesystem,
                )
                .unwrap();
        }
        let root = base.join("game");
        let project = base.join("project");
        fs::create_dir_all(root.join("versions/sample/mods")).unwrap();
        fs::write(root.join("versions/sample/sample.json"), br#"{"id":"sample","clientVersion":"1.20.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]}"#).unwrap();
        Self {
            _temporary: temporary,
            root,
            project,
        }
    }
    fn capture(&self) -> TargetSnapshot {
        capture(
            &self.root,
            &self.project,
            "root-fixture",
            "sample",
            &AtomicBool::new(false),
        )
        .unwrap()
    }
}
#[test]
fn actual_inventory_token_matches_resource_panel_and_detects_same_size_edit() {
    let fixture = Fixture::new();
    let path = fixture
        .root
        .join("versions/sample/mods/sample.jar.disabled");
    fs::write(&path, b"resource one").unwrap();
    let target = fixture.capture();
    assert_eq!(target.local_files.len(), 1);
    assert!(!target.local_files[0].enabled);
    assert_eq!(
        target.local_files[0].resource_fingerprint,
        crate::resource_ops::token_for(&File::open(&path).unwrap()).unwrap()
    );
    check(&target, &AtomicBool::new(false)).unwrap();
    fs::write(path, b"resource two").unwrap();
    assert!(check(&target, &AtomicBool::new(false)).is_err());
}
#[test]
fn unknown_stage_prefix_is_bound_as_inventory_evidence() {
    let fixture = Fixture::new();
    let path = fixture
        .root
        .join("versions/sample/mods/.pcl-resource-stage-unknown");
    fs::write(&path, b"user-owned").unwrap();
    let target = fixture.capture();
    assert!(target.views["mods"]
        .others
        .contains_key(".pcl-resource-stage-unknown"));
    fs::remove_file(path).unwrap();
    assert!(check(&target, &AtomicBool::new(false)).is_err());
}
#[test]
fn hardlinked_download_target_is_refused_without_modifying_either_name() {
    let fixture = Fixture::new();
    let path = fixture.root.join("versions/sample/mods/sample.jar");
    fs::write(&path, b"original").unwrap();
    fs::hard_link(&path, fixture.project.join("original")).unwrap();
    assert!(capture(
        &fixture.root,
        &fixture.project,
        "root-fixture",
        "sample",
        &AtomicBool::new(false)
    )
    .is_err());
    assert_eq!(fs::read(path).unwrap(), b"original");
    assert_eq!(
        fs::read(fixture.project.join("original")).unwrap(),
        b"original"
    );
}
#[test]
fn actual_download_writer_retires_its_typed_source() {
    let fixture = Fixture::new();
    let staging = Dir::open(&fixture.project)
        .unwrap()
        .ensure(".pcl-rust")
        .unwrap()
        .ensure("resource-downloads")
        .unwrap()
        .with_input_sources()
        .unwrap();
    let mut file = staging.allocate(64).unwrap();
    file.write_all(b"bounded network bytes").unwrap();
    file.sync_all().unwrap();
    #[cfg(target_os = "linux")]
    assert_eq!(file.metadata().unwrap().nlink(), 0);
    #[cfg(not(target_os = "linux"))]
    file.verify_owned().unwrap();
    drop(file);
    staging.check_cleanup().unwrap();
    #[cfg(not(target_os = "linux"))]
    {
        let area = source::open_area(&staging.0, "registered-sources", MAX_BATCH_BYTES).unwrap();
        assert_eq!(area.pending().unwrap(), 0);
    }
    assert!(staging.allocate(MAX_FILE_BYTES + 1).is_err());
}
