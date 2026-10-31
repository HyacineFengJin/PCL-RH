//! Launcher-owned instance metadata. No game files or settings are read or
//! written here; callers resolve and authorize the captured game root.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
    sync::{Mutex, MutexGuard},
};

#[cfg(target_os = "linux")]
#[path = "instance_meta/linux.rs"]
mod filesystem;
#[cfg(not(target_os = "linux"))]
#[path = "instance_meta/native.rs"]
mod filesystem;
use filesystem::{Directory, FileStamp, Identity};
#[cfg(target_os = "linux")]
use std::{
    io::Write,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "instance-metadata.json";
const LOCK_NAME: &str = ".instance-metadata.lock";
const MAX_DESCRIPTION_CHARS: usize = 4096;
const MAX_ROOT_ID_BYTES: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_INSTANCE_ID_BYTES: usize = 255;
const MAX_RECORDS: usize = 4096;
// Both reading and writing apply this bound, including JSON escaping.
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
#[cfg(target_os = "linux")]
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Icon {
    #[default]
    Auto,
    Grass,
    Forge,
    Neoforge,
    Command,
    Steve,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    #[default]
    Auto,
    Vanilla,
    Forge,
    Neoforge,
    Fabric,
    Quilt,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub icon: Icon,
    #[serde(default)]
    pub category: Category,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataPatch {
    pub description: Option<String>,
    pub favorite: Option<bool>,
    pub icon: Option<Icon>,
    pub category: Option<Category>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MetaView {
    pub description: String,
    pub favorite: bool,
    pub icon: Icon,
    pub category: Category,
    pub revision: String,
}

impl MetaView {
    pub fn metadata(&self) -> Metadata {
        Metadata {
            description: self.description.clone(),
            favorite: self.favorite,
            icon: self.icon,
            category: self.category,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    root_id: String,
    root_path: String,
    instance_id: String,
    revision: u64,
    metadata: Metadata,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Persisted {
    schema_version: u32,
    entries: Vec<Record>,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileToken {
    stamp: FileStamp,
    digest: [u8; 32],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Snapshot {
    directory: Option<Identity>,
    file: Option<FileToken>,
}

fn disk_snapshot(project: &Directory) -> Result<(Snapshot, Option<Vec<u8>>), String> {
    let Some(directory) = project.storage()? else {
        return Ok((Snapshot::default(), None));
    };
    let directory_identity = directory.identity()?;
    let file = directory.read_file()?;
    // Detect replacement of the containing directory while the descriptor was
    // being read, as well as replacement of the metadata file itself.
    if project
        .storage()?
        .map(|directory| directory.identity())
        .transpose()?
        != Some(directory_identity.clone())
    {
        return Err("元数据目录在读取期间发生改变，请重新打开启动器".into());
    }
    let (file, bytes) = match file {
        Some((token, bytes)) => (Some(token), Some(bytes)),
        None => (None, None),
    };
    Ok((
        Snapshot {
            directory: Some(directory_identity),
            file,
        },
        bytes,
    ))
}

struct Stored {
    data: Persisted,
    disk: Snapshot,
    blocked: Option<String>,
    #[cfg(test)]
    fail_before_replace: bool,
    #[cfg(test)]
    before_write_lock: Option<std::sync::Arc<std::sync::Barrier>>,
}

pub struct MetadataStore {
    project: Option<Directory>,
    project_path: std::path::PathBuf,
    inner: Mutex<Stored>,
}

fn validate_scope(root_id: &str, root_path: &Path, id: &str) -> Result<(), String> {
    if root_id.is_empty()
        || root_id.len() > MAX_ROOT_ID_BYTES
        || !root_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("游戏目录标识无效或过长".into());
    }
    let text = root_path.to_str().ok_or("游戏目录路径不是有效的 UTF-8")?;
    if !root_path.is_absolute()
        || text.len() > MAX_PATH_BYTES
        || text.chars().any(char::is_control)
        || root_path.components().any(|component| {
            !matches!(component, Component::RootDir | Component::Normal(_))
                && !(cfg!(windows) && matches!(component, Component::Prefix(_)))
        })
    {
        return Err("实例元数据需要规范的绝对游戏目录路径".into());
    }
    #[cfg(windows)]
    pcl_core::retained_fs::validate_absolute_path(root_path)
        .map_err(|_| "实例元数据需要规范的绝对游戏目录路径".to_string())?;
    let normalized: std::path::PathBuf = root_path.components().collect();
    if normalized.as_os_str() != root_path.as_os_str() {
        return Err("实例元数据需要规范的绝对游戏目录路径".into());
    }
    if id.is_empty()
        || id.len() > MAX_INSTANCE_ID_BYTES
        || matches!(id, "." | "..")
        || id.contains(['/', '\\', ':'])
        || id.chars().any(char::is_control)
    {
        return Err("实例标识无效或过长".into());
    }
    Ok(())
}

fn validate_description(description: &str) -> Result<(), String> {
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err("实例描述最多允许 4096 个字符".into());
    }
    if description
        .chars()
        .any(|character| character.is_control() && character != '\n')
    {
        return Err("实例描述不能包含换行以外的控制字符".into());
    }
    if description.trim() != description {
        return Err("实例描述首尾不能含有空白字符".into());
    }
    Ok(())
}

fn validate_persisted(data: &Persisted) -> Result<(), String> {
    if data.schema_version != SCHEMA_VERSION {
        return Err(format!("不支持实例元数据版本 {}", data.schema_version));
    }
    if data.entries.len() > MAX_RECORDS {
        return Err("实例元数据超过 4096 条记录上限".into());
    }
    let mut scopes = BTreeSet::new();
    for record in &data.entries {
        validate_scope(
            &record.root_id,
            Path::new(&record.root_path),
            &record.instance_id,
        )?;
        validate_description(&record.metadata.description)?;
        if record.revision == 0 {
            return Err("实例元数据记录修订号无效".into());
        }
        if !scopes.insert((&record.root_id, &record.root_path, &record.instance_id)) {
            return Err("实例元数据含有重复实例记录".into());
        }
    }
    Ok(())
}

fn digest_part(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn view(
    root_id: &str,
    root_path: &Path,
    id: &str,
    metadata: &Metadata,
    generation: u64,
) -> MetaView {
    let mut digest = Sha256::new();
    digest.update(b"pcl-instance-metadata-revision-v1\0");
    digest_part(&mut digest, root_id.as_bytes());
    digest_part(&mut digest, root_path.as_os_str().as_encoded_bytes());
    digest_part(&mut digest, id.as_bytes());
    digest.update(generation.to_be_bytes());
    // Metadata also participates so an externally edited file cannot reuse an
    // older client token after an intentional reload without changing a number.
    digest_part(&mut digest, metadata.description.as_bytes());
    digest.update([
        metadata.favorite as u8,
        metadata.icon as u8,
        metadata.category as u8,
    ]);
    MetaView {
        description: metadata.description.clone(),
        favorite: metadata.favorite,
        icon: metadata.icon,
        category: metadata.category,
        revision: format!("{:x}", digest.finalize()),
    }
}

fn record_index(data: &Persisted, root_id: &str, root_path: &Path, id: &str) -> Option<usize> {
    let root_path = root_path.to_str()?;
    data.entries.iter().position(|record| {
        record.root_id == root_id && record.root_path == root_path && record.instance_id == id
    })
}

fn current_view(data: &Persisted, root_id: &str, root_path: &Path, id: &str) -> MetaView {
    match record_index(data, root_id, root_path, id) {
        Some(index) => {
            let record = &data.entries[index];
            view(root_id, root_path, id, &record.metadata, record.revision)
        }
        None => view(root_id, root_path, id, &Metadata::default(), 0),
    }
}

#[cfg(target_os = "linux")]
fn nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{:x}-{sequence:x}", std::process::id())
}

impl MetadataStore {
    /// Reads only project/.pcl-rust/instance-metadata.json. Invalid, unreadable,
    /// unknown, or future data is preserved and disables writes for this store.
    pub fn load(project: &Path) -> Self {
        let project_path = project.to_path_buf();
        let mut data = Persisted::default();
        let mut disk = Snapshot::default();
        let mut blocked = None;
        let project = match Directory::open(project) {
            Ok(project) => Some(project),
            Err(error) => {
                blocked = Some(error);
                None
            }
        };
        if let Some(project) = &project {
            match disk_snapshot(project) {
                Ok((snapshot, bytes)) => {
                    disk = snapshot;
                    if let Some(bytes) = bytes {
                        match serde_json::from_slice::<Persisted>(&bytes) {
                            Ok(saved) => match validate_persisted(&saved) {
                                Ok(()) => data = saved,
                                Err(error) => blocked = Some(error),
                            },
                            Err(error) => blocked = Some(format!("实例元数据无法读取：{error}")),
                        }
                    }
                }
                Err(error) => blocked = Some(error),
            }
        }
        if let Some(error) = &mut blocked {
            *error = format!("{error}；已保留原文件并使用只读默认资料，请修复后重新打开启动器");
        }
        Self {
            project,
            project_path,
            inner: Mutex::new(Stored {
                data,
                disk,
                blocked,
                #[cfg(test)]
                fail_before_replace: false,
                #[cfg(test)]
                before_write_lock: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Stored> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn warning(&self) -> Option<String> {
        self.lock().blocked.clone()
    }

    /// The caller passes the canonical path captured by its root resolver.
    /// This read does not require the game directory to be online.
    pub fn get(&self, root_id: &str, canonical_path: &Path, id: &str) -> MetaView {
        current_view(&self.lock().data, root_id, canonical_path, id)
    }

    fn check_disk(&self, current: &mut Stored) -> Result<(), String> {
        let project = self.project.as_ref().ok_or("元数据项目目录不可用")?;
        let result = disk_snapshot(project);
        match result {
            Ok((snapshot, _)) if snapshot == current.disk => Ok(()),
            // Settings/account services may legitimately create .pcl-rust
            // after this read-only store loaded. Adopt only a plain directory
            // whose metadata file is still absent; an existing directory's
            // identity can never be retargeted this way.
            Ok((snapshot, _))
                if current.disk.directory.is_none()
                    && current.disk.file.is_none()
                    && snapshot.file.is_none() =>
            {
                current.disk = snapshot;
                Ok(())
            }
            _ => {
                let error =
                    "实例元数据文件或目录已由外部修改；未覆盖原文件，请重新打开启动器".to_string();
                current.blocked = Some(error.clone());
                Err(error)
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn commit(&self, current: &mut Stored, next: Persisted) -> Result<(), String> {
        validate_persisted(&next)?;
        let mut bytes = serde_json::to_vec_pretty(&next).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        if bytes.len() > MAX_FILE_BYTES {
            return Err("实例元数据文件超过 8 MiB 上限".into());
        }
        self.check_disk(current)?;
        let project = self.project.as_ref().ok_or("元数据项目目录不可用")?;
        let directory = match project.storage()? {
            Some(directory) => directory,
            None => {
                let directory = project.create_storage()?;
                current.disk.directory = Some(directory.identity()?);
                directory
            }
        };
        if Some(directory.identity()?) != current.disk.directory {
            return Err("元数据目录已改变，请重新打开启动器".into());
        }
        // All stores/processes using this module share this lock. Recheck only
        // after admission so two clients cannot both pass the same old token
        // and then replace the entire file with incompatible patches.
        #[cfg(test)]
        if let Some(barrier) = &current.before_write_lock {
            barrier.wait();
        }
        let _lock = directory.write_lock()?;
        crate::instance_rename_refs::ensure_project_ready(&self.project_path()?)?;
        self.check_disk(current)?;
        let temporary = format!(".instance-metadata-{}.tmp", nonce());
        let result = (|| {
            let mut file = directory.temporary(&temporary)?;
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            let before = FileStamp::of(&file.metadata().map_err(|error| error.to_string())?);
            self.check_disk(current)?;
            #[cfg(test)]
            if current.fail_before_replace {
                return Err("测试：原子替换前保存失败".into());
            }
            directory.replace(&temporary)?;
            // Rename is the commit point. Every subsequent branch must keep
            // memory aligned with the committed data, even if directory fsync
            // or the post-rename metadata probe fails.
            let stamp = file
                .metadata()
                .map(|metadata| FileStamp::of(&metadata))
                .unwrap_or(before);
            current.disk.file = Some(FileToken {
                stamp,
                digest: Sha256::digest(&bytes).into(),
            });
            current.data = next;
            let _ = directory.0.sync_all();
            Ok(())
        })();
        if result.is_err() {
            directory.remove_temporary(&temporary);
        }
        result
    }

    #[cfg(not(target_os = "linux"))]
    fn commit(&self, current: &mut Stored, next: Persisted) -> Result<(), String> {
        validate_persisted(&next)?;
        let mut bytes = serde_json::to_vec_pretty(&next).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        if bytes.len() > MAX_FILE_BYTES {
            return Err("实例元数据文件超过 8 MiB 上限".into());
        }
        self.check_disk(current)?;
        let project = self.project.as_ref().ok_or("元数据项目目录不可用")?;
        let directory = match project.storage()? {
            Some(directory) => directory,
            None => {
                let directory = project.create_storage()?;
                current.disk.directory = Some(directory.identity()?);
                directory
            }
        };
        if Some(directory.identity()?) != current.disk.directory {
            return Err("元数据目录已改变，请重新打开启动器".into());
        }
        #[cfg(test)]
        if let Some(barrier) = &current.before_write_lock {
            barrier.wait();
        }
        let _lock = directory.write_lock()?;
        crate::instance_rename_refs::ensure_project_ready(&self.project_path()?)?;
        self.check_disk(current)?;
        #[cfg(test)]
        if current.fail_before_replace {
            return Err("测试：原子替换前保存失败".into());
        }
        let result = directory.replace(current.disk.file.as_ref(), &bytes);
        // A native commit can succeed before a durability/cleanup report fails.
        // Reconcile the logical document, never the Windows physical base, and
        // retain the actual committed data in memory even when returning Err.
        match disk_snapshot(project) {
            Ok((disk, Some(actual))) if actual == bytes => {
                current.disk = disk;
                current.data = next;
                result
            }
            Ok((disk, _)) if disk == current.disk && result.is_err() => result,
            _ => {
                let error = result.err().unwrap_or_else(|| {
                    "实例元数据提交后的状态无法确认；原件及证据已保留，请重新打开启动器".into()
                });
                current.blocked = Some(error.clone());
                Err(error)
            }
        }
    }

    pub fn patch(
        &self,
        root_id: &str,
        canonical_path: &Path,
        id: &str,
        expected_revision: &str,
        patch: MetadataPatch,
    ) -> Result<MetaView, String> {
        validate_scope(root_id, canonical_path, id)?;
        let mut current = self.lock();
        if let Some(error) = &current.blocked {
            return Err(error.clone());
        }
        let old = current_view(&current.data, root_id, canonical_path, id);
        if old.revision != expected_revision {
            return Err("实例资料已更新或游戏目录已改变，请刷新后重试".into());
        }
        if patch.description.is_none()
            && patch.favorite.is_none()
            && patch.icon.is_none()
            && patch.category.is_none()
        {
            return Err("请指定要修改的实例资料字段".into());
        }
        let mut metadata = old.metadata();
        if let Some(description) = patch.description {
            let description = description.replace("\r\n", "\n");
            if description.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err("实例描述最多允许 4096 个字符".into());
            }
            // Check control characters before trim, so NUL/TAB/CR cannot be
            // silently removed at the edges by normalization.
            if description
                .chars()
                .any(|character| character.is_control() && character != '\n')
            {
                return Err("实例描述不能包含换行以外的控制字符".into());
            }
            metadata.description = description.trim().to_owned();
            validate_description(&metadata.description)?;
        }
        if let Some(favorite) = patch.favorite {
            metadata.favorite = favorite;
        }
        if let Some(icon) = patch.icon {
            metadata.icon = icon;
        }
        if let Some(category) = patch.category {
            metadata.category = category;
        }
        if metadata == old.metadata() {
            self.check_disk(&mut current)?;
            return Ok(old);
        }
        let mut next = current.data.clone();
        match record_index(&next, root_id, canonical_path, id) {
            Some(index) => {
                let record = &mut next.entries[index];
                record.revision = record
                    .revision
                    .checked_add(1)
                    .ok_or("实例资料修订号已耗尽")?;
                record.metadata = metadata;
            }
            None => next.entries.push(Record {
                root_id: root_id.into(),
                root_path: canonical_path
                    .to_str()
                    .ok_or("游戏目录路径不是有效的 UTF-8")?
                    .into(),
                instance_id: id.into(),
                revision: 1,
                metadata,
            }),
        }
        // Keep all-default records and their generation to avoid an ABA token
        // when a user clears metadata previously edited by another client.
        self.commit(&mut current, next)?;
        Ok(current_view(&current.data, root_id, canonical_path, id))
    }

    fn project_path(&self) -> Result<std::path::PathBuf, String> {
        let directory = self.project.as_ref().ok_or("元数据项目目录不可用")?;
        if directory.identity()? != Directory::open(&self.project_path)?.identity()? {
            return Err("元数据项目目录已被外部替换".into());
        }
        Ok(self.project_path.clone())
    }

    pub fn ensure_rename_snapshot(&self) -> Result<(), String> {
        let mut current = self.lock();
        if let Some(error) = &current.blocked {
            return Err(error.clone());
        }
        self.project_path()?;
        let storage = self
            .project
            .as_ref()
            .ok_or("元数据项目目录不可用")?
            .storage()?;
        let _guard = storage
            .as_ref()
            .map(|directory| directory.write_lock())
            .transpose()?;
        self.check_disk(&mut current)
    }

    pub fn ensure_new_instance_name(
        &self,
        root_id: &str,
        root: &Path,
        id: &str,
    ) -> Result<(), String> {
        self.ensure_rename_snapshot()?;
        let current = self.lock();
        // Even a cleared record carries a generation. Reusing it would allow
        // an old metadata confirmation to edit a different physical instance.
        if record_index(&current.data, root_id, root, id).is_some() {
            return Err("此名称仍有保存的实例资料，请使用新的实例名称".into());
        }
        Ok(())
    }

    /// Explicitly adopts validated disk state after a journalled rename.
    pub fn refresh_after_rename(&self) -> Result<(), String> {
        let mut current = self.lock();
        let project = self.project.as_ref().ok_or("元数据项目目录不可用")?;
        let directory = project.storage()?;
        let _guard = directory
            .as_ref()
            .map(|directory| directory.write_lock())
            .transpose()?;
        let result: Result<(), String> = (|| {
            self.project_path()?;
            let (disk, bytes) = disk_snapshot(project)?;
            if current.disk.file.is_some() && disk.file.is_none() {
                return Err("实例元数据文件已消失，已保留当前资料并禁止写入".into());
            }
            let data = match bytes {
                Some(bytes) => serde_json::from_slice::<Persisted>(&bytes)
                    .map_err(|error| format!("实例元数据无法读取：{error}"))?,
                None => Persisted::default(),
            };
            validate_persisted(&data)?;
            current.disk = disk;
            current.data = data;
            current.blocked = None;
            Ok(())
        })();
        if let Err(error) = &result {
            current.blocked = Some(error.clone());
        }
        result
    }
}

pub(crate) fn rename_bytes(
    bytes: &[u8],
    root_id: &str,
    root: &Path,
    old: &str,
    new: &str,
) -> Result<Vec<u8>, String> {
    rename_optional_bytes(Some(bytes), root_id, root, old, new)
}

pub(crate) fn rename_optional_bytes(
    bytes: Option<&[u8]>,
    root_id: &str,
    root: &Path,
    old: &str,
    new: &str,
) -> Result<Vec<u8>, String> {
    validate_scope(root_id, root, old)?;
    validate_scope(root_id, root, new)?;
    if old == new {
        return Err("实例名称没有改变".into());
    }
    let mut data: Persisted = match bytes {
        Some(bytes) => {
            serde_json::from_slice(bytes).map_err(|_| "实例元数据损坏，原文件已保留".to_string())?
        }
        None => Persisted::default(),
    };
    validate_persisted(&data)?;
    let source = record_index(&data, root_id, root, old);
    let target = record_index(&data, root_id, root, new);
    if target.is_some_and(|index| data.entries[index].metadata != Metadata::default()) {
        return Err("目标名称已有保存的实例资料，请选择其他名称".into());
    }
    let source_generation = source.map_or(0, |index| data.entries[index].revision);
    let target_generation = target.map_or(0, |index| data.entries[index].revision);
    let generation = source_generation
        .max(target_generation)
        .checked_add(1)
        .ok_or("实例资料修订号已耗尽")?;
    let source_revision = source_generation
        .checked_add(1)
        .ok_or("实例资料修订号已耗尽")?;
    let moved = source
        .map(|index| data.entries[index].metadata.clone())
        .unwrap_or_default();
    let record = |instance_id: &str, revision, metadata| Record {
        root_id: root_id.into(),
        root_path: root.to_string_lossy().into_owned(),
        instance_id: instance_id.into(),
        revision,
        metadata,
    };
    match source {
        Some(index) => data.entries[index] = record(old, source_revision, Metadata::default()),
        None => data
            .entries
            .push(record(old, source_revision, Metadata::default())),
    }
    match target {
        Some(index) => data.entries[index] = record(new, generation, moved),
        None => data.entries.push(record(new, generation, moved)),
    }
    validate_persisted(&data)?;
    let mut result = serde_json::to_vec_pretty(&data).map_err(|error| error.to_string())?;
    result.push(b'\n');
    if result.len() > MAX_FILE_BYTES {
        return Err("实例元数据文件超过 8 MiB 上限".into());
    }
    Ok(result)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        path::PathBuf,
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let project = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../work/instance-metadata-tests")
                .join(nonce());
            fs::create_dir_all(&project).unwrap();
            Self(fs::canonicalize(project).unwrap())
        }

        fn file(&self) -> PathBuf {
            self.0.join(".pcl-rust").join(FILE_NAME)
        }

        fn root(&self, value: &str) -> PathBuf {
            // A canonical, offline root is enough: metadata never reads games.
            self.0.join(value)
        }

        fn write(&self, bytes: &[u8]) {
            fs::create_dir_all(self.file().parent().unwrap()).unwrap();
            fs::write(self.file(), bytes).unwrap();
        }

        fn set(&self, store: &MetadataStore, root: &Path, patch: MetadataPatch) -> MetaView {
            let old = store.get("root-a", root, "same");
            store
                .patch("root-a", root, "same", &old.revision, patch)
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn description(value: &str) -> MetadataPatch {
        MetadataPatch {
            description: Some(value.into()),
            ..Default::default()
        }
    }

    #[test]
    fn missing_metadata_defaults_and_settings_accounts_are_untouched() {
        let fixture = Fixture::new();
        let storage = fixture.0.join(".pcl-rust");
        fs::create_dir_all(&storage).unwrap();
        fs::write(storage.join("settings.json"), b"old settings").unwrap();
        fs::write(storage.join("accounts.json"), b"private fixture accounts").unwrap();
        let store = MetadataStore::load(&fixture.0);
        let root = fixture.root("offline-root");
        let old = store.get("root-a", &root, "same");
        assert_eq!(old.metadata(), Metadata::default());
        assert_eq!(old.revision.len(), 64);
        assert!(store.warning().is_none());
        assert!(!fixture.file().exists());
        fixture.set(&store, &root, description("hello"));
        assert_eq!(
            fs::read(storage.join("settings.json")).unwrap(),
            b"old settings"
        );
        assert_eq!(
            fs::read(storage.join("accounts.json")).unwrap(),
            b"private fixture accounts"
        );
    }

    #[test]
    fn default_revisions_and_same_name_records_are_fully_scoped() {
        let fixture = Fixture::new();
        let store = MetadataStore::load(&fixture.0);
        let first = fixture.root("first");
        let second = fixture.root("second");
        let old = store.get("root-a", &first, "same");
        for (root_id, path, id) in [
            ("root-b", first.as_path(), "same"),
            ("root-a", second.as_path(), "same"),
            ("root-a", first.as_path(), "other"),
        ] {
            assert_ne!(old.revision, store.get(root_id, path, id).revision);
            assert!(store
                .patch(root_id, path, id, &old.revision, description("wrong"))
                .is_err());
        }
        fixture.set(&store, &first, description("first description"));
        fixture.set(&store, &second, description("second description"));
        assert_eq!(
            store.get("root-a", &first, "same").description,
            "first description"
        );
        assert_eq!(
            store.get("root-a", &second, "same").description,
            "second description"
        );
    }

    #[test]
    fn patches_merge_normalize_unicode_and_survive_reload() {
        let fixture = Fixture::new();
        let store = MetadataStore::load(&fixture.0);
        let root = fixture.root("root");
        fixture.set(&store, &root, description("  第一行\r\n第二行  "));
        fixture.set(
            &store,
            &root,
            MetadataPatch {
                favorite: Some(true),
                ..Default::default()
            },
        );
        fixture.set(
            &store,
            &root,
            MetadataPatch {
                icon: Some(Icon::Steve),
                ..Default::default()
            },
        );
        let expected = fixture.set(
            &store,
            &root,
            MetadataPatch {
                category: Some(Category::Fabric),
                ..Default::default()
            },
        );
        assert_eq!(expected.description, "第一行\n第二行");
        assert!(expected.favorite);
        assert_eq!(expected.icon, Icon::Steve);
        assert_eq!(expected.category, Category::Fabric);
        let reopened = MetadataStore::load(&fixture.0);
        assert_eq!(reopened.get("root-a", &root, "same"), expected);
        assert!(reopened.warning().is_none());
        assert_eq!(
            fs::metadata(fixture.file()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(serde_json::to_value(&expected).unwrap()["icon"], "steve");
        assert!(serde_json::to_value(&expected)
            .unwrap()
            .get("metadata")
            .is_none());
    }

    #[test]
    fn stale_revisions_and_clear_to_defaults_do_not_reuse_tokens() {
        let fixture = Fixture::new();
        let store = MetadataStore::load(&fixture.0);
        let root = fixture.root("root");
        let default = store.get("root-a", &root, "same");
        let edited = fixture.set(&store, &root, description("edited"));
        assert!(store
            .patch(
                "root-a",
                &root,
                "same",
                &default.revision,
                description("stale")
            )
            .is_err());
        let cleared = fixture.set(&store, &root, description(""));
        assert_eq!(cleared.metadata(), Metadata::default());
        assert_ne!(cleared.revision, default.revision);
        assert_ne!(cleared.revision, edited.revision);
        let reopened = MetadataStore::load(&fixture.0);
        assert_eq!(reopened.get("root-a", &root, "same"), cleared);
        assert!(reopened
            .patch(
                "root-a",
                &root,
                "same",
                &default.revision,
                description("stale")
            )
            .is_err());
    }

    #[test]
    fn invalid_values_and_unknown_patch_fields_are_rejected_without_writes() {
        let fixture = Fixture::new();
        let store = MetadataStore::load(&fixture.0);
        let root = fixture.root("root");
        let old = store.get("root-a", &root, "same");
        for invalid in ["nul\0", "\tx", "bare\r", "other\u{7f}"] {
            assert!(store
                .patch("root-a", &root, "same", &old.revision, description(invalid))
                .is_err());
        }
        assert!(store
            .patch(
                "root-a",
                &root,
                "same",
                &old.revision,
                description(&"汉".repeat(4097))
            )
            .is_err());
        assert!(store
            .patch(
                "root-a",
                &root,
                "same",
                &old.revision,
                MetadataPatch::default()
            )
            .is_err());
        for invalid in ["../unsafe", ".", "", "bad:name", "bad\nname"] {
            let view = store.get("root-a", &root, invalid);
            assert!(store
                .patch("root-a", &root, invalid, &view.revision, description("x"))
                .is_err());
        }
        assert!(serde_json::from_value::<MetadataPatch>(json!({"icon":"unknown"})).is_err());
        assert!(serde_json::from_value::<MetadataPatch>(json!({"category":"unknown"})).is_err());
        assert!(serde_json::from_value::<MetadataPatch>(json!({"alias":"new"})).is_err());
        assert!(!fixture.file().exists());
        let max = fixture.set(&store, &root, description(&"汉".repeat(4096)));
        assert_eq!(max.description.chars().count(), 4096);
    }

    #[test]
    fn future_corrupt_unknown_and_invalid_files_are_preserved_read_only() {
        for bytes in [
            br#"{"schema_version":2,"entries":[]}"#.to_vec(),
            b"broken json".to_vec(),
            br#"{"schema_version":1,"entries":[],"extra":1}"#.to_vec(),
            br#"{"schema_version":1,"entries":[{"root_id":"root-a","root_path":"/fixture/root","instance_id":"same","revision":0,"metadata":{}}]}"#.to_vec(),
            br#"{"schema_version":1,"entries":[{"root_id":"root-a","root_path":"/fixture/root","instance_id":"same","revision":1,"metadata":{"icon":"bad"}}]}"#.to_vec(),
        ] {
            let fixture = Fixture::new();
            fixture.write(&bytes);
            let store = MetadataStore::load(&fixture.0);
            let root = fixture.root("root");
            let old = store.get("root-a", &root, "same");
            assert_eq!(old.metadata(), Metadata::default());
            assert!(store.warning().is_some());
            assert!(store.patch("root-a", &root, "same", &old.revision, description("cannot write")).is_err());
            assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
        }
    }

    #[test]
    fn duplicate_scopes_and_noncanonical_persisted_paths_are_invalid() {
        let fixture = Fixture::new();
        let record = json!({"root_id":"root-a","root_path":"/fixture/root","instance_id":"same","revision":1,"metadata":{}});
        fixture.write(
            json!({"schema_version":1,"entries":[record.clone(),record]})
                .to_string()
                .as_bytes(),
        );
        assert!(MetadataStore::load(&fixture.0).warning().is_some());
        for path in [
            "/fixture/../root",
            "/fixture//root",
            "/fixture/root/",
            "relative",
        ] {
            fixture.write(json!({"schema_version":1,"entries":[{"root_id":"root-a","root_path":path,"instance_id":"same","revision":1,"metadata":{}}]}).to_string().as_bytes());
            assert!(MetadataStore::load(&fixture.0).warning().is_some());
        }
    }

    #[test]
    fn symlink_metadata_and_storage_are_rejected_without_following() {
        let fixture = Fixture::new();
        let target = fixture.0.join("target");
        fs::write(&target, b"protected fixture").unwrap();
        fs::create_dir_all(fixture.file().parent().unwrap()).unwrap();
        symlink(&target, fixture.file()).unwrap();
        let store = MetadataStore::load(&fixture.0);
        assert!(store.warning().is_some());
        let root = fixture.root("root");
        let old = store.get("root-a", &root, "same");
        assert!(store
            .patch(
                "root-a",
                &root,
                "same",
                &old.revision,
                description("blocked")
            )
            .is_err());
        assert_eq!(fs::read(&target).unwrap(), b"protected fixture");
        fs::remove_file(fixture.file()).unwrap();
        fs::remove_dir(fixture.file().parent().unwrap()).unwrap();
        let other = fixture.0.join("other-storage");
        fs::create_dir(&other).unwrap();
        symlink(&other, fixture.file().parent().unwrap()).unwrap();
        assert!(MetadataStore::load(&fixture.0).warning().is_some());
        assert_eq!(fs::read_dir(&other).unwrap().count(), 0);
    }

    #[test]
    fn external_edits_replacements_and_directory_retargets_block_overwrite() {
        for mode in ["edit", "replace", "directory", "symlink", "delete"] {
            let fixture = Fixture::new();
            let store = MetadataStore::load(&fixture.0);
            let root = fixture.root("root");
            let old = fixture.set(&store, &root, description("before"));
            let external = b"external fixture contents";
            match mode {
                "edit" => fs::write(fixture.file(), external).unwrap(),
                "replace" => {
                    let next = fixture.0.join("replacement");
                    fs::write(&next, external).unwrap();
                    fs::rename(next, fixture.file()).unwrap();
                }
                "directory" => {
                    fs::rename(
                        fixture.file().parent().unwrap(),
                        fixture.0.join("old-storage"),
                    )
                    .unwrap();
                    fixture.write(external);
                }
                "symlink" => {
                    fs::remove_file(fixture.file()).unwrap();
                    let target = fixture.0.join("external-target");
                    fs::write(&target, external).unwrap();
                    symlink(target, fixture.file()).unwrap();
                }
                "delete" => fs::remove_file(fixture.file()).unwrap(),
                _ => unreachable!(),
            }
            assert!(
                store
                    .patch(
                        "root-a",
                        &root,
                        "same",
                        &old.revision,
                        description("overwrite")
                    )
                    .is_err(),
                "{mode}"
            );
            assert!(store.warning().is_some());
            assert_eq!(store.get("root-a", &root, "same"), old);
            if mode != "delete" {
                assert_eq!(fs::read(fixture.file()).unwrap(), external);
            } else {
                assert!(!fixture.file().exists());
            }
        }
    }

    #[test]
    fn separate_stores_detect_other_writer_and_missing_file_creation() {
        let fixture = Fixture::new();
        let first = MetadataStore::load(&fixture.0);
        let second = MetadataStore::load(&fixture.0);
        let root = fixture.root("root");
        let stale = second.get("root-a", &root, "same");
        let changed = fixture.set(&first, &root, description("first writer"));
        assert!(second
            .patch(
                "root-a",
                &root,
                "same",
                &stale.revision,
                description("second writer")
            )
            .is_err());
        assert_eq!(
            MetadataStore::load(&fixture.0).get("root-a", &root, "same"),
            changed
        );
    }

    #[test]
    fn settings_may_create_initial_storage_after_metadata_load() {
        let fixture = Fixture::new();
        let store = MetadataStore::load(&fixture.0);
        let storage = fixture.file().parent().unwrap().to_owned();
        fs::create_dir(&storage).unwrap();
        fs::write(storage.join("settings.json"), b"fixture settings").unwrap();
        let root = fixture.root("root");
        fixture.set(&store, &root, description("first metadata"));
        assert_eq!(
            fs::read(storage.join("settings.json")).unwrap(),
            b"fixture settings"
        );
    }

    #[test]
    fn atomic_save_failure_preserves_previous_data_and_cleans_temporary_files() {
        let fixture = Fixture::new();
        let store = MetadataStore::load(&fixture.0);
        let root = fixture.root("root");
        let old = fixture.set(&store, &root, description("before"));
        let original = fs::read(fixture.file()).unwrap();
        store.lock().fail_before_replace = true;
        assert!(store
            .patch("root-a", &root, "same", &old.revision, description("after"))
            .is_err());
        assert_eq!(store.get("root-a", &root, "same"), old);
        assert_eq!(fs::read(fixture.file()).unwrap(), original);
        assert_eq!(
            fs::read_dir(fixture.file().parent().unwrap())
                .unwrap()
                .count(),
            2
        );
        assert!(store.warning().is_none());
        store.lock().fail_before_replace = false;
        let after = fixture.set(&store, &root, description("after"));
        assert_eq!(
            MetadataStore::load(&fixture.0).get("root-a", &root, "same"),
            after
        );
    }

    #[test]
    fn failure_after_creating_directory_can_retry_without_losing_defaults() {
        let fixture = Fixture::new();
        let store = MetadataStore::load(&fixture.0);
        let root = fixture.root("root");
        let old = store.get("root-a", &root, "same");
        store.lock().fail_before_replace = true;
        assert!(store
            .patch("root-a", &root, "same", &old.revision, description("first"))
            .is_err());
        assert_eq!(store.get("root-a", &root, "same"), old);
        assert!(!fixture.file().exists());
        store.lock().fail_before_replace = false;
        fixture.set(&store, &root, description("retry"));
    }

    #[test]
    fn concurrent_stores_cannot_both_commit_from_the_same_disk_token() {
        let fixture = Fixture::new();
        let root = fixture.root("root");
        let initial = MetadataStore::load(&fixture.0);
        fixture.set(
            &initial,
            &root,
            MetadataPatch {
                description: Some("base".into()),
                icon: Some(Icon::Steve),
                category: Some(Category::Fabric),
                ..Default::default()
            },
        );
        let first = MetadataStore::load(&fixture.0);
        let second = MetadataStore::load(&fixture.0);
        let old = first.get("root-a", &root, "same");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        first.lock().before_write_lock = Some(barrier.clone());
        second.lock().before_write_lock = Some(barrier);
        let (one, two) = std::thread::scope(|scope| {
            let one = scope.spawn(|| {
                first.patch(
                    "root-a",
                    &root,
                    "same",
                    &old.revision,
                    description("writer one"),
                )
            });
            let two = scope.spawn(|| {
                second.patch(
                    "root-a",
                    &root,
                    "same",
                    &old.revision,
                    MetadataPatch {
                        favorite: Some(true),
                        ..Default::default()
                    },
                )
            });
            (one.join().unwrap(), two.join().unwrap())
        });
        assert_ne!(one.is_ok(), two.is_ok());
        let winner = one.as_ref().ok().or_else(|| two.as_ref().ok()).unwrap();
        let rejected = one.as_ref().err().or_else(|| two.as_ref().err()).unwrap();
        assert!(rejected.contains("另一个启动器") || rejected.contains("外部修改"));
        let reloaded = MetadataStore::load(&fixture.0).get("root-a", &root, "same");
        assert_eq!(&reloaded, winner);
        assert_eq!(reloaded.icon, Icon::Steve);
        assert_eq!(reloaded.category, Category::Fabric);
        if one.is_ok() {
            assert_eq!(reloaded.description, "writer one");
            assert!(!reloaded.favorite);
        } else {
            assert_eq!(reloaded.description, "base");
            assert!(reloaded.favorite);
        }
    }

    #[test]
    fn write_lock_is_nonblocking_and_symlinks_are_never_followed() {
        let fixture = Fixture::new();
        let storage_path = fixture.file().parent().unwrap().to_owned();
        fs::create_dir(&storage_path).unwrap();
        let store = MetadataStore::load(&fixture.0);
        let project = Directory::open(&fixture.0).unwrap();
        let storage = project.storage().unwrap().unwrap();
        let guard = storage.write_lock().unwrap();
        let root = fixture.root("root");
        let old = store.get("root-a", &root, "same");
        let error = store
            .patch(
                "root-a",
                &root,
                "same",
                &old.revision,
                description("blocked"),
            )
            .unwrap_err();
        assert!(error.contains("另一个启动器"));
        assert!(!fixture.file().exists());
        assert!(store.warning().is_none());
        drop(guard);
        fixture.set(&store, &root, description("after lock released"));
        let previous = fs::read(fixture.file()).unwrap();
        let target = fixture.0.join("protected-lock-target");
        fs::write(&target, b"fixture unchanged").unwrap();
        fs::remove_file(storage_path.join(LOCK_NAME)).unwrap();
        symlink(&target, storage_path.join(LOCK_NAME)).unwrap();
        let current = store.get("root-a", &root, "same");
        assert!(store
            .patch(
                "root-a",
                &root,
                "same",
                &current.revision,
                description("unsafe lock")
            )
            .is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), previous);
        assert_eq!(fs::read(target).unwrap(), b"fixture unchanged");
    }

    #[test]
    fn shared_record_and_file_size_limits_reject_unreadable_or_unwritable_data() {
        let fixture = Fixture::new();
        fixture.write(&vec![b' '; MAX_FILE_BYTES + 1]);
        let store = MetadataStore::load(&fixture.0);
        assert!(store.warning().is_some());
        assert_eq!(
            fs::metadata(fixture.file()).unwrap().len(),
            MAX_FILE_BYTES as u64 + 1
        );
        fs::remove_file(fixture.file()).unwrap();
        let store = MetadataStore::load(&fixture.0);
        let root = fixture.root("root");
        let mut next = Persisted::default();
        for index in 0..=MAX_RECORDS {
            next.entries.push(Record {
                root_id: "root-a".into(),
                root_path: root.to_str().unwrap().into(),
                instance_id: format!("instance-{index}"),
                revision: 1,
                metadata: Metadata::default(),
            });
        }
        assert!(store.commit(&mut store.lock(), next.clone()).is_err());
        next.entries.truncate(600);
        for record in &mut next.entries {
            record.metadata.description = "😀".repeat(MAX_DESCRIPTION_CHARS);
        }
        assert!(serde_json::to_vec_pretty(&next).unwrap().len() > MAX_FILE_BYTES);
        assert!(store.commit(&mut store.lock(), next).is_err());
        assert!(!fixture.file().exists());
    }
}

#[cfg(test)]
#[path = "instance_meta/native_tests.rs"]
mod native_tests;
