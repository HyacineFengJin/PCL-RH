#[cfg(test)]
mod fixtures {
    use super::super::*;
    use std::{
        fs,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{symlink, PermissionsExt},
        },
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../work/launcher-options-2026-10-05/tests/fixtures")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn file(&self) -> PathBuf {
            self.0.join(".pcl-rust/launcher-preferences.json")
        }
        fn seed(&self, bytes: &[u8]) {
            fs::create_dir_all(self.0.join(".pcl-rust")).unwrap();
            fs::write(self.file(), bytes).unwrap();
        }
        fn store(&self) -> LauncherPreferencesStore {
            LauncherPreferencesStore::load(&self.0)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn patch(value: serde_json::Value) -> LauncherPreferencesPatch {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn game_defaults_older_documents_partial_edits_stale_revisions_and_transfer() {
        use pcl_core::launch_options::{IpPreference, WindowSize};
        let fixture = Fixture::new();
        let mut old = serde_json::to_value(Persisted::default()).unwrap();
        old["preferences"]
            .as_object_mut()
            .unwrap()
            .remove("game_launch");
        let bytes = serde_json::to_vec(&old).unwrap();
        fixture.seed(&bytes);
        let store = fixture.store();
        let before = store.snapshot();
        assert_eq!(before.preferences.game_launch, Default::default());
        assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
        let sized = store
            .update(
                &before.revision,
                patch(serde_json::json!({
                    "game_launch": {
                        "window": {"mode": "custom", "width": 1920, "height": 1080},
                        "window_title": "你好 Minecraft 🦀",
                        "custom_info": "Custom version",
                        "jvm_args": "-Dexample=fixture",
                        "game_args": "--demo"
                    }
                })),
            )
            .unwrap();
        assert!(store
            .update(
                &before.revision,
                patch(serde_json::json!({"game_launch":{"ip":"ipv6"}}))
            )
            .is_err());
        let after = store
            .update(
                &sized.revision,
                patch(serde_json::json!({"game_launch":{"ip":"ipv6"}})),
            )
            .unwrap();
        assert_eq!(
            after.preferences.game_launch.window,
            WindowSize::Custom {
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(after.preferences.game_launch.ip, IpPreference::Ipv6);
        assert_eq!(after.preferences.game_launch.custom_info, "Custom version");
        assert_eq!(after.preferences.game_launch.jvm_args, "-Dexample=fixture");
        assert_eq!(after.preferences.game_launch.game_args, "--demo");
        assert_eq!(
            after.preferences.game_launch.window_title,
            "你好 Minecraft 🦀"
        );
        assert_eq!(
            fixture.store().snapshot().preferences.game_launch,
            after.preferences.game_launch
        );
        assert_eq!(after.preferences.appearance, before.preferences.appearance);
        let export = store.export_settings(&after.revision).unwrap();
        let mut old_export: serde_json::Value = serde_json::from_slice(&export).unwrap();
        old_export["preferences"]
            .as_object_mut()
            .unwrap()
            .remove("game_launch");
        assert_eq!(
            super::super::transfer::parse_import(&serde_json::to_vec(&old_export).unwrap())
                .unwrap()
                .game_launch,
            Default::default()
        );
        let target = Fixture::new();
        let target_store = target.store();
        let imported = target_store
            .import_settings(&target_store.snapshot().revision, &export)
            .unwrap();
        assert_eq!(
            imported.preferences.game_launch,
            after.preferences.game_launch
        );
    }
    #[test]
    fn invalid_game_defaults_leave_disk_intact_and_invalid_external_data_blocks() {
        let fixture = Fixture::new();
        fixture.seed(&serde_json::to_vec(&Persisted::default()).unwrap());
        let store = fixture.store();
        let before = store.snapshot();
        let bytes = fs::read(fixture.file()).unwrap();
        assert!(store
            .update(
                &before.revision,
                patch(serde_json::json!({
                    "game_launch": {
                        "window": {"mode": "custom", "width": 319, "height": 720}
                    }
                }))
            )
            .is_err());
        for title in ["bad\ntitle".to_owned(), "🦀".repeat(257)] {
            assert!(store
                .update(
                    &before.revision,
                    patch(serde_json::json!({"game_launch":{"window_title":title}}))
                )
                .is_err());
            assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
        }
        assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
        let mut invalid: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        invalid["preferences"]["game_launch"]["window"] =
            serde_json::json!({"mode":"custom","width":0,"height":720});
        let unknown = serde_json::to_vec(&invalid).unwrap();
        fixture.seed(&unknown);
        assert!(store.reload().warning.is_some());
        assert!(store
            .update(
                &store.snapshot().revision,
                patch(serde_json::json!({"game_launch":{"ip":"ipv4"}}))
            )
            .is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), unknown);
    }
    #[test]
    fn older_game_documents_default_arguments_and_partial_clear_preserves_other_fields() {
        let fixture = Fixture::new();
        let mut older = serde_json::to_value(Persisted::default()).unwrap();
        for key in ["custom_info", "jvm_args", "game_args"] {
            older["preferences"]["game_launch"]
                .as_object_mut()
                .unwrap()
                .remove(key);
        }
        let bytes = serde_json::to_vec(&older).unwrap();
        fixture.seed(&bytes);
        let store = fixture.store();
        let before = store.snapshot();
        assert!(before.warning.is_none());
        assert!(before.preferences.game_launch.custom_info.is_empty());
        assert!(before.preferences.game_launch.jvm_args.is_empty());
        assert!(before.preferences.game_launch.game_args.is_empty());
        assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
        let changed = store.update(&before.revision, patch(serde_json::json!({
            "game_launch": {"custom_info":"Custom", "jvm_args":"-Dexample=fixture", "game_args":"--demo"}
        }))).unwrap();
        let cleared = store
            .update(
                &changed.revision,
                patch(serde_json::json!({"game_launch":{"jvm_args":""}})),
            )
            .unwrap();
        assert!(cleared.preferences.game_launch.jvm_args.is_empty());
        assert_eq!(cleared.preferences.game_launch.custom_info, "Custom");
        assert_eq!(cleared.preferences.game_launch.game_args, "--demo");
        let saved = fs::read(fixture.file()).unwrap();
        for arguments in [
            serde_json::json!({"custom_info":"bad\ninfo"}),
            serde_json::json!({"jvm_args":"-Dprivate='private-value"}),
            serde_json::json!({"game_args":"--accessToken=private-value"}),
        ] {
            let error = store
                .update(
                    &cleared.revision,
                    patch(serde_json::json!({"game_launch": arguments.clone()})),
                )
                .unwrap_err();
            assert!(!error.contains("private-value"));
            let mut import: serde_json::Value =
                serde_json::from_slice(&store.export_settings(&cleared.revision).unwrap()).unwrap();
            import["preferences"]["game_launch"] = arguments;
            let error = store
                .import_settings(&cleared.revision, &serde_json::to_vec(&import).unwrap())
                .unwrap_err();
            assert!(!error.contains("private-value"));
            assert_eq!(fs::read(fixture.file()).unwrap(), saved);
            assert_eq!(store.snapshot().revision, cleared.revision);
        }
        let mut malformed: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        malformed["preferences"]["game_launch"]["game_args"] =
            serde_json::json!("--accessToken=private-value");
        let malformed = serde_json::to_vec(&malformed).unwrap();
        fixture.seed(&malformed);
        let view = store.reload();
        assert!(view.warning.is_some());
        assert!(store
            .update(
                &view.revision,
                patch(serde_json::json!({"game_launch":{"game_args":""}}))
            )
            .is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), malformed);
    }

    fn changed(store: &LauncherPreferencesStore) -> LauncherPreferencesView {
        let view = store.snapshot();
        store
            .update(
                &view.revision,
                patch(serde_json::json!({"appearance":{"opacity_percent":90}})),
            )
            .unwrap()
    }

    #[test]
    fn defaults_are_read_only_and_keep_the_current_appearance_and_privacy() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let view = store.snapshot();
        assert_eq!(view.preferences.appearance.theme, Theme::Light);
        assert_eq!(view.preferences.appearance.light_palette, Palette::Blue);
        assert_eq!(view.preferences.appearance.opacity_percent, 100);
        assert!(view.preferences.appearance.global_font.is_empty());
        assert!(!view.preferences.local_diagnostics_enabled);
        assert!(!view.preferences.network.doh_enabled);
        assert_eq!(view.preferences.network.proxy_mode, ProxyMode::System);
        assert!(view.preferences.background.background_asset_id.is_empty());
        assert!(view.preferences.background.music_asset_id.is_empty());
        assert!(view.preferences.navigation.f12_reveal_enabled);
        assert!(!fixture.0.join(".pcl-rust").exists());
        assert!(view.warning.is_none());
    }

    #[test]
    fn interaction_preferences_migrate_defaults_and_survive_restart() {
        let fixture = Fixture::new();
        // Legacy files omit interaction fields and can retain hidden network IDs.
        fixture.seed(br#"{"schema_version":1,"preferences":{"appearance":{"theme":"dark"},"navigation":{"hidden_menu_ids":["tools.network","settings.network"]}}}"#);
        let store = fixture.store();
        let before = store.snapshot();
        assert!(before.preferences.appearance.custom_cursor);
        assert_eq!(before.preferences.appearance.cursor_size, CursorSize::Small);
        assert_eq!(
            before.preferences.appearance.cursor_style,
            CursorStyle::Accent
        );
        assert!(before.preferences.appearance.custom_context_menu);
        let after = store.update(&before.revision, patch(serde_json::json!({
            "appearance": {"custom_cursor": false, "cursor_style": "outline", "cursor_size": "standard",
                "custom_context_menu": false, "context_menu_density": "compact"},
            "navigation": {"hidden_menu_ids": ["tools.network", "tools.ai", "tools.maker", "tools.porter", "tools.extensions"]}
        }))).unwrap();
        assert_eq!(after.preferences.appearance.theme, Theme::Dark);
        assert!(!after.preferences.appearance.custom_cursor);
        assert_eq!(
            after.preferences.appearance.cursor_style,
            CursorStyle::Outline
        );
        assert_eq!(
            after.preferences.appearance.context_menu_density,
            ContextMenuDensity::Compact
        );
        assert_eq!(fixture.store().snapshot().preferences, after.preferences);
    }

    #[test]
    fn interaction_preferences_reject_unknown_styles_without_writing() {
        let fixture = Fixture::new();
        let store = fixture.store();
        for value in [
            serde_json::json!({"appearance":{"cursor_size":"giant"}}),
            serde_json::json!({"appearance":{"cursor_style":"unknown"}}),
            serde_json::json!({"appearance":{"context_menu_density":"unknown"}}),
        ] {
            assert!(serde_json::from_value::<LauncherPreferencesPatch>(value).is_err());
        }
        assert!(!fixture.file().exists());
        assert_eq!(
            store.snapshot().preferences.appearance.cursor_size,
            CursorSize::Small
        );
    }

    #[test]
    fn partial_updates_survive_restart_without_changing_game_settings() {
        let fixture = Fixture::new();
        fixture.seed(b"{\"schema_version\":1,\"preferences\":{}}");
        let settings = fixture.0.join(".pcl-rust/settings.json");
        fs::write(&settings, b"opaque v3 game settings bytes").unwrap();
        let store = fixture.store();
        let before = store.snapshot();
        let after = store
            .update(
                &before.revision,
                patch(serde_json::json!({
                    "title":{"mode":"text","text":"Fixture title"},
                    "navigation":{"hidden_menu_ids":["instance.mods","settings.logs"]},
                    "launch_visibility":"hide_while_game"
                })),
            )
            .unwrap();
        assert_eq!(after.preferences.title.mode, TitleMode::Text);
        assert_eq!(after.preferences.appearance, before.preferences.appearance);
        assert_ne!(before.revision, after.revision);
        let restart = fixture.store().snapshot();
        assert_eq!(restart.preferences, after.preferences);
        assert_ne!(restart.revision, after.revision);
        assert_eq!(
            fs::read(settings).unwrap(),
            b"opaque v3 game settings bytes"
        );
        assert_eq!(
            fs::metadata(fixture.file()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn stale_revision_and_invalid_patches_leave_the_document_unchanged() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.snapshot();
        let current = changed(&store);
        let bytes = fs::read(fixture.file()).unwrap();
        assert!(store
            .update(
                &initial.revision,
                patch(serde_json::json!({"local_diagnostics_enabled":true}))
            )
            .is_err());
        for invalid in [
            serde_json::json!({"appearance":{"opacity_percent":0}}),
            serde_json::json!({"appearance":{"global_font":"x; background:red"}}),
            serde_json::json!({"animation":{"fps_limit":241}}),
            serde_json::json!({"animation":{"speed_percent":0}}),
            serde_json::json!({"realtime_log_line_limit":100001}),
            serde_json::json!({"advanced":{"artificial_delay_ms":5001}}),
            serde_json::json!({"home":{"mode":"remote","remote_url":"javascript:alert(1)"}}),
            serde_json::json!({"home":{"local_path":"/tmp/../outside"}}),
            serde_json::json!({"background":{"background_asset_id":"../outside"}}),
            serde_json::json!({"navigation":{"hidden_menu_ids":["main.tools","main.tools"]}}),
        ] {
            assert!(store.update(&current.revision, patch(invalid)).is_err());
            assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
            assert_eq!(store.snapshot().revision, current.revision);
        }
        assert!(serde_json::from_value::<LauncherPreferencesPatch>(
            serde_json::json!({"root_id":"none"})
        )
        .is_err());
        assert!(serde_json::from_value::<LauncherPreferencesPatch>(
            serde_json::json!({"navigation":{"hidden_menu_ids":["invented.page"]}})
        )
        .is_err());
    }

    #[test]
    fn malformed_future_unknown_and_oversized_files_are_retained_read_only() {
        for bytes in [
            b"{broken".to_vec(),
            b"{\"schema_version\":2,\"preferences\":{}}".to_vec(),
            b"{\"schema_version\":1,\"preferences\":{\"accounts\":[]}}".to_vec(),
            vec![b' '; MAX_IMPORT_BYTES + 1],
        ] {
            let fixture = Fixture::new();
            fixture.seed(&bytes);
            let store = fixture.store();
            let view = store.snapshot();
            assert!(view.warning.is_some());
            assert_eq!(view.preferences, LauncherPreferences::default());
            assert!(store
                .update(
                    &view.revision,
                    patch(serde_json::json!({"local_diagnostics_enabled":true}))
                )
                .is_err());
            assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
        }
    }

    #[test]
    fn external_content_edits_and_inode_replacements_need_explicit_reload() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let view = changed(&store);
        let mut doc: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
        doc["preferences"]["appearance"]["opacity_percent"] = 88.into();
        let external = serde_json::to_vec(&doc).unwrap();
        fs::write(fixture.file(), &external).unwrap();
        assert!(store.snapshot().warning.is_some());
        assert!(store
            .update(
                &view.revision,
                patch(serde_json::json!({"local_diagnostics_enabled":true}))
            )
            .is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), external);
        let fresh = store.reload();
        assert!(fresh.warning.is_none());
        assert_eq!(fresh.preferences.appearance.opacity_percent, 88);
        assert_ne!(fresh.revision, view.revision);
        let replacement = fixture.0.join("replacement");
        fs::write(&replacement, &external).unwrap();
        fs::rename(replacement, fixture.file()).unwrap();
        assert!(store.snapshot().warning.is_some());
        assert_eq!(fs::read(fixture.file()).unwrap(), external);
    }

    #[test]
    fn symlinks_hardlinks_special_files_and_replaced_directories_are_refused() {
        let fixture = Fixture::new();
        let outside = fixture.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("launcher-preferences.json"), b"external").unwrap();
        symlink(&outside, fixture.0.join(".pcl-rust")).unwrap();
        assert!(fixture.store().snapshot().warning.is_some());
        fs::remove_file(fixture.0.join(".pcl-rust")).unwrap();
        fs::create_dir(fixture.0.join(".pcl-rust")).unwrap();
        symlink(outside.join("launcher-preferences.json"), fixture.file()).unwrap();
        assert!(fixture.store().snapshot().warning.is_some());
        fs::remove_file(fixture.file()).unwrap();
        fs::hard_link(outside.join("launcher-preferences.json"), fixture.file()).unwrap();
        assert!(fixture.store().snapshot().warning.is_some());
        fs::remove_file(fixture.file()).unwrap();
        let fifo = std::ffi::CString::new(fixture.file().as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(fixture.store().snapshot().warning.is_some());
        fs::remove_file(fixture.file()).unwrap();
        let store = fixture.store();
        let view = changed(&store);
        fs::rename(fixture.0.join(".pcl-rust"), fixture.0.join("old")).unwrap();
        fs::create_dir(fixture.0.join(".pcl-rust")).unwrap();
        assert!(store
            .update(
                &view.revision,
                patch(serde_json::json!({"local_diagnostics_enabled":true}))
            )
            .is_err());
        assert!(!fixture.file().exists());
        assert_eq!(
            fs::read(outside.join("launcher-preferences.json")).unwrap(),
            b"external"
        );
    }

    #[test]
    fn settings_export_import_keeps_preferences_and_excludes_runtime_identity() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let before = store.snapshot();
        let after = store.update(&before.revision, patch(serde_json::json!({
            "title":{"mode":"image","image_path":"/fixture/title.png"},
            "home":{"mode":"remote","local_path":"/fixture/home.json","remote_url":"https://example.invalid/home.json?version=1"},
            "network":{"proxy_mode":"custom","custom_proxy_url":"http://127.0.0.1:8080"},
            "appearance":{"global_font":"Noto Sans CJK SC"},
            "localization":{"language":"en-US","region":"zh-CN"}
        }))).unwrap();
        let exported = store.export_settings(&after.revision).unwrap();
        let text = String::from_utf8(exported.clone()).unwrap();
        assert!(text.contains("/fixture/title.png"));
        assert!(text.contains("/fixture/home.json"));
        assert!(text.contains("https://example.invalid/home.json?version=1"));
        assert!(text.contains("http://127.0.0.1:8080"));
        for forbidden in [
            "root_id",
            "java_paths",
            "accounts",
            "revision",
            "task_id",
            "password",
            "cdk",
            "highWater",
            "readIds",
            "policyRevision",
        ] {
            assert!(!text.contains(forbidden));
        }
        let target = Fixture::new();
        let target_store = target.store();
        let target_view = target_store.snapshot();
        let imported = target_store
            .import_settings(&target_view.revision, &exported)
            .unwrap();
        assert_eq!(imported.preferences, after.preferences);
        assert!(!target.0.join("fixture").exists());
        let bytes = fs::read(target.file()).unwrap();
        let mut invalid: serde_json::Value = serde_json::from_slice(&exported).unwrap();
        invalid["preferences"]["accounts"] = serde_json::json!([]);
        assert!(target_store
            .import_settings(&imported.revision, &serde_json::to_vec(&invalid).unwrap())
            .is_err());
        invalid["preferences"]
            .as_object_mut()
            .unwrap()
            .remove("accounts");
        invalid["schema_version"] = 2.into();
        assert!(target_store
            .import_settings(&imported.revision, &serde_json::to_vec(&invalid).unwrap())
            .is_err());
        assert!(target_store
            .import_settings(&imported.revision, &vec![b' '; MAX_IMPORT_BYTES + 1])
            .is_err());
        assert_eq!(fs::read(target.file()).unwrap(), bytes);
    }

    #[test]
    fn minecraft_notice_flags_are_independent_persisted_and_old_backups_default_to_disabled() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let defaults = store.snapshot();
        assert!(
            !defaults
                .preferences
                .management
                .minecraft_release_notifications
        );
        assert!(
            !defaults
                .preferences
                .management
                .minecraft_snapshot_notifications
        );
        let release = store
            .update(
                &defaults.revision,
                patch(serde_json::json!({
                    "management":{"minecraft_release_notifications":true}
                })),
            )
            .unwrap();
        assert!(
            !release
                .preferences
                .management
                .minecraft_snapshot_notifications
        );
        let both = store
            .update(
                &release.revision,
                patch(serde_json::json!({
                    "management":{"minecraft_snapshot_notifications":true}
                })),
            )
            .unwrap();
        let restarted = fixture.store().snapshot();
        assert!(
            restarted
                .preferences
                .management
                .minecraft_release_notifications
        );
        assert!(
            restarted
                .preferences
                .management
                .minecraft_snapshot_notifications
        );
        let exported = store.export_settings(&both.revision).unwrap();
        let fresh = Fixture::new();
        let target = fresh.store();
        let view = target.snapshot();
        let imported = target.import_settings(&view.revision, &exported).unwrap();
        assert_eq!(imported.preferences.management, both.preferences.management);

        // Older schema-1 backups know neither flag. They must restore disabled
        // policy, rather than inherit notification consent from this process.
        let mut old: serde_json::Value = serde_json::from_slice(&exported).unwrap();
        let management = old["preferences"]["management"].as_object_mut().unwrap();
        management.remove("minecraft_release_notifications");
        management.remove("minecraft_snapshot_notifications");
        let legacy = target
            .import_settings(&imported.revision, &serde_json::to_vec(&old).unwrap())
            .unwrap();
        assert!(
            !legacy
                .preferences
                .management
                .minecraft_release_notifications
        );
        assert!(
            !legacy
                .preferences
                .management
                .minecraft_snapshot_notifications
        );
        assert!(!fresh
            .0
            .join(".pcl-rust/launcher-local/minecraft-updates.json")
            .exists());
    }

    #[test]
    fn proxy_authentication_query_fragment_and_unsupported_schemes_never_persist_or_export() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let view = store.snapshot();
        for endpoint in [
            "http://user:secret@host:8080",
            "http://user@host:8080",
            "http://host:8080?secret=token",
            "http://host:8080/#secret",
            "socks5://host:1080",
            "http://host:8080/path",
        ] {
            assert!(store.update(&view.revision, patch(serde_json::json!({"network":{"proxy_mode":"custom","custom_proxy_url":endpoint}}))).is_err());
            assert!(!fixture.file().exists());
        }
        let mut document: serde_json::Value =
            serde_json::from_slice(&store.export_settings(&view.revision).unwrap()).unwrap();
        document["preferences"]["network"]["proxy_mode"] = "custom".into();
        document["preferences"]["network"]["custom_proxy_url"] =
            "http://user:secret@host:8080".into();
        assert!(store
            .import_settings(&view.revision, &serde_json::to_vec(&document).unwrap())
            .is_err());
        assert!(!fixture.file().exists());
    }

    #[test]
    fn hardware_acceleration_reports_restart_only_for_changed_startup_policy() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = store.snapshot();
        let changed = store
            .update(
                &initial.revision,
                patch(serde_json::json!({"disable_hardware_acceleration":true})),
            )
            .unwrap();
        assert!(changed.hardware_acceleration_restart_required);
        assert!(
            !fixture
                .store()
                .snapshot()
                .hardware_acceleration_restart_required
        );
        let restored = store
            .update(
                &changed.revision,
                patch(serde_json::json!({"disable_hardware_acceleration":false})),
            )
            .unwrap();
        assert!(!restored.hardware_acceleration_restart_required);
    }

    #[test]
    fn staged_write_failure_and_publication_race_preserve_original_bytes() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let initial = changed(&store);
        let original = fs::read(fixture.file()).unwrap();
        store.test_before_replace(|_| Err("fixture disk failure".into()));
        assert!(store
            .update(
                &initial.revision,
                patch(serde_json::json!({"local_diagnostics_enabled":true}))
            )
            .is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), original);
        assert!(store.snapshot().warning.is_none());
        let external = b"{\"schema_version\":2,\"preferences\":{}}".to_vec();
        let copied = external.clone();
        store.test_before_replace(move |project| {
            fs::write(project.join(".pcl-rust/launcher-preferences.json"), copied).unwrap();
            Ok(())
        });
        assert!(store
            .update(
                &initial.revision,
                patch(serde_json::json!({"local_diagnostics_enabled":true}))
            )
            .is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), external);
        assert!(store.snapshot().warning.is_some());
        assert!(fs::read_dir(fixture.0.join(".pcl-rust"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }

    #[test]
    fn newly_created_file_at_publication_is_not_overwritten() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let view = store.snapshot();
        store.test_before_replace(|project| {
            fs::write(
                project.join(".pcl-rust/launcher-preferences.json"),
                b"outside bytes",
            )
            .unwrap();
            Ok(())
        });
        assert!(store
            .update(
                &view.revision,
                patch(serde_json::json!({"local_diagnostics_enabled":true}))
            )
            .is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), b"outside bytes");
    }

    #[test]
    fn all_supported_hide_ids_can_be_saved_together() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let before = store.snapshot();
        let after = store
            .update(
                &before.revision,
                patch(serde_json::json!({"navigation":{"hidden_menu_ids":MenuId::ALL}})),
            )
            .unwrap();
        assert_eq!(
            after.preferences.navigation.hidden_menu_ids.len(),
            MenuId::ALL.len()
        );
        assert_eq!(
            fixture
                .store()
                .snapshot()
                .preferences
                .navigation
                .hidden_menu_ids,
            MenuId::ALL
        );
    }

    #[test]
    fn oversized_and_symlink_publication_races_restore_the_outside_object() {
        for link in [false, true] {
            let fixture = Fixture::new();
            let store = fixture.store();
            let before = changed(&store);
            let outside = fixture.0.join("outside");
            fs::write(&outside, b"outside original").unwrap();
            store.test_before_replace(move |project| {
                let target = project.join(".pcl-rust/launcher-preferences.json");
                if link {
                    fs::remove_file(&target).unwrap();
                    symlink(project.join("outside"), &target).unwrap();
                } else {
                    fs::write(&target, vec![b'x'; MAX_IMPORT_BYTES + 1]).unwrap();
                }
                Ok(())
            });
            assert!(store
                .update(
                    &before.revision,
                    patch(serde_json::json!({"local_diagnostics_enabled":true}))
                )
                .is_err());
            if link {
                assert!(fs::symlink_metadata(fixture.file())
                    .unwrap()
                    .file_type()
                    .is_symlink());
                assert_eq!(fs::read(outside).unwrap(), b"outside original");
            } else {
                assert_eq!(
                    fs::metadata(fixture.file()).unwrap().len(),
                    MAX_IMPORT_BYTES as u64 + 1
                );
            }
            assert!(store.snapshot().warning.is_some());
        }
    }

    #[test]
    fn file_export_is_private_anonymous_and_never_replaces_existing_targets() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let view = changed(&store);
        let exported = fixture.0.join("settings.json");
        export_to_file(&store, &view.revision, &exported).unwrap();
        assert_eq!(
            fs::metadata(&exported).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let bytes = read_settings_file(&exported).unwrap();
        assert_eq!(preview_import(&bytes).unwrap(), view.preferences);
        assert_eq!(
            fs::read(&exported).unwrap(),
            store.export_settings(&view.revision).unwrap()
        );
        assert!(export_to_file(&store, &view.revision, &exported).is_err());
        assert_eq!(fs::read(&exported).unwrap(), bytes);
        let wrong_extension = fixture.0.join("settings.zip");
        assert!(export_to_file(&store, &view.revision, &wrong_extension).is_err());
        assert!(!wrong_extension.exists());
        let linked = fixture.0.join("linked.json");
        symlink(&exported, &linked).unwrap();
        assert!(export_to_file(&store, &view.revision, &linked).is_err());
        assert!(fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&exported).unwrap(), bytes);
    }

    #[test]
    fn selected_import_files_require_bounded_stable_independent_regular_files() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let view = store.snapshot();
        let original = fixture.0.join("original.json");
        fs::write(&original, store.export_settings(&view.revision).unwrap()).unwrap();
        let linked = fixture.0.join("linked.json");
        symlink(&original, &linked).unwrap();
        assert!(read_settings_file(&linked).is_err());
        let hard = fixture.0.join("hard.json");
        fs::hard_link(&original, &hard).unwrap();
        assert!(read_settings_file(&hard).is_err());
        assert!(read_settings_file(&original).is_err());
        fs::remove_file(&hard).unwrap();
        let directory = fixture.0.join("folder.json");
        fs::create_dir(&directory).unwrap();
        assert!(read_settings_file(&directory).is_err());
        let oversized = fixture.0.join("oversized.json");
        fs::write(&oversized, vec![b' '; MAX_IMPORT_BYTES + 1]).unwrap();
        assert!(read_settings_file(&oversized).is_err());
        let fifo = fixture.0.join("fifo.json");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(read_settings_file(&fifo).is_err());
        assert!(read_settings_file(Path::new("relative.json")).is_err());
        let parent_link = fixture.0.join("alias");
        symlink(&fixture.0, &parent_link).unwrap();
        assert!(read_settings_file(&parent_link.join("original.json")).is_err());
        assert!(export_to_file(&store, &view.revision, &parent_link.join("new.json")).is_err());
        assert!(!fixture.0.join("new.json").exists());
    }

    #[test]
    fn preview_validation_has_no_disk_or_network_effect_and_import_backs_up_previous_preferences() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let original = changed(&store);
        let source = Fixture::new();
        let source_store = source.store();
        let view = source_store.snapshot();
        let remote = source_store.update(&view.revision, patch(serde_json::json!({"home":{"mode":"remote","remote_url":"https://example.invalid/home.json"}}))).unwrap();
        let bytes = source_store.export_settings(&remote.revision).unwrap();
        let before_bytes = fs::read(fixture.file()).unwrap();
        let preview = preview_import(&bytes).unwrap();
        assert_eq!(preview, remote.preferences);
        assert_eq!(fs::read(fixture.file()).unwrap(), before_bytes);
        assert_eq!(store.snapshot().revision, original.revision);
        let imported = store.import_settings(&original.revision, &bytes).unwrap();
        assert_eq!(imported.preferences, preview);
        let backup_path = PathBuf::from(imported.last_import_backup_path.unwrap());
        assert!(backup_path.starts_with(fixture.0.join(".pcl-rust")));
        assert_eq!(
            fs::metadata(&backup_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let prior = preview_import(&read_settings_file(&backup_path).unwrap()).unwrap();
        assert_eq!(prior, original.preferences);
        let recovered = store
            .import_settings(
                &imported.revision,
                &read_settings_file(&backup_path).unwrap(),
            )
            .unwrap();
        assert_eq!(recovered.preferences, original.preferences);
    }

    #[test]
    fn absent_document_import_has_no_backup_and_failed_publish_keeps_recoverable_backup() {
        let source = Fixture::new();
        let source_store = source.store();
        let wanted = changed(&source_store);
        let bytes = source_store.export_settings(&wanted.revision).unwrap();
        let fixture = Fixture::new();
        let store = fixture.store();
        let before = store.snapshot();
        let imported = store.import_settings(&before.revision, &bytes).unwrap();
        assert!(imported.last_import_backup_path.is_none());
        let original_bytes = fs::read(fixture.file()).unwrap();
        let defaults_source = Fixture::new();
        let defaults = defaults_source.store();
        let default_view = defaults.snapshot();
        let defaults_bytes = defaults.export_settings(&default_view.revision).unwrap();
        store.test_before_replace(|_| Err("fixture interrupted import".into()));
        assert!(store
            .import_settings(&imported.revision, &defaults_bytes)
            .is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), original_bytes);
        let retained = store.snapshot();
        let backup = retained.last_import_backup_path.unwrap();
        assert_eq!(
            preview_import(&read_settings_file(Path::new(&backup)).unwrap()).unwrap(),
            imported.preferences
        );
        assert!(retained.warning.is_none());
    }

    #[test]
    fn resetting_preferences_uses_the_same_recoverable_settings_only_backup() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let before = changed(&store);
        let after = store.reset(&before.revision).unwrap();
        assert_eq!(after.preferences, LauncherPreferences::default());
        let backup = after.last_import_backup_path.unwrap();
        assert_eq!(
            preview_import(&read_settings_file(Path::new(&backup)).unwrap()).unwrap(),
            before.preferences
        );
    }

    #[test]
    fn process_lock_is_nonblocking_and_explicitly_unlocks_duplicated_descriptors() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let view = store.snapshot();
        let duplicate = store
            .test_with_write_lock(|fd| {
                let duplicate = unsafe { libc::dup(fd) };
                assert!(duplicate >= 0);
                let other = fixture.store();
                let started = std::time::Instant::now();
                assert!(other
                    .update(
                        &other.snapshot().revision,
                        patch(serde_json::json!({"local_diagnostics_enabled":true}))
                    )
                    .is_err());
                assert!(started.elapsed() < std::time::Duration::from_secs(1));
                unsafe { std::fs::File::from_raw_fd(duplicate) }
            })
            .unwrap();
        assert!(store
            .update(
                &view.revision,
                patch(serde_json::json!({"local_diagnostics_enabled":true}))
            )
            .is_ok());
        assert!(duplicate.as_raw_fd() >= 0);
        drop(duplicate);
    }
}
