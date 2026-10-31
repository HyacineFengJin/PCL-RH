//! Fixed user XDG entries, with exact project ownership and persistent undo.
//! The existing install-desktop.sh template is recognized byte-for-byte; edited
//! or foreign entries are retained. Withdrawal journals intent before a rename
//! into private trash on the same filesystem, so no package/game data is touched.
use super::filesystem::{optional_snapshot, Dir, DirectoryIdentity, Scope, Snapshot, Stamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
const NAME: &str = "pcl-linux-experimental.desktop";
const TRASH: &str = ".pcl-linux-entry-trash";
const LIMIT: u64 = 16 * 1024;
const CURRENT_NAMES: &str = "Name=PCL RH\nName[zh_CN]=PCL RH\n";
const FORMER_NAMES: &str = "Name=PCL Linux (实验版)\nName[zh_CN]=PCL Linux（实验版）\n";
const CURRENT_COMMENTS: &str = "Comment=Minecraft launcher\nComment[zh_CN]=Minecraft 启动器\n";
const FORMER_COMMENTS: &str = "Comment=Launch existing Minecraft versions on Linux\nComment[zh_CN]=在 Linux 上启动已有的 Minecraft 版本\n";
static NEXT: AtomicU64 = AtomicU64::new(0);
pub const PACKAGE_UNINSTALL_UNSUPPORTED:&str="软件包安装的启动器请使用原包管理器（如 paru）移除。本地入口服务仅管理本项目的应用入口与桌面快捷方式，游戏和账号数据会保留。";

#[derive(Clone, Debug)]
pub struct XdgPaths {
    pub applications: PathBuf,
    pub desktop: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ShortcutTarget {
    Applications,
    Desktop,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutEntry {
    pub path: String,
    pub already_registered: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutPlan {
    pub revision: String,
    pub entries: Vec<ShortcutEntry>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutOutcome {
    pub paths: Vec<String>,
    pub operation_ids: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutRecovery {
    pub operation_id: String,
    pub revision: String,
    pub path: String,
    pub created_at: u64,
    pub warnings: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    project: String,
    target: ShortcutTarget,
    operation_id: String,
    created_at: u64,
    parent: DirectoryIdentity,
    stamp: Stamp,
    digest: String,
}

/// Recovery checks and the subsequent rename share these exact FDs. Reopening
/// paths between revision validation and mutation would lose the approved state.
struct RecoveryState {
    parent: Dir,
    trash: Dir,
    operation: Dir,
    journal: Journal,
    journal_snapshot: Snapshot,
    archive: Option<Snapshot>,
    original: Option<Snapshot>,
    unknown: Vec<String>,
}
impl RecoveryState {
    fn recheck_directories(&self, path: &Path, id: &str) -> Result<(), String> {
        let parent = Dir::absolute(path)?;
        let trash = parent.child(TRASH)?;
        let operation = trash.child(id)?;
        if parent.identity()? != self.parent.identity()?
            || trash.identity()? != self.trash.identity()?
            || operation.identity()? != self.operation.identity()?
        {
            return Err("入口恢复目录发生变化，已保留文件".into());
        }
        Ok(())
    }
}

pub struct ShortcutStore {
    project: PathBuf,
    paths: XdgPaths,
    writer: Mutex<()>,
}
impl ShortcutStore {
    pub fn new(project: PathBuf, paths: XdgPaths) -> Self {
        Self {
            project,
            paths,
            writer: Mutex::new(()),
        }
    }
    fn path(&self, target: ShortcutTarget) -> Result<&Path, String> {
        match target {
            ShortcutTarget::Applications => Ok(&self.paths.applications),
            ShortcutTarget::Desktop => self
                .paths
                .desktop
                .as_deref()
                .ok_or_else(|| "系统没有配置桌面目录，请先设置 XDG_DESKTOP_DIR".to_string()),
        }
    }
    fn targets(&self) -> Vec<ShortcutTarget> {
        let mut targets = vec![ShortcutTarget::Applications];
        if self.paths.desktop.is_some() {
            targets.push(ShortcutTarget::Desktop);
        }
        targets
    }
    fn project_key(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(self.project.as_os_str().as_encoded_bytes())
        )
    }
    fn contents(&self) -> Result<Vec<u8>, String> {
        self.registered_contents(CURRENT_NAMES, CURRENT_COMMENTS, "pcl-rh.png")
    }
    fn registered_contents(
        &self,
        names: &str,
        comments: &str,
        icon: &str,
    ) -> Result<Vec<u8>, String> {
        let project = self.project.to_str().ok_or("项目路径必须是 UTF-8")?;
        let exec = desktop_exec(&self.project.join("start-native.sh"))?;
        Ok(format!("[Desktop Entry]\nType=Application\nVersion=1.0\n{names}{comments}Exec={exec}\nPath={}\nIcon={}\nTerminal=false\nCategories=Game;\nKeywords=Minecraft;PCL;Forge;\nStartupNotify=true\nStartupWMClass=pcl-desktop\nX-PCL-Linux-Owner=launcher-local-v1\nX-PCL-Linux-Project={}\n",desktop_value(project)?,desktop_value(&self.project.join("assets").join(icon).display().to_string())?,self.project_key()).into_bytes())
    }
    fn legacy(&self) -> Result<Vec<u8>, String> {
        let project = self.project.to_str().ok_or("项目路径必须是 UTF-8")?;
        if project.chars().any(char::is_control) {
            return Err("项目路径含有控制字符".into());
        }
        Ok(format!("[Desktop Entry]\nType=Application\nVersion=1.0\nName=PCL Linux (实验版)\nName[zh_CN]=PCL Linux（实验版）\nComment=Launch existing Minecraft versions on Linux\nComment[zh_CN]=在 Linux 上启动已有的 Minecraft 版本\nExec=\"{project}/start-native.sh\"\nPath={project}\nIcon={project}/assets/pcl-linux.png\nTerminal=false\nCategories=Game;\nKeywords=Minecraft;PCL;Forge;\nStartupNotify=true\nStartupWMClass=pcl-desktop\n").into_bytes())
    }
    fn owned(&self, snapshot: &Snapshot) -> Result<bool, String> {
        // Legacy ownership remains recognizable even when its path cannot be
        // encoded into a new Exec token (for example '='). Withdrawal may still
        // preserve that exact registered file for undo.
        if self.legacy_owned(snapshot)? || snapshot.header == self.contents()? {
            return Ok(true);
        }
        // Keep the exact former icon/comment bytes with each historical name.
        // A brand update grants no ownership over arbitrary edited variants.
        for names in [CURRENT_NAMES, FORMER_NAMES] {
            if snapshot.header
                == self.registered_contents(names, FORMER_COMMENTS, "pcl-linux.png")?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn legacy_owned(&self, snapshot: &Snapshot) -> Result<bool, String> {
        let former = self.legacy()?;
        let current = String::from_utf8(former.clone())
            .map_err(|_| "应用入口文本无效")?
            .replace(FORMER_NAMES, CURRENT_NAMES);
        Ok(snapshot.header == former || snapshot.header == current.as_bytes())
    }
    fn snapshot(&self, target: ShortcutTarget) -> Result<Option<(Dir, Snapshot)>, String> {
        let Some(dir) = optional_dir(self.path(target)?)? else {
            return Ok(None);
        };
        let snapshot = optional_snapshot(&dir, NAME, LIMIT)?;
        Ok(snapshot.map(|snapshot| (dir, snapshot)))
    }
    fn plan(&self, targets: &[ShortcutTarget], create: bool) -> Result<ShortcutPlan, String> {
        let mut entries = Vec::new();
        let mut state = Vec::new();
        let mut warnings = Vec::new();
        for target in targets {
            let path = self.path(*target)?.join(NAME);
            let snapshot = self.snapshot(*target)?;
            if let Some((dir, snapshot)) = &snapshot {
                if !self.owned(snapshot)? {
                    if create {
                        return Err("同名应用入口属于其它项目或已被修改，已保留原文件".into());
                    }
                    warnings.push(format!(
                        "同名入口属于其它项目或已被修改，已保留：{}",
                        path.display()
                    ));
                } else {
                    if create
                        && self.legacy_owned(snapshot)?
                        && self.project.to_str().is_some_and(|path| {
                            path.contains(['%', '"', '`', '$', '\\', '=']) || path.ends_with(' ')
                        })
                    {
                        return Err("旧应用入口路径需要重新转义，请先移除本项目入口再创建；原文件会保留以便恢复".into());
                    }
                    entries.push(ShortcutEntry {
                        path: path.display().to_string(),
                        already_registered: true,
                    });
                }
                state.push((
                    target,
                    path,
                    Some((
                        dir.identity()?,
                        snapshot.stamp.clone(),
                        snapshot.digest.clone(),
                    )),
                ));
            } else {
                if create {
                    entries.push(ShortcutEntry {
                        path: path.display().to_string(),
                        already_registered: false,
                    });
                }
                state.push((target, path, None));
            }
        }
        let revision = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(self.project_key(), create, state))
                    .map_err(|_| "应用入口状态无法序列化")?
            )
        );
        Ok(ShortcutPlan {
            revision,
            entries,
            warnings,
        })
    }
    pub fn prepare_create(&self, target: ShortcutTarget) -> Result<ShortcutPlan, String> {
        require_xdg_backend()?;
        self.contents()?;
        self.plan(&[target], true)
    }
    pub fn create(
        &self,
        target: ShortcutTarget,
        expected_revision: &str,
    ) -> Result<ShortcutOutcome, String> {
        let _writer = self.writer.lock().map_err(|_| "本地入口状态暂时不可用")?;
        let plan = self.prepare_create(target)?;
        if plan.revision != expected_revision {
            return Err("应用入口状态已变化，请重新确认创建".into());
        }
        validate_launcher(&self.project)?;
        let scope = Scope::create(&self.project, "launcher-local")?;
        let _lock = scope.folder.as_ref().unwrap().lock(".shortcuts.lock")?;
        if self.prepare_create(target)?.revision != expected_revision {
            return Err("应用入口状态已变化，请重新确认创建".into());
        }
        let path = self.path(target)?;
        let parent = Dir::create_absolute(path)?;
        if let Some(snapshot) = optional_snapshot(&parent, NAME, LIMIT)? {
            if !self.owned(&snapshot)? {
                return Err("同名应用入口已被替换，已保留原文件".into());
            }
        } else {
            parent.write_new_mode(
                NAME,
                &self.contents()?,
                if target == ShortcutTarget::Desktop {
                    0o700
                } else {
                    0o600
                },
            )?;
        }
        if Dir::absolute(path)?.identity() != parent.identity() {
            return Err("应用入口已保存，但目标目录发生变化，请检查实际目录".into());
        }
        scope.recheck()?;
        Ok(ShortcutOutcome {
            paths: vec![path.join(NAME).display().to_string()],
            operation_ids: Vec::new(),
        })
    }
    pub fn prepare_withdraw(&self) -> Result<ShortcutPlan, String> {
        require_xdg_backend()?;
        self.plan(&self.targets(), false)
    }
    pub fn withdraw(&self, expected_revision: &str) -> Result<ShortcutOutcome, String> {
        let _writer = self.writer.lock().map_err(|_| "本地入口状态暂时不可用")?;
        let plan = self.prepare_withdraw()?;
        if plan.revision != expected_revision {
            return Err("本项目入口已变化，请重新确认停止使用".into());
        }
        if plan.entries.is_empty() {
            return Err("本项目没有可移除的已注册入口；其它入口会保留".into());
        }
        let scope = Scope::create(&self.project, "launcher-local")?;
        let _lock = scope.folder.as_ref().unwrap().lock(".shortcuts.lock")?;
        if self.prepare_withdraw()?.revision != expected_revision {
            return Err("本项目入口已变化，请重新确认停止使用".into());
        }
        let mut captured = Vec::new();
        for target in self.targets() {
            if plan.entries.iter().any(|entry| {
                entry.path == self.path(target).unwrap().join(NAME).display().to_string()
            }) {
                let (parent, snapshot) =
                    self.snapshot(target)?.ok_or("已确认的入口在移除前消失")?;
                if !self.owned(&snapshot)? {
                    return Err("已确认的入口被修改，已保留原文件".into());
                }
                captured.push((target, parent, snapshot));
            }
        }
        let mut moved = Vec::new();
        let result = (|| {
            for (target, parent, snapshot) in captured {
                let trash = parent.create(TRASH)?;
                private(&trash)?;
                let id = format!(
                    "entry-{}-{}-{}",
                    now(),
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                );
                let operation = trash.create(&id)?;
                private(&operation)?;
                let journal = Journal {
                    schema_version: 1,
                    project: self.project_key(),
                    target,
                    operation_id: id.clone(),
                    created_at: now(),
                    parent: parent.identity()?,
                    stamp: snapshot.stamp.clone(),
                    digest: snapshot.digest.clone(),
                };
                operation.write_new(
                    "journal.json",
                    &serde_json::to_vec(&journal).map_err(|_| "应用入口恢复清单无法序列化")?,
                )?;
                snapshot.recheck(&parent, NAME)?;
                if Dir::absolute(self.path(target)?)?.identity() != parent.identity() {
                    return Err("应用入口目录在移除前发生变化，已保留文件".into());
                }
                // Durability failure can occur after rename succeeds. Register
                // intent before calling move so rollback checks both locations.
                moved.push((target, id.clone()));
                parent.move_new(NAME, &operation)?;
                let archived = Snapshot::open(&operation, NAME, LIMIT)?;
                if !journal.stamp.moved_matches(&archived.stamp)
                    || journal.digest != archived.digest
                {
                    return Err("应用入口移动期间发生变化，已保留恢复副本".into());
                }
                if Dir::absolute(self.path(target)?)?.identity() != parent.identity() {
                    return Err("应用入口目录发生变化，已保留恢复副本".into());
                }
            }
            scope.recheck()
        })();
        if let Err(error) = result {
            let mut failures = Vec::new();
            for (target, id) in &moved {
                if let Err(error) = self.restore_one(*target, id, None) {
                    failures.push(error);
                }
            }
            return Err(if failures.is_empty() {
                format!("{error}；已恢复被移动的入口")
            } else {
                format!("{error}；部分入口保留在恢复目录：{}", failures.join("；"))
            });
        }
        Ok(ShortcutOutcome {
            paths: plan.entries.into_iter().map(|entry| entry.path).collect(),
            operation_ids: moved
                .into_iter()
                .map(|(target, id)| format!("{}:{id}", target_key(target)))
                .collect(),
        })
    }
    pub fn recovery(&self) -> Result<Vec<ShortcutRecovery>, String> {
        require_xdg_backend()?;
        let mut rows = Vec::new();
        for target in self.targets() {
            let Some(parent) = optional_dir(self.path(target)?)? else {
                continue;
            };
            let Some(trash) = parent.optional(TRASH)? else {
                continue;
            };
            private(&trash)?;
            for id in trash.names()? {
                let result = self.recovery_one(target, &id);
                match result {
                    Ok(Some(row)) => rows.push(row),
                    Ok(None) => {}
                    Err(error) => rows.push(ShortcutRecovery {
                        operation_id: format!("{}:{id}", target_key(target)),
                        revision: String::new(),
                        path: self.path(target)?.join(NAME).display().to_string(),
                        created_at: 0,
                        warnings: vec![error],
                    }),
                }
            }
        }
        rows.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.operation_id.cmp(&a.operation_id))
        });
        Ok(rows)
    }
    pub fn restore(
        &self,
        operation_id: &str,
        expected_revision: &str,
    ) -> Result<ShortcutOutcome, String> {
        require_xdg_backend()?;
        let (target, id) = operation_id.split_once(':').ok_or("无效入口恢复标识")?;
        let target = match target {
            "applications" => ShortcutTarget::Applications,
            "desktop" => ShortcutTarget::Desktop,
            _ => return Err("无效入口恢复目标".into()),
        };
        super::filesystem::component(id)?;
        let _writer = self.writer.lock().map_err(|_| "本地入口状态暂时不可用")?;
        let scope = Scope::create(&self.project, "launcher-local")?;
        let _lock = scope.folder.as_ref().unwrap().lock(".shortcuts.lock")?;
        self.restore_one(target, id, Some(expected_revision))?;
        scope.recheck()?;
        Ok(ShortcutOutcome {
            paths: vec![self.path(target)?.join(NAME).display().to_string()],
            operation_ids: vec![operation_id.to_owned()],
        })
    }
    fn recovery_one(
        &self,
        target: ShortcutTarget,
        id: &str,
    ) -> Result<Option<ShortcutRecovery>, String> {
        let state = self.capture_recovery(target, id)?;
        self.describe_recovery(target, id, &state)
    }
    fn capture_recovery(&self, target: ShortcutTarget, id: &str) -> Result<RecoveryState, String> {
        super::filesystem::component(id)?;
        let parent = Dir::absolute(self.path(target)?)?;
        let trash = parent.child(TRASH)?;
        private(&trash)?;
        let operation = trash.child(id)?;
        private(&operation)?;
        let (journal, journal_snapshot) = read_journal(&operation)?;
        if journal.schema_version != 1
            || journal.project != self.project_key()
            || journal.target != target
            || journal.operation_id != id
            || journal.parent != parent.identity()?
        {
            return Err("入口恢复记录与本项目或原目录不匹配，已保留文件".into());
        }
        let archive = optional_snapshot(&operation, NAME, LIMIT)?;
        // A completed restore no longer owns the original filename. Its later
        // replacement, edit or symlink must not revive this old operation.
        let original = if archive.is_some() {
            optional_snapshot(&parent, NAME, LIMIT)?
        } else {
            None
        };
        let unknown = operation
            .names()?
            .into_iter()
            .filter(|name| name != NAME && name != "journal.json")
            .collect();
        let state = RecoveryState {
            parent,
            trash,
            operation,
            journal,
            journal_snapshot,
            archive,
            original,
            unknown,
        };
        state.recheck_directories(self.path(target)?, id)?;
        state
            .journal_snapshot
            .recheck(&state.operation, "journal.json")?;
        Ok(state)
    }
    fn describe_recovery(
        &self,
        target: ShortcutTarget,
        id: &str,
        state: &RecoveryState,
    ) -> Result<Option<ShortcutRecovery>, String> {
        let RecoveryState {
            journal,
            journal_snapshot,
            archive,
            original,
            unknown,
            ..
        } = state;
        let mut warnings = unknown
            .iter()
            .map(|name| format!("恢复目录包含未知文件，已保留：{name}"))
            .collect::<Vec<_>>();
        if let Some(archive) = &archive {
            if !journal.stamp.moved_matches(&archive.stamp)
                || journal.digest != archive.digest
                || !self.owned(archive)?
            {
                warnings.push("恢复副本被修改，已保留并拒绝自动恢复".into());
            }
            if original.is_some() {
                warnings.push("原位置已有入口，恢复不会覆盖".into());
            }
        } else if warnings.is_empty() {
            return Ok(None);
        }
        let token = |snapshot: &Snapshot| (snapshot.stamp.clone(), snapshot.digest.clone());
        let revision = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    self.project_key(),
                    target,
                    id,
                    &journal_snapshot.digest,
                    archive.as_ref().map(token),
                    original.as_ref().map(token),
                    &warnings
                ))
                .map_err(|_| "入口恢复状态无法序列化")?
            )
        );
        Ok(Some(ShortcutRecovery {
            operation_id: format!("{}:{id}", target_key(target)),
            revision,
            path: self.path(target)?.join(NAME).display().to_string(),
            created_at: journal.created_at,
            warnings,
        }))
    }
    fn restore_one(
        &self,
        target: ShortcutTarget,
        id: &str,
        expected_revision: Option<&str>,
    ) -> Result<(), String> {
        let state = self.capture_recovery(target, id)?;
        let row = match self.describe_recovery(target, id, &state)? {
            Some(row) => row,
            None if expected_revision.is_none() => {
                let original = Snapshot::open(&state.parent, NAME, LIMIT)?;
                if state.journal.stamp.moved_matches(&original.stamp)
                    && state.journal.digest == original.digest
                {
                    return Ok(());
                }
                return Err("入口原位置已变化，已保留恢复记录".into());
            }
            None => return Err("该入口已经恢复或没有可恢复副本".into()),
        };
        if expected_revision.is_some_and(|revision| revision != row.revision) {
            return Err("入口恢复副本或原位置已变化，请重新读取".into());
        }
        if !row.warnings.is_empty() {
            return Err(row.warnings.join("；"));
        }
        let archived = state.archive.as_ref().ok_or("入口恢复副本已消失")?;
        state.recheck_directories(self.path(target)?, id)?;
        state
            .journal_snapshot
            .recheck(&state.operation, "journal.json")?;
        archived.recheck(&state.operation, NAME)?;
        if state.operation.names()? != vec!["journal.json".to_string(), NAME.to_string()] {
            return Err("入口恢复目录在恢复前发生变化，已保留文件".into());
        }
        state.operation.move_new(NAME, &state.parent)?;
        let restored = Snapshot::open(&state.parent, NAME, LIMIT)?;
        if !archived.stamp.moved_matches(&restored.stamp) || archived.digest != restored.digest {
            return Err("入口恢复期间文件被修改，已保留原位置文件".into());
        }
        state.recheck_directories(self.path(target)?, id)?;
        Ok(())
    }
}

fn optional_dir(path: &Path) -> Result<Option<Dir>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Dir::absolute(path).map(Some),
        Ok(_) => Err("本地入口目标必须是真实目录，不能是符号链接".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("无法读取本地入口目标目录".into()),
    }
}
fn private(dir: &Dir) -> Result<(), String> {
    dir.require_private()
}
fn require_xdg_backend() -> Result<(), String> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err("此平台尚未接入原生应用入口；XDG 桌面入口只适用于 Linux".into())
    }
}
fn validate_launcher(project: &Path) -> Result<(), String> {
    require_xdg_backend()?;
    let dir = Dir::absolute(project)?;
    let file = dir
        .file("start-native.sh")?
        .ok_or("项目缺少启动脚本，不能创建应用入口")?;
    #[cfg(unix)]
    if file
        .metadata()
        .map_err(|_| "无法读取启动脚本")?
        .permissions()
        .mode()
        & 0o111
        == 0
    {
        return Err("项目启动脚本不可执行，不能创建应用入口".into());
    }
    #[cfg(not(unix))]
    drop(file);
    Ok(())
}
fn read_journal(operation: &Dir) -> Result<(Journal, Snapshot), String> {
    let snapshot = Snapshot::open(operation, "journal.json", LIMIT)?;
    let journal: Journal =
        serde_json::from_slice(&snapshot.header).map_err(|_| "入口恢复清单无效，已保留文件")?;
    Ok((journal, snapshot))
}
fn target_key(target: ShortcutTarget) -> &'static str {
    match target {
        ShortcutTarget::Applications => "applications",
        ShortcutTarget::Desktop => "desktop",
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn desktop_value(text: &str) -> Result<String, String> {
    if text.chars().any(char::is_control) {
        return Err("应用入口路径含有控制字符".into());
    }
    let start = text.len() - text.trim_start_matches(' ').len();
    let end = text.trim_end_matches(' ').len();
    let mut value = String::new();
    for (offset, ch) in text.char_indices() {
        if ch == '\\' {
            value.push_str("\\\\");
        } else if ch == ' ' && (offset < start || offset >= end) {
            value.push_str("\\s");
        } else {
            value.push(ch);
        }
    }
    Ok(value)
}
/// The desktop string parser runs before Exec quoting. Escape the command token
/// first, then its backslashes as a desktop value. Literal '%' always becomes
/// '%%'; no shell, field-code argument or environment expansion is introduced.
fn desktop_exec(path: &Path) -> Result<String, String> {
    let text = path.to_str().ok_or("启动脚本路径必须是 UTF-8")?;
    if text.contains('=') || text.chars().any(char::is_control) {
        return Err("启动脚本路径含有 desktop Exec 不支持的字符".into());
    }
    let mut quoted = String::from("\"");
    for c in text.chars() {
        match c {
            '%' => quoted.push_str("%%"),
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    desktop_value(&quoted)
}

#[cfg(all(test, target_os = "linux"))]
#[path = "shortcuts_tests.rs"]
mod tests;

#[cfg(test)]
mod native_tests {
    use super::*;
    #[test]
    fn journal_retains_native_parent_identity_and_legacy_unix_wire() {
        let parent = super::super::filesystem::fixture_path("shortcut-native-wire");
        std::fs::create_dir_all(&parent).unwrap();
        let parent = std::fs::canonicalize(parent).unwrap();
        let dir = Dir::absolute(&parent).unwrap();
        let identity = dir.identity().unwrap();
        let wire = serde_json::to_string(&identity).unwrap();
        #[cfg(unix)]
        assert_eq!(wire, format!("[{},{}]", identity.0, identity.1));
        #[cfg(windows)]
        {
            let value = serde_json::to_value(&identity).unwrap();
            assert_eq!(value["platform"], "windows");
            assert_eq!(value["file"].as_array().unwrap().len(), 16);
            assert!(serde_json::from_str::<DirectoryIdentity>("[42,7]").is_err());
        }
        assert_eq!(
            identity,
            serde_json::from_str::<DirectoryIdentity>(&wire).unwrap()
        );
    }
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unavailable_native_registration_never_creates_xdg_or_project_state() {
        let project = super::super::filesystem::fixture_path("shortcut-unavailable-project");
        let target = super::super::filesystem::fixture_path("shortcut-unavailable-target");
        assert!(!project.exists() && !target.exists());
        let store = ShortcutStore::new(
            project.clone(),
            XdgPaths {
                applications: target.clone(),
                desktop: None,
            },
        );
        assert!(store.prepare_create(ShortcutTarget::Applications).is_err());
        assert!(store
            .create(ShortcutTarget::Applications, "unapproved")
            .is_err());
        assert!(store.prepare_withdraw().is_err());
        assert!(store.recovery().is_err());
        assert!(store.restore("applications:any", "unapproved").is_err());
        assert!(!project.exists() && !target.exists());
    }
}
