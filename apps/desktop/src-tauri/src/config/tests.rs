use super::*;
use serde_json::json;

#[test]
fn downloaded_java_registration_merges_current_root_and_manual_choices() {
    let fixture = Fixture::new();
    let (store, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    let captured = store.snapshot();
    let second = store
        .register(fixture.root("second"), Some("Second".into()))
        .unwrap();
    store.select(&second.id).unwrap();
    let mut current = store.snapshot();
    current.memory_gib = 14;
    current.java = JavaSelection::Manual {
        path: fixture.0.join("manual/bin/java").display().to_string(),
    };
    current
        .java_overrides
        .insert("Example".into(), JavaSelection::Auto);
    store.save(current).unwrap();
    let before = store.snapshot();
    let path = fixture.0.join("managed/bin/java").display().to_string();
    let added = store.register_downloaded_java(path.clone()).unwrap();
    assert_ne!(added.root_id, captured.root_id);
    let mut expected = before.clone();
    expected.java_paths.push(path.clone());
    expected.revision = added.revision.clone();
    assert_eq!(added, expected);
    assert_eq!(
        store.register_downloaded_java(path).unwrap().java_paths,
        added.java_paths
    );
}

#[test]
fn downloaded_java_registration_limit_failure_preserves_config_and_runtime() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    for n in 0..MAX_JAVA_PATHS {
        store
            .register_downloaded_java(fixture.0.join(format!("java-{n}")).display().to_string())
            .unwrap();
    }
    let runtime = fixture.0.join("published-java");
    fs::write(&runtime, b"published runtime").unwrap();
    let before = fs::read(fixture.file()).unwrap();
    assert!(store
        .register_downloaded_java(runtime.display().to_string())
        .is_err());
    assert_eq!(fs::read(fixture.file()).unwrap(), before);
    assert_eq!(fs::read(runtime).unwrap(), b"published runtime");
}

fn projection(mut settings: Settings) -> Settings {
    settings.revision.clear();
    settings
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/java-management-2026-10-04/config-fixtures");
        let project = base.join(nonce());
        fs::create_dir_all(project.join("Minecraft/.minecraft")).unwrap();
        Self(fs::canonicalize(project).unwrap())
    }
    fn file(&self) -> PathBuf {
        self.0.join(".pcl-rust/settings.json")
    }
    fn root(&self, name: &str) -> String {
        let path = self.0.join(name);
        fs::create_dir_all(&path).unwrap();
        path.to_str().unwrap().into()
    }
    fn write(&self, bytes: &[u8]) {
        fs::create_dir_all(self.file().parent().unwrap()).unwrap();
        fs::write(self.file(), bytes).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn legacy_migration_preserves_values_exact_path_and_original_backup_once() {
    let fixture = Fixture::new();
    let root = fixture.root("legacy");
    let exact = format!("{root}/../legacy");
    let original = json!({
        "root":exact,"player":"Old_Player","memory_gib":14,"selected":"Same Instance",
        "overrides":{"Same Instance":12,"Other":8}
    })
    .to_string()
    .into_bytes();
    fixture.write(&original);
    let (store, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    let settings = store.snapshot();
    assert_eq!(settings.root, exact);
    assert_eq!(settings.player, "Old_Player");
    assert_eq!(settings.memory_gib, 14);
    assert_eq!(settings.selected.as_deref(), Some("Same Instance"));
    assert_eq!(
        settings.overrides,
        BTreeMap::from([("Same Instance".into(), 12), ("Other".into(), 8)])
    );
    let backups = || {
        fs::read_dir(fixture.file().parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("settings.v1-backup-")
            })
            .collect::<Vec<_>>()
    };
    let before = backups();
    assert_eq!(before.len(), 1);
    assert_eq!(fs::read(&before[0]).unwrap(), original);
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
    assert_eq!(persisted["schema_version"], 3);
    assert_eq!(persisted["roots"][0]["path"], exact);
    let (reloaded, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    assert_eq!(projection(reloaded.snapshot()), projection(settings));
    assert_eq!(backups(), before);
}

#[test]
fn same_instance_id_has_independent_root_selection_and_memory() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let first = store.snapshot().root_id;
    let second = store
        .register(fixture.root("second"), Some("Second".into()))
        .unwrap();
    let mut settings = store.snapshot();
    settings.selected = Some("Same".into());
    settings.overrides.insert("Same".into(), 14);
    store.save(settings).unwrap();
    let mut settings = store.select(&second.id).unwrap();
    settings.overrides.insert("Same".into(), 4);
    settings.selected = Some("Different".into());
    settings.memory_gib = 10;
    store.save(settings).unwrap();
    store.select(&first).unwrap();
    store.select_installed(&second.id, "Same").unwrap();
    assert_eq!(store.snapshot().root_id, first);
    assert_eq!(store.snapshot().overrides["Same"], 14);
    assert_eq!(store.snapshot().memory_gib, 10);
    assert_eq!(
        store.registered(&second.id).unwrap().selected.as_deref(),
        Some("Same")
    );
    assert_eq!(store.registered(&second.id).unwrap().overrides["Same"], 4);
    let (reloaded, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    assert_eq!(
        projection(reloaded.snapshot()),
        projection(store.snapshot())
    );
    assert_eq!(
        reloaded.registered(&second.id).unwrap(),
        store.registered(&second.id).unwrap()
    );
}

#[test]
fn stale_active_root_and_changed_path_saves_leave_settings_unchanged() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let stale = store.snapshot();
    let second = store.register(fixture.root("second"), None).unwrap();
    store.select(&second.id).unwrap();
    let before = fs::read(fixture.file()).unwrap();
    assert!(store.save(stale).unwrap_err().contains("设置已更新"));
    let mut changed = store.snapshot();
    changed.root = fixture.root("replacement");
    assert!(store.save(changed).unwrap_err().contains("目录管理"));
    let mut missing_id = store.snapshot();
    missing_id.root_id.clear();
    assert!(store.save(missing_id).is_err());
    assert_eq!(fs::read(fixture.file()).unwrap(), before);
    assert_eq!(store.snapshot().root_id, second.id);
}

#[test]
fn unavailable_root_stays_registered_and_can_be_selected_then_recover() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let path = fixture.root("removable");
    let root = store.register(path.clone(), None).unwrap();
    let moved = fixture.0.join("temporarily-absent");
    fs::rename(&path, &moved).unwrap();
    let selected = store.select(&root.id).unwrap();
    assert_eq!(selected.root, path);
    assert_eq!(store.registered(&root.id).unwrap().path, path);
    assert!(store.resolve(Some(&root.id)).is_err());
    let (settings, summaries) = store.view();
    assert_eq!(settings.root_id, root.id);
    let summary = summaries
        .iter()
        .find(|summary| summary.id == root.id)
        .unwrap();
    assert!(!summary.available);
    assert!(summary.error.is_some());
    let (reloaded, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    assert_eq!(projection(reloaded.snapshot()), projection(selected));
    fs::rename(moved, &path).unwrap();
    assert!(
        store
            .roots()
            .iter()
            .find(|summary| summary.id == root.id)
            .unwrap()
            .available
    );
    assert_eq!(store.resolve(None).unwrap().id, root.id);
}

#[cfg(unix)]
#[test]
fn canonical_alias_registration_reuses_stable_id() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let initial = store.snapshot();
    let alias = fixture.0.join("alias");
    std::os::unix::fs::symlink(&initial.root, &alias).unwrap();
    let duplicate = store
        .register(alias.to_str().unwrap().into(), Some("Alias".into()))
        .unwrap();
    assert_eq!(duplicate.id, initial.root_id);
    assert_eq!(duplicate.path, initial.root);
    assert_eq!(store.roots().len(), 1);
    assert_eq!(
        store.resolve(Some(&duplicate.id)).unwrap().path,
        fs::canonicalize(&initial.root).unwrap().to_str().unwrap()
    );
    assert!(store.register("relative".into(), None).is_err());
    let regular_file = fixture.0.join("file");
    fs::write(&regular_file, "fixture").unwrap();
    assert!(store
        .register(regular_file.to_str().unwrap().into(), None)
        .is_err());
}

#[test]
fn malformed_or_future_settings_block_all_writes_and_preserve_original() {
    for bytes in [
        b"{broken".as_slice(),
        br#"{"schema_version":3,"player":"Future","memory_gib":14,"roots":[]}"#.as_slice(),
        br#"{"schema_version":2,"active_root_id":"missing","player":"Player","memory_gib":6,"roots":[]}"#.as_slice(),
        br#"{"root":"/example","player":"Player","memory_gib":6,"selected":null,"version":5}"#.as_slice(),
    ] {
        let fixture = Fixture::new();
        fixture.write(bytes);
        let (store, warning) = ConfigStore::load(&fixture.0);
        assert!(warning.as_deref().unwrap().contains("禁止保存"));
        assert_eq!(store.warning(), warning);
        assert!(store.ensure_writable().is_err());
        assert!(store.save(store.snapshot()).is_err());
        assert!(store.select(&store.snapshot().root_id).is_err());
        assert!(store.update(&store.snapshot().root_id, Some("Changed".into()), None).is_err());
        assert!(store.register(fixture.root("other"), None).is_err());
        assert!(store.select_installed(&store.snapshot().root_id, "Example").is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
    }
}

#[cfg(unix)]
#[test]
fn dangling_settings_link_is_preserved_and_blocks_saves() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.file().parent().unwrap()).unwrap();
    let target = fixture.0.join("unavailable-settings.json");
    std::os::unix::fs::symlink(&target, fixture.file()).unwrap();
    let (store, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_some());
    assert!(store.ensure_writable().is_err());
    assert!(store.save(store.snapshot()).is_err());
    assert_eq!(fs::read_link(fixture.file()).unwrap(), target);
    assert!(!target.exists());
}

#[test]
fn removal_only_forgets_reference_and_reorder_survives_reload() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let first = store.snapshot().root_id;
    let path = fixture.root("second");
    let game_file = Path::new(&path).join("save-fixture");
    fs::write(&game_file, b"unchanged").unwrap();
    let second = store.register(path, None).unwrap();
    store
        .update(&second.id, Some("Renamed".into()), Some(0))
        .unwrap();
    let (reloaded, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    assert_eq!(reloaded.roots()[0].id, second.id);
    assert_eq!(reloaded.roots()[0].name, "Renamed");
    assert!(reloaded.update(&second.id, None, Some(2)).is_err());
    reloaded.select(&second.id).unwrap();
    assert_eq!(reloaded.remove(&second.id).unwrap().root_id, first);
    assert_eq!(fs::read(&game_file).unwrap(), b"unchanged");
    assert!(reloaded.registered(&second.id).is_err());
    assert!(reloaded.remove(&first).is_err());
    assert_eq!(reloaded.roots().len(), 1);
}

#[test]
fn failed_atomic_save_keeps_previous_memory_state_and_cleans_temporary_file() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let original = store.snapshot();
    store.save(original.clone()).unwrap();
    let original = store.snapshot();
    fs::remove_file(fixture.file()).unwrap();
    fs::create_dir(fixture.file()).unwrap();
    let mut changed = original.clone();
    changed.memory_gib = 14;
    assert!(store.save(changed).is_err());
    assert_eq!(store.snapshot(), original);
    assert!(fs::read_dir(fixture.file().parent().unwrap())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
}

#[test]
fn version_two_migration_keeps_settings_and_exact_backup_once() {
    let fixture = Fixture::new();
    let root = fixture.root("version-two");
    let original = serde_json::to_vec(&json!({"schema_version":2,"active_root_id":"root-v2",
        "player":"FixturePlayer","memory_gib":14,"roots":[{"id":"root-v2","name":"Fixture","path":root,
        "selected":"Installed","overrides":{"Installed":12}}]})).unwrap();
    fixture.write(&original);
    let accounts = fixture.file().parent().unwrap().join("accounts.json");
    fs::write(&accounts, b"fixture-private-accounts").unwrap();
    let (store, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    let settings = store.snapshot();
    assert_eq!(settings.memory_gib, 14);
    assert_eq!(settings.player, "FixturePlayer");
    assert_eq!(settings.selected.as_deref(), Some("Installed"));
    assert_eq!(settings.overrides["Installed"], 12);
    assert_eq!(settings.java, JavaSelection::Auto);
    assert!(settings.java_paths.is_empty());
    assert!(settings.java_overrides.is_empty());
    let backup_paths = || {
        fs::read_dir(fixture.file().parent().unwrap())
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
    let backups = backup_paths();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
    assert_eq!(saved["schema_version"], 3);
    assert_eq!(saved["java"], json!({"mode":"auto"}));
    assert_eq!(saved["java_paths"], json!([]));
    assert_eq!(saved["roots"][0]["java_overrides"], json!({}));
    assert!(ConfigStore::load(&fixture.0).1.is_none());
    assert_eq!(backup_paths(), backups);
    assert_eq!(fs::read(accounts).unwrap(), b"fixture-private-accounts");
}

#[test]
fn java_save_keeps_registry_and_other_roots_and_adds_manual_choices() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let first = store.snapshot().root_id;
    let second = store.register(fixture.root("second"), None).unwrap();
    let imported = fixture
        .0
        .join("imported/bin/java")
        .to_string_lossy()
        .into_owned();
    let global = fixture
        .0
        .join("global/bin/java")
        .to_string_lossy()
        .into_owned();
    let personal = fixture
        .0
        .join("personal/bin/java")
        .to_string_lossy()
        .into_owned();
    let initial = store.snapshot();
    let registered = store
        .register_java(imported.clone(), &initial.revision, &first)
        .unwrap();
    assert_eq!(registered.java_paths, vec![imported.clone()]);
    assert!(store
        .register_java(global.clone(), &initial.revision, &first)
        .is_err());
    assert!(store
        .register_java(global.clone(), &registered.revision, &second.id)
        .is_err());
    let other_before = store.registered(&second.id).unwrap();
    let mut edited = store.snapshot();
    edited.java_paths = vec!["malicious-client-registry".into()];
    edited.java = JavaSelection::Manual {
        path: global.clone(),
    };
    edited
        .java_overrides
        .insert("ExplicitAuto".into(), JavaSelection::Auto);
    edited.java_overrides.insert(
        "Personal".into(),
        JavaSelection::Manual {
            path: personal.clone(),
        },
    );
    store.save(edited).unwrap();
    let current = store.snapshot();
    assert_eq!(
        current.java_paths,
        vec![imported, global.clone(), personal.clone()]
    );
    assert_eq!(store.registered(&second.id).unwrap(), other_before);
    assert_eq!(
        store
            .roots()
            .iter()
            .find(|root| root.id == first)
            .unwrap()
            .java_overrides,
        current.java_overrides
    );
    let mut other = store.select(&second.id).unwrap();
    assert!(other.java_overrides.is_empty());
    assert_eq!(other.java, JavaSelection::Manual { path: global });
    other
        .java_overrides
        .insert("Personal".into(), JavaSelection::Auto);
    store.save(other).unwrap();
    store.select(&first).unwrap();
    assert_eq!(
        store.snapshot().java_overrides["Personal"],
        JavaSelection::Manual { path: personal }
    );
    let before = store.snapshot();
    let bytes = fs::read(fixture.file()).unwrap();
    let mut invalid = before.clone();
    invalid.java = JavaSelection::Manual {
        path: "../java".into(),
    };
    assert!(store.save(invalid).is_err());
    assert_eq!(store.snapshot(), before);
    assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
    let (reloaded, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    assert_eq!(projection(reloaded.snapshot()), projection(before));
}

#[test]
fn java_bounds_and_strict_format_reject_without_overwrites() {
    let fixture = Fixture::new();
    let valid = serde_json::to_value(defaults(&fixture.0)).unwrap();
    let mut cases = Vec::new();
    for path in [
        "relative/java",
        "/fixture/../java",
        "/fixture//java",
        "/fixture/java\n",
        "/fixture/java/",
    ] {
        let mut value = valid.clone();
        value["java"] = json!({"mode":"manual","path":path});
        cases.push(value);
    }
    let mut unknown = valid.clone();
    unknown["java"] = json!({"mode":"auto","path":"/ignored"});
    cases.push(unknown);
    let mut future = valid.clone();
    future["schema_version"] = json!(99);
    cases.push(future);
    let mut duplicate = valid.clone();
    duplicate["java_paths"] = json!(["/fixture/java", "/fixture/java"]);
    cases.push(duplicate);
    let mut overfull = valid.clone();
    overfull["java_paths"] = json!((0..65)
        .map(|id| format!("/fixture/{id}/java"))
        .collect::<Vec<_>>());
    cases.push(overfull);
    let mut overrides = valid.clone();
    overrides["roots"][0]["java_overrides"] = json!((0..4097)
        .map(|id| (format!("Instance{id}"), json!({"mode":"auto"})))
        .collect::<BTreeMap<_, _>>());
    cases.push(overrides);
    for value in cases {
        let bytes = serde_json::to_vec(&value).unwrap();
        fixture.write(&bytes);
        let (store, warning) = ConfigStore::load(&fixture.0);
        assert!(warning.is_some());
        assert!(store.save(store.snapshot()).is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
    }
    let mut full = valid;
    full["java_paths"] = json!((0..64)
        .map(|id| format!("/fixture/{id}/java"))
        .collect::<Vec<_>>());
    fixture.write(&serde_json::to_vec(&full).unwrap());
    let (store, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    let before = store.snapshot();
    assert!(store
        .register_java(
            "/fixture/extra/java".into(),
            &before.revision,
            &before.root_id
        )
        .is_err());
    assert_eq!(store.snapshot(), before);
}

#[test]
fn removal_prefers_a_usable_neighbour_and_retains_offline_entries() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let first = store.snapshot().root_id;
    let imported = store.register(fixture.root("imported"), None).unwrap();
    let offline = store.register(fixture.root("offline"), None).unwrap();
    let next = store.register(fixture.root("next"), None).unwrap();
    fs::remove_dir_all(&offline.path).unwrap();
    store.select(&imported.id).unwrap();
    assert_eq!(store.remove(&imported.id).unwrap().root_id, next.id);
    assert!(store.registered(&offline.id).is_ok());
    assert!(!Path::new(&offline.path).exists());
    // Removing a non-selected entry must not switch the active root.
    assert_eq!(store.remove(&offline.id).unwrap().root_id, next.id);
    // At the end of the list, wrap back to the remaining available root.
    assert_eq!(store.remove(&next.id).unwrap().root_id, first);
}

#[test]
fn removal_with_all_remaining_roots_offline_keeps_a_valid_registry_selection() {
    let fixture = Fixture::new();
    let (store, _) = ConfigStore::load(&fixture.0);
    let first = store.snapshot();
    let imported = store.register(fixture.root("imported"), None).unwrap();
    let offline = store.register(fixture.root("offline"), None).unwrap();
    store.select(&imported.id).unwrap();
    for path in [&first.root, &imported.path, &offline.path] {
        fs::remove_dir_all(path).unwrap();
    }
    assert_eq!(store.remove(&imported.id).unwrap().root_id, offline.id);
    let (reloaded, warning) = ConfigStore::load(&fixture.0);
    assert!(warning.is_none());
    assert_eq!(reloaded.snapshot().root_id, offline.id);
    assert!(reloaded.roots().iter().all(|root| !root.available));
    assert!(!Path::new(&first.root).exists());
    assert!(!Path::new(&offline.path).exists());
}
