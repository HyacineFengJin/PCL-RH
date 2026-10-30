//! Launcher-owned UI policy, separate from the journal-sensitive game settings.
//!
//! `load` is read-only; defaults create no file. Snapshot/update/export use an
//! opaque process revision plus an inode/content snapshot. Invalid/future files
//! and external edits remain intact and block saves until an explicit reload.
//! The store owns `.pcl-rust/launcher-preferences.json` and unique settings-only
//! import backups beside it. Backup documents restore through the import parser.
//! Runtime effects, asset resolution, network activity and native window actions
//! belong to callers; importing a home URL never fetches or executes its contents.
#[path = "launcher_prefs/io.rs"]
mod io;
#[path = "launcher_prefs/model.rs"]
mod model;
#[path = "launcher_prefs/transfer.rs"]
mod transfer;

pub use model::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
    time::{SystemTime, UNIX_EPOCH},
};
pub use transfer::{export_to_file, preview_import, read_settings_file, LauncherSettingsExport};

const SCHEMA_VERSION: u32 = 1;
static NEXT_REVISION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Persisted {
    schema_version: u32,
    preferences: LauncherPreferences,
}
impl Default for Persisted {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            preferences: LauncherPreferences::default(),
        }
    }
}
impl Persisted {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("启动器设置无法读取：{error}"))?;
        if value.schema_version != SCHEMA_VERSION {
            return Err(format!("不支持启动器设置版本 {}", value.schema_version));
        }
        value.preferences.validate()?;
        Ok(value)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LauncherPreferencesView {
    pub preferences: LauncherPreferences,
    pub revision: String,
    pub warning: Option<String>,
    pub runtime_warning: Option<String>,
    pub hardware_acceleration_restart_required: bool,
    pub last_import_backup_path: Option<String>,
}

struct Stored {
    data: Persisted,
    disk: io::Snapshot,
    revision: String,
    blocked: Option<String>,
    last_import_backup_path: Option<String>,
}

pub struct LauncherPreferencesStore {
    project: Option<io::Directory>,
    project_path: PathBuf,
    renderer_disabled_at_start: bool,
    inner: Mutex<Stored>,
    #[cfg(test)]
    before_replace: Mutex<Option<Box<dyn FnOnce(&Path) -> Result<(), String> + Send>>>,
}

fn nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{nanos:x}-{:x}-{:x}",
        std::process::id(),
        NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
    )
}
fn revision() -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("pcl-launcher-preferences-v1:{}", nonce()).as_bytes())
    )
}
fn retained_warning(error: String) -> String {
    format!("{error}；已保留原文件，启动器偏好暂为只读，请修复后重新读取或重新打开启动器")
}

impl LauncherPreferencesStore {
    /// No directories, lock files, assets, settings or game files are created.
    pub fn load(project_path: &Path) -> Self {
        let mut data = Persisted::default();
        let mut disk = io::Snapshot::default();
        let mut blocked = None;
        let project = match io::Directory::open_preferences(project_path) {
            Ok(project) => Some(project),
            Err(error) => {
                blocked = Some(retained_warning(error));
                None
            }
        };
        if let Some(project) = &project {
            match io::snapshot(project) {
                Ok((snapshot, bytes)) => {
                    disk = snapshot;
                    if let Some(bytes) = bytes {
                        match Persisted::parse(&bytes) {
                            Ok(saved) => data = saved,
                            Err(error) => blocked = Some(retained_warning(error)),
                        }
                    }
                }
                Err(error) => blocked = Some(retained_warning(error)),
            }
        }
        Self {
            project,
            project_path: project_path.to_path_buf(),
            renderer_disabled_at_start: data.preferences.disable_hardware_acceleration,
            inner: Mutex::new(Stored {
                data,
                disk,
                revision: revision(),
                blocked,
                last_import_backup_path: None,
            }),
            #[cfg(test)]
            before_replace: Mutex::new(None),
        }
    }
    fn lock(&self) -> MutexGuard<'_, Stored> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn view(&self, state: &Stored) -> LauncherPreferencesView {
        LauncherPreferencesView {
            preferences: state.data.preferences.clone(),
            revision: state.revision.clone(),
            warning: state.blocked.clone(),
            runtime_warning: None,
            hardware_acceleration_restart_required: state
                .data
                .preferences
                .disable_hardware_acceleration
                != self.renderer_disabled_at_start,
            last_import_backup_path: state.last_import_backup_path.clone(),
        }
    }
    fn verify_disk(&self, state: &mut Stored) -> Result<(), String> {
        if let Some(error) = &state.blocked {
            return Err(error.clone());
        }
        let project = self.project.as_ref().ok_or("启动器设置项目目录不可用")?;
        if let Err(error) = io::verify(project, &self.project_path, &state.disk) {
            let warning = retained_warning(error);
            state.blocked = Some(warning.clone());
            return Err(warning);
        }
        Ok(())
    }
    fn require_revision(&self, state: &mut Stored, expected: &str) -> Result<(), String> {
        if expected != state.revision {
            return Err("启动器偏好已变化，请重新读取后重试".into());
        }
        self.verify_disk(state)
    }
    /// Reports external edits as a warning instead of calling cached values a
    /// successful fresh read. It never adopts unchecked outside data.
    pub fn snapshot(&self) -> LauncherPreferencesView {
        let mut state = self.lock();
        let _ = self.verify_disk(&mut state);
        self.view(&state)
    }
    /// Cheap generation guard for an already captured worker. Disk identity is
    /// checked separately at publication boundaries, never once per HTTP chunk.
    pub(crate) fn matches_revision(&self, expected: &str) -> bool {
        let state = self.lock();
        state.blocked.is_none() && state.revision == expected
    }
    /// Explicitly adopts a valid on-disk edit. A failed reload keeps the last
    /// successful data, exposes its warning and continues to refuse writes.
    pub fn reload(&self) -> LauncherPreferencesView {
        let mut state = self.lock();
        let result = (|| {
            let project = self.project.as_ref().ok_or("启动器设置项目目录不可用")?;
            if io::Directory::open_preferences(&self.project_path)?.identity()?
                != project.identity()?
            {
                return Err("启动器项目目录已被替换，请重新打开启动器".into());
            }
            let (disk, bytes) = io::snapshot(project)?;
            let data = match bytes {
                Some(bytes) => Persisted::parse(&bytes)?,
                None => Persisted::default(),
            };
            Ok::<_, String>((disk, data))
        })();
        match result {
            Ok((disk, data)) => {
                state.disk = disk;
                state.data = data;
                state.blocked = None;
                state.revision = revision();
            }
            Err(error) => state.blocked = Some(retained_warning(error)),
        }
        self.view(&state)
    }
    pub fn update(
        &self,
        expected_revision: &str,
        patch: LauncherPreferencesPatch,
    ) -> Result<LauncherPreferencesView, String> {
        let mut state = self.lock();
        self.require_revision(&mut state, expected_revision)?;
        let mut preferences = state.data.preferences.clone();
        patch.apply(&mut preferences);
        self.save(&mut state, preferences, false)
    }
    fn save(
        &self,
        state: &mut Stored,
        preferences: LauncherPreferences,
        backup_before_import: bool,
    ) -> Result<LauncherPreferencesView, String> {
        preferences.validate()?;
        if !backup_before_import && preferences == state.data.preferences {
            return Ok(self.view(state));
        }
        let data = Persisted {
            schema_version: SCHEMA_VERSION,
            preferences,
        };
        let bytes = serde_json::to_vec_pretty(&data).map_err(|error| error.to_string())?;
        if bytes.len() > io::MAX_BYTES {
            return Err("启动器设置文件超过 64 KiB 上限".into());
        }
        let project = self.project.as_ref().ok_or("启动器设置项目目录不可用")?;
        // Lock order is the store mutex, then a nonblocking file lock. No other
        // launcher store is acquired; rename's v3 settings bytes are independent.
        let dir = io::prepare_directory(project, &mut state.disk)?;
        let _write_lock = dir.write_lock()?;
        io::verify(project, &self.project_path, &state.disk)?;
        if backup_before_import && state.disk.has_file() {
            let previous = LauncherSettingsExport::from_preferences(&state.data.preferences);
            let backup = serde_json::to_vec_pretty(&previous).map_err(|error| error.to_string())?;
            let name = format!("launcher-preferences.backup-{}.json", nonce());
            let storage_path = self.project_path.join(".pcl-rust");
            dir.publish_anonymous(&storage_path, &name, &backup)?;
            state.last_import_backup_path =
                Some(storage_path.join(name).to_string_lossy().into_owned());
            io::verify(project, &self.project_path, &state.disk)?;
        }
        if data.preferences == state.data.preferences {
            return Ok(self.view(state));
        }
        let temporary = format!(".launcher-preferences-{}.tmp", nonce());
        let disk = io::persist(
            project,
            &self.project_path,
            &dir,
            &state.disk,
            &bytes,
            &temporary,
            || self.run_before_replace(),
        );
        match disk {
            Ok(disk) => {
                state.disk = disk;
                state.data = data;
                state.revision = revision();
                Ok(self.view(state))
            }
            Err(error) => {
                // A failure after publication or an outside edit must block the
                // next write. A pre-publication disk failure remains retryable.
                let _ = self.verify_disk(state);
                Err(error)
            }
        }
    }
    pub fn export_settings(&self, expected_revision: &str) -> Result<Vec<u8>, String> {
        let mut state = self.lock();
        self.require_revision(&mut state, expected_revision)?;
        state.data.preferences.validate()?;
        let document = LauncherSettingsExport::from_preferences(&state.data.preferences);
        let bytes = serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?;
        if bytes.len() > io::MAX_BYTES {
            return Err("设置备份超过 64 KiB 上限".into());
        }
        Ok(bytes)
    }
    /// Byte validation happens before any storage mutation. Native callers must
    /// also bound file reads to MAX_IMPORT_BYTES before allocating their buffer.
    pub fn import_settings(
        &self,
        expected_revision: &str,
        bytes: &[u8],
    ) -> Result<LauncherPreferencesView, String> {
        let preferences = transfer::parse_import(bytes)?;
        let mut state = self.lock();
        self.require_revision(&mut state, expected_revision)?;
        self.save(&mut state, preferences, true)
    }
    #[cfg(test)]
    pub fn reset(&self, expected_revision: &str) -> Result<LauncherPreferencesView, String> {
        let mut state = self.lock();
        self.require_revision(&mut state, expected_revision)?;
        self.save(&mut state, LauncherPreferences::default(), true)
    }
    fn run_before_replace(&self) -> Result<(), String> {
        #[cfg(test)]
        if let Some(hook) = self.before_replace.lock().unwrap().take() {
            return hook(&self.project_path);
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn test_before_replace(
        &self,
        hook: impl FnOnce(&Path) -> Result<(), String> + Send + 'static,
    ) {
        *self.before_replace.lock().unwrap() = Some(Box::new(hook));
    }
    #[cfg(all(test, unix))]
    pub fn test_with_write_lock<R>(
        &self,
        action: impl FnOnce(std::os::fd::RawFd) -> R,
    ) -> Result<R, String> {
        let project = self.project.as_ref().ok_or("fixture project missing")?;
        let dir = project.ensure_storage()?;
        let lock = dir.write_lock()?;
        Ok(action(lock.raw_fd()))
    }
}

pub const MAX_IMPORT_BYTES: usize = io::MAX_BYTES;

#[cfg(test)]
#[path = "launcher_prefs/tests.rs"]
mod tests;
