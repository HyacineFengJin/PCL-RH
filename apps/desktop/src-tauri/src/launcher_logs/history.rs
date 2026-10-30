//! Confirmed history moves and persistent restore. The journal is immutable;
//! FD-verified source/archive locations describe progress after any interruption.
//! A completed restore saves a receipt, releasing the originals so a later game
//! launch may replace them. Unrecognized files and conflicts remain in place.
use super::*;
use std::{
    sync::atomic::AtomicU64,
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub fn prepare_clear(root: &Path, current: Option<&Path>) -> Result<ClearPlan, String> {
    let scope = Scope::open(root)?;
    let entries = scan_historical(&scope, current, &AtomicBool::new(false))?;
    clear_plan(&scope, &entries, current)
}

fn scan_historical(
    scope: &Scope,
    current: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, Entry>, String> {
    let current = scope.current_name(current);
    let names: Vec<String> = match &scope.logs {
        Some(logs) => logs
            .names()?
            .into_iter()
            .filter(|name| name.ends_with(".log") && Some(name) != current.as_ref())
            .collect(),
        None => Vec::new(),
    };
    // The protected current log is never read or moved by cleanup. Its content
    // may continue growing (or exceed export bounds) without affecting history.
    scan(scope, Some(&names), cancel)
}

fn clear_plan(
    scope: &Scope,
    entries: &BTreeMap<String, Entry>,
    current: Option<&Path>,
) -> Result<ClearPlan, String> {
    let current = scope.current_name(current);
    let historical: BTreeMap<_, _> = entries
        .iter()
        .filter(|(name, _)| Some(*name) != current.as_ref())
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect();
    Ok(ClearPlan {
        revision: revision(scope, current.as_deref(), entries, "clear")?,
        file_count: historical.len(),
        bytes: historical.values().map(|e| e.stamp.bytes).sum(),
        names: historical.into_keys().collect(),
        retained_current: current,
    })
}

pub fn execute_clear(
    root: &Path,
    expected_revision: &str,
    current: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<ClearOutcome, String> {
    cancelled(cancel)?;
    let scope = Scope::open(root)?;
    let before = scan_historical(&scope, current, cancel)?;
    let plan = clear_plan(&scope, &before, current)?;
    if plan.revision != expected_revision {
        return Err("日志列表或当前日志已变化，请重新确认清理".into());
    }
    if plan.file_count == 0 {
        return Err("没有可清理的历史日志，当前日志会保留".into());
    }
    // This is the first filesystem mutation: it runs only after explicit UI
    // confirmation and a fresh source scan. The lock serializes other processes.
    let owner = scope.owner.as_ref().ok_or("日志目录不存在")?;
    let logs = scope.logs.as_ref().ok_or("日志目录不存在")?;
    let store = owner.private_child("log-trash")?;
    let _lock = store.lock()?;
    let fresh = scan_historical(&scope, current, cancel)?;
    if clear_plan(&scope, &fresh, current)?.revision != expected_revision {
        return Err("日志列表已变化，请重新确认清理".into());
    }
    if store.names()?.len() >= MAX_ENTRIES {
        return Err("日志恢复记录过多，请先保留并整理恢复目录".into());
    }
    let operation_id = operation_id();
    let operation = store.new_private_child(&operation_id)?;
    let entries = fresh
        .into_iter()
        .filter(|(name, _)| Some(name) != plan.retained_current.as_ref())
        .collect();
    let journal = Journal {
        schema: 1,
        operation_id: operation_id.clone(),
        created_at: now(),
        root: scope.root.stamp()?.identity(),
        logs: logs.stamp()?.identity(),
        entries,
    };
    let journal_bytes = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
    let journal_digest = format!("{:x}", Sha256::digest(&journal_bytes));
    operation.write_new("journal.json", &journal_bytes)?;
    let result = (|| {
        for (name, entry) in &journal.entries {
            cancelled(cancel)?;
            scope.recheck()?;
            let (stamp, digest, _) =
                snapshot(logs, name, MAX_FILE_BYTES)?.ok_or("清理前日志已消失")?;
            if stamp != entry.stamp || digest != entry.digest {
                return Err("清理期间日志被外部修改".into());
            }
            logs.move_new(name, &operation)?;
            let (stamp, digest, _) =
                snapshot(&operation, name, MAX_FILE_BYTES)?.ok_or("移动后的日志已消失")?;
            if !entry.stamp.after_move_matches(&stamp) || digest != entry.digest {
                return Err("移动期间日志被外部修改，已保留恢复副本".into());
            }
        }
        cancelled(cancel)?;
        scope.recheck()
    })();
    if let Err(error) = result {
        let rollback = restore_entries(logs, &operation, &journal, None, &AtomicBool::new(false))
            .and_then(|restored| {
                complete(&operation, &journal_digest)?;
                Ok(restored)
            });
        return Err(match rollback {
            Ok(_) => format!("{error}；已恢复被移动的日志"),
            Err(recovery) => format!("{error}；部分日志保留在恢复记录 {operation_id}：{recovery}"),
        });
    }
    Ok(ClearOutcome {
        operation_id,
        file_count: journal.entries.len(),
    })
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(super) fn operation_id() -> String {
    format!(
        "logs-{}-{}-{}",
        now(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn journal(scope: &Scope, operation: &Dir, id: &str) -> Result<(Journal, String), String> {
    component(id)?;
    operation.verify_private()?;
    let (_, digest, data) = snapshot(operation, "journal.json", MAX_JOURNAL_BYTES)?
        .ok_or("恢复记录缺少日志清单，已保留目录")?;
    let journal: Journal =
        serde_json::from_slice(&data).map_err(|e| format!("日志恢复清单无效，已保留文件：{e}"))?;
    if journal.schema != 1
        || journal.operation_id != id
        || journal.entries.len() > MAX_ENTRIES
        || journal.root != scope.root.stamp()?.identity()
        || Some(journal.logs.clone())
            != scope
                .logs
                .as_ref()
                .map(|d| d.stamp().map(|s| s.identity()))
                .transpose()?
    {
        return Err("日志恢复清单与当前目录不匹配，已保留文件".into());
    }
    let mut total = 0u64;
    for (name, entry) in &journal.entries {
        valid_log(name)?;
        if !entry.stamp.regular()
            || !entry.stamp.owned()
            || entry.stamp.bytes > MAX_FILE_BYTES
            || entry.digest.len() != 64
            || !entry.digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("日志恢复清单包含不安全条目，已保留文件".into());
        }
        total = total
            .checked_add(entry.stamp.bytes)
            .ok_or("日志恢复清单过大")?;
        if total > MAX_TOTAL_BYTES {
            return Err("日志恢复清单超过安全上限".into());
        }
    }
    Ok((journal, digest))
}

/// Completed operations relinquish original-path ownership. In particular a
/// later game launch overwriting a restored log must not resurrect old recovery
/// warnings. The receipt authenticates its exact immutable journal, while any
/// unexpected file in the private directory still remains visible and retained.
fn completed(operation: &Dir) -> Result<bool, String> {
    operation.verify_private()?;
    let Some((_, _, bytes)) = snapshot(operation, "restored.json", MAX_JOURNAL_BYTES)? else {
        return Ok(false);
    };
    let receipt: Completion = serde_json::from_slice(&bytes)
        .map_err(|e| format!("日志恢复完成标记无效，已保留记录：{e}"))?;
    let (_, digest, _) = snapshot(operation, "journal.json", MAX_JOURNAL_BYTES)?
        .ok_or("日志恢复完成标记缺少对应清单，已保留记录")?;
    if receipt.schema != 1 || receipt.journal_digest != digest {
        return Err("日志恢复完成标记与清单不匹配，已保留记录".into());
    }
    let unknown: Vec<String> = operation
        .names()?
        .into_iter()
        .filter(|name| name != "journal.json" && name != "restored.json")
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "已完成的日志恢复记录包含其它文件，已保留：{}",
            unknown.join("、")
        ));
    }
    Ok(true)
}

fn complete(operation: &Dir, journal_digest: &str) -> Result<(), String> {
    if completed(operation)? {
        return Ok(());
    }
    if operation.names()?.iter().any(|name| name != "journal.json") {
        return Err("日志恢复目录仍有文件，已保留记录并拒绝标记完成".into());
    }
    let receipt = Completion {
        schema: 1,
        journal_digest: journal_digest.to_owned(),
    };
    operation
        .write_new(
            "restored.json",
            &serde_json::to_vec(&receipt).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("日志已恢复，但完成标记保存失败；请保留恢复记录并重新读取：{e}"))
}

fn recovery_row(
    scope: &Scope,
    operation: &Dir,
    journal: &Journal,
    journal_digest: &str,
) -> Result<RecoveryRow, String> {
    let logs = scope.logs.as_ref().ok_or("日志目录不存在")?;
    let mut states = BTreeMap::new();
    let mut names = Vec::new();
    let mut warnings = Vec::new();
    let mut bytes = 0u64;
    for (name, entry) in &journal.entries {
        let archived = snapshot(operation, name, MAX_FILE_BYTES)?;
        let original = snapshot(logs, name, MAX_FILE_BYTES)?;
        if let Some((stamp, digest, _)) = &archived {
            names.push(name.clone());
            bytes += stamp.bytes;
            if !entry.stamp.after_move_matches(stamp) || digest != &entry.digest {
                warnings.push(format!("恢复副本已变化，将保留并拒绝自动恢复：{name}"));
            }
            if original.is_some() {
                warnings.push(format!("原位置已有文件，恢复不会覆盖：{name}"));
            }
        } else if original.as_ref().is_none_or(|(stamp, digest, _)| {
            !entry.stamp.after_move_matches(stamp) || digest != &entry.digest
        }) {
            warnings.push(format!("日志不在原位置或恢复位置，请检查：{name}"));
        }
        states.insert(
            name,
            (
                archived.map(|(s, d, _)| (s, d)),
                original.map(|(s, d, _)| (s, d)),
            ),
        );
    }
    for name in operation.names()? {
        if name != "journal.json" && !journal.entries.contains_key(&name) {
            warnings.push(format!("恢复目录包含未知文件，已保留：{name}"));
        }
    }
    let revision = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                scope.identities()?,
                journal_digest,
                operation.stamp()?.identity(),
                states,
                &warnings
            ))
            .map_err(|e| e.to_string())?
        )
    );
    Ok(RecoveryRow {
        operation_id: journal.operation_id.clone(),
        revision,
        created_at: journal.created_at,
        needs_completion: names.is_empty() && warnings.is_empty(),
        file_count: names.len(),
        bytes,
        names,
        warnings,
    })
}

pub fn list_recovery(root: &Path) -> Result<Vec<RecoveryRow>, String> {
    let scope = Scope::open(root)?;
    let Some(store) = scope.store()? else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for name in store.names()?.into_iter().filter(|n| n != "lock") {
        // An interrupted initial directory registration, malformed journal or
        // foreign entry cannot hide other valid recovery operations. Surface it
        // as retained data; restore still demands a fully validated journal.
        let row = (|| {
            component(&name)?;
            let operation = store.child(&name)?;
            if completed(&operation)? {
                return Ok(None);
            }
            let (journal, digest) = journal(&scope, &operation, &name)?;
            recovery_row(&scope, &operation, &journal, &digest).map(Some)
        })()
        .unwrap_or_else(|error: String| {
            Some(RecoveryRow {
                operation_id: name,
                revision: String::new(),
                created_at: 0,
                file_count: 0,
                bytes: 0,
                names: Vec::new(),
                warnings: vec![error],
                needs_completion: false,
            })
        });
        let Some(row) = row else {
            continue;
        };
        if row.file_count > 0 || !row.warnings.is_empty() || row.needs_completion {
            rows.push(row);
        }
    }
    scope.recheck()?;
    rows.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.operation_id.cmp(&a.operation_id))
    });
    Ok(rows)
}

pub fn restore(
    root: &Path,
    operation_id: &str,
    expected_revision: &str,
    current: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<RestoreOutcome, String> {
    component(operation_id)?;
    cancelled(cancel)?;
    let scope = Scope::open(root)?;
    let store = scope.store()?.ok_or("没有日志恢复记录")?;
    let _lock = store.lock()?;
    let operation = store.child(operation_id)?;
    if completed(&operation)? {
        return Err("该日志恢复任务已经完成，请重新读取列表".into());
    }
    let (journal, digest) = journal(&scope, &operation, operation_id)?;
    let row = recovery_row(&scope, &operation, &journal, &digest)?;
    if row.revision != expected_revision {
        return Err("日志恢复记录或原位置已变化，请重新读取后恢复".into());
    }
    if !row.warnings.is_empty() {
        return Err(row.warnings.join("；"));
    }
    scope.recheck()?;
    let restored = restore_entries(
        scope.logs.as_ref().ok_or("日志目录不存在")?,
        &operation,
        &journal,
        scope.current_name(current).as_deref(),
        cancel,
    )?;
    scope.recheck()?;
    complete(&operation, &digest)?;
    Ok(RestoreOutcome {
        operation_id: operation_id.to_owned(),
        file_count: restored,
    })
}

/// Restore is safely retryable after interruption. It never overwrites an
/// existing log; each successful rename is already a durable restored file.
fn restore_entries(
    logs: &Dir,
    operation: &Dir,
    journal: &Journal,
    current: Option<&str>,
    cancel: &AtomicBool,
) -> Result<usize, String> {
    let mut restored = 0;
    for (name, entry) in &journal.entries {
        cancelled(cancel)?;
        let Some((stamp, digest, _)) = snapshot(operation, name, MAX_FILE_BYTES)? else {
            if snapshot(logs, name, MAX_FILE_BYTES)?.is_none_or(|(stamp, digest, _)| {
                !entry.stamp.after_move_matches(&stamp) || digest != entry.digest
            }) {
                return Err(format!("原日志发生变化或丢失，已保留其它恢复副本：{name}"));
            }
            continue;
        };
        if Some(name.as_str()) == current {
            return Err("恢复记录包含当前会话日志，已保留恢复副本".into());
        }
        if !entry.stamp.after_move_matches(&stamp) || digest != entry.digest {
            return Err(format!("恢复副本发生变化，已保留：{name}"));
        }
        if logs.file(name)?.is_some() {
            return Err(format!("原位置已有日志，已保留恢复副本：{name}"));
        }
        operation.move_new(name, logs)?;
        restored += 1;
    }
    Ok(restored)
}
