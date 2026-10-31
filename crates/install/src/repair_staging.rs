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
//! lock. The original fixed repair namespace never discards payloads. Separate
//! caller-authorized namespaces can explicitly seal/sync a discard intent and
//! reconcile only that generated source; they never guess business recovery.
//! The original intent/marker protocol remains business recovery authority.
use pcl_core::retained_fs::{
    self,
    stage::{
        DiscardOutcome as NativeDiscardOutcome, DiscardReceipt, Evidence, PublishOutcome, Receipt,
        Sealed, SourceKind, SourceResidue, Stage,
    },
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

#[path = "repair_staging/batch.rs"]
mod batch;
pub use batch::{AllocationBatchError, BatchOwner, DiscardBatchError};

#[derive(Clone, Debug)]
pub struct Limits {
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
pub enum LogStatus {
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
pub enum IssueKind {
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
pub struct Issue {
    pub kind: IssueKind,
    pub name: Option<OsString>,
    pub ticket: Option<u64>,
    pub detail: String,
    pub blocks_writes: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    Present { len: u64 },
    Missing,
    Replaced,
    AnonymousUnobservable,
}
#[derive(Clone, Debug)]
pub struct SourceReport {
    pub ticket: u64,
    pub name: Option<OsString>,
    pub identity: Identity,
    pub state: SourceState,
    pub active: bool,
    pub sealed: bool,
    pub has_receipt: bool,
    pub publication_uncertain: bool,
    pub discard_requested: bool,
}
#[derive(Clone, Debug)]
pub struct AreaReport {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
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
    Header {
        version: u32,
        area: Identity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        discard_scope: Option<DiscardScope>,
    },
    Allocation {
        allocation: Allocation,
    },
    Sealed {
        ticket: u64,
        evidence: Evidence,
    },
    Receipt {
        ticket: u64,
        receipt: Receipt,
    },
    Exists {
        ticket: u64,
    },
    DiscardIntent {
        ticket: u64,
        intent: DiscardIntent,
    },
    DiscardReceipt {
        ticket: u64,
        receipt: DiscardReceipt,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscardIntent {
    allocation: Allocation,
    evidence: Evidence,
    metadata: Metadata,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscardScope {
    purpose: String,
    parent: Identity,
    name: OsString,
}
struct Entry {
    allocation: Allocation,
    sealed: Option<Evidence>,
    receipt: Option<Receipt>,
    exists: bool,
    discard_intent: Option<DiscardIntent>,
    discard_receipt: Option<DiscardReceipt>,
}
impl Entry {
    fn completed(&self) -> bool {
        if self.discard_receipt.as_ref().is_some_and(|receipt| {
            receipt.discard_confirmed
                && receipt.directory_synced
                && receipt.source_residue == SourceResidue::None
        }) {
            return true;
        }
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
    discard_scope: Option<DiscardScope>,
}
struct Inner {
    directory: Directory,
    identity: Identity,
    limits: Limits,
    // File closes before the owning native lock is released.
    state: Mutex<State>,
    _lock: Lock,
    expected_discard_scope: Option<DiscardScope>,
    #[cfg(test)]
    fail_next_receipt_append: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    after_receipt_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    after_discard_intent: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
#[derive(Clone)]
pub struct Area {
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
pub struct Ticket {
    stage: Stage,
    allocation: Allocation,
    active: Active,
}
pub struct SealedTicket {
    sealed: Sealed,
    allocation: Allocation,
    active: Active,
}
pub enum Outcome {
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
pub enum DiscardOutcome {
    Discarded {
        receipt: DiscardReceipt,
    },
    MayHaveDiscarded {
        ticket: Option<SealedTicket>,
        receipt: DiscardReceipt,
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
        Self::open_directory(directory, create, limits, None)
    }
    /// Open one dedicated staging namespace below a caller-authorized private
    /// app context. `name` is one literal leaf, never a business operation path.
    /// Existing nonempty directories without a bound registry are refused
    /// before creating registry.log; existing evidence is never deleted or
    /// converted from another schema. Ownership/privacy are checked, not fixed.
    ///
    /// The same framed registry, native lock, checksums, limits, recovery and
    /// no-replace publication implementation serves both this and repair's
    /// original fixed `.pcl-linux/dependency-staging` namespace.
    /// A newly initialized custom Header declares its discard purpose and binds
    /// the context's full ID and this leaf. Existing legacy Headers stay in
    /// their original publication-only mode; opening or renaming never upgrades
    /// them to discard authority. Declared scopes cannot be rebound by moving.
    pub fn open_at_private(
        parent: &Directory,
        name: &Leaf,
        create: bool,
        limits: Limits,
    ) -> io::Result<Self> {
        limits.validate()?;
        parent.metadata()?.require_private()?;
        // Leaf's fields are private, but revalidate at this public boundary so
        // this constructor never depends on an unchecked future Leaf producer.
        let name = Leaf::new(name.as_os_str())?;
        let directory = child_leaf(parent, &name, create, true)?;
        let identity = directory.identity()?;
        parent.recheck(&name, &identity)?;
        let log = Leaf::new(LOG_NAME)?;
        let existing_log = directory.entry(&log)?;
        if existing_log
            .as_ref()
            .is_none_or(|metadata| metadata.len == 0)
        {
            let names = directory.names(limits.entries)?;
            if names.iter().any(|name| name != log.as_os_str()) {
                return Err(invalid(
                    "Nonempty unbound staging namespace retained without creating a registry",
                ));
            }
        }
        // The original fixed repair namespace must keep its old record set.
        let scope = if name.as_os_str() == "dependency-staging" {
            None
        } else {
            Some(DiscardScope {
                purpose: "app-generated-input-discard-v1".into(),
                parent: parent.identity()?,
                name: name.as_os_str().to_owned(),
            })
        };
        let area = Self::open_directory(directory, create, limits, scope)?;
        parent.metadata()?.require_private()?;
        parent.recheck(&name, &identity)?;
        Ok(area)
    }
    fn open_directory(
        directory: Directory,
        create: bool,
        limits: Limits,
        expected_discard_scope: Option<DiscardScope>,
    ) -> io::Result<Self> {
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
            discard_scope: None,
        };
        read_log(
            &mut file,
            &identity,
            &directory,
            &limits,
            &mut state,
            expected_discard_scope.as_ref(),
        )?;
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
                expected_discard_scope,
                #[cfg(test)]
                fail_next_receipt_append: std::sync::atomic::AtomicBool::new(false),
                #[cfg(test)]
                after_receipt_write: Mutex::new(None),
                #[cfg(test)]
                after_discard_intent: Mutex::new(None),
            }),
        };
        let empty = { area.state()?.status == LogStatus::Empty };
        if create && empty {
            area.append(
                Record::Header {
                    version: VERSION,
                    area: area.inner.identity.clone(),
                    discard_scope: area.inner.expected_discard_scope.clone(),
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
    /// Reconcile only already-durable discard intents in this dedicated Area.
    /// Unregistered/orphan allocations without intent are retained. A present
    /// source must match its full metadata and SHA-256; a missing source counts
    /// only for a valid intent, followed by a namespace flush and receipt.
    /// Darwin retains the private-workspace cooperative POSIX deletion limit.
    pub fn reconcile_discarded(&self) -> io::Result<usize> {
        let mut state = self.state()?;
        self.require_discard_scope(&state)?;
        if state.status != LogStatus::Ready {
            return Err(invalid(
                "Discard recovery requires a valid complete registry",
            ));
        }
        self.check_log(&state)?;
        let expected: [u8; 32] = state.prefix_sha256.clone().finalize().into();
        let length = state.valid_end;
        if hash_prefix(&mut state.file, length)? != expected {
            return Err(invalid("Registry prefix changed before discard recovery"));
        }
        let intents: Vec<_> = state
            .entries
            .iter()
            .filter_map(|(ticket, entry)| {
                if entry.discard_receipt.is_none() && !state.active.contains(ticket) {
                    entry.discard_intent.clone().map(|intent| (*ticket, intent))
                } else {
                    None
                }
            })
            .collect();
        let mut completed = 0;
        for (ticket, intent) in intents {
            self.check_log(&state)?;
            let receipt = discarded_receipt(&intent);
            let record = Record::DiscardReceipt { ticket, receipt };
            self.check_capacity(&state, std::slice::from_ref(&record))?;
            discard_intended_source(self.directory(), &intent)?;
            self.append_locked(&mut state, record, false)?;
            completed += 1;
        }
        Ok(completed)
    }
    fn check_capacity(&self, state: &State, records: &[Record]) -> io::Result<()> {
        let mut bytes = state.valid_end;
        let mut frames = state.next_sequence;
        for record in records {
            let length = serde_json::to_vec(record).map_err(io::Error::other)?.len();
            if length > self.inner.limits.record_bytes {
                return Err(invalid(
                    "Discard evidence/receipt exceeds registry record quota; registry evidence retained",
                ));
            }
            bytes = bytes
                .checked_add(FRAME_HEADER as u64)
                .and_then(|bytes| bytes.checked_add(length as u64))
                .ok_or_else(|| invalid("Discard registry size overflow"))?;
            frames = frames
                .checked_add(1)
                .ok_or_else(|| invalid("Discard registry sequence overflow"))?;
        }
        if bytes > self.inner.limits.log_bytes || frames > self.inner.limits.frames {
            return Err(invalid(
                "Registry cannot fit durable discard intent and receipt; discard not admitted and registry evidence retained",
            ));
        }
        Ok(())
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
                self.inner.expected_discard_scope.as_ref(),
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
                            // A truncated first header has no durable discard
                            // declaration to recover. Never infer it from a new
                            // factory/path name while repairing an empty prefix.
                            discard_scope: None,
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
        let (allocation, _) = self
            .register_stage_locked(&mut stage, target, leaf, max_bytes, state, None, &mut None)?;
        state.active.insert(allocation.ticket);
        Ok(Ticket {
            stage,
            allocation: allocation.clone(),
            active: Active {
                area: self.clone(),
                ticket: allocation.ticket,
            },
        })
    }
    fn register_stage_locked(
        &self,
        stage: &mut Stage,
        target: &Directory,
        leaf: &Leaf,
        max_bytes: u64,
        state: &mut State,
        mut ledger: Option<&mut batch::Ledger>,
        attempted: &mut Option<Allocation>,
    ) -> io::Result<(Allocation, Metadata)> {
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
        if let Some(ledger) = ledger.as_deref_mut() {
            ledger.admit_source(self, candidate)?;
        } else {
            self.ensure_budget(state, candidate, max_bytes, true)?;
        }
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
        *attempted = Some(allocation.clone());
        self.append_locked_with_ledger(
            state,
            Record::Allocation {
                allocation: allocation.clone(),
            },
            false,
            ledger,
        )?;
        Ok((allocation, confirmed))
    }
    fn state(&self) -> io::Result<MutexGuard<'_, State>> {
        self.inner
            .state
            .lock()
            .map_err(|_| io::Error::other("Staging registry lock was poisoned"))
    }
    fn require_discard_scope(&self, state: &State) -> io::Result<()> {
        if state.discard_scope.is_none()
            || state.discard_scope.as_ref() != self.inner.expected_discard_scope.as_ref()
        {
            return Err(invalid("The durable Header has no matching custom discard capability; legacy records retained without upgrade"));
        }
        Ok(())
    }
    fn check_actual_append_budget(
        &self,
        state: &State,
        record: &Record,
        frame_bytes: u64,
    ) -> io::Result<()> {
        let report = self.inspect(state, None)?;
        if !report.writable() || !report.usage_complete {
            return Err(invalid(
                "Staging Area is unknown, changed or over quota; append refused",
            ));
        }
        let mut credit = 0u64;
        if let Some(ticket) = record_ticket(record) {
            if let Some(entry) = state.entries.get(&ticket) {
                let reserved = state.active.contains(&ticket) || entry.discard_intent.is_some();
                if reserved {
                    let after = match record {
                        Record::Sealed { .. } => 3,
                        Record::Exists { .. } => 2,
                        Record::DiscardIntent { .. } => 1,
                        Record::Receipt { .. } | Record::DiscardReceipt { .. } => 0,
                        _ => remaining_frames(entry),
                    };
                    credit = remaining_frames(entry)
                        .saturating_sub(after)
                        .saturating_mul((FRAME_HEADER + self.inner.limits.record_bytes) as u64);
                }
                let completed = matches!(record, Record::DiscardReceipt { .. })
                    || matches!(record, Record::Receipt { receipt, .. }
                        if receipt.publication_confirmed && receipt.directories_synced);
                if completed && state.active.contains(&ticket) {
                    // Completion retires the unused payload reservation too.
                    // Never credit physical bytes of an observed replacement.
                    let unused = report
                        .sources
                        .iter()
                        .find(|source| source.ticket == ticket)
                        .map_or(0, |source| match source.state {
                            SourceState::Present { len } => {
                                entry.allocation.max_bytes.saturating_sub(len)
                            }
                            SourceState::Missing | SourceState::AnonymousUnobservable => {
                                entry.allocation.max_bytes
                            }
                            SourceState::Replaced => 0,
                        });
                    credit = credit.saturating_add(unused);
                }
                if let Record::DiscardReceipt { .. } = record {
                    let intent = entry
                        .discard_intent
                        .as_ref()
                        .ok_or_else(|| invalid("Missing discard intent"))?;
                    if observed_residue(self.directory(), intent)? != SourceResidue::None {
                        return Err(invalid(
                            "Discard source reappeared before receipt; intent retained",
                        ));
                    }
                }
            }
        }
        let after = report
            .physical_bytes_lower_bound
            .checked_add(report.reserved_bytes)
            .and_then(|bytes| bytes.checked_add(frame_bytes))
            .and_then(|bytes| bytes.checked_sub(credit.min(report.reserved_bytes)))
            .ok_or_else(|| invalid("Staging append quota overflow"))?;
        if after > self.inner.limits.area_bytes {
            return Err(invalid(
                "Actual registry frame exceeds remaining Area budget; evidence retained",
            ));
        }
        Ok(())
    }
    fn append(&self, record: Record, initializing: bool) -> io::Result<()> {
        let mut state = self.state()?;
        if !initializing && self.require_discard_scope(&state).is_err() {
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
        self.append_locked_with_ledger(state, record, initializing, None)
    }
    fn append_locked_with_ledger(
        &self,
        state: &mut State,
        record: Record,
        initializing: bool,
        ledger: Option<&mut batch::Ledger>,
    ) -> io::Result<()> {
        if !(state.status == LogStatus::Ready || initializing && state.status == LogStatus::Empty) {
            return Err(io::Error::other(
                "Staging registry requires reconciliation; bytes were retained",
            ));
        }
        self.check_log(state)?;
        if is_discard(&record) {
            self.require_discard_scope(state)?;
        }
        if let Record::Header {
            discard_scope: Some(scope),
            ..
        } = &record
        {
            if Some(scope) != self.inner.expected_discard_scope.as_ref() {
                return Err(invalid(
                    "Discard Header purpose differs from the authorized custom namespace",
                ));
            }
        }
        #[cfg(test)]
        if matches!(
            record,
            Record::Receipt { .. } | Record::DiscardReceipt { .. }
        ) && self
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
        #[cfg(test)]
        let batched = ledger.is_some();
        if let Some(ledger) = ledger {
            ledger.check_frame(self, state, &record, frame.len() as u64)?;
        } else if self.require_discard_scope(state).is_ok() {
            self.check_actual_append_budget(state, &record, frame.len() as u64)?;
        }
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
        let sync_directory = matches!(
            record,
            Record::Header { .. }
                | Record::Allocation { .. }
                | Record::DiscardIntent { .. }
                | Record::DiscardReceipt { .. }
        );
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
            #[cfg(test)]
            if batched {
                batch::point(
                    self,
                    batch::Point::FrameWritten,
                    state.next_sequence,
                    Some(&state.file),
                );
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
                // A new custom ticket reserves its allocation plus the
                // longest remaining route: seal, optional collision, discard
                // intent and receipt. No wire field or second quota ledger is
                // introduced; frame size is bounded by record_bytes already.
                let frames = if allocation && self.require_discard_scope(state).is_ok() {
                    5
                } else {
                    1
                };
                value.checked_add(
                    ((FRAME_HEADER + self.inner.limits.record_bytes) as u64).saturating_mul(frames),
                )
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
        #[cfg(test)]
        batch::note_inspect();
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
            if self.require_discard_scope(state).is_ok()
                && (active || entry.discard_intent.is_some())
            {
                // A durable intent keeps its final receipt reservation even
                // after restart. Ordinary inactive orphans gain no cleanup
                // authority or unused-payload reservation.
                report.reserved_bytes = report.reserved_bytes.saturating_add(
                    remaining_frames(entry)
                        .saturating_mul((FRAME_HEADER + limits.record_bytes) as u64),
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
                        && entry.discard_intent.is_none()
                        && !entry.completed(),
                    discard_requested: entry.discard_intent.is_some(),
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
    /// Borrow the original stage handle for inspection/read/seek. This is not
    /// an OS-readonly capability: File's existing rights are unchanged. Any
    /// duplicated writer must be retired before seal so final metadata/bytes
    /// can be checked through the original stage without delayed-close writes.
    pub fn file(&self) -> &File {
        self.stage.file()
    }
    pub fn file_mut(&mut self) -> &mut File {
        self.stage.file_mut()
    }
    /// Cancel a generated input through the shared durable registry. Close all
    /// duplicated handles first. The exact sealed intent is synced before any
    /// discard; uncertain outcomes retain that intent for reconciliation.
    pub fn discard(self) -> io::Result<DiscardOutcome> {
        {
            let state = self.active.area.state()?;
            self.active.area.require_discard_scope(&state)?;
        }
        self.seal()?.discard()
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
    /// Discard only this allocated generated lease, never its target. Darwin
    /// follows private-workspace POSIX unlinkat under cooperative locks after
    /// full content/metadata/name checks, not atomic compare-delete against a
    /// noncooperating namespace mutator. Unknown observed changes are refused.
    pub fn discard(self) -> io::Result<DiscardOutcome> {
        let area = self.active.area.clone();
        let mut state = area.state()?;
        let step = self.discard_locked(&area, &mut state, None);
        // Active::drop takes this mutex. Retire or return owners only after
        // unlocking, including every pre-intent rejection.
        drop(state);
        step.into_single()
    }
    fn discard_locked(
        self,
        area: &Area,
        state: &mut State,
        mut ledger: Option<&mut batch::Ledger>,
    ) -> batch::DiscardStep {
        let intent = DiscardIntent {
            allocation: self.allocation.clone(),
            evidence: self.sealed.evidence().clone(),
            metadata: self.sealed.metadata().clone(),
        };
        let expected_receipt = discarded_receipt(&intent);
        let request = Record::DiscardIntent {
            ticket: self.allocation.ticket,
            intent: intent.clone(),
        };
        let admission = (|| {
            area.require_discard_scope(state)?;
            area.check_capacity(
                state,
                &[
                    request.clone(),
                    Record::DiscardReceipt {
                        ticket: self.allocation.ticket,
                        receipt: expected_receipt.clone(),
                    },
                ],
            )?;
            area.append_locked_with_ledger(state, request, false, ledger.as_deref_mut())
        })();
        if let Err(error) = admission {
            return batch::DiscardStep::Rejected {
                ticket: self,
                error,
            };
        }
        let Self {
            sealed,
            allocation,
            active,
        } = self;
        #[cfg(test)]
        if let Some(interfere) = area.inner.after_discard_intent.lock().unwrap().take() {
            interfere();
        }
        match sealed.discard() {
            Ok(NativeDiscardOutcome::Discarded { receipt }) => {
                let recorded = (|| {
                    if let Some(ledger) = ledger.as_deref_mut() {
                        ledger.removed(area, &receipt)?;
                    }
                    area.append_locked_with_ledger(
                        state,
                        Record::DiscardReceipt {
                            ticket: allocation.ticket,
                            receipt: receipt.clone(),
                        },
                        false,
                        ledger,
                    )
                })();
                let outcome = match recorded {
                    Ok(()) => DiscardOutcome::Discarded { receipt },
                    Err(error) => DiscardOutcome::MayHaveDiscarded {
                        ticket: None,
                        receipt,
                        error,
                    },
                };
                batch::DiscardStep::Finished {
                    outcome,
                    retired: Some(active),
                }
            }
            Ok(NativeDiscardOutcome::MayHaveDiscarded {
                stage,
                receipt,
                error,
            }) => {
                // Retain the original object on native failure, including an
                // anonymous source. Dropping this returned owner is explicit;
                // a pending disposition may finish then, so intent recovery
                // must never interpret May as a definite non-removal.
                let (ticket, retired) = if let Some(sealed) = stage {
                    (
                        Some(Self {
                            sealed,
                            allocation,
                            active,
                        }),
                        None,
                    )
                } else {
                    (None, Some(active))
                };
                batch::DiscardStep::Finished {
                    outcome: DiscardOutcome::MayHaveDiscarded {
                        ticket,
                        receipt,
                        error,
                    },
                    retired,
                }
            }
            Err(error) => {
                let mut receipt = expected_receipt;
                receipt.discard_confirmed = false;
                receipt.directory_synced = false;
                receipt.source_residue =
                    observed_residue(area.directory(), &intent).unwrap_or(SourceResidue::Unknown);
                batch::DiscardStep::Finished {
                    outcome: DiscardOutcome::MayHaveDiscarded {
                        ticket: Some(Self { sealed: error.stage, allocation, active }), receipt,
                        error: io::Error::other(format!("Discard intent durable; sealed source recheck failed and evidence retained: {}", error.error)),
                    },
                    retired: None,
                }
            }
        }
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
    child_leaf(parent, &leaf, create, private)
}
fn child_leaf(
    parent: &Directory,
    leaf: &Leaf,
    create: bool,
    private: bool,
) -> io::Result<Directory> {
    let directory = if create {
        match parent.create_private(leaf, Traversal::SameFilesystem) {
            Ok(directory) => {
                parent.sync()?;
                directory
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                parent.child(leaf, Traversal::SameFilesystem)?
            }
            Err(error) => return Err(error),
        }
    } else {
        parent.child(leaf, Traversal::SameFilesystem)?
    };
    if private {
        directory.metadata()?.require_private()?;
    }
    parent.recheck(leaf, &directory.identity()?)?;
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
fn is_discard(record: &Record) -> bool {
    matches!(
        record,
        Record::DiscardIntent { .. } | Record::DiscardReceipt { .. }
    )
}
fn remaining_frames(entry: &Entry) -> u64 {
    if entry.receipt.is_some() || entry.discard_receipt.is_some() {
        0
    } else if entry.discard_intent.is_some() {
        1
    } else if entry.exists {
        2
    } else if entry.sealed.is_some() {
        3
    } else {
        4
    }
}
fn record_ticket(record: &Record) -> Option<u64> {
    match record {
        Record::Header { .. } => None,
        Record::Allocation { allocation } => Some(allocation.ticket),
        Record::Sealed { ticket, .. }
        | Record::Receipt { ticket, .. }
        | Record::Exists { ticket }
        | Record::DiscardIntent { ticket, .. }
        | Record::DiscardReceipt { ticket, .. } => Some(*ticket),
    }
}
fn discarded_receipt(intent: &DiscardIntent) -> DiscardReceipt {
    DiscardReceipt {
        source: intent.evidence.clone(),
        metadata: intent.metadata.clone(),
        source_residue: SourceResidue::None,
        discard_confirmed: true,
        directory_synced: true,
    }
}
fn observed_residue(directory: &Directory, intent: &DiscardIntent) -> io::Result<SourceResidue> {
    let Some(name) = &intent.evidence.source_name else {
        return Ok(SourceResidue::None);
    };
    Ok(match directory.entry(&Leaf::new(name)?)? {
        None => SourceResidue::None,
        Some(stamp) if stamp.identity == intent.evidence.identity => SourceResidue::LeasePresent,
        Some(_) => SourceResidue::NameReplaced,
    })
}
fn check_discard_hash(file: &File, intent: &DiscardIntent) -> io::Result<()> {
    let before = retained_fs::metadata(file)?;
    before.require_private()?;
    if before != intent.metadata {
        return Err(invalid(
            "Discard source full metadata changed; source retained",
        ));
    }
    let mut read = file.try_clone()?;
    read.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let count = read.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| invalid("Discard source length overflow"))?;
        if bytes > intent.evidence.len {
            return Err(invalid("Discard source grew; source retained"));
        }
        hash.update(&buffer[..count]);
    }
    if bytes != intent.evidence.len
        || <[u8; 32]>::from(hash.finalize()) != intent.evidence.sha256
        || retained_fs::metadata(file)? != before
    {
        return Err(invalid(
            "Discard source SHA-256 or metadata changed; source retained",
        ));
    }
    // Retire this duplicate before disposition so Windows last-close semantics
    // cannot be mistaken for a completed source disappearance.
    drop(read);
    Ok(())
}
fn discard_intended_source(directory: &Directory, intent: &DiscardIntent) -> io::Result<()> {
    directory.metadata()?.require_private()?;
    if directory.identity()? != intent.evidence.parent_identity {
        return Err(invalid("Discard Area identity changed"));
    }
    if let Some(name) = &intent.evidence.source_name {
        let name = Leaf::new(name)?;
        if directory.entry(&name)?.is_some() {
            #[cfg(windows)]
            {
                let held = directory.read_lease(&name, true)?;
                check_discard_hash(held.file(), intent)?;
                held.recheck()?;
                match held.remove_generated()? {
                    retained_fs::WorkspaceRemoval::Removed => {}
                    retained_fs::WorkspaceRemoval::MayHaveRemoved { error } => return Err(error),
                }
            }
            #[cfg(unix)]
            {
                let held = directory.open_regular(&name, Links::Single)?;
                // Same cooperative source lock as native named Stage, never a
                // claim of atomic unlink against noncooperating path mutation.
                use std::os::fd::AsRawFd;
                if unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                check_discard_hash(&held, intent)?;
                directory.recheck(&name, &intent.evidence.identity)?;
                match directory.remove_workspace_entry(&name, &intent.metadata)? {
                    retained_fs::WorkspaceRemoval::Removed => {}
                    retained_fs::WorkspaceRemoval::MayHaveRemoved { error } => return Err(error),
                }
            }
        }
        if directory.entry(&name)?.is_some() {
            return Err(invalid("Discard source remains or its name was replaced"));
        }
    }
    // Anonymous allocations had no namespace link and their original owner is
    // no longer active. Missing named sources are accepted only from this
    // fully validated durable intent, never from an arbitrary dropped ticket.
    directory.sync()?;
    if observed_residue(directory, intent)? != SourceResidue::None {
        return Err(invalid(
            "Discard source name reappeared during recovery sync",
        ));
    }
    Ok(())
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
            ..
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
                || entry.discard_intent.is_some()
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
                || entry.discard_intent.is_some()
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
            if entry.sealed.is_none()
                || entry.receipt.is_some()
                || entry.exists
                || entry.discard_intent.is_some()
            {
                return Err(invalid("Duplicate or inconsistent collision record"));
            }
            Ok(())
        }
        Record::DiscardIntent { ticket, intent } => {
            let entry = state
                .entries
                .get(ticket)
                .ok_or_else(|| invalid("Discard intent has no allocation"))?;
            if entry.discard_intent.is_some()
                || entry.receipt.is_some()
                || intent.allocation != entry.allocation
                || entry.sealed.as_ref() != Some(&intent.evidence)
                || !matches_allocation(&intent.evidence, &entry.allocation)
                || intent.metadata.identity != intent.evidence.identity
                || intent.metadata.kind != Kind::Regular
                || intent.metadata.reparse_tag.is_some()
                || intent.metadata.len != intent.evidence.len
                || intent.metadata.links
                    != if intent.evidence.source_name.is_some() {
                        1
                    } else {
                        0
                    }
                || intent.metadata.require_private().is_err()
            {
                return Err(invalid(
                    "Duplicate/inconsistent discard intent; source not authorized",
                ));
            }
            Ok(())
        }
        Record::DiscardReceipt { ticket, receipt } => {
            let entry = state
                .entries
                .get(ticket)
                .ok_or_else(|| invalid("Discard receipt has no allocation"))?;
            let intent = entry
                .discard_intent
                .as_ref()
                .ok_or_else(|| invalid("Discard receipt has no durable intent"))?;
            if entry.discard_receipt.is_some()
                || entry.receipt.is_some()
                || receipt != &discarded_receipt(intent)
            {
                return Err(invalid("Duplicate/inconsistent discard receipt"));
            }
            Ok(())
        }
    }
}
fn apply_record(record: Record, sequence: u64, state: &mut State) -> io::Result<()> {
    match record {
        Record::Header { discard_scope, .. } => {
            state.discard_scope = discard_scope;
        }
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
                    discard_intent: None,
                    discard_receipt: None,
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
        Record::DiscardIntent { ticket, intent } => {
            state
                .entries
                .get_mut(&ticket)
                .ok_or_else(|| invalid("Missing discard allocation"))?
                .discard_intent = Some(intent);
        }
        Record::DiscardReceipt { ticket, receipt } => {
            state
                .entries
                .get_mut(&ticket)
                .ok_or_else(|| invalid("Missing discard allocation"))?
                .discard_receipt = Some(receipt);
            state.pending.remove(&ticket);
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
    expected_discard_scope: Option<&DiscardScope>,
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
        let invalid_scope = match &record {
            Record::Header {
                discard_scope: Some(scope),
                ..
            } => Some(scope) != expected_discard_scope,
            record if is_discard(record) => {
                state.discard_scope.is_none()
                    || state.discard_scope.as_ref() != expected_discard_scope
            }
            _ => false,
        };
        if invalid_scope {
            state.status = LogStatus::Corrupt {
                offset: state.valid_end,
                reason: "Discard Header/record has no matching durable custom namespace purpose"
                    .into(),
            };
            return Ok(());
        }
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
        discard_scope: None,
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
