use super::*;
use std::{fs, io::Write};

struct Fixture {
    area: Area,
    context: Directory,
    target: Directory,
    _temporary: tempfile::TempDir,
}
impl Fixture {
    fn new(limits: Limits) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let context = root
            .create_private(&leaf("context"), Traversal::SameFilesystem)
            .unwrap();
        let target = root
            .create_private(&leaf("target"), Traversal::SameFilesystem)
            .unwrap();
        let area = Area::open_at_private(&context, &leaf("batch-inputs"), true, limits).unwrap();
        Self {
            area,
            context,
            target,
            _temporary: temporary,
        }
    }
    fn requests(count: usize, max: u64) -> Vec<(Leaf, u64)> {
        (0..count)
            .map(|i| (leaf(&format!("output-{i}")), max))
            .collect()
    }
    fn allocate(&self, count: usize) -> Vec<Ticket> {
        let mut tickets = self
            .area
            .allocate_batch(
                &self.target,
                &Self::requests(count, 128),
                &AtomicBool::new(false),
            )
            .unwrap();
        for (index, ticket) in tickets.iter_mut().enumerate() {
            ticket
                .file_mut()
                .write_all(format!("real source {index}").as_bytes())
                .unwrap();
        }
        tickets
    }
}
fn leaf(name: &str) -> Leaf {
    Leaf::new(name).unwrap()
}
fn log(area: &Area) -> Vec<u8> {
    let mut state = area.state().unwrap();
    let position = state.file.stream_position().unwrap();
    state.file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    state.file.read_to_end(&mut bytes).unwrap();
    state.file.seek(SeekFrom::Start(position)).unwrap();
    bytes
}
struct Hook;
impl Hook {
    fn install(f: impl FnMut(&Area, Point, u64, Option<&File>) + 'static) -> Self {
        HOOK.with(|hook| {
            assert!(hook.borrow().is_none());
            *hook.borrow_mut() = Some(Box::new(f));
        });
        Self
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[test]
fn real_batch_uses_two_full_inventories_per_phase_and_reopens_old_wire() {
    let f = Fixture::new(Limits::default());
    INSPECTS.with(|count| count.set(0));
    let tickets = f.allocate(32);
    assert_eq!(INSPECTS.with(|count| count.get()), 2);
    let original_ids: Vec<_> = tickets
        .iter()
        .map(|ticket| ticket.allocation.source_identity.clone())
        .collect();
    INSPECTS.with(|count| count.set(0));
    let receipts = f
        .area
        .discard_batch(tickets, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(INSPECTS.with(|count| count.get()), 2);
    assert_eq!(receipts.len(), 32);
    for (receipt, id) in receipts.iter().zip(original_ids) {
        assert_eq!(receipt.source.identity, id);
        assert!(receipt.discard_confirmed && receipt.directory_synced);
    }
    let report = f.area.report().unwrap();
    assert_eq!(
        (
            report.pending_tickets,
            report.completed_tickets,
            report.frames
        ),
        (0, 32, 129)
    );
    assert!(f.target.names(1).unwrap().is_empty());
    let bytes = log(&f.area);
    let Fixture {
        area,
        context,
        target: _,
        _temporary,
    } = f;
    drop(area);
    let reopened =
        Area::open_at_private(&context, &leaf("batch-inputs"), false, Limits::default()).unwrap();
    assert_eq!(log(&reopened), bytes);
    assert_eq!(reopened.report().unwrap().pending_tickets, 0);
    assert_eq!(reopened.reconcile_discarded().unwrap(), 0);
    drop(reopened);
    drop(context);
    drop(_temporary);
}

#[test]
fn aggregate_quota_refuses_before_the_first_real_stage() {
    let f = Fixture::new(Limits {
        log_bytes: 65536,
        area_bytes: 65536,
        record_bytes: 4096,
        ..Limits::default()
    });
    let before = log(&f.area);
    let failure = f
        .area
        .allocate_batch(
            &f.target,
            &Fixture::requests(2, 40000),
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
    assert!(!failure.not_eligible);
    assert!(failure.allocated.is_empty() && failure.retained_stage.is_none());
    assert_eq!(
        f.area.directory().names(2).unwrap(),
        vec![OsString::from(LOG_NAME)]
    );
    assert_eq!(log(&f.area), before);
}

#[test]
fn aggregate_reservation_boundary_can_really_seal_discard_and_reopen() {
    let limits = Limits {
        log_bytes: 65536,
        area_bytes: 65536,
        record_bytes: 4096,
        ..Limits::default()
    };
    let f = Fixture::new(limits);
    let physical = f.area.report().unwrap().physical_bytes_lower_bound;
    let per_source = (65536 - physical) / 2 - 5 * (FRAME_HEADER + 4096) as u64;
    let mut tickets = f
        .area
        .allocate_batch(
            &f.target,
            &Fixture::requests(2, per_source),
            &AtomicBool::new(false),
        )
        .unwrap();
    for ticket in &mut tickets {
        ticket
            .file_mut()
            .write_all(&vec![b'x'; per_source as usize])
            .unwrap();
    }
    assert_eq!(
        f.area
            .discard_batch(tickets, &AtomicBool::new(false))
            .unwrap()
            .len(),
        2
    );
    assert_eq!(f.area.report().unwrap().pending_tickets, 0);
}

#[test]
fn count_quota_and_cancellation_before_creation_leave_only_old_log() {
    let f = Fixture::new(Limits {
        pending_tickets: 1,
        ..Limits::default()
    });
    let before = log(&f.area);
    let failure = f
        .area
        .allocate_batch(
            &f.target,
            &Fixture::requests(2, 128),
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
    assert!(failure.allocated.is_empty() && failure.retained_stage.is_none());
    let failure = f
        .area
        .allocate_batch(
            &f.target,
            &Fixture::requests(1, 128),
            &AtomicBool::new(true),
        )
        .err()
        .unwrap();
    assert_eq!(failure.error.kind(), io::ErrorKind::Interrupted);
    assert!(failure.allocated.is_empty() && failure.retained_stage.is_none());
    assert_eq!(log(&f.area), before);
}

#[test]
fn partial_allocation_cancel_returns_original_owners_without_mutex_reentry() {
    let f = Fixture::new(Limits::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = cancel.clone();
    let hook = Hook::install(move |_, point, _, _| {
        if point == Point::Allocated {
            trigger.store(true, Ordering::Release);
        }
    });
    let failure = f
        .area
        .allocate_batch(&f.target, &Fixture::requests(3, 128), &cancel)
        .err()
        .unwrap();
    drop(hook);
    assert_eq!(failure.error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(failure.allocated.len(), 1);
    assert!(failure.retained_stage.is_none());
    cancel.store(false, Ordering::Release);
    assert_eq!(
        f.area
            .discard_batch(failure.allocated, &cancel)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(f.area.report().unwrap().pending_tickets, 0);
}

#[test]
fn unknown_mid_batch_entry_is_preserved_with_actual_allocated_and_unregistered_owners() {
    let f = Fixture::new(Limits::default());
    let mut injected = false;
    let hook = Hook::install(move |area, point, _, _| {
        if point == Point::Allocated && !injected {
            injected = true;
            let mut file = area
                .directory()
                .create_file(&leaf("foreign-literal"))
                .unwrap();
            file.write_all(b"unknown original").unwrap();
            file.sync_all().unwrap();
        }
    });
    let failure = f
        .area
        .allocate_batch(
            &f.target,
            &Fixture::requests(3, 128),
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
    drop(hook);
    assert_eq!(failure.allocated.len(), 1);
    assert!(failure.retained_stage.is_some());
    assert!(failure.uncertain_allocation.is_none());
    let mut bytes = Vec::new();
    f.area
        .directory()
        .open_regular(&leaf("foreign-literal"), Links::Single)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"unknown original");
    let report = failure.report.as_ref().unwrap();
    assert!(report.issue_count > 0);
    assert!(report.issues.iter().any(|issue| {
        issue.kind == IssueKind::UnregisteredSource
            && issue.name.as_deref() == Some(std::ffi::OsStr::new("foreign-literal"))
    }));
    assert!(f.target.names(1).unwrap().is_empty());
}

#[test]
fn final_native_source_recheck_rejects_changed_bytes_even_with_remaining_quota() {
    let f = Fixture::new(Limits::default());
    let mut changed = false;
    let hook = Hook::install(move |_, point, _, file| {
        if point == Point::Allocated && !changed {
            changed = true;
            let mut file = file.unwrap();
            file.write_all(b"external bytes").unwrap();
            file.sync_all().unwrap();
        }
    });
    let failure = f
        .area
        .allocate_batch(
            &f.target,
            &Fixture::requests(2, 128),
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
    drop(hook);
    assert_eq!(failure.allocated.len(), 2);
    assert!(failure.error.to_string().contains("changed"));
    assert!(failure.report.as_ref().unwrap().writable());
    assert!(f.target.names(1).unwrap().is_empty());
}

#[test]
fn batch_append_still_reads_the_actual_entire_old_prefix() {
    let f = Fixture::new(Limits::default());
    let hook = Hook::install(|_, point, _, file| {
        if point == Point::FrameWritten {
            let mut file = file.unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(b"X").unwrap();
            file.sync_all().unwrap();
        }
    });
    let failure = f
        .area
        .allocate_batch(
            &f.target,
            &Fixture::requests(1, 128),
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
    drop(hook);
    assert!(failure.allocated.is_empty());
    assert!(failure.retained_stage.is_some() && failure.uncertain_allocation.is_some());
    assert!(matches!(
        f.area.report().unwrap().status,
        LogStatus::Corrupt { .. }
    ));
    assert!(failure.error.to_string().contains("readback"));
    assert!(f.target.names(1).unwrap().is_empty());
}

#[test]
fn ineligible_partial_owner_set_is_returned_unchanged_for_the_old_single_path() {
    let f = Fixture::new(Limits::default());
    let mut tickets = f.allocate(2);
    let other = tickets.pop().unwrap();
    let before = log(&f.area);
    let failure = f
        .area
        .discard_batch(tickets, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(failure.not_eligible && failure.discarded.is_empty() && failure.uncertain.is_none());
    assert_eq!(failure.remaining.len(), 1);
    assert_eq!(log(&f.area), before);
    for owner in failure.remaining {
        let outcome = match owner {
            BatchOwner::Ticket(ticket) => ticket.discard().unwrap(),
            BatchOwner::Sealed(_) => panic!("unchanged writable owner expected"),
        };
        assert!(matches!(outcome, DiscardOutcome::Discarded { .. }));
    }
    assert!(matches!(
        other.discard().unwrap(),
        DiscardOutcome::Discarded { .. }
    ));
}

#[test]
fn pending_orphan_and_foreign_area_never_gain_batch_authority() {
    let f = Fixture::new(Limits::default());
    let mut tickets = f.allocate(1);
    drop(tickets.pop());
    let before = log(&f.area);
    let failure = f
        .area
        .allocate_batch(
            &f.target,
            &Fixture::requests(1, 128),
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
    assert!(
        failure.not_eligible && failure.allocated.is_empty() && failure.retained_stage.is_none()
    );
    assert_eq!(log(&f.area), before);
    let other = Fixture::new(Limits::default());
    let foreign = other.allocate(1);
    let failure = f
        .area
        .discard_batch(foreign, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(failure.not_eligible && failure.remaining.len() == 1);
    assert_eq!(other.area.report().unwrap().pending_tickets, 1);
}

#[test]
fn discard_cancel_is_between_tickets_and_returns_the_unprocessed_real_owners() {
    let f = Fixture::new(Limits::default());
    let tickets = f.allocate(3);
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = cancel.clone();
    let hook = Hook::install(move |_, point, _, _| {
        if point == Point::Discarded {
            trigger.store(true, Ordering::Release);
        }
    });
    let failure = f.area.discard_batch(tickets, &cancel).err().unwrap();
    drop(hook);
    assert_eq!(failure.discarded.len(), 1);
    assert_eq!(failure.remaining.len(), 2);
    assert!(failure.uncertain.is_none());
    assert_eq!(f.area.report().unwrap().pending_tickets, 2);
    let tickets = failure
        .remaining
        .into_iter()
        .map(|owner| match owner {
            BatchOwner::Ticket(ticket) => ticket,
            BatchOwner::Sealed(_) => panic!("unprocessed writable owner expected"),
        })
        .collect();
    assert_eq!(
        f.area
            .discard_batch(tickets, &AtomicBool::new(false))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn failed_batch_receipt_keeps_intent_and_recovers_only_that_actual_source() {
    let f = Fixture::new(Limits::default());
    let tickets = f.allocate(2);
    f.area
        .inner
        .fail_next_receipt_append
        .store(true, Ordering::Release);
    let failure = f
        .area
        .discard_batch(tickets, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(failure.discarded.is_empty());
    assert_eq!(failure.remaining.len(), 1);
    assert!(matches!(
        failure.uncertain.as_ref().unwrap(),
        DiscardOutcome::MayHaveDiscarded { ticket: None, .. }
    ));
    drop(failure);
    assert_eq!(f.area.reconcile_discarded().unwrap(), 1);
    assert_eq!(f.area.report().unwrap().pending_tickets, 1);
    assert!(f.target.names(1).unwrap().is_empty());
}

#[test]
fn post_intent_native_source_change_returns_the_real_may_owner_without_deleting() {
    let f = Fixture::new(Limits::default());
    let tickets = f.allocate(2);
    let mut external = tickets[0].file().try_clone().unwrap();
    *f.area.inner.after_discard_intent.lock().unwrap() = Some(Box::new(move || {
        external.seek(SeekFrom::Start(0)).unwrap();
        external.write_all(b"changed bytes").unwrap();
        external.sync_all().unwrap();
        drop(external);
    }));
    let failure = f
        .area
        .discard_batch(tickets, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(failure.discarded.is_empty() && failure.remaining.len() == 1);
    let DiscardOutcome::MayHaveDiscarded {
        ticket, receipt, ..
    } = failure.uncertain.as_ref().unwrap()
    else {
        panic!("May required")
    };
    assert!(ticket.is_some() && !receipt.discard_confirmed);
    assert_eq!(f.area.report().unwrap().pending_tickets, 2);
    assert!(f.target.names(1).unwrap().is_empty());
}

#[test]
fn fixed_repair_keeps_the_original_header_and_cannot_batch_discard() {
    let temporary = tempfile::tempdir().unwrap();
    let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
    let target = root
        .create_private(&leaf("targets"), Traversal::SameFilesystem)
        .unwrap();
    let area = Area::open(&root, true).unwrap();
    let tickets = area
        .allocate_batch(&target, &Fixture::requests(2, 128), &AtomicBool::new(false))
        .unwrap();
    let bytes = log(&area);
    let failure = area
        .discard_batch(tickets, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(failure.not_eligible && failure.remaining.len() == 2);
    assert_eq!(log(&area), bytes);
    assert!(!String::from_utf8_lossy(&bytes).contains("discard_scope"));
    assert!(fs::read_dir(temporary.path().join("targets"))
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn cancel_after_durable_intent_finishes_the_current_ticket_before_stopping() {
    let f = Fixture::new(Limits::default());
    let tickets = f.allocate(2);
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = cancel.clone();
    *f.area.inner.after_discard_intent.lock().unwrap() = Some(Box::new(move || {
        trigger.store(true, Ordering::Release);
    }));
    let failure = f.area.discard_batch(tickets, &cancel).err().unwrap();
    assert_eq!(failure.error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(failure.discarded.len(), 1);
    assert_eq!(failure.remaining.len(), 1);
    assert!(failure.uncertain.is_none());
    assert_eq!(f.area.report().unwrap().completed_tickets, 1);
}

#[test]
fn final_report_refuses_success_even_after_all_owned_discards_finished() {
    let f = Fixture::new(Limits::default());
    let tickets = f.allocate(1);
    let hook = Hook::install(|area, point, _, _| {
        if point == Point::Discarded {
            let mut file = area.directory().create_file(&leaf("late-foreign")).unwrap();
            file.write_all(b"preserve foreign bytes").unwrap();
            file.sync_all().unwrap();
        }
    });
    let failure = f
        .area
        .discard_batch(tickets, &AtomicBool::new(false))
        .err()
        .unwrap();
    drop(hook);
    assert_eq!(failure.discarded.len(), 1);
    assert!(failure.remaining.is_empty() && failure.uncertain.is_none());
    let report = failure.report.as_ref().unwrap();
    assert!(report.issue_count > 0);
    assert!(report.issues.iter().any(|issue| {
        issue.kind == IssueKind::UnregisteredSource
            && issue.name.as_deref() == Some(std::ffi::OsStr::new("late-foreign"))
    }));
    let mut bytes = Vec::new();
    f.area
        .directory()
        .open_regular(&leaf("late-foreign"), Links::Single)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"preserve foreign bytes");
}

#[test]
fn batch_discard_preserves_existing_allocated_target_bytes() {
    let f = Fixture::new(Limits::default());
    let mut original = f.target.create_file(&leaf("output-0")).unwrap();
    original.write_all(b"business original").unwrap();
    original.sync_all().unwrap();
    drop(original);
    let tickets = f.allocate(2);
    assert_eq!(
        f.area
            .discard_batch(tickets, &AtomicBool::new(false))
            .unwrap()
            .len(),
        2
    );
    let mut bytes = Vec::new();
    f.target
        .open_regular(&leaf("output-0"), Links::Single)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"business original");
    assert_eq!(f.target.names(2).unwrap(), vec![OsString::from("output-0")]);
}
