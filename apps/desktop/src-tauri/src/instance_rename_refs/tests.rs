use super::*;
use crate::{
    config::ConfigStore,
    instance_meta::{Category, Icon, MetadataPatch, MetadataStore},
};
use serde_json::{json, Value};
use std::{fs, sync::atomic::AtomicBool};

struct Fixture {
    project: PathBuf,
    root: PathBuf,
    other: PathBuf,
}
impl Fixture {
    fn new(isolated: bool) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/java-management-2026-10-04/rename-ref-fixtures")
            .join(nonce());
        fs::create_dir_all(&path).unwrap();
        let project = path.canonicalize().unwrap();
        let root = project.join("game");
        let other = project.join("other-game");
        fs::create_dir_all(root.join("versions/Old")).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(
            root.join("versions/Old/Old.json"),
            br#"{"id":"Old","libraries":[]}"#,
        )
        .unwrap();
        fs::write(root.join("versions/Old/Old.jar"), b"fixture").unwrap();
        if isolated {
            fs::create_dir(root.join("versions/Old/mods")).unwrap();
        } else {
            fs::create_dir(root.join("mods")).unwrap();
        }
        fs::create_dir(project.join(".pcl-rust")).unwrap();
        let fixture = Self {
            project,
            root,
            other,
        };
        fixture.save_settings(json!({"schema_version":3,"active_root_id":"root-a","player":"FixturePlayer","memory_gib":10,"memory_mode":"manual","isolation":"automatic","java":{"mode":"auto"},"java_paths":[],
            "roots":[{"id":"root-a","name":"A","path":fixture.root,"selected":"Old","overrides":{"Old":12,"Unrelated":8},"memory_mode_overrides":{},"java_overrides":{},"isolation_overrides":{},"window_title_overrides":{},"argument_overrides":{}},
            {"id":"root-b","name":"B","path":fixture.other,"selected":"Old","overrides":{"Old":6},"memory_mode_overrides":{},"java_overrides":{},"isolation_overrides":{},"window_title_overrides":{},"argument_overrides":{}}]}));
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.project.join(".pcl-rust").join(name)
    }
    fn save_settings(&self, value: Value) {
        fs::write(
            self.path("settings.json"),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
    fn settings(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("settings.json")).unwrap()).unwrap()
    }
    fn refs(&self) -> RenameReferences {
        prepare(&self.project, "root-a", &self.root, "Old", "New").unwrap()
    }
    fn request() -> crate::instance_export::ExportRequest {
        serde_json::from_value(
            json!({"name":"Fixture Pack","version":"1.0.0","checks":{},"excluded":{}}),
        )
        .unwrap()
    }
    fn metadata(&self) -> MetadataStore {
        let store = MetadataStore::load(&self.project);
        let old = store.get("root-a", &self.root, "Old");
        store
            .patch(
                "root-a",
                &self.root,
                "Old",
                &old.revision,
                MetadataPatch {
                    description: Some("Fixture description".into()),
                    favorite: Some(true),
                    icon: Some(Icon::Forge),
                    category: Some(Category::Forge),
                },
            )
            .unwrap();
        store
    }
    fn undo(&self, isolated: bool) -> String {
        let folder = if isolated {
            self.root.join("versions/Old/mods")
        } else {
            self.root.join("mods")
        };
        let path = folder.join("fixture.jar");
        fs::write(&path, b"retained resource").unwrap();
        let selected = crate::resource_ops::ResourceFile {
            file_name: "fixture.jar".into(),
            fingerprint: crate::resource_ops::fingerprint(&path).unwrap(),
        };
        crate::resource_ops::remove(
            &self.root,
            "Old",
            "mods",
            &[selected],
            &AtomicBool::new(false),
            &mut || Ok(()),
            &mut |_, _| {},
        )
        .unwrap()
        .undo_id
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.project);
    }
}

#[test]
fn selected_override_metadata_and_preset_move_without_other_state_changes() {
    let fixture = Fixture::new(true);
    let (config, warning) = ConfigStore::load(&fixture.project);
    assert!(warning.is_none());
    let metadata = fixture.metadata();
    let before = metadata.get("root-a", &fixture.root, "Old");
    crate::export_presets::save(
        &fixture.project,
        "root-a",
        &fixture.root,
        "Old",
        Fixture::request(),
    )
    .unwrap();
    let settings_before = fixture.settings();
    config.ensure_rename_snapshot().unwrap();
    metadata.ensure_rename_snapshot().unwrap();
    let refs = fixture.refs();
    let revision = refs.revision().unwrap();
    let decoded: RenameReferences =
        serde_json::from_slice(&serde_json::to_vec(&refs).unwrap()).unwrap();
    assert_eq!(decoded.revision().unwrap(), revision);
    refs.verify_before(&fixture.project, &fixture.root).unwrap();
    refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
    refs.apply(&fixture.project, &fixture.root).unwrap();
    refs.apply(&fixture.project, &fixture.root).unwrap();
    refs.clear_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
    config.refresh_after_rename().unwrap();
    metadata.refresh_after_rename().unwrap();
    let settings = fixture.settings();
    assert_eq!(settings["roots"][0]["selected"], "New");
    assert_eq!(settings["roots"][0]["overrides"]["New"], 12);
    assert!(settings["roots"][0]["overrides"].get("Old").is_none());
    assert_eq!(settings["roots"][0]["overrides"]["Unrelated"], 8);
    assert_eq!(settings["roots"][1], settings_before["roots"][1]);
    for field in ["player", "memory_gib", "active_root_id"] {
        assert_eq!(settings[field], settings_before[field]);
    }
    assert_eq!(config.snapshot().selected.as_deref(), Some("New"));
    let after = metadata.get("root-a", &fixture.root, "New");
    assert_eq!(after.metadata(), before.metadata());
    assert_ne!(after.revision, before.revision);
    assert_eq!(metadata.get("root-a", &fixture.root, "Old").description, "");
    assert!(
        crate::export_presets::read(&fixture.project, "root-a", &fixture.root, "Old")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        crate::export_presets::read(&fixture.project, "root-a", &fixture.root, "New")
            .unwrap()
            .unwrap(),
        Fixture::request()
    );
    assert!(!fixture.path(MARKER).exists());
}

#[test]
fn historical_undo_remains_usable_after_isolated_and_global_rename() {
    for isolated in [true, false] {
        let fixture = Fixture::new(isolated);
        let undo = fixture.undo(isolated);
        let refs = fixture.refs();
        refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
            .unwrap();
        fs::rename(
            fixture.root.join("versions/Old"),
            fixture.root.join("versions/New"),
        )
        .unwrap();
        fs::remove_file(fixture.root.join("versions/New/Old.json")).unwrap();
        fs::write(
            fixture.root.join("versions/New/New.json"),
            br#"{"id":"New","libraries":[]}"#,
        )
        .unwrap();
        fs::rename(
            fixture.root.join("versions/New/Old.jar"),
            fixture.root.join("versions/New/New.jar"),
        )
        .unwrap();
        refs.apply(&fixture.project, &fixture.root).unwrap();
        refs.clear_pending(&fixture.project, &fixture.root, "n-123-456-1")
            .unwrap();
        let journal_path = fixture
            .root
            .join(".pcl-linux/resource-operations")
            .join(&undo)
            .join("journal.json");
        let journal: Value = serde_json::from_slice(&fs::read(journal_path).unwrap()).unwrap();
        assert_eq!(journal["instance_id"], "New");
        assert_eq!(
            journal["resource_relative"],
            if isolated {
                "versions/New/mods"
            } else {
                "mods"
            }
        );
        assert_eq!(
            crate::resource_ops::removed(&fixture.root, "New", "mods").unwrap()[0].id,
            undo
        );
        crate::resource_ops::restore(
            &fixture.root,
            "New",
            "mods",
            &undo,
            &AtomicBool::new(false),
            &mut || Ok(()),
            &mut |_, _| {},
        )
        .unwrap();
        let restored = fixture.root.join(if isolated {
            "versions/New/mods/fixture.jar"
        } else {
            "mods/fixture.jar"
        });
        assert_eq!(fs::read(restored).unwrap(), b"retained resource");
    }
}

#[test]
fn partial_replay_is_idempotent_and_external_user_updates_are_preserved() {
    let fixture = Fixture::new(true);
    fixture.metadata();
    let refs = fixture.refs();
    refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
    let settings = refs
        .changes
        .iter()
        .find(|change| change.target == Target::Settings)
        .unwrap();
    let (dir, name) = refs
        .location(&fixture.project, &fixture.root, &settings.target)
        .unwrap();
    dir.update(
        &name,
        settings.snapshot.file.as_ref(),
        decoded(&settings.after, SETTINGS_LIMIT).unwrap().as_deref(),
        SETTINGS_LIMIT,
    )
    .unwrap();
    refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
    let mut edited = fixture.settings();
    edited["memory_gib"] = json!(18);
    fixture.save_settings(edited.clone());
    assert!(refs.apply(&fixture.project, &fixture.root).is_err());
    assert_eq!(fixture.settings(), edited);
    assert!(fixture.path(MARKER).exists());
    // Restoring the exact prepared after state permits recovery to finish.
    fs::write(
        fixture.path("settings.json"),
        decoded(&settings.after, SETTINGS_LIMIT).unwrap().unwrap(),
    )
    .unwrap();
    refs.apply(&fixture.project, &fixture.root).unwrap();
    refs.clear_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
}

#[test]
fn edits_between_verify_and_marker_are_rejected_before_commit() {
    let fixture = Fixture::new(true);
    let refs = fixture.refs();
    refs.verify_before(&fixture.project, &fixture.root).unwrap();
    let mut edited = fixture.settings();
    edited["player"] = json!("EditedPlayer");
    fixture.save_settings(edited.clone());
    assert!(refs
        .mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .is_err());
    assert!(!fixture.path(MARKER).exists());
    assert_eq!(fixture.settings(), edited);
}

#[test]
fn pending_marker_blocks_all_reference_writers_and_survives_offline_root() {
    let fixture = Fixture::new(true);
    let (config, _) = ConfigStore::load(&fixture.project);
    let metadata = fixture.metadata();
    let refs = fixture.refs();
    refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
    let original = fs::read(fixture.path("settings.json")).unwrap();
    let mut settings = config.snapshot();
    settings.memory_gib = 18;
    assert!(config.save(settings).unwrap_err().contains(PENDING_ERROR));
    let old = metadata.get("root-a", &fixture.root, "Old");
    assert!(metadata
        .patch(
            "root-a",
            &fixture.root,
            "Old",
            &old.revision,
            MetadataPatch {
                description: Some("Blocked".into()),
                ..MetadataPatch::default()
            }
        )
        .unwrap_err()
        .contains(PENDING_ERROR));
    assert!(crate::export_presets::save(
        &fixture.project,
        "root-a",
        &fixture.root,
        "Old",
        Fixture::request()
    )
    .unwrap_err()
    .contains(PENDING_ERROR));
    assert_eq!(fs::read(fixture.path("settings.json")).unwrap(), original);
    let offline = fixture.project.join("offline");
    fs::rename(&fixture.root, &offline).unwrap();
    assert_eq!(
        pending_root(&fixture.project).unwrap(),
        Some(fixture.root.clone())
    );
    assert_eq!(
        pending_operation(&fixture.project).unwrap().as_deref(),
        Some("n-123-456-1")
    );
    assert!(ensure_project_ready(&fixture.project)
        .unwrap_err()
        .contains(PENDING_ERROR));
    fs::rename(offline, &fixture.root).unwrap();
    refs.clear_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
}

#[test]
fn target_override_saved_metadata_and_preset_collisions_are_preserved() {
    let fixture = Fixture::new(true);
    let mut settings = fixture.settings();
    settings["roots"][0]["overrides"]["New"] = json!(14);
    fixture.save_settings(settings.clone());
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_err());
    assert_eq!(fixture.settings(), settings);
    settings["roots"][0]["overrides"]
        .as_object_mut()
        .unwrap()
        .remove("New");
    fixture.save_settings(settings);
    fs::write(
        fixture.path("instance-metadata.json"),
        serde_json::to_vec(&json!({"schema_version":1,"entries":[{
        "root_id":"root-a","root_path":fixture.root,"instance_id":"New","revision":3,
        "metadata":{"description":"Saved target description","favorite":false,"icon":"auto","category":"auto"}}]}))
        .unwrap(),
    )
    .unwrap();
    let original = fs::read(fixture.path("instance-metadata.json")).unwrap();
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_err());
    assert_eq!(
        fs::read(fixture.path("instance-metadata.json")).unwrap(),
        original
    );
    fs::remove_file(fixture.path("instance-metadata.json")).unwrap();
    crate::export_presets::save(
        &fixture.project,
        "root-a",
        &fixture.root,
        "New",
        Fixture::request(),
    )
    .unwrap();
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_err());
}

#[test]
fn rename_back_preserves_metadata_and_invalidates_all_old_tokens() {
    for saved in [false, true] {
        let fixture = Fixture::new(true);
        let metadata = if saved {
            fixture.metadata()
        } else {
            MetadataStore::load(&fixture.project)
        };
        let initial_old = metadata.get("root-a", &fixture.root, "Old");
        let initial_target = metadata.get("root-a", &fixture.root, "New");
        let refs = fixture.refs();
        refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
            .unwrap();
        refs.apply(&fixture.project, &fixture.root).unwrap();
        refs.clear_pending(&fixture.project, &fixture.root, "n-123-456-1")
            .unwrap();
        metadata.refresh_after_rename().unwrap();
        let cleared_old = metadata.get("root-a", &fixture.root, "Old");
        let moved = metadata.get("root-a", &fixture.root, "New");
        assert_eq!(moved.metadata(), initial_old.metadata());
        assert_ne!(initial_old.revision, cleared_old.revision);
        assert_ne!(initial_target.revision, moved.revision);
        assert!(metadata
            .patch(
                "root-a",
                &fixture.root,
                "Old",
                &initial_old.revision,
                MetadataPatch {
                    description: Some("Stale write".into()),
                    ..Default::default()
                }
            )
            .is_err());
        let back = prepare(&fixture.project, "root-a", &fixture.root, "New", "Old").unwrap();
        back.mark_pending(&fixture.project, &fixture.root, "n-123-456-2")
            .unwrap();
        back.apply(&fixture.project, &fixture.root).unwrap();
        back.clear_pending(&fixture.project, &fixture.root, "n-123-456-2")
            .unwrap();
        metadata.refresh_after_rename().unwrap();
        let returned = metadata.get("root-a", &fixture.root, "Old");
        assert_eq!(returned.metadata(), initial_old.metadata());
        assert_eq!(metadata.get("root-a", &fixture.root, "New").description, "");
        for token in [&initial_old.revision, &cleared_old.revision] {
            assert_ne!(*token, returned.revision);
            assert!(metadata
                .patch(
                    "root-a",
                    &fixture.root,
                    "Old",
                    token,
                    MetadataPatch {
                        description: Some("Stale write".into()),
                        ..Default::default()
                    }
                )
                .is_err());
        }
        let data: Value =
            serde_json::from_slice(&fs::read(fixture.path("instance-metadata.json")).unwrap())
                .unwrap();
        assert_eq!(data["entries"].as_array().unwrap().len(), 2);
        for entry in data["entries"].as_array().unwrap() {
            assert_eq!(entry["revision"], if saved { 3 } else { 2 });
        }
    }
}

#[test]
fn default_target_generation_is_retained_and_advanced() {
    let fixture = Fixture::new(true);
    fs::write(
        fixture.path("instance-metadata.json"),
        serde_json::to_vec(&json!({"schema_version":1,"entries":[{
        "root_id":"root-a","root_path":fixture.root,"instance_id":"New","revision":7,
        "metadata":{"description":"","favorite":false,"icon":"auto","category":"auto"}}]}))
        .unwrap(),
    )
    .unwrap();
    let metadata = MetadataStore::load(&fixture.project);
    let target = metadata.get("root-a", &fixture.root, "New");
    let refs = fixture.refs();
    refs.apply(&fixture.project, &fixture.root).unwrap();
    metadata.refresh_after_rename().unwrap();
    assert_ne!(
        target.revision,
        metadata.get("root-a", &fixture.root, "New").revision
    );
    let data: Value =
        serde_json::from_slice(&fs::read(fixture.path("instance-metadata.json")).unwrap()).unwrap();
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry["instance_id"] == "New")
            .unwrap()["revision"],
        8
    );
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry["instance_id"] == "Old")
            .unwrap()["revision"],
        1
    );
}

#[test]
fn metadata_generation_and_added_record_limits_refuse_without_writes() {
    let fixture = Fixture::new(true);
    let record = |id: &str, generation: u64| {
        json!({
        "root_id":"root-a","root_path":fixture.root,"instance_id":id,"revision":generation,
        "metadata":{"description":"","favorite":false,"icon":"auto","category":"auto"}})
    };
    for name in ["Old", "New"] {
        let bytes =
            serde_json::to_vec(&json!({"schema_version":1,"entries":[record(name,u64::MAX)]}))
                .unwrap();
        fs::write(fixture.path("instance-metadata.json"), &bytes).unwrap();
        assert!(
            prepare(&fixture.project, "root-a", &fixture.root, "Old", "New")
                .unwrap_err()
                .contains("修订号已耗尽")
        );
        assert_eq!(
            fs::read(fixture.path("instance-metadata.json")).unwrap(),
            bytes
        );
    }
    let entries: Vec<_> = (0..4095)
        .map(|index| record(&format!("Fixture{index}"), 1))
        .collect();
    let bytes = serde_json::to_vec(&json!({"schema_version":1,"entries":entries})).unwrap();
    fs::write(fixture.path("instance-metadata.json"), &bytes).unwrap();
    assert!(
        prepare(&fixture.project, "root-a", &fixture.root, "Old", "New")
            .unwrap_err()
            .contains("4096")
    );
    assert_eq!(
        fs::read(fixture.path("instance-metadata.json")).unwrap(),
        bytes
    );
}

#[test]
fn malformed_future_and_symlink_files_are_never_overwritten() {
    let fixture = Fixture::new(true);
    let mut future = fixture.settings();
    future["schema_version"] = json!(99);
    fixture.save_settings(future.clone());
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_err());
    assert_eq!(fixture.settings(), future);
    let original = fixture.path("settings.json");
    fs::remove_file(&original).unwrap();
    let protected = fixture.project.join("protected");
    fs::write(&protected, b"protected content").unwrap();
    std::os::unix::fs::symlink(&protected, &original).unwrap();
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_err());
    assert_eq!(fs::read(protected).unwrap(), b"protected content");
    let (config, warning) = ConfigStore::load(&fixture.project);
    assert!(warning.is_some());
    assert!(config.ensure_writable().is_err());
}

#[test]
fn forged_payload_paths_hashes_and_unrelated_changes_are_refused() {
    let fixture = Fixture::new(true);
    let refs = fixture.refs();
    let mut forged = refs.clone();
    let change = forged
        .changes
        .iter_mut()
        .find(|change| change.target == Target::Settings)
        .unwrap();
    let mut settings: Value =
        serde_json::from_slice(&decoded(&change.after, SETTINGS_LIMIT).unwrap().unwrap()).unwrap();
    settings["memory_gib"] = json!(18);
    let bytes = serde_json::to_vec_pretty(&settings).unwrap();
    change.after_hash = Some(hash(&bytes));
    change.after = Some(STANDARD.encode(bytes));
    assert!(forged.revision().is_err());
    assert!(forged.apply(&fixture.project, &fixture.root).is_err());
    assert_eq!(fixture.settings()["memory_gib"], 10);
    assert!(refs
        .verify_binding(&fixture.project, &fixture.root, "Other", "New")
        .is_err());
    let mut value = serde_json::to_value(&refs).unwrap();
    value["changes"][0]["target"] = json!({"type":"arbitrary","path":"accounts.json"});
    assert!(serde_json::from_value::<RenameReferences>(value).is_err());
}

#[test]
fn changed_marker_is_preserved_and_never_cleared_by_another_operation() {
    let fixture = Fixture::new(true);
    let refs = fixture.refs();
    refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
    let original = fs::read(fixture.path(MARKER)).unwrap();
    assert!(refs
        .clear_pending(&fixture.project, &fixture.root, "n-123-456-2")
        .is_err());
    assert_eq!(fs::read(fixture.path(MARKER)).unwrap(), original);
    fs::write(fixture.path(MARKER), b"malformed marker").unwrap();
    assert!(pending_root(&fixture.project).is_err());
    assert!(ensure_project_ready(&fixture.project).is_err());
    assert!(refs
        .clear_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .is_err());
    assert_eq!(fs::read(fixture.path(MARKER)).unwrap(), b"malformed marker");
}

#[test]
fn exact_content_marker_replacement_is_not_adopted_or_cleared() {
    let fixture = Fixture::new(true);
    let refs = fixture.refs();
    refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .unwrap();
    let bytes = fs::read(fixture.path(MARKER)).unwrap();
    fs::rename(fixture.path(MARKER), fixture.path("preserved-marker.json")).unwrap();
    fs::write(fixture.path(MARKER), &bytes).unwrap();
    assert!(pending_root(&fixture.project).is_err());
    assert!(refs
        .mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .is_err());
    assert!(refs
        .clear_pending(&fixture.project, &fixture.root, "n-123-456-1")
        .is_err());
    assert_eq!(fs::read(fixture.path(MARKER)).unwrap(), bytes);
}

#[test]
fn stale_cache_refuses_writes_and_refresh_never_adopts_missing_file_defaults() {
    let fixture = Fixture::new(true);
    let (config, _) = ConfigStore::load(&fixture.project);
    let metadata = fixture.metadata();
    let mut external = fixture.settings();
    external["memory_gib"] = json!(18);
    fixture.save_settings(external.clone());
    assert!(config.ensure_rename_snapshot().is_err());
    let mut edited = config.snapshot();
    edited.memory_gib = 20;
    assert!(config.save(edited).is_err());
    assert_eq!(fixture.settings(), external);
    fs::remove_file(fixture.path("settings.json")).unwrap();
    assert!(config.refresh_after_rename().is_err());
    assert_eq!(config.snapshot().memory_gib, 10);
    fs::remove_file(fixture.path("instance-metadata.json")).unwrap();
    assert!(metadata.refresh_after_rename().is_err());
    assert_eq!(
        metadata.get("root-a", &fixture.root, "Old").description,
        "Fixture description"
    );
}

#[test]
fn settings_transport_revision_rejects_dtos_captured_before_rename_refresh() {
    for changed in [false, true] {
        let fixture = Fixture::new(true);
        if !changed {
            let mut settings = fixture.settings();
            settings["roots"][0]["selected"] = json!("Unrelated");
            settings["roots"][0]["overrides"]
                .as_object_mut()
                .unwrap()
                .remove("Old");
            fixture.save_settings(settings);
        }
        let initial_bytes = fs::read(fixture.path("settings.json")).unwrap();
        let (config, warning) = ConfigStore::load(&fixture.project);
        assert!(warning.is_none());
        let mut stale = config.snapshot();
        let old_revision = stale.revision.clone();
        let refs = fixture.refs();
        refs.mark_pending(&fixture.project, &fixture.root, "n-123-456-1")
            .unwrap();
        refs.apply(&fixture.project, &fixture.root).unwrap();
        refs.clear_pending(&fixture.project, &fixture.root, "n-123-456-1")
            .unwrap();
        config.refresh_after_rename().unwrap();
        let after = config.snapshot();
        assert_ne!(after.revision, old_revision);
        let saved = fs::read(fixture.path("settings.json")).unwrap();
        if changed {
            assert_eq!(after.selected.as_deref(), Some("New"));
            assert_eq!(after.overrides.get("New"), Some(&12));
            assert!(!after.overrides.contains_key("Old"));
        } else {
            assert_eq!(saved, initial_bytes);
        }
        stale.memory_gib = 18;
        assert!(config.save(stale).unwrap_err().contains("设置已更新"));
        assert_eq!(fs::read(fixture.path("settings.json")).unwrap(), saved);
        assert_eq!(config.snapshot(), after);
        let mut fresh = after.clone();
        fresh.memory_gib = 16;
        config.save(fresh).unwrap();
        assert_eq!(config.snapshot().memory_gib, 16);
        assert_ne!(config.snapshot().revision, after.revision);
        assert!(config.save(after).is_err());
        assert!(fixture.settings().get("revision").is_none());
    }
}

#[test]
fn pending_version_two_rename_preserves_golden_bytes_and_defers_backed_upgrade() {
    let fixture = Fixture::new(true);
    let mut old = fixture.settings();
    old["schema_version"] = json!(2);
    old["memory_gib"] = json!(14);
    for field in ["java", "java_paths", "isolation", "memory_mode"] {
        old.as_object_mut().unwrap().remove(field);
    }
    for root in old["roots"].as_array_mut().unwrap() {
        for field in [
            "memory_mode_overrides",
            "java_overrides",
            "isolation_overrides",
            "window_title_overrides",
            "argument_overrides",
        ] {
            root.as_object_mut().unwrap().remove(field);
        }
    }
    fixture.save_settings(old);
    let before = fs::read(fixture.path("settings.json")).unwrap();
    let refs = fixture.refs();
    let expected = r#"{
  "schema_version": 2,
  "active_root_id": "root-a",
  "player": "FixturePlayer",
  "memory_gib": 14,
  "roots": [
    {
      "id": "root-a",
      "name": "A",
      "path": @ROOT@,
      "selected": "New",
      "overrides": {
        "New": 12,
        "Unrelated": 8
      }
    },
    {
      "id": "root-b",
      "name": "B",
      "path": @OTHER@,
      "selected": "Old",
      "overrides": {
        "Old": 6
      }
    }
  ]
}
"#
    .replace("@ROOT@", &serde_json::to_string(&fixture.root).unwrap())
    .replace("@OTHER@", &serde_json::to_string(&fixture.other).unwrap())
    .into_bytes();
    let delta = refs
        .changes
        .iter()
        .find(|delta| delta.target == Target::Settings)
        .unwrap();
    assert_eq!(
        decoded(&delta.after, SETTINGS_LIMIT).unwrap().unwrap(),
        expected
    );
    refs.mark_pending(&fixture.project, &fixture.root, "n-abc-1-1")
        .unwrap();
    let (config, warning) = ConfigStore::load(&fixture.project);
    assert!(warning.is_none());
    assert_eq!(config.snapshot().java, pcl_core::java::JavaSelection::Auto);
    assert_eq!(config.snapshot().memory_gib, 14);
    assert_eq!(fs::read(fixture.path("settings.json")).unwrap(), before);
    assert!(config
        .save(config.snapshot())
        .unwrap_err()
        .contains(PENDING_ERROR));
    let backups = || {
        fs::read_dir(fixture.path("settings.json").parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("settings.v2-backup-")
            })
            .collect::<Vec<_>>()
    };
    assert!(backups().is_empty());
    refs.apply(&fixture.project, &fixture.root).unwrap();
    refs.clear_pending(&fixture.project, &fixture.root, "n-abc-1-1")
        .unwrap();
    config.refresh_after_rename().unwrap();
    assert_eq!(config.snapshot().selected.as_deref(), Some("New"));
    assert_eq!(fs::read(fixture.path("settings.json")).unwrap(), expected);
    assert!(backups().is_empty());
    let current = config.snapshot();
    let java = fixture
        .project
        .join("fixture-java/bin/java")
        .to_string_lossy()
        .into_owned();
    config
        .register_java(java.clone(), &current.revision, &current.root_id)
        .unwrap();
    assert_eq!(backups().len(), 1);
    assert_eq!(fs::read(&backups()[0]).unwrap(), expected);
    assert_eq!(fixture.settings()["schema_version"], 3);
    assert_eq!(config.snapshot().memory_gib, 14);
    assert_eq!(config.snapshot().java_paths, vec![java]);
}

#[test]
fn java_overrides_move_back_and_other_root_global_registry_stay_unchanged() {
    let fixture = Fixture::new(true);
    let global = fixture
        .project
        .join("java-global/bin/java")
        .to_string_lossy()
        .into_owned();
    let own = fixture
        .project
        .join("java-own/bin/java")
        .to_string_lossy()
        .into_owned();
    let other = fixture
        .project
        .join("java-other/bin/java")
        .to_string_lossy()
        .into_owned();
    let mut settings = fixture.settings();
    settings["roots"][0]["argument_overrides"] = json!({
        "Old":{"custom_info":"First info","jvm_args":"","game_args":null},
        "Unrelated":{"custom_info":null,"jvm_args":null,"game_args":"--demo"}
    });
    settings["roots"][1]["argument_overrides"] = json!({
        "Old":{"custom_info":null,"jvm_args":"-Dother=value","game_args":null}
    });
    settings["java"] = json!({"mode":"manual","path":global});
    settings["java_paths"] = json!([global, own, other]);
    settings["roots"][0]["java_overrides"] =
        json!({"Old":{"mode":"manual","path":own},"ExplicitAuto":{"mode":"auto"}});
    settings["roots"][1]["java_overrides"] = json!({"Old":{"mode":"manual","path":other}});
    fixture.save_settings(settings.clone());
    let refs = fixture.refs();
    refs.apply(&fixture.project, &fixture.root).unwrap();
    let after = fixture.settings();
    assert_eq!(after["java"], settings["java"]);
    assert_eq!(after["java_paths"], settings["java_paths"]);
    assert_eq!(after["roots"][1], settings["roots"][1]);
    assert_eq!(
        after["roots"][0]["java_overrides"]["New"],
        settings["roots"][0]["java_overrides"]["Old"]
    );
    assert_eq!(
        after["roots"][0]["java_overrides"]["ExplicitAuto"],
        json!({"mode":"auto"})
    );
    assert!(after["roots"][0]["java_overrides"].get("Old").is_none());
    assert!(after["roots"][0]["argument_overrides"].get("Old").is_none());
    assert_eq!(
        after["roots"][0]["argument_overrides"]["New"],
        settings["roots"][0]["argument_overrides"]["Old"]
    );
    assert_eq!(
        after["roots"][0]["argument_overrides"]["Unrelated"],
        settings["roots"][0]["argument_overrides"]["Unrelated"]
    );
    prepare(&fixture.project, "root-a", &fixture.root, "New", "Old")
        .unwrap()
        .apply(&fixture.project, &fixture.root)
        .unwrap();
    assert_eq!(fixture.settings(), settings);
    for (field, value) in [
        ("java_overrides", json!({"mode":"auto"})),
        ("argument_overrides", json!({})),
    ] {
        let mut collision = settings.clone();
        collision["roots"][0][field]["New"] = value;
        fixture.save_settings(collision.clone());
        assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_err());
        assert_eq!(fixture.settings(), collision);
    }
}

#[test]
fn any_registered_java_inside_renamed_folder_refuses_and_prefix_siblings_work() {
    let fixture = Fixture::new(true);
    let baseline = fixture.settings();
    let inside = fixture
        .root
        .join("versions/Old/runtime/bin/java")
        .to_string_lossy()
        .into_owned();
    fs::create_dir_all(Path::new(&inside).parent().unwrap()).unwrap();
    fs::write(&inside, b"fixture only, never executed").unwrap();
    for location in 0..4 {
        let mut value = baseline.clone();
        match location {
            0 => value["java_paths"] = json!([inside]),
            1 => value["java"] = json!({"mode":"manual","path":inside}),
            index => {
                value["roots"][index - 2]["java_overrides"] =
                    json!({"Old":{"mode":"manual","path":inside}})
            }
        }
        fixture.save_settings(value.clone());
        assert!(
            prepare(&fixture.project, "root-a", &fixture.root, "Old", "New")
                .unwrap_err()
                .contains("Java 位于待改名实例内")
        );
        assert_eq!(fixture.settings(), value);
    }
    let mut sibling = baseline;
    sibling["java_paths"] = json!([fixture.root.join("versions/Old-Sibling/runtime/bin/java")]);
    fixture.save_settings(sibling);
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_ok());
}

#[test]
fn outside_java_aliases_to_old_runtime_refuse_even_with_a_missing_executable() {
    let fixture = Fixture::new(true);
    let baseline = fixture.settings();
    let runtime = fixture.root.join("versions/Old/runtime");
    fs::create_dir_all(runtime.join("bin")).unwrap();
    fs::write(runtime.join("bin/java"), b"fixture only, never executed").unwrap();
    let alias = fixture.project.join("outside-java");
    std::os::unix::fs::symlink(&runtime, &alias).unwrap();
    let path = alias.join("bin/java").to_string_lossy().into_owned();
    for missing in [false, true] {
        if missing {
            fs::remove_file(runtime.join("bin/java")).unwrap();
        }
        for location in 0..4 {
            let mut value = baseline.clone();
            match location {
                0 => value["java_paths"] = json!([path]),
                1 => value["java"] = json!({"mode":"manual","path":path}),
                index => {
                    value["roots"][index - 2]["java_overrides"] =
                        json!({"Old":{"mode":"manual","path":path}})
                }
            }
            fixture.save_settings(value.clone());
            let result = prepare(&fixture.project, "root-a", &fixture.root, "Old", "New");
            if location == 0 && missing {
                assert!(result.is_ok());
            } else {
                assert!(result.unwrap_err().contains("Java 位于待改名实例内"));
            }
            assert_eq!(fixture.settings(), value);
        }
    }
    let dangling = fixture.project.join("dangling-java");
    std::os::unix::fs::symlink("game/versions/Old/runtime/bin/java", &dangling).unwrap();
    let mut value = baseline;
    value["java"] = json!({"mode":"manual","path":dangling});
    fixture.save_settings(value.clone());
    assert!(
        prepare(&fixture.project, "root-a", &fixture.root, "Old", "New")
            .unwrap_err()
            .contains("Java 位于待改名实例内")
    );
    assert_eq!(fixture.settings(), value);
    value["java"] = json!({"mode":"auto"});
    value["java_paths"] = json!([dangling]);
    fixture.save_settings(value.clone());
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_ok());
    assert_eq!(fixture.settings(), value);
}

#[test]
fn relocated_java_keeps_unavailable_registry_entry_and_allows_rename() {
    let fixture = Fixture::new(true);
    let original = fixture.root.join("versions/Old/runtime/bin/java");
    fs::create_dir_all(original.parent().unwrap()).unwrap();
    fs::write(&original, b"fixture only, never executed").unwrap();
    let mut settings = fixture.settings();
    settings["memory_gib"] = json!(14);
    settings["java"] = json!({"mode":"manual","path":original});
    settings["java_paths"] = json!([original]);
    fixture.save_settings(settings);
    let (config, warning) = ConfigStore::load(&fixture.project);
    assert!(warning.is_none());
    assert!(
        prepare(&fixture.project, "root-a", &fixture.root, "Old", "New")
            .unwrap_err()
            .contains("Java 位于待改名实例内")
    );
    let outside = fixture.project.join("relocated-java/bin/java");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::rename(&original, &outside).unwrap();
    let current = config.snapshot();
    let mut updated = config
        .register_java(
            outside.to_string_lossy().into_owned(),
            &current.revision,
            &current.root_id,
        )
        .unwrap();
    updated.java = pcl_core::java::JavaSelection::Manual {
        path: outside.to_string_lossy().into_owned(),
    };
    config.save(updated).unwrap();
    let refs = fixture.refs();
    refs.apply(&fixture.project, &fixture.root).unwrap();
    config.refresh_after_rename().unwrap();
    let final_settings = config.snapshot();
    assert_eq!(final_settings.memory_gib, 14);
    assert_eq!(final_settings.selected.as_deref(), Some("New"));
    assert_eq!(
        final_settings.java,
        pcl_core::java::JavaSelection::Manual {
            path: outside.to_string_lossy().into_owned()
        }
    );
    assert_eq!(
        final_settings.java_paths,
        vec![
            original.to_string_lossy().into_owned(),
            outside.to_string_lossy().into_owned()
        ]
    );
    assert!(!original.exists());
    assert_eq!(fs::read(outside).unwrap(), b"fixture only, never executed");
}

#[test]
fn java_alias_traversing_old_then_parent_refuses_even_when_destination_is_outside() {
    let fixture = Fixture::new(true);
    let shared = fixture.root.join("versions/Shared/bin");
    fs::create_dir_all(&shared).unwrap();
    fs::write(shared.join("java"), b"fixture only, never executed").unwrap();
    let alias = fixture.project.join("outside-traversing-java");
    let old = fixture.root.join("versions/Old");
    std::os::unix::fs::symlink(old.join("../Shared/bin/java"), &alias).unwrap();
    assert_eq!(fs::canonicalize(&alias).unwrap(), shared.join("java"));
    let mut settings = fixture.settings();
    settings["java"] = json!({"mode":"manual","path":alias});
    fixture.save_settings(settings.clone());
    assert!(
        prepare(&fixture.project, "root-a", &fixture.root, "Old", "New")
            .unwrap_err()
            .contains("Java 位于待改名实例内")
    );
    assert_eq!(fixture.settings(), settings);
    // Demonstrate the actual lookup dependency without running the game or Java.
    fs::rename(old, fixture.root.join("versions/New")).unwrap();
    assert_eq!(
        fs::canonicalize(alias).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn unsupported_or_pending_resource_journals_block_migration() {
    let fixture = Fixture::new(true);
    let undo = fixture.undo(true);
    let path = fixture
        .root
        .join(".pcl-linux/resource-operations")
        .join(&undo)
        .join("journal.json");
    let mut data: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    data["state"] = json!("prepared");
    let bytes = serde_json::to_vec(&data).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    data["state"] = json!("committed");
    data["version"] = json!(99);
    let bytes = serde_json::to_vec(&data).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn memory_modes_and_dormant_manual_values_migrate_with_root_scoped_rename() {
    let fixture = Fixture::new(true);
    let mut before = fixture.settings();
    before["memory_mode"] = json!("auto");
    before["roots"][0]["memory_mode_overrides"] = json!({"Old":"auto", "Unrelated":"manual"});
    before["roots"][1]["memory_mode_overrides"] = json!({"Old":"manual"});
    fixture.save_settings(before.clone());
    let refs = fixture.refs();
    refs.verify_before(&fixture.project, &fixture.root).unwrap();
    refs.apply(&fixture.project, &fixture.root).unwrap();
    refs.apply(&fixture.project, &fixture.root).unwrap();
    let after = fixture.settings();
    let mut expected = before;
    expected["roots"][0]["selected"] = json!("New");
    expected["roots"][0]["overrides"] = json!({"New":12,"Unrelated":8});
    expected["roots"][0]["memory_mode_overrides"] = json!({"New":"auto","Unrelated":"manual"});
    assert_eq!(after, expected);
    let (config, warning) = ConfigStore::load(&fixture.project);
    assert!(warning.is_none());
    let settings = config.snapshot();
    assert_eq!(
        config
            .registered("root-a")
            .unwrap()
            .resolved_memory_mode("New", settings.memory_mode),
        pcl_core::memory::MemoryMode::Auto
    );
    assert_eq!(settings.overrides["New"], 12);
    assert_eq!(
        config
            .registered("root-b")
            .unwrap()
            .resolved_memory_mode("Old", settings.memory_mode),
        pcl_core::memory::MemoryMode::Manual
    );
}

#[test]
fn mode_only_destination_blocks_rename_and_keeps_all_reference_bytes() {
    let fixture = Fixture::new(true);
    let mut settings = fixture.settings();
    settings["roots"][0]["memory_mode_overrides"] = json!({"New":"auto"});
    fixture.save_settings(settings.clone());
    let original = fs::read(fixture.path("settings.json")).unwrap();
    let error = prepare(&fixture.project, "root-a", &fixture.root, "Old", "New").unwrap_err();
    assert!(error.contains("目标名称"));
    assert_eq!(fs::read(fixture.path("settings.json")).unwrap(), original);
    assert_eq!(fixture.settings(), settings);
}
