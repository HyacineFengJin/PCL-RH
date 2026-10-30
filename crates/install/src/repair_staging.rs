//! Separate retained staging registry for new dependency bytes.
//!
//! All named leases and the append-only registry live below the private
//! `.pcl-linux/dependency-staging` directory, never in a schema-one business
//! operation directory. An allocation is synced before a payload handle is
//! exposed. Sealed evidence and publication receipts are additional records;
//! they do not authorize business recovery or deletion of any source/target.
//!
//! One native lock protects a framed registry.log. Incomplete tails, checksum
//! errors, unknown entries and substitutions are retained and diagnosed. This
//! version truncates only a revalidated incomplete log tail under its owning
//! lock; it never compacts/unlinks payloads or guesses business recovery. The
//! original intent/marker protocol remains recovery authority.
use pcl_core::retained_fs::{
    self,
    stage::{Evidence, PublishOutcome, Receipt, Sealed, SourceKind, SourceResidue, Stage},
    Directory, Identity, Kind, Leaf, Links, Lock, Metadata, Traversal,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex, MutexGuard},
};

const LOG_NAME: &str = "registry.log";
const MAGIC: &[u8; 8] = b"PCLSRG01";
const FRAME_HEADER: usize = 8 + 8 + 4 + 32;
const VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub(super) struct Limits {
    pub log_bytes: u64,
    pub frames: u64,
    pub record_bytes: usize,
    pub area_bytes: u64,
    pub pending_tickets: usize,
    pub entries: usize,
    pub issues: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            log_bytes: 256 * 1024 * 1024,
            frames: 1_000_000,
            record_bytes: 128 * 1024,
            area_bytes: 8 * 1024 * 1024 * 1024,
            pending_tickets: 8192,
            entries: 8192,
            issues: 128,
        }
    }
}
impl Limits {
    fn validate(&self) -> io::Result<()> {
        if self.log_bytes < FRAME_HEADER as u64
            || self.log_bytes > 1024 * 1024 * 1024
            || self.frames == 0
            || self.frames > 2_000_000
            || !(128..=1024 * 1024).contains(&self.record_bytes)
            || self.area_bytes < self.log_bytes
            || !(1..=100_000).contains(&self.pending_tickets)
            || !(2..=100_000).contains(&self.entries)
            || !(1..=1024).contains(&self.issues)
        {
            return Err(invalid("Invalid staging registry limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LogStatus {
    Empty,
    Ready,
    TruncatedTail {
        offset: u64,
        available: u64,
        required: u64,
    },
    Corrupt {
        offset: u64,
        reason: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum IssueKind {
    UnregisteredSource,
    UnknownEntry,
    ReplacedSource,
    MissingSource,
    SourceOverLimit,
    CompletedSourceRemains,
    Quota,
    LogChanged,
}
#[derive(Clone, Debug)]
pub(super) struct Issue {
    pub kind: IssueKind,
    pub name: Option<OsString>,
    pub ticket: Option<u64>,
    pub detail: String,
    pub blocks_writes: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SourceState {
    Present { len: u64 },
    Missing,
    Replaced,
    AnonymousUnobservable,
}
#[derive(Clone, Debug)]
pub(super) struct SourceReport {
    pub ticket: u64,
    pub name: Option<OsString>,
    pub identity: Identity,
    pub state: SourceState,
    pub active: bool,
    pub sealed: bool,
    pub has_receipt: bool,
    pub publication_uncertain: bool,
}
#[derive(Clone, Debug)]
pub(super) struct AreaReport {
    pub status: LogStatus,
    pub frames: u64,
    pub valid_prefix_bytes: u64,
    pub log_bytes: u64,
    pub completed_tickets: usize,
    pub pending_tickets: usize,
    pub physical_bytes_lower_bound: u64,
    pub reserved_bytes: u64,
    pub usage_complete: bool,
    pub sources: Vec<SourceReport>,
    pub issues: Vec<Issue>,
    pub issue_count: usize,
    pub blocking_issue_count: usize,
}
impl AreaReport {
    pub fn writable(&self) -> bool {
        self.status == LogStatus::Ready && self.blocking_issue_count == 0
    }
    fn issue(
        &mut self,
        limits: &Limits,
        kind: IssueKind,
        name: Option<OsString>,
        ticket: Option<u64>,
        detail: impl Into<String>,
    ) {
        self.issue_count = self.issue_count.saturating_add(1);
        let blocks_writes = matches!(
            kind,
            IssueKind::UnknownEntry | IssueKind::Quota | IssueKind::LogChanged
        );
        if blocks_writes {
            self.blocking_issue_count = self.blocking_issue_count.saturating_add(1);
        }
        if self.issues.len() < limits.issues {
            self.issues.push(Issue {
                kind,
                name,
                ticket,
                detail: detail.into(),
                blocks_writes,
            });
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Allocation {
    pub ticket: u64,
    pub source_parent: Identity,
    pub source_name: Option<OsString>,
    pub source_identity: Identity,
    pub source_kind: SourceKind,
    pub target_parent: Identity,
    pub target_leaf: OsString,
    pub max_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Header { version: u32, area: Identity },
    Allocation { allocation: Allocation },
    Sealed { ticket: u64, evidence: Evidence },
    Receipt { ticket: u64, receipt: Receipt },
    Exists { ticket: u64 },
}
struct Entry {
    allocation: Allocation,
    sealed: Option<Evidence>,
    receipt: Option<Receipt>,
    exists: bool,
}
impl Entry {
    fn completed(&self) -> bool {
        self.receipt.as_ref().is_some_and(|receipt| {
            receipt.publication_confirmed
                && receipt.directories_synced
                && receipt.source_residue == SourceResidue::None
                && receipt.observed_target.as_ref() == Some(&self.allocation.source_identity)
        })
    }
}
struct State {
    file: File,
    stamp: Metadata,
    status: LogStatus,
    next_sequence: u64,
    valid_end: u64,
    entries: HashMap<u64, Entry>,
    by_name: HashMap<OsString, u64>,
    pending: HashSet<u64>,
    active: HashSet<u64>,
    prefix_sha256: Sha256,
}
struct Inner {
    directory: Directory,
    identity: Identity,
    limits: Limits,
    // File closes before the owning native lock is released.
    state: Mutex<State>,
    _lock: Lock,
    #[cfg(test)]
    fail_next_receipt_append: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    after_receipt_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
#[derive(Clone)]
pub(super) struct Area {
    inner: Arc<Inner>,
}
struct Active {
    area: Area,
    ticket: u64,
}
impl Drop for Active {
    fn drop(&mut self) {
        if let Ok(mut state) = self.area.inner.state.lock() {
            state.active.remove(&self.ticket);
        }
    }
}
pub(super) struct Ticket {
    stage: Stage,
    allocation: Allocation,
    active: Active,
}
pub(super) struct SealedTicket {
    sealed: Sealed,
    allocation: Allocation,
    active: Active,
}
pub(super) enum Outcome {
    Published {
        receipt: Receipt,
    },
    Exists {
        ticket: SealedTicket,
    },
    MayHavePublished {
        ticket: Option<SealedTicket>,
        receipt: Receipt,
        error: io::Error,
    },
}

impl Area {
    pub fn open(root: &Directory, create: bool) -> io::Result<Self> {
        Self::open_with_limits(root, create, Limits::default())
    }
    pub fn open_with_limits(root: &Directory, create: bool, limits: Limits) -> io::Result<Self> {
        limits.validate()?;
        let storage = child(root, ".pcl-linux", create, false)?;
        let directory = child(&storage, "dependency-staging", create, true)?;
        let identity = directory.identity()?;
        let log = Leaf::new(LOG_NAME)?;
        if !create && directory.entry(&log)?.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Staging registry does not exist",
            ));
        }
        let lock = directory.lock(&log)?;
        let mut file = lock.file().try_clone()?;
        let stamp = retained_fs::metadata(&file)?;
        let mut state = State {
            file: file.try_clone()?,
            stamp: stamp.clone(),
            status: LogStatus::Empty,
            next_sequence: 0,
            valid_end: 0,
            entries: HashMap::new(),
            by_name: HashMap::new(),
            pending: HashSet::new(),
            active: HashSet::new(),
            prefix_sha256: Sha256::new(),
        };
        read_log(&mut file, &identity, &directory, &limits, &mut state)?;
        if retained_fs::metadata(&file)? != stamp {
            state.status = LogStatus::Corrupt {
                offset: 0,
                reason: "Registry changed during its read".into(),
            };
        }
        directory.recheck(&log, &stamp.identity)?;
        let area = Self {
            inner: Arc::new(Inner {
                directory,
                identity,
                limits,
                state: Mutex::new(state),
                _lock: lock,
                #[cfg(test)]
                fail_next_receipt_append: std::sync::atomic::AtomicBool::new(false),
                #[cfg(test)]
                after_receipt_write: Mutex::new(None),
            }),
        };
        let empty = { area.state()?.status == LogStatus::Empty };
        if create && empty {
            area.append(
                Record::Header {
                    version: VERSION,
                    area: area.inner.identity.clone(),
                },
                true,
            )?;
        }
        Ok(area)
    }
    pub fn directory(&self) -> &Directory {
        &self.inner.directory
    }
    pub fn report(&self) -> io::Result<AreaReport> {
        let state = self.state()?;
        self.inspect(&state, None)
    }
    /// Repair only an incomplete frame suffix, never a checksum/format error.
    /// The lock, exact held log identity, metadata and full valid-prefix hash
    /// are checked again before truncating this one known registry file.
    pub fn recover_tail(&self) -> io::Result<bool> {
        let mut state = self.state()?;
        match state.status {
            LogStatus::Ready | LogStatus::Empty => return Ok(false),
            LogStatus::Corrupt { .. } => {
                return Err(invalid(
                    "A corrupt registry is not a recoverable short tail",
                ))
            }
            LogStatus::TruncatedTail { .. } => {}
        }
        if !state.active.is_empty() {
            return Err(io::Error::other("Active tickets prevent log-tail recovery"));
        }
        self.check_log(&state)?;
        let before = state.stamp.clone();
        let expected: [u8; 32] = state.prefix_sha256.clone().finalize().into();
        let prefix_len = state.valid_end;
        let observed = hash_prefix(&mut state.file, prefix_len)?;
        if observed != expected || retained_fs::metadata(&state.file)? != before {
            return Err(invalid("Registry prefix changed before tail recovery"));
        }
        let recovered = (|| {
            state.file.set_len(prefix_len)?;
            state.file.sync_all()?;
            let after = retained_fs::metadata(&state.file)?;
            if after.identity != before.identity
                || after.len != prefix_len
                || after.security != before.security
                || after.links != 1
                || after.kind != Kind::Regular
                || after.reparse_tag.is_some()
            {
                return Err(invalid(
                    "Registry changed while truncating its incomplete suffix",
                ));
            }
            self.directory()
                .recheck(&Leaf::new(LOG_NAME)?, &after.identity)?;
            let mut file = state.file.try_clone()?;
            let mut fresh = empty_state(file.try_clone()?, after.clone());
            read_log(
                &mut file,
                &self.inner.identity,
                self.directory(),
                &self.inner.limits,
                &mut fresh,
            )?;
            if retained_fs::metadata(&file)? != after
                || fresh.valid_end != prefix_len
                || !matches!(fresh.status, LogStatus::Ready | LogStatus::Empty)
                || <[u8; 32]>::from(fresh.prefix_sha256.clone().finalize()) != expected
            {
                return Err(invalid("Recovered registry prefix failed full validation"));
            }
            Ok(fresh)
        })();
        match recovered {
            Ok(fresh) => {
                *state = fresh;
                if state.status == LogStatus::Empty {
                    self.append_locked(
                        &mut state,
                        Record::Header {
                            version: VERSION,
                            area: self.inner.identity.clone(),
                        },
                        true,
                    )?;
                }
                Ok(true)
            }
            Err(error) => {
                state.status = LogStatus::Corrupt {
                    offset: prefix_len,
                    reason: format!("Uncertain log-tail recovery: {error}"),
                };
                Err(error)
            }
        }
    }
    pub fn allocate(&self, target: &Directory, leaf: &Leaf, max_bytes: u64) -> io::Result<Ticket> {
        let mut state = self.state()?;
        self.ensure_budget(&state, None, max_bytes, true)?;
        let stage = Stage::new(self.directory())?;
        self.register_locked(stage, target, leaf, max_bytes, &mut state)
    }
    /// Existing Stage registration is permitted only for an empty private stage
    /// in this Area. Concurrent callers should use allocate(), which serializes
    /// creation and registration before exposing a payload handle.
    pub fn register(
        &self,
        stage: Stage,
        target: &Directory,
        leaf: &Leaf,
        max_bytes: u64,
    ) -> io::Result<Ticket> {
        let mut state = self.state()?;
        self.register_locked(stage, target, leaf, max_bytes, &mut state)
    }
    fn register_locked(
        &self,
        mut stage: Stage,
        target: &Directory,
        leaf: &Leaf,
        max_bytes: u64,
        state: &mut State,
    ) -> io::Result<Ticket> {
        let source_parent = stage.parent().identity()?;
        if source_parent != self.inner.identity {
            return Err(invalid("Stage is not in this private staging Area"));
        }
        let source = retained_fs::metadata(stage.file_mut())?;
        source.require_private()?;
        let source_name = stage.source_name().map(|name| name.as_os_str().to_owned());
        let source_kind = source_kind(source_name.is_some());
        let expected_links = if source_name.is_some() { 1 } else { 0 };
        if source.kind != Kind::Regular
            || source.reparse_tag.is_some()
            || source.len != 0
            || source.links != expected_links
        {
            return Err(invalid("Registration requires an empty new private stage"));
        }
        // Persist the empty native inode/SD before a durable record names it.
        // Allocation's directory sync then persists its named lease entry;
        // payload access is unavailable until both operations complete.
        stage.file_mut().sync_all()?;
        let confirmed = retained_fs::metadata(stage.file_mut())?;
        confirmed.require_private()?;
        if confirmed.identity != source.identity
            || confirmed.len != 0
            || confirmed.links != expected_links
            || confirmed.kind != Kind::Regular
            || confirmed.reparse_tag.is_some()
        {
            return Err(invalid(
                "Stage changed while persisting its allocation identity",
            ));
        }
        if let Some(name) = &source_name {
            self.directory()
                .recheck(&Leaf::new(name)?, &source.identity)?;
        }
        let candidate = source_name.as_ref().map(|name| (name, &source.identity));
        self.ensure_budget(state, candidate, max_bytes, true)?;
        let destination = target.metadata()?;
        if destination.kind != Kind::Directory
            || destination.reparse_tag.is_some()
            || !same_filesystem(&source.identity, &destination.identity)
        {
            return Err(invalid(
                "Target is not a literal directory on the stage filesystem",
            ));
        }
        let ticket = state.next_sequence;
        let allocation = Allocation {
            ticket,
            source_parent,
            source_name,
            source_identity: source.identity,
            source_kind,
            target_parent: destination.identity,
            target_leaf: leaf.as_os_str().to_owned(),
            max_bytes,
        };
        check_record_capacity(&allocation, self.inner.limits.record_bytes)?;
        self.append_locked(
            state,
            Record::Allocation {
                allocation: allocation.clone(),
            },
            false,
        )?;
        state.active.insert(ticket);
        Ok(Ticket {
            stage,
            allocation,
            active: Active {
                area: self.clone(),
                ticket,
            },
        })
    }
    fn state(&self) -> io::Result<MutexGuard<'_, State>> {
        self.inner
            .state
            .lock()
            .map_err(|_| io::Error::other("Staging registry lock was poisoned"))
    }
    fn append(&self, record: Record, initializing: bool) -> io::Result<()> {
        let mut state = self.state()?;
        if !initializing {
            self.ensure_budget(&state, None, 0, false)?;
        }
        self.append_locked(&mut state, record, initializing)
    }
    fn append_locked(
        &self,
        state: &mut State,
        record: Record,
        initializing: bool,
    ) -> io::Result<()> {
        if !(state.status == LogStatus::Ready || initializing && state.status == LogStatus::Empty) {
            return Err(io::Error::other(
                "Staging registry requires reconciliation; bytes were retained",
            ));
        }
        self.check_log(state)?;
        #[cfg(test)]
        if matches!(record, Record::Receipt { .. })
            && self
                .inner
                .fail_next_receipt_append
                .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(io::Error::other("fixture receipt append I/O failure"));
        }
        validate_record(&record, state.next_sequence, &self.inner.identity, state)?;
        validate_live_source_identity(&record, state, self.directory())?;
        let payload = serde_json::to_vec(&record).map_err(io::Error::other)?;
        if payload.len() > self.inner.limits.record_bytes
            || state.next_sequence >= self.inner.limits.frames
        {
            return Err(io::Error::other("Staging registry record quota exceeded"));
        }
        let frame = encode_frame(state.next_sequence, &payload);
        let end = state
            .valid_end
            .checked_add(frame.len() as u64)
            .ok_or_else(|| invalid("Registry size overflow"))?;
        if end > self.inner.limits.log_bytes {
            return Err(io::Error::other("Staging registry byte quota exceeded"));
        }
        let before = state.stamp.clone();
        let mut expected_prefix = state.prefix_sha256.clone();
        expected_prefix.update(&frame);
        let expected_digest: [u8; 32] = expected_prefix.clone().finalize().into();
        let sync_directory = matches!(record, Record::Header { .. } | Record::Allocation { .. });
        let result = (|| {
            state.file.seek(SeekFrom::Start(state.valid_end))?;
            state.file.write_all(&frame)?;
            state.file.sync_all()?;
            if sync_directory {
                self.directory().sync()?;
            }
            #[cfg(test)]
            if matches!(record, Record::Receipt { .. }) {
                if let Some(interfere) = self.inner.after_receipt_write.lock().unwrap().take() {
                    interfere();
                }
            }
            let after = retained_fs::metadata(&state.file)?;
            if after.identity != before.identity
                || after.len != end
                || after.links != 1
                || after.security != before.security
                || after.kind != Kind::Regular
                || after.reparse_tag.is_some()
            {
                return Err(invalid(
                    "Registry identity/length/security changed during append",
                ));
            }
            // Our write changes the metadata revision, so its new stamp alone
            // cannot prove that an independent writer left the OLD prefix
            // intact. Validate the entire expected prefix plus new frame,
            // bracketed by native metadata, before accepting that revision.
            // The native registry lock coordinates participating writers; it
            // does not make independent same-user handles exclusive.
            if hash_prefix(&mut state.file, end)? != expected_digest
                || retained_fs::metadata(&state.file)? != after
            {
                return Err(invalid("Registry append readback failed"));
            }
            self.directory()
                .recheck(&Leaf::new(LOG_NAME)?, &after.identity)?;
            if self.directory().identity()? != self.inner.identity {
                return Err(invalid("Staging Area identity changed"));
            }
            Ok(after)
        })();
        match result {
            Ok(after) => {
                apply_record(record, state.next_sequence, state)?;
                state.prefix_sha256 = expected_prefix;
                state.stamp = after;
                state.valid_end = end;
                state.next_sequence += 1;
                state.status = LogStatus::Ready;
                Ok(())
            }
            Err(error) => {
                state.status = LogStatus::Corrupt {
                    offset: state.valid_end,
                    reason: format!("Uncertain append: {error}"),
                };
                Err(error)
            }
        }
    }
    fn check_log(&self, state: &State) -> io::Result<()> {
        let area = self.directory().metadata()?;
        area.require_private()?;
        if area.identity != self.inner.identity
            || retained_fs::metadata(&state.file)? != state.stamp
        {
            return Err(invalid("Staging Area or registry changed since validation"));
        }
        self.directory()
            .recheck(&Leaf::new(LOG_NAME)?, &state.stamp.identity)
    }
    fn ensure_budget(
        &self,
        state: &State,
        candidate: Option<(&OsString, &Identity)>,
        additional: u64,
        allocation: bool,
    ) -> io::Result<()> {
        let report = self.inspect(state, candidate)?;
        if !report.writable() {
            return Err(io::Error::other(format!(
                "Staging Area is blocked; {} retained issue(s), log {:?}",
                report.issue_count, report.status
            )));
        }
        if allocation && state.pending.len() >= self.inner.limits.pending_tickets {
            return Err(io::Error::other(
                "Staging pending allocation quota exceeded",
            ));
        }
        let estimated = report
            .physical_bytes_lower_bound
            .checked_add(report.reserved_bytes)
            .and_then(|value| value.checked_add(additional))
            .and_then(|value| {
                value.checked_add((FRAME_HEADER + self.inner.limits.record_bytes) as u64)
            })
            .ok_or_else(|| invalid("Staging quota overflow"))?;
        if estimated > self.inner.limits.area_bytes {
            return Err(io::Error::other(
                "Staging Area byte reservation quota exceeded",
            ));
        }
        Ok(())
    }
    fn inspect(
        &self,
        state: &State,
        candidate: Option<(&OsString, &Identity)>,
    ) -> io::Result<AreaReport> {
        let limits = &self.inner.limits;
        let mut report = AreaReport {
            status: state.status.clone(),
            frames: state.next_sequence,
            valid_prefix_bytes: state.valid_end,
            log_bytes: retained_fs::metadata(&state.file)?.len,
            completed_tickets: state.entries.len() - state.pending.len(),
            pending_tickets: state.pending.len(),
            physical_bytes_lower_bound: 0,
            reserved_bytes: 0,
            usage_complete: true,
            sources: Vec::new(),
            issues: Vec::new(),
            issue_count: 0,
            blocking_issue_count: 0,
        };
        if self.check_log(state).is_err() {
            report.issue(
                limits,
                IssueKind::LogChanged,
                Some(LOG_NAME.into()),
                None,
                "Registry/Area binding changed; no write or cleanup is authorized",
            );
        }
        let names = match self.directory().names(limits.entries + 1) {
            Ok(names) if names.len() <= limits.entries => names,
            _ => {
                report.usage_complete = false;
                report.issue(
                    limits,
                    IssueKind::Quota,
                    None,
                    None,
                    "Area entry quota exceeded or enumeration failed",
                );
                return Ok(report);
            }
        };
        let mut observed = HashMap::new();
        for name in names {
            let leaf = match Leaf::new(&name) {
                Ok(leaf) => leaf,
                Err(_) => {
                    report.issue(
                        limits,
                        IssueKind::UnknownEntry,
                        Some(name),
                        None,
                        "Invalid literal entry retained",
                    );
                    report.usage_complete = false;
                    continue;
                }
            };
            let Some(mut metadata) = self.directory().entry(&leaf)? else {
                report.issue(
                    limits,
                    IssueKind::UnknownEntry,
                    Some(name),
                    None,
                    "Entry disappeared during inventory",
                );
                continue;
            };
            if metadata.kind == Kind::Regular
                && metadata.reparse_tag.is_none()
                && metadata.links == 1
            {
                match self.directory().open_regular(&leaf, Links::Single) {
                    Ok(file) => {
                        let held = retained_fs::metadata(&file)?;
                        if !metadata.same_object(&held) {
                            return Err(invalid("Area entry changed during inventory"));
                        }
                        self.directory().recheck(&leaf, &held.identity)?;
                        metadata = held;
                    }
                    Err(error) => {
                        report.issue(
                            limits,
                            IssueKind::UnknownEntry,
                            Some(name),
                            None,
                            format!("Entry cannot be inspected safely: {error}"),
                        );
                        report.usage_complete = false;
                        continue;
                    }
                }
            }
            report.physical_bytes_lower_bound = report
                .physical_bytes_lower_bound
                .saturating_add(metadata.len);
            if name == LOG_NAME {
                continue;
            }
            if candidate.is_some_and(|(candidate_name, id)| {
                candidate_name == &name && id == &metadata.identity
            }) {
                continue;
            }
            match state
                .by_name
                .get(&name)
                .and_then(|ticket| state.entries.get(ticket).map(|entry| (*ticket, entry)))
            {
                Some((ticket, entry)) => {
                    if metadata.kind != Kind::Regular || metadata.reparse_tag.is_some() {
                        report.usage_complete = false;
                        report.issue(limits, IssueKind::UnknownEntry, Some(name.clone()), Some(ticket),
                            "Replacement is not a flat ordinary source; its storage cannot be inventoried safely");
                    }
                    if metadata.identity != entry.allocation.source_identity
                        || metadata.kind != Kind::Regular
                        || metadata.reparse_tag.is_some()
                        || metadata.links != 1
                        || metadata.require_private().is_err()
                    {
                        report.issue(
                            limits,
                            IssueKind::ReplacedSource,
                            Some(name.clone()),
                            Some(ticket),
                            "Registered source identity/type/security was replaced",
                        );
                    } else if metadata.len > entry.allocation.max_bytes {
                        report.issue(
                            limits,
                            IssueKind::SourceOverLimit,
                            Some(name.clone()),
                            Some(ticket),
                            "Registered source exceeds allocation limit",
                        );
                    } else if entry.completed() {
                        report.issue(
                            limits,
                            IssueKind::CompletedSourceRemains,
                            Some(name.clone()),
                            Some(ticket),
                            "Completed receipt conflicts with a remaining source",
                        );
                    }
                    observed.insert(ticket, metadata);
                }
                None => {
                    let source = metadata.kind == Kind::Regular
                        && metadata.reparse_tag.is_none()
                        && metadata.links == 1
                        && metadata.require_private().is_ok();
                    if !source {
                        report.usage_complete = false;
                    }
                    report.issue(
                        limits,
                        if source {
                            IssueKind::UnregisteredSource
                        } else {
                            IssueKind::UnknownEntry
                        },
                        Some(name),
                        None,
                        "Unregistered entry retained; names/prefixes never authorize deletion",
                    );
                }
            }
        }
        for ticket in &state.pending {
            let entry = &state.entries[ticket];
            let source = observed.get(ticket);
            let source_state = match (&entry.allocation.source_name, source) {
                (None, _) => SourceState::AnonymousUnobservable,
                (Some(_), None) => {
                    report.issue(
                        limits,
                        IssueKind::MissingSource,
                        entry.allocation.source_name.clone(),
                        Some(*ticket),
                        "Registered named source is absent without a complete publication receipt",
                    );
                    SourceState::Missing
                }
                (Some(_), Some(metadata))
                    if metadata.identity == entry.allocation.source_identity
                        && metadata.kind == Kind::Regular
                        && metadata.reparse_tag.is_none()
                        && metadata.links == 1
                        && metadata.require_private().is_ok() =>
                {
                    SourceState::Present { len: metadata.len }
                }
                _ => SourceState::Replaced,
            };
            let active = state.active.contains(ticket);
            if active {
                // Only a live managed Ticket can expose a payload writer.
                // Closed/restarted leases retain their actual bytes and entry
                // quota, not a permanent reservation of an unused maximum.
                report.reserved_bytes = report.reserved_bytes.saturating_add(
                    entry
                        .allocation
                        .max_bytes
                        .saturating_sub(source.map_or(0, |metadata| metadata.len)),
                );
            }
            if report.sources.len() < limits.pending_tickets {
                report.sources.push(SourceReport {
                    ticket: *ticket,
                    name: entry.allocation.source_name.clone(),
                    identity: entry.allocation.source_identity.clone(),
                    state: source_state,
                    active,
                    sealed: entry.sealed.is_some(),
                    has_receipt: entry.receipt.is_some(),
                    publication_uncertain: entry.sealed.is_some()
                        && !entry.exists
                        && !entry.completed(),
                });
            }
        }
        if state.pending.len() > limits.pending_tickets
            || report
                .physical_bytes_lower_bound
                .saturating_add(report.reserved_bytes)
                > limits.area_bytes
        {
            report.issue(
                limits,
                IssueKind::Quota,
                None,
                None,
                "Staging pending/byte quota exceeded; all bytes retained",
            );
        }
        if state.next_sequence >= limits.frames
            || report.log_bytes.saturating_add(FRAME_HEADER as u64) > limits.log_bytes
        {
            report.issue(
                limits,
                IssueKind::Quota,
                Some(LOG_NAME.into()),
                None,
                "Registry frame/log byte quota is exhausted",
            );
        }
        Ok(report)
    }
}

impl Ticket {
    pub fn file_mut(&mut self) -> &mut File {
        self.stage.file_mut()
    }
    pub fn allocation(&self) -> &Allocation {
        &self.allocation
    }
    pub fn seal(self) -> io::Result<SealedTicket> {
        let Self {
            stage,
            allocation,
            active,
        } = self;
        let sealed = stage.seal(allocation.max_bytes).map_err(|error| {
            io::Error::other(format!(
                "Stage seal failed; registered bytes retained: {}",
                error.error
            ))
        })?;
        active.area.append(
            Record::Sealed {
                ticket: allocation.ticket,
                evidence: sealed.evidence().clone(),
            },
            false,
        )?;
        Ok(SealedTicket {
            sealed,
            allocation,
            active,
        })
    }
    pub fn publish(self, target: &Directory, leaf: &Leaf) -> io::Result<Outcome> {
        self.seal()?.publish(target, leaf)
    }
}
impl SealedTicket {
    pub fn evidence(&self) -> &Evidence {
        self.sealed.evidence()
    }
    pub fn publish(self, target: &Directory, leaf: &Leaf) -> io::Result<Outcome> {
        let Self {
            sealed,
            allocation,
            active,
        } = self;
        if target.identity()? != allocation.target_parent
            || leaf.as_os_str() != allocation.target_leaf
        {
            return Err(invalid("Publication target differs from its allocation"));
        }
        {
            let state = active.area.state()?;
            active.area.ensure_budget(&state, None, 0, false)?;
            let entry = state
                .entries
                .get(&allocation.ticket)
                .ok_or_else(|| invalid("Publication has no durable allocation"))?;
            if entry.exists
                || entry.receipt.is_some()
                || entry.sealed.as_ref() != Some(sealed.evidence())
            {
                return Err(invalid(
                    "Allocation already has a result or its sealed evidence differs",
                ));
            }
            // Native publication cannot be undone merely because its receipt
            // does not fit. Reject known exhaustion before touching the target.
            // This is an admission check, not a reservation against other live
            // tickets or independent writers consuming/changing the log later.
            let payload_bound = publication_payload_bound(&allocation, sealed.evidence())?;
            let frame_bound = (FRAME_HEADER as u64)
                .checked_add(payload_bound as u64)
                .ok_or_else(|| invalid("Registry size overflow"))?;
            if payload_bound > active.area.inner.limits.record_bytes
                || state.next_sequence >= active.area.inner.limits.frames
                || state
                    .valid_end
                    .checked_add(frame_bound)
                    .is_none_or(|end| end > active.area.inner.limits.log_bytes)
            {
                return Err(io::Error::other(
                    "Staging registry cannot fit the publication result; source retained",
                ));
            }
        }
        match sealed.publish_new(target, leaf) {
            Ok(PublishOutcome::Published { receipt }) => {
                match active.area.append(
                    Record::Receipt {
                        ticket: allocation.ticket,
                        receipt: receipt.clone(),
                    },
                    false,
                ) {
                    Ok(()) => Ok(Outcome::Published { receipt }),
                    Err(error) => Ok(Outcome::MayHavePublished {
                        ticket: None,
                        receipt,
                        error,
                    }),
                }
            }
            Ok(PublishOutcome::Exists { stage: sealed }) => {
                active.area.append(
                    Record::Exists {
                        ticket: allocation.ticket,
                    },
                    false,
                )?;
                Ok(Outcome::Exists {
                    ticket: Self {
                        sealed,
                        allocation,
                        active,
                    },
                })
            }
            Ok(PublishOutcome::MayHavePublished {
                stage: sealed,
                receipt,
                error,
            }) => {
                let recorded = active.area.append(
                    Record::Receipt {
                        ticket: allocation.ticket,
                        receipt: receipt.clone(),
                    },
                    false,
                );
                let error = match recorded {
                    Ok(()) => error,
                    Err(registry) => io::Error::other(format!(
                        "{error}; receipt persistence also failed: {registry}"
                    )),
                };
                Ok(Outcome::MayHavePublished {
                    ticket: Some(Self {
                        sealed,
                        allocation,
                        active,
                    }),
                    receipt,
                    error,
                })
            }
            Err(error) => Err(io::Error::other(format!(
                "Publication admission failed; registered bytes retained: {}",
                error.error
            ))),
        }
    }
}

fn child(parent: &Directory, name: &str, create: bool, private: bool) -> io::Result<Directory> {
    let leaf = Leaf::new(name)?;
    let directory = if create {
        match parent.create_private(&leaf, Traversal::SameFilesystem) {
            Ok(directory) => {
                parent.sync()?;
                directory
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                parent.child(&leaf, Traversal::SameFilesystem)?
            }
            Err(error) => return Err(error),
        }
    } else {
        parent.child(&leaf, Traversal::SameFilesystem)?
    };
    if private {
        directory.metadata()?.require_private()?;
    }
    Ok(directory)
}
fn source_kind(named: bool) -> SourceKind {
    if !named {
        SourceKind::AnonymousFd
    } else if cfg!(windows) {
        SourceKind::ExclusiveHandleLease
    } else {
        SourceKind::NamedLease
    }
}
fn same_filesystem(left: &Identity, right: &Identity) -> bool {
    match (left, right) {
        (Identity::Unix { device: a, .. }, Identity::Unix { device: b, .. }) => a == b,
        (Identity::Windows { volume: a, .. }, Identity::Windows { volume: b, .. }) => a == b,
        _ => false,
    }
}
fn matches_allocation(evidence: &Evidence, allocation: &Allocation) -> bool {
    evidence.parent_identity == allocation.source_parent
        && evidence.source_name == allocation.source_name
        && evidence.identity == allocation.source_identity
        && evidence.source_kind == allocation.source_kind
        && evidence.len <= allocation.max_bytes
}
// Reject a target whose eventual evidence cannot fit before exposing payload
// access. Publication must not knowingly create an unrecordable receipt merely
// because native names or numeric identities expand when encoded as JSON.
fn check_record_capacity(allocation: &Allocation, limit: usize) -> io::Result<()> {
    let evidence = Evidence {
        parent_identity: allocation.source_parent.clone(),
        source_name: allocation.source_name.clone(),
        identity: allocation.source_identity.clone(),
        len: allocation.max_bytes,
        sha256: [255; 32],
        source_kind: allocation.source_kind,
    };
    if publication_payload_bound(allocation, &evidence)? > limit {
        return Err(io::Error::other(
            "Allocation/evidence/receipt cannot fit the bounded registry record",
        ));
    }
    for record in [
        Record::Allocation {
            allocation: allocation.clone(),
        },
        Record::Sealed {
            ticket: allocation.ticket,
            evidence,
        },
    ] {
        if serde_json::to_vec(&record).map_err(io::Error::other)?.len() > limit {
            return Err(io::Error::other(
                "Allocation/evidence/receipt cannot fit the bounded registry record",
            ));
        }
    }
    Ok(())
}

/// Serialized upper bound for the next publication result, including a
/// collision and every source-residue spelling. The sealed source, destination
/// and ticket are already fixed. `false` is the longer boolean spelling, and
/// the largest same-platform native numbers bound any observed replacement ID.
/// These numeric bounds are used only for encoding capacity, never persisted or
/// treated as observed identity/authority.
fn publication_payload_bound(allocation: &Allocation, evidence: &Evidence) -> io::Result<usize> {
    let observed_bound = match allocation.source_identity {
        Identity::Unix { .. } => Identity::Unix {
            device: u64::MAX,
            inode: u64::MAX,
        },
        Identity::Windows { .. } => Identity::Windows {
            volume: u64::MAX,
            file: [u8::MAX; 16],
        },
    };
    let mut bound = serde_json::to_vec(&Record::Exists {
        ticket: allocation.ticket,
    })
    .map_err(io::Error::other)?
    .len();
    for source_residue in [
        SourceResidue::None,
        SourceResidue::LeasePresent,
        SourceResidue::NameReplaced,
        SourceResidue::Unknown,
    ] {
        let record = Record::Receipt {
            ticket: allocation.ticket,
            receipt: Receipt {
                source: evidence.clone(),
                target_parent: allocation.target_parent.clone(),
                target_name: allocation.target_leaf.clone(),
                observed_target: Some(observed_bound.clone()),
                source_residue,
                publication_confirmed: false,
                directories_synced: false,
            },
        };
        bound = bound.max(serde_json::to_vec(&record).map_err(io::Error::other)?.len());
    }
    Ok(bound)
}
fn validate_record(
    record: &Record,
    sequence: u64,
    area: &Identity,
    state: &State,
) -> io::Result<()> {
    match record {
        Record::Header {
            version,
            area: observed,
        } if sequence == 0 && *version == VERSION && observed == area => Ok(()),
        Record::Header { .. } => Err(invalid("Duplicate/wrong staging Area header")),
        _ if sequence == 0 => Err(invalid("Registry starts without its Area header")),
        Record::Allocation { allocation } => {
            if allocation.ticket != sequence
                || state.entries.contains_key(&allocation.ticket)
                || allocation.source_parent != *area
                || !same_filesystem(&allocation.source_identity, area)
                || !same_filesystem(&allocation.target_parent, area)
            {
                return Err(invalid("Duplicate or invalid allocation identity/sequence"));
            }
            Leaf::new(&allocation.target_leaf)?;
            match (&allocation.source_name, allocation.source_kind) {
                (None, SourceKind::AnonymousFd) => {}
                (Some(name), SourceKind::NamedLease | SourceKind::ExclusiveHandleLease) => {
                    Leaf::new(name)?;
                    if state.by_name.contains_key(name) {
                        return Err(invalid("Duplicate registered source name"));
                    }
                    let name = name
                        .to_str()
                        .ok_or_else(|| invalid("Invalid native stage lease name"))?;
                    let entropy = name
                        .strip_prefix(".pcl-stage-")
                        .and_then(|name| name.strip_suffix(".lease"));
                    if !entropy.is_some_and(|value| {
                        value.len() == 32
                            && value
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    }) {
                        return Err(invalid("Allocation source is not a native stage lease"));
                    }
                }
                _ => return Err(invalid("Allocation source kind/name mismatch")),
            }
            Ok(())
        }
        Record::Sealed { ticket, evidence } => {
            let entry = state
                .entries
                .get(ticket)
                .ok_or_else(|| invalid("Seal has no allocation"))?;
            if entry.sealed.is_some()
                || entry.receipt.is_some()
                || !matches_allocation(evidence, &entry.allocation)
            {
                return Err(invalid("Duplicate or inconsistent sealed evidence"));
            }
            Ok(())
        }
        Record::Receipt { ticket, receipt } => {
            let entry = state
                .entries
                .get(ticket)
                .ok_or_else(|| invalid("Receipt has no allocation"))?;
            if entry.receipt.is_some()
                || entry.exists
                || entry.sealed.as_ref() != Some(&receipt.source)
                || receipt.target_parent != entry.allocation.target_parent
                || receipt.target_name != entry.allocation.target_leaf
                || receipt.directories_synced
                    && (!receipt.publication_confirmed
                        || receipt.source_residue != SourceResidue::None
                        || receipt.observed_target.as_ref()
                            != Some(&entry.allocation.source_identity))
            {
                return Err(invalid("Duplicate or inconsistent publication receipt"));
            }
            Ok(())
        }
        Record::Exists { ticket } => {
            let entry = state
                .entries
                .get(ticket)
                .ok_or_else(|| invalid("Collision has no allocation"))?;
            if entry.sealed.is_none() || entry.receipt.is_some() || entry.exists {
                return Err(invalid("Duplicate or inconsistent collision record"));
            }
            Ok(())
        }
    }
}
fn apply_record(record: Record, sequence: u64, state: &mut State) -> io::Result<()> {
    match record {
        Record::Header { .. } => {}
        Record::Allocation { allocation } => {
            if let Some(name) = &allocation.source_name {
                state.by_name.insert(name.clone(), allocation.ticket);
            }
            state.pending.insert(allocation.ticket);
            state.entries.insert(
                allocation.ticket,
                Entry {
                    allocation,
                    sealed: None,
                    receipt: None,
                    exists: false,
                },
            );
        }
        Record::Sealed { ticket, evidence } => {
            state
                .entries
                .get_mut(&ticket)
                .ok_or_else(|| invalid("Missing allocation"))?
                .sealed = Some(evidence);
        }
        Record::Receipt { ticket, receipt } => {
            let entry = state
                .entries
                .get_mut(&ticket)
                .ok_or_else(|| invalid("Missing allocation"))?;
            entry.receipt = Some(receipt);
            if entry.completed() {
                state.pending.remove(&ticket);
            }
        }
        Record::Exists { ticket } => {
            state
                .entries
                .get_mut(&ticket)
                .ok_or_else(|| invalid("Missing allocation"))?
                .exists = true;
        }
    }
    let _ = sequence;
    Ok(())
}
fn encode_frame(sequence: u64, payload: &[u8]) -> Vec<u8> {
    let length = (payload.len() as u32).to_le_bytes();
    let sequence = sequence.to_le_bytes();
    let mut digest = Sha256::new();
    digest.update(sequence);
    digest.update(length);
    digest.update(payload);
    let mut frame = Vec::with_capacity(FRAME_HEADER + payload.len());
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&sequence);
    frame.extend_from_slice(&length);
    frame.extend_from_slice(&digest.finalize());
    frame.extend_from_slice(payload);
    frame
}
fn read_log(
    file: &mut File,
    area: &Identity,
    directory: &Directory,
    limits: &Limits,
    state: &mut State,
) -> io::Result<()> {
    if state.stamp.len > limits.log_bytes {
        state.status = LogStatus::Corrupt {
            offset: 0,
            reason: "Registry exceeds configured log quota".into(),
        };
        return Ok(());
    }
    file.seek(SeekFrom::Start(0))?;
    while state.valid_end < state.stamp.len {
        let available = state.stamp.len - state.valid_end;
        if available < FRAME_HEADER as u64 {
            let mut partial = vec![0u8; available as usize];
            file.read_exact(&mut partial)?;
            let magic_len = partial.len().min(MAGIC.len());
            let expected_sequence = state.next_sequence.to_le_bytes();
            let sequence_len = partial.len().saturating_sub(8).min(8);
            let bad_prefix = partial[..magic_len] != MAGIC[..magic_len]
                || sequence_len != 0
                    && partial[8..8 + sequence_len] != expected_sequence[..sequence_len]
                || partial.len() >= 20
                    && u32::from_le_bytes(partial[16..20].try_into().unwrap()) as usize
                        > limits.record_bytes;
            if bad_prefix {
                state.status = LogStatus::Corrupt {
                    offset: state.valid_end,
                    reason: "Invalid partial frame header (not a safe tail)".into(),
                };
                return Ok(());
            }
            state.status = LogStatus::TruncatedTail {
                offset: state.valid_end,
                available,
                required: FRAME_HEADER as u64,
            };
            return Ok(());
        }
        let mut header = [0u8; FRAME_HEADER];
        file.read_exact(&mut header)?;
        let sequence = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let length = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        let bad = if &header[..8] != MAGIC {
            Some("Invalid frame magic")
        } else if sequence != state.next_sequence {
            Some("Invalid frame sequence")
        } else if length > limits.record_bytes || sequence >= limits.frames {
            Some("Frame exceeds registry quota")
        } else {
            None
        };
        if let Some(reason) = bad {
            state.status = LogStatus::Corrupt {
                offset: state.valid_end,
                reason: reason.into(),
            };
            return Ok(());
        }
        let required = FRAME_HEADER as u64 + length as u64;
        if available < required {
            state.status = LogStatus::TruncatedTail {
                offset: state.valid_end,
                available,
                required,
            };
            return Ok(());
        }
        let mut payload = vec![0u8; length];
        file.read_exact(&mut payload)?;
        let mut digest = Sha256::new();
        digest.update(&header[8..20]);
        digest.update(&payload);
        if digest.finalize().as_slice() != &header[20..52] {
            state.status = LogStatus::Corrupt {
                offset: state.valid_end,
                reason: "Frame checksum mismatch (not a safe tail)".into(),
            };
            return Ok(());
        }
        let record = match serde_json::from_slice::<Record>(&payload) {
            Ok(record) => record,
            Err(error) => {
                state.status = LogStatus::Corrupt {
                    offset: state.valid_end,
                    reason: error.to_string().chars().take(256).collect(),
                };
                return Ok(());
            }
        };
        if let Err(error) = validate_record(&record, sequence, area, state) {
            state.status = LogStatus::Corrupt {
                offset: state.valid_end,
                reason: error.to_string(),
            };
            return Ok(());
        }
        if let Err(error) = validate_live_source_identity(&record, state, directory) {
            state.status = LogStatus::Corrupt {
                offset: state.valid_end,
                reason: error.to_string(),
            };
            return Ok(());
        }
        apply_record(record, sequence, state)?;
        state.prefix_sha256.update(header);
        state.prefix_sha256.update(&payload);
        state.valid_end += required;
        state.next_sequence += 1;
        state.status = LogStatus::Ready;
    }
    Ok(())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn validate_live_source_identity(
    record: &Record,
    state: &State,
    directory: &Directory,
) -> io::Result<()> {
    let Record::Allocation { allocation } = record else {
        return Ok(());
    };
    for ticket in &state.pending {
        let previous = &state.entries[ticket].allocation;
        if previous.source_identity != allocation.source_identity {
            continue;
        }
        // File IDs are not globally unique for the lifetime of an append-only
        // log. A vanished historical source can have its inode reused. Reject
        // only a currently active owner or an actual still-bound named source;
        // a restarted anonymous FD has no observable binding and grants none.
        let bound = match &previous.source_name {
            Some(name) => directory.entry(&Leaf::new(name)?)?.is_some_and(|metadata| {
                metadata.identity == previous.source_identity
                    && metadata.kind == Kind::Regular
                    && metadata.reparse_tag.is_none()
            }),
            None => false,
        };
        if state.active.contains(ticket) || bound {
            return Err(invalid("Duplicate currently live native source identity"));
        }
    }
    Ok(())
}

fn empty_state(file: File, stamp: Metadata) -> State {
    State {
        file,
        stamp,
        status: LogStatus::Empty,
        next_sequence: 0,
        valid_end: 0,
        entries: HashMap::new(),
        by_name: HashMap::new(),
        pending: HashSet::new(),
        active: HashSet::new(),
        prefix_sha256: Sha256::new(),
    }
}
fn hash_prefix(file: &mut File, length: u64) -> io::Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 32 * 1024];
    while remaining != 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..count])?;
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(digest.finalize().into())
}

#[cfg(test)]
#[path = "repair_staging_tests.rs"]
mod tests;
