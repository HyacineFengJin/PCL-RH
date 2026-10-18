use super::*;
use crate::tasks::TaskStage;
use pcl_core::java::JavaRuntime;
use std::sync::Arc;

fn fixture(
    major: u32,
    exact: bool,
    manual: bool,
) -> (
    crate::integration_tests::Fixture,
    Arc<Shared>,
    PreparedLaunch,
    Settings,
) {
    let (fixture, shared, mut prepared) = super::super::tests::fixture(false, true);
    let path = Path::new(&prepared.root.path).join("versions/fixture/fixture.json");
    let mut data: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    data["javaVersion"] = serde_json::json!({"majorVersion":major});
    if exact {
        data["libraries"] =
            serde_json::json!([{"name":"net.minecraftforge:forge:1.12.2-14.23.5.2860"}]);
    }
    fs::write(path, data.to_string()).unwrap();
    let mut settings = shared.config.snapshot();
    if manual {
        settings.java = JavaSelection::Manual {
            path: fixture
                .0
                .join("missing-explicit-java")
                .display()
                .to_string(),
        };
        shared.config.save(settings).unwrap();
        settings = shared.config.snapshot();
        prepared.settings_revision = settings.revision.clone();
    }
    (fixture, shared, prepared, settings)
}

fn installed(shared: &Shared, major: u32) -> java_download::Installed {
    let path = shared
        .project
        .join(format!("synthetic-managed-{major}/bin/java"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"synthetic runtime; never executed").unwrap();
    java_download::Installed {
        path: path.clone(),
        runtime: JavaRuntime {
            path: path.display().to_string(),
            major,
            vendor: "Fixture".into(),
            arch: std::env::consts::ARCH.into(),
        },
    }
}

#[test]
fn auto_missing_java_uses_recommended_major_and_rebinds_only_its_registration() {
    let (_fixture, shared, mut prepared, settings) = fixture(8, true, false);
    let before = settings.revision.clone();
    let current = prepared
        .ensure_java_using(
            &shared,
            &settings,
            || Ok(JavaCatalog::default()),
            |captured, requirement, task| {
                assert_eq!((requirement.major, requirement.exact), (8, true));
                assert_eq!(
                    shared.tasks.snapshot(task.id()).unwrap().kind,
                    TaskKind::JavaInstall
                );
                assert!(shared.operations.try_lock().is_ok());
                captured.verify_java(&shared, task, requirement)?;
                Ok(installed(&shared, 8))
            },
        )
        .unwrap();
    assert_ne!(current.revision, before);
    assert_eq!(prepared.settings_revision, current.revision);
    assert_eq!(current.java, JavaSelection::Auto);
    assert_eq!(current.selected, settings.selected);
    assert_eq!(current.java_paths.len(), 1);
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Complete);
    prepared.verify(&shared, None).unwrap();
}

#[test]
fn manual_selection_never_discovers_or_downloads_a_fallback() {
    let (_fixture, shared, mut prepared, settings) = fixture(17, true, true);
    let current = prepared
        .ensure_java_using(
            &shared,
            &settings,
            || panic!("manual discovery"),
            |_, _, _| panic!("manual fallback"),
        )
        .unwrap();
    assert_eq!(current.java, settings.java);
    assert_eq!(current.revision, settings.revision);
    assert!(shared.tasks.list().is_empty());
}

#[test]
fn compatible_auto_runtime_skips_network_and_exact_loader_rejects_newer() {
    for exact in [false, true] {
        let (_fixture, shared, mut prepared, settings) = fixture(8, exact, false);
        let catalog = JavaCatalog {
            runtimes: vec![JavaRuntime {
                path: "/synthetic/java21".into(),
                major: 21,
                vendor: "Fixture".into(),
                arch: std::env::consts::ARCH.into(),
            }],
            unavailable: Vec::new(),
        };
        let current = prepared
            .ensure_java_using(
                &shared,
                &settings,
                || Ok(catalog),
                |_, requirement, _| {
                    assert!(exact);
                    Ok(installed(&shared, requirement.major))
                },
            )
            .unwrap();
        assert_eq!(current.java_paths.len(), usize::from(exact));
        assert_eq!(shared.tasks.list().len(), usize::from(exact));
    }
}

#[test]
fn concurrent_settings_edit_is_preserved_and_never_registered_or_rebound() {
    let (_fixture, shared, mut prepared, settings) = fixture(17, false, false);
    let old_revision = prepared.settings_revision.clone();
    let result = prepared.ensure_java_using(
        &shared,
        &settings,
        || Ok(JavaCatalog::default()),
        |_, _, _| {
            let mut concurrent = shared.config.snapshot();
            concurrent.memory_gib = 7;
            shared.config.save(concurrent).unwrap();
            Ok(installed(&shared, 17))
        },
    );
    assert!(result.is_err());
    assert_eq!(shared.config.snapshot().memory_gib, 7);
    assert!(shared.config.snapshot().java_paths.is_empty());
    assert_eq!(prepared.settings_revision, old_revision);
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Error);
}

#[test]
fn stop_cancel_and_real_download_error_keep_distinct_outcomes() {
    for error in [java_download::CANCELLED, "synthetic Java SHA1 failure"] {
        let (_fixture, shared, mut prepared, settings) = fixture(17, false, false);
        let result = prepared.ensure_java_using(
            &shared,
            &settings,
            || Ok(JavaCatalog::default()),
            |_, _, task| {
                launcher_monitor_runtime::request_stop(&shared).unwrap();
                assert!(task.cancellation_token().load(Ordering::SeqCst));
                Err(error.into())
            },
        );
        assert_eq!(result.unwrap_err(), error);
        assert_eq!(
            shared.tasks.list()[0].stage,
            if error == java_download::CANCELLED {
                TaskStage::Cancelled
            } else {
                TaskStage::Error
            }
        );
        assert!(shared.config.snapshot().java_paths.is_empty());
        assert!(prepared.authorize(&shared).is_err());
    }
}

#[test]
fn metadata_requirement_change_is_not_a_java_registration_authority() {
    let (_fixture, shared, mut prepared, settings) = fixture(17, false, false);
    let path = Path::new(&prepared.root.path).join("versions/fixture/fixture.json");
    let result = prepared.ensure_java_using(
        &shared,
        &settings,
        || Ok(JavaCatalog::default()),
        |_, _, _| {
            let mut data: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            data["javaVersion"] = serde_json::json!({"majorVersion":21});
            fs::write(&path, data.to_string()).unwrap();
            Ok(installed(&shared, 17))
        },
    );
    assert!(result.unwrap_err().contains("版本要求已变化"));
    assert!(shared.config.snapshot().java_paths.is_empty());
}

#[test]
fn catalog_failure_is_preserved_and_admit_listener_stop_cannot_install_java() {
    let (_fixture, shared, mut prepared, settings) = fixture(17, false, false);
    let result = prepared.ensure_java_using(
        &shared,
        &settings,
        || Err("synthetic catalog failure".into()),
        |_, _, _| panic!("catalog failed"),
    );
    assert_eq!(result.unwrap_err(), "synthetic catalog failure");
    assert!(shared.tasks.list().is_empty());
    let weak = Arc::downgrade(&shared);
    shared.tasks.set_listener(move |snapshot| {
        if snapshot.stage == TaskStage::Preparing {
            launcher_monitor_runtime::request_stop(&weak.upgrade().unwrap()).unwrap();
        }
    });
    let result = prepared.ensure_java_using(
        &shared,
        &settings,
        || Ok(JavaCatalog::default()),
        |_, _, _| panic!("cancelled before Java install"),
    );
    assert_eq!(result.unwrap_err(), crate::tasks::CANCELLED);
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Cancelled);
}

#[test]
fn java_registration_then_missing_repair_continue_the_same_generation() {
    let (_fixture, shared, mut prepared) = super::super::tests::fixture(true, true);
    let settings = shared.config.snapshot();
    let current = prepared
        .ensure_java_using(
            &shared,
            &settings,
            || Ok(JavaCatalog::default()),
            |_, requirement, _| Ok(installed(&shared, requirement.major)),
        )
        .unwrap();
    let output = Path::new(&prepared.root.path).join("libraries/fixture/file.jar");
    prepared
        .prepare_using(
            &shared,
            || {
                assert_eq!(current.java_paths.len(), 1);
                assert!(output.exists());
                Ok(())
            },
            |_, _, _| {
                fs::write(&output, b"fixture").unwrap();
                Ok(super::super::tests::repaired())
            },
        )
        .unwrap();
    let tasks = shared.tasks.list();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].kind, TaskKind::InstanceRepair);
    assert_eq!(tasks[1].kind, TaskKind::JavaInstall);
    assert!(tasks.iter().all(|task| task.stage == TaskStage::Complete));
    assert!(prepared.authorize(&shared).is_ok());
}

#[test]
fn java_publication_followed_by_stop_keeps_runtime_but_never_registers_or_launches() {
    let (_fixture, shared, mut prepared, settings) = fixture(17, false, false);
    let result = prepared.ensure_java_using(
        &shared,
        &settings,
        || Ok(JavaCatalog::default()),
        |_, _, _| {
            let runtime = installed(&shared, 17);
            launcher_monitor_runtime::request_stop(&shared).unwrap();
            Ok(runtime)
        },
    );
    assert_eq!(result.unwrap_err(), crate::tasks::CANCELLED);
    assert!(shared
        .project
        .join("synthetic-managed-17/bin/java")
        .exists());
    assert!(shared.config.snapshot().java_paths.is_empty());
    assert_eq!(shared.tasks.list()[0].stage, TaskStage::Cancelled);
    assert!(prepared.authorize(&shared).is_err());
}

#[test]
fn root_preferences_close_and_foreign_java_results_cannot_rebind_settings() {
    for change in ["root", "preferences", "close", "major"] {
        let (_fixture, shared, mut prepared, settings) = fixture(17, false, false);
        let root_path = prepared.root.path.clone();
        let result = prepared.ensure_java_using(
            &shared,
            &settings,
            || Ok(JavaCatalog::default()),
            |_, _, _| {
                match change {
                    "root" => {
                        fs::rename(&root_path, format!("{root_path}-old")).unwrap();
                        fs::create_dir(&root_path).unwrap();
                    }
                    "preferences" => {
                        let view = shared.launcher_preferences.snapshot();
                        shared
                            .launcher_preferences
                            .update(
                                &view.revision,
                                serde_json::from_value(
                                    serde_json::json!({"game_launch":{"custom_info":"changed"}}),
                                )
                                .unwrap(),
                            )
                            .unwrap();
                    }
                    "close" => {
                        shared.close_generation.fetch_add(2, Ordering::SeqCst);
                    }
                    _ => {}
                }
                Ok(installed(&shared, if change == "major" { 21 } else { 17 }))
            },
        );
        assert!(result.is_err(), "{change}");
        assert!(shared.config.snapshot().java_paths.is_empty(), "{change}");
        assert_eq!(shared.tasks.list()[0].stage, TaskStage::Error, "{change}");
        assert!(prepared.authorize(&shared).is_err(), "{change}");
    }
}
