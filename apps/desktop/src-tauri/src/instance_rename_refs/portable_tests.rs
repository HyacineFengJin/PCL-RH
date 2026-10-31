//! Actual reference domain transformations, persistent replay and real native
//! documents. No game/JVM/network process or user directory is used.
use super::*;
use crate::{
    config::ConfigStore,
    instance_meta::{MetadataPatch, MetadataStore},
};
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use serde_json::{json, Value};
use std::{fs, io::Write};

struct Fixture {
    _temporary: tempfile::TempDir,
    project: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap();
        let parent = Directory::open_absolute(&path).unwrap();
        let project = parent
            .create_private(&Leaf::new("project").unwrap(), Traversal::AllowMounts)
            .unwrap();
        let game = project
            .create_private(&Leaf::new("game").unwrap(), Traversal::AllowMounts)
            .unwrap();
        let versions = game
            .create_private(&Leaf::new("versions").unwrap(), Traversal::AllowMounts)
            .unwrap();
        let old = versions
            .create_private(&Leaf::new("Old").unwrap(), Traversal::AllowMounts)
            .unwrap();
        let mut file = old.create_file(&Leaf::new("Old.json").unwrap()).unwrap();
        file.write_all(br#"{"id":"Old","libraries":[]}"#).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let project_path = path.join("project");
        let fixture = Self {
            _temporary: temporary,
            root: project_path.join("game"),
            project: project_path,
        };
        let settings = json!({"schema_version":3,"active_root_id":"root-a","player":"FixturePlayer","memory_gib":10,"memory_mode":"manual","isolation":"automatic","java":{"mode":"auto"},"java_paths":[],
            "roots":[{"id":"root-a","name":"A","path":fixture.root,"selected":"Old","overrides":{"Old":12,"Unrelated":8},"memory_mode_overrides":{},"java_overrides":{},"isolation_overrides":{},"window_title_overrides":{},"argument_overrides":{}}]});
        let directory = app_directory(&fixture.project, true).unwrap().unwrap();
        let before = directory
            .snapshot("settings.json", SETTINGS_LIMIT)
            .unwrap()
            .0;
        directory
            .update_checked(
                "settings.json",
                &before,
                Some(&serde_json::to_vec_pretty(&settings).unwrap()),
                SETTINGS_LIMIT,
            )
            .unwrap();
        fixture
    }
    fn refs(&self, old: &str, new: &str) -> RenameReferences {
        prepare(&self.project, "root-a", &self.root, old, new).unwrap()
    }
    fn settings(&self) -> Value {
        serde_json::from_slice(
            &store_snapshot(&self.project, "settings.json", SETTINGS_LIMIT)
                .unwrap()
                .1
                .unwrap(),
        )
        .unwrap()
    }
    fn request() -> crate::instance_export::ExportRequest {
        serde_json::from_value(
            json!({"name":"Fixture Pack","version":"1.0.0","checks":{},"excluded":{}}),
        )
        .unwrap()
    }
    fn preset(&self, id: &str) -> Option<crate::instance_export::ExportRequest> {
        crate::export_presets::read(&self.project, "root-a", &self.root, id).unwrap()
    }
}
fn roundtrip(refs: &RenameReferences) -> RenameReferences {
    let bytes = serde_json::to_vec(refs).unwrap();
    let recovered: RenameReferences = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(recovered.revision().unwrap(), refs.revision().unwrap());
    recovered
}
#[test]
fn actual_settings_metadata_and_preset_replay_reactivate_deleted_preset_on_rename_back() {
    let fixture = Fixture::new();
    let (config, warning) = ConfigStore::load(&fixture.project);
    assert!(warning.is_none(), "{warning:?}");
    let metadata = MetadataStore::load(&fixture.project);
    let initial = metadata.get("root-a", &fixture.root, "Old");
    metadata
        .patch(
            "root-a",
            &fixture.root,
            "Old",
            &initial.revision,
            MetadataPatch {
                description: Some("retained description".into()),
                ..Default::default()
            },
        )
        .unwrap();
    crate::export_presets::save(
        &fixture.project,
        "root-a",
        &fixture.root,
        "Old",
        Fixture::request(),
    )
    .unwrap();
    let refs = roundtrip(&fixture.refs("Old", "New"));
    refs.verify_before(&fixture.project, &fixture.root).unwrap();
    refs.mark_pending(&fixture.project, &fixture.root, "n-1-2-3")
        .unwrap();
    assert!(ensure_project_ready(&fixture.project).is_err());
    refs.apply(&fixture.project, &fixture.root).unwrap();
    roundtrip(&refs)
        .apply(&fixture.project, &fixture.root)
        .unwrap();
    refs.clear_pending(&fixture.project, &fixture.root, "n-1-2-3")
        .unwrap();
    config.refresh_after_rename().unwrap();
    metadata.refresh_after_rename().unwrap();
    assert_eq!(config.snapshot().selected.as_deref(), Some("New"));
    assert_eq!(
        metadata.get("root-a", &fixture.root, "New").description,
        "retained description"
    );
    assert_eq!(fixture.preset("Old"), None);
    assert_eq!(fixture.preset("New"), Some(Fixture::request()));
    assert_eq!(fixture.settings()["roots"][0]["overrides"]["Unrelated"], 8);
    let back = roundtrip(&fixture.refs("New", "Old"));
    back.mark_pending(&fixture.project, &fixture.root, "n-1-2-4")
        .unwrap();
    back.apply(&fixture.project, &fixture.root).unwrap();
    back.clear_pending(&fixture.project, &fixture.root, "n-1-2-4")
        .unwrap();
    config.refresh_after_rename().unwrap();
    metadata.refresh_after_rename().unwrap();
    assert_eq!(config.snapshot().selected.as_deref(), Some("Old"));
    assert_eq!(
        metadata.get("root-a", &fixture.root, "Old").description,
        "retained description"
    );
    assert_eq!(fixture.preset("Old"), Some(Fixture::request()));
    assert_eq!(fixture.preset("New"), None);
}
#[test]
fn replay_reopens_after_one_document_commit_and_skips_exact_after_content() {
    let fixture = Fixture::new();
    crate::export_presets::save(
        &fixture.project,
        "root-a",
        &fixture.root,
        "Old",
        Fixture::request(),
    )
    .unwrap();
    let refs = fixture.refs("Old", "New");
    refs.mark_pending(&fixture.project, &fixture.root, "n-4-5-6")
        .unwrap();
    let settings = refs
        .changes
        .iter()
        .find(|change| change.target == Target::Settings)
        .unwrap();
    let (directory, name) = refs
        .location(&fixture.project, &fixture.root, &settings.target)
        .unwrap();
    directory
        .update_checked(
            &name,
            &settings.snapshot,
            decoded(&settings.after, SETTINGS_LIMIT).unwrap().as_deref(),
            SETTINGS_LIMIT,
        )
        .unwrap();
    let before = store_snapshot(&fixture.project, "settings.json", SETTINGS_LIMIT)
        .unwrap()
        .0;
    let recovered = roundtrip(&refs);
    recovered.apply(&fixture.project, &fixture.root).unwrap();
    assert_eq!(
        store_snapshot(&fixture.project, "settings.json", SETTINGS_LIMIT)
            .unwrap()
            .0,
        before
    );
    recovered
        .clear_pending(&fixture.project, &fixture.root, "n-4-5-6")
        .unwrap();
    assert_eq!(fixture.settings()["roots"][0]["selected"], "New");
    assert_eq!(fixture.preset("Old"), None);
    assert_eq!(fixture.preset("New"), Some(Fixture::request()));
}
#[test]
fn third_state_preflight_preserves_every_other_reference_and_pending_marker() {
    let fixture = Fixture::new();
    crate::export_presets::save(
        &fixture.project,
        "root-a",
        &fixture.root,
        "Old",
        Fixture::request(),
    )
    .unwrap();
    let refs = fixture.refs("Old", "New");
    refs.mark_pending(&fixture.project, &fixture.root, "n-7-8-9")
        .unwrap();
    let (directory, name) = refs
        .location(&fixture.project, &fixture.root, &Target::Settings)
        .unwrap();
    let current = directory.snapshot(&name, SETTINGS_LIMIT).unwrap().0;
    let mut settings = fixture.settings();
    settings["memory_gib"] = json!(18);
    directory
        .update_checked(
            &name,
            &current,
            Some(&serde_json::to_vec(&settings).unwrap()),
            SETTINGS_LIMIT,
        )
        .unwrap();
    let metadata = store_snapshot(&fixture.project, "instance-metadata.json", METADATA_LIMIT)
        .unwrap()
        .0;
    assert!(roundtrip(&refs)
        .apply(&fixture.project, &fixture.root)
        .is_err());
    assert_eq!(fixture.settings(), settings);
    assert_eq!(fixture.preset("Old"), Some(Fixture::request()));
    assert_eq!(fixture.preset("New"), None);
    assert_eq!(
        store_snapshot(&fixture.project, "instance-metadata.json", METADATA_LIMIT)
            .unwrap()
            .0,
        metadata
    );
    assert_eq!(
        pending_operation(&fixture.project).unwrap().as_deref(),
        Some("n-7-8-9")
    );
}
#[test]
fn offline_root_and_replaced_marker_never_release_cooperating_writers() {
    let fixture = Fixture::new();
    let refs = fixture.refs("Old", "New");
    refs.mark_pending(&fixture.project, &fixture.root, "n-a-b-c")
        .unwrap();
    let offline = fixture.project.join("offline");
    fs::rename(&fixture.root, &offline).unwrap();
    assert_eq!(
        pending_root(&fixture.project).unwrap(),
        Some(fixture.root.clone())
    );
    assert!(ensure_project_ready(&fixture.project).is_err());
    fs::rename(&offline, &fixture.root).unwrap();
    let directory = app_directory(&fixture.project, false).unwrap().unwrap();
    let (_, bytes) = directory.read(MARKER, PRESET_LIMIT).unwrap().unwrap();
    let marker = fixture.project.join(".pcl-rust").join(MARKER);
    fs::rename(&marker, marker.with_file_name("preserved-marker.json")).unwrap();
    directory
        .write_backup(MARKER, &bytes, PRESET_LIMIT)
        .unwrap();
    assert!(refs
        .clear_pending(&fixture.project, &fixture.root, "n-a-b-c")
        .is_err());
    assert!(ensure_project_ready(&fixture.project).is_err());
    assert_eq!(fs::read(marker).unwrap(), bytes);
}
