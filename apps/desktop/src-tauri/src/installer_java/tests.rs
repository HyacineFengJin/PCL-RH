use super::*;
use crate::{
    integration_tests::Fixture,
    tasks::{TaskProgress, TaskScope, TaskStage, TaskTarget},
    RunStatus,
};
use pcl_core::java::JavaRuntime;
use sha1::{Digest, Sha1};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::atomic::{AtomicBool, AtomicUsize},
    time::Duration,
};

fn fixture() -> (Fixture, Shared, Context, TaskHandle) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../work/installer-java-2026-10-18/fixtures")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(path.join("Minecraft/.minecraft")).unwrap();
    let fixture = Fixture(path.canonicalize().unwrap());
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    let context = capture(&shared, &root, TaskKind::ModpackInstall);
    let scope = TaskScope::root(Path::new(&root.path))
        .unwrap()
        .union(TaskScope::managed_runtime_store(&shared.project).unwrap())
        .unwrap();
    let task = shared
        .tasks
        .admit_queued(
            TaskTarget {
                root_id: root.id,
                root_path: root.path,
                instance_id: Some("fixture".into()),
            },
            TaskKind::ModpackInstall,
            scope,
        )
        .unwrap();
    task.wait_turn().unwrap();
    (fixture, shared, context, task)
}

fn capture(shared: &Shared, root: &GameRoot, kind: TaskKind) -> Context {
    capture_named(shared, root, "fixture", kind)
}

fn capture_named(shared: &Shared, root: &GameRoot, id: &str, kind: TaskKind) -> Context {
    let _operation = shared.operations.lock().unwrap();
    Context::capture(
        shared,
        root,
        id,
        kind,
        Inputs {
            settings: shared.config.snapshot(),
            preferences_revision: shared.launcher_preferences.snapshot().revision,
            factory: pcl_network::snapshot(),
            scheduler: pcl_network::download_snapshot(),
        },
    )
    .unwrap()
}

fn requirement() -> Option<ProcessorJavaRequirement> {
    Some(ProcessorJavaRequirement {
        major: 8,
        exact: true,
    })
}

fn installer(context: &Context) -> Installer {
    Installer::from_factory(&context.inputs.factory)
        .unwrap()
        .with_download_policy(context.inputs.scheduler.clone())
}

fn installed(shared: &Shared) -> java_download::Installed {
    let path = shared
        .project
        .join(".pcl-rust/java/synthetic-java8/bin/java");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"synthetic; never executed").unwrap();
    java_download::Installed {
        path: path.clone(),
        runtime: JavaRuntime {
            path: path.display().to_string(),
            major: 8,
            vendor: "Fixture".into(),
            arch: std::env::consts::ARCH.into(),
        },
    }
}

#[test]
fn no_processors_do_not_discover_initialize_store_or_modify_task() {
    let (_fixture, shared, mut context, task) = fixture();
    let before = shared.config.snapshot();
    let runtime = shared.project.join(".pcl-rust/java");
    let installer = installer(&context);
    context
        .ensure_using(
            &shared,
            &task,
            installer,
            None,
            || panic!("no processor discovery"),
            |_, _| panic!("no processor download"),
        )
        .unwrap();
    assert!(!runtime.exists());
    assert_eq!(shared.config.snapshot(), before);
    assert_eq!(shared.tasks.list().len(), 1);
    assert!(shared.tasks.snapshot(task.id()).unwrap().can_cancel);
}

#[test]
fn exact_compatible_auto_java_does_not_download() {
    let (_fixture, shared, mut context, task) = fixture();
    let installer = installer(&context);
    context
        .ensure_using(
            &shared,
            &task,
            installer,
            requirement(),
            || {
                assert!(shared.operations.try_lock().is_ok());
                Ok(JavaCatalog {
                    runtimes: vec![JavaRuntime {
                        path: "/synthetic/java8".into(),
                        major: 8,
                        vendor: "Fixture".into(),
                        arch: std::env::consts::ARCH.into(),
                    }],
                    unavailable: vec![],
                })
            },
            |_, _| panic!("compatible Java needs no download"),
        )
        .unwrap();
    assert_eq!(context.network_bytes(), 0);
    assert!(!shared.project.join(".pcl-rust/java").exists());
}

#[test]
fn unavailable_managed_store_is_deferred_until_java_download_is_required() {
    for mode in ["none", "local", "download"] {
        let (_fixture, shared, _, task) = fixture();
        let external = shared.project.join("unrelated");
        fs::create_dir(&external).unwrap();
        fs::create_dir_all(shared.project.join(".pcl-rust")).unwrap();
        std::os::unix::fs::symlink(&external, shared.project.join(".pcl-rust/java")).unwrap();
        let root = shared.config.resolve(None).unwrap();
        let mut context = capture(&shared, &root, TaskKind::ModpackInstall);
        assert!(context.store_binding.is_err());
        let expected = context.store_binding.as_ref().err().unwrap().clone();
        let installer = installer(&context);
        let result = context.ensure_using(
            &shared,
            &task,
            installer,
            if mode == "none" { None } else { requirement() },
            || {
                Ok(JavaCatalog {
                    runtimes: if mode == "local" {
                        vec![JavaRuntime {
                            path: "/synthetic/java8".into(),
                            major: 8,
                            vendor: "Fixture".into(),
                            arch: std::env::consts::ARCH.into(),
                        }]
                    } else {
                        vec![]
                    },
                    unavailable: vec![],
                })
            },
            |captured, _| {
                let _store = captured
                    .store_binding
                    .as_ref()
                    .map_err(Clone::clone)?
                    .open()?;
                panic!("unsafe store must refuse before download")
            },
        );
        if mode == "download" {
            assert_eq!(result.err().unwrap(), expected);
        } else {
            assert!(result.is_ok(), "{mode}");
        }
        assert_eq!(fs::read_dir(external).unwrap().count(), 0);
    }
}

#[test]
fn newer_java_does_not_replace_exact_requirement_and_original_task_remains_cancellable() {
    let (_fixture, shared, mut context, task) = fixture();
    let before = shared.config.snapshot();
    let installer = installer(&context);
    context
        .ensure_using(
            &shared,
            &task,
            installer,
            requirement(),
            || {
                Ok(JavaCatalog {
                    runtimes: vec![JavaRuntime {
                        path: "/synthetic/java21".into(),
                        major: 21,
                        vendor: "Fixture".into(),
                        arch: std::env::consts::ARCH.into(),
                    }],
                    unavailable: vec![],
                })
            },
            |captured, required| {
                assert_eq!(
                    *required,
                    ProcessorJavaRequirement {
                        major: 8,
                        exact: true
                    }
                );
                assert!(shared.operations.try_lock().is_ok());
                captured.verify(&shared, &task)?;
                task.update(TaskProgress {
                    stage: TaskStage::Downloading,
                    phase: "java-files".into(),
                    message: "synthetic".into(),
                    completed: 1,
                    total: 1,
                    bytes_done: 123,
                    bytes_total: 123,
                    network_bytes: 123,
                    steps: vec![],
                });
                Ok(installed(&shared))
            },
        )
        .unwrap();
    assert_eq!(context.network_bytes(), 123);
    assert_eq!(shared.config.snapshot(), before);
    assert_eq!(shared.tasks.list().len(), 1);
    assert_eq!(
        shared.tasks.snapshot(task.id()).unwrap().kind,
        TaskKind::ModpackInstall
    );
    assert!(shared.tasks.snapshot(task.id()).unwrap().can_cancel);
    shared.tasks.cancel(task.id()).unwrap();
    assert!(task.cancellation_token().load(Ordering::SeqCst));
    assert_eq!(
        context.verify_fast(&shared, &task).unwrap_err(),
        crate::tasks::CANCELLED
    );
}

#[test]
fn explicit_manual_never_discovers_or_downloads_fallback() {
    let (_fixture, shared, _, task) = fixture();
    let mut settings = shared.config.snapshot();
    settings.java = JavaSelection::Manual {
        path: shared
            .project
            .join("missing-manual-java")
            .display()
            .to_string(),
    };
    shared.config.save(settings).unwrap();
    let root = shared.config.resolve(None).unwrap();
    let mut context = capture(&shared, &root, TaskKind::ModpackInstall);
    let installer = installer(&context);
    context
        .ensure_using(
            &shared,
            &task,
            installer,
            requirement(),
            || panic!("manual discovery"),
            |_, _| panic!("manual fallback"),
        )
        .unwrap();
    assert!(!shared.project.join(".pcl-rust/java").exists());
    assert!(matches!(
        shared.config.snapshot().java,
        JavaSelection::Manual { .. }
    ));
}

#[test]
fn accepted_cancel_blocks_discovery() {
    let (_fixture, shared, mut context, task) = fixture();
    shared.tasks.cancel(task.id()).unwrap();
    let installer = installer(&context);
    assert_eq!(
        context
            .ensure_using(
                &shared,
                &task,
                installer,
                requirement(),
                || panic!("cancelled discovery"),
                |_, _| panic!("cancelled download")
            )
            .err()
            .unwrap(),
        crate::tasks::CANCELLED
    );
}

#[test]
fn genuine_discovery_or_hash_error_is_not_overwritten_by_late_cancel() {
    for discovering in [true, false] {
        let (_fixture, shared, mut context, task) = fixture();
        let installer = installer(&context);
        let result = context.ensure_using(
            &shared,
            &task,
            installer,
            requirement(),
            || {
                if discovering {
                    shared.tasks.cancel(task.id()).unwrap();
                    Err("synthetic discovery failure".into())
                } else {
                    Ok(JavaCatalog::default())
                }
            },
            |_, _| {
                shared.tasks.cancel(task.id()).unwrap();
                Err("synthetic bad SHA1".into())
            },
        );
        assert_eq!(
            result.err().unwrap(),
            if discovering {
                "synthetic discovery failure"
            } else {
                "synthetic bad SHA1"
            }
        );
    }
}

#[test]
fn unrelated_settings_change_preserves_new_setting_and_continues_installation() {
    let (_fixture, shared, mut context, task) = fixture();
    let installer = installer(&context);
    let result = context.ensure_using(
        &shared,
        &task,
        installer,
        requirement(),
        || Ok(JavaCatalog::default()),
        |_, _| {
            let runtime = installed(&shared);
            let mut settings = shared.config.snapshot();
            settings.player = "ChangedPlayer".into();
            shared.config.save(settings)?;
            Ok(runtime)
        },
    );
    assert!(result.is_ok());
    assert_eq!(shared.config.snapshot().player, "ChangedPlayer");
    assert!(shared
        .project
        .join(".pcl-rust/java/synthetic-java8/bin/java")
        .is_file());
    assert!(shared.config.snapshot().java_paths.is_empty());
}

#[test]
fn unreadable_preferences_close_and_root_identity_changes_refuse_continuation() {
    for change in ["preferences", "close", "root"] {
        let (_fixture, shared, mut context, task) = fixture();
        let installer = installer(&context);
        let result = context.ensure_using(
            &shared,
            &task,
            installer,
            requirement(),
            || Ok(JavaCatalog::default()),
            |captured, _| {
                let runtime = installed(&shared);
                match change {
                    "preferences" => {
                        fs::write(
                            shared.project.join(".pcl-rust/launcher-preferences.json"),
                            b"{\"schema_version\":200}",
                        )
                        .unwrap();
                    }
                    "close" => {
                        shared.close_generation.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {
                        fs::rename(&captured.root.path, format!("{}-held", captured.root.path))
                            .unwrap();
                        fs::create_dir(&captured.root.path).unwrap();
                    }
                }
                Ok(runtime)
            },
        );
        assert!(result.is_err(), "{change}");
    }
}

#[test]
fn queued_processor_task_survives_prior_autoselection_and_unrelated_preferences() {
    for inactive_target in [false, true] {
        let (_fixture, shared, _, first) = fixture();
        let first_root = shared.config.resolve(None).unwrap();
        let root = if inactive_target {
            let path = shared.project.join("other-root");
            fs::create_dir(&path).unwrap();
            shared
                .config
                .register(path.display().to_string(), Some("Other".into()))
                .unwrap()
        } else {
            first_root.clone()
        };
        let context = capture_named(&shared, &root, "second", TaskKind::Install);
        let factory = context.inputs.factory.clone();
        let scheduler = context.inputs.scheduler.clone();
        let second = shared
            .tasks
            .admit_queued(
                TaskTarget {
                    root_id: root.id.clone(),
                    root_path: root.path.clone(),
                    instance_id: Some("second".into()),
                },
                TaskKind::Install,
                TaskScope::root(Path::new(&root.path))
                    .unwrap()
                    .union(TaskScope::managed_runtime_store(&shared.project).unwrap())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            shared.tasks.snapshot(second.id()).unwrap().stage,
            TaskStage::Queued
        );
        let old_revision = shared.config.snapshot().revision;
        shared
            .config
            .select_installed(&first_root.id, "installed-first")
            .unwrap();
        first.finish(crate::tasks::TaskOutcome::Complete {
            result: None,
            message: "synthetic first install".into(),
            error: None,
        });
        second.wait_turn().unwrap();
        assert_ne!(shared.config.snapshot().revision, old_revision);
        let prefs = shared.launcher_preferences.snapshot();
        shared
            .launcher_preferences
            .update(
                &prefs.revision,
                serde_json::from_value(
                    serde_json::json!({"auto_select_installed":false,"management":{"max_concurrent_transfers":8}}),
                )
                .unwrap(),
            )
            .unwrap();
        shared.config.select(&root.id).unwrap();
        let mut settings = shared.config.snapshot();
        settings.memory_gib = if settings.memory_gib == 4 { 6 } else { 4 };
        shared.config.save(settings).unwrap();
        context.verify_fast(&shared, &second).unwrap();
        context.verify(&shared, &second).unwrap();
        assert!(Arc::ptr_eq(&context.inputs.factory, &factory));
        assert!(Arc::ptr_eq(&context.inputs.scheduler, &scheduler));
    }
}

#[test]
fn effective_java_choice_or_registered_candidate_changes_revoke_processor_authority() {
    for change in ["global", "override", "paths"] {
        let (_fixture, shared, context, task) = fixture();
        let path = shared.project.join("explicit-java").display().to_string();
        match change {
            "paths" => {
                shared.config.register_downloaded_java(path).unwrap();
            }
            _ => {
                let mut settings = shared.config.snapshot();
                let manual = JavaSelection::Manual { path };
                if change == "global" {
                    settings.java = manual;
                } else {
                    settings.java_overrides.insert("fixture".into(), manual);
                }
                shared.config.save(settings).unwrap();
            }
        }
        assert_eq!(
            context.verify_fast(&shared, &task).unwrap_err(),
            CHANGED,
            "{change}"
        );
        assert!(context.verify(&shared, &task).is_err(), "{change}");
    }
}

#[test]
fn same_visible_status_does_not_adopt_a_new_monitor_generation() {
    let (_fixture, shared, context, task) = fixture();
    let original = shared.status.lock().unwrap().clone();
    task.finish(crate::tasks::TaskOutcome::Failed(
        "finished synthetic worker".into(),
    ));
    launcher_monitor_runtime::begin_preparation(
        &shared,
        RunStatus {
            stage: "preparing".into(),
            version: Some("other".into()),
            ..Default::default()
        },
    )
    .unwrap();
    *shared.status.lock().unwrap() = original;
    assert_eq!(
        context.verify_binding(&shared).unwrap_err(),
        "游戏启动状态已变化，请重新提交安装"
    );
}

#[test]
fn foreign_task_and_inactive_target_are_distinct_from_active_settings_root() {
    let (_fixture, shared, _, task) = fixture();
    let other_path = shared.project.join("inactive");
    fs::create_dir(&other_path).unwrap();
    let other = shared
        .config
        .register(other_path.display().to_string(), Some("Inactive".into()))
        .unwrap();
    let active = shared.config.snapshot().root_id;
    // Registry insertion does not authorize an existing task for this root.
    let context = capture(&shared, &other, TaskKind::ModpackInstall);
    assert_ne!(context.root.id, active);
    assert!(context.verify_fast(&shared, &task).is_err());
    assert_eq!(context.inputs.settings.root_id, active);
}

struct Loopback {
    origin: String,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Loopback {
    fn new(java: Vec<u8>) -> Self {
        let hash = |bytes: &[u8]| format!("{:x}", Sha1::digest(bytes));
        let java_hash = hash(&java);
        let manifest = serde_json::to_vec(&serde_json::json!({"files":{
            "bin":{"type":"directory"},
            "bin/java":{"type":"file","executable":true,"downloads":{"raw":{
                "url":format!("https://piston-data.mojang.com/v1/objects/{java_hash}/java"),
                "sha1":java_hash,"size":java.len()
            }}}
        }}))
        .unwrap();
        let digest = hash(&manifest);
        let platform = pcl_core::platform::Platform::current()
            .unwrap()
            .mojang_java_platform()
            .unwrap();
        let all = serde_json::to_vec(&serde_json::json!({platform:{"jre-legacy":[{
            "manifest":{"url":format!("https://piston-meta.mojang.com/v1/packages/{digest}/manifest.json"),"sha1":digest,"size":manifest.len()},
            "version":{"name":"1.8.0_451","released":"2026-10-18T00:00:00Z"}
        }]}})).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let (stopping, count) = (stop.clone(), requests.clone());
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut header = Vec::new();
                        let mut byte = [0];
                        while header.len() < 8192 && !header.ends_with(b"\r\n\r\n") {
                            if stream.read(&mut byte).unwrap_or(0) == 0 {
                                break;
                            }
                            header.push(byte[0]);
                        }
                        let text = String::from_utf8_lossy(&header);
                        let path = text.split_whitespace().nth(1).unwrap_or("");
                        count.fetch_add(1, Ordering::SeqCst);
                        let body = if path.ends_with("all.json") {
                            &all
                        } else if path.ends_with("manifest.json") {
                            &manifest
                        } else {
                            &java
                        };
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        Self {
            origin,
            stop,
            requests,
            worker: Some(worker),
        }
    }
}
impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn processor_java8_download_runs_actual_guarded_engine_in_original_modpack_task() {
    let (_fixture, shared, mut context, task) = fixture();
    context.inputs.factory = Arc::new(
        pcl_network::ClientFactory::new(pcl_network::Policy {
            proxy: pcl_network::ProxyPolicy::None,
            ..Default::default()
        })
        .unwrap(),
    );
    let before = shared.config.snapshot();
    let server = Loopback::new(format!("#!/bin/sh\nprintf 'java.specification.version = 8\\njava.vendor = Fixture JVM\\nos.arch = {}\\n' >&2\n", std::env::consts::ARCH).into_bytes());
    let installer = installer(&context);
    context
        .ensure_using(
            &shared,
            &task,
            installer,
            requirement(),
            || Ok(JavaCatalog::default()),
            |captured, required| {
                let store = captured
                    .store_binding
                    .as_ref()
                    .map_err(Clone::clone)?
                    .open()?;
                let mut http = java_download::http(
                    captured.inputs.factory.clone(),
                    captured.inputs.scheduler.clone(),
                )?;
                http.fixture = Some(server.origin.clone());
                tauri::async_runtime::block_on(java_download::install_recommended_guarded(
                    &store,
                    required.major,
                    http,
                    &task,
                    &|| captured.verify_fast(&shared, &task),
                    &|| captured.verify(&shared, &task),
                ))
            },
        )
        .unwrap();
    assert!(server.requests.load(Ordering::SeqCst) >= 4);
    let managed = pcl_core::java::managed_runtime_paths(&shared.project);
    assert_eq!(managed.len(), 1);
    assert!(managed[0].is_file());
    assert!(context.network_bytes() > 0);
    assert_eq!(shared.config.snapshot(), before);
    let own = shared.tasks.snapshot(task.id()).unwrap();
    assert_eq!(own.kind, TaskKind::ModpackInstall);
    assert!(own.can_cancel && !own.stage.is_terminal());
    assert!(own
        .steps
        .iter()
        .all(|step| !step.id.starts_with("java-") || step.state == "complete"));
    assert_eq!(shared.tasks.list().len(), 1);
    context.verify(&shared, &task).unwrap();
}
