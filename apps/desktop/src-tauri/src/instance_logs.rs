//! Explicit game-log selection and export. A listing binds configuration and
//! directory identities, while each row separately binds its file stamp. Thus
//! an unrelated log append cannot retire every row. No source mutation, recovery,
//! current-failure inference or arbitrary source path is part of this service.
//! Export reuses the verified, redacted read carrier and a native chosen file.
mod binding;
pub(super) mod export;
mod reader;
#[cfg(test)]
mod tests;

use crate::{config::GameRoot, launcher_local::filesystem::Stamp, Shared};
use binding::{Binding, Selected};
use serde::Serialize;
mod model;
use model::digest;
pub(super) use model::LogKind;
use std::{
    path::Path,
    sync::{atomic::Ordering, MutexGuard},
};

pub(super) const CHANGED: &str = "游戏日志或实例目录已变化，请刷新列表后重新选择日志";
const MAX_ROWS: usize = 512;

#[derive(Serialize)]
pub(super) struct LogRow {
    pub kind: LogKind,
    pub name: String,
    pub size: u64,
    pub modified: Option<u64>,
    pub revision: String,
}
#[derive(Serialize)]
pub(super) struct LogListing {
    pub id: String,
    pub game_dir: String,
    pub shared: bool,
    pub revision: String,
    pub entries: Vec<LogRow>,
    pub truncated: bool,
}
#[derive(Serialize)]
pub(super) struct LogContent {
    pub id: String,
    pub kind: LogKind,
    pub name: String,
    pub text: String,
    pub truncated: bool,
    pub content_revision: String,
}

fn row_revision(scope: &str, kind: LogKind, name: &str, stamp: &Stamp) -> Result<String, String> {
    digest(&(scope, kind, name, stamp))
}

/// These are read admission checks, so an already running game is allowed.
/// Existing transaction journals still block inspection of an unstable scope;
/// checking them never invokes recovery or creates a lock file.
fn ready(shared: &Shared, root: &GameRoot) -> Result<(), String> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err("启动器正在关闭，请重新打开后查看游戏日志".into());
    }
    if shared.status.lock().unwrap().stage == "preparing" {
        return Err("请等待游戏启动准备结束后查看游戏日志".into());
    }
    if shared.tasks.active().is_some() {
        return Err("请等待当前文件任务结束后查看游戏日志".into());
    }
    crate::ensure_instance_files_ready(shared, root)?;
    crate::instance_reset::ensure_ready(Path::new(&root.path))?;
    crate::resource_ops::ensure_ready(Path::new(&root.path))
}

struct Capture {
    root: GameRoot,
    settings_revision: String,
    close_generation: u64,
    tasks_revision: u64,
    binding: Binding,
    revision: String,
}
fn capture(shared: &Shared, root_id: &str, id: &str) -> Result<Capture, String> {
    let _operation = shared.operations.lock().unwrap();
    pcl_core::identifier(id)?;
    let settings = shared.config.checked_snapshot()?;
    if settings.root_id != root_id {
        return Err(CHANGED.into());
    }
    let isolation = shared.config.isolation_settings(Some(root_id))?;
    if isolation.revision != settings.revision {
        return Err(CHANGED.into());
    }
    let root = isolation.root.clone();
    // resolve() supplies the launcher's canonical root projection. Walk the
    // original registered path without following links as well: canonicalizing
    // first must not authorize a newly substituted root/ancestor symlink.
    let binding = Binding::capture_with_isolation(
        Path::new(&isolation.registered_path),
        id,
        isolation.policy(id),
    )?;
    ready(shared, &root)?;
    let close_generation = shared.close_generation.load(Ordering::SeqCst);
    let tasks_revision = shared.tasks.revision();
    let revision = digest(&(
        &root,
        &settings.revision,
        close_generation,
        tasks_revision,
        binding.signature()?,
    ))?;
    Ok(Capture {
        root,
        settings_revision: settings.revision,
        close_generation,
        tasks_revision,
        binding,
        revision,
    })
}
fn verify(shared: &Shared, captured: &Capture, selected: Option<&Selected>) -> Result<(), String> {
    let _operation = shared.operations.lock().unwrap();
    verify_locked(shared, captured, selected)
}
/// Caller owns operations. Export returns that guard from its final authorize
/// callback so no writer is admitted between this check and file publication.
fn verify_locked(
    shared: &Shared,
    captured: &Capture,
    selected: Option<&Selected>,
) -> Result<(), String> {
    if shared.closing.load(Ordering::SeqCst)
        || shared.close_generation.load(Ordering::SeqCst) != captured.close_generation
        || shared.tasks.revision() != captured.tasks_revision
        || shared.config.checked_snapshot()?.revision != captured.settings_revision
        || shared.config.resolve(Some(&captured.root.id))? != captured.root
    {
        return Err(CHANGED.into());
    }
    ready(shared, &captured.root)?;
    captured.binding.verify()?;
    if let Some(selected) = selected {
        selected.verify(&captured.binding)?;
    }
    Ok(())
}

struct PreparedRead {
    captured: Capture,
    selected: Selected,
    content: LogContent,
}
impl PreparedRead {
    fn authorize<'a>(&self, shared: &'a Shared) -> Result<MutexGuard<'a, ()>, String> {
        let guard = shared.operations.lock().unwrap();
        verify_locked(shared, &self.captured, Some(&self.selected))
            .map_err(|_| CHANGED.to_string())?;
        Ok(guard)
    }
}

pub(super) fn list(shared: &Shared, root_id: &str, id: &str) -> Result<LogListing, String> {
    list_with(shared, root_id, id, |_| {})
}
fn list_with(
    shared: &Shared,
    root_id: &str,
    id: &str,
    after_scan: impl FnOnce(&Capture),
) -> Result<LogListing, String> {
    let mut captured = capture(shared, root_id, id)?;
    // JSON validation, directory enumeration and metadata probes all run after
    // releasing operations. A supported unsafe carrier refuses the listing;
    // neither a symlink nor an unreadable entry is silently treated as absent.
    let result = (|| {
        captured.binding.validate_json()?;
        let mut entries = Vec::new();
        for kind in [LogKind::Game, LogKind::Crash] {
            let Some(folder) = captured.binding.folder(kind) else {
                continue;
            };
            // Existing names() bounds enumeration at 4096 entries per folder.
            for name in folder.names()? {
                if !kind.accepts(&name) {
                    continue;
                }
                binding::valid_name(kind, &name)?;
                let file = folder.file(&name)?.ok_or(CHANGED)?;
                let metadata = file.metadata().map_err(|_| "无法检查游戏日志")?;
                let stamp = Stamp::from_file(&file)?;
                entries.push(LogRow {
                    kind,
                    revision: row_revision(&captured.revision, kind, &name, &stamp)?,
                    name,
                    size: metadata.len(),
                    modified: metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs()),
                });
            }
        }
        entries.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.kind.directory().cmp(b.kind.directory()))
                .then_with(|| a.name.cmp(&b.name))
        });
        let truncated = entries.len() > MAX_ROWS;
        entries.truncate(MAX_ROWS);
        Ok(LogListing {
            id: id.into(),
            game_dir: captured.binding.game_path().display().to_string(),
            shared: captured.binding.is_shared(),
            revision: captured.revision.clone(),
            entries,
            truncated,
        })
    })();
    after_scan(&captured);
    // Verify even an I/O error before publishing it. Error text cannot describe
    // a scope that was replaced while the slow work ran.
    verify(shared, &captured, None).map_err(|_| CHANGED.to_string())?;
    result
}

pub(super) fn read(
    shared: &Shared,
    root_id: &str,
    id: &str,
    kind: LogKind,
    name: &str,
    revision: &str,
    listing_revision: &str,
) -> Result<LogContent, String> {
    read_with(
        shared,
        root_id,
        id,
        kind,
        name,
        revision,
        listing_revision,
        |selected| selected.read(&shared.closing),
        |text| shared.accounts.redact(text.to_owned()),
    )
}
#[allow(clippy::too_many_arguments)]
fn read_with(
    shared: &Shared,
    root_id: &str,
    id: &str,
    kind: LogKind,
    name: &str,
    revision: &str,
    listing_revision: &str,
    load: impl FnOnce(&mut Selected) -> Result<reader::TextTail, String>,
    redact: impl FnOnce(&str) -> String,
) -> Result<LogContent, String> {
    prepare_read_with(
        shared,
        root_id,
        id,
        kind,
        name,
        revision,
        listing_revision,
        load,
        redact,
    )
    .map(|prepared| prepared.content)
}
#[allow(clippy::too_many_arguments)]
fn prepare_read_with(
    shared: &Shared,
    root_id: &str,
    id: &str,
    kind: LogKind,
    name: &str,
    revision: &str,
    listing_revision: &str,
    load: impl FnOnce(&mut Selected) -> Result<reader::TextTail, String>,
    redact: impl FnOnce(&str) -> String,
) -> Result<PreparedRead, String> {
    binding::valid_name(kind, name)?;
    if revision.len() != 64 || listing_revision.len() != 64 {
        return Err(CHANGED.into());
    }
    let mut captured = capture(shared, root_id, id)?;
    if captured.revision != listing_revision {
        return Err(CHANGED.into());
    }
    let mut selected = Selected::capture(&captured.binding, kind, name)?;
    if row_revision(&captured.revision, kind, name, &selected.stamp)? != revision {
        return Err(CHANGED.into());
    }
    let result: Result<LogContent, String> = (|| {
        captured.binding.validate_json()?;
        let tail = load(&mut selected)?;
        // Only the currently loaded text is passed to the existing account
        // redactor. Retain a bounded Unicode suffix if replacement expanded it.
        let safe = reader::bound_redacted(redact(&tail.text), tail.truncated);
        let content_revision = digest(&(
            "instance-log-content-v1",
            listing_revision,
            revision,
            id,
            kind,
            name,
            &safe.text,
            safe.truncated,
        ))?;
        Ok(LogContent {
            id: id.into(),
            kind,
            name: name.into(),
            text: safe.text,
            truncated: safe.truncated,
            content_revision,
        })
    })();
    verify(shared, &captured, Some(&selected)).map_err(|_| CHANGED.to_string())?;
    Ok(PreparedRead {
        captured,
        selected,
        content: result?,
    })
}
