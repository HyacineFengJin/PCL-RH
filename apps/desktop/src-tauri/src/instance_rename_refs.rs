//! Replayable launcher-reference migration for physical instance rename.
//!
//! `instance_rename` owns the physical files and journal phase. This module
//! snapshots and updates only launcher settings, instance metadata, export
//! presets and resource undo journals. The application service refreshes its
//! in-memory stores after the transaction completes.
//!
//! # Replay contract
//!
//! Each typed target records its before snapshot and intended after bytes.
//! Before the first physical move, `verify_before` requires the exact snapshot.
//! Once files are committed, `apply` permits either before or intended after
//! content: already applied updates are skipped, and unapplied updates resume.
//! Any third state is an external conflict and remains untouched.
//!
//! A serialized delta is not write authority. `validate` reconstructs every
//! allowed rename transformation and checks limits/hashes. `Target` resolves to
//! fixed store locations; payloads never choose arbitrary destination paths.
//! Directory identities and bound resource-history lists are also rechecked.
//!
//! # Coordination
//!
//! The project marker survives restarts and blocks cooperating reference writers
//! across all registered roots. `mark_pending` captures it under the same store
//! locks used for replay; clearing requires the matching operation and marker
//! identity. An offline game root must not make the marker disappear.
//!
//! FD-based checked store helpers here are also used by `config` and
//! `instance_meta`; their disk snapshots detect external edits independently of
//! transport revisions. I/O conflict and domain-replay tests live in
//! `instance_rename_refs/tests.rs`.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
#[path = "instance_rename_refs/filesystem.rs"]
mod filesystem;
use filesystem::{absolute, hash, nonce, Dir, MARKER, MAX_HISTORY, PRESET_LIMIT};
pub(crate) use filesystem::{DiskSnapshot, Identity, ReferenceLock};

const SETTINGS_LIMIT: usize = 2 * 1024 * 1024;
const METADATA_LIMIT: usize = 8 * 1024 * 1024;
const RESOURCE_LIMIT: usize = 2 * 1024 * 1024;
const PAYLOAD_LIMIT: usize = 32 * 1024 * 1024;
const PENDING_ERROR: &str = "存在未完成的实例重命名，请先恢复后再操作或启动";

fn id(value: &str) -> Result<(), String> {
    pcl_core::identifier(value)?;
    if value.len() > 255 || value.chars().any(char::is_control) {
        return Err("实例名称无效".into());
    }
    Ok(())
}

fn wire_version() -> u32 {
    if cfg!(windows) {
        2
    } else {
        1
    }
}

fn app_directory(project: &Path, create: bool) -> Result<Option<Dir>, String> {
    let project = Dir::open(project)?;
    if create {
        project.ensure(".pcl-rust").map(Some)
    } else {
        project.optional(".pcl-rust")
    }
}

pub(crate) fn store_snapshot(
    project: &Path,
    filename: &str,
    limit: usize,
) -> Result<(DiskSnapshot, Option<Vec<u8>>), String> {
    if !matches!(filename, "settings.json" | "instance-metadata.json") {
        return Err("引用文件类型无效".into());
    }
    let Some(dir) = app_directory(project, false)? else {
        return Ok((DiskSnapshot::default(), None));
    };
    let identity = dir.identity()?;
    let (snapshot, bytes) = dir.snapshot(filename, limit)?;
    if app_directory(project, false)?
        .map(|dir| dir.identity())
        .transpose()?
        != Some(identity.clone())
    {
        return Err("引用资料目录已被外部替换".into());
    }
    Ok((snapshot, bytes))
}

pub(crate) fn with_settings_lock<T>(
    project: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let dir = app_directory(project, true)?.ok_or("设置目录不可用")?;
    let _lock = dir.lock(".settings.lock")?;
    if app_directory(project, false)?
        .map(|dir| dir.identity())
        .transpose()?
        != Some(dir.identity()?)
    {
        return Err("设置目录在锁定期间已改变".into());
    }
    f()
}

pub(crate) fn backup_legacy_checked(
    project: &Path,
    bytes: &[u8],
    expected: &DiskSnapshot,
) -> Result<(), String> {
    backup_settings_checked(project, bytes, expected, 1)
}

pub(crate) fn backup_settings_checked(
    project: &Path,
    bytes: &[u8],
    expected: &DiskSnapshot,
    source_version: u32,
) -> Result<(), String> {
    if !matches!(source_version, 1 | 2) {
        return Err("设置备份版本无效".into());
    }
    let dir = app_directory(project, false)?.ok_or("设置目录不可用")?;
    let (now, current) = store_snapshot(project, "settings.json", SETTINGS_LIMIT)?;
    if !expected.compatible(&now) || current.as_deref() != Some(bytes) {
        return Err("旧设置在备份前已变化，原文件已保留".into());
    }
    let name = format!("settings.v{source_version}-backup-{}.json", nonce());
    dir.write_backup(&name, bytes, SETTINGS_LIMIT)
}

pub(crate) fn write_store_checked(
    project: &Path,
    filename: &str,
    bytes: &[u8],
    expected: &DiskSnapshot,
    limit: usize,
) -> Result<DiskSnapshot, String> {
    if !matches!(filename, "settings.json" | "instance-metadata.json") {
        return Err("引用文件类型无效".into());
    }
    let dir = app_directory(project, true)?.ok_or("引用目录不可用")?;
    let (now, _) = store_snapshot(project, filename, limit)?;
    if now.directory != Some(dir.identity()?) {
        return Err("引用资料目录已被外部替换".into());
    }
    if !expected.compatible(&now) {
        return Err("设置文件已被外部修改，未覆盖原文件，请重新打开启动器".into());
    }
    let saved = dir.update_checked(filename, &now, Some(bytes), limit);
    let (disk, current) = store_snapshot(project, filename, limit)?;
    if disk.directory == Some(dir.identity()?) && current.as_deref() == Some(bytes) {
        Ok(disk)
    } else {
        saved?;
        Err("引用资料在保存期间已变化，原文件已保留".into())
    }
}

pub(crate) use filesystem::root_history_lock;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
// Keep replay destinations closed and typed. Adding a new target also requires
// defining its location, size limit and authorized transform in validate().
enum Target {
    Settings,
    Metadata,
    PresetOld,
    PresetNew,
    ResourceJournal { operation_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Delta {
    target: Target,
    snapshot: DiskSnapshot,
    before: Option<String>,
    after: Option<String>,
    after_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A journal payload whose authority is revalidated before every write/replay.
pub struct RenameReferences {
    schema_version: u32,
    root_id: String,
    root_path: String,
    old_id: String,
    new_id: String,
    project_identity: Identity,
    root_identity: Identity,
    history_directory: Option<Identity>,
    history_ids: Vec<String>,
    changes: Vec<Delta>,
}

fn decoded(value: &Option<String>, limit: usize) -> Result<Option<Vec<u8>>, String> {
    value
        .as_ref()
        .map(|value| {
            if value.len() > (limit + 2) / 3 * 4 {
                return Err("重命名引用载荷过大".into());
            }
            let bytes = STANDARD
                .decode(value)
                .map_err(|_| "重命名引用载荷编码无效".to_string())?;
            if bytes.len() > limit {
                return Err("重命名引用载荷过大".into());
            }
            Ok(bytes)
        })
        .transpose()
}

fn history_directory(root: &Path) -> Result<Option<Dir>, String> {
    let Some(pcl) = Dir::open(root)?.optional(".pcl-linux")? else {
        return Ok(None);
    };
    pcl.optional("resource-operations")
}

fn preset_directory(project: &Path, create: bool) -> Result<Option<Dir>, String> {
    let Some(app) = app_directory(project, create)? else {
        return Ok(None);
    };
    if create {
        app.ensure("export-presets").map(Some)
    } else {
        app.optional("export-presets")
    }
}

struct Locks {
    _settings: ReferenceLock,
    _metadata: ReferenceLock,
    _presets: Vec<ReferenceLock>,
    _history: ReferenceLock,
}

impl RenameReferences {
    fn limit(target: &Target) -> usize {
        match target {
            Target::Settings => SETTINGS_LIMIT,
            Target::Metadata => METADATA_LIMIT,
            Target::PresetOld | Target::PresetNew => PRESET_LIMIT,
            Target::ResourceJournal { .. } => RESOURCE_LIMIT,
        }
    }
    fn locks(&self, project: &Path, root: &Path) -> Result<Locks, String> {
        // Keep one acquisition order across verification, marker creation and
        // replay. Preset lock names are sorted because old/new names can swap.
        let app = app_directory(project, true)?.ok_or("引用目录不可用")?;
        let settings = app.lock(".settings.lock")?;
        let metadata = app.lock(".instance-metadata.lock")?;
        let presets = preset_directory(project, true)?.ok_or("导出配置目录不可用")?;
        let mut names = [
            crate::export_presets::name(&self.root_id, &self.old_id),
            crate::export_presets::name(&self.root_id, &self.new_id),
        ];
        names.sort();
        let preset_locks = names
            .into_iter()
            .map(|name| presets.lock(&format!("{name}.lock")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Locks {
            _settings: settings,
            _metadata: metadata,
            _presets: preset_locks,
            _history: root_history_lock(root)?,
        })
    }
    fn location(
        &self,
        project: &Path,
        root: &Path,
        target: &Target,
    ) -> Result<(Dir, String), String> {
        match target {
            Target::Settings => Ok((
                app_directory(project, false)?.ok_or("设置目录已消失")?,
                "settings.json".into(),
            )),
            Target::Metadata => Ok((
                app_directory(project, false)?.ok_or("实例资料目录已消失")?,
                "instance-metadata.json".into(),
            )),
            Target::PresetOld | Target::PresetNew => Ok((
                preset_directory(project, false)?.ok_or("导出配置目录已消失")?,
                crate::export_presets::name(
                    &self.root_id,
                    if *target == Target::PresetOld {
                        &self.old_id
                    } else {
                        &self.new_id
                    },
                ),
            )),
            Target::ResourceJournal { operation_id } => {
                if !crate::resource_ops::valid_operation_id(operation_id) {
                    return Err("资源历史标识无效".into());
                }
                Ok((
                    history_directory(root)?
                        .ok_or("资源历史目录已消失")?
                        .child(operation_id)?,
                    "journal.json".into(),
                ))
            }
        }
    }
    fn capture(
        &mut self,
        project: &Path,
        root: &Path,
        target: Target,
        after: Option<Vec<u8>>,
    ) -> Result<(), String> {
        let (dir, name) = self.location(project, root, &target)?;
        let (snapshot, current) = dir.snapshot(&name, Self::limit(&target))?;
        self.changes.push(Delta {
            target,
            snapshot,
            before: current.map(|bytes| STANDARD.encode(bytes)),
            after_hash: after.as_ref().map(|bytes| hash(bytes)),
            after: after.map(|bytes| STANDARD.encode(bytes)),
        });
        Ok(())
    }
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != wire_version()
            || self.root_id.is_empty()
            || self.root_id.len() > 128
            || !self
                .root_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            || self.old_id == self.new_id
            || self.changes.len() > MAX_HISTORY + 4
            || self.history_ids.len() > MAX_HISTORY
        {
            return Err("重命名引用载荷范围无效".into());
        }
        id(&self.old_id)?;
        id(&self.new_id)?;
        absolute(Path::new(&self.root_path))?;
        if self.new_id.len() > 120
            || self.new_id.trim() != self.new_id
            || self.new_id.starts_with(".install-")
        {
            return Err("新的实例名称无效".into());
        }
        let mut seen = BTreeSet::new();
        let mut total = 0usize;
        let mut old_preset = None;
        for change in &self.changes {
            let key = serde_json::to_string(&change.target).map_err(|e| e.to_string())?;
            if !seen.insert(key) {
                return Err("重命名引用载荷含有重复目标".into());
            }
            let before = decoded(&change.before, Self::limit(&change.target))?;
            let after = decoded(&change.after, Self::limit(&change.target))?;
            total = total
                .checked_add(
                    before.as_ref().map_or(0, Vec::len) + after.as_ref().map_or(0, Vec::len),
                )
                .ok_or("重命名引用载荷过大")?;
            #[cfg(windows)]
            if !change.snapshot.valid_logical() {
                return Err("引用逻辑快照格式无效".into());
            }
            if total > PAYLOAD_LIMIT
                || before.as_ref().map(|bytes| hash(bytes))
                    != change
                        .snapshot
                        .file
                        .as_ref()
                        .map(|token| token.hash.clone())
                || after.as_ref().map(|bytes| hash(bytes)) != change.after_hash
                || change.snapshot.directory.is_none()
            {
                return Err("重命名引用载荷哈希或目录范围无效".into());
            }
            let expected = match (&change.target, before.as_deref()) {
                (Target::Settings, Some(bytes)) => Some(crate::config::rename_bytes(
                    bytes,
                    &self.root_id,
                    Path::new(&self.root_path),
                    &self.old_id,
                    &self.new_id,
                )?),
                (Target::Metadata, bytes) => Some(crate::instance_meta::rename_optional_bytes(
                    bytes,
                    &self.root_id,
                    Path::new(&self.root_path),
                    &self.old_id,
                    &self.new_id,
                )?),
                (Target::Settings, None) => None,
                (Target::PresetOld, bytes) => {
                    old_preset = bytes.map(Vec::from);
                    None
                }
                (Target::PresetNew, None) => continue,
                (Target::PresetNew, Some(_)) => return Err("目标实例已有导出配置，无法覆盖".into()),
                (Target::ResourceJournal { operation_id }, Some(bytes)) => {
                    if !crate::resource_ops::valid_operation_id(operation_id)
                        || !self.history_ids.contains(operation_id)
                    {
                        return Err("资源历史引用来源无效".into());
                    }
                    Some(
                        crate::resource_ops::rename_journal_bytes(
                            bytes,
                            operation_id,
                            &self.old_id,
                            &self.new_id,
                        )?
                        .unwrap_or_else(|| bytes.to_vec()),
                    )
                }
                (Target::ResourceJournal { .. }, None) => None,
            };
            if expected != after {
                return Err("重命名引用载荷包含非授权的内容变化".into());
            }
        }
        if !seen.contains("{\"type\":\"settings\"}")
            || !seen.contains("{\"type\":\"metadata\"}")
            || !seen.contains("{\"type\":\"preset_old\"}")
            || !seen.contains("{\"type\":\"preset_new\"}")
        {
            return Err("重命名引用载荷缺少必需目标".into());
        }
        let expected = old_preset
            .as_deref()
            .map(|bytes| {
                crate::export_presets::rename_bytes(
                    bytes,
                    &self.root_id,
                    Path::new(&self.root_path),
                    &self.old_id,
                    &self.new_id,
                )
            })
            .transpose()?;
        let new_preset = self
            .changes
            .iter()
            .find(|change| change.target == Target::PresetNew)
            .ok_or("引用载荷缺少导出配置目标")?;
        if decoded(&new_preset.after, PRESET_LIMIT)? != expected {
            return Err("导出配置迁移载荷来源无效".into());
        }
        if self
            .history_ids
            .iter()
            .any(|op| !crate::resource_ops::valid_operation_id(op))
            || self.history_ids.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("资源历史列表无效".into());
        }
        if self
            .changes
            .iter()
            .filter(|change| matches!(change.target, Target::ResourceJournal { .. }))
            .count()
            != self.history_ids.len()
        {
            return Err("资源历史迁移载荷不完整".into());
        }
        Ok(())
    }
    pub fn revision(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_vec(self)
            .map(|bytes| hash(&bytes))
            .map_err(|e| e.to_string())
    }
    pub fn matches_scope(&self, root: &Path, old: &str, new: &str) -> Result<(), String> {
        self.validate()?;
        if Path::new(&self.root_path) != root || self.old_id != old || self.new_id != new {
            return Err("重命名引用与文件事务范围不符".into());
        }
        Ok(())
    }
    pub fn verify_binding(
        &self,
        project: &Path,
        root: &Path,
        old: &str,
        new: &str,
    ) -> Result<(), String> {
        self.matches_scope(root, old, new)?;
        if Dir::open(project)?.identity()? != self.project_identity
            || Dir::open(root)?.identity()? != self.root_identity
        {
            return Err("重命名引用与项目或游戏目录范围不符".into());
        }
        Ok(())
    }
    fn scope(&self, project: &Path, root: &Path) -> Result<(), String> {
        self.validate()?;
        if Path::new(&self.root_path) != root
            || Dir::open(project)?.identity()? != self.project_identity
            || Dir::open(root)?.identity()? != self.root_identity
        {
            return Err("重命名引用的项目或游戏目录已改变，原文件已保留".into());
        }
        let history = history_directory(root)?;
        if history.as_ref().map(Dir::identity).transpose()? != self.history_directory
            || history
                .map(|dir| dir.names())
                .transpose()?
                .unwrap_or_default()
                != self.history_ids
        {
            return Err("资源历史目录或记录列表已变化，重命名仍等待恢复".into());
        }
        Ok(())
    }
    fn check_changes(&self, project: &Path, root: &Path, allow_after: bool) -> Result<(), String> {
        for change in &self.changes {
            let (dir, name) = self.location(project, root, &change.target)?;
            if Some(dir.identity()?) != change.snapshot.directory {
                return Err("引用资料目录已被外部替换，原文件已保留".into());
            }
            let current = dir.snapshot(&name, Self::limit(&change.target))?.0;
            if current == change.snapshot {
                continue;
            }
            if allow_after
                && current.file.as_ref().map(|token| &token.hash) == change.after_hash.as_ref()
            {
                continue;
            }
            return Err("引用文件已被外部修改，未覆盖原文件；重命名仍等待恢复".into());
        }
        Ok(())
    }
    pub fn verify_before(&self, project: &Path, root: &Path) -> Result<(), String> {
        let _locks = self.locks(project, root)?;
        self.scope(project, root)?;
        self.check_changes(project, root, false)
    }
    /// Finish a possibly partial replay. Validate every target before touching
    /// the first; each publication also rechecks its source and staged bytes.
    pub fn apply(&self, project: &Path, root: &Path) -> Result<(), String> {
        let _locks = self.locks(project, root)?;
        self.scope(project, root)?;
        self.check_changes(project, root, true)?;
        // Create the new preset before deleting its source, so either side of
        // an interruption always retains a validated copy.
        let mut changes: Vec<_> = self.changes.iter().collect();
        changes.sort_by_key(|change| match change.target {
            Target::PresetNew => 0,
            Target::PresetOld => 2,
            _ => 1,
        });
        for change in changes {
            let (dir, name) = self.location(project, root, &change.target)?;
            let current = dir.snapshot(&name, Self::limit(&change.target))?.0;
            if current.file.as_ref().map(|token| &token.hash) == change.after_hash.as_ref() {
                continue;
            }
            if current != change.snapshot {
                return Err("引用文件已被外部修改，重命名仍等待恢复".into());
            }
            let after = decoded(&change.after, Self::limit(&change.target))?;
            dir.update_checked(
                &name,
                &current,
                after.as_deref(),
                Self::limit(&change.target),
            )?;
        }
        self.check_changes(project, root, true)
    }
    fn marker(&self, operation_id: &str, file_identity: Identity) -> Result<Pending, String> {
        if !valid_rename_operation(operation_id) {
            return Err("重命名事务标识无效".into());
        }
        Ok(Pending {
            schema_version: wire_version(),
            root_path: self.root_path.clone(),
            root_id: self.root_id.clone(),
            root_identity: self.root_identity.clone(),
            project_identity: self.project_identity.clone(),
            file_identity,
            old_id: self.old_id.clone(),
            new_id: self.new_id.clone(),
            operation_id: operation_id.into(),
            revision: self.revision()?,
        })
    }
    pub fn mark_pending(
        &self,
        project: &Path,
        root: &Path,
        operation_id: &str,
    ) -> Result<(), String> {
        let _locks = self.locks(project, root)?;
        self.scope(project, root)?;
        let dir = app_directory(project, true)?.ok_or("引用目录不可用")?;
        let _lock = dir.lock(".instance-rename.lock")?;
        if let Some((token, current)) = dir.read(MARKER, PRESET_LIMIT)? {
            let marker = serde_json::to_vec(&self.marker(operation_id, token.stamp.identity)?)
                .map_err(|e| e.to_string())?;
            if current == marker {
                self.check_changes(project, root, true)?;
                return Ok(());
            }
            return Err("存在其他或已被外部修改的重命名标记，原文件已保留".into());
        }
        self.check_changes(project, root, false)?;
        dir.publish_marker(|identity| {
            serde_json::to_vec(&self.marker(operation_id, identity)?).map_err(|e| e.to_string())
        })
    }

    pub fn clear_pending(
        &self,
        project: &Path,
        root: &Path,
        operation_id: &str,
    ) -> Result<(), String> {
        self.scope(project, root)?;
        let dir = app_directory(project, false)?.ok_or("引用目录不可用")?;
        let _lock = dir.lock(".instance-rename.lock")?;
        let Some((token, current)) = dir.read(MARKER, PRESET_LIMIT)? else {
            return Ok(());
        };
        let expected =
            serde_json::to_vec(&self.marker(operation_id, token.stamp.identity.clone())?)
                .map_err(|e| e.to_string())?;
        if current != expected {
            return Err("重命名标记已被外部修改，原文件已保留".into());
        }
        dir.clear_marker(&token, &current)
    }
}

pub fn prepare(
    project: &Path,
    root_id: &str,
    root: &Path,
    old: &str,
    new: &str,
) -> Result<RenameReferences, String> {
    ensure_project_ready(project)?;
    id(old)?;
    id(new)?;
    let mut refs = RenameReferences {
        schema_version: wire_version(),
        root_id: root_id.into(),
        root_path: root.to_str().ok_or("游戏目录不是 UTF-8")?.into(),
        old_id: old.into(),
        new_id: new.into(),
        project_identity: Dir::open(project)?.identity()?,
        root_identity: Dir::open(root)?.identity()?,
        history_directory: None,
        history_ids: vec![],
        changes: vec![],
    };
    let _locks = refs.locks(project, root)?;
    ensure_project_ready(project)?;
    for (target, name, transform) in [
        (
            Target::Settings,
            "settings.json",
            crate::config::rename_bytes
                as fn(&[u8], &str, &Path, &str, &str) -> Result<Vec<u8>, String>,
        ),
        (
            Target::Metadata,
            "instance-metadata.json",
            crate::instance_meta::rename_bytes,
        ),
    ] {
        let data = app_directory(project, false)?
            .ok_or("引用目录不可用")?
            .snapshot(name, RenameReferences::limit(&target))?
            .1
            .map(|bytes| transform(&bytes, root_id, root, old, new))
            .transpose()?;
        let data = if target == Target::Metadata && data.is_none() {
            Some(crate::instance_meta::rename_optional_bytes(
                None, root_id, root, old, new,
            )?)
        } else {
            data
        };
        refs.capture(project, root, target, data)?;
    }
    let presets = preset_directory(project, false)?.ok_or("导出配置目录不可用")?;
    if presets
        .snapshot(&crate::export_presets::name(root_id, new), PRESET_LIMIT)?
        .1
        .is_some()
    {
        return Err("目标名称已有导出配置，请选择其他名称".into());
    }
    let new_preset = presets
        .snapshot(&crate::export_presets::name(root_id, old), PRESET_LIMIT)?
        .1
        .map(|bytes| crate::export_presets::rename_bytes(&bytes, root_id, root, old, new))
        .transpose()?;
    refs.capture(project, root, Target::PresetOld, None)?;
    refs.capture(project, root, Target::PresetNew, new_preset)?;
    if let Some(history) = history_directory(root)? {
        refs.history_directory = Some(history.identity()?);
        refs.history_ids = history.names()?;
        for operation in refs.history_ids.clone() {
            if !crate::resource_ops::valid_operation_id(&operation) {
                return Err("资源操作目录含有未知记录，原文件已保留".into());
            }
            let data = history
                .child(&operation)?
                .snapshot("journal.json", RESOURCE_LIMIT)?
                .1
                .map(|bytes| {
                    crate::resource_ops::rename_journal_bytes(&bytes, &operation, old, new)
                        .map(|after| after.unwrap_or(bytes))
                })
                .transpose()?;
            refs.capture(
                project,
                root,
                Target::ResourceJournal {
                    operation_id: operation,
                },
                data,
            )?;
        }
    }
    refs.scope(project, root)?;
    refs.check_changes(project, root, false)?;
    Ok(refs)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema_version: u32,
    root_path: String,
    root_id: String,
    root_identity: Identity,
    project_identity: Identity,
    file_identity: Identity,
    old_id: String,
    new_id: String,
    operation_id: String,
    revision: String,
}
fn valid_rename_operation(value: &str) -> bool {
    value.len() <= 80
        && value.strip_prefix("n-").is_some_and(|tail| {
            let pieces: Vec<_> = tail.split('-').collect();
            pieces.len() == 3
                && pieces.iter().all(|piece| {
                    !piece.is_empty()
                        && piece
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                })
        })
}
pub fn pending_root(project: &Path) -> Result<Option<PathBuf>, String> {
    read_pending(project).map(|marker| marker.map(|marker| marker.root_path.into()))
}
pub fn pending_operation(project: &Path) -> Result<Option<String>, String> {
    read_pending(project).map(|marker| marker.map(|marker| marker.operation_id))
}
fn read_pending(project: &Path) -> Result<Option<Pending>, String> {
    let Some(dir) = app_directory(project, false)? else {
        return Ok(None);
    };
    let Some((token, bytes)) = dir.read(MARKER, PRESET_LIMIT)? else {
        return Ok(None);
    };
    let marker: Pending = serde_json::from_slice(&bytes)
        .map_err(|_| "重命名待恢复标记损坏或格式不受支持，原文件已保留".to_string())?;
    if marker.schema_version != wire_version()
        || !valid_rename_operation(&marker.operation_id)
        || marker.revision.len() != 64
        || !marker.revision.bytes().all(|c| c.is_ascii_hexdigit())
        || marker.project_identity != Dir::open(project)?.identity()?
        || marker.file_identity != token.stamp.identity
        || marker.root_id.is_empty()
        || marker.root_id.len() > 128
        || !marker
            .root_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        || marker.old_id == marker.new_id
    {
        return Err("重命名待恢复标记范围或版本无效，原文件已保留".into());
    }
    id(&marker.old_id)?;
    id(&marker.new_id)?;
    absolute(Path::new(&marker.root_path))?;
    Ok(Some(marker))
}
pub fn ensure_project_ready(project: &Path) -> Result<(), String> {
    if pending_root(project)?.is_some() {
        Err(PENDING_ERROR.into())
    } else {
        crate::instance_delete::ensure_project_ready(project)
    }
}

// This original suite exercises Linux physical files and Unix Java aliases.
// Portable replay tests below use native private fixtures and logical readers.
#[cfg(test)]
#[path = "instance_rename_refs/portable_tests.rs"]
mod portable_tests;
#[cfg(all(test, target_os = "linux"))]
#[path = "instance_rename_refs/tests.rs"]
mod tests;
