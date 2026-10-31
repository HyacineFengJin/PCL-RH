//! Optional app-owned documents. Missing is a durable revision, never a magic
//! payload or `None` CAS expectation. The physical base remains the same fence.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum Presence {
    NeverCreated,
    Present,
    Tombstone,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Token {
    parent: Identity,
    name: String,
    max: u64,
    base: Option<Evidence>,
    sidecar: Option<Identity>,
    payloads: Option<Identity>,
    sequence: usize,
    tail: String,
    presence: Presence,
    current: Option<Evidence>,
}
// No constructor, serialization or public fields: absence can only be obtained
// by actually reading this exact authorized document namespace.
pub(crate) struct OwnedState {
    token: Token,
    snapshot: Option<Snapshot>,
}
impl OwnedState {
    pub fn fact_digest(&self) -> String {
        // Every field is a fixed serializable native fact. This digest is
        // journal evidence, never a deserializable CAS capability.
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&self.token).expect("native state facts serialize"))
        )
    }
    pub fn presence(&self) -> Presence {
        self.token.presence
    }
    pub fn generation(&self) -> usize {
        self.token.sequence
    }
}
fn from_store(parent: &Dir, name: &str, max: u64, store: &Store) -> Result<OwnedState, String> {
    let missing = store
        .commits
        .last()
        .is_some_and(|row| row.payload().is_none());
    let snapshot = if missing {
        None
    } else {
        Some(store.snapshot(parent, name, max)?)
    };
    let current = snapshot.as_ref().map(evidence).transpose()?;
    store.recheck(parent, name)?;
    Ok(OwnedState {
        token: Token {
            parent: parent.identity()?,
            name: name.into(),
            max,
            base: Some(store.header.base.clone()),
            sidecar: Some(store.header.sidecar.clone()),
            payloads: Some(store.header.payloads.clone()),
            sequence: store.commits.len(),
            tail: store.tail_sha256.clone(),
            presence: if missing {
                Presence::Tombstone
            } else {
                Presence::Present
            },
            current,
        },
        snapshot,
    })
}
pub(crate) fn state_owned(parent: &Dir, name: &str, max: u64) -> Result<OwnedState, String> {
    component(name)?;
    parent.require_private()?;
    if let Some(store) = Store::open(parent, name, max, false)? {
        return from_store(parent, name, max, &store);
    }
    let snapshot = match parent.file(name)? {
        Some(_) => Some(Snapshot::physical(parent, name, max)?),
        None => None,
    };
    let current = snapshot.as_ref().map(evidence).transpose()?;
    // An independently initialized sidecar must not be mistaken for physical
    // base state. Cooperative writers also serialize all mutations by lock.
    if parent.optional(&sidecar(name))?.is_some() {
        return Err(fail("读取可选文档时 sidecar 已出现，请重新读取"));
    }
    parent.require_private()?;
    Ok(OwnedState {
        token: Token {
            parent: parent.identity()?,
            name: name.into(),
            max,
            base: current.clone(),
            sidecar: None,
            payloads: None,
            sequence: 0,
            tail: String::new(),
            presence: if snapshot.is_some() {
                Presence::Present
            } else {
                Presence::NeverCreated
            },
            current,
        },
        snapshot,
    })
}
fn scope(parent: &Dir, name: &str, max: u64, expected: &OwnedState) -> Result<(), String> {
    if expected.token.parent != parent.identity()?
        || expected.token.name != name
        || expected.token.max != max
    {
        return Err(fail("可选文档 CAS token 不属于此能力"));
    }
    Ok(())
}
fn same(fresh: &OwnedState, expected: &OwnedState) -> Result<(), String> {
    if fresh.token != expected.token {
        return Err(fail("可选文档 generation/presence CAS 冲突，请重新读取"));
    }
    Ok(())
}
pub(crate) fn read_state_owned(
    parent: &Dir,
    name: &str,
    max: u64,
    expected: &mut OwnedState,
) -> Result<Option<Vec<u8>>, String> {
    scope(parent, name, max, expected)?;
    same(&state_owned(parent, name, max)?, expected)?;
    let Some(snapshot) = expected.snapshot.as_mut() else {
        return Ok(None);
    };
    snapshot.file.seek(SeekFrom::Start(0)).map_err(fail)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut snapshot.file)
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(fail)?;
    if bytes.len() as u64 != snapshot.stamp.bytes
        || format!("{:x}", Sha256::digest(&bytes)) != snapshot.digest
        || Some(evidence(snapshot)?) != expected.token.current
    {
        return Err(fail("可选文档在读取期间变化"));
    }
    same(&state_owned(parent, name, max)?, expected)?;
    Ok(Some(bytes))
}
pub(crate) fn compare_owned(
    parent: &Dir,
    name: &str,
    max: u64,
    expected: &OwnedState,
    bytes: Option<&[u8]>,
) -> Result<(), String> {
    compare_with_hook(parent, name, max, expected, bytes, || Ok(()))
}
fn compare_with_hook(
    parent: &Dir,
    name: &str,
    max: u64,
    expected: &OwnedState,
    bytes: Option<&[u8]>,
    after_durable: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    scope(parent, name, max, expected)?;
    if bytes.is_some_and(|bytes| bytes.is_empty() || bytes.len() as u64 > max) {
        return Err(fail("新文档为空或超过上限"));
    }
    parent.require_private()?;
    let _writer = parent.lock(&format!("{}.lock", sidecar(name)))?;
    same(&state_owned(parent, name, max)?, expected)?;
    if bytes.is_none() && expected.presence() != Presence::Present {
        // Exact absent token checked while holding the same writer lock. This
        // no-op does not manufacture a tombstone for a never-created document.
        return Ok(());
    }
    if expected.presence() == Presence::NeverCreated {
        let bytes = bytes.ok_or_else(|| fail("缺少新文档内容"))?;
        // Once publication begins an error can follow a successful native
        // publish/flush. Preserve that uncertainty, including post-publish
        // verification failures, and never delete the physical base to retry.
        parent.write_new(name, bytes).map_err(may)?;
        return after_durable().map_err(may);
    }
    let initialized = expected.token.sidecar.is_none();
    if initialized {
        initialize(parent, name, max)?;
    }
    let mut store = Store::open(parent, name, max, true)?.ok_or_else(|| fail("sidecar 消失"))?;
    let held = from_store(parent, name, max, &store)?;
    if initialized {
        // Our own initialization changes only protocol identity, not the base
        // or selected document. All pre-existing evidence must still match.
        if !store.commits.is_empty()
            || held.token.base != expected.token.base
            || held.token.current != expected.token.current
            || held.presence() != expected.presence()
        {
            return Err(fail("初始化后可选文档 CAS 冲突"));
        }
    } else {
        same(&held, expected)?;
    }
    drop(held);
    recover_authorized_gc(&mut store, parent, name, max)
        .map_err(|error| format!("本次尚未提交新逻辑版本，历史清理待恢复：{error}"))?;
    if store.commits.len() >= MAX_COMMITS {
        return Err(fail("达到 8192 次提交上限"));
    }
    if let Some(bytes) = bytes {
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
    }
    let next_name = payload_name(store.next_payload);
    let mut row = CommitV2 {
        record_version: 2,
        sequence: store.commits.len() + 1,
        previous_sha256: store.tail_sha256.clone(),
        value: match bytes {
            Some(_) => ValueV2::Present {
                payload: Payload {
                    name: next_name.clone(),
                    evidence: store.header.base.clone(),
                },
            },
            None => ValueV2::Missing {},
        },
        gc: store.commits.last().and_then(Row::payload).cloned(),
    };
    let preflight = json(&row)?.len() as u64;
    if preflight + 1024 > MAX_RECORD || store.record_bytes + preflight + 1024 > MAX_RECORDS {
        return Err(fail("版本记录达到上限"));
    }
    store.recheck(parent, name)?;
    let held = if let Some(bytes) = bytes {
        store.payloads.write_new(&next_name, bytes)?;
        let held = lease(&store.payloads, &next_name, false)?;
        let snapshot = read_held(&store.payloads, &next_name, &held, max)?;
        row.value = ValueV2::Present {
            payload: Payload {
                name: next_name,
                evidence: evidence(&snapshot)?,
            },
        };
        Some(held)
    } else {
        None
    };
    publish_row(
        &mut store,
        parent,
        name,
        max,
        Row::V2(row),
        held,
        after_durable,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const NAME: &str = "optional-document.json";
    const MAX: u64 = 65536;
    fn fixture() -> (tempfile::TempDir, Dir) {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let directory = root
            .create_private(&leaf("documents").unwrap(), Traversal::AllowMounts)
            .unwrap();
        (temporary, Dir(directory.try_clone_file().unwrap()))
    }
    fn state(dir: &Dir) -> OwnedState {
        state_owned(dir, NAME, MAX).unwrap()
    }
    fn write(dir: &Dir, expected: &OwnedState, bytes: Option<&[u8]>) {
        compare_owned(dir, NAME, MAX, expected, bytes).unwrap();
    }
    fn bytes(dir: &Dir) -> Option<Vec<u8>> {
        read_state_owned(dir, NAME, MAX, &mut state(dir)).unwrap()
    }
    #[test]
    fn lifecycle_retains_base_and_requires_a_real_absent_generation() {
        let (_temporary, dir) = fixture();
        let never = state(&dir);
        assert_eq!(never.presence(), Presence::NeverCreated);
        assert_eq!(names(&dir, 10).unwrap(), Vec::<String>::new());
        write(&dir, &never, Some(b"physical base"));
        let base = evidence(&Snapshot::physical(&dir, NAME, MAX).unwrap()).unwrap();
        let present = state(&dir);
        write(&dir, &present, None);
        let missing = state(&dir);
        assert_eq!(missing.presence(), Presence::Tombstone);
        assert_eq!(missing.generation(), 1);
        assert_eq!(bytes(&dir), None);
        assert!(snapshot_owned(&dir, NAME, MAX).is_err());
        assert!(replace_owned(&dir, NAME, None, b"wrong legacy create", MAX).is_err());
        assert!(compare_owned(&dir, NAME, MAX, &never, Some(b"stale create")).is_err());
        write(&dir, &missing, Some(b"reactivated"));
        assert_eq!(bytes(&dir).unwrap(), b"reactivated");
        write(&dir, &state(&dir), None);
        assert!(compare_owned(&dir, NAME, MAX, &missing, Some(b"ABA")).is_err());
        assert_eq!(
            evidence(&Snapshot::physical(&dir, NAME, MAX).unwrap()).unwrap(),
            base
        );
        assert_eq!(
            Snapshot::physical(&dir, NAME, MAX).unwrap().header,
            b"physical base"
        );
    }
    #[test]
    fn first_publication_hook_failure_is_may_committed_and_retains_present_base() {
        let (_temporary, dir) = fixture();
        let never = state(&dir);
        let error = compare_with_hook(
            &dir,
            NAME,
            MAX,
            &never,
            Some(b"first published base"),
            || Err("simulated failure after first publication".into()),
        )
        .unwrap_err();
        assert!(error.starts_with("MayHaveCommitted"), "{error}");
        let current = state(&dir);
        assert_eq!(current.presence(), Presence::Present);
        assert_eq!(bytes(&dir).unwrap(), b"first published base");
        assert_eq!(
            Snapshot::physical(&dir, NAME, MAX).unwrap().header,
            b"first published base"
        );
        assert!(compare_owned(&dir, NAME, MAX, &never, Some(b"blind retry")).is_err());
        assert_eq!(bytes(&dir).unwrap(), b"first published base");
    }
    #[test]
    fn legacy_bytes_chain_into_v2_without_reusing_gc_payload_names() {
        let (_temporary, dir) = fixture();
        dir.write_new(NAME, b"base").unwrap();
        let base = Snapshot::physical(&dir, NAME, MAX).unwrap();
        replace_owned(&dir, NAME, Some(&base), b"legacy current", MAX).unwrap();
        let store = Store::open(&dir, NAME, MAX, false).unwrap().unwrap();
        let old_name = store.commits[0].payload().unwrap().name.clone();
        let header = Snapshot::physical(&store.directory, HEADER, MAX_RECORD)
            .unwrap()
            .header;
        let old_row = Snapshot::physical(&store.directory, &record_name(1), MAX_RECORD)
            .unwrap()
            .header;
        drop(store);
        write(&dir, &state(&dir), None);
        let store = Store::open(&dir, NAME, MAX, false).unwrap().unwrap();
        assert!(store.payload_names.is_empty());
        drop(store);
        write(&dir, &state(&dir), Some(b"new current"));
        let store = Store::open(&dir, NAME, MAX, false).unwrap().unwrap();
        assert_ne!(store.commits[2].payload().unwrap().name, old_name);
        assert_eq!(
            Snapshot::physical(&store.directory, HEADER, MAX_RECORD)
                .unwrap()
                .header,
            header
        );
        assert_eq!(
            Snapshot::physical(&store.directory, &record_name(1), MAX_RECORD)
                .unwrap()
                .header,
            old_row
        );
        assert_eq!(
            store.commits[1].previous(),
            format!("{:x}", Sha256::digest(&old_row))
        );
        drop(store);
        let current = snapshot_owned(&dir, NAME, MAX).unwrap().unwrap();
        replace_owned(&dir, NAME, Some(&current), b"v1 again", MAX).unwrap();
        assert_eq!(bytes(&dir).unwrap(), b"v1 again");
    }
    #[test]
    fn foreign_document_and_changed_missing_tokens_are_rejected() {
        let (_temporary, dir) = fixture();
        let never = state(&dir);
        assert!(compare_owned(&dir, "other.json", MAX, &never, Some(b"escape")).is_err());
        assert!(compare_owned(&dir, NAME, MAX / 2, &never, Some(b"wrong cap")).is_err());
        write(&dir, &never, Some(b"base"));
        write(&dir, &state(&dir), None);
        let mut missing = state(&dir);
        write(&dir, &state(&dir), Some(b"replacement"));
        assert!(read_state_owned(&dir, NAME, MAX, &mut missing).is_err());
        assert!(compare_owned(&dir, NAME, MAX, &missing, None).is_err());
        assert_eq!(bytes(&dir).unwrap(), b"replacement");
    }
    #[test]
    fn durable_tombstone_and_reactivation_failures_are_may_committed_and_recoverable() {
        let (_temporary, dir) = fixture();
        write(&dir, &state(&dir), Some(b"base"));
        write(&dir, &state(&dir), Some(b"generated"));
        let error = compare_with_hook(&dir, NAME, MAX, &state(&dir), None, || {
            Err("simulated crash after durable tombstone".into())
        })
        .unwrap_err();
        assert!(error.starts_with("MayHaveCommitted"), "{error}");
        assert_eq!(state(&dir).presence(), Presence::Tombstone);
        let store = Store::open(&dir, NAME, MAX, false).unwrap().unwrap();
        assert_eq!(store.payload_names.len(), 1); // GC was interrupted.
        store
            .payloads
            .write_new(&payload_name(40), b"complete orphan")
            .unwrap();
        drop(store);
        let error = compare_with_hook(&dir, NAME, MAX, &state(&dir), Some(b"reactivated"), || {
            Err("simulated crash after durable present".into())
        })
        .unwrap_err();
        assert!(error.starts_with("MayHaveCommitted"), "{error}");
        assert_eq!(bytes(&dir).unwrap(), b"reactivated");
        let store = Store::open(&dir, NAME, MAX, false).unwrap().unwrap();
        assert_eq!(
            store.commits.last().unwrap().payload().unwrap().name,
            payload_name(41)
        );
        assert!(store.payload_names.contains(&payload_name(40)));
        assert_eq!(store.payload_names.len(), 2);
    }
    #[test]
    fn strict_v2_decoder_rejects_future_versions_and_ambiguous_missing_values() {
        let good = br#"{"record_version":2,"sequence":1,"previous_sha256":"hash","value":{"state":"missing"},"gc":null}"#;
        assert!(Row::decode(good).unwrap().payload().is_none());
        let mut value: serde_json::Value = serde_json::from_slice(good).unwrap();
        value["record_version"] = 3.into();
        assert!(Row::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value["record_version"] = 2.into();
        value["value"]["payload"] = serde_json::Value::Null;
        assert!(Row::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value["value"] = serde_json::json!({"state":"present"});
        assert!(Row::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        let (_temporary, dir) = fixture();
        write(&dir, &state(&dir), Some(b"base"));
        write(&dir, &state(&dir), None);
        let store = Store::open(&dir, NAME, MAX, false).unwrap().unwrap();
        let bad = serde_json::json!({"record_version":3,"sequence":2,
            "previous_sha256":store.tail_sha256,"value":{"state":"missing"},"gc":null});
        store
            .directory
            .write_new(&record_name(2), &serde_json::to_vec(&bad).unwrap())
            .unwrap();
        drop(store);
        assert!(state_owned(&dir, NAME, MAX).is_err());
        assert_eq!(Snapshot::physical(&dir, NAME, MAX).unwrap().header, b"base");
    }
    #[test]
    fn full_payload_quota_does_not_block_tombstone_or_reactivation_after_gc() {
        let (_temporary, dir) = fixture();
        let max = 4 * 1024 * 1024;
        let content = vec![b'x'; max as usize];
        let state = || state_owned(&dir, NAME, max).unwrap();
        compare_owned(&dir, NAME, max, &state(), Some(b"base")).unwrap();
        compare_owned(&dir, NAME, max, &state(), Some(&content)).unwrap();
        let store = Store::open(&dir, NAME, max, false).unwrap().unwrap();
        store
            .payloads
            .write_new(&payload_name(50), &content)
            .unwrap();
        drop(store);
        let before = state();
        assert!(compare_owned(&dir, NAME, max, &before, Some(b"over quota")).is_err());
        compare_owned(&dir, NAME, max, &before, None).unwrap();
        assert_eq!(state().presence(), Presence::Tombstone);
        compare_owned(&dir, NAME, max, &state(), Some(&content)).unwrap();
        let store = Store::open(&dir, NAME, max, false).unwrap().unwrap();
        assert_eq!(store.payload_bytes, payload_limit(max));
        assert_eq!(
            store.payload_names,
            vec![payload_name(50), payload_name(51)]
        );
        assert_eq!(Snapshot::physical(&dir, NAME, max).unwrap().header, b"base");
    }
}
