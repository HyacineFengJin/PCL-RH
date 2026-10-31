//! Only isolated stores and bounded Node stdio fixtures run here. Clipboard
//! callbacks are injected; no Tauri window, GTK display, Agent or user app runs.
use super::*;
use crate::experimental::Operation;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc, Arc, MutexGuard},
    time::{Duration, Instant},
};

struct Fixture {
    backing: crate::integration_tests::Fixture,
    _temporary: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("experimental")).unwrap();
        fs::create_dir_all(root.join("Minecraft/.minecraft")).unwrap();
        let fixture = Self {
            backing: crate::integration_tests::Fixture(root.canonicalize().unwrap()),
            _temporary: temporary,
        };
        fs::write(
            fixture.root().join("experimental/host.mjs"),
            r#"
import fs from 'node:fs/promises';
import readline from 'node:readline';
import path from 'node:path';
const root = path.resolve(process.argv[2], '../..');
let revoked = false;
for await (const line of readline.createInterface({input: process.stdin})) {
  const {operation} = JSON.parse(line);
  try {
    let result = true;
    if (operation === 'extensions_action') {
      if (revoked) throw new Error('Permission revoked');
      result = JSON.parse(await fs.readFile(path.join(root, 'intent.json'), 'utf8'));
    }
    if (operation === 'extensions_revoke') revoked = true;
    process.stdout.write(JSON.stringify({ok: true, result}) + '\n');
  } catch (error) {
    process.stdout.write(JSON.stringify({ok: false, error: error.message}) + '\n');
  }
  if (operation === 'shutdown') break;
}
"#,
        )
        .unwrap();
        fixture
    }
    fn root(&self) -> &Path {
        &self.backing.0
    }
    fn shared(&self) -> Arc<Shared> {
        Arc::new(self.backing.shared())
    }
    fn host(&self) -> Arc<Host> {
        Arc::new(Host::new(self.root().to_owned()))
    }
    fn intent(&self, value: Value) {
        fs::write(
            self.root().join("intent.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    fn prefs_file(&self) -> PathBuf {
        self.root().join(".pcl-rust/launcher-preferences.json")
    }
    fn use_real_light_host(&self) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../experimental");
        fs::copy(
            source.join("host.mjs"),
            self.root().join("experimental/host.mjs"),
        )
        .unwrap();
        copy_tree(
            &source.join("extensions/src"),
            &self.root().join("experimental/extensions/src"),
        );
        assert!(!self.root().join("experimental/agent-host.mjs").exists());
    }
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            assert!(entry.file_type().unwrap().is_file());
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}
fn args() -> Value {
    json!({"extensionId":"example.fixture","cardId":"card","actionId":"apply",
        "expected":{"digest":"fixture","generation":1}})
}
fn appearance(values: Value) -> Value {
    json!({"kind":"set-launcher-appearance","extensionId":"example.fixture","values":values})
}
fn downloads(values: Value) -> Value {
    json!({"kind":"set-launcher-downloads","extensionId":"example.fixture","values":values})
}

/// Serialize scheduler replacements with the existing policy readers as well
/// as other effects, and restore the exact original Arc even during unwinding.
struct DownloadIsolation {
    _lock: MutexGuard<'static, ()>,
    original: Arc<pcl_network::DownloadScheduler>,
}
impl DownloadIsolation {
    fn new() -> Self {
        let lock = crate::launcher_commands::test_download_scheduler_guard();
        let original = pcl_network::install_download_snapshot(
            pcl_network::DownloadScheduler::new(pcl_network::DownloadPolicy::default()).unwrap(),
        );
        Self {
            _lock: lock,
            original,
        }
    }
}
impl Drop for DownloadIsolation {
    fn drop(&mut self) {
        pcl_network::install_download_snapshot(self.original.clone());
    }
}
fn copy(text: &str) -> Value {
    json!({"kind":"copy-text","extensionId":"example.fixture","text":text})
}
fn no_copy(_: String) -> Result<(), String> {
    panic!("this request must never reach the clipboard boundary")
}
fn wait_until(test: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !test() {
        assert!(
            Instant::now() < deadline,
            "native fixture did not reach its boundary"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn settings_rpc_is_additive_and_effects_are_not_renderer_operations() {
    let operation: Operation = serde_json::from_value(json!("extensions_settings")).unwrap();
    assert_eq!(
        serde_json::to_value(operation).unwrap(),
        "extensions_settings"
    );
    for operation in [
        "copy-text",
        "set-launcher-appearance",
        "set-launcher-downloads",
        "launcher_preferences_update",
    ] {
        assert!(serde_json::from_value::<Operation>(json!(operation)).is_err());
    }
}

#[test]
fn community_catalog_filtered_cards_and_saved_layout_survive_plugin_retirement() {
    let fixture = Fixture::new();
    fixture.use_real_light_host();
    let host = fixture.host();
    let operation: Operation = serde_json::from_value(json!("extensions_catalog")).unwrap();
    assert_eq!(
        serde_json::to_value(operation).unwrap(),
        "extensions_catalog"
    );
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../experimental/extensions/examples/workspace-shortcuts.json"),
    )
    .unwrap();
    let review = host
        .call("extensions_review", json!({"source":source}))
        .unwrap();
    let entry = host
        .call(
            "extensions_confirm",
            json!({"token":review["token"],
        "grants":["ui.cards","launcher.navigate","instances.summary"]}),
        )
        .unwrap();
    let catalog = host.call("extensions_catalog", json!({})).unwrap();
    let panel = &catalog["panels"][0];
    let view = panel["viewId"].as_str().unwrap().to_owned();
    assert!(view.starts_with("rh.extensions.panel.") && view.len() <= 120);
    let cards = host
        .call(
            "extensions_cards",
            json!({"slot":"rh-code.cards",
        "filter":{"extensionId":panel["extensionId"],"cardId":panel["cardId"]}}),
        )
        .unwrap();
    assert_eq!(cards.as_array().unwrap().len(), 1);
    assert_eq!(cards[0]["id"], panel["cardId"]);
    assert!(!fixture.root().join("experimental/agent-host.mjs").exists());

    let folder = fixture.root().join("source-project");
    fs::create_dir(&folder).unwrap();
    let store = crate::rh_code::Store::new(fixture.root());
    let bootstrap = store.call("bootstrap", json!({})).unwrap();
    let created = store
        .call(
            "project_create",
            json!({"name":"Panel layout",
        "folder":folder,"expectedRevision":bootstrap["revision"]}),
        )
        .unwrap();
    let mut state = serde_json::Map::new();
    state.insert(view.clone(), json!({"collapsed":false}));
    let layout = json!({"type":"area","id":"community-area","view":view,"state":state});
    store
        .call(
            "layout_save",
            json!({"projectId":created["projects"][0]["id"],
        "expectedRevision":created["revision"],"layout":layout}),
        )
        .unwrap();
    let disabled = host
        .call(
            "extensions_disable",
            json!({"id":entry["id"],
        "expected":{"digest":entry["digest"],"generation":entry["generation"]}}),
        )
        .unwrap();
    assert_eq!(
        host.call("extensions_catalog", json!({})).unwrap()["panels"],
        json!([])
    );
    assert_eq!(
        crate::rh_code::Store::new(fixture.root())
            .call("bootstrap", json!({}))
            .unwrap()["projects"][0]["layout"],
        layout
    );
    host.call(
        "extensions_enable",
        json!({"id":entry["id"],
        "expected":{"digest":disabled["digest"],"generation":disabled["generation"]}}),
    )
    .unwrap();
    assert_eq!(
        host.call("extensions_catalog", json!({})).unwrap()["panels"][0]["viewId"],
        view
    );
    let reopened_entry = host.call("extensions_list", json!({})).unwrap()["entries"][0].clone();
    host.call(
        "extensions_remove",
        json!({"id":entry["id"],
        "expected":{"digest":reopened_entry["digest"],"generation":reopened_entry["generation"]}}),
    )
    .unwrap();
    assert_eq!(
        crate::rh_code::Store::new(fixture.root())
            .call("bootstrap", json!({}))
            .unwrap()["projects"][0]["layout"],
        layout
    );
}

#[test]
fn appearance_revalidates_only_six_concrete_values() {
    for invalid in [
        json!({}),
        json!([]),
        json!({"theme":"dark","opacity_percent":50}),
        json!({"theme":null}),
        json!({"theme":{"setting":"theme"}}),
        json!({"theme":"Dark"}),
        json!({"custom_cursor":"true"}),
        json!({"custom_cursor":1}),
        json!({"cursor_style":"script"}),
        json!({"cursor_size":"large"}),
        json!({"custom_context_menu":null}),
        json!({"context_menu_density":"tiny"}),
        json!({"network":{"proxy_mode":"none"}}),
    ] {
        assert!(
            appearance_patch(invalid.clone()).is_err(),
            "accepted {invalid}"
        );
    }
    for valid in [
        json!({"theme":"system"}),
        json!({"theme":"light"}),
        json!({"theme":"dark","custom_cursor":false,"cursor_style":"outline",
            "cursor_size":"standard","custom_context_menu":false,"context_menu_density":"compact"}),
    ] {
        assert!(appearance_patch(valid).is_ok());
    }
}

#[test]
fn downloads_revalidate_only_two_bounded_concrete_integers() {
    for invalid in [
        json!(null),
        json!({}),
        json!([]),
        json!({"max_concurrent_transfers":null}),
        json!({"max_concurrent_transfers":{"setting":"threads"}}),
        json!({"max_concurrent_transfers":"4"}),
        json!({"max_concurrent_transfers":true}),
        json!({"max_concurrent_transfers":4.0}),
        json!({"max_concurrent_transfers":4.5}),
        json!({"max_concurrent_transfers":-1}),
        json!({"max_concurrent_transfers":0}),
        json!({"max_concurrent_transfers":65}),
        json!({"max_concurrent_transfers":u64::MAX}),
        json!({"total_rate_limit_mib_per_second":null}),
        json!({"total_rate_limit_mib_per_second":"0"}),
        json!({"total_rate_limit_mib_per_second":0.0}),
        json!({"total_rate_limit_mib_per_second":-1}),
        json!({"total_rate_limit_mib_per_second":1025}),
        json!({"total_rate_limit_mib_per_second":u64::MAX}),
        json!({"max_concurrent_transfers":4,"quick_download":"auto"}),
        json!({"forbid_download_copy":true}),
        json!({"management":{"max_concurrent_transfers":4}}),
        json!({"max_concurrent_transfers":4,"total_rate_limit_mib_per_second":0,"theme":"dark"}),
    ] {
        assert!(
            downloads_patch(invalid.clone()).is_err(),
            "accepted {invalid}"
        );
    }
    for valid in [
        json!({"max_concurrent_transfers":1}),
        json!({"max_concurrent_transfers":64}),
        json!({"total_rate_limit_mib_per_second":0}),
        json!({"total_rate_limit_mib_per_second":1024}),
        json!({"max_concurrent_transfers":12,"total_rate_limit_mib_per_second":7}),
    ] {
        assert!(downloads_patch(valid).is_ok());
    }
}

#[test]
fn forged_intents_never_write_preferences_or_call_clipboard() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    for intent in [
        json!({"kind":"set-launcher-appearance","extensionId":"example.other","values":{"theme":"dark"}}),
        json!({"kind":"set-launcher-appearance","extensionId":"example.fixture","values":{"theme":"dark"},"ipc":"anything"}),
        json!({"kind":"set-launcher-downloads","extensionId":"example.other","values":{"max_concurrent_transfers":12}}),
        json!({"kind":"set-launcher-downloads","extensionId":"example.fixture","values":{"max_concurrent_transfers":12},"ipc":"anything"}),
        downloads(json!({"max_concurrent_transfers":null})),
        downloads(json!({"max_concurrent_transfers":12,"quick_download":"auto"})),
        json!({"kind":"copy-text","extensionId":"example.other","text":"private"}),
        json!({"kind":"copy-text","extensionId":"example.fixture","text":"private","read":true}),
        json!({"kind":"launcher-appearance-applied","extensionId":"example.fixture"}),
        json!({"kind":"launcher-downloads-applied","extensionId":"example.fixture"}),
        json!({"kind":"text-copied","extensionId":"example.fixture"}),
        json!({"kind":"arbitrary-ipc","command":"launch"}),
    ] {
        fixture.intent(intent.clone());
        assert!(
            execute(&host, &shared, args(), no_copy).is_err(),
            "accepted {intent}"
        );
    }
    for id in [
        "example",
        "example.fixture\n",
        "example.Fixture",
        "a..b",
        "a.b/c",
    ] {
        let mut request = args();
        request["extensionId"] = json!(id);
        assert!(execute(&host, &shared, request, no_copy).is_err());
    }
    assert!(!fixture.prefs_file().exists());
    assert_eq!(host.call("extensions_list", json!({})).unwrap(), true);
}

#[test]
fn appearance_commits_real_preferences_preserves_unrelated_values_and_returns_completion_only() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    let initial = shared.launcher_preferences.snapshot();
    let before = shared.launcher_preferences.update(&initial.revision, serde_json::from_value(json!({
        "game_launch":{"custom_info":"fixture-default"},
        "appearance":{"opacity_percent":73,"lock_window_size":true,"global_font":"Fixture Font"},
        "management":{"max_concurrent_transfers":7},"local_diagnostics_enabled":true
    })).unwrap()).unwrap().preferences;
    fixture.intent(appearance(
        json!({"theme":"dark","custom_cursor":false,"cursor_style":"outline",
        "cursor_size":"standard","custom_context_menu":false,"context_menu_density":"compact"}),
    ));
    assert_eq!(
        execute(&host, &shared, args(), no_copy).unwrap(),
        json!({"kind":"launcher-appearance-applied","extensionId":"example.fixture"})
    );
    let mut expected = before;
    expected.appearance.theme = Theme::Dark;
    expected.appearance.custom_cursor = false;
    expected.appearance.cursor_style = CursorStyle::Outline;
    expected.appearance.cursor_size = CursorSize::Standard;
    expected.appearance.custom_context_menu = false;
    expected.appearance.context_menu_density = ContextMenuDensity::Compact;
    let fresh = LauncherPreferencesStore::load(fixture.root()).snapshot();
    assert!(fresh.warning.is_none());
    assert_eq!(fresh.preferences, expected);
    // Disable/removal/revoke never restores the previous launcher appearance.
    host.call("extensions_revoke", json!({})).unwrap();
    assert_eq!(shared.launcher_preferences.snapshot().preferences, expected);
}

#[test]
fn downloads_commit_preferences_and_scheduler_under_the_same_native_and_host_ownership() {
    let _isolation = DownloadIsolation::new();
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    let initial = shared.launcher_preferences.snapshot();
    let before = shared.launcher_preferences.update(&initial.revision, serde_json::from_value(json!({
        "game_launch":{"custom_info":"download-fixture"},
        "appearance":{"opacity_percent":73,"theme":"dark"},
        "management":{"clipboard_resource_detection":false,"minecraft_release_notifications":false},
        "advanced":{"forbid_download_copy":true},"local_diagnostics_enabled":true
    })).unwrap()).unwrap().preferences;
    let old_scheduler = pcl_network::download_snapshot();
    let checked_scheduler = old_scheduler.clone();
    let checked_shared = shared.clone();
    let checked_host = host.clone();
    shared.launcher_preferences.test_before_replace(move |_| {
        assert!(checked_shared.operations.try_lock().is_err());
        assert!(checked_host.session.try_lock().is_err());
        assert!(checked_host.suspend().unwrap().busy);
        assert!(Arc::ptr_eq(
            &pcl_network::download_snapshot(),
            &checked_scheduler
        ));
        Ok(())
    });
    fixture.intent(downloads(
        json!({"max_concurrent_transfers":12,"total_rate_limit_mib_per_second":7}),
    ));
    assert_eq!(
        execute(&host, &shared, args(), no_copy).unwrap(),
        json!({"kind":"launcher-downloads-applied","extensionId":"example.fixture"})
    );
    let mut expected = before;
    expected.management.max_concurrent_transfers = 12;
    expected.management.total_rate_limit_mib_per_second = 7;
    let fresh = LauncherPreferencesStore::load(fixture.root()).snapshot();
    assert!(fresh.warning.is_none());
    assert_eq!(fresh.preferences, expected);
    let applied = pcl_network::download_snapshot();
    assert!(!Arc::ptr_eq(&old_scheduler, &applied));
    assert_eq!(
        applied.policy(),
        &crate::launcher_commands::download_policy(&expected)
    );
    assert!(applied.policy().forbid_cross_root_cache_copy);
    // A partial action preserves the other download budget and all unrelated
    // management values; the same saved policy retains its shared scheduler.
    fixture.intent(downloads(json!({"max_concurrent_transfers":12})));
    execute(&host, &shared, args(), no_copy).unwrap();
    assert!(Arc::ptr_eq(&applied, &pcl_network::download_snapshot()));
    assert_eq!(shared.launcher_preferences.snapshot().preferences, expected);
    host.call("extensions_revoke", json!({})).unwrap();
    assert!(execute(&host, &shared, args(), no_copy).is_err());
    assert_eq!(shared.launcher_preferences.snapshot().preferences, expected);
    assert!(Arc::ptr_eq(&applied, &pcl_network::download_snapshot()));
}

#[test]
fn downloads_failed_publication_or_external_cas_never_install_the_prepared_scheduler() {
    let _isolation = DownloadIsolation::new();
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    let initial = shared.launcher_preferences.snapshot();
    let initial = shared
        .launcher_preferences
        .update(
            &initial.revision,
            serde_json::from_value(json!({"appearance":{"opacity_percent":85}})).unwrap(),
        )
        .unwrap();
    let bytes = fs::read(fixture.prefs_file()).unwrap();
    let original = pcl_network::download_snapshot();
    fixture.intent(downloads(
        json!({"max_concurrent_transfers":64,"total_rate_limit_mib_per_second":9}),
    ));
    shared
        .launcher_preferences
        .test_before_replace(|_| Err("fixture download publication failure".into()));
    let error = execute(&host, &shared, args(), no_copy).unwrap_err();
    assert!(error.contains("fixture download publication failure"));
    assert_eq!(fs::read(fixture.prefs_file()).unwrap(), bytes);
    let after = shared.launcher_preferences.snapshot();
    assert_eq!(after.revision, initial.revision);
    assert_eq!(after.preferences, initial.preferences);
    assert!(Arc::ptr_eq(&original, &pcl_network::download_snapshot()));
    // A retry can use the same host session and commits both native consumers.
    assert!(execute(&host, &shared, args(), no_copy).is_ok());
    let committed = pcl_network::download_snapshot();
    assert!(!Arc::ptr_eq(&original, &committed));
    fixture.intent(downloads(json!({"total_rate_limit_mib_per_second":11})));
    let external = b"{\"schema_version\":999}";
    shared
        .launcher_preferences
        .test_before_replace(move |project| {
            fs::write(
                project.join(".pcl-rust/launcher-preferences.json"),
                external,
            )
            .unwrap();
            Ok(())
        });
    assert!(execute(&host, &shared, args(), no_copy).is_err());
    assert_eq!(fs::read(fixture.prefs_file()).unwrap(), external);
    assert!(Arc::ptr_eq(&committed, &pcl_network::download_snapshot()));
    assert!(shared.launcher_preferences.snapshot().warning.is_some());
    assert!(execute(&host, &shared, args(), no_copy).is_err());
    assert!(Arc::ptr_eq(&committed, &pcl_network::download_snapshot()));
    assert_eq!(host.call("extensions_list", json!({})).unwrap(), true);
}

#[test]
fn downloads_reuse_idle_guards_for_queued_work_games_monitor_and_close() {
    let _isolation = DownloadIsolation::new();
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    fixture.intent(downloads(json!({"max_concurrent_transfers":64})));
    let original = pcl_network::download_snapshot();
    let before = shared.launcher_preferences.snapshot();
    let assert_unchanged = || {
        assert!(!fixture.prefs_file().exists());
        let after = shared.launcher_preferences.snapshot();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.preferences, before.preferences);
        assert!(Arc::ptr_eq(&original, &pcl_network::download_snapshot()));
    };
    let root = shared.config.resolve(None).unwrap();
    let task = shared
        .tasks
        .admit_queued(
            crate::tasks::TaskTarget {
                root_id: root.id,
                root_path: root.path.clone(),
                instance_id: None,
            },
            crate::tasks::TaskKind::Install,
            crate::tasks::TaskScope::root(Path::new(&root.path)).unwrap(),
        )
        .unwrap();
    assert!(execute(&host, &shared, args(), no_copy).is_err());
    assert_unchanged();
    drop(task);
    for stage in ["preparing", "running"] {
        shared.status.lock().unwrap().stage = stage.into();
        assert!(execute(&host, &shared, args(), no_copy).is_err());
        assert_unchanged();
    }
    shared.status.lock().unwrap().stage = "idle".into();
    crate::launcher_monitor_runtime::test_attach(&shared);
    assert!(execute(&host, &shared, args(), no_copy).is_err());
    assert_unchanged();
    shared.closing.store(true, Ordering::SeqCst);
    assert!(execute(&host, &shared, args(), no_copy).is_err());
    assert_unchanged();
}

#[test]
fn failed_preference_publication_preserves_bytes_generation_and_host_session() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    let initial = shared.launcher_preferences.snapshot();
    let initial = shared
        .launcher_preferences
        .update(
            &initial.revision,
            serde_json::from_value(json!({"appearance":{"opacity_percent":85}})).unwrap(),
        )
        .unwrap();
    let bytes = fs::read(fixture.prefs_file()).unwrap();
    fixture.intent(appearance(json!({"theme":"dark"})));
    shared
        .launcher_preferences
        .test_before_replace(|_| Err("fixture publication failure".into()));
    let error = execute(&host, &shared, args(), no_copy).unwrap_err();
    assert!(error.contains("fixture publication failure"));
    assert_eq!(fs::read(fixture.prefs_file()).unwrap(), bytes);
    let after = shared.launcher_preferences.snapshot();
    assert_eq!(after.revision, initial.revision);
    assert_eq!(after.preferences, initial.preferences);
    assert!(execute(&host, &shared, args(), no_copy).is_ok());
    // An external bad edit must stay intact and must not be treated as defaults.
    fs::write(fixture.prefs_file(), b"{\"schema_version\":999}").unwrap();
    assert!(execute(&host, &shared, args(), no_copy).is_err());
    assert_eq!(
        fs::read(fixture.prefs_file()).unwrap(),
        b"{\"schema_version\":999}"
    );
}

#[test]
fn clipboard_accepts_empty_and_unicode_bounded_saved_text_and_hides_it_from_renderer() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    for text in [
        String::new(),
        "saved note\n第二行".into(),
        "🦀".repeat(4000),
    ] {
        fixture.intent(copy(&text));
        assert_eq!(
            execute(&host, &shared, args(), |received| {
                assert!(shared.operations.try_lock().is_ok());
                assert_eq!(received, text);
                Ok(())
            })
            .unwrap(),
            json!({"kind":"text-copied","extensionId":"example.fixture"})
        );
    }
    for text in [
        "🦀".repeat(4001),
        "interior\0nul".into(),
        "control\u{001b}".into(),
        "bidi\u{202e}".into(),
        "isolate\u{2066}".into(),
    ] {
        fixture.intent(copy(&text));
        assert!(execute(&host, &shared, args(), no_copy).is_err());
    }
    fixture.intent(copy("saved"));
    assert_eq!(
        execute(&host, &shared, args(), |_| Err(
            "fixture clipboard unavailable".into()
        )),
        Err("fixture clipboard unavailable".into())
    );
    assert_eq!(host.call("extensions_list", json!({})).unwrap(), true);
    assert!(!fixture.prefs_file().exists());
}

#[test]
fn permission_decision_and_effect_share_session_and_queued_actions_do_not_block_gtk_admission() {
    let fixture = Fixture::new();
    fixture.intent(copy("saved"));
    let shared = fixture.shared();
    let host = fixture.host();
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let worker_host = host.clone();
    let worker_shared = shared.clone();
    let worker = std::thread::spawn(move || {
        execute(&worker_host, &worker_shared, args(), |_| {
            entered.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    });
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(host.suspend().unwrap().busy);
    assert!(!host.is_stopped());
    let revoke_host = host.clone();
    let (revoked, revoke_done) = mpsc::sync_channel(1);
    let revoke = std::thread::spawn(move || {
        let result = revoke_host.call("extensions_revoke", json!({}));
        revoked.send(()).unwrap();
        result
    });
    let waiting_host = host.clone();
    let waiting_shared = shared.clone();
    let waiting =
        std::thread::spawn(move || execute(&waiting_host, &waiting_shared, args(), |_| Ok(())));
    wait_until(|| host.callers.load(Ordering::SeqCst) == 3);
    assert!(matches!(
        revoke_done.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let operations = shared.operations.try_lock();
    assert!(
        operations.is_ok(),
        "a queued action must release operations before waiting for the clipboard owner"
    );
    drop(operations);
    release.send(()).unwrap();
    assert_eq!(worker.join().unwrap().unwrap()["kind"], "text-copied");
    revoke.join().unwrap().unwrap();
    let _ = waiting.join().unwrap();
    assert!(execute(&host, &shared, args(), no_copy).is_err());
}

#[test]
fn close_can_take_operations_while_copy_is_pending_and_drains_native_ownership() {
    let fixture = Fixture::new();
    fixture.intent(copy("saved"));
    let shared = fixture.shared();
    let host = fixture.host();
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let worker_host = host.clone();
    let worker_shared = shared.clone();
    let worker = std::thread::spawn(move || {
        execute(&worker_host, &worker_shared, args(), |_| {
            entered.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
            if worker_shared.closing.load(Ordering::SeqCst) {
                Err("closing before GTK".into())
            } else {
                Ok(())
            }
        })
    });
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    {
        let _operation = shared
            .operations
            .try_lock()
            .expect("GTK close must remain unblocked");
        shared.closing.store(true, Ordering::SeqCst);
    }
    host.shutdown();
    assert!(!host.is_stopped());
    release.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Err("closing before GTK".into()));
    wait_until(|| host.is_stopped());
    assert!(execute(&host, &shared, args(), no_copy).is_err());
}

#[test]
fn legacy_action_intents_keep_their_existing_shape() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    for value in [
        json!({"kind":"navigate","target":"settings","extensionId":"example.fixture"}),
        json!({"kind":"show-instance-summary","summary":{"modCount":2},"extensionId":"example.fixture"}),
    ] {
        fixture.intent(value.clone());
        assert_eq!(execute(&host, &shared, args(), no_copy).unwrap(), value);
    }
}

#[test]
fn real_imported_appearance_notes_and_checklist_examples_cross_native_boundary_without_agent() {
    let fixture = Fixture::new();
    fixture.use_real_light_host();
    let shared = fixture.shared();
    let host = fixture.host();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../experimental/extensions/examples");
    for file in [
        "appearance-switcher.json",
        "launch-checklist.json",
        "workspace-notes.json",
    ] {
        let manifest: Value =
            serde_json::from_slice(&fs::read(source.join(file)).unwrap()).unwrap();
        let review = host
            .call("extensions_review", json!({"source":manifest.to_string()}))
            .unwrap();
        let grants: Vec<_> = manifest["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|capability| capability["id"].clone())
            .collect();
        let mut entry = host
            .call(
                "extensions_confirm",
                json!({"token":review["token"],"grants":grants}),
            )
            .unwrap();
        let id = manifest["id"].clone();
        let mut values = serde_json::Map::new();
        for setting in manifest["settings"].as_array().unwrap() {
            let value = match setting["type"].as_str().unwrap() {
                "toggle" => json!(!setting["default"].as_bool().unwrap()),
                "text" => json!("Native fixture saved note"),
                "select" => setting["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|option| option["value"] != setting["default"])
                    .unwrap()["value"]
                    .clone(),
                "number" => {
                    if setting["default"] == setting["min"] {
                        setting["max"].clone()
                    } else {
                        setting["min"].clone()
                    }
                }
                other => panic!("unexpected sample setting type {other}"),
            };
            values.insert(setting["id"].as_str().unwrap().to_owned(), value);
        }
        entry = host
            .call(
                "extensions_settings",
                json!({"id":id,"expected":expected(&entry),"values":values}),
            )
            .unwrap();
        let cards = host
            .call(
                "extensions_cards",
                json!({"slot":manifest["contributions"]["cards"][0]["slot"]}),
            )
            .unwrap();
        for card in cards
            .as_array()
            .unwrap()
            .iter()
            .filter(|card| card["extensionId"] == id)
        {
            for field in card["fields"].as_array().unwrap() {
                assert_eq!(field["value"], values[field["id"].as_str().unwrap()]);
            }
        }
        let mut protected_action = None;
        for card in manifest["contributions"]["cards"].as_array().unwrap() {
            for action in card["actions"].as_array().unwrap() {
                let request = json!({"extensionId":id,"cardId":card["id"],"actionId":action["id"],"expected":expected(&entry)});
                let result = execute(&host, &shared, request, |text| {
                    assert_eq!(
                        text,
                        values[action["setting"].as_str().unwrap()]
                            .as_str()
                            .unwrap()
                    );
                    Ok(())
                })
                .unwrap();
                let kind = action["kind"].as_str().unwrap();
                assert_eq!(
                    result["kind"],
                    match kind {
                        "set-launcher-appearance" => "launcher-appearance-applied",
                        "copy-text" => "text-copied",
                        "navigate" => "navigate",
                        other => panic!("unexpected sample action {other}"),
                    }
                );
                assert!(result.get("text").is_none());
                assert!(result.get("values").is_none());
                if kind == "set-launcher-appearance" {
                    let applied = serde_json::to_value(
                        shared
                            .launcher_preferences
                            .snapshot()
                            .preferences
                            .appearance,
                    )
                    .unwrap();
                    for (key, value) in action["values"].as_object().unwrap() {
                        let resolved = value["setting"]
                            .as_str()
                            .map_or(value, |setting| &values[setting]);
                        assert_eq!(&applied[key], resolved);
                    }
                }
                let capability = match kind {
                    "set-launcher-appearance" => "launcher.appearance",
                    "copy-text" => "clipboard.write",
                    "navigate" => "launcher.navigate",
                    _ => unreachable!(),
                };
                protected_action = Some((capability, card["id"].clone(), action["id"].clone()));
            }
        }
        if let Some((capability, card, action)) = protected_action {
            entry = host
                .call(
                    "extensions_revoke",
                    json!({"id":id,"capability":capability,"expected":expected(&entry)}),
                )
                .unwrap();
            let refused = json!({"extensionId":id,"cardId":card,"actionId":action,"expected":expected(&entry)});
            assert!(execute(&host, &shared, refused, no_copy).is_err());
        }
    }
    let fresh = LauncherPreferencesStore::load(fixture.root()).snapshot();
    assert!(fresh.warning.is_none());
    assert_ne!(
        fresh.preferences.appearance,
        AppearancePreferences::default()
    );
    assert!(fixture.prefs_file().exists());
    assert!(!fixture.root().join("experimental/agent-host.mjs").exists());
}

#[test]
fn real_launcher_samples_save_project_widgets_and_apply_downloads_across_host_reopen_and_revoke() {
    let _isolation = DownloadIsolation::new();
    let fixture = Fixture::new();
    fixture.use_real_light_host();
    let shared = fixture.shared();
    let host = fixture.host();
    let original = pcl_network::download_snapshot();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../experimental/extensions/examples");
    let mut entries = serde_json::Map::new();
    let saved_downloads = json!({"connections":23,"limit":37});
    let saved_dashboard = json!({"done":4,"note":"已检查 Java\nNative sample saved note"});
    for (file, values) in [
        ("download-presets.json", saved_downloads.clone()),
        ("launcher-dashboard.json", saved_dashboard.clone()),
    ] {
        let manifest: Value =
            serde_json::from_slice(&fs::read(source.join(file)).unwrap()).unwrap();
        let review = host
            .call("extensions_review", json!({"source":manifest.to_string()}))
            .unwrap();
        let grants: Vec<_> = manifest["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|capability| capability["id"].clone())
            .collect();
        let entry = host
            .call(
                "extensions_confirm",
                json!({"token":review["token"],"grants":grants}),
            )
            .unwrap();
        let entry = host
            .call(
                "extensions_settings",
                json!({"id":manifest["id"],"expected":expected(&entry),"values":values}),
            )
            .unwrap();
        entries.insert(manifest["id"].as_str().unwrap().into(), entry);
    }
    let cards =
        |host: &Host, slot: &str| host.call("extensions_cards", json!({"slot":slot})).unwrap();
    let card = |cards: &Value, extension_id: &str| {
        cards
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["extensionId"] == extension_id)
            .unwrap()
            .clone()
    };
    let content = |mut cards: Value| {
        // Generations belong to one live host. Reopening restores saved content
        // while creating fresh action identities, which callers must reread.
        for card in cards.as_array_mut().unwrap() {
            card.as_object_mut().unwrap().remove("generation");
        }
        cards
    };
    let entry = |host: &Host| {
        host.call("extensions_list", json!({})).unwrap()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == "local.download-presets")
            .unwrap()
            .clone()
    };
    let quick = cards(&host, "home.quick-actions");
    assert_eq!(quick.as_array().unwrap().len(), 2);
    let download_quick = card(&quick, "local.download-presets");
    assert_eq!(download_quick["id"], "quick");
    assert_eq!(download_quick["presentation"], "compact");
    assert_eq!(download_quick["actions"][0]["enabled"], true);
    assert_eq!(
        download_quick["widgets"],
        json!([
            {"id":"connections","kind":"value","label":"预设并发","value":23},
            {"id":"limit","kind":"value","label":"预设限速","value":37,"suffix":"MiB/s"}
        ])
    );
    let manage = cards(&host, "settings.manage");
    assert_eq!(manage.as_array().unwrap().len(), 1);
    assert_eq!(manage[0]["id"], "downloads");
    assert_eq!(manage[0]["fields"][0]["value"], 23);
    assert_eq!(manage[0]["fields"][1]["value"], 37);
    assert_eq!(manage[0]["widgets"][0]["kind"], "notice");
    assert_eq!(manage[0]["widgets"][0]["tone"], "info");
    let checklist = cards(&host, "settings.launch");
    assert_eq!(checklist[0]["fields"][0]["value"], saved_dashboard["done"]);
    assert_eq!(checklist[0]["fields"][1]["value"], saved_dashboard["note"]);
    assert_eq!(
        checklist[0]["widgets"],
        json!([
            {"id":"done","kind":"progress","label":"已保存的检查进度","value":4,"max":5},
            {"id":"note","kind":"value","label":"已保存备注","value":saved_dashboard["note"]}
        ])
    );
    assert_eq!(
        cards(&host, "home.banner")[0]["widgets"][0]["kind"],
        "notice"
    );
    // Import, Save and card projection never apply launcher preferences. Only
    // an explicitly admitted action crosses the native publication boundary.
    assert!(!fixture.prefs_file().exists());
    assert!(Arc::ptr_eq(&original, &pcl_network::download_snapshot()));
    assert_eq!(
        execute(
            &host,
            &shared,
            json!({"extensionId":"local.launcher-dashboard",
            "cardId":"shortcuts","actionId":"manage",
            "expected":expected(&entries["local.launcher-dashboard"])}),
            no_copy
        )
        .unwrap(),
        json!({"kind":"navigate","target":"settings.manage","extensionId":"local.launcher-dashboard"})
    );
    let request = |entry: &Value, card_id: &str| {
        json!({"extensionId":"local.download-presets","cardId":card_id,
            "actionId":"apply","expected":expected(entry)})
    };
    assert_eq!(
        execute(
            &host,
            &shared,
            request(&entries["local.download-presets"], "downloads"),
            no_copy
        )
        .unwrap(),
        json!({"kind":"launcher-downloads-applied","extensionId":"local.download-presets"})
    );
    let applied = shared.launcher_preferences.snapshot();
    assert_eq!(applied.preferences.management.max_concurrent_transfers, 23);
    assert_eq!(
        applied
            .preferences
            .management
            .total_rate_limit_mib_per_second,
        37
    );
    let scheduler = pcl_network::download_snapshot();
    assert!(!Arc::ptr_eq(&original, &scheduler));
    assert_eq!(
        scheduler.policy(),
        &crate::launcher_commands::download_policy(&applied.preferences)
    );
    let bytes = fs::read(fixture.prefs_file()).unwrap();
    let store: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .root()
                .join(".pcl-rust/experimental/extensions.json"),
        )
        .unwrap(),
    )
    .unwrap();
    for (id, values) in [
        ("local.download-presets", &saved_downloads),
        ("local.launcher-dashboard", &saved_dashboard),
    ] {
        assert_eq!(
            &store["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == id)
                .unwrap()["settings"],
            values
        );
    }
    assert!(host.suspend().unwrap().released);
    let reopened = fixture.host();
    assert_eq!(
        content(cards(&reopened, "home.quick-actions")),
        content(quick)
    );
    assert_eq!(
        content(cards(&reopened, "settings.manage")),
        content(manage)
    );
    assert_eq!(
        content(cards(&reopened, "settings.launch")),
        content(checklist)
    );
    assert_eq!(
        LauncherPreferencesStore::load(fixture.root())
            .snapshot()
            .preferences,
        applied.preferences
    );
    // Both actual download cards resolve the same persisted values after the
    // new host has opened its own store; an unchanged policy keeps one budget.
    let reopened_entry = entry(&reopened);
    execute(
        &reopened,
        &shared,
        request(&reopened_entry, "quick"),
        no_copy,
    )
    .unwrap();
    assert!(Arc::ptr_eq(&scheduler, &pcl_network::download_snapshot()));
    let revoked = reopened
        .call(
            "extensions_revoke",
            json!({"id":"local.download-presets",
        "capability":"launcher.downloads","expected":expected(&reopened_entry)}),
        )
        .unwrap();
    for entry in [&reopened_entry, &revoked] {
        assert!(execute(&reopened, &shared, request(entry, "downloads"), no_copy).is_err());
    }
    assert_eq!(
        cards(&reopened, "settings.manage")[0]["actions"][0]["enabled"],
        false
    );
    assert_eq!(
        card(
            &cards(&reopened, "home.quick-actions"),
            "local.download-presets"
        )["widgets"],
        download_quick["widgets"]
    );
    assert_eq!(fs::read(fixture.prefs_file()).unwrap(), bytes);
    assert_eq!(
        shared.launcher_preferences.snapshot().preferences,
        applied.preferences
    );
    assert!(Arc::ptr_eq(&scheduler, &pcl_network::download_snapshot()));
    assert!(reopened.suspend().unwrap().released);
    let restored = fixture.host();
    let restored_quick = card(
        &cards(&restored, "home.quick-actions"),
        "local.download-presets",
    );
    assert_eq!(restored_quick["widgets"], download_quick["widgets"]);
    assert_eq!(restored_quick["actions"][0]["enabled"], false);
    assert!(execute(
        &restored,
        &shared,
        request(&entry(&restored), "quick"),
        no_copy
    )
    .is_err());
    assert_eq!(fs::read(fixture.prefs_file()).unwrap(), bytes);
    assert!(Arc::ptr_eq(&scheduler, &pcl_network::download_snapshot()));
    assert!(!fixture.root().join("experimental/agent-host.mjs").exists());
}

#[test]
fn settings_protocol_budget_saves_eight_maximum_unicode_values_and_preserves_session_on_oversize() {
    let fixture = Fixture::new();
    fixture.use_real_light_host();
    let host = fixture.host();
    let settings: Vec<_> = (0..8)
        .map(|index| {
            json!({"id":format!("note-{index}"),"label":format!("Note {index}"),
            "type":"text","default":"","maxLength":4000,"multiline":true})
        })
        .collect();
    let manifest = json!({
        "format":"pcl-linux.declarative-extension","schemaVersion":1,
        "id":"example.unicode-budget","name":"Unicode budget","version":"1.0.0",
        "publisher":"Native fixture","api":{"min":1,"maxExclusive":2},
        "capabilities":[
            {"id":"ui.cards","required":true,"reason":"Display one saved note."},
            {"id":"storage.settings","required":true,"reason":"Save bounded notes."}
        ],
        "settings":settings,
        "contributions":{"cards":[{"id":"notes","slot":"home.secondary",
            "title":"Saved note","text":"One projected field keeps the card response bounded.",
            "fields":["note-0"],"actions":[]}]}
    });
    let review = host
        .call("extensions_review", json!({"source":manifest.to_string()}))
        .unwrap();
    let entry = host
        .call(
            "extensions_confirm",
            json!({"token":review["token"],"grants":["ui.cards","storage.settings"]}),
        )
        .unwrap();
    let text = "🦀".repeat(4000);
    let values: serde_json::Map<_, _> = (0..8)
        .map(|index| (format!("note-{index}"), json!(text)))
        .collect();
    let request = json!({"id":manifest["id"],"expected":expected(&entry),"values":values});
    let bytes =
        serde_json::to_vec(&json!({"operation":"extensions_settings","args":request})).unwrap();
    assert!(bytes.len() > 128_000 && bytes.len() < 512 * 1024);
    let saved = host.call("extensions_settings", request).unwrap();
    assert_ne!(saved["generation"], entry["generation"]);
    let cards = host
        .call("extensions_cards", json!({"slot":"home.secondary"}))
        .unwrap();
    assert_eq!(cards[0]["fields"][0]["value"], text);
    let store_file = fixture
        .root()
        .join(".pcl-rust/experimental/extensions.json");
    let committed = fs::read(&store_file).unwrap();
    let store: Value = serde_json::from_slice(&committed).unwrap();
    for (key, value) in &values {
        assert_eq!(&store["entries"][0]["settings"][key], value);
    }
    let oversized = json!({"id":manifest["id"],"expected":expected(&saved),
        "values":{"note-0":"x".repeat(512 * 1024)}});
    let pid = host.session.lock().unwrap().as_ref().unwrap().child.id();
    let error = host.call("extensions_settings", oversized).unwrap_err();
    assert!(
        error.contains("524 KB"),
        "expected native request-budget refusal: {error}"
    );
    assert_eq!(fs::read(&store_file).unwrap(), committed);
    // Refusal happens before any stdio write and must not poison or replace the
    // live host. Its next result still belongs to its own protocol request.
    let listed = host.call("extensions_list", json!({})).unwrap();
    assert_eq!(listed["entries"][0]["generation"], saved["generation"]);
    assert_eq!(
        host.session.lock().unwrap().as_ref().unwrap().child.id(),
        pid
    );
    assert!(host.suspend().unwrap().released);
    let cards = host
        .call("extensions_cards", json!({"slot":"home.secondary"}))
        .unwrap();
    assert_eq!(cards[0]["fields"][0]["value"], text);
    assert_eq!(fs::read(store_file).unwrap(), committed);
}

fn expected(entry: &Value) -> Value {
    json!({"digest":entry["digest"],"generation":entry["generation"]})
}
