//! Durable manifests, small launch-status records and inode ownership receipts.
//! Full manifests are written only at phase boundaries. Root pending markers
//! outlive filesystem changes and are removed only after durable completion.
use super::*;
const SMALL: u64 = 16 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    root: Key,
    versions: Key,
    instance: Key,
    history: Key,
    operation: Key,
    pcl: Key,
    markers: Key,
}
impl Binding {
    fn of(j: &Journal) -> Result<Self> {
        Ok(Self {
            root: j.scope.root.clone(),
            versions: j.scope.versions.clone(),
            instance: j.scope.instance.clone(),
            history: j.scope.history.clone().ok_or("更新历史身份缺失")?,
            operation: j.scope.operation.clone().ok_or("更新记录身份缺失")?,
            pcl: j.scope.pcl.clone().ok_or("更新根标记身份缺失")?,
            markers: j.scope.markers.clone().ok_or("更新根标记目录身份缺失")?,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Marker {
    schema: u32,
    pub(super) id: String,
    binding: Binding,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    schema: u32,
    id: String,
    state: State,
    binding: Binding,
    journal_token: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Receipt {
    Marker {
        schema: u32,
        operation: Key,
        owned: Owned,
    },
    Copy {
        schema: u32,
        operation: Key,
        index: usize,
        backup: bool,
        owned: Owned,
    },
    Directory {
        schema: u32,
        operation: Key,
        kind: String,
        key: Key,
    },
}
#[cfg(test)]
thread_local! {pub(super) static FAIL_COMMITTED_WRITE:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};pub(super) static FULL_WRITES:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
fn bytes(dir: &Dir, name: &str, limit: u64) -> Result<Vec<u8>> {
    #[cfg(not(target_os = "linux"))]
    if matches!(name, "journal.json" | "status.json" | "owned-marker.json") {
        let mut state = dir.document_state(name, limit)?;
        return dir
            .read_document(name, limit, &mut state)?
            .ok_or("资源业务文档缺失".into());
    }
    let mut file = dir.regular(name)?;
    let token = super::super::token_for(&file)?;
    if file.metadata().map_err(err)?.len() > limit || !filesystem::single(&file)? {
        return Err("更新记录过大或含额外硬链接".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() as u64 > limit
        || super::super::token_for(&file)? != token
        || super::super::token_for(&dir.regular(name)?)? != token
    {
        return Err("更新记录读取期间被修改或替换".into());
    }
    Ok(bytes)
}
fn owned_ok(owned: &Owned) -> bool {
    owned.size <= MAX_FILE_BYTES && valid_hash(&owned.sha512) && owned.regular()
}
fn validate_inventory(value: &Inventory) -> Result<()> {
    if value.keys().map(String::as_str).collect::<BTreeSet<_>>() != KINDS.into_iter().collect() {
        return Err("更新 inventory 类型无效".into());
    }
    let mut n = 0;
    let mut size = 0u64;
    for nodes in value.values() {
        for (name, node) in nodes {
            super::super::safe_name(name)?;
            n += 1;
            if n > 2048 {
                return Err("更新 inventory 数量无效".into());
            }
            match node {
                Node::File { owned } => {
                    if !owned_ok(owned) {
                        return Err("更新 inventory 文件声明无效".into());
                    }
                    size = size
                        .checked_add(owned.size)
                        .filter(|s| *s <= 16 * 1024 * 1024 * 1024)
                        .ok_or("更新 inventory 过大")?;
                }
                #[cfg(unix)]
                Node::Directory { mode, .. } => {
                    if mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32 {
                        return Err("更新 inventory 目录声明无效".into());
                    }
                }
                #[cfg(windows)]
                Node::Directory { security, .. } => {
                    // Persisted evidence must belong to this platform. Exact
                    // ID/security equality against the live directory remains
                    // mandatory in Context inventory checks before any move.
                    if !matches!(security, pcl_core::retained_fs::Security::Windows {
                        owner, descriptor, ..
                    } if !owner.is_empty() && !descriptor.is_empty())
                    {
                        return Err("更新 inventory 原生目录声明无效".into());
                    }
                }
            }
        }
    }
    Ok(())
}
fn validate(j: &Journal, id: &str) -> Result<()> {
    if j.schema != SCHEMA
        || j.id != id
        || !valid_id(id)
        || j.items.is_empty()
        || j.items.len() > MAX_FILES
    {
        return Err("资源更新记录格式无效".into());
    }
    Binding::of(j)?;
    validate_inventory(&j.before)?;
    if !j.state.partial() {
        validate_inventory(&j.after)?;
    } else if !j.after.is_empty() {
        validate_inventory(&j.after)?;
    }
    if j.scope
        .resources
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != KINDS.into_iter().collect()
    {
        return Err("资源更新绑定类型无效".into());
    }
    for b in j.scope.resources.values() {
        if b.before.is_some() && b.created.is_some() {
            return Err("资源目录所有权重复".into());
        }
    }
    let mut names = BTreeSet::new();
    let mut oldnames = BTreeSet::new();
    let mut total = 0u64;
    let mut oldsize = 0u64;
    for item in &j.items {
        name_ok(&item.kind, &item.name)?;
        if item.size == 0
            || item.size > MAX_FILE_BYTES
            || !valid_hash(&item.sha512)
            || !names.insert((&item.kind, &item.name))
        {
            return Err("更新下载声明无效".into());
        }
        total = total
            .checked_add(item.size)
            .filter(|s| *s <= MAX_BATCH_BYTES)
            .ok_or("更新下载声明过大")?;
        if let Some(owned) = &item.new {
            if !owned_ok(owned) || owned.size != item.size || owned.sha512 != item.sha512 {
                return Err("更新新文件所有权无效".into());
            }
        } else if !j.state.partial() {
            return Err("更新新文件缺少持久所有权".into());
        }
        if let Some(old) = &item.old {
            name_ok(&item.kind, &old.name)?;
            if !owned_ok(&old.owned)
                || !super::super::valid_token(&old.fingerprint)
                || !oldnames.insert((&item.kind, &old.name))
                || old.name.ends_with(".disabled") != item.name.ends_with(".disabled")
                || j.before[&item.kind].get(&old.name)
                    != Some(&Node::File {
                        owned: old.owned.clone(),
                    })
            {
                return Err("更新旧文件声明无效".into());
            }
            oldsize = oldsize
                .checked_add(old.owned.size)
                .filter(|s| *s <= MAX_BATCH_BYTES)
                .ok_or("更新旧备份声明过大")?;
            if let Some(copy) = &old.backup {
                if !owned_ok(copy) || copy.size != old.owned.size || copy.sha512 != old.owned.sha512
                {
                    return Err("更新旧备份所有权无效".into());
                }
            } else if !j.state.partial() {
                return Err("旧备份缺少持久所有权".into());
            }
        }
    }
    if !j.state.partial() && scope::expected_after(j)? != j.after {
        return Err("更新提交 inventory 与映射不一致".into());
    }
    Ok(())
}
fn receipt_name(r: &Receipt) -> String {
    match r {
        Receipt::Marker { .. } => "owned-marker.json".into(),
        Receipt::Copy { index, backup, .. } => format!(
            "owned-{}-{index:04}.json",
            if *backup { "backup" } else { "new" }
        ),
        Receipt::Directory { kind, .. } => format!("owned-dir-{kind}.json"),
    }
}
fn receipt(context: &Context, j: &Journal, name: &str) -> Result<Receipt> {
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    let r: Receipt = serde_json::from_slice(&bytes(operation, name, SMALL)?).map_err(err)?;
    if receipt_name(&r) != name {
        return Err("更新所有权记录名称不一致".into());
    }
    match &r {
        Receipt::Marker {
            schema,
            operation,
            owned,
        } => {
            if *schema != 1
                || Some(operation) != j.scope.operation.as_ref()
                || !owned_ok(owned)
                || owned.size > SMALL
            {
                return Err("更新根标记所有权记录无效".into());
            }
        }
        Receipt::Copy {
            schema,
            operation,
            index,
            backup,
            owned,
        } => {
            let item = j.items.get(*index).ok_or("更新所有权编号无效")?;
            let (size, hash, previous) = if *backup {
                let old = item.old.as_ref().ok_or("多余旧备份所有权记录")?;
                (old.owned.size, &old.owned.sha512, old.backup.as_ref())
            } else {
                (item.size, &item.sha512, item.new.as_ref())
            };
            if *schema != 1
                || Some(operation) != j.scope.operation.as_ref()
                || !owned_ok(owned)
                || owned.size != size
                || &owned.sha512 != hash
                || previous.is_some_and(|v| v != owned)
            {
                return Err("更新文件所有权记录冲突".into());
            }
        }
        Receipt::Directory {
            schema,
            operation,
            kind,
            key,
        } => {
            let b = j
                .scope
                .resources
                .get(kind)
                .ok_or("更新目录所有权类型无效")?;
            if *schema != 1
                || Some(operation) != j.scope.operation.as_ref()
                || b.before.is_some()
                || b.created.as_ref().is_some_and(|v| v != key)
            {
                return Err("更新目录所有权记录冲突".into());
            }
        }
    }
    Ok(r)
}
fn hydrate(context: &Context, j: &mut Journal) -> Result<()> {
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    for name in operation.names_with_limit(MAX_FILES * 5 + 16)? {
        if !name.starts_with("owned-") || name == "owned-marker.next" {
            continue;
        }
        match receipt(context, j, &name)? {
            Receipt::Marker { owned, .. } => j.marker = Some(owned),
            Receipt::Copy {
                index,
                backup,
                owned,
                ..
            } => {
                if backup {
                    j.items[index].old.as_mut().ok_or("旧文件缺失")?.backup = Some(owned)
                } else {
                    j.items[index].new = Some(owned)
                }
            }
            Receipt::Directory { kind, key, .. } => {
                j.scope
                    .resources
                    .get_mut(&kind)
                    .ok_or("目录登记缺失")?
                    .created = Some(key)
            }
        }
    }
    Ok(())
}
pub(super) fn read(context: &Context) -> Result<Journal> {
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    let mut j: Journal =
        serde_json::from_slice(&bytes(operation, "journal.json", MAX_JOURNAL_BYTES)?)
            .map_err(err)?;
    validate(&j, &context.operation_id)?;
    context.check(&j)?;
    hydrate(context, &mut j)?;
    validate(&j, &context.operation_id)?;
    Ok(j)
}
fn publish(dir: &Dir, name: &str, value: &impl Serialize, limit: u64) -> Result<Owned> {
    let bytes = serde_json::to_vec(value).map_err(err)?;
    if bytes.len() as u64 > limit {
        return Err("更新记录过大".into());
    }
    let mut file = dir.stage_for(name, bytes.len() as u64)?;
    file.write_all(&bytes).map_err(err)?;
    file.sync_all().map_err(err)?;
    let owned = filesystem::verify_file(
        &mut file,
        None,
        bytes.len() as u64,
        &format!("{:x}", sha2::Sha512::digest(&bytes)),
        None,
    )?;
    dir.link_anonymous(&mut file, name)?;
    Ok(owned)
}
use sha2::Digest;
fn replace(
    context: &Context,
    j: &Journal,
    name: &str,
    _next: &str,
    value: &impl Serialize,
    limit: u64,
) -> Result<()> {
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    context.check(j)?;
    #[cfg(not(target_os = "linux"))]
    {
        let mut expected = operation.document_state(name, limit)?;
        if let Some(previous) = operation.read_document(name, limit, &mut expected)? {
            validate_previous(j, name, &previous)?;
        }
        let bytes = serde_json::to_vec(value).map_err(err)?;
        context.check(j)?;
        operation.compare_document(name, limit, &expected, Some(&bytes))?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        if operation.stat(name)?.is_none() {
            publish(operation, name, value, limit)?;
            return Ok(());
        }
        let before = super::super::token_for(&operation.regular(name)?)?;
        if operation.stat(_next)?.is_some() {
            let token = super::super::token_for(&operation.regular(_next)?)?;
            validate_private_name(context, j, _next)?;
            context.check(j)?;
            if super::super::token_for(&operation.regular(_next)?)? != token {
                return Err("更新临时状态记录已被替换".into());
            }
            operation.unlink(_next, false)?;
        }
        publish(operation, _next, value, limit)?;
        context.check(j)?;
        if super::super::token_for(&operation.regular(name)?)? != before {
            return Err("更新状态记录被外部替换".into());
        }
        operation.replace_journal(_next, name)
    }
}
#[cfg(not(target_os = "linux"))]
fn validate_previous(j: &Journal, name: &str, bytes: &[u8]) -> Result<()> {
    match name {
        "journal.json" => {
            let previous: Journal = serde_json::from_slice(bytes).map_err(err)?;
            validate(&previous, &j.id)?;
            if Binding::of(&previous)? != Binding::of(j)?
                || previous.items.len() != j.items.len()
                || previous.items.iter().zip(&j.items).any(|(a, b)| {
                    a.kind != b.kind || a.name != b.name || a.size != b.size || a.sha512 != b.sha512
                })
            {
                return Err("原生更新日志业务绑定已经变化，原文档已保留".into());
            }
        }
        "status.json" => {
            let previous: Status = serde_json::from_slice(bytes).map_err(err)?;
            if previous.schema != 1 || previous.id != j.id || previous.binding != Binding::of(j)? {
                return Err("原生更新状态业务绑定已经变化，原文档已保留".into());
            }
        }
        "owned-marker.json" => {
            let previous: Receipt = serde_json::from_slice(bytes).map_err(err)?;
            if !matches!(previous, Receipt::Marker { schema:1, ref operation, ref owned } if Some(operation) == j.scope.operation.as_ref() && owned_ok(owned) && owned.size <= SMALL)
            {
                return Err("原生更新标记业务绑定已经变化，原文档已保留".into());
            }
        }
        _ => return Err("此记录没有原生文档所有权".into()),
    }
    Ok(())
}
pub(super) fn write(context: &Context, j: &Journal) -> Result<()> {
    validate(j, &j.id)?;
    context.check(j)?;
    #[cfg(test)]
    FULL_WRITES.with(|n| n.set(n.get() + 1));
    replace(
        context,
        j,
        "journal.json",
        "journal.next",
        j,
        MAX_JOURNAL_BYTES,
    )?;
    #[cfg(test)]
    if j.state == State::Committed && FAIL_COMMITTED_WRITE.with(|v| v.replace(false)) {
        return Err("更新提交日志同步失败（故障夹具）".into());
    }
    status(context, j)
}
pub(super) fn status(context: &Context, j: &Journal) -> Result<()> {
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    let status = Status {
        schema: 1,
        id: j.id.clone(),
        state: j.state,
        binding: Binding::of(j)?,
        journal_token: super::super::token_for(&operation.regular("journal.json")?)?,
    };
    replace(context, j, "status.json", "status.next", &status, SMALL)
}
fn known_private_name(name: &str) -> bool {
    if matches!(
        name,
        "journal.json"
            | "journal.next"
            | "status.json"
            | "status.next"
            | "owned-marker.json"
            | "owned-marker.next"
    ) {
        return true;
    }
    if KINDS
        .iter()
        .any(|kind| name == format!("target-{kind}") || name == format!("owned-dir-{kind}.json"))
    {
        return true;
    }
    ["new-", "old-", "backup-", "owned-new-", "owned-backup-"]
        .iter()
        .any(|prefix| {
            let Some(index) = name.strip_prefix(prefix) else {
                return false;
            };
            let index = if prefix.starts_with("owned-") {
                let Some(i) = index.strip_suffix(".json") else {
                    return false;
                };
                i
            } else {
                index
            };
            index.len() == 4
                && index.bytes().all(|b| b.is_ascii_digit())
                && index.parse::<usize>().is_ok_and(|i| i < MAX_FILES)
        })
}
pub(super) fn status_ready(context: &Context) -> Result<bool> {
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    // A status record is a fast state guard, not permission to ignore external
    // content inside the private history directory. Unknown names block every
    // writer before destructive instance operations can carry or discard it.
    for name in operation.names_with_limit(MAX_FILES * 5 + 16)? {
        if !known_private_name(&name) {
            return Err("资源更新历史含未知文件，已保留".into());
        }
    }
    #[cfg(windows)]
    if operation.stat("journal.json")?.is_some() {
        let journal = read(context)?;
        if journal.state.terminal() {
            super::recovery::verify_terminal(context, &journal)?;
            return Ok(true);
        }
    }
    if operation.stat("status.json")?.is_none() || operation.stat("journal.json")?.is_none() {
        return Ok(false);
    }
    let status: Status =
        serde_json::from_slice(&bytes(operation, "status.json", SMALL)?).map_err(err)?;
    if status.schema != 1
        || status.id != context.operation_id
        || !valid_id(&status.id)
        || status.binding.root != context.root.key()?
        || status.binding.versions != context.versions.key()?
        || status.binding.instance != context.instance.key()?
        || Some(status.binding.history.clone())
            != context.history.as_ref().map(|d| d.key()).transpose()?
        || status.binding.operation != operation.key()?
    {
        return Err("更新历史状态绑定无效".into());
    }
    if status.journal_token != super::super::token_for(&operation.regular("journal.json")?)? {
        return Ok(false);
    }
    Ok(!status.state.pending())
}
pub(super) fn register_copy(
    context: &Context,
    j: &Journal,
    index: usize,
    backup: bool,
    owned: &Owned,
) -> Result<()> {
    context.check(j)?;
    let r = Receipt::Copy {
        schema: 1,
        operation: j.scope.operation.clone().ok_or("更新目录身份缺失")?,
        index,
        backup,
        owned: owned.clone(),
    };
    publish(
        context.operation.as_ref().ok_or("更新目录缺失")?,
        &receipt_name(&r),
        &r,
        SMALL,
    )?;
    Ok(())
}
pub(super) fn register_directory(
    context: &Context,
    j: &Journal,
    kind: &str,
    key: &Key,
) -> Result<()> {
    context.check(j)?;
    let r = Receipt::Directory {
        schema: 1,
        operation: j.scope.operation.clone().ok_or("更新目录身份缺失")?,
        kind: kind.into(),
        key: key.clone(),
    };
    publish(
        context.operation.as_ref().ok_or("更新目录缺失")?,
        &receipt_name(&r),
        &r,
        SMALL,
    )?;
    Ok(())
}
fn marker_value(j: &Journal) -> Result<Marker> {
    Ok(Marker {
        schema: 1,
        id: j.id.clone(),
        binding: Binding::of(j)?,
    })
}
pub(super) fn create_marker(context: &Context, j: &mut Journal) -> Result<()> {
    context.check(j)?;
    let store = scope::marker_store(&context.path)?.ok_or("更新根标记目录缺失")?;
    let value = marker_value(j)?;
    let name = format!("{}.json", j.id);
    let bytes = serde_json::to_vec(&value).map_err(err)?;
    let mut file = store.stage_for(&name, bytes.len() as u64)?;
    file.write_all(&bytes).map_err(err)?;
    file.sync_all().map_err(err)?;
    let owned = filesystem::verify_file(
        &mut file,
        None,
        bytes.len() as u64,
        &format!("{:x}", sha2::Sha512::digest(&bytes)),
        None,
    )?;
    // Root markers are replaced between update and undo. Register the current
    // anonymous inode before naming it, so a crash before the next full phase
    // manifest can still recover the marker without trusting an obsolete inode.
    let receipt = Receipt::Marker {
        schema: 1,
        operation: j.scope.operation.clone().ok_or("更新目录身份缺失")?,
        owned: owned.clone(),
    };
    replace(
        context,
        j,
        "owned-marker.json",
        "owned-marker.next",
        &receipt,
        SMALL,
    )?;
    j.marker = Some(owned);
    context.check(j)?;
    store.link_anonymous(&mut file, &name)
}
pub(super) fn read_marker(store: &Dir, name: &str) -> Result<Marker> {
    let value: Marker = serde_json::from_slice(&bytes(store, name, SMALL)?).map_err(err)?;
    if value.schema != 1 || name != format!("{}.json", value.id) || !valid_id(&value.id) {
        return Err("未知资源更新根标记，已保留".into());
    }
    Ok(value)
}
pub(super) fn marker_context(marker: &Marker, contexts: &[Context]) -> Result<usize> {
    contexts
        .iter()
        .position(|c| {
            c.operation_id == marker.id
                && c.instance.key().as_ref() == Ok(&marker.binding.instance)
                && c.operation
                    .as_ref()
                    .is_some_and(|op| op.key().as_ref() == Ok(&marker.binding.operation))
        })
        .ok_or_else(|| "更新根标记绑定的实例或历史目录已移走，已保留".into())
}
pub(super) fn remove_marker(context: &Context, j: &Journal) -> Result<()> {
    context.check(j)?;
    let store = scope::marker_store(&context.path)?.ok_or("更新根标记目录缺失")?;
    let name = format!("{}.json", j.id);
    if store.stat(&name)?.is_none() {
        return Ok(());
    }
    let marker = read_marker(&store, &name)?;
    if marker.binding != Binding::of(j)? {
        return Err("更新根标记绑定已经变化".into());
    }
    verify_owned(
        &store,
        &name,
        j.marker.as_ref().ok_or("更新根标记缺少持久所有权")?,
    )?;
    context.check(j)?;
    verify_owned(&store, &name, j.marker.as_ref().ok_or("根标记所有权缺失")?)?;
    store.unlink(&name, false)
}
pub(super) fn remove_orphan_marker(context: &Context) -> Result<()> {
    let Some(store) = scope::marker_store(&context.path)? else {
        return Ok(());
    };
    let name = format!("{}.json", context.operation_id);
    if store.stat(&name)?.is_none() {
        return Ok(());
    }
    let token = super::super::token_for(&store.regular(&name)?)?;
    let m = read_marker(&store, &name)?;
    if m.binding.root != context.root.key()?
        || m.binding.versions != context.versions.key()?
        || m.binding.instance != context.instance.key()?
        || Some(m.binding.history) != context.history.as_ref().map(|d| d.key()).transpose()?
        || Some(m.binding.operation) != context.operation.as_ref().map(|d| d.key()).transpose()?
        || m.binding.pcl != context.root.child(".pcl-linux")?.key()?
        || m.binding.markers != store.key()?
    {
        return Err("孤立更新根标记绑定冲突".into());
    }
    if super::super::token_for(&store.regular(&name)?)? != token {
        return Err("孤立根标记被替换".into());
    }
    store.unlink(&name, false)
}
pub(super) fn validate_private_name(context: &Context, j: &Journal, name: &str) -> Result<()> {
    if name == "owned-marker.next" {
        let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
        let r: Receipt = serde_json::from_slice(&bytes(operation, name, SMALL)?).map_err(err)?;
        if !matches!(r,Receipt::Marker{schema:1,ref operation,ref owned} if Some(operation)==j.scope.operation.as_ref()&&owned_ok(owned)&&owned.size<=SMALL)
        {
            return Err("更新临时根标记所有权记录冲突".into());
        }
        return Ok(());
    }
    if name.starts_with("owned-") {
        receipt(context, j, name)?;
        return Ok(());
    }
    let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
    if name == "journal.json" || name == "journal.next" {
        let other: Journal =
            serde_json::from_slice(&bytes(operation, name, MAX_JOURNAL_BYTES)?).map_err(err)?;
        validate(&other, &j.id)?;
        if Binding::of(&other)? != Binding::of(j)?
            || other.items.len() != j.items.len()
            || other.items.iter().zip(&j.items).any(|(a, b)| {
                a.kind != b.kind || a.name != b.name || a.size != b.size || a.sha512 != b.sha512
            })
        {
            return Err("更新临时日志绑定冲突".into());
        }
        return Ok(());
    }
    if name == "status.json" || name == "status.next" {
        let other: Status = serde_json::from_slice(&bytes(operation, name, SMALL)?).map_err(err)?;
        if other.schema != 1 || other.id != j.id || other.binding != Binding::of(j)? {
            return Err("更新临时状态绑定冲突".into());
        }
        return Ok(());
    }
    Err("资源更新历史含未知文件，已保留".into())
}
