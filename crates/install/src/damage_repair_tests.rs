//! Synthetic project-local files and loopback transport only. Durable states
//! reopen through the public recovery API, as after process or power loss.
use super::*;
use crate::repair::tests::{desc, fixture, installer, metadata, server};
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::sync::atomic::AtomicUsize;

fn capture(root: &Path) -> DamageRepair {
    DamageRepair::capture_bound(
        crate::native_port_tests::directory_file(root),
        root,
        "fixture",
    )
    .unwrap()
}
fn repair(installer: &Installer, request: &DamageRepair) -> DamageRepairReport {
    installer
        .repair_damaged(
            request,
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|_| {},
        )
        .unwrap()
}
fn recover(root: &Path) -> DamageRecoveryReport {
    DamageRepair::recover_pending_bound(
        crate::native_port_tests::directory_file(root),
        root,
        &AtomicBool::new(false),
        &|| Ok(()),
        &|| Ok(()),
        &|_| {},
    )
    .unwrap()
}

#[test]
fn absent_store_has_no_pending_operation_and_readiness_is_read_only() {
    let root = fixture();
    assert_eq!(DamageRepair::pending(root.path()).unwrap(), 0);
    DamageRepair::ensure_ready(root.path()).unwrap();
    assert!(!root.path().join(".pcl-linux").exists());
}

#[test]
fn bad_core_gets_verified_replacement_and_independent_persistent_backup() {
    let root = fixture();
    let server = server(vec![("/client", b"good client".to_vec())]);
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",b"good client")}}),
    );
    let path = root.path().join("versions/fixture/fixture.jar");
    fs::write(&path, b"damaged old jar").unwrap();
    let request = capture(root.path());
    let report = repair(&installer(&server), &request);
    assert_eq!(
        (
            report.files_repaired,
            report.files_already_valid,
            report.files_missing,
            report.unsupported_total
        ),
        (1, 0, 0, 0)
    );
    assert_eq!(fs::read(&path).unwrap(), b"good client");
    let backup = root.path().join(&report.backup_directory).join("backup-0");
    assert_eq!(fs::read(&backup).unwrap(), b"damaged old jar");
    assert_ne!(
        native_fs::metadata(&fs::File::open(&backup).unwrap())
            .unwrap()
            .identity,
        native_fs::metadata(&fs::File::open(&path).unwrap())
            .unwrap()
            .identity
    );
    assert_eq!(DamageRepair::pending(root.path()).unwrap(), 0);
    DamageRepair::ensure_ready(root.path()).unwrap();
    fs::write(path, b"later user edit").unwrap();
    assert_eq!(fs::read(backup).unwrap(), b"damaged old jar");
    assert_eq!(recover(root.path()).recovered, 0);
    assert_eq!(server.requests.load(Ordering::Relaxed), 1);
}

#[test]
fn valid_missing_untrusted_and_hardlinked_files_are_never_replaced() {
    let root = fixture();
    let server = server(Vec::new());
    let mut unknown = desc(&server, "/unknown", b"expected");
    unknown["path"] = "unknown.jar".into();
    unknown["url"] = "https://example.com/unknown.jar".into();
    let mut hard = desc(&server, "/hard", b"new");
    hard["path"] = "hard.jar".into();
    let mut absent = desc(&server, "/missing", b"missing");
    absent["path"] = "missing.jar".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",b"valid")},"libraries":[{"name":"org.example:unknown:1","downloads":{"artifact":unknown}},{"name":"org.example:hard:1","downloads":{"artifact":hard}},{"name":"org.example:missing:1","downloads":{"artifact":absent}}]}),
    );
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"valid").unwrap();
    fs::create_dir(root.path().join("libraries")).unwrap();
    fs::write(root.path().join("libraries/unknown.jar"), b"user bytes").unwrap();
    fs::write(root.path().join("libraries/hard.jar"), b"old").unwrap();
    fs::hard_link(
        root.path().join("libraries/hard.jar"),
        root.path().join("outside-alias"),
    )
    .unwrap();
    let report = repair(&installer(&server), &capture(root.path()));
    assert_eq!(
        (
            report.files_repaired,
            report.files_already_valid,
            report.files_missing,
            report.unsupported_total
        ),
        (0, 1, 1, 2)
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    assert_eq!(
        fs::read(root.path().join("libraries/unknown.jar")).unwrap(),
        b"user bytes"
    );
    assert_eq!(fs::read(root.path().join("outside-alias")).unwrap(), b"old");
    assert!(!root.path().join(store::STORE).exists());
}

#[test]
fn checksum_failure_and_download_cancellation_preserve_original_and_backup() {
    for cancel_download in [false, true] {
        let root = fixture();
        let payload = vec![b'g'; 262144];
        let server = server(vec![("/client", payload.clone())]);
        let mut descriptor = desc(&server, "/client", &payload);
        if !cancel_download {
            descriptor["sha1"] = "0000000000000000000000000000000000000000".into();
        }
        metadata(
            root.path(),
            "fixture",
            json!({"id":"fixture","downloads":{"client":descriptor}}),
        );
        let target = root.path().join("versions/fixture/fixture.jar");
        fs::write(&target, b"bad old file").unwrap();
        let request = capture(root.path());
        let cancel = AtomicBool::new(false);
        let result = installer(&server).repair_damaged(
            &request,
            &cancel,
            &|| Ok(()),
            &|| Ok(()),
            &|progress| {
                if cancel_download && progress.network_bytes > 0 {
                    cancel.store(true, Ordering::Release);
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(target).unwrap(), b"bad old file");
        assert_eq!(
            fs::read(
                root.path()
                    .join(store::STORE)
                    .join(request.operation_id())
                    .join("backup-0")
            )
            .unwrap(),
            b"bad old file"
        );
        assert_eq!(DamageRepair::pending(root.path()).unwrap(), 0);
    }
}

#[test]
fn concurrent_target_edit_during_download_is_preserved_without_overwrite() {
    let root = fixture();
    let payload = vec![b'g'; 262144];
    let server = server(vec![("/client", payload.clone())]);
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",&payload)}}),
    );
    let path = root.path().join("versions/fixture/fixture.jar");
    fs::write(&path, b"old bad file").unwrap();
    let request = capture(root.path());
    let changed = AtomicBool::new(false);
    let result = installer(&server).repair_damaged(
        &request,
        &AtomicBool::new(false),
        &|| Ok(()),
        &|| Ok(()),
        &|progress| {
            if progress.network_bytes > 0 && !changed.swap(true, Ordering::Relaxed) {
                fs::write(&path, b"concurrent user edit").unwrap();
            }
        },
    );
    assert!(result.is_err());
    assert_eq!(fs::read(path).unwrap(), b"concurrent user edit");
    assert_eq!(DamageRepair::pending(root.path()).unwrap(), 0);
}

fn staged(root: &Path) -> (Store, Operation, store::Intent) {
    fs::create_dir(root.join("libraries")).unwrap();
    fs::write(root.join("libraries/fixture.jar"), b"old damaged bytes").unwrap();
    let mut filesystem =
        RepairFilesystem::new(crate::native_port_tests::directory_file(root), root).unwrap();
    filesystem.inspect("libraries/fixture.jar").unwrap();
    let original = fingerprint(
        &mut store::open_regular(&filesystem.directories["libraries"], "fixture.jar").unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let mut storage = Store::open(
        crate::native_port_tests::directory_file(root),
        root,
        true,
        true,
    )
    .unwrap()
    .unwrap();
    let operation = storage.begin("d-a-b-c", "fixture").unwrap();
    let directory = operation.directory();
    for (name, bytes) in [
        ("backup-0", b"old damaged bytes".as_slice()),
        ("incoming-0", b"new verified bytes".as_slice()),
    ] {
        let mut file = RepairStage::new(
            directory.clone(),
            name,
            bytes.len() as u64,
            operation.staging_area(),
        )
        .unwrap();
        file.file_mut().write_all(bytes).unwrap();
        assert!(file.publish().unwrap());
    }
    let backup = fingerprint(
        &mut store::open_regular(&directory, "backup-0").unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let incoming = fingerprint(
        &mut store::open_regular(&directory, "incoming-0").unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let intent = operation
        .prepare(
            0,
            "libraries/fixture.jar",
            &filesystem,
            original,
            incoming,
            backup,
        )
        .unwrap();
    (storage, operation, intent)
}

#[test]
fn reopened_recovery_handles_intent_original_moved_and_replacement_published() {
    for stop in [0, 1, 2] {
        let root = fixture();
        let (mut storage, operation, intent) = staged(root.path());
        if stop > 0 {
            let calls = AtomicUsize::new(0);
            assert!(store::publish(&mut storage, &operation, &intent, &|| {
                if calls.fetch_add(1, Ordering::Relaxed) + 1 == stop {
                    Err("synthetic power interruption".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        }
        assert!(DamageRepair::ensure_ready(root.path()).is_err());
        DamageRepair::ensure_ready_except(root.path(), Some("d-a-b-c")).unwrap();
        drop(intent);
        drop(operation);
        drop(storage);
        assert_eq!(recover(root.path()).recovered, 1);
        assert_eq!(
            fs::read(root.path().join("libraries/fixture.jar")).unwrap(),
            b"old damaged bytes"
        );
        assert_eq!(
            fs::read(root.path().join(store::STORE).join("d-a-b-c/backup-0")).unwrap(),
            b"old damaged bytes"
        );
        assert_eq!(DamageRepair::pending(root.path()).unwrap(), 0);
        assert_eq!(recover(root.path()).recovered, 0);
    }
}

#[test]
fn reopened_committed_state_keeps_replacement_and_original_backup() {
    let root = fixture();
    let (mut storage, operation, intent) = staged(root.path());
    store::publish(&mut storage, &operation, &intent, &|| Ok(())).unwrap();
    drop(intent);
    drop(operation);
    drop(storage);
    assert_eq!(recover(root.path()).recovered, 1);
    assert_eq!(
        fs::read(root.path().join("libraries/fixture.jar")).unwrap(),
        b"new verified bytes"
    );
    assert_eq!(
        fs::read(root.path().join(store::STORE).join("d-a-b-c/backup-0")).unwrap(),
        b"old damaged bytes"
    );
}

#[test]
fn recovery_conflicts_and_tampered_records_preserve_every_file_and_guard() {
    for tamper in [false, true] {
        let root = fixture();
        let (mut storage, operation, intent) = staged(root.path());
        assert!(store::publish(&mut storage, &operation, &intent, &|| Err(
            "synthetic stop".into()
        ))
        .is_err());
        if tamper {
            fs::write(
                root.path().join(store::STORE).join("d-a-b-c/intent-0.json"),
                b"not JSON",
            )
            .unwrap();
        } else {
            fs::write(
                root.path().join("libraries/fixture.jar"),
                b"new concurrent file",
            )
            .unwrap();
        }
        drop(intent);
        drop(operation);
        drop(storage);
        assert!(DamageRepair::recover_pending_bound(
            crate::native_port_tests::directory_file(root.path()),
            root.path(),
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|_| {}
        )
        .is_err());
        assert!(DamageRepair::ensure_ready(root.path()).is_err());
        assert_eq!(
            fs::read(root.path().join(store::STORE).join("d-a-b-c/backup-0")).unwrap(),
            b"old damaged bytes"
        );
        assert_eq!(
            fs::read(root.path().join(store::STORE).join("d-a-b-c/original-0")).unwrap(),
            b"old damaged bytes"
        );
        if !tamper {
            assert_eq!(
                fs::read(root.path().join("libraries/fixture.jar")).unwrap(),
                b"new concurrent file"
            );
        }
    }
}

#[test]
fn empty_operation_creation_window_is_recoverable_without_deleting_files() {
    let root = fixture();
    fs::create_dir_all(root.path().join(store::STORE).join("d-a-b-c")).unwrap();
    assert_eq!(DamageRepair::pending(root.path()).unwrap(), 1);
    assert_eq!(recover(root.path()).recovered, 1);
    DamageRepair::ensure_ready(root.path()).unwrap();
}

#[test]
#[cfg(unix)]
fn root_parent_and_symlink_retarget_never_redirect_recovery() {
    for replacement in ["root", "parent", "link"] {
        let container = fixture();
        let root = container.path().join("root");
        fs::create_dir(&root).unwrap();
        let (mut storage, operation, intent) = staged(&root);
        assert!(store::publish(&mut storage, &operation, &intent, &|| Err(
            "synthetic stop".into()
        ))
        .is_err());
        if replacement == "root" {
            fs::rename(&root, container.path().join("saved-root")).unwrap();
            fs::create_dir(&root).unwrap();
        } else {
            fs::rename(
                root.join("libraries"),
                container.path().join("saved-libraries"),
            )
            .unwrap();
            if replacement == "link" {
                symlink(
                    container.path().join("saved-libraries"),
                    root.join("libraries"),
                )
                .unwrap();
            } else {
                fs::create_dir(root.join("libraries")).unwrap();
            }
        }
        assert!(store::recover(&mut storage, &operation, &|| Ok(())).is_err());
        assert!(!root.join("libraries/fixture.jar").exists());
    }
}

#[test]
fn damaged_index_is_replaced_before_trusted_new_objects_are_repaired() {
    let root = fixture();
    let object = b"verified asset";
    let hash = format!("{:x}", Sha1::digest(object));
    let index =
        serde_json::to_vec(&json!({"objects":{"logical":{"hash":hash,"size":object.len()}}}))
            .unwrap();
    let object_route = format!("/{}/{hash}", &hash[..2]);
    let server = server(vec![
        ("/index", index.clone()),
        (&object_route, object.to_vec()),
    ]);
    let mut index_descriptor = desc(&server, "/index", &index);
    index_descriptor["id"] = "fixture-index".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",b"valid")},"assetIndex":index_descriptor}),
    );
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"valid").unwrap();
    fs::create_dir_all(root.path().join("assets/indexes")).unwrap();
    fs::write(
        root.path().join("assets/indexes/fixture-index.json"),
        b"broken index JSON",
    )
    .unwrap();
    fs::create_dir_all(root.path().join(format!("assets/objects/{}", &hash[..2]))).unwrap();
    let target = root
        .path()
        .join(format!("assets/objects/{}/{hash}", &hash[..2]));
    fs::write(&target, b"bad asset").unwrap();
    let report = repair(&installer(&server), &capture(root.path()));
    assert_eq!(
        (
            report.files_repaired,
            report.files_already_valid,
            report.unsupported_total
        ),
        (2, 1, 0)
    );
    assert!(report.assets_enumerated);
    assert_eq!(fs::read(target).unwrap(), object);
    assert_eq!(
        fs::read(root.path().join("assets/indexes/fixture-index.json")).unwrap(),
        index
    );
    assert_eq!(
        fs::read(root.path().join(&report.backup_directory).join("backup-0")).unwrap(),
        b"broken index JSON"
    );
    assert_eq!(
        fs::read(root.path().join(&report.backup_directory).join("backup-1")).unwrap(),
        b"bad asset"
    );
}

#[test]
fn cancellation_after_one_commit_preserves_only_completed_repair_and_all_originals() {
    let root = fixture();
    let server = server(vec![("/a", b"new-a".to_vec()), ("/b", b"new-b".to_vec())]);
    let mut a = desc(&server, "/a", b"new-a");
    a["path"] = "a.jar".into();
    let mut b = desc(&server, "/b", b"new-b");
    b["path"] = "b.jar".into();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",b"valid")},"libraries":[{"name":"org.example:a:1","downloads":{"artifact":a}},{"name":"org.example:b:1","downloads":{"artifact":b}}]}),
    );
    fs::write(root.path().join("versions/fixture/fixture.jar"), b"valid").unwrap();
    fs::create_dir(root.path().join("libraries")).unwrap();
    fs::write(root.path().join("libraries/a.jar"), b"old-a").unwrap();
    fs::write(root.path().join("libraries/b.jar"), b"old-b").unwrap();
    let request = capture(root.path());
    let cancel = AtomicBool::new(false);
    let result =
        installer(&server).repair_damaged(&request, &cancel, &|| Ok(()), &|| Ok(()), &|progress| {
            if progress.completed == 1 {
                cancel.store(true, Ordering::Release);
            }
        });
    assert!(result.unwrap_err().contains("取消"));
    assert_eq!(
        fs::read(root.path().join("libraries/a.jar")).unwrap(),
        b"new-a"
    );
    assert_eq!(
        fs::read(root.path().join("libraries/b.jar")).unwrap(),
        b"old-b"
    );
    assert_eq!(
        fs::read(
            root.path()
                .join(store::STORE)
                .join(request.operation_id())
                .join("backup-0")
        )
        .unwrap(),
        b"old-a"
    );
    assert_eq!(DamageRepair::pending(root.path()).unwrap(), 0);
}

#[test]
fn oversized_or_unsafe_recovery_records_fail_before_any_target_move() {
    for unsafe_path in [false, true] {
        let root = fixture();
        let (storage, operation, intent) = staged(root.path());
        drop(intent);
        drop(operation);
        drop(storage);
        let path = root.path().join(store::STORE).join("d-a-b-c/intent-0.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if unsafe_path {
            value["path"] = "mods/user-owned.jar".into();
        } else {
            value["original"]["stamp"]["len"] = (manifest::MAX_REPAIR_FILE_BYTES + 1).into();
        }
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(DamageRepair::recover_pending_bound(
            crate::native_port_tests::directory_file(root.path()),
            root.path(),
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Ok(()),
            &|_| {}
        )
        .is_err());
        assert_eq!(
            fs::read(root.path().join("libraries/fixture.jar")).unwrap(),
            b"old damaged bytes"
        );
        assert!(!root.path().join("mods").exists());
    }
}

#[test]
fn healthy_existing_launcher_directory_and_unsafe_store_probes_are_read_only() {
    let root = fixture();
    fs::create_dir(root.path().join(".pcl-linux")).unwrap();
    fs::write(root.path().join(".pcl-linux/retained"), b"keep").unwrap();
    DamageRepair::ensure_ready(root.path()).unwrap();
    assert_eq!(DamageRepair::pending(root.path()).unwrap(), 0);
    let outside = fixture();
    fs::write(outside.path().join("retained"), b"outside").unwrap();
    #[cfg(unix)]
    {
        symlink(outside.path(), root.path().join(store::STORE)).unwrap();
        assert!(DamageRepair::ensure_ready(root.path()).is_err());
    }
    assert_eq!(
        fs::read(outside.path().join("retained")).unwrap(),
        b"outside"
    );
    assert_eq!(
        fs::read(root.path().join(".pcl-linux/retained")).unwrap(),
        b"keep"
    );
}

#[test]
fn backup_edited_after_publication_cannot_be_blessed_by_its_new_fingerprint() {
    let root = fixture();
    let server = server(vec![("/client", b"verified replacement".to_vec())]);
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":desc(&server,"/client",b"verified replacement")}}),
    );
    let target = root.path().join("versions/fixture/fixture.jar");
    fs::write(&target, b"original bad").unwrap();
    let request = capture(root.path());
    let backup = root
        .path()
        .join(store::STORE)
        .join(request.operation_id())
        .join("backup-0");
    let edited = AtomicBool::new(false);
    let result = installer(&server).repair_damaged(
        &request,
        &AtomicBool::new(false),
        &|| Ok(()),
        &|| Ok(()),
        &|progress| {
            if progress.stage == "damage-check"
                && backup.exists()
                && !edited.swap(true, Ordering::Relaxed)
            {
                // Same length isolates the content-fingerprint requirement.
                fs::write(&backup, b"modified bad").unwrap();
            }
        },
    );
    assert!(edited.load(Ordering::Relaxed));
    assert!(result.unwrap_err().contains("备份发布后内容变化"));
    assert_eq!(fs::read(target).unwrap(), b"original bad");
    assert_eq!(fs::read(backup).unwrap(), b"modified bad");
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    assert_eq!(DamageRepair::pending(root.path()).unwrap(), 0);
}
