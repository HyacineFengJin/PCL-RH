//! Small durable state records and one ownership receipt per inode. The complete
//! manifest is rewritten only at state boundaries, so staging remains linear.
//! An inode receipt reaches disk before its first pathname; recovery may only
//! delete a file whose receipt still matches its inode, size, mode and SHA512.
use super::*;
use std::io::Write;
const MAX_RECEIPT: u64 = 16 * 1024;
#[cfg(test)]
thread_local! {pub(super) static FAIL_COMMITTED_WRITE:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Receipt {
    File {
        schema: u32,
        operation_key: Key,
        index: usize,
        owned: Owned,
    },
    Directory {
        schema: u32,
        operation_key: Key,
        resource_kind: String,
        key: Key,
    },
    Published {
        schema: u32,
        operation_key: Key,
        index: usize,
        owned: Owned,
    },
}
fn receipt_name(record: &Receipt) -> String {
    match record {
        Receipt::File { index, .. } => format!("owned-{index:04}.json"),
        Receipt::Published { index, .. } => format!("published-{index:04}.json"),
        Receipt::Directory { resource_kind, .. } => format!("dir-{resource_kind}.json"),
    }
}
fn bytes(dir: &Dir, name: &str, limit: u64) -> Result<Vec<u8>> {
    #[cfg(not(target_os = "linux"))]
    if matches!(name, "journal.json" | "status.json" | "owned-marker.json") {
        let mut state = dir.document_state(name, limit)?;
        return dir
            .read_document(name, limit, &mut state)?
            .ok_or("资源业务文档缺失".into());
    }
    let mut file = dir.regular(name)?;
    let token = token_for(&file)?;
    if file.metadata().map_err(error)?.len() > limit {
        return Err("资源批次记录过大".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > limit
        || token_for(&file)? != token
        || token_for(&dir.regular(name)?)? != token
    {
        return Err("资源批次记录读取期间已变化".into());
    }
    Ok(bytes)
}
fn validate(j: &Journal, name: &str) -> Result<()> {
    if j.schema != SCHEMA
        || !valid_op(name)
        || j.operation_id != name
        || !j.scope.root.is_absolute()
        || pcl_core::retained_fs::validate_absolute_path(&j.scope.root).is_err()
        || j.scope.profile_token.len() > 512
        || j.items.is_empty()
        || j.items.len() > MAX_FILES
    {
        return Err("资源批次记录格式无效".into());
    }
    pcl_core::identifier(&j.scope.instance_id)?;
    safe_name(&j.scope.instance_id)?;
    let mut total = 0u64;
    let mut names = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for item in &j.items {
        name_ok(&item.kind, &item.file_name)?;
        if !valid_hash(&item.sha512)
            || item.size == 0
            || item.size > MAX_FILE_BYTES
            || !names.insert((&item.kind, &item.file_name))
        {
            return Err("资源批次文件声明无效".into());
        }
        total = total
            .checked_add(item.size)
            .filter(|s| *s <= MAX_BATCH_BYTES)
            .ok_or("资源批次文件大小越界")?;
        kinds.insert(item.kind.clone());
        if let Some(owned) = &item.owned {
            if owned.size != item.size || owned.sha512 != item.sha512 || !owned.regular() {
                return Err("资源批次所有权声明无效".into());
            }
        }
        if item.published && item.owned.is_none() {
            return Err("资源发布记录缺少所有权".into());
        }
        if !j.state.partial() && item.owned.is_none() {
            return Err("已准备资源缺少所有权".into());
        }
        if j.state == State::Committed && !item.published {
            return Err("已提交批次包含未发布资源".into());
        }
    }
    if kinds != j.scope.destinations.keys().cloned().collect() {
        return Err("资源批次目录声明不匹配".into());
    }
    for (kind, d) in &j.scope.destinations {
        if d.relative != *kind && d.relative != format!("versions/{}/{kind}", j.scope.instance_id) {
            return Err("资源批次目录路径无效".into());
        }
        if d.before.is_some() && d.created.is_some() {
            return Err("资源批次目录所有权重复".into());
        }
    }
    if !j.state.partial() && j.stage_key.is_none() {
        return Err("已准备批次缺少暂存目录登记".into());
    }
    Ok(())
}
fn receipt(dir: &Dir, name: &str, j: &Journal) -> Result<Receipt> {
    let r: Receipt = serde_json::from_slice(&bytes(dir, name, MAX_RECEIPT)?).map_err(error)?;
    if receipt_name(&r) != name {
        return Err("资源批次登记名称不匹配".into());
    }
    match &r {
        Receipt::File {
            schema,
            operation_key,
            index,
            owned,
        }
        | Receipt::Published {
            schema,
            operation_key,
            index,
            owned,
        } => {
            let item = j.items.get(*index).ok_or("资源批次登记编号无效")?;
            if *schema != 1
                || *operation_key != j.operation_key
                || owned.size != item.size
                || owned.sha512 != item.sha512
                || !owned.regular()
                || item.owned.as_ref().is_some_and(|v| v != owned)
            {
                return Err("资源文件所有权登记冲突".into());
            }
        }
        Receipt::Directory {
            schema,
            operation_key,
            resource_kind,
            key,
        } => {
            let d = j
                .scope
                .destinations
                .get(resource_kind)
                .ok_or("资源目录登记类型无效")?;
            if *schema != 1
                || *operation_key != j.operation_key
                || d.before.is_some()
                || d.created.as_ref().is_some_and(|v| v != key)
            {
                return Err("资源目录所有权登记冲突".into());
            }
        }
    }
    Ok(r)
}
fn is_receipt(name: &str) -> bool {
    name.starts_with("owned-")
        || name.starts_with("published-")
        || name.starts_with("dir-") && name.ends_with(".json")
}
fn hydrate(operation: &Dir, j: &mut Journal) -> Result<()> {
    for name in operation.names()? {
        if !is_receipt(&name) {
            continue;
        }
        match receipt(operation, &name, j)? {
            Receipt::File { index, owned, .. } => j.items[index].owned = Some(owned),
            Receipt::Published { index, owned, .. } => {
                j.items[index].owned = Some(owned);
                j.items[index].published = true;
            }
            Receipt::Directory {
                resource_kind, key, ..
            } => {
                j.scope
                    .destinations
                    .get_mut(&resource_kind)
                    .ok_or("资源目录登记缺失")?
                    .created = Some(key)
            }
        }
    }
    Ok(())
}
pub(super) fn read_journal(operation: &Dir, name: &str) -> Result<Journal> {
    let mut j: Journal =
        serde_json::from_slice(&bytes(operation, "journal.json", MAX_JOURNAL_BYTES)?)
            .map_err(error)?;
    // Staging permits a receipt newer than the manifest, including a crash
    // between receipt sync and linkat. Other states already carry all items.
    validate(&j, name)?;
    if operation.key()? != j.operation_key {
        return Err("资源批次目录 inode 已变化".into());
    }
    hydrate(operation, &mut j)?;
    validate(&j, name)?;
    Ok(j)
}
fn write_record(operation: &Dir, name: &str, value: &impl Serialize, limit: u64) -> Result<()> {
    let data = serde_json::to_vec(value).map_err(error)?;
    if data.len() as u64 > limit {
        return Err("资源批次记录过大".into());
    }
    let mut file = operation.stage_for(name, data.len() as u64)?;
    file.write_all(&data).map_err(error)?;
    file.sync_all().map_err(error)?;
    operation.link_anonymous(&mut file, name)
}
pub(super) fn write_journal(operation: &Dir, j: &Journal) -> Result<()> {
    validate(j, &j.operation_id)?;
    if operation.key()? != j.operation_key {
        return Err("资源批次目录 inode 已变化".into());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut state = operation.document_state("journal.json", MAX_JOURNAL_BYTES)?;
        if let Some(bytes) =
            operation.read_document("journal.json", MAX_JOURNAL_BYTES, &mut state)?
        {
            let previous: Journal = serde_json::from_slice(&bytes).map_err(error)?;
            validate(&previous, &j.operation_id)?;
            if previous.operation_key != j.operation_key
                || previous.items.len() != j.items.len()
                || previous.items.iter().zip(&j.items).any(|(a, b)| {
                    a.kind != b.kind
                        || a.file_name != b.file_name
                        || a.size != b.size
                        || a.sha512 != b.sha512
                })
                || previous.scope.root != j.scope.root
                || previous.scope.root_key != j.scope.root_key
                || previous.scope.instance_key != j.scope.instance_key
                || previous.pcl_key != j.pcl_key
                || previous.store_key != j.store_key
            {
                return Err("资源批次绑定已变化".into());
            }
        }
        let bytes = serde_json::to_vec(j).map_err(error)?;
        operation.compare_document("journal.json", MAX_JOURNAL_BYTES, &state, Some(&bytes))?;
        #[cfg(test)]
        if j.state == State::Committed && FAIL_COMMITTED_WRITE.with(|flag| flag.replace(false)) {
            return Err("MayHaveCommitted：提交日志同步失败（故障夹具）".into());
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        if operation.stat("journal.json")?.is_none() {
            return write_record(operation, "journal.json", j, MAX_JOURNAL_BYTES);
        }
        let previous = read_journal(operation, &j.operation_id)?;
        if previous.scope.root != j.scope.root
            || previous.scope.root_key != j.scope.root_key
            || previous.scope.instance_key != j.scope.instance_key
            || previous.pcl_key != j.pcl_key
            || previous.store_key != j.store_key
        {
            return Err("资源批次绑定已变化".into());
        }
        let before = token_for(&operation.regular("journal.json")?)?;
        operation.absent("journal.next")?;
        write_record(operation, "journal.next", j, MAX_JOURNAL_BYTES)?;
        if token_for(&operation.regular("journal.json")?)? != before {
            return Err("资源批次状态记录被外部替换".into());
        }
        operation.replace_journal("journal.next", "journal.json")?;
        // Fault gate models rename having succeeded before a durability error.
        #[cfg(test)]
        if j.state == State::Committed && FAIL_COMMITTED_WRITE.with(|flag| flag.replace(false)) {
            return Err("提交日志同步失败（故障夹具）".into());
        }
        Ok(())
    }
}
pub(super) fn register_owned(
    operation: &Dir,
    j: &Journal,
    index: usize,
    owned: &Owned,
) -> Result<()> {
    let r = Receipt::File {
        schema: 1,
        operation_key: j.operation_key.clone(),
        index,
        owned: owned.clone(),
    };
    write_record(operation, &receipt_name(&r), &r, MAX_RECEIPT)
}
pub(super) fn register_directory(
    operation: &Dir,
    j: &Journal,
    kind: &str,
    key: &Key,
) -> Result<()> {
    let r = Receipt::Directory {
        schema: 1,
        operation_key: j.operation_key.clone(),
        resource_kind: kind.into(),
        key: key.clone(),
    };
    write_record(operation, &receipt_name(&r), &r, MAX_RECEIPT)
}
pub(super) fn register_published(operation: &Dir, j: &Journal, index: usize) -> Result<()> {
    let owned = j.items[index].owned.clone().ok_or("资源文件未登记")?;
    let r = Receipt::Published {
        schema: 1,
        operation_key: j.operation_key.clone(),
        index,
        owned,
    };
    write_record(operation, &receipt_name(&r), &r, MAX_RECEIPT)
}
pub(super) fn remove_registrations(operation: &Dir, j: &Journal) -> Result<()> {
    // Validate the whole inventory before removing any authority receipts.
    let names = operation.names()?;
    for name in &names {
        if is_receipt(name) {
            receipt(operation, name, j)?;
        } else if name == "journal.next" {
            let next: Journal = serde_json::from_slice(&bytes(operation, name, MAX_JOURNAL_BYTES)?)
                .map_err(error)?;
            validate(&next, &j.operation_id)?;
            if next.operation_key != j.operation_key || next.scope.root_key != j.scope.root_key {
                return Err("资源批次临时日志绑定冲突".into());
            }
        } else if name != "journal.json" {
            return Err("资源批次目录包含未知文件，已保留".into());
        }
    }
    for name in names {
        if name != "journal.json" {
            operation_binding(operation, j)?;
            operation.unlink(&name, false)?;
        }
    }
    Ok(())
}
