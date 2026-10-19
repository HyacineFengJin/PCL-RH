use super::*;
use crate::integration_tests::Fixture;
use memory::GIB;
use std::os::unix::fs::symlink;

fn fixture() -> (Fixture, Shared, Settings, GameRoot) {
    let fixture = Fixture::new();
    let shared = fixture.shared();
    let settings = shared.config.snapshot();
    let root = shared.config.resolve(None).unwrap();
    fs::create_dir_all(Path::new(&root.path).join("versions/Example")).unwrap();
    (fixture, shared, settings, root)
}
fn inputs() -> Result<(MemorySnapshot, u32)> {
    Ok((
        MemorySnapshot {
            total_bytes: 32 * GIB,
            available_bytes: 20 * GIB,
        },
        240,
    ))
}

#[test]
fn manual_does_not_read_ram_or_the_game_folder() {
    let (_fixture, _shared, settings, mut root) = fixture();
    root.overrides.insert("Example".into(), 10);
    let preview = evaluate_using(&settings, &root, Some("Example"), || {
        panic!("manual mode must not inspect the system")
    });
    assert_eq!(preview.mode, MemoryMode::Manual);
    assert_eq!(preview.memory_gib, 10);
    assert!(preview.error.is_none());
    assert!(preview.mod_count.is_none());
}

#[test]
fn inherited_and_explicit_modes_preserve_dormant_manual_values() {
    let (_fixture, _shared, mut settings, mut root) = fixture();
    settings.memory_mode = MemoryMode::Auto;
    let inherited = evaluate_using(&settings, &root, Some("Example"), inputs);
    assert_eq!(inherited.mode, MemoryMode::Auto);
    assert_eq!(inherited.memory_gib, 6);
    root.overrides.insert("Example".into(), 14);
    let legacy = evaluate_using(&settings, &root, Some("Example"), || {
        panic!("legacy custom memory remains manual")
    });
    assert_eq!(legacy.memory_gib, 14);
    root.memory_mode_overrides
        .insert("Example".into(), MemoryMode::Auto);
    let automatic = evaluate_using(&settings, &root, Some("Example"), inputs);
    assert_eq!(automatic.memory_gib, 6);
    assert_eq!(automatic.manual_gib, 14);
    assert_eq!(root.overrides["Example"], 14);
    let global = evaluate_using(&settings, &root, None, || {
        Ok((
            MemorySnapshot {
                total_bytes: 32 * GIB,
                available_bytes: 20 * GIB,
            },
            0,
        ))
    });
    assert_eq!(
        global.memory_gib, 3,
        "global preview has no selected instance workload"
    );
}

#[test]
fn automatic_failure_is_explicit_and_never_uses_saved_manual_memory() {
    let (_fixture, _shared, mut settings, root) = fixture();
    settings.memory_mode = MemoryMode::Auto;
    for error in ["无法读取系统内存", "模组目录已被替换"] {
        let preview = evaluate_using(&settings, &root, Some("Example"), || Err(error.into()));
        assert_eq!(preview.memory_gib, 0);
        assert_eq!(preview.error.as_deref(), Some(error));
        assert_eq!(preview.manual_gib, settings.memory_gib);
    }
    let low = evaluate_using(&settings, &root, Some("Example"), || {
        Ok((
            MemorySnapshot {
                total_bytes: 32 * GIB,
                available_bytes: 4 * GIB,
            },
            240,
        ))
    });
    assert!(low.error.is_some());
    assert_eq!(low.memory_gib, 0);
}

#[test]
fn enabled_mod_count_uses_the_resolved_content_directory_without_following_links() {
    let (_fixture, _shared, _settings, root) = fixture();
    let path = Path::new(&root.path);
    fs::create_dir(path.join("mods")).unwrap();
    fs::write(path.join("mods/shared.jar"), b"not opened").unwrap();
    assert_eq!(
        enabled_mods(path, "Example", IsolationPolicy::Automatic).unwrap(),
        1
    );
    let mods = path.join("versions/Example/mods");
    fs::create_dir(&mods).unwrap();
    for name in ["one.jar", "two.JAR", "disabled.jar.disabled", "readme.txt"] {
        fs::write(mods.join(name), b"not opened").unwrap();
    }
    fs::create_dir(mods.join("directory.jar")).unwrap();
    symlink(path.join("mods/shared.jar"), mods.join("linked.jar")).unwrap();
    assert_eq!(
        enabled_mods(path, "Example", IsolationPolicy::Automatic).unwrap(),
        2
    );
    assert_eq!(
        enabled_mods(path, "Example", IsolationPolicy::Shared).unwrap(),
        1
    );
    assert_eq!(
        enabled_mods(path, "Example", IsolationPolicy::Isolated).unwrap(),
        2
    );
    assert!(enabled_mods(path, "../Example", IsolationPolicy::Automatic).is_err());
}

#[test]
fn unsafe_isolation_markers_and_mod_directories_do_not_silently_mean_zero_mods() {
    let (_fixture, _shared, _settings, root) = fixture();
    let path = Path::new(&root.path);
    let instance = path.join("versions/Example");
    fs::create_dir(path.join("foreign")).unwrap();
    symlink(path.join("foreign"), instance.join("mods")).unwrap();
    assert!(enabled_mods(path, "Example", IsolationPolicy::Shared).is_err());
    fs::remove_file(instance.join("mods")).unwrap();
    symlink(path.join("foreign"), path.join("mods")).unwrap();
    assert!(enabled_mods(path, "Example", IsolationPolicy::Shared).is_err());
}

#[test]
fn legacy_root_spellings_preview_the_same_mods_as_the_resolved_launch_root() {
    for alias in [false, true] {
        let fixture = Fixture::new();
        let canonical = fixture.0.join("Minecraft/.minecraft");
        fs::create_dir_all(canonical.join("versions/Example/mods")).unwrap();
        fs::write(
            canonical.join("versions/Example/mods/enabled.jar"),
            b"not opened",
        )
        .unwrap();
        let exact = if alias {
            let alias = fixture.0.join("root-alias");
            symlink(&canonical, &alias).unwrap();
            alias
        } else {
            canonical.join("../.minecraft")
        };
        fs::create_dir(fixture.0.join(".pcl-rust")).unwrap();
        fs::write(
            fixture.0.join(".pcl-rust/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "root": exact, "player": "Player", "memory_gib": 4,
                "selected": "Example", "overrides": {}
            }))
            .unwrap(),
        )
        .unwrap();
        let shared = fixture.shared();
        let mut settings = shared.config.snapshot();
        settings.memory_mode = MemoryMode::Auto;
        shared.config.save(settings).unwrap();
        let registered = shared
            .config
            .registered(&shared.config.snapshot().root_id)
            .unwrap();
        assert_eq!(Path::new(&registered.path), exact);
        let preview = preview_with(
            &shared,
            Some("Example"),
            Some(&registered.id),
            |settings, root, id, isolation| {
                evaluate_using(settings, root, id, || {
                    let mods = enabled_mods(Path::new(&root.path), id.unwrap(), isolation)?;
                    let (snapshot, _) = inputs()?;
                    Ok((snapshot, mods))
                })
            },
        )
        .unwrap();
        assert_eq!(preview.mod_count, Some(1));
        assert!(preview.error.is_none());
        let launch_root = shared.config.resolve(None).unwrap();
        assert_eq!(
            enabled_mods(
                Path::new(&launch_root.path),
                "Example",
                IsolationPolicy::Automatic
            )
            .unwrap(),
            preview.mod_count.unwrap()
        );
    }
}

#[test]
fn preview_retires_settings_and_close_changes_after_a_slow_read() {
    for change in ["settings", "close"] {
        let (_fixture, shared, _settings, root) = fixture();
        let result = preview_with(
            &shared,
            Some("Example"),
            Some(&root.id),
            |settings, root, id, _| {
                assert!(shared.operations.try_lock().is_ok());
                let result = evaluate_using(settings, root, id, inputs);
                match change {
                    "settings" => {
                        let mut next = shared.config.snapshot();
                        next.memory_mode = MemoryMode::Auto;
                        shared.config.save(next).unwrap();
                    }
                    _ => {
                        shared.close_generation.fetch_add(1, Ordering::SeqCst);
                    }
                }
                result
            },
        );
        // Manual preview does not open a game folder, but a changed registered
        // settings identity or close fence always invalidates its publication.
        assert!(result.is_err(), "{change}");
    }
}

#[test]
fn two_roots_with_the_same_instance_name_use_the_explicit_submission() {
    let (fixture, shared, _settings, first) = fixture();
    let second_path = fixture.0.join("second");
    fs::create_dir(&second_path).unwrap();
    let second = shared
        .config
        .register(second_path.display().to_string(), Some("Second".into()))
        .unwrap();
    let settings = shared.config.snapshot();
    let preview = preview_with(
        &shared,
        Some("Example"),
        Some(&second.id),
        |settings, root, id, _| {
            evaluate_using(settings, root, id, || {
                panic!("manual preview reads no folder")
            })
        },
    )
    .unwrap();
    assert_eq!(settings.root_id, first.id);
    assert_eq!(preview.root_id, second.id);
}
