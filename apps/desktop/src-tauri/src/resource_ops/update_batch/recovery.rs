//! Phase-specific rollback and root recovery. Every move is no-replace and
//! requires a matching inode/hash. Durable RolledBack/Restored states make
//! interruption during private cleanup repeatable without requiring deleted
//! backup files to reappear. Unknown files are always retained.
use super::*;
fn operation(context: &Context) -> Result<&Dir> {
    context.operation.as_ref().ok_or("资源更新目录缺失".into())
}
fn present(dir: &Dir, name: &str, owned: &Owned) -> Result<bool> {
    if dir.stat(name)?.is_none() {
        return Ok(false);
    }
    verify_owned(dir, name, owned)?;
    Ok(true)
}
fn matches(dir: &Dir, name: &str, owned: &Owned) -> Result<bool> {
    if dir.stat(name)?.is_none() {
        return Ok(false);
    }
    Ok(verify_owned(dir, name, owned).is_ok())
}
fn private_inventory(context: &Context, j: &Journal, require_backups: bool) -> Result<()> {
    context.check(j)?;
    let op = operation(context)?;
    for name in op.names_with_limit(MAX_FILES * 5 + 16)? {
        let mut accepted = false;
        for (index, item) in j.items.iter().enumerate() {
            if name == slot("new", index) {
                verify_owned(
                    op,
                    &name,
                    item.new.as_ref().ok_or("新暂存文件缺少持久登记")?,
                )?;
                accepted = true;
                break;
            }
            if name == slot("backup", index) {
                verify_owned(
                    op,
                    &name,
                    item.old
                        .as_ref()
                        .and_then(|o| o.backup.as_ref())
                        .ok_or("旧备份缺少持久登记")?,
                )?;
                accepted = true;
                break;
            }
            if name == slot("old", index) {
                verify_owned(op, &name, &item.old.as_ref().ok_or("未登记旧文件")?.owned)?;
                accepted = true;
                break;
            }
        }
        if accepted {
            continue;
        }
        if let Some(kind) = name.strip_prefix("target-") {
            let b = j
                .scope
                .resources
                .get(kind)
                .ok_or("私有新资源目录类型无效")?;
            let dir = op.child(&name)?;
            if b.before.is_some()
                || b.created
                    .as_ref()
                    .is_none_or(|k| dir.key().as_ref() != Ok(k))
                || !dir.names()?.is_empty()
            {
                return Err("私有新资源目录冲突，已保留".into());
            }
            continue;
        }
        journal::validate_private_name(context, j, &name)?;
    }
    if require_backups {
        for (index, item) in j.items.iter().enumerate() {
            if let Some(old) = &item.old {
                if !present(
                    op,
                    &slot("backup", index),
                    old.backup.as_ref().ok_or("旧备份未登记")?,
                )? {
                    return Err("旧文件持久备份已移走".into());
                }
            }
        }
    }
    Ok(())
}
pub(super) fn verify_committed_private(context: &Context, j: &Journal) -> Result<()> {
    private_inventory(context, j, true)?;
    let op = operation(context)?;
    for (index, item) in j.items.iter().enumerate() {
        if op.stat(&slot("new", index))?.is_some() {
            return Err("已提交的新文件仍占用私有暂存名".into());
        }
        if let Some(old) = &item.old {
            if !present(op, &slot("old", index), &old.owned)? {
                return Err("撤销所需原文件已移走".into());
            }
        }
    }
    Ok(())
}
fn applying_layout(context: &Context, j: &Journal) -> Result<()> {
    private_inventory(context, j, true)?;
    let op = operation(context)?;
    for (index, item) in j.items.iter().enumerate() {
        let dir = context.resources(j, &item.kind, true)?;
        let new = item.new.as_ref().ok_or("新文件所有权缺失")?;
        let private_new = present(op, &slot("new", index), new)?;
        if let Some(dir) = &dir {
            if dir.stat(&item.name)?.is_some() {
                if matches(dir, &item.name, new)? {
                    if private_new {
                        return Err("更新新文件出现额外路径，已保留".into());
                    }
                } else if item.old.as_ref().is_some_and(|o| {
                    o.name == item.name && verify_owned(dir, &o.name, &o.owned).is_ok()
                }) {
                } else {
                    return Err("更新目标含外部文件，已保留".into());
                }
            } else if !private_new {
                return Err("已发布更新文件被移走，已保留".into());
            }
        } else if !private_new {
            return Err("已发布更新资源目录已移走".into());
        }
        if let Some(old) = &item.old {
            let private_old = present(op, &slot("old", index), &old.owned)?;
            let public_old = dir
                .as_ref()
                .map(|dir| matches(dir, &old.name, &old.owned))
                .transpose()?
                .unwrap_or(false);
            if private_old && public_old || !private_old && !public_old {
                return Err("旧资源原 inode 丢失或存在额外路径，已保留".into());
            }
            if private_old {
                if let Some(dir) = &dir {
                    if dir.stat(&old.name)?.is_some() && !matches(dir, &old.name, new)? {
                        return Err("原文件恢复位置含外部文件，已保留".into());
                    }
                }
            }
        }
    }
    Ok(())
}
fn rollback_apply(context: &Context, j: &Journal) -> Result<()> {
    applying_layout(context, j)?;
    let op = operation(context)?;
    for (index, item) in j.items.iter().enumerate() {
        context.check(j)?;
        let Some(dir) = context.resources(j, &item.kind, true)? else {
            continue;
        };
        let new = item.new.as_ref().ok_or("新资源所有权缺失")?;
        if matches(&dir, &item.name, new)? {
            op.absent(&slot("new", index))?;
            verify_owned(&dir, &item.name, new)?;
            dir.move_directory(&item.name, op, &slot("new", index))?;
        }
    }
    for (index, item) in j.items.iter().enumerate() {
        if let Some(old) = &item.old {
            context.check(j)?;
            let dir = context
                .resources(j, &item.kind, true)?
                .ok_or("原资源目录缺失")?;
            if present(op, &slot("old", index), &old.owned)? {
                dir.absent(&old.name)?;
                verify_owned(op, &slot("old", index), &old.owned)?;
                op.move_directory(&slot("old", index), &dir, &old.name)?;
            }
            verify_owned(&dir, &old.name, &old.owned)?;
        }
    }
    scope::remove_created_targets(context, j)?;
    let mut rolled_back = j.clone();
    rolled_back.state = State::RolledBack;
    journal::write(context, &rolled_back)?;
    discard(context, &rolled_back)
}
fn undo_layout(context: &Context, j: &Journal) -> Result<()> {
    private_inventory(context, j, true)?;
    let op = operation(context)?;
    for (index, item) in j.items.iter().enumerate() {
        let dir = context.resources(j, &item.kind, true)?;
        let new = item.new.as_ref().ok_or("新资源所有权缺失")?;
        let private_new = present(op, &slot("new", index), new)?;
        let public_new = dir
            .as_ref()
            .map(|d| matches(d, &item.name, new))
            .transpose()?
            .unwrap_or(false);
        if private_new && public_new || !private_new && !public_new {
            return Err("撤销时新资源 inode 丢失或含额外路径".into());
        }
        if private_new {
            if let Some(dir) = &dir {
                if dir.stat(&item.name)?.is_some()
                    && !item.old.as_ref().is_some_and(|o| {
                        o.name == item.name && verify_owned(dir, &o.name, &o.owned).is_ok()
                    })
                {
                    return Err("新资源恢复位置含外部文件，已保留".into());
                }
            }
        }
        if let Some(old) = &item.old {
            let private_old = present(op, &slot("old", index), &old.owned)?;
            let public_old = dir
                .as_ref()
                .map(|d| matches(d, &old.name, &old.owned))
                .transpose()?
                .unwrap_or(false);
            if private_old && public_old || !private_old && !public_old {
                return Err("撤销时原文件 inode 丢失或含额外路径".into());
            }
            if private_old {
                if let Some(dir) = &dir {
                    if dir.stat(&old.name)?.is_some() && !matches(dir, &old.name, new)? {
                        return Err("原文件撤销位置含外部文件，已保留".into());
                    }
                }
            }
        }
    }
    Ok(())
}
fn rollback_undo(context: &Context, j: &Journal) -> Result<()> {
    undo_layout(context, j)?;
    let op = operation(context)?;
    for (index, item) in j.items.iter().enumerate() {
        if let Some(old) = &item.old {
            context.check(j)?;
            let Some(dir) = context.resources(j, &item.kind, true)? else {
                continue;
            };
            if matches(&dir, &old.name, &old.owned)? {
                op.absent(&slot("old", index))?;
                verify_owned(&dir, &old.name, &old.owned)?;
                dir.move_directory(&old.name, op, &slot("old", index))?;
            }
        }
    }
    // Undo keeps created resource directories until Restored is durable, so
    // their original inode binding remains usable during rollback retries.
    let mut resumed = j.clone();
    for (index, item) in resumed.items.iter().enumerate() {
        context.check(&resumed)?;
        let dir = context
            .resources(&resumed, &item.kind, true)?
            .ok_or("新资源恢复目录缺失")?;
        let new = item.new.as_ref().ok_or("新资源所有权缺失")?;
        if present(op, &slot("new", index), new)? {
            dir.absent(&item.name)?;
            dir.absent(&counterpart(&item.name))?;
            op.move_directory(&slot("new", index), &dir, &item.name)?;
        }
        verify_owned(&dir, &item.name, new)?;
    }
    resumed.state = State::Committed;
    journal::write(context, &resumed)?;
    journal::remove_marker(context, &resumed)
}
fn verify_rolled_back(context: &Context, j: &Journal) -> Result<()> {
    for item in &j.items {
        if let Some(old) = &item.old {
            let dir = context
                .resources(j, &item.kind, true)?
                .ok_or("原资源恢复目录缺失")?;
            verify_owned(&dir, &old.name, &old.owned)?;
        }
        if let Some(dir) = context.resources(j, &item.kind, true)? {
            if item
                .new
                .as_ref()
                .is_some_and(|new| verify_owned(&dir, &item.name, new).is_ok())
            {
                return Err("已回滚的新文件重新出现在资源目录，已保留".into());
            }
        }
    }
    Ok(())
}
fn discard(context: &Context, j: &Journal) -> Result<()> {
    private_inventory(context, j, false)?;
    let op = operation(context)?;
    // Validate the whole private inventory first; delete data before receipts.
    for (index, item) in j.items.iter().enumerate() {
        for (prefix, owned) in [
            ("new", item.new.as_ref()),
            ("backup", item.old.as_ref().and_then(|o| o.backup.as_ref())),
        ] {
            let name = slot(prefix, index);
            if op.stat(&name)?.is_some() {
                let owned = owned.ok_or("待清理更新文件缺少持久登记")?;
                context.check(j)?;
                verify_owned(op, &name, owned)?;
                op.unlink(&name, false)?;
            }
        }
        if op.stat(&slot("old", index))?.is_some() {
            return Err("待清理更新历史仍含未还原原 inode".into());
        }
    }
    for (kind, b) in &j.scope.resources {
        let name = format!("target-{kind}");
        if let Some(dir) = op.optional(&name)? {
            if b.before.is_some()
                || b.created
                    .as_ref()
                    .is_none_or(|key| dir.key().as_ref() != Ok(key))
                || !dir.names()?.is_empty()
            {
                return Err("私有新资源目录冲突".into());
            }
            context.check(j)?;
            if op.child(&name)?.key()? != dir.key()? {
                return Err("私有新资源目录清理前被替换".into());
            }
            op.unlink(&name, true)?;
        }
    }
    let names = op.names_with_limit(MAX_FILES * 5 + 16)?;
    for name in &names {
        journal::validate_private_name(context, j, name)?;
    }
    // Staging's full manifest may predate marker registration. Preserve its
    // marker receipt until the root marker is gone, otherwise interruption
    // between receipt deletion and marker unlink would strand root recovery.
    journal::remove_marker(context, j)?;
    for name in names {
        if matches!(name.as_str(), "journal.json" | "status.json")
            || (cfg!(windows) && name == "owned-marker.json")
        {
            continue;
        }
        context.check(j)?;
        journal::validate_private_name(context, j, &name)?;
        op.unlink(&name, false)?;
    }
    #[cfg(windows)]
    {
        op.check_cleanup()?;
        let mut terminal = j.clone();
        terminal.state = if j.state == State::Restored {
            State::FinishedRestored
        } else {
            State::FinishedRolledBack
        };
        journal::write(context, &terminal)?;
        journal::status(context, &terminal)?;
        return verify_terminal(context, &terminal);
    }
    #[cfg(not(windows))]
    {
        for name in ["status.json", "journal.json"] {
            if op.stat(name)?.is_some() {
                context.check(j)?;
                journal::validate_private_name(context, j, name)?;
                op.unlink(name, false)?;
            }
        }
        context.check(j)?;
        if !op.names_with_limit(MAX_FILES * 5 + 16)?.is_empty() {
            return Err("更新记录清理时出现未知文件".into());
        }
        let history = context.history.as_ref().ok_or("历史目录缺失")?;
        if history.child(&j.id)?.key()? != op.key()? {
            return Err("更新记录清理前被替换".into());
        }
        history.unlink(&j.id, true)
    }
}
#[cfg(windows)]
pub(super) fn verify_terminal(context: &Context, j: &Journal) -> Result<()> {
    if !j.state.terminal() {
        return Err("更新事务尚未到达清理终态".into());
    }
    context.check(j)?;
    let op = operation(context)?;
    for name in op.names_with_limit(16)? {
        if !matches!(
            name.as_str(),
            "journal.json" | "status.json" | "owned-marker.json"
        ) {
            return Err("已清理更新终态仍有未知或原件文件，已保留".into());
        }
        journal::validate_private_name(context, j, &name)?;
    }
    let actual = journal::read(context)?;
    if actual.state != j.state || actual.id != j.id || actual.scope.operation != j.scope.operation {
        return Err("更新终态日志已经变化".into());
    }
    Ok(())
}
pub(super) fn cleanup_restored(context: &Context, j: &Journal) -> Result<()> {
    verify_rolled_back(context, j)?;
    scope::remove_created_targets(context, j)?;
    discard(context, j)
}
pub(super) fn one(context: &Context, j: &Journal) -> Result<()> {
    context.check(j)?;
    #[cfg(windows)]
    if j.state.terminal() {
        return verify_terminal(context, j);
    }
    // Even precommit cleanup must retain its record when a bound namespace was
    // replaced. The error is actionable evidence; silently dropping the record
    // would conceal which captured scope the failed transaction actually owned.
    for kind in j.scope.resources.keys() {
        context.resources(j, kind, true)?;
    }
    match j.state {
        State::Staging | State::Prepared => discard(context, j),
        State::Applying => rollback_apply(context, j),
        State::Committed => {
            context.inventory_matches(j, false, None)?;
            verify_committed_private(context, j)?;
            journal::status(context, j)?;
            journal::remove_marker(context, j)
        }
        State::UndoPrepared => {
            context.inventory_matches(j, false, None)?;
            verify_committed_private(context, j)?;
            let mut committed = j.clone();
            committed.state = State::Committed;
            journal::write(context, &committed)?;
            journal::remove_marker(context, &committed)
        }
        State::UndoApplying => rollback_undo(context, j),
        State::RolledBack | State::Restored => cleanup_restored(context, j),
        #[cfg(windows)]
        State::FinishedRolledBack | State::FinishedRestored => verify_terminal(context, j),
    }
}
pub(super) fn setup_failed(context: &Context, j: &Journal) -> Result<()> {
    let op = operation(context)?;
    if op.stat("journal.json")?.is_some() {
        return one(context, j);
    }
    if !op.names_with_limit(MAX_FILES * 5 + 16)?.is_empty() {
        return Err("未登记更新目录包含文件，已保留".into());
    }
    journal::remove_marker(context, j)?;
    context.check(j)?;
    context
        .history
        .as_ref()
        .ok_or("历史目录缺失")?
        .unlink(&j.id, true)
}
fn orphan(context: &Context) -> Result<()> {
    let op = operation(context)?;
    if !op.names_with_limit(MAX_FILES * 5 + 16)?.is_empty() {
        return Err("未登记更新历史含未知文件，已保留".into());
    }
    journal::remove_orphan_marker(context)?;
    let current = Context::existing(&context.path, &context.id, &context.operation_id)?;
    if operation(&current)?.key()? != op.key()?
        || current.instance.key()? != context.instance.key()?
    {
        return Err("孤立更新目录已被替换".into());
    }
    context
        .history
        .as_ref()
        .ok_or("历史目录缺失")?
        .unlink(&context.operation_id, true)
}
pub(super) fn root(root: &Path) -> Result<MutationResult> {
    let path = root.canonicalize().map_err(err)?;
    let _lock = crate::instance_rename_refs::root_history_lock(&path)?;
    super::super::ensure_local_resources_ready(&path)?;
    super::super::verified_batch::ensure_ready(&path)?;
    let contexts = scope::contexts(&path)?;
    let mut active = BTreeSet::new();
    if let Some(markers) = scope::marker_store(&path)? {
        for name in markers.names()? {
            let marker = journal::read_marker(&markers, &name)?;
            active.insert(journal::marker_context(&marker, &contexts)?);
        }
    }
    let mut changed = 0;
    for (index, context) in contexts.iter().enumerate() {
        let op = operation(context)?;
        if op.stat("journal.json")?.is_none() {
            orphan(context)?;
            changed += 1;
            continue;
        }
        let j = journal::read(context)?;
        if j.state.pending() || active.contains(&index) || !journal::status_ready(context)? {
            one(context, &j).map_err(|e| format!("资源更新恢复失败：{e}"))?;
            changed += 1;
        }
    }
    scope::ensure_ready(&path)?;
    Ok(MutationResult {
        changed,
        undo_id: None,
        message: format!("已恢复 {changed} 个资源更新或撤销事务"),
    })
}
