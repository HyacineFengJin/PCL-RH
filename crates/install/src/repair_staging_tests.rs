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
        let log_before = fs::read(area_path(&temporary).join(LOG_NAME)).unwrap();
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
            assert_eq!(
                fs::read(area_path(&temporary).join(LOG_NAME)).unwrap(),
                log_before
            );
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
