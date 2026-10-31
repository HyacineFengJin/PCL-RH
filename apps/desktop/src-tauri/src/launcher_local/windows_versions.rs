//! Windows launcher-owned logical documents. The physical original is a
//! permanent CAS fence. Immutable no-replace records choose generated payloads;
//! no rename exchange, replacement, or deletion of that original is performed.
//! This deliberately bounded store is not used for arbitrary user/game files.
//! A self-contained local journal is not an external monotonic trust anchor:
//! replay/deletion of the entire sidecar state is outside its rollback claims.
use super::*;
use pcl_core::retained_fs::{Metadata as NativeMetadata, ReadLease, WorkspaceRemoval};
use std::collections::{BTreeMap, BTreeSet};

const MAX_COMMITS: usize = 8192;
const MAX_RECORD: u64 = 64 * 1024;
const MAX_RECORDS: u64 = 32 * 1024 * 1024;
const MAX_PAYLOADS: u64 = 8 * 1024 * 1024;
const HEADER: &str = "header.json";
const PAYLOADS: &str = "payloads";

pub(super) fn document_limit(name: &str) -> u64 {
    match name {
        "instance-metadata.json" => 8 * 1024 * 1024,
        "launcher-preferences.json" => 64 * 1024,
        "resource-favorites.json" => 4 * 1024 * 1024,
        "launcher-0.log" | "launcher-1.log" | "launcher-2.log" | "launcher-3.log" => 1024 * 1024,
        _ => 16 * 1024,
    }
}
pub(super) fn managed(name: &str) -> bool {
    matches!(
        name,
        "active.json"
            | "statistics.json"
            | "launcher-0.log"
            | "launcher-1.log"
            | "launcher-2.log"
            | "launcher-3.log"
            | "launcher-preferences.json"
            | "resource-favorites.json"
            | "instance-metadata.json"
    )
}
fn payload_limit(max_document_bytes: u64) -> u64 {
    // A full-sized current document and its successor must coexist until the
    // latter's durable commit authorizes GC. Orphans share this finite budget.
    MAX_PAYLOADS.max(max_document_bytes.saturating_mul(2))
}
fn payload_limit_error(max_document_bytes: u64) -> String {
    let bytes = payload_limit(max_document_bytes);
    let budget = if bytes % (1024 * 1024) == 0 {
        format!("{} MiB", bytes / (1024 * 1024))
    } else {
        format!("{bytes} 字节")
    };
    fail(format!("payload（包括孤立版本）达到 {budget} 上限"))
}
fn sidecar(name: &str) -> String {
    format!(".local-versions-{name}")
}
fn fail(message: impl std::fmt::Display) -> String {
    format!("Windows 本地文件版本记录无效，所有原件与证据已保留：{message}")
}
fn may(message: impl std::fmt::Display) -> String {
    format!("MayHaveCommitted：本地文件可能已提交，持久化、复核或旧版本清理失败；请重新读取，勿盲目重试：{message}")
}
fn native<T>(value: std::io::Result<T>) -> Result<T, String> {
    value.map_err(fail)
}
fn phase<T>(operation: &str, value: Result<T, String>) -> Result<T, String> {
    value.map_err(|error| format!("{operation}：{error}"))
}
fn native_phase<T>(operation: &str, value: std::io::Result<T>) -> Result<T, String> {
    value.map_err(|error| {
        fail(format!(
            "{operation} [native={:?}]：{error}",
            error.raw_os_error()
        ))
    })
}
fn json<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(fail)
}
fn record_name(index: usize) -> String {
    format!("commit-{index:08}.json")
}
fn payload_name(index: u64) -> String {
    format!("payload-{index:016}")
}
fn payload_index(name: &str) -> Option<u64> {
    let index = name.strip_prefix("payload-")?.parse::<u64>().ok()?;
    (payload_name(index) == name).then_some(index)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    metadata: NativeMetadata,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: String,
    name: String,
    parent: Identity,
    sidecar: Identity,
    payloads: Identity,
    base: Evidence,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    name: String,
    evidence: Evidence,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    sequence: usize,
    previous_sha256: String,
    payload: Payload,
    // Only the immediately previous generated payload is authorized. The base
    // can never appear here; its evidence has no generated payload name.
    gc: Option<Payload>,
}
struct Store {
    directory: Dir,
    payloads: Dir,
    header: Header,
    tail_sha256: String,
    commits: Vec<Commit>,
    record_bytes: u64,
    payload_bytes: u64,
    next_payload: u64,
    base: ReadLease,
    current: Option<ReadLease>,
    records: Vec<ReadLease>,
    directory_names: Vec<String>,
    payload_names: Vec<String>,
}
fn names(dir: &Dir, max: usize) -> Result<Vec<String>, String> {
    let mut names = native(dir.native()?.names(max))?
        .into_iter()
        .map(|name| name.into_string().map_err(|_| fail("非 UTF-8 版本条目")))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    Ok(names)
}
fn lease(dir: &Dir, name: &str, delete: bool) -> Result<ReadLease, String> {
    native(dir.native()?.read_lease(&leaf(name)?, delete))
}
fn read_held(dir: &Dir, name: &str, held: &ReadLease, max: u64) -> Result<Snapshot, String> {
    let snapshot = Snapshot::physical(dir, name, max)?;
    if native(retained_fs::metadata(&snapshot.file))? != *held.metadata() {
        return Err(fail("读取的句柄不是已租用的版本"));
    }
    native(held.recheck())?;
    Ok(snapshot)
}
fn evidence(snapshot: &Snapshot) -> Result<Evidence, String> {
    Ok(Evidence {
        metadata: native(retained_fs::metadata(&snapshot.file))?,
        sha256: snapshot.digest.clone(),
    })
}
fn verify_payload(
    dir: &Dir,
    payload: &Payload,
    delete: bool,
    max: u64,
) -> Result<ReadLease, String> {
    let held = lease(dir, &payload.name, delete)?;
    let snapshot = read_held(dir, &payload.name, &held, max)?;
    if evidence(&snapshot)? != payload.evidence {
        return Err(fail("已生成版本的身份、内容或元数据变化"));
    }
    Ok(held)
}
impl Store {
    fn open(
        parent: &Dir,
        name: &str,
        max: u64,
        delete_current: bool,
    ) -> Result<Option<Self>, String> {
        parent.require_private()?;
        let Some(directory) = parent.optional(&sidecar(name))? else {
            return Ok(None);
        };
        directory.require_private()?;
        let payloads = directory.child(PAYLOADS)?;
        payloads.require_private()?;
        let base = lease(parent, name, false)?;
        let base_snapshot = read_held(parent, name, &base, max)?;
        let header_lease = lease(&directory, HEADER, false)?;
        let mut header_snapshot = read_held(&directory, HEADER, &header_lease, MAX_RECORD)?;
        let header: Header =
            serde_json::from_slice(&header_snapshot.bytes(&directory, HEADER)?).map_err(fail)?;
        if header.format != "windows-local-versions-v1"
            || header.name != name
            || header.parent != parent.identity()?
            || header.sidecar != directory.identity()?
            || header.payloads != payloads.identity()?
            || header.base != evidence(&base_snapshot)?
        {
            return Err(fail("header、目录身份或永久 base fence 变化"));
        }
        let directory_names = names(&directory, MAX_COMMITS + 2)?;
        let record_count = directory_names
            .len()
            .checked_sub(2)
            .ok_or_else(|| fail("缺少 header/payloads"))?;
        let mut expected_names = vec![HEADER.to_owned(), PAYLOADS.to_owned()];
        expected_names.extend((1..=record_count).map(record_name));
        expected_names.sort();
        if directory_names != expected_names || record_count > MAX_COMMITS {
            return Err(fail("未知或不连续的 commit 条目"));
        }
        let mut records = vec![header_lease];
        let mut tail_sha256 = header_snapshot.digest;
        let mut record_bytes = header_snapshot.stamp.bytes;
        let mut commits: Vec<Commit> = Vec::new();
        let mut referenced = BTreeSet::new();
        for index in 1..=record_count {
            let record_name = record_name(index);
            let held = lease(&directory, &record_name, false)?;
            let mut snapshot = read_held(&directory, &record_name, &held, MAX_RECORD)?;
            record_bytes = record_bytes
                .checked_add(snapshot.stamp.bytes)
                .ok_or_else(|| fail("记录大小溢出"))?;
            if record_bytes > MAX_RECORDS {
                return Err(fail("记录达到 32 MiB 上限"));
            }
            let row: Commit =
                serde_json::from_slice(&snapshot.bytes(&directory, &record_name)?).map_err(fail)?;
            if row.sequence != index
                || row.previous_sha256 != tail_sha256
                || row.gc.as_ref() != commits.last().map(|old| &old.payload)
                || payload_index(&row.payload.name).is_none()
                || !referenced.insert(row.payload.name.clone())
            {
                return Err(fail("commit 链、GC 授权或 payload 名称错误"));
            }
            tail_sha256 = snapshot.digest;
            commits.push(row);
            records.push(held);
        }
        let payload_names = names(&payloads, MAX_COMMITS + 1)?;
        let present: BTreeSet<_> = payload_names.iter().cloned().collect();
        let mut payload_bytes = 0u64;
        let mut next_payload = 0u64;
        let mut current = None;
        for payload_name in &payload_names {
            let index =
                payload_index(payload_name).ok_or_else(|| fail("未知或未封存的 payload 条目"))?;
            next_payload = next_payload.max(
                index
                    .checked_add(1)
                    .ok_or_else(|| fail("payload 序号溢出"))?,
            );
            let stamp = native(payloads.native()?.entry(&leaf(payload_name)?))?
                .ok_or_else(|| fail("payload 消失"))?;
            stamp.require_private().map_err(fail)?;
            if stamp.kind != Kind::Regular
                || stamp.reparse_tag.is_some()
                || stamp.links != 1
                || stamp.len == 0
                || stamp.len > max
            {
                return Err(fail("payload 不是有界私有单链接文件"));
            }
            // Includes uncommitted complete orphans; they are never GC'd.
            payload_bytes = payload_bytes
                .checked_add(stamp.len)
                .ok_or_else(|| fail("payload 大小溢出"))?;
            if payload_bytes > payload_limit(max) {
                return Err(payload_limit_error(max));
            }
        }
        for (index, row) in commits.iter().enumerate() {
            if present.contains(&row.payload.name) {
                let is_current = index + 1 == commits.len();
                let held =
                    verify_payload(&payloads, &row.payload, delete_current && is_current, max)?;
                if is_current {
                    current = Some(held);
                }
            } else if commits.get(index + 1).and_then(|next| next.gc.as_ref()) != Some(&row.payload)
            {
                return Err(fail("缺少没有后继 GC 授权的 payload"));
            }
        }
        let store = Self {
            directory,
            payloads,
            header,
            tail_sha256,
            commits,
            record_bytes,
            payload_bytes,
            next_payload,
            base,
            current,
            records,
            directory_names,
            payload_names,
        };
        store.recheck(parent, name)?;
        Ok(Some(store))
    }
    fn recheck(&self, parent: &Dir, name: &str) -> Result<(), String> {
        self.recheck_evidence(parent, name)?;
        if names(&self.directory, MAX_COMMITS + 2)? != self.directory_names
            || names(&self.payloads, MAX_COMMITS + 1)? != self.payload_names
        {
            return Err(fail("版本目录条目在读取期间变化"));
        }
        Ok(())
    }
    fn recheck_evidence(&self, parent: &Dir, name: &str) -> Result<(), String> {
        parent.require_private()?;
        native(
            parent
                .native()?
                .recheck(&leaf(&sidecar(name))?, &self.header.sidecar),
        )?;
        native(
            self.directory
                .native()?
                .recheck(&leaf(PAYLOADS)?, &self.header.payloads),
        )?;
        if parent.identity()? != self.header.parent
            || self.directory.identity()? != self.header.sidecar
            || self.payloads.identity()? != self.header.payloads
        {
            return Err(fail("版本目录身份变化"));
        }
        self.directory.require_private()?;
        self.payloads.require_private()?;
        native(self.base.recheck())?;
        for held in &self.records {
            native(held.recheck())?;
        }
        if let Some(current) = &self.current {
            native(current.recheck())?;
        }
        Ok(())
    }
    fn snapshot(&self, parent: &Dir, name: &str, max: u64) -> Result<Snapshot, String> {
        let snapshot = if let Some(row) = self.commits.last() {
            read_held(
                &self.payloads,
                &row.payload.name,
                self.current
                    .as_ref()
                    .ok_or_else(|| fail("当前 payload 缺失"))?,
                max,
            )?
        } else {
            read_held(parent, name, &self.base, max)?
        };
        self.recheck(parent, name)?;
        Ok(snapshot)
    }
}

pub(super) fn snapshot(parent: &Dir, name: &str, max: u64) -> Result<Option<Snapshot>, String> {
    let max = max.min(document_limit(name));
    if let Some(store) = Store::open(parent, name, max, false)? {
        // Only the ordinary READ/WRITE/DELETE-sharing snapshot survives. Strong
        // leases end here so a cached reader cannot block the next CAS/GC.
        return store.snapshot(parent, name, max).map(Some);
    }
    match parent.file(name)? {
        Some(_) => Snapshot::physical(parent, name, max).map(Some),
        None => Ok(None),
    }
}
fn initialize(parent: &Dir, name: &str, max: u64) -> Result<(), String> {
    let base = phase("initialize/base-lease", lease(parent, name, false))?;
    let base_snapshot = phase(
        "initialize/base-snapshot",
        read_held(parent, name, &base, max),
    )?;
    let native_parent = phase("initialize/parent-handle", parent.native())?;
    // Prepare off-name. A crash cannot expose an empty/half-written sidecar as
    // a valid store; the unbound private workspace remains evidence.
    let (temporary, staged) = native_phase(
        "initialize/create-workspace",
        native_parent.create_private_workspace(),
    )?;
    let directory = Dir(native_phase(
        "initialize/clone-workspace",
        staged.try_clone_file(),
    )?);
    let header = {
        // NTFS may refuse renaming a directory while one of its descendants
        // remains open, including ordinary DELETE-sharing directory handles.
        // Keep the payload directory only through its identity capture and
        // actual namespace flush. The staged root and its full identity stay
        // held across publication; no child capability survives this scope.
        let payloads = phase("initialize/create-payloads", directory.create(PAYLOADS))?;
        let header = Header {
            format: "windows-local-versions-v1".into(),
            name: name.into(),
            parent: phase("initialize/parent-identity", parent.identity())?,
            sidecar: phase("initialize/sidecar-identity", directory.identity())?,
            payloads: phase("initialize/payloads-identity", payloads.identity())?,
            base: phase("initialize/base-evidence", evidence(&base_snapshot))?,
        };
        phase(
            "initialize/write-header",
            directory.write_new(HEADER, &json(&header)?),
        )?;
        phase("initialize/sync-payloads", payloads.sync())?;
        phase("initialize/sync-workspace", directory.sync())?;
        header
    };
    native_phase("initialize/recheck-base", base.recheck())?;
    native_phase(
        "initialize/recheck-workspace",
        native_parent.recheck(&temporary, &header.sidecar),
    )?;
    native_phase(
        "initialize/publish-sidecar-no-replace",
        pcl_core::file_ops::rename_noreplace_at(
            &parent.0,
            temporary.as_os_str(),
            &parent.0,
            sidecar(name).as_ref(),
        ),
    )?;
    // This changes protocol state, but does not change logical document bytes.
    phase("initialize/sync-parent-after-publication", parent.sync()).map_err(may)?;
    native_phase(
        "initialize/recheck-published-sidecar",
        native_parent.recheck(&leaf(&sidecar(name))?, &header.sidecar),
    )
    .map_err(may)
}

/// Recover only already-committed historical GC before admitting new bytes.
/// The current generated payload, physical base and complete orphans are never
/// candidates. Every record was file-synced by Stage before publication; sync
/// the existing namespaces again under held immutable record leases before
/// acting on a recovered authorization (including a prior MayHaveCommitted).
fn recover_authorized_gc(
    store: &mut Store,
    parent: &Dir,
    name: &str,
    max: u64,
) -> Result<(), String> {
    let current = store.commits.last().map(|row| &row.payload);
    let authorized: BTreeMap<String, Payload> = store
        .commits
        .iter()
        .filter_map(|row| row.gc.clone())
        .map(|payload| (payload.name.clone(), payload))
        .collect();
    let mut held = Vec::new();
    for payload in authorized.into_values() {
        if payload.evidence.metadata.identity == store.header.base.metadata.identity
            || current == Some(&payload)
        {
            return Err(fail("恢复 GC 授权指向 base 或当前版本"));
        }
        if !store.payload_names.contains(&payload.name) {
            continue;
        }
        let lease = verify_payload(&store.payloads, &payload, true, max)?;
        held.push((payload, lease));
    }
    if held.is_empty() {
        return Ok(());
    }
    // Admit every exact historical object before deleting any one of them.
    store.recheck(parent, name)?;
    store.directory.sync()?;
    store.payloads.sync()?;
    parent.sync()?;
    store.recheck(parent, name)?;
    for (_, lease) in &held {
        native(lease.recheck())?;
    }
    let mut removed = BTreeSet::new();
    let mut removed_bytes = 0u64;
    for (payload, lease) in held {
        match native(lease.remove_generated())? {
            WorkspaceRemoval::Removed => {}
            WorkspaceRemoval::MayHaveRemoved { error } => return Err(fail(error)),
        }
        removed_bytes = removed_bytes
            .checked_add(payload.evidence.metadata.len)
            .ok_or_else(|| fail("恢复 GC 大小溢出"))?;
        removed.insert(payload.name);
    }
    let mut expected_names = store.payload_names.clone();
    expected_names.retain(|name| !removed.contains(name));
    store.recheck_evidence(parent, name)?;
    if names(&store.directory, MAX_COMMITS + 2)? != store.directory_names
        || names(&store.payloads, MAX_COMMITS + 1)? != expected_names
    {
        return Err(fail("恢复 GC 后目录条目变化"));
    }
    store.payload_bytes = store
        .payload_bytes
        .checked_sub(removed_bytes)
        .ok_or_else(|| fail("恢复 GC 大小与原记录不一致"))?;
    store.payload_names = expected_names;
    // Retain the pre-GC next ordinal: never reuse a reclaimed generated name.
    Ok(())
}

pub(super) fn replace(
    parent: &Dir,
    name: &str,
    expected: Option<&Snapshot>,
    bytes: &[u8],
    max: u64,
) -> Result<(), String> {
    let max = max.min(document_limit(name));
    replace_with_hook(parent, name, expected, bytes, max, || Ok(()))
}
fn replace_with_hook(
    parent: &Dir,
    name: &str,
    expected: Option<&Snapshot>,
    bytes: &[u8],
    max: u64,
    after_durable: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() as u64 > max {
        return Err(fail("新文档为空或超过上限"));
    }
    parent.require_private()?;
    let _writer = phase(
        "replace/writer-lock",
        parent.lock(&format!(".local-versions-{name}.lock")),
    )?;
    let token = |snapshot: &Snapshot| (snapshot.stamp.clone(), snapshot.digest.clone());
    let fresh = phase("replace/read-current", snapshot(parent, name, max))?;
    if fresh.as_ref().map(token) != expected.map(token) {
        return Err(fail("CAS 冲突；本地文件已被外部修改"));
    }
    if expected.is_none() {
        return parent.write_new(name, bytes);
    }
    if parent.optional(&sidecar(name))?.is_none() {
        initialize(parent, name, max)?;
    }
    let mut store = phase("replace/open-store", Store::open(parent, name, max, true))?
        .ok_or_else(|| fail("sidecar 消失"))?;
    let fresh = phase("replace/leased-snapshot", store.snapshot(parent, name, max))?;
    if Some(token(&fresh)) != expected.map(token) {
        return Err(fail("持有原生 lease 后的 CAS 冲突"));
    }
    recover_authorized_gc(&mut store, parent, name, max).map_err(|error| {
        format!("本地文件历史清理未完全确认；本次尚未提交新逻辑版本，原件及其余证据已保留，请重新读取：{error}")
    })?;
    if store.commits.len() >= MAX_COMMITS {
        return Err(fail("达到 8192 次提交上限"));
    }
    if store.payload_names.len() >= MAX_COMMITS + 1 {
        return Err(fail("已生成与孤立 payload 条目达到上限"));
    }
    if store
        .payload_bytes
        .checked_add(bytes.len() as u64)
        .is_none_or(|size| size > payload_limit(max))
    {
        return Err(payload_limit_error(max));
    }
    let payload_name = payload_name(store.next_payload);
    // Preflight record quota before leaving a complete orphan. Actual metadata
    // has the same fixed native shape; final encoded size is checked as well.
    let prospective = Commit {
        sequence: store.commits.len() + 1,
        previous_sha256: store.tail_sha256.clone(),
        payload: Payload {
            name: payload_name.clone(),
            evidence: evidence(&fresh)?,
        },
        gc: store.commits.last().map(|row| row.payload.clone()),
    };
    let preflight_bytes = json(&prospective)?.len() as u64;
    if preflight_bytes + 1024 > MAX_RECORD
        || store.record_bytes + preflight_bytes + 1024 > MAX_RECORDS
    {
        return Err(fail("版本记录达到 32 MiB 上限"));
    }
    phase(
        "replace/recheck-before-payload",
        store.recheck(parent, name),
    )?;
    phase(
        "replace/publish-payload",
        store.payloads.write_new(&payload_name, bytes),
    )?;
    let payload_lease = phase(
        "replace/lease-new-payload",
        lease(&store.payloads, &payload_name, false),
    )?;
    let payload_snapshot = phase(
        "replace/read-new-payload",
        read_held(&store.payloads, &payload_name, &payload_lease, max),
    )?;
    let row = Commit {
        payload: Payload {
            name: payload_name.clone(),
            evidence: evidence(&payload_snapshot)?,
        },
        ..prospective
    };
    let encoded = json(&row)?;
    if encoded.len() as u64 > MAX_RECORD || store.record_bytes + encoded.len() as u64 > MAX_RECORDS
    {
        return Err(fail("新 payload 已保留，版本记录超过上限"));
    }
    store.recheck_evidence(parent, name)?;
    let mut published_payloads = store.payload_names.clone();
    published_payloads.push(payload_name.clone());
    published_payloads.sort();
    if names(&store.directory, MAX_COMMITS + 2)? != store.directory_names
        || names(&store.payloads, MAX_COMMITS + 1)? != published_payloads
    {
        return Err(fail("发布 commit 前版本条目变化，新 payload 已保留"));
    }
    // From this point a failed publication/flush may already have selected the
    // new payload. Never report a definite non-commit or roll back by deleting.
    store
        .directory
        .write_new(&record_name(row.sequence), &encoded)
        .map_err(may)?;
    let after_commit = || -> Result<(), String> {
        let committed_name = record_name(row.sequence);
        let committed_lease = lease(&store.directory, &committed_name, false)?;
        let committed_snapshot = read_held(
            &store.directory,
            &committed_name,
            &committed_lease,
            MAX_RECORD,
        )?;
        if committed_snapshot.digest != format!("{:x}", Sha256::digest(&encoded)) {
            return Err(fail("刚发布的 commit 内容变化"));
        }
        native(
            parent
                .native()?
                .recheck(&leaf(&sidecar(name))?, &store.header.sidecar),
        )?;
        native(
            store
                .directory
                .native()?
                .recheck(&leaf(PAYLOADS)?, &store.header.payloads),
        )?;
        store.directory.require_private()?;
        store.payloads.require_private()?;
        store.directory.sync()?;
        store.payloads.sync()?;
        parent.sync()?;
        native(store.base.recheck())?;
        for held in &store.records {
            native(held.recheck())?;
        }
        native(payload_lease.recheck())?;
        let mut expected_records = store.directory_names.clone();
        expected_records.push(record_name(row.sequence));
        expected_records.sort();
        let mut expected_payloads = store.payload_names.clone();
        expected_payloads.push(payload_name);
        expected_payloads.sort();
        if names(&store.directory, MAX_COMMITS + 2)? != expected_records
            || names(&store.payloads, MAX_COMMITS + 1)? != expected_payloads
        {
            return Err(fail("提交时出现未知条目"));
        }
        // Every historical deletion is explicitly authorized by a successor row.
        // The new row is durable before any cleanup starts. Complete orphans and
        // the permanent physical base are deliberately excluded from this map.
        after_durable()?;
        let mut authorized: BTreeMap<String, Payload> = store
            .commits
            .iter()
            .filter_map(|row| row.gc.clone())
            .map(|payload| (payload.name.clone(), payload))
            .collect();
        if let Some(payload) = &row.gc {
            authorized.insert(payload.name.clone(), payload.clone());
        }
        let gc_names: BTreeSet<_> = authorized.keys().cloned().collect();
        for payload in authorized.into_values() {
            if native(store.payloads.native()?.entry(&leaf(&payload.name)?))?.is_none() {
                continue;
            }
            let held = if store
                .commits
                .last()
                .is_some_and(|old| old.payload == payload)
            {
                store
                    .current
                    .take()
                    .ok_or_else(|| fail("GC 当前旧版本 lease 缺失"))?
            } else {
                verify_payload(&store.payloads, &payload, true, max)?
            };
            match native(held.remove_generated())? {
                WorkspaceRemoval::Removed => {}
                WorkspaceRemoval::MayHaveRemoved { error } => return Err(fail(error)),
            }
        }
        native(committed_lease.recheck())?;
        native(payload_lease.recheck())?;
        store.recheck_evidence(parent, name)?;
        expected_payloads.retain(|name| !gc_names.contains(name));
        if names(&store.directory, MAX_COMMITS + 2)? != expected_records
            || names(&store.payloads, MAX_COMMITS + 1)? != expected_payloads
        {
            return Err(fail("GC 后版本目录条目变化"));
        }
        Ok(())
    };
    after_commit().map_err(may)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = fixture_path(&format!(
                "versions-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let parent = fs::canonicalize(path.parent().unwrap()).unwrap();
            let root = native(Directory::open_absolute(&parent)).unwrap();
            native(root.create_private(
                &leaf(path.file_name().unwrap().to_str().unwrap()).unwrap(),
                Traversal::SameFilesystem,
            ))
            .unwrap();
            Self(parent.join(path.file_name().unwrap()))
        }
        fn dir(&self) -> Dir {
            Dir::absolute(&self.0).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const NAME: &str = "active.json";
    fn read(dir: &Dir) -> Snapshot {
        Snapshot::open(dir, NAME, 16384).unwrap()
    }
    fn ready() -> (Fixture, Dir, Snapshot) {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        dir.replace(NAME, None, b"physical original", 16384)
            .unwrap();
        let base = read(&dir);
        dir.replace(NAME, Some(&base), b"first generated", 16384)
            .unwrap();
        let snapshot = read(&dir);
        (fixture, dir, snapshot)
    }
    fn store(dir: &Dir) -> Store {
        Store::open(dir, NAME, 16384, false).unwrap().unwrap()
    }
    #[test]
    fn open_payload_directory_blocks_whole_workspace_rename_until_released() {
        let fixture = Fixture::new();
        let parent = fixture.dir();
        let native_parent = parent.native().unwrap();
        let (temporary, staged) = native_parent.create_private_workspace().unwrap();
        let directory = Dir(staged.try_clone_file().unwrap());
        let payloads = directory.create(PAYLOADS).unwrap();
        let workspace_id = directory.identity().unwrap();
        let payloads_id = payloads.identity().unwrap();
        directory
            .write_new(HEADER, b"retained prepared header")
            .unwrap();
        payloads.sync().unwrap();
        directory.sync().unwrap();
        let target = sidecar(NAME);
        let blocked = pcl_core::file_ops::rename_noreplace_at(
            &parent.0,
            temporary.as_os_str(),
            &parent.0,
            target.as_ref(),
        )
        .expect_err("held-payload-directory/publish-no-replace unexpectedly succeeded");
        assert!(
            matches!(blocked.raw_os_error(), Some(5 | 32)),
            "held-payload-directory/publish-no-replace unexpected failure: {blocked}"
        );
        native_parent.recheck(&temporary, &workspace_id).unwrap();
        assert!(parent.optional(&target).unwrap().is_none());
        assert_eq!(
            directory.child(PAYLOADS).unwrap().identity().unwrap(),
            payloads_id
        );
        assert_eq!(
            fs::read(fixture.0.join(temporary.as_os_str()).join(HEADER)).unwrap(),
            b"retained prepared header"
        );
        // The source workspace and its header are unchanged. Releasing this
        // descendant, rather than weakening sharing/ACLs or retrying, makes
        // one publication attempt on the prepared state possible.
        drop(payloads);
        pcl_core::file_ops::rename_noreplace_at(
            &parent.0,
            temporary.as_os_str(),
            &parent.0,
            target.as_ref(),
        )
        .expect("released-payload-directory/publish-no-replace");
        parent.sync().unwrap();
        native_parent
            .recheck(&leaf(&target).unwrap(), &workspace_id)
            .unwrap();
        let published = parent.child(&target).unwrap();
        assert_eq!(
            published.child(PAYLOADS).unwrap().identity().unwrap(),
            payloads_id
        );
        assert_eq!(
            fs::read(fixture.0.join(&target).join(HEADER)).unwrap(),
            b"retained prepared header"
        );
        assert!(native_parent.entry(&temporary).unwrap().is_none());
    }
    #[test]
    fn initialized_store_reopens_with_exact_base_header_and_payload_directory() {
        let fixture = Fixture::new();
        let parent = fixture.dir();
        parent.write_new(NAME, b"permanent physical base").unwrap();
        let original = Snapshot::physical(&parent, NAME, 16384).unwrap();
        let original_evidence = evidence(&original).unwrap();
        initialize(&parent, NAME, 16384).unwrap();
        let reopened = fixture.dir();
        let checked = Store::open(&reopened, NAME, 16384, false).unwrap().unwrap();
        assert_eq!(checked.header.base, original_evidence);
        assert_eq!(checked.header.parent, reopened.identity().unwrap());
        assert_eq!(
            checked.header.sidecar,
            checked.directory.identity().unwrap()
        );
        assert_eq!(
            checked.header.payloads,
            checked.payloads.identity().unwrap()
        );
        assert_eq!(
            checked.directory_names,
            vec![HEADER.to_owned(), PAYLOADS.to_owned()]
        );
        assert!(checked.payload_names.is_empty());
        assert!(checked.commits.is_empty());
        let mut selected = checked.snapshot(&reopened, NAME, 16384).unwrap();
        assert_eq!(
            selected.bytes(&reopened, NAME).unwrap(),
            b"permanent physical base"
        );
        assert_eq!(evidence(&selected).unwrap(), original_evidence);
        assert_eq!(
            fs::read(fixture.0.join(NAME)).unwrap(),
            b"permanent physical base"
        );
    }
    #[test]
    fn repeated_cas_reopens_gc_only_generated_and_cached_snapshots_do_not_block_next_write() {
        let (fixture, dir, first) = ready();
        dir.replace(NAME, Some(&first), b"second generated", 16384)
            .unwrap();
        assert!(dir
            .replace(NAME, Some(&first), b"stale overwrite", 16384)
            .is_err());
        assert!(first.recheck(&dir, NAME).is_err());
        let reopened = fixture.dir();
        let mut current = read(&reopened);
        assert_eq!(current.bytes(&reopened, NAME).unwrap(), b"second generated");
        assert_eq!(
            fs::read(fixture.0.join(NAME)).unwrap(),
            b"physical original"
        );
        let checked = store(&reopened);
        assert_eq!(checked.commits.len(), 2);
        assert_eq!(checked.payload_names.len(), 1);
        assert!(checked
            .payloads
            .file(&checked.commits[0].payload.name)
            .unwrap()
            .is_none());
        assert_eq!(
            checked.commits[1].gc.as_ref(),
            Some(&checked.commits[0].payload)
        );
    }
    #[test]
    fn external_base_edit_is_a_permanent_conflict_and_preserves_all_payloads() {
        let (fixture, dir, expected) = ready();
        fs::write(fixture.0.join(NAME), b"external user original").unwrap();
        let before = names(
            &dir.child(&sidecar(NAME)).unwrap().child(PAYLOADS).unwrap(),
            MAX_COMMITS,
        )
        .unwrap();
        assert!(snapshot(&dir, NAME, 16384).is_err());
        assert!(dir
            .replace(NAME, Some(&expected), b"must not publish", 16384)
            .is_err());
        assert_eq!(
            fs::read(fixture.0.join(NAME)).unwrap(),
            b"external user original"
        );
        assert_eq!(
            names(
                &dir.child(&sidecar(NAME)).unwrap().child(PAYLOADS).unwrap(),
                MAX_COMMITS
            )
            .unwrap(),
            before
        );
    }
    #[test]
    fn existing_generated_editor_prevents_cas_without_new_commit() {
        let (fixture, dir, expected) = ready();
        let payload_name = { store(&dir).commits[0].payload.name.clone() };
        let path = fixture
            .0
            .join(sidecar(NAME))
            .join(PAYLOADS)
            .join(payload_name);
        let editor = fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(dir
            .replace(NAME, Some(&expected), b"must not publish", 16384)
            .is_err());
        drop(editor);
        assert_eq!(store(&dir).commits.len(), 1);
        assert_eq!(fs::read(path).unwrap(), b"first generated");
    }
    #[test]
    fn durable_commit_with_failed_cleanup_is_may_committed_and_reopen_validates_present_history() {
        let (fixture, dir, expected) = ready();
        let error = replace_with_hook(&dir, NAME, Some(&expected), b"committed new", 16384, || {
            Err("injected cleanup failure".into())
        })
        .unwrap_err();
        assert!(error.contains("MayHaveCommitted"), "{error}");
        let mut latest = read(&dir);
        assert_eq!(latest.bytes(&dir, NAME).unwrap(), b"committed new");
        let old_name = {
            let checked = store(&dir);
            assert_eq!(checked.payload_names.len(), 2);
            checked.commits[0].payload.name.clone()
        };
        let old_path = fixture.0.join(sidecar(NAME)).join(PAYLOADS).join(old_name);
        fs::write(&old_path, b"tampered historical payload").unwrap();
        assert!(snapshot(&dir, NAME, 16384).is_err());
        assert!(dir
            .replace(NAME, Some(&latest), b"must retain tamper", 16384)
            .is_err());
        assert_eq!(fs::read(old_path).unwrap(), b"tampered historical payload");
    }
    #[test]
    fn uncommitted_complete_orphan_is_preserved_counted_and_never_selected_or_gc_authorized() {
        let (_fixture, dir, expected) = ready();
        let orphan_name = {
            let checked = store(&dir);
            let name = payload_name(checked.next_payload);
            checked
                .payloads
                .write_new(&name, b"complete orphan")
                .unwrap();
            name
        };
        assert_eq!(read(&dir).digest, expected.digest);
        dir.replace(NAME, Some(&expected), b"after interrupted payload", 16384)
            .unwrap();
        let checked = store(&dir);
        assert_eq!(checked.payload_names.len(), 2);
        assert!(checked.payloads.file(&orphan_name).unwrap().is_some());
        assert!(checked
            .commits
            .iter()
            .all(|row| row.payload.name != orphan_name
                && row.gc.as_ref().is_none_or(|gc| gc.name != orphan_name)));
    }
    #[test]
    fn unknown_stage_gaps_current_loss_and_record_tamper_are_refused_without_cleanup() {
        for case in 0..4 {
            let (fixture, dir, expected) = ready();
            let root = fixture.0.join(sidecar(NAME));
            match case {
                0 => fs::write(root.join("unknown-stage"), b"preserve evidence").unwrap(),
                1 => fs::rename(root.join(record_name(1)), root.join(record_name(2))).unwrap(),
                2 => {
                    let name = { store(&dir).commits[0].payload.name.clone() };
                    fs::remove_file(root.join(PAYLOADS).join(name)).unwrap();
                }
                _ => fs::write(root.join(record_name(1)), b"tampered commit").unwrap(),
            }
            assert!(snapshot(&dir, NAME, 16384).is_err(), "case {case}");
            assert!(
                dir.replace(NAME, Some(&expected), b"replacement", 16384)
                    .is_err(),
                "case {case}"
            );
            assert_eq!(
                fs::read(fixture.0.join(NAME)).unwrap(),
                b"physical original"
            );
        }
    }
    #[test]
    fn base_and_sidecar_identity_binding_rejects_transplanted_valid_header() {
        let (fixture, _dir, expected) = ready();
        // Windows refuses moving a directory containing an open child file.
        // Close this reader before transplanting the complete on-disk store;
        // the test exercises header binding, not native sharing refusal.
        drop(expected);
        let second = Fixture::new();
        let other = second.dir();
        other.write_new(NAME, b"physical original").unwrap();
        // Move only a fully valid store, keeping all IDs/content intact. The
        // destination parent/base still must match header's native evidence.
        fs::rename(fixture.0.join(sidecar(NAME)), second.0.join(sidecar(NAME))).unwrap();
        assert!(snapshot(&other, NAME, 16384).is_err());
        assert_eq!(fs::read(second.0.join(NAME)).unwrap(), b"physical original");
    }
    #[test]
    fn orphan_payload_bytes_enforce_quota_before_new_publication() {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        let name = "launcher-0.log";
        dir.write_new(name, b"base").unwrap();
        let original = Snapshot::open(&dir, name, 1024 * 1024).unwrap();
        dir.replace(name, Some(&original), b"generated", 1024 * 1024)
            .unwrap();
        let payloads = dir.child(&sidecar(name)).unwrap().child(PAYLOADS).unwrap();
        for index in 1..=7 {
            payloads
                .write_new(&payload_name(index), &vec![b'x'; 1024 * 1024])
                .unwrap();
        }
        payloads
            .write_new(&payload_name(8), &vec![b'x'; 1024 * 1024 - 16])
            .unwrap();
        let expected = Snapshot::open(&dir, name, 1024 * 1024).unwrap();
        let before = names(&payloads, MAX_COMMITS).unwrap();
        let error = dir
            .replace(name, Some(&expected), &[b'y'; 128], 1024 * 1024)
            .unwrap_err();
        assert!(error.contains("8 MiB"), "{error}");
        assert_eq!(names(&payloads, MAX_COMMITS).unwrap(), before);
        assert_eq!(fs::read(fixture.0.join(name)).unwrap(), b"base");
    }
    #[test]
    fn recovered_gc_frees_full_favorites_quota_before_admitting_next_save() {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        let name = "resource-favorites.json";
        let max = 4 * 1024 * 1024;
        dir.write_new(name, b"permanent base").unwrap();
        let base = Snapshot::open(&dir, name, max).unwrap();
        dir.replace(name, Some(&base), &vec![b'a'; max as usize], max)
            .unwrap();
        let first = Snapshot::open(&dir, name, max).unwrap();
        let error = replace_with_hook(
            &dir,
            name,
            Some(&first),
            &vec![b'b'; max as usize],
            max,
            || Err("injected cleanup failure at full quota".into()),
        )
        .unwrap_err();
        assert!(error.contains("MayHaveCommitted"), "{error}");
        let old_name = {
            let checked = Store::open(&dir, name, max, false).unwrap().unwrap();
            assert_eq!(checked.payload_bytes, MAX_PAYLOADS);
            assert_eq!(checked.commits.len(), 2);
            checked.commits[0].payload.name.clone()
        };
        let reopened = fixture.dir();
        let mut second = Snapshot::open(&reopened, name, max).unwrap();
        assert_eq!(
            second.bytes(&reopened, name).unwrap(),
            vec![b'b'; max as usize]
        );
        // Cached ordinary snapshots survive both recovered GC and the next CAS.
        reopened
            .replace(name, Some(&second), b"third generation", max)
            .unwrap();
        let mut latest = Snapshot::open(&reopened, name, max).unwrap();
        assert_eq!(latest.bytes(&reopened, name).unwrap(), b"third generation");
        let checked = Store::open(&reopened, name, max, false).unwrap().unwrap();
        assert_eq!(checked.commits.len(), 3);
        assert_eq!(checked.payload_names.len(), 1);
        assert_eq!(checked.payload_bytes, b"third generation".len() as u64);
        assert!(checked.payloads.file(&old_name).unwrap().is_none());
        assert_eq!(fs::read(fixture.0.join(name)).unwrap(), b"permanent base");
    }
}
