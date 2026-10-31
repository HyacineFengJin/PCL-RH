use super::*;

#[test]
fn final_update_token_cancel_uses_the_real_finish_path_and_keeps_closed_window_conflicts() {
    let fixture = resource_ops::update_batch::tests::Fixture::new(false, true);
    let files = vec![fixture.file("mods", "fixture.jar", b"actual archive")];
    let tokens = files
        .iter()
        .map(|file| resource_ops::token_for(&file.file).unwrap())
        .collect::<Vec<_>>();
    for closed_window_conflict in [false, true] {
        let tasks = Arc::new(tasks::Tasks::new());
        let handle = tasks
            .admit(
                tasks::TaskTarget {
                    root_id: "fixture-root".into(),
                    root_path: "/fixture".into(),
                    instance_id: Some("ExampleGame".into()),
                },
                tasks::TaskKind::ResourceUpdate,
            )
            .unwrap();
        let id = handle.id().to_owned();
        tasks.cancel(&id).unwrap();
        let cancel = handle.cancellation_token();
        if closed_window_conflict {
            files[0].file.set_len(files[0].size - 1).unwrap();
        } else {
            resource_ops::update_batch::sources_unchanged(&files, &tokens, None).unwrap();
        }
        let error = resource_ops::update_batch::sources_unchanged(
            &files,
            &tokens,
            if closed_window_conflict {
                None
            } else {
                Some(&cancel)
            },
        )
        .unwrap_err();
        if !closed_window_conflict {
            assert_eq!(error, modrinth_install::CANCELLED);
        }
        finish(handle, Err(error.clone()), "complete");
        let actual = tasks.wait_terminal(&id).unwrap();
        if closed_window_conflict {
            assert_eq!(actual.stage, tasks::TaskStage::Error);
            assert_eq!(actual.error.as_deref(), Some(error.as_str()));
        } else {
            assert_eq!(actual.stage, tasks::TaskStage::Cancelled);
            assert!(actual.error.is_none());
        }
        assert!(tasks.active().is_none());
    }
}

#[cfg(windows)]
#[test]
fn windows_actual_final_update_token_read_cancel_finishes_as_cancelled() {
    let fixture = resource_ops::update_batch::tests::Fixture::new(false, true);
    let files = vec![fixture.file("mods", "fixture.jar", b"actual archive")];
    let tokens = vec![resource_ops::token_for(&files[0].file).unwrap()];
    let tasks = Arc::new(tasks::Tasks::new());
    let handle = tasks
        .admit(
            tasks::TaskTarget {
                root_id: "fixture-root".into(),
                root_path: "/fixture".into(),
                instance_id: Some("ExampleGame".into()),
            },
            tasks::TaskKind::ResourceUpdate,
        )
        .unwrap();
    let id = handle.id().to_owned();
    let cancel = handle.cancellation_token();
    assert!(!cancel.load(Ordering::Acquire));
    let after_read = tasks.clone();
    let cancelled_id = id.clone();
    resource_ops::after_token_read_for_test(move || {
        after_read.cancel(&cancelled_id).unwrap();
    });
    let error =
        resource_ops::update_batch::sources_unchanged(&files, &tokens, Some(&cancel)).unwrap_err();
    assert!(cancel.load(Ordering::Acquire));
    assert_eq!(error, modrinth_install::CANCELLED);
    finish(handle, Err(error), "complete");
    let actual = tasks.wait_terminal(&id).unwrap();
    assert_eq!(actual.stage, tasks::TaskStage::Cancelled);
    assert!(actual.error.is_none());
    assert!(tasks.active().is_none());
}

#[test]
fn dependency_failure_retains_traffic_and_error_after_cancel() {
    let tasks = Arc::new(tasks::Tasks::new());
    let handle = tasks
        .admit(
            tasks::TaskTarget {
                root_id: "fixture-root".into(),
                root_path: "/fixture".into(),
                instance_id: Some("ExampleGame".into()),
            },
            tasks::TaskKind::ResourceUpdate,
        )
        .unwrap();
    let id = handle.id().to_owned();
    transfer_progress(
        &handle,
        modrinth_install::DownloadProgress {
            phase: "metadata".into(),
            message: "checked".into(),
            completed: 0,
            total: 0,
            bytes_done: 0,
            bytes_total: 0,
            network_bytes: 4096,
        },
    );
    tasks.cancel(&id).unwrap();
    finish(
        handle,
        Err("Pinned installed dependency prevents replacement".into()),
        "complete",
    );
    let result = tasks.wait_terminal(&id).unwrap();
    assert_eq!(result.stage, tasks::TaskStage::Error);
    assert_eq!(result.network_bytes, 4096);
    assert!(result
        .error
        .unwrap()
        .contains("Pinned installed dependency"));
    assert!(tasks.active().is_none());
}

#[test]
fn accepted_cancel_before_restore_gate_keeps_admission_until_cleanup() {
    let tasks = Arc::new(tasks::Tasks::new());
    let handle = tasks
        .admit(
            tasks::TaskTarget {
                root_id: "fixture-root".into(),
                root_path: "/fixture".into(),
                instance_id: Some("ExampleGame".into()),
            },
            tasks::TaskKind::ResourceUpdateRestore,
        )
        .unwrap();
    let id = handle.id().to_owned();
    tasks.cancel(&id).unwrap();
    handle.begin_finishing();
    assert!(handle.cancellation_token().load(Ordering::SeqCst));
    assert!(tasks.active().is_some());
    finish(handle, Err(modrinth_install::CANCELLED.into()), "complete");
    let result = tasks.wait_terminal(&id).unwrap();
    assert_eq!(result.stage, tasks::TaskStage::Cancelled);
    assert!(result.error.is_none());
    assert!(result.message.contains("恢复记录已保留"));
    assert!(tasks.active().is_none());
}
