//! Only this operation-owned journal is a logical Document. Marker, instance
//! payloads and dependency JSON remain literal physical objects.
use super::*;
use crate::launcher_local::filesystem::documents::{Document, DocumentPresence, DocumentState};

fn current(operation: &Dir) -> Result<(DocumentState, Option<Vec<u8>>)> {
    let local = crate::launcher_local::filesystem::Dir(operation.document_file()?);
    let document = Document::new(&local, "journal.json", MAX_JOURNAL as u64)?;
    let mut state = document.state()?;
    let bytes = document.read_state(&mut state)?;
    if state.presence() == DocumentPresence::Tombstone {
        return Err("实例删除 journal 已被逻辑删除，原记录与副本已保留".into());
    }
    Ok((state, bytes))
}
pub(super) fn journal_exists(operation: &Dir) -> Result<bool> {
    current(operation).map(|(_, bytes)| bytes.is_some())
}
fn inventory(operation: &Dir) -> Result<()> {
    let local = crate::launcher_local::filesystem::Dir(operation.document_file()?);
    let document = Document::new(&local, "journal.json", MAX_JOURNAL as u64)?;
    let mut allowed = document.auxiliary_names()?;
    allowed.extend(["journal.json".into(), "instance".into()]);
    for name in operation.names()? {
        if !allowed.contains(&name) {
            return Err("实例恢复目录含未知文件，文件已保留".into());
        }
    }
    Ok(())
}
fn decode(operation: &Dir, name: &str, bytes: &[u8]) -> Result<Journal> {
    let journal: Journal =
        serde_json::from_slice(bytes).map_err(|_| "实例删除记录损坏，文件已保留")?;
    validate(&journal, name)?;
    if operation.key()? != journal.operation_key {
        return Err("实例恢复记录目录已被替换".into());
    }
    Ok(journal)
}
pub(super) fn read_record(operation: &Dir, name: &str) -> Result<Record> {
    let (state, bytes) = current(operation)?;
    let bytes = bytes.ok_or("实例删除 journal 缺失，恢复目录已保留")?;
    let journal = decode(operation, name, &bytes)?;
    inventory(operation)?;
    Ok(Record {
        journal,
        token: Some(state),
    })
}
pub(super) fn write_record(operation: &Dir, record: &mut Record) -> Result<()> {
    if operation.key()? != record.journal.operation_key {
        return Err("实例恢复记录目录已被替换".into());
    }
    let local = crate::launcher_local::filesystem::Dir(operation.document_file()?);
    let document = Document::new(&local, "journal.json", MAX_JOURNAL as u64)?;
    let initial;
    let expected: &DocumentState = match record.token.as_mut() {
        Some(expected) => {
            let bytes = document
                .read_state(expected)?
                .ok_or("实例删除记录已消失，已保留")?;
            let previous = decode(operation, &record.journal.operation_id, &bytes)?;
            if immutable_journal(&previous)? != immutable_journal(&record.journal)? {
                return Err("实例删除记录不可变范围变化，已保留".into());
            }
            record.journal.generation = previous
                .generation
                .checked_add(1)
                .ok_or("实例删除记录代次已耗尽")?;
            record.journal.previous_hash = Some(hash(&bytes));
            expected
        }
        None => {
            initial = document.state()?;
            if initial.presence() != DocumentPresence::NeverCreated {
                return Err("初始实例删除记录已被外部占用或删除，已保留".into());
            }
            &initial
        }
    };
    validate(&record.journal, &record.journal.operation_id)?;
    inventory(operation)?;
    let bytes = serde_json::to_vec(&record.journal).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_JOURNAL {
        return Err("实例删除记录超过安全上限".into());
    }
    // An error can follow publication. Caller retains its intended in-memory
    // phase and reopens actual durable state before choosing any recovery move.
    document.compare_write(expected, Some(&bytes))?;
    #[cfg(test)]
    fail_after_compare()?;
    let (state, actual) = current(operation).map_err(|error| {
        format!("MayHavePublished：实例删除记录已尝试提交，须重开复核：{error}")
    })?;
    if actual.as_deref() != Some(bytes.as_slice()) {
        return Err("MayHavePublished：实例删除记录提交后复核不符，现存证据已保留".into());
    }
    inventory(operation).map_err(|error| format!("MayHavePublished：{error}"))?;
    record.token = Some(state);
    Ok(())
}
pub(super) fn cleanup_journal_temps(
    root: &Dir,
    store: &Dir,
    operation: &Dir,
    name: &str,
    project: &Path,
) -> Result<()> {
    let record = read_record(operation, name)?;
    bind(root, store, operation, &record.journal, project)?;
    // Document validates its exact auxiliary namespace. No .next or prefix GC
    // is authorized on Windows, and no instance/user file enters this path.
    inventory(operation)
}

#[cfg(test)]
thread_local! { static FAIL_AFTER_COMPARE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(super) fn inject_post_compare_failure() {
    FAIL_AFTER_COMPARE.with(|flag| flag.set(true));
}
#[cfg(test)]
fn fail_after_compare() -> Result<()> {
    if FAIL_AFTER_COMPARE.with(|flag| flag.replace(false)) {
        Err("MayHavePublished：测试在 durable CAS 后中断，须重开记录".into())
    } else {
        Ok(())
    }
}
