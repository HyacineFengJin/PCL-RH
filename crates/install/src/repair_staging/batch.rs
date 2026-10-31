//! Scoped batch admission, never a persistent metadata cache or a filesystem
//! freeze. Only batches owning every pending source use the ledger. Records,
//! native per-source checks and the actual append/readback engine stay shared.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Retained allocation evidence. `retained_stage` may have an uncertain durable
/// allocation: neither its name nor `uncertain_allocation` authorizes discard.
/// Reconcile the actual registry before deciding how to retire that source.
pub struct AllocationBatchError {
    pub allocated: Vec<Ticket>,
    pub retained_stage: Option<Stage>,
    pub uncertain_allocation: Option<Allocation>,
    pub report: Option<AreaReport>,
    pub not_eligible: bool,
    pub error: io::Error,
}

/// The actual owner returned at its reached stage, not a reconstructed write
/// capability. A Sealed owner does not promise its last append was confirmed.
pub enum BatchOwner {
    Ticket(Ticket),
    Sealed(SealedTicket),
}

pub struct DiscardBatchError {
    pub discarded: Vec<DiscardReceipt>,
    pub remaining: Vec<BatchOwner>,
    pub uncertain: Option<DiscardOutcome>,
    pub report: Option<AreaReport>,
    pub not_eligible: bool,
    pub error: io::Error,
}

impl std::fmt::Debug for AllocationBatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllocationBatchError")
            .field("allocated_count", &self.allocated.len())
            .field("retained_stage", &self.retained_stage.is_some())
            .field("uncertain_allocation", &self.uncertain_allocation.is_some())
            .field("not_eligible", &self.not_eligible)
            .field("error_kind", &self.error.kind())
            .finish()
    }
}
impl std::fmt::Debug for DiscardBatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscardBatchError")
            .field("discarded_count", &self.discarded.len())
            .field("remaining_count", &self.remaining.len())
            .field("uncertain", &self.uncertain.is_some())
            .field("not_eligible", &self.not_eligible)
            .field("error_kind", &self.error.kind())
            .finish()
    }
}
impl std::fmt::Display for AllocationBatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}
impl std::error::Error for AllocationBatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
impl std::fmt::Display for DiscardBatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}
impl std::error::Error for DiscardBatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

pub(super) enum DiscardStep {
    Rejected {
        ticket: SealedTicket,
        error: io::Error,
    },
    Finished {
        outcome: DiscardOutcome,
        retired: Option<Active>,
    },
}
impl DiscardStep {
    pub(super) fn into_single(self) -> io::Result<DiscardOutcome> {
        match self {
            Self::Rejected { ticket, error } => {
                drop(ticket);
                Err(error)
            }
            Self::Finished { outcome, retired } => {
                drop(retired);
                Ok(outcome)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Allocate,
    Discard,
}
pub(super) struct Ledger {
    identity: Identity,
    names: HashSet<OsString>,
    mode: Mode,
}
impl Ledger {
    fn new(area: &Area, state: &mut State, mode: Mode) -> io::Result<Self> {
        area.check_log(state)?;
        let expected: [u8; 32] = state.prefix_sha256.clone().finalize().into();
        if hash_prefix(&mut state.file, state.valid_end)? != expected {
            return Err(invalid("Registry prefix changed before batch admission"));
        }
        area.check_log(state)?;
        let mut names = HashSet::from([OsString::from(LOG_NAME)]);
        for ticket in &state.pending {
            if let Some(name) = &state.entries[ticket].allocation.source_name {
                if !names.insert(name.clone()) {
                    return Err(invalid("Batch source names are not unique"));
                }
            }
        }
        let ledger = Self {
            identity: area.inner.identity.clone(),
            names,
            mode,
        };
        ledger.check_names(area)?;
        Ok(ledger)
    }
    fn check_names(&self, area: &Area) -> io::Result<()> {
        if area.inner.identity != self.identity {
            return Err(invalid("Batch admission belongs to another Area"));
        }
        let names = area.directory().names(area.inner.limits.entries + 1)?;
        if names.len() > area.inner.limits.entries
            || names.len() != self.names.len()
            || names.into_iter().collect::<HashSet<_>>() != self.names
        {
            return Err(invalid("Batch namespace changed; unknown entries retained"));
        }
        Ok(())
    }
    pub(super) fn admit_source(
        &mut self,
        area: &Area,
        candidate: Option<(&OsString, &Identity)>,
    ) -> io::Result<()> {
        if !matches!(self.mode, Mode::Allocate) {
            return Err(invalid("This batch cannot allocate a source"));
        }
        if let Some((name, identity)) = candidate {
            area.directory().recheck(&Leaf::new(name)?, identity)?;
            if !self.names.insert(name.clone()) {
                return Err(invalid("Batch source name already registered"));
            }
        }
        self.check_names(area)
    }
    pub(super) fn removed(&mut self, area: &Area, receipt: &DiscardReceipt) -> io::Result<()> {
        if !receipt.discard_confirmed
            || !receipt.directory_synced
            || receipt.source_residue != SourceResidue::None
        {
            return Err(invalid("Batch discard has no confirmed native result"));
        }
        if let Some(name) = &receipt.source.source_name {
            if !self.names.remove(name) {
                return Err(invalid("Discarded source is outside this batch"));
            }
        }
        self.check_names(area)
    }
    pub(super) fn check_frame(
        &self,
        area: &Area,
        state: &State,
        record: &Record,
        frame_bytes: u64,
    ) -> io::Result<()> {
        match (self.mode, record) {
            (Mode::Allocate, Record::Allocation { .. })
            | (Mode::Discard, Record::Sealed { .. })
            | (Mode::Discard, Record::DiscardIntent { .. })
            | (Mode::Discard, Record::DiscardReceipt { .. }) => {}
            _ => return Err(invalid("Record is outside its batch admission")),
        }
        self.check_names(area)?;
        if let Record::DiscardReceipt { ticket, .. } = record {
            let intent = state
                .entries
                .get(ticket)
                .and_then(|entry| entry.discard_intent.as_ref())
                .ok_or_else(|| invalid("Missing discard intent"))?;
            if observed_residue(area.directory(), intent)? != SourceResidue::None {
                return Err(invalid("Discard source reappeared; intent retained"));
            }
        }
        // All pending writers belong to this call. Charge each its full
        // admitted maximum, not cached lengths or another object's bytes.
        // Actual per-source metadata/hash and the final full report remain
        // authoritative; independent FS mutation is never declared frozen.
        let mut bytes = state
            .valid_end
            .checked_add(frame_bytes)
            .ok_or_else(|| invalid("Batch registry size overflow"))?;
        let padding = (FRAME_HEADER + area.inner.limits.record_bytes) as u64;
        let custom = area.require_discard_scope(state).is_ok();
        for ticket in &state.pending {
            let entry = &state.entries[ticket];
            let mut frames = remaining_frames(entry);
            if record_ticket(record) == Some(*ticket) {
                frames = match record {
                    Record::Sealed { .. } => 3,
                    Record::DiscardIntent { .. } => 1,
                    Record::DiscardReceipt { .. } => continue,
                    _ => frames,
                };
            }
            bytes = bytes
                .checked_add(entry.allocation.max_bytes)
                .and_then(|bytes| {
                    bytes.checked_add(if custom {
                        frames.checked_mul(padding)?
                    } else {
                        0
                    })
                })
                .ok_or_else(|| invalid("Batch reservation overflow"))?;
        }
        if let Record::Allocation { allocation } = record {
            bytes = bytes
                .checked_add(allocation.max_bytes)
                .and_then(|bytes| {
                    bytes.checked_add(if custom { padding.checked_mul(4)? } else { 0 })
                })
                .ok_or_else(|| invalid("Batch allocation reservation overflow"))?;
        }
        if bytes > area.inner.limits.area_bytes {
            return Err(invalid("Batch Area byte reservation quota exceeded"));
        }
        Ok(())
    }
}

struct Provisional {
    stage: Stage,
    allocation: Allocation,
    stamp: Metadata,
}
impl Provisional {
    fn ticket(self, area: &Area) -> Ticket {
        let ticket = self.allocation.ticket;
        Ticket {
            stage: self.stage,
            allocation: self.allocation,
            active: Active {
                area: area.clone(),
                ticket,
            },
        }
    }
}

impl Area {
    /// Allocate a bounded group before exposing any payload writer. The fast
    /// path requires no pending source and a complete clean inventory. An
    /// ineligible call has no allocation side effects: callers may use the
    /// unchanged single-item API instead. History and every frame stay v1.
    pub fn allocate_batch(
        &self,
        target: &Directory,
        requests: &[(Leaf, u64)],
        cancel: &AtomicBool,
    ) -> Result<Vec<Ticket>, AllocationBatchError> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let mut parts = Vec::new();
        let mut retained_stage = None;
        let mut uncertain_allocation = None;
        let mut report = None;
        let mut not_eligible = false;
        let result = (|| -> io::Result<()> {
            let mut state = self.state()?;
            let initial = self.inspect(&state, None)?;
            let eligible = initial.writable()
                && initial.usage_complete
                && initial.issue_count == 0
                && state.pending.is_empty()
                && state.active.is_empty();
            report = Some(initial);
            if !eligible {
                not_eligible = true;
                return Err(invalid(
                    "Batch allocation needs an Area without pending sources",
                ));
            }
            let mut ledger = Ledger::new(self, &mut state, Mode::Allocate)?;
            let admission = (|| {
                if requests.len() > self.inner.limits.pending_tickets
                    || ledger
                        .names
                        .len()
                        .checked_add(if cfg!(target_os = "linux") {
                            0
                        } else {
                            requests.len()
                        })
                        .is_none_or(|n| n > self.inner.limits.entries)
                    || state
                        .next_sequence
                        .checked_add(requests.len() as u64)
                        .is_none_or(|n| n >= self.inner.limits.frames)
                {
                    return Err(invalid("Batch allocation count quota exceeded"));
                }
                let destination = target.metadata()?;
                if destination.kind != Kind::Directory
                    || destination.reparse_tag.is_some()
                    || !same_filesystem(&self.inner.identity, &destination.identity)
                {
                    return Err(invalid(
                        "Batch target is not a literal directory on the Area filesystem",
                    ));
                }
                let padding = (FRAME_HEADER + self.inner.limits.record_bytes) as u64;
                let frames = if self.require_discard_scope(&state).is_ok() {
                    5
                } else {
                    1
                };
                let mut total = state.valid_end;
                for (_, max) in requests {
                    total = total
                        .checked_add(*max)
                        .and_then(|n| n.checked_add(padding.checked_mul(frames)?))
                        .ok_or_else(|| invalid("Batch allocation quota overflow"))?;
                }
                if total > self.inner.limits.area_bytes {
                    return Err(invalid("Batch Area byte reservation quota exceeded"));
                }
                Ok(())
            })();
            admission?;
            let mut stopped = None;
            for (leaf, max) in requests {
                if cancel.load(Ordering::Acquire) {
                    stopped = Some(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "Batch allocation cancelled between tickets",
                    ));
                    break;
                }
                let mut stage = match Stage::new(self.directory()) {
                    Ok(stage) => stage,
                    Err(error) => {
                        stopped = Some(error);
                        break;
                    }
                };
                let mut attempted = None;
                match self.register_stage_locked(
                    &mut stage,
                    target,
                    leaf,
                    *max,
                    &mut state,
                    Some(&mut ledger),
                    &mut attempted,
                ) {
                    Ok((allocation, stamp)) => {
                        state.active.insert(allocation.ticket);
                        // No Active destructor is installed while holding the
                        // state lock. Every path converts owners after unlock.
                        parts.push(Provisional {
                            stage,
                            allocation,
                            stamp,
                        });
                        #[cfg(test)]
                        point(
                            self,
                            Point::Allocated,
                            state.next_sequence,
                            parts.last().map(|part| part.stage.file()),
                        );
                    }
                    Err(error) => {
                        retained_stage = Some(stage);
                        uncertain_allocation = attempted;
                        stopped = Some(error);
                        break;
                    }
                }
            }
            if stopped.is_none() && cancel.load(Ordering::Acquire) {
                stopped = Some(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Batch allocation cancelled between tickets",
                ));
            }
            let verify_owners = || {
                ledger.check_names(self)?;
                for part in &parts {
                    if retained_fs::metadata(part.stage.file())? != part.stamp {
                        return Err(invalid("Batch source changed before writer admission"));
                    }
                    if let Some(name) = &part.allocation.source_name {
                        self.directory()
                            .recheck(&Leaf::new(name)?, &part.stamp.identity)?;
                    }
                }
                Ok(())
            };
            let before_report = verify_owners();
            let actual = self.inspect(&state, None);
            match actual {
                Ok(actual) => {
                    let valid =
                        actual.writable() && actual.usage_complete && actual.issue_count == 0;
                    report = Some(actual);
                    if stopped.is_none() && !valid {
                        stopped = Some(invalid("Batch final inventory changed; sources retained"));
                    }
                }
                Err(error) => {
                    if stopped.is_none() {
                        stopped = Some(error);
                    }
                }
            }
            if let Some(error) = stopped {
                return Err(error);
            }
            before_report?;
            verify_owners()
        })();
        // The closure's state guard is gone before Active owners exist.
        let allocated = parts.into_iter().map(|part| part.ticket(self)).collect();
        match result {
            Ok(()) => Ok(allocated),
            Err(error) => Err(AllocationBatchError {
                allocated,
                retained_stage,
                uncertain_allocation,
                report,
                not_eligible,
                error,
            }),
        }
    }

    /// Consume all pending active owners in one dedicated discard Area. A
    /// noneligible call returns every unchanged owner for the single-item path.
    /// Cancellation is observed between tickets; a durable intent's current
    /// source completes the existing protocol or returns its original May.
    pub fn discard_batch(
        &self,
        tickets: Vec<Ticket>,
        cancel: &AtomicBool,
    ) -> Result<Vec<DiscardReceipt>, DiscardBatchError> {
        if tickets.is_empty() {
            return Ok(Vec::new());
        }
        let mut waiting: std::collections::VecDeque<_> = tickets.into();
        let mut remaining = Vec::new();
        let mut discarded = Vec::new();
        let mut uncertain = None;
        let mut retired = Vec::new();
        let mut report = None;
        let mut not_eligible = false;
        let result = (|| -> io::Result<()> {
            let mut state = self.state()?;
            let initial = self.inspect(&state, None)?;
            let ids: HashSet<_> = waiting
                .iter()
                .map(|ticket| ticket.allocation.ticket)
                .collect();
            let owners_match = waiting.iter().all(|ticket| {
                Arc::ptr_eq(&self.inner, &ticket.active.area.inner)
                    && state
                        .entries
                        .get(&ticket.allocation.ticket)
                        .is_some_and(|entry| {
                            entry.allocation == ticket.allocation
                                && entry.sealed.is_none()
                                && entry.receipt.is_none()
                                && entry.discard_intent.is_none()
                        })
            });
            let eligible = self.require_discard_scope(&state).is_ok()
                && initial.writable()
                && initial.usage_complete
                && initial.issue_count == 0
                && ids.len() == waiting.len()
                && ids == state.pending
                && ids == state.active
                && owners_match;
            report = Some(initial);
            if !eligible {
                not_eligible = true;
                return Err(invalid(
                    "Batch discard needs every pending active source in this Area",
                ));
            }
            let mut ledger = Ledger::new(self, &mut state, Mode::Discard)?;
            let mut stopped = None;
            while !waiting.is_empty() {
                if cancel.load(Ordering::Acquire) {
                    stopped = Some(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "Batch discard cancelled between tickets",
                    ));
                    break;
                }
                let ticket = waiting.pop_front().expect("nonempty batch");
                let sealed = match seal_owned(self, ticket, &mut state, &mut ledger) {
                    Ok(sealed) => sealed,
                    Err((owner, error)) => {
                        remaining.push(owner);
                        stopped = Some(error);
                        break;
                    }
                };
                match sealed.discard_locked(self, &mut state, Some(&mut ledger)) {
                    DiscardStep::Rejected { ticket, error } => {
                        remaining.push(BatchOwner::Sealed(ticket));
                        stopped = Some(error);
                        break;
                    }
                    DiscardStep::Finished {
                        outcome,
                        retired: done,
                    } => {
                        if let Some(done) = done {
                            retired.push(done);
                        }
                        match outcome {
                            DiscardOutcome::Discarded { receipt } => {
                                discarded.push(receipt);
                                #[cfg(test)]
                                point(self, Point::Discarded, state.next_sequence, None);
                            }
                            outcome @ DiscardOutcome::MayHaveDiscarded { .. } => {
                                uncertain = Some(outcome);
                                stopped = Some(io::Error::other(
                                    "Batch discard outcome needs reconciliation",
                                ));
                                break;
                            }
                        }
                    }
                }
            }
            if stopped.is_none() && cancel.load(Ordering::Acquire) {
                stopped = Some(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Batch discard cancelled between tickets",
                ));
            }
            let names = ledger.check_names(self);
            match self.inspect(&state, None) {
                Ok(actual) => {
                    let valid =
                        actual.writable() && actual.usage_complete && actual.issue_count == 0;
                    report = Some(actual);
                    if stopped.is_none() && !valid {
                        stopped = Some(invalid("Batch final inventory changed; sources retained"));
                    }
                }
                Err(error) => {
                    if stopped.is_none() {
                        stopped = Some(error);
                    }
                }
            }
            if let Some(error) = stopped {
                return Err(error);
            }
            names?;
            ledger.check_names(self)
        })();
        // No Active destructor can run under the state lock, on any outcome.
        drop(retired);
        remaining.extend(waiting.into_iter().map(BatchOwner::Ticket));
        match result {
            Ok(()) => Ok(discarded),
            Err(error) => Err(DiscardBatchError {
                discarded,
                remaining,
                uncertain,
                report,
                not_eligible,
                error,
            }),
        }
    }
}

fn seal_owned(
    area: &Area,
    ticket: Ticket,
    state: &mut State,
    ledger: &mut Ledger,
) -> Result<SealedTicket, (BatchOwner, io::Error)> {
    let Ticket {
        stage,
        allocation,
        active,
    } = ticket;
    let sealed = match stage.seal(allocation.max_bytes) {
        Ok(sealed) => sealed,
        Err(error) => {
            return Err((
                BatchOwner::Ticket(Ticket {
                    stage: error.stage,
                    allocation,
                    active,
                }),
                error.error,
            ))
        }
    };
    let owner = SealedTicket {
        sealed,
        allocation,
        active,
    };
    let record = Record::Sealed {
        ticket: owner.allocation.ticket,
        evidence: owner.sealed.evidence().clone(),
    };
    match area.append_locked_with_ledger(state, record, false, Some(ledger)) {
        Ok(()) => Ok(owner),
        Err(error) => Err((BatchOwner::Sealed(owner), error)),
    }
}

#[cfg(test)]
thread_local! {
    static INSPECTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&Area, Point, u64, Option<&File>)>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Point {
    Allocated,
    Discarded,
    FrameWritten,
}
#[cfg(test)]
pub(super) fn note_inspect() {
    INSPECTS.with(|count| count.set(count.get() + 1));
}
#[cfg(test)]
pub(super) fn point(area: &Area, at: Point, ticket: u64, file: Option<&File>) {
    HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(area, at, ticket, file);
        }
    });
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
