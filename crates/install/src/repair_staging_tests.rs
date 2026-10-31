use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn fixture() -> (tempfile::TempDir, Directory, Directory) {
    let work =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/repair-staging-registry-tests");
    fs::create_dir_all(&work).unwrap();
    let temporary = tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap();
    let root = Directory::open_absolute(temporary.path()).unwrap();
    let target = root
        .create_private(&leaf("targets"), Traversal::SameFilesystem)
        .unwrap();
    (temporary, root, target)
}
fn leaf(name: &str) -> Leaf {
    Leaf::new(name).unwrap()
}
fn area_path(fixture: &tempfile::TempDir) -> PathBuf {
    fixture.path().join(".pcl-linux/dependency-staging")
}
fn published(outcome: Outcome) -> Receipt {
    match outcome {
        Outcome::Published { receipt } => receipt,
        Outcome::Exists { .. } => panic!("Unexpected existing target"),
        Outcome::MayHavePublished { error, receipt, .. } => {
            panic!("Uncertain publication: {error}; {receipt:?}")
        }
    }
}
fn write_ticket(area: &Area, target: &Directory, name: &str, bytes: &[u8]) -> Receipt {
    let mut ticket = area.allocate(target, &leaf(name), 4096).unwrap();
    ticket.file_mut().write_all(bytes).unwrap();
    published(ticket.publish(target, &leaf(name)).unwrap())
}
fn append_raw(area: &Area, bytes: &[u8]) {
    let mut state = area.state().unwrap();
    state.file.seek(SeekFrom::End(0)).unwrap();
    state.file.write_all(bytes).unwrap();
    state.file.sync_all().unwrap();
}
fn read_locked_log(area: &Area) -> Vec<u8> {
    // Windows' mandatory byte-range lock rejects a separately opened reader.
    // Read through the owning file object and restore its shared stream cursor.
    let mut state = area.state().unwrap();
    let position = state.file.stream_position().unwrap();
    state.file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    let result = state.file.read_to_end(&mut bytes);
    state.file.seek(SeekFrom::Start(position)).unwrap();
    result.unwrap();
    bytes
}

fn discard_area(root: &Directory, limits: Limits) -> (Directory, Area) {
    let context = root
        .create_private(&leaf("discard-context"), Traversal::SameFilesystem)
        .unwrap();
    let area = Area::open_at_private(&context, &leaf("import-staging"), true, limits).unwrap();
    (context, area)
}
fn discarded(result: io::Result<DiscardOutcome>) -> DiscardReceipt {
    match result.unwrap() {
        DiscardOutcome::Discarded { receipt } => receipt,
        DiscardOutcome::MayHaveDiscarded { error, receipt, .. } => {
            panic!("Discard uncertain: {error}; {receipt:?}")
        }
    }
}
fn crash_after_discard_intent(ticket: Ticket) -> (DiscardIntent, Option<OsString>) {
    let SealedTicket {
        sealed,
        allocation,
        active,
    } = ticket.seal().unwrap();
    let intent = DiscardIntent {
        allocation: allocation.clone(),
        evidence: sealed.evidence().clone(),
        metadata: sealed.metadata().clone(),
    };
    active
        .area
        .append(
            Record::DiscardIntent {
                ticket: allocation.ticket,
                intent: intent.clone(),
            },
            false,
        )
        .unwrap();
    let name = allocation.source_name;
    drop(sealed);
    drop(active);
    (intent, name)
}

#[test]
fn discarded_inputs_release_pending_quota_and_never_touch_allocated_target() {
    let (temporary, root, target) = fixture();
    let limits = Limits {
        pending_tickets: 1,
        ..Limits::default()
    };
    let (context, area) = discard_area(&root, limits.clone());
    let mut original = target.create_file(&leaf("existing-target")).unwrap();
    original.write_all(b"business original stays").unwrap();
    original.sync_all().unwrap();
    drop(original);
    for index in 0..8 {
        let mut ticket = area
            .allocate(&target, &leaf("existing-target"), 4096)
            .unwrap();
        ticket
            .file_mut()
            .write_all(format!("cancelled input {index}").as_bytes())
            .unwrap();
        let expected = ticket.allocation().source_identity.clone();
        let receipt = discarded(ticket.discard());
        assert_eq!(receipt.source.identity, expected);
        assert!(receipt.discard_confirmed && receipt.directory_synced);
        assert_eq!(receipt.source_residue, SourceResidue::None);
        assert_eq!(area.report().unwrap().pending_tickets, 0);
    }
    assert_eq!(area.report().unwrap().completed_tickets, 8);
    assert_eq!(
        area.directory().names(10).unwrap(),
        vec![OsString::from(LOG_NAME)]
    );
    let bytes = read_locked_log(&area);
    drop(area);
    let reopened = Area::open_at_private(&context, &leaf("import-staging"), false, limits).unwrap();
    assert_eq!(read_locked_log(&reopened), bytes);
    assert_eq!(reopened.report().unwrap().pending_tickets, 0);
    assert_eq!(reopened.reconcile_discarded().unwrap(), 0);
    assert_eq!(
        fs::read(temporary.path().join("targets/existing-target")).unwrap(),
        b"business original stays"
    );
    assert!(!temporary.path().join(".pcl-linux").exists());
}

#[test]
fn admitted_boundary_input_can_seal_discard_and_reopen_without_padding_deadlock() {
    let (_temporary, root, target) = fixture();
    let limits = Limits {
        log_bytes: 64 * 1024,
        area_bytes: 64 * 1024,
        record_bytes: 4096,
        pending_tickets: 1,
        ..Limits::default()
    };
    let (context, area) = discard_area(&root, limits.clone());
    let physical = area.report().unwrap().physical_bytes_lower_bound;
    let frame_bound = (FRAME_HEADER + limits.record_bytes) as u64;
    // The former one-padding formula left no room for sealing/discarding its
    // own admitted body. Reject this overcommit before creating any Stage.
    let former_max = limits.area_bytes - physical - frame_bound;
    assert!(area
        .allocate(&target, &leaf("overcommitted"), former_max)
        .is_err());
    assert_eq!(area.report().unwrap().pending_tickets, 0);
    assert_eq!(
        area.directory().names(10).unwrap(),
        vec![OsString::from(LOG_NAME)]
    );
    // Reserve the allocation and the longest valid continuation, then consume
    // every admitted payload byte. Actual frames spend that existing reserve.
    let max = limits.area_bytes - physical - 5 * frame_bound;
    assert!(max > 0 && max <= 64 * 1024);
    let mut ticket = area
        .allocate(&target, &leaf("read-only-input"), max)
        .unwrap();
    ticket
        .file_mut()
        .write_all(&vec![b'x'; max as usize])
        .unwrap();
    let held = area.report().unwrap();
    assert!(held.physical_bytes_lower_bound + held.reserved_bytes <= limits.area_bytes);
    discarded(ticket.discard());
    assert_eq!(area.report().unwrap().pending_tickets, 0);
    assert!(area.report().unwrap().physical_bytes_lower_bound <= limits.area_bytes);
    drop(area);
    let reopened = Area::open_at_private(&context, &leaf("import-staging"), false, limits).unwrap();
    assert_eq!(reopened.reconcile_discarded().unwrap(), 0);
    assert_eq!(reopened.report().unwrap().pending_tickets, 0);
    assert_eq!(reopened.report().unwrap().completed_tickets, 1);
    assert!(target.entry(&leaf("read-only-input")).unwrap().is_none());
}

#[test]
fn boundary_collision_ticket_can_still_discard_from_its_existing_reservation() {
    let (_temporary, root, target) = fixture();
    let limits = Limits {
        log_bytes: 64 * 1024,
        area_bytes: 64 * 1024,
        record_bytes: 4096,
        pending_tickets: 1,
        ..Limits::default()
    };
    let (_context, area) = discard_area(&root, limits.clone());
    let mut original = target.create_file(&leaf("collision")).unwrap();
    original.write_all(b"business original").unwrap();
    drop(original);
    let max = limits.area_bytes
        - area.report().unwrap().physical_bytes_lower_bound
        - 5 * (FRAME_HEADER + limits.record_bytes) as u64;
    let mut ticket = area.allocate(&target, &leaf("collision"), max).unwrap();
    ticket
        .file_mut()
        .write_all(&vec![b'y'; max as usize])
        .unwrap();
    let ticket = match ticket.publish(&target, &leaf("collision")).unwrap() {
        Outcome::Exists { ticket } => ticket,
        _ => panic!("collision did not preserve its sealed owner"),
    };
    discarded(ticket.discard());
    assert_eq!(area.report().unwrap().pending_tickets, 0);
    assert_eq!(area.report().unwrap().completed_tickets, 1);
    let mut original = target
        .open_regular(&leaf("collision"), Links::Single)
        .unwrap();
    let mut bytes = Vec::new();
    original.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"business original");
}

#[test]
fn durable_discard_intent_recovers_after_source_owner_exits_before_delete() {
    let (_temporary, root, target) = fixture();
    let (context, area) = discard_area(&root, Limits::default());
    let mut ticket = area
        .allocate(&target, &leaf("never-published"), 4096)
        .unwrap();
    ticket
        .file_mut()
        .write_all(b"interrupted discard bytes")
        .unwrap();
    let (intent, name) = crash_after_discard_intent(ticket);
    assert_eq!(area.report().unwrap().pending_tickets, 1);
    drop(area);
    let reopened =
        Area::open_at_private(&context, &leaf("import-staging"), false, Limits::default()).unwrap();
    assert_eq!(reopened.reconcile_discarded().unwrap(), 1);
    assert_eq!(reopened.report().unwrap().pending_tickets, 0);
    assert_eq!(reopened.report().unwrap().completed_tickets, 1);
    if let Some(name) = name {
        assert!(reopened
            .directory()
            .entry(&Leaf::new(name).unwrap())
            .unwrap()
            .is_none());
    }
    assert!(target.entry(&leaf("never-published")).unwrap().is_none());
    let state = reopened.state().unwrap();
    assert_eq!(
        state.entries[&intent.allocation.ticket]
            .discard_receipt
            .as_ref()
            .unwrap(),
        &discarded_receipt(&intent)
    );
}

#[test]
fn failed_discard_receipt_append_recovers_missing_source_only_with_its_intent() {
    let (_temporary, root, target) = fixture();
    let (context, area) = discard_area(&root, Limits::default());
    let mut ticket = area
        .allocate(&target, &leaf("not-a-publication"), 4096)
        .unwrap();
    ticket
        .file_mut()
        .write_all(b"finished read-only input")
        .unwrap();
    let name = ticket.allocation().source_name.clone();
    area.inner
        .fail_next_receipt_append
        .store(true, std::sync::atomic::Ordering::Release);
    match ticket.discard().unwrap() {
        DiscardOutcome::MayHaveDiscarded {
            ticket,
            receipt,
            error,
        } => {
            assert!(ticket.is_none());
            assert!(receipt.discard_confirmed && receipt.directory_synced);
            assert!(error.to_string().contains("fixture receipt append"));
        }
        _ => panic!("Missing discard receipt was reported as fully recorded"),
    }
    assert_eq!(area.report().unwrap().pending_tickets, 1);
    if let Some(name) = name {
        assert!(area
            .directory()
            .entry(&Leaf::new(name).unwrap())
            .unwrap()
            .is_none());
    }
    drop(area);
    let reopened =
        Area::open_at_private(&context, &leaf("import-staging"), false, Limits::default()).unwrap();
    assert_eq!(reopened.reconcile_discarded().unwrap(), 1);
    assert_eq!(reopened.report().unwrap().pending_tickets, 0);
    assert!(target.entry(&leaf("not-a-publication")).unwrap().is_none());
}

#[test]
fn source_changes_after_intent_preserve_original_owner_and_refuse_deletion() {
    let (_temporary, root, target) = fixture();
    let (_context, area) = discard_area(&root, Limits::default());
    let mut ticket = area.allocate(&target, &leaf("never-target"), 4096).unwrap();
    ticket
        .file_mut()
        .write_all(b"original source bytes")
        .unwrap();
    let name = ticket.allocation().source_name.clone();
    let mut outsider = ticket.file().try_clone().unwrap();
    *area.inner.after_discard_intent.lock().unwrap() = Some(Box::new(move || {
        outsider.seek(SeekFrom::Start(0)).unwrap();
        outsider.write_all(b"modified source bytes").unwrap();
        outsider.sync_all().unwrap();
    }));
    let retained = match ticket.discard().unwrap() {
        DiscardOutcome::MayHaveDiscarded {
            ticket: Some(ticket),
            receipt,
            error,
        } => {
            assert!(!receipt.discard_confirmed);
            assert!(error.to_string().contains("recheck failed"));
            ticket
        }
        _ => panic!("Modified stage was not preserved for inspection"),
    };
    assert_eq!(area.report().unwrap().pending_tickets, 1);
    assert!(area.report().unwrap().sources[0].active);
    assert!(area.report().unwrap().sources[0].discard_requested);
    assert_eq!(
        area.reconcile_discarded().unwrap(),
        0,
        "active original owner must exclude recovery"
    );
    if let Some(name) = name {
        assert!(area
            .directory()
            .entry(&Leaf::new(name).unwrap())
            .unwrap()
            .is_some());
    }
    assert!(target.entry(&leaf("never-target")).unwrap().is_none());
    drop(retained);
}

#[test]
fn discard_recovery_retains_allocations_without_intent_and_unregistered_sources() {
    let (_temporary, root, target) = fixture();
    let (context, area) = discard_area(&root, Limits::default());
    let mut orphan = Stage::new(area.directory()).unwrap();
    orphan.file_mut().write_all(b"unregistered orphan").unwrap();
    let orphan_name = orphan.source_name().cloned();
    drop(orphan);
    let mut ticket = area
        .allocate(&target, &leaf("cancelled-without-intent"), 4096)
        .unwrap();
    ticket
        .file_mut()
        .write_all(b"allocation has no discard authorization")
        .unwrap();
    let source_name = ticket.allocation().source_name.clone();
    drop(ticket);
    let before = read_locked_log(&area);
    drop(area);
    let reopened =
        Area::open_at_private(&context, &leaf("import-staging"), false, Limits::default()).unwrap();
    assert_eq!(reopened.reconcile_discarded().unwrap(), 0);
    assert_eq!(read_locked_log(&reopened), before);
    assert_eq!(reopened.report().unwrap().pending_tickets, 1);
    for name in orphan_name
        .into_iter()
        .chain(source_name.map(|name| Leaf::new(name).unwrap()))
    {
        assert!(reopened.directory().entry(&name).unwrap().is_some());
    }
}

#[test]
fn original_repair_namespace_cannot_write_discard_records() {
    let (_temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut ticket = area.allocate(&target, &leaf("repair-owned"), 4096).unwrap();
    ticket
        .file_mut()
        .write_all(b"old repair staging bytes")
        .unwrap();
    let before = read_locked_log(&area);
    assert!(ticket.discard().is_err());
    assert_eq!(read_locked_log(&area), before);
    assert!(area.reconcile_discarded().is_err());
    assert_eq!(read_locked_log(&area), before);
    assert_eq!(area.report().unwrap().frames, 2);
    assert!(target.entry(&leaf("repair-owned")).unwrap().is_none());
}

#[test]
fn legacy_header_serialization_stays_exact_without_null_or_discard_capability() {
    let (_temporary, root, _target) = fixture();
    let identity = root.identity().unwrap();
    let old_wire = format!(
        "{{\"kind\":\"header\",\"version\":1,\"area\":{}}}",
        serde_json::to_string(&identity).unwrap()
    );
    let record = Record::Header {
        version: VERSION,
        area: identity,
        discard_scope: None,
    };
    assert_eq!(serde_json::to_vec(&record).unwrap(), old_wire.as_bytes());
    assert!(matches!(
        serde_json::from_slice::<Record>(old_wire.as_bytes()).unwrap(),
        Record::Header {
            discard_scope: None,
            ..
        }
    ));
}

#[test]
fn renaming_complete_legacy_repair_area_to_custom_name_never_grants_discard() {
    let (_temporary, root, target) = fixture();
    let original = Area::open(&root, true).unwrap();
    write_ticket(
        &original,
        &target,
        "legacy-published",
        b"old format publication",
    );
    let identity = original.directory().identity().unwrap();
    let old_bytes = read_locked_log(&original);
    drop(original);
    let storage = root
        .child(&leaf(".pcl-linux"), Traversal::SameFilesystem)
        .unwrap();
    let context = root
        .create_private(&leaf("moved-context"), Traversal::SameFilesystem)
        .unwrap();
    pcl_core::file_ops::rename_noreplace_at(
        storage.file(),
        leaf("dependency-staging").as_os_str(),
        context.file(),
        leaf("renamed-custom-area").as_os_str(),
    )
    .unwrap();
    let moved = Area::open_at_private(
        &context,
        &leaf("renamed-custom-area"),
        true,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(moved.directory().identity().unwrap(), identity);
    assert_eq!(read_locked_log(&moved), old_bytes);
    assert!(moved.report().unwrap().writable());
    let mut input = moved
        .allocate(&target, &leaf("discard-must-not-publish"), 4096)
        .unwrap();
    input
        .file_mut()
        .write_all(b"legacy allocation remains legacy")
        .unwrap();
    let before_discard = read_locked_log(&moved);
    assert!(input.discard().is_err());
    assert_eq!(
        read_locked_log(&moved),
        before_discard,
        "discard must fail before seal or any new record"
    );
    assert!(moved.state().unwrap().discard_scope.is_none());
    assert!(moved.reconcile_discarded().is_err());
    assert_eq!(read_locked_log(&moved), before_discard);
    // An old Header remains usable for the original operation set. Moving it
    // back must not leave records that an old fixed repair reader cannot parse.
    write_ticket(
        &moved,
        &target,
        "still-legacy-publish",
        b"continued old operations",
    );
    let continued = read_locked_log(&moved);
    drop(moved);
    pcl_core::file_ops::rename_noreplace_at(
        context.file(),
        leaf("renamed-custom-area").as_os_str(),
        storage.file(),
        leaf("dependency-staging").as_os_str(),
    )
    .unwrap();
    let restored = Area::open(&root, false).unwrap();
    assert_eq!(restored.directory().identity().unwrap(), identity);
    assert_eq!(read_locked_log(&restored), continued);
    assert!(restored.report().unwrap().writable());
    assert_eq!(restored.report().unwrap().completed_tickets, 2);
    assert!(target
        .entry(&leaf("discard-must-not-publish"))
        .unwrap()
        .is_none());
}

#[test]
fn declared_custom_discard_scope_cannot_be_rebound_by_renaming_namespace() {
    let (_temporary, root, _target) = fixture();
    let (context, original) = discard_area(&root, Limits::default());
    let bytes = read_locked_log(&original);
    assert!(original.state().unwrap().discard_scope.is_some());
    drop(original);
    pcl_core::file_ops::rename_noreplace_at(
        context.file(),
        leaf("import-staging").as_os_str(),
        context.file(),
        leaf("different-name").as_os_str(),
    )
    .unwrap();
    let moved =
        Area::open_at_private(&context, &leaf("different-name"), true, Limits::default()).unwrap();
    assert!(matches!(
        moved.report().unwrap().status,
        LogStatus::Corrupt { .. }
    ));
    assert_eq!(read_locked_log(&moved), bytes);
    assert!(moved.reconcile_discarded().is_err());
}

#[test]
fn fixed_factory_and_its_custom_alias_reject_a_declared_discard_header() {
    let (_temporary, root, _target) = fixture();
    let (context, custom) = discard_area(&root, Limits::default());
    let bytes = read_locked_log(&custom);
    drop(custom);
    let storage = root
        .create_private(&leaf(".pcl-linux"), Traversal::SameFilesystem)
        .unwrap();
    pcl_core::file_ops::rename_noreplace_at(
        context.file(),
        leaf("import-staging").as_os_str(),
        storage.file(),
        leaf("dependency-staging").as_os_str(),
    )
    .unwrap();
    let fixed = Area::open(&root, false).unwrap();
    assert!(matches!(
        fixed.report().unwrap().status,
        LogStatus::Corrupt { .. }
    ));
    assert_eq!(read_locked_log(&fixed), bytes);
    drop(fixed);
    let alias = Area::open_at_private(
        &storage,
        &leaf("dependency-staging"),
        false,
        Limits::default(),
    )
    .unwrap();
    assert!(matches!(
        alias.report().unwrap().status,
        LogStatus::Corrupt { .. }
    ));
    assert_eq!(read_locked_log(&alias), bytes);
    assert!(alias.reconcile_discarded().is_err());
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn recovered_discard_refuses_changed_named_bytes_and_preserves_them() {
    let (temporary, root, target) = fixture();
    let (context, area) = discard_area(&root, Limits::default());
    let mut ticket = area
        .allocate(&target, &leaf("original-target"), 4096)
        .unwrap();
    ticket.file_mut().write_all(b"sealed input").unwrap();
    let (_intent, name) = crash_after_discard_intent(ticket);
    let name = Leaf::new(name.unwrap()).unwrap();
    let path = temporary
        .path()
        .join("discard-context/import-staging")
        .join(name.as_os_str());
    fs::write(&path, b"external changed bytes must remain").unwrap();
    drop(area);
    let reopened =
        Area::open_at_private(&context, &leaf("import-staging"), false, Limits::default()).unwrap();
    assert!(reopened.reconcile_discarded().is_err());
    assert_eq!(reopened.report().unwrap().pending_tickets, 1);
    assert_eq!(
        fs::read(path).unwrap(),
        b"external changed bytes must remain"
    );
    assert!(target.entry(&leaf("original-target")).unwrap().is_none());
}

#[test]
fn discard_receipt_capacity_is_checked_before_intent_or_source_removal() {
    let (_temporary, root, target) = fixture();
    // Header + Allocation + Sealed fit, but intent+receipt do not. Existing
    // named bytes must remain evidence instead of an unrecordable native discard.
    let limits = Limits {
        frames: 4,
        ..Limits::default()
    };
    let (_context, area) = discard_area(&root, limits);
    let mut ticket = area
        .allocate(&target, &leaf("unchanged-target"), 4096)
        .unwrap();
    ticket
        .file_mut()
        .write_all(b"retained on receipt budget exhaustion")
        .unwrap();
    let name = ticket.allocation().source_name.clone();
    assert!(ticket.discard().is_err());
    assert_eq!(area.report().unwrap().frames, 3);
    assert_eq!(area.report().unwrap().pending_tickets, 1);
    assert!(area
        .state()
        .unwrap()
        .entries
        .values()
        .all(|entry| entry.discard_intent.is_none()));
    if let Some(name) = name {
        assert!(area
            .directory()
            .entry(&Leaf::new(name).unwrap())
            .unwrap()
            .is_some());
    }
    assert!(target.entry(&leaf("unchanged-target")).unwrap().is_none());
}

#[test]
fn discard_recovery_hashes_full_source_in_addition_to_metadata() {
    let (_temporary, root, target) = fixture();
    let (_context, area) = discard_area(&root, Limits::default());
    let mut ticket = area
        .allocate(&target, &leaf("not-published"), 4096)
        .unwrap();
    ticket
        .file_mut()
        .write_all(b"same metadata but wrong recorded digest")
        .unwrap();
    let inspected = ticket.file().try_clone().unwrap();
    let sealed = ticket.seal().unwrap();
    let mut intent = DiscardIntent {
        allocation: sealed.allocation.clone(),
        evidence: sealed.sealed.evidence().clone(),
        metadata: sealed.sealed.metadata().clone(),
    };
    intent.evidence.sha256[0] ^= 0x80;
    intent.metadata = retained_fs::metadata(&inspected).unwrap();
    // The helper receives the actual current native metadata but must reject
    // the wrong digest. A metadata/ID-only cleanup would wrongly accept it.
    let error = check_discard_hash(&inspected, &intent);
    assert!(error.unwrap_err().to_string().contains("SHA-256"));
    assert!(target.entry(&leaf("not-published")).unwrap().is_none());
}

#[test]
fn custom_private_area_reuses_registry_and_borrowed_ticket_io_without_repair_pollution() {
    let (temporary, root, target) = fixture();
    let context = root
        .create_private(&leaf("app-import-context"), Traversal::SameFilesystem)
        .unwrap();
    let name = leaf("verified-import-staging");
    let area = Area::open_at_private(&context, &name, true, Limits::default()).unwrap();
    assert!(area.report().unwrap().writable());
    assert_eq!(area.report().unwrap().frames, 1);
    assert!(!temporary.path().join(".pcl-linux").exists());
    let area_identity = area.directory().identity().unwrap();
    let mut ticket = area.allocate(&target, &leaf("imported"), 4096).unwrap();
    ticket
        .file_mut()
        .write_all(b"verified import bytes")
        .unwrap();
    let expected = ticket.allocation().source_identity.clone();
    let mut reader = ticket.file();
    assert_eq!(retained_fs::metadata(reader).unwrap().identity, expected);
    reader.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"verified import bytes");
    let receipt = published(ticket.publish(&target, &leaf("imported")).unwrap());
    assert_eq!(receipt.source.parent_identity, area_identity);
    assert_eq!(receipt.source.identity, expected);
    let before = read_locked_log(&area);
    assert!(
        Area::open_at_private(&context, &name, false, Limits::default()).is_err(),
        "the same native registry lock must cover every factory"
    );
    drop(area);
    let reopened = Area::open_at_private(&context, &name, false, Limits::default()).unwrap();
    assert_eq!(read_locked_log(&reopened), before);
    assert_eq!(reopened.directory().identity().unwrap(), area_identity);
    assert_eq!(reopened.report().unwrap().completed_tickets, 1);
    assert_eq!(
        fs::read(temporary.path().join("targets/imported")).unwrap(),
        b"verified import bytes"
    );
    assert!(!temporary.path().join(".pcl-linux").exists());
}

#[test]
fn custom_factory_reopens_original_repair_namespace_with_identical_header_and_lock() {
    let (_temporary, root, target) = fixture();
    let original = Area::open(&root, true).unwrap();
    write_ticket(&original, &target, "repair-original", b"repair evidence");
    let storage = root
        .child(&leaf(".pcl-linux"), Traversal::SameFilesystem)
        .unwrap();
    let before = read_locked_log(&original);
    let identity = original.directory().identity().unwrap();
    assert!(Area::open_at_private(
        &storage,
        &leaf("dependency-staging"),
        false,
        Limits::default()
    )
    .is_err());
    drop(original);
    let custom = Area::open_at_private(
        &storage,
        &leaf("dependency-staging"),
        false,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(read_locked_log(&custom), before);
    assert_eq!(custom.directory().identity().unwrap(), identity);
    assert_eq!(custom.report().unwrap().completed_tickets, 1);
    drop(custom);
    let original = Area::open(&root, false).unwrap();
    assert_eq!(read_locked_log(&original), before);
    assert!(original.report().unwrap().writable());
}

#[test]
fn custom_factory_refuses_business_directories_without_creating_or_rewriting_registry() {
    let (temporary, root, _target) = fixture();
    let context = root
        .create_private(&leaf("private-context"), Traversal::SameFilesystem)
        .unwrap();
    let business = context
        .create_private(&leaf("business"), Traversal::SameFilesystem)
        .unwrap();
    let mut original = business.create_file(&leaf("user-original")).unwrap();
    original.write_all(b"original business bytes").unwrap();
    original.sync_all().unwrap();
    drop(original);
    assert!(Area::open_at_private(&context, &leaf("business"), true, Limits::default()).is_err());
    assert!(business.entry(&leaf(LOG_NAME)).unwrap().is_none());
    let path = temporary
        .path()
        .join("private-context/business/user-original");
    assert_eq!(fs::read(&path).unwrap(), b"original business bytes");
    let empty_log = business.create_file(&leaf(LOG_NAME)).unwrap();
    drop(empty_log);
    assert!(Area::open_at_private(&context, &leaf("business"), true, Limits::default()).is_err());
    assert_eq!(business.entry(&leaf(LOG_NAME)).unwrap().unwrap().len, 0);
    assert_eq!(fs::read(path).unwrap(), b"original business bytes");
}

#[test]
fn custom_factory_rejects_transplanted_header_without_reinitializing_it() {
    let (temporary, root, _target) = fixture();
    let context = root
        .create_private(&leaf("private-context"), Traversal::SameFilesystem)
        .unwrap();
    let first =
        Area::open_at_private(&context, &leaf("first-staging"), true, Limits::default()).unwrap();
    let bytes = read_locked_log(&first);
    drop(first);
    let second = context
        .create_private(&leaf("second-staging"), Traversal::SameFilesystem)
        .unwrap();
    let mut transplanted = second.create_file(&leaf(LOG_NAME)).unwrap();
    transplanted.write_all(&bytes).unwrap();
    transplanted.sync_all().unwrap();
    drop(transplanted);
    let checked =
        Area::open_at_private(&context, &leaf("second-staging"), true, Limits::default()).unwrap();
    assert!(matches!(
        checked.report().unwrap().status,
        LogStatus::Corrupt { .. }
    ));
    assert!(!checked.report().unwrap().writable());
    assert_eq!(read_locked_log(&checked), bytes);
    drop(checked);
    assert_eq!(
        fs::read(
            temporary
                .path()
                .join("private-context/second-staging/registry.log")
        )
        .unwrap(),
        bytes
    );
}

#[test]
fn allocation_is_durable_before_payload_and_drop_does_not_pollute_business_namespace() {
    let (_temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let storage = root
        .child(&leaf(".pcl-linux"), Traversal::SameFilesystem)
        .unwrap();
    let business = storage
        .create_private(&leaf("dependency-damage"), Traversal::SameFilesystem)
        .unwrap();
    let mut ticket = area.allocate(&target, &leaf("cancelled"), 4096).unwrap();
    let allocation = ticket.allocation().clone();
    assert_eq!(ticket.file_mut().metadata().unwrap().len(), 0);
    let report = area.report().unwrap();
    assert_eq!(report.frames, 2);
    assert_eq!(report.pending_tickets, 1);
    assert!(report.sources[0].active);
    ticket
        .file_mut()
        .write_all(b"retained cancelled payload")
        .unwrap();
    drop(ticket);
    assert!(business.names(10).unwrap().is_empty());
    assert!(target.entry(&leaf("cancelled")).unwrap().is_none());
    let report = area.report().unwrap();
    assert!(!report.sources[0].active);
    assert!(report.writable());
    if let Some(name) = &allocation.source_name {
        let mut file = area
            .directory()
            .open_regular(&Leaf::new(name).unwrap(), Links::Single)
            .unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"retained cancelled payload");
        assert_eq!(
            retained_fs::metadata(&file).unwrap().identity,
            allocation.source_identity
        );
    }
    drop(area);
    let reopened = Area::open(&root, false).unwrap();
    assert!(reopened.report().unwrap().writable());
    write_ticket(&reopened, &target, "later", b"later payload");
    assert!(business.names(10).unwrap().is_empty());
}

#[test]
fn sealed_evidence_and_publication_receipt_match_native_source_disappearance() {
    let (temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut ticket = area.allocate(&target, &leaf("published"), 4096).unwrap();
    let allocation = ticket.allocation().clone();
    ticket.file_mut().write_all(b"sealed payload").unwrap();
    let sealed = ticket.seal().unwrap();
    assert_eq!(area.report().unwrap().frames, 3);
    assert_eq!(sealed.evidence().identity, allocation.source_identity);
    let receipt = published(sealed.publish(&target, &leaf("published")).unwrap());
    assert!(receipt.publication_confirmed && receipt.directories_synced);
    assert_eq!(
        receipt.source.parent_identity,
        area.directory().identity().unwrap()
    );
    assert_eq!(receipt.source.identity, allocation.source_identity);
    if let Some(name) = &allocation.source_name {
        assert!(area
            .directory()
            .entry(&Leaf::new(name).unwrap())
            .unwrap()
            .is_none());
    }
    let report = area.report().unwrap();
    assert_eq!(
        (
            report.frames,
            report.completed_tickets,
            report.pending_tickets
        ),
        (4, 1, 0)
    );
    assert!(report.writable());
    assert_eq!(
        fs::read(temporary.path().join("targets/published")).unwrap(),
        b"sealed payload"
    );
    drop(area);
    assert_eq!(
        Area::open(&root, false)
            .unwrap()
            .report()
            .unwrap()
            .completed_tickets,
        1
    );
}

#[test]
fn unregistered_source_before_allocation_is_retained_and_does_not_permanently_block_writers() {
    let (temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut unregistered = Stage::new(area.directory()).unwrap();
    let orphan = match unregistered.source_name() {
        Some(name) => {
            let name = name.clone();
            unregistered
                .file_mut()
                .write_all(b"unregistered crash bytes")
                .unwrap();
            name
        }
        None => {
            // Linux's original anonymous backend has no named crash residue.
            // Use an actual private native unknown file to exercise inventory;
            // Darwin/Windows above exercise their real named Stage lease.
            let name = leaf("unregistered-without-a-reserved-prefix");
            area.directory()
                .create_file(&name)
                .unwrap()
                .write_all(b"unregistered crash bytes")
                .unwrap();
            name
        }
    };
    drop(unregistered);
    drop(area);
    let reopened = Area::open(&root, false).unwrap();
    let report = reopened.report().unwrap();
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.kind == IssueKind::UnregisteredSource
            && issue.name.as_deref() == Some(orphan.as_os_str())
            && !issue.blocks_writes));
    assert!(report.writable());
    write_ticket(
        &reopened,
        &target,
        "after-orphan",
        b"new registered payload",
    );
    assert_eq!(
        fs::read(area_path(&temporary).join(orphan.as_os_str())).unwrap(),
        b"unregistered crash bytes"
    );
}

#[test]
fn partial_receipt_tail_recovers_only_the_log_prefix_and_keeps_publication_uncertain() {
    let (temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut ticket = area
        .allocate(&target, &leaf("before-receipt-crash"), 4096)
        .unwrap();
    ticket
        .file_mut()
        .write_all(b"published before receipt crash")
        .unwrap();
    let SealedTicket {
        sealed,
        allocation,
        active,
    } = ticket.seal().unwrap();
    let receipt = match sealed
        .publish_new(&target, &leaf("before-receipt-crash"))
        .unwrap()
    {
        PublishOutcome::Published { receipt } => receipt,
        _ => panic!("Native publication did not complete"),
    };
    let (sequence, prefix) = {
        let state = area.state().unwrap();
        (state.next_sequence, state.valid_end)
    };
    let record = serde_json::to_vec(&Record::Receipt {
        ticket: allocation.ticket,
        receipt,
    })
    .unwrap();
    let frame = encode_frame(sequence, &record);
    append_raw(&area, &frame[..FRAME_HEADER + record.len() / 2]);
    drop(active);
    drop(area);
    let reopened = Area::open(&root, false).unwrap();
    assert!(matches!(
        reopened.report().unwrap().status,
        LogStatus::TruncatedTail { .. }
    ));
    assert!(reopened
        .allocate(&target, &leaf("blocked-before-recovery"), 1)
        .is_err());
    assert!(reopened.recover_tail().unwrap());
    let report = reopened.report().unwrap();
    assert_eq!(report.valid_prefix_bytes, prefix);
    assert_eq!(report.completed_tickets, 0);
    assert!(report.sources[0].publication_uncertain);
    assert!(!report.sources[0].has_receipt);
    assert!(report.writable());
    assert_eq!(
        fs::read(temporary.path().join("targets/before-receipt-crash")).unwrap(),
        b"published before receipt crash"
    );
    write_ticket(
        &reopened,
        &target,
        "after-tail-recovery",
        b"later registered payload",
    );
}

#[test]
fn actual_publish_receipt_io_failure_returns_uncertain_without_deleting_new_bytes() {
    let (temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut ticket = area
        .allocate(&target, &leaf("actual-publish"), 4096)
        .unwrap();
    let allocation = ticket.allocation().clone();
    ticket
        .file_mut()
        .write_all(b"actual native publication bytes")
        .unwrap();
    let sealed = ticket.seal().unwrap();
    let log_before = area.report().unwrap().log_bytes;
    area.inner
        .fail_next_receipt_append
        .store(true, std::sync::atomic::Ordering::Release);
    let receipt = match sealed.publish(&target, &leaf("actual-publish")).unwrap() {
        Outcome::MayHavePublished {
            ticket: None,
            receipt,
            error,
        } => {
            assert_eq!(error.to_string(), "fixture receipt append I/O failure");
            receipt
        }
        Outcome::MayHavePublished {
            ticket: Some(_),
            error,
            ..
        } => panic!("Native publish itself was uncertain: {error}"),
        _ => panic!("Receipt persistence failure was incorrectly returned as Published/Exists"),
    };
    assert!(receipt.publication_confirmed && receipt.directories_synced);
    assert_eq!(receipt.source_residue, SourceResidue::None);
    assert_eq!(receipt.source.identity, allocation.source_identity);
    assert_eq!(
        fs::read(temporary.path().join("targets/actual-publish")).unwrap(),
        b"actual native publication bytes"
    );
    if let Some(name) = allocation.source_name {
        assert!(area
            .directory()
            .entry(&Leaf::new(name).unwrap())
            .unwrap()
            .is_none());
    }
    let report = area.report().unwrap();
    assert_eq!(report.log_bytes, log_before);
    assert_eq!(
        (
            report.frames,
            report.completed_tickets,
            report.pending_tickets
        ),
        (3, 0, 1)
    );
    assert!(report.sources[0].publication_uncertain && !report.sources[0].has_receipt);
    assert!(!report.sources[0].active && report.writable());
    write_ticket(
        &area,
        &target,
        "after-receipt-error",
        b"independent later bytes",
    );
    assert_eq!(area.report().unwrap().completed_tickets, 1);
    assert_eq!(
        fs::read(temporary.path().join("targets/actual-publish")).unwrap(),
        b"actual native publication bytes"
    );
}

#[test]
fn publication_requires_room_for_its_encoded_result_before_touching_the_target() {
    for boundary in 0..3 {
        let (temporary, root, target) = fixture();
        let area = Area::open(&root, true).unwrap();
        let mut ticket = area
            .allocate(&target, &leaf("capacity-boundary"), 4096)
            .unwrap();
        ticket
            .file_mut()
            .write_all(b"retain source when receipt cannot fit")
            .unwrap();
        let mut retained = ticket.file_mut().try_clone().unwrap();
        let SealedTicket {
            sealed,
            allocation,
            mut active,
        } = ticket.seal().unwrap();
        let source_name = allocation.source_name.clone();
        let source_identity = allocation.source_identity.clone();
        let frame_bound = FRAME_HEADER as u64
            + publication_payload_bound(&allocation, sealed.evidence()).unwrap() as u64;
        let before = active.area.report().unwrap();
        let log_before = read_locked_log(&active.area);
        drop(area);
        // Configure the limit while this is the Area's sole owner. Existing
        // native bytes/stamps are untouched; use the actual sealed log length
        // so native name/identity encoding cannot make this test approximate.
        let room = match boundary {
            0 => FRAME_HEADER as u64 + 1,
            1 => frame_bound - 1,
            _ => frame_bound,
        };
        let limits = &mut Arc::get_mut(&mut active.area.inner).unwrap().limits;
        limits.log_bytes = before.log_bytes + room;
        limits.validate().unwrap();
        let area = active.area.clone();
        assert!(area.report().unwrap().writable());
        let ticket = SealedTicket {
            sealed,
            allocation,
            active,
        };
        let result = ticket.publish(&target, &leaf("capacity-boundary"));
        if boundary < 2 {
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("Insufficient result capacity must fail before publication"),
            };
            assert_eq!(
                error.to_string(),
                "Staging registry cannot fit the publication result; source retained"
            );
            assert!(target.entry(&leaf("capacity-boundary")).unwrap().is_none());
            assert_eq!(read_locked_log(&area), log_before);
            let report = area.report().unwrap();
            assert_eq!(report.frames, before.frames);
            assert_eq!(report.completed_tickets, 0);
            assert!(report.writable());
            if let Some(name) = source_name {
                let source = area
                    .directory()
                    .entry(&Leaf::new(name).unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(source.identity, source_identity);
                assert_eq!(
                    source.len,
                    b"retain source when receipt cannot fit".len() as u64
                );
            }
        } else {
            let receipt = published(result.unwrap());
            assert!(receipt.publication_confirmed && receipt.directories_synced);
            assert_eq!(
                fs::read(temporary.path().join("targets/capacity-boundary")).unwrap(),
                b"retain source when receipt cannot fit"
            );
            assert_eq!(area.report().unwrap().completed_tickets, 1);
        }
        retained.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        retained.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"retain source when receipt cannot fit");
    }
}

#[test]
fn receipt_append_rejects_old_prefix_mutation_before_accepting_its_new_stamp() {
    let (temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut retained = area
        .allocate(&target, &leaf("retained-unpublished"), 4096)
        .unwrap();
    retained
        .file_mut()
        .write_all(b"independent retained payload")
        .unwrap();
    let retained_allocation = retained.allocation().clone();
    let mut retained_file = retained.file_mut().try_clone().unwrap();
    drop(retained);
    fs::write(temporary.path().join("targets/original"), b"original bytes").unwrap();
    let mut ticket = area.allocate(&target, &leaf("prefix-race"), 4096).unwrap();
    ticket
        .file_mut()
        .write_all(b"published payload survives registry corruption")
        .unwrap();
    let sealed = ticket.seal().unwrap();
    let before = area.report().unwrap();
    let mut independent = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(area_path(&temporary).join(LOG_NAME))
        .unwrap();
    *area.inner.after_receipt_write.lock().unwrap() = Some(Box::new(move || {
        // Run a real second native writer after append/sync but BEFORE the
        // appender samples its new revision. Changing metadata here must not
        // be silently absorbed into the accepted post-append stamp. Offset
        // FRAME_HEADER is in the old header payload, outside Windows' first
        // byte coordination lock and outside the newly written receipt.
        std::thread::spawn(move || {
            independent
                .seek(SeekFrom::Start(FRAME_HEADER as u64))
                .unwrap();
            let mut byte = [0];
            independent.read_exact(&mut byte).unwrap();
            byte[0] ^= 1;
            independent
                .seek(SeekFrom::Start(FRAME_HEADER as u64))
                .unwrap();
            independent.write_all(&byte).unwrap();
            independent.sync_all().unwrap();
        })
        .join()
        .unwrap();
    }));
    let receipt = match sealed.publish(&target, &leaf("prefix-race")).unwrap() {
        Outcome::MayHavePublished {
            ticket: None,
            receipt,
            error,
        } => {
            assert_eq!(error.to_string(), "Registry append readback failed");
            receipt
        }
        _ => panic!("A changed old prefix must never produce Published/Exists"),
    };
    assert!(receipt.publication_confirmed && receipt.directories_synced);
    let report = area.report().unwrap();
    assert!(matches!(report.status, LogStatus::Corrupt { .. }));
    assert!(!report.writable());
    assert_eq!(report.frames, before.frames);
    assert_eq!(report.completed_tickets, 0);
    assert!(report.log_bytes > before.log_bytes);
    let length = report.log_bytes;
    assert!(area.allocate(&target, &leaf("refused"), 4096).is_err());
    assert!(area.recover_tail().is_err());
    assert_eq!(area.report().unwrap().log_bytes, length);
    assert!(target.entry(&leaf("refused")).unwrap().is_none());
    drop(area);
    let reopened = Area::open(&root, false).unwrap();
    assert!(matches!(
        reopened.report().unwrap().status,
        LogStatus::Corrupt { .. }
    ));
    assert!(!reopened.report().unwrap().writable());
    assert!(reopened
        .allocate(&target, &leaf("refused-after-reopen"), 4096)
        .is_err());
    assert_eq!(reopened.report().unwrap().log_bytes, length);
    assert_eq!(
        fs::read(temporary.path().join("targets/prefix-race")).unwrap(),
        b"published payload survives registry corruption"
    );
    assert_eq!(
        fs::read(temporary.path().join("targets/original")).unwrap(),
        b"original bytes"
    );
    retained_file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    retained_file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"independent retained payload");
    if let Some(name) = retained_allocation.source_name {
        assert_eq!(
            reopened
                .directory()
                .entry(&Leaf::new(name).unwrap())
                .unwrap()
                .unwrap()
                .identity,
            retained_allocation.source_identity
        );
    }
}

#[test]
fn checksum_error_and_wrong_partial_sequence_are_not_recoverable_tails() {
    for wrong_sequence in [false, true] {
        let (temporary, root, target) = fixture();
        let area = Area::open(&root, true).unwrap();
        let sequence = area.state().unwrap().next_sequence;
        let payload = serde_json::to_vec(&Record::Exists { ticket: 999 }).unwrap();
        let mut frame = encode_frame(sequence, &payload);
        if wrong_sequence {
            frame[8..16].copy_from_slice(&(sequence + 7).to_le_bytes());
            append_raw(&area, &frame[..16]);
        } else {
            *frame.last_mut().unwrap() ^= 1;
            append_raw(&area, &frame);
        }
        let length = fs::metadata(area_path(&temporary).join(LOG_NAME))
            .unwrap()
            .len();
        drop(area);
        let reopened = Area::open(&root, false).unwrap();
        assert!(matches!(
            reopened.report().unwrap().status,
            LogStatus::Corrupt { .. }
        ));
        assert!(reopened.recover_tail().is_err());
        assert!(reopened.allocate(&target, &leaf("blocked"), 1).is_err());
        assert_eq!(
            fs::metadata(area_path(&temporary).join(LOG_NAME))
                .unwrap()
                .len(),
            length
        );
    }
}

#[test]
fn duplicate_seal_records_are_preserved_and_fail_closed() {
    let (temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut ticket = area
        .allocate(&target, &leaf("duplicate-fixture"), 4096)
        .unwrap();
    ticket
        .file_mut()
        .write_all(b"retain duplicate evidence")
        .unwrap();
    let sealed = ticket.seal().unwrap();
    let record = Record::Sealed {
        ticket: sealed.allocation.ticket,
        evidence: sealed.evidence().clone(),
    };
    let sequence = area.state().unwrap().next_sequence;
    append_raw(
        &area,
        &encode_frame(sequence, &serde_json::to_vec(&record).unwrap()),
    );
    let length = fs::metadata(area_path(&temporary).join(LOG_NAME))
        .unwrap()
        .len();
    drop(sealed);
    drop(area);
    let reopened = Area::open(&root, false).unwrap();
    assert!(matches!(
        reopened.report().unwrap().status,
        LogStatus::Corrupt { .. }
    ));
    assert!(reopened.recover_tail().is_err());
    assert!(reopened.allocate(&target, &leaf("blocked"), 1).is_err());
    assert_eq!(
        fs::metadata(area_path(&temporary).join(LOG_NAME))
            .unwrap()
            .len(),
        length
    );
}

#[test]
fn duplicate_live_native_identity_and_complete_malformed_records_are_not_safe_tails() {
    for duplicate_identity in [true, false] {
        let (temporary, root, target) = fixture();
        let area = Area::open(&root, true).unwrap();
        let mut ticket = area.allocate(&target, &leaf("retained"), 4096).unwrap();
        ticket
            .file_mut()
            .write_all(b"retain original allocation")
            .unwrap();
        let sequence = area.state().unwrap().next_sequence;
        let payload = if duplicate_identity {
            let mut allocation = ticket.allocation().clone();
            allocation.ticket = sequence;
            if allocation.source_name.is_some() {
                allocation.source_name =
                    Some(".pcl-stage-00000000000000000000000000000000.lease".into());
            }
            serde_json::to_vec(&Record::Allocation { allocation }).unwrap()
        } else {
            br#"{"kind":"allocation","allocation":{"not":"an allocation"}}"#.to_vec()
        };
        if duplicate_identity && ticket.allocation().source_name.is_none() {
            // A live anonymous owner is observable only in this session. After
            // close/restart its historical ID must be allowed to be recycled.
            let record: Record = serde_json::from_slice(&payload).unwrap();
            assert!(validate_live_source_identity(
                &record,
                &area.state().unwrap(),
                area.directory()
            )
            .is_err());
            drop(ticket);
            drop(area);
            assert!(Area::open(&root, false)
                .unwrap()
                .report()
                .unwrap()
                .writable());
            continue;
        }
        append_raw(&area, &encode_frame(sequence, &payload));
        let length = fs::metadata(area_path(&temporary).join(LOG_NAME))
            .unwrap()
            .len();
        drop(ticket);
        drop(area);
        let reopened = Area::open(&root, false).unwrap();
        assert!(matches!(
            reopened.report().unwrap().status,
            LogStatus::Corrupt { .. }
        ));
        assert!(reopened.recover_tail().is_err());
        assert!(reopened.allocate(&target, &leaf("blocked"), 1).is_err());
        assert_eq!(
            fs::metadata(area_path(&temporary).join(LOG_NAME))
                .unwrap()
                .len(),
            length
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn real_anonymous_inode_allocation_loop_survives_drop_restart_and_any_native_reuse() {
    let (_temporary, root, target) = fixture();
    let mut area = Area::open(&root, true).unwrap();
    let mut seen = HashSet::new();
    let mut reused = 0usize;
    const SAMPLES: usize = 256;
    for sample in 0..SAMPLES {
        let ticket = area.allocate(&target, &leaf("never-published"), 0).unwrap();
        let native_identity = serde_json::to_vec(&ticket.allocation().source_identity).unwrap();
        if !seen.insert(native_identity) {
            reused += 1;
        }
        drop(ticket); // Actual O_TMPFILE last close, never a fabricated ID.
        if sample % 32 == 31 {
            drop(area);
            area = Area::open(&root, false).unwrap();
            assert!(area.report().unwrap().writable());
        }
    }
    assert_eq!(area.report().unwrap().frames, SAMPLES as u64 + 1);
    assert!(target.names(8).unwrap().is_empty());
    // Native allocation policy varies; every sample executes registration and
    // periodic replay. A filesystem need not recycle an inode in this bounded
    // window, and no test replaces an actual ID or fabricates a time generation.
    eprintln!("actual anonymous allocations={SAMPLES}, native ID reuses observed={reused}");
}

#[test]
fn an_incomplete_first_registry_header_can_be_recovered_without_touching_business_entries() {
    let (_temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let identity = area.directory().identity().unwrap();
    {
        let mut state = area.state().unwrap();
        let frame = encode_frame(
            0,
            &serde_json::to_vec(&Record::Header {
                version: VERSION,
                area: identity,
                discard_scope: None,
            })
            .unwrap(),
        );
        state.file.set_len(0).unwrap();
        state.file.seek(SeekFrom::Start(0)).unwrap();
        state.file.write_all(&frame[..16]).unwrap();
        state.file.sync_all().unwrap();
    }
    drop(area);
    let reopened = Area::open(&root, false).unwrap();
    assert!(matches!(
        reopened.report().unwrap().status,
        LogStatus::TruncatedTail { offset: 0, .. }
    ));
    assert!(reopened.recover_tail().unwrap());
    assert_eq!(reopened.report().unwrap().frames, 1);
    assert!(reopened.report().unwrap().writable());
    write_ticket(&reopened, &target, "after-header-recovery", b"new payload");
}

#[test]
fn tail_recovery_checks_the_entire_prefix_hash_under_the_original_log_lock() {
    let (temporary, root, _target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let sequence = area.state().unwrap().next_sequence;
    let frame = encode_frame(sequence, b"{}");
    append_raw(&area, &frame[..FRAME_HEADER + 1]);
    drop(area);
    let reopened = Area::open(&root, false).unwrap();
    let length = fs::metadata(area_path(&temporary).join(LOG_NAME))
        .unwrap()
        .len();
    {
        let mut state = reopened.state().unwrap();
        state
            .file
            .seek(SeekFrom::Start(FRAME_HEADER as u64))
            .unwrap();
        state.file.write_all(b"!").unwrap();
        state.file.sync_all().unwrap();
        // Fault injection independently exercises prefix SHA256 after the
        // earlier metadata CAS would normally reject this native revision.
        state.stamp = retained_fs::metadata(&state.file).unwrap();
    }
    assert!(reopened.recover_tail().is_err());
    assert_eq!(
        fs::metadata(area_path(&temporary).join(LOG_NAME))
            .unwrap()
            .len(),
        length
    );
}

#[test]
fn quota_rejects_new_work_without_truncation_and_terminal_tickets_do_not_use_pending_budget() {
    let (temporary, root, target) = fixture();
    let limits = Limits {
        log_bytes: 256 * 1024,
        frames: 256,
        record_bytes: 4096,
        area_bytes: 512 * 1024,
        pending_tickets: 1,
        entries: 8,
        issues: 16,
    };
    let area = Area::open_with_limits(&root, true, limits.clone()).unwrap();
    let mut ticket = area.allocate(&target, &leaf("first"), 4096).unwrap();
    assert!(area.allocate(&target, &leaf("too-many-active"), 1).is_err());
    ticket.file_mut().write_all(b"first payload").unwrap();
    published(ticket.publish(&target, &leaf("first")).unwrap());
    for index in 0..8 {
        write_ticket(&area, &target, &format!("terminal-{index}"), b"small bytes");
    }
    assert_eq!(area.report().unwrap().completed_tickets, 9);
    let names = area.directory().names(16).unwrap();
    assert!(area
        .allocate(&target, &leaf("too-many-bytes"), limits.area_bytes * 2)
        .is_err());
    assert_eq!(area.directory().names(16).unwrap(), names);
    let mut unknown = area
        .directory()
        .create_file(&leaf("large-unregistered"))
        .unwrap();
    unknown.write_all(b"preserve").unwrap();
    unknown.set_len(limits.area_bytes + 1).unwrap();
    drop(unknown);
    assert!(!area.report().unwrap().writable());
    assert!(area
        .allocate(&target, &leaf("blocked-by-bytes"), 1)
        .is_err());
    assert_eq!(
        fs::metadata(area_path(&temporary).join("large-unregistered"))
            .unwrap()
            .len(),
        limits.area_bytes + 1
    );
}

#[test]
fn exhausted_frame_budget_retains_allocation_without_writing_a_partial_seal() {
    let (_temporary, root, target) = fixture();
    let limits = Limits {
        frames: 2,
        ..Limits::default()
    };
    let area = Area::open_with_limits(&root, true, limits).unwrap();
    let mut ticket = area
        .allocate(&target, &leaf("quota-unpublished"), 4096)
        .unwrap();
    let name = ticket.allocation().source_name.clone();
    ticket
        .file_mut()
        .write_all(b"retain frame-quota bytes")
        .unwrap();
    let length = area.report().unwrap().log_bytes;
    assert!(ticket.seal().is_err());
    let report = area.report().unwrap();
    assert_eq!((report.frames, report.log_bytes), (2, length));
    assert_eq!(report.status, LogStatus::Ready);
    assert!(!report.writable());
    assert!(!report.sources[0].sealed && !report.sources[0].active);
    assert!(target.entry(&leaf("quota-unpublished")).unwrap().is_none());
    if let Some(name) = name {
        let mut file = area
            .directory()
            .open_regular(&Leaf::new(name).unwrap(), Links::Single)
            .unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"retain frame-quota bytes");
    }
}

#[test]
fn dropped_empty_allocations_do_not_permanently_reserve_their_unused_payload_maxima() {
    let (_temporary, root, target) = fixture();
    let limits = Limits {
        log_bytes: 16 * 1024,
        frames: 256,
        record_bytes: 2048,
        area_bytes: 32 * 1024,
        pending_tickets: 64,
        entries: 64,
        issues: 16,
    };
    let area = Area::open_with_limits(&root, true, limits).unwrap();
    for index in 0..6 {
        let ticket = area
            .allocate(&target, &leaf(&format!("cancelled-{index}")), 8 * 1024)
            .unwrap();
        assert_eq!(area.report().unwrap().reserved_bytes, 8 * 1024);
        drop(ticket);
        assert_eq!(area.report().unwrap().reserved_bytes, 0);
        assert!(area.report().unwrap().writable());
    }
    assert_eq!(area.report().unwrap().pending_tickets, 6);
    assert!(target.names(16).unwrap().is_empty());
    // Named leases remain present; releasing an unused reservation is not
    // deleting bytes or declaring cancellation/business recovery successful.
    #[cfg(any(target_os = "macos", windows))]
    assert_eq!(area.directory().names(16).unwrap().len(), 7);
}

#[test]
fn ticket_keeps_native_area_lock_and_another_writer_cannot_share_or_clean_it() {
    let (_temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut ticket = area.allocate(&target, &leaf("active"), 4096).unwrap();
    ticket.file_mut().write_all(b"active source").unwrap();
    drop(area);
    assert!(Area::open(&root, false).is_err());
    assert!(ticket.active.area.report().unwrap().sources[0].active);
    drop(ticket);
    let reopened = Area::open(&root, false).unwrap();
    assert!(!reopened.report().unwrap().sources[0].active);
    assert_eq!(reopened.report().unwrap().pending_tickets, 1);
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn replaced_named_source_is_reported_and_neither_original_nor_replacement_is_deleted() {
    let (temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut ticket = area.allocate(&target, &leaf("unpublished"), 4096).unwrap();
    ticket
        .file_mut()
        .write_all(b"original lease bytes")
        .unwrap();
    let allocation = ticket.allocation().clone();
    drop(ticket);
    let name = allocation.source_name.unwrap();
    fs::rename(
        area_path(&temporary).join(&name),
        area_path(&temporary).join("retained-original"),
    )
    .unwrap();
    area.directory()
        .create_file(&Leaf::new(&name).unwrap())
        .unwrap()
        .write_all(b"replacement bytes")
        .unwrap();
    let report = area.report().unwrap();
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.kind == IssueKind::ReplacedSource));
    assert!(report.writable());
    write_ticket(&area, &target, "independent-later", b"independent bytes");
    assert_eq!(
        fs::read(area_path(&temporary).join(&name)).unwrap(),
        b"replacement bytes"
    );
    assert_eq!(
        fs::read(area_path(&temporary).join("retained-original")).unwrap(),
        b"original lease bytes"
    );
}

#[test]
fn populated_unregistered_stage_cannot_be_retroactively_registered() {
    let (_temporary, root, target) = fixture();
    let area = Area::open(&root, true).unwrap();
    let mut stage = Stage::new(area.directory()).unwrap();
    stage
        .file_mut()
        .write_all(b"payload before registration")
        .unwrap();
    assert!(area
        .register(stage, &target, &leaf("must-not-register"), 4096)
        .is_err());
    assert_eq!(area.report().unwrap().frames, 1);
    write_ticket(
        &area,
        &target,
        "properly-allocated",
        b"allocated before writing",
    );
}
