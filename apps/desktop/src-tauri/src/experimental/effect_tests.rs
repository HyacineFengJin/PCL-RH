//! Only isolated stores and bounded Node stdio fixtures run here. Clipboard
//! callbacks are injected; no Tauri window, GTK display, Agent or user app runs.
use super::*;
use crate::experimental::Operation;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

struct Fixture {
    backing: crate::integration_tests::Fixture,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/plugin-capabilities-2026-10-20/native-fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
        fs::create_dir_all(root.join("experimental")).unwrap();
        fs::create_dir_all(root.join("Minecraft/.minecraft")).unwrap();
        let fixture = Self {
            backing: crate::integration_tests::Fixture(root.canonicalize().unwrap()),
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
        "launcher_preferences_update",
    ] {
        assert!(serde_json::from_value::<Operation>(json!(operation)).is_err());
    }
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
fn forged_intents_never_write_preferences_or_call_clipboard() {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let host = fixture.host();
    for intent in [
        json!({"kind":"set-launcher-appearance","extensionId":"example.other","values":{"theme":"dark"}}),
        json!({"kind":"set-launcher-appearance","extensionId":"example.fixture","values":{"theme":"dark"},"ipc":"anything"}),
        json!({"kind":"copy-text","extensionId":"example.other","text":"private"}),
        json!({"kind":"copy-text","extensionId":"example.fixture","text":"private","read":true}),
        json!({"kind":"launcher-appearance-applied","extensionId":"example.fixture"}),
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
