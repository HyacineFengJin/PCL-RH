//! Missing-only repair and explicit read-only integrity checks of a captured
//! installed instance. The writer's destination authority
//! comes from bounded metadata enumeration and retained directory descriptors,
//! never from an inspection report or a UI path. The missing-only writer does
//! not validate or replace existing contents. IntegrityCheck hashes existing
//! declared dependencies separately. No install processors or Java run here.
//! DamageRepair separately authorizes verified replacements with persistent
//! original backups and explicit interrupted-transaction recovery.
//!
//! State flow: capture version/index bytes and directory identities -> validate
//! -> fetch a missing verified index into memory -> enumerate objects -> fetch
//! each missing file into an anonymous inode -> revalidate -> publish without
//! replacement -> report current existence. Cancellation drops unpublished
//! inodes; already published verified files remain usable on the next attempt.
use super::*;
use pcl_core::repair::{self as manifest, RequiredFile};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    time::Instant,
};

#[path = "integrity.rs"]
mod integrity;
pub use integrity::{IntegrityCheck, IntegrityProblem, IntegrityProblemKind, IntegrityReport};
#[path = "damage_repair.rs"]
mod damage_repair;
pub use damage_repair::{DamageRecoveryReport, DamageRepair, DamageRepairReport};

const MAX_VERSION_BYTES: u64 = 8 * 1024 * 1024;
const MAX_VERSION_TOTAL: u64 = 32 * 1024 * 1024;
const MAX_MISSING_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const UNSUPPORTED_LIMIT: usize = 32;
const TEXT_LIMIT: usize = 2048;
const MAX_BOUND_DIRECTORIES: usize = 4096;

#[derive(Clone, Debug, Serialize)]
pub struct RepairUnsupported {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MissingRepairReport {
    pub id: String,
    /// Counts verified files newly published by this attempt.
    pub files_downloaded: u64,
    /// Existence only: old contents, checksums and compatibility are not tested.
    pub files_present: u64,
    pub unsupported: Vec<RepairUnsupported>,
    pub unsupported_total: usize,
}

pub struct MissingRepair {
    id: String,
    files: Vec<RequiredFile>,
    index: Option<RequiredFile>,
    captured_index: Option<Value>,
    state: Mutex<RepairState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureMode {
    MissingOnly,
    Verify,
}

impl MissingRepair {
    /// The display root must be the caller's canonical physical directory. It
    /// is checked against the supplied FD; changes never redirect the request.
    /// Capture performs reads only and is suitable for a background worker.
    pub fn capture_bound(root: fs::File, root_display: impl AsRef<Path>, id: &str) -> Result<Self> {
        Self::capture(root, root_display.as_ref(), id, CaptureMode::MissingOnly)
    }

    fn capture(root: fs::File, root_display: &Path, id: &str, mode: CaptureMode) -> Result<Self> {
        pcl_core::identifier(id)?;
        let state = RefCell::new(RepairState {
            filesystem: RepairFilesystem::new(root, root_display)?,
            metadata: Vec::new(),
            jar_probes: BTreeMap::new(),
            missing_index: None,
            version_bytes: 0,
        });
        let metadata = manifest::resolve_metadata(
            id,
            |version| {
                let mut state = state.borrow_mut();
                let relative = format!("versions/{version}/{version}.json");
                if let Some(snapshot) = state.metadata.iter().find(|s| s.relative == relative) {
                    return json(&snapshot.bytes);
                }
                let snapshot = state.filesystem.snapshot(&relative, MAX_VERSION_BYTES)?;
                state.version_bytes = state
                    .version_bytes
                    .checked_add(snapshot.bytes.len() as u64)
                    .ok_or("版本元数据总大小溢出")?;
                if state.version_bytes > MAX_VERSION_TOTAL {
                    return Err("版本元数据总大小超过补全上限".into());
                }
                let value = json(&snapshot.bytes)?;
                state.metadata.push(snapshot);
                Ok(value)
            },
            |version| {
                let mut state = state.borrow_mut();
                let relative = format!("versions/{version}/{version}.jar");
                let present = state.filesystem.inspect(&relative)?.present();
                state.jar_probes.insert(relative, present);
                Ok(present)
            },
        )?;
        let required = manifest::required_files(&metadata)?;
        let mut captured_index = None;
        let mut files = required.files;
        if let Some(index) = &required.asset_index {
            if let Some(relative) = &index.relative {
                let mut state = state.borrow_mut();
                if state.filesystem.inspect(relative)?.present() {
                    let snapshot = state
                        .filesystem
                        .snapshot(relative, manifest::MAX_ASSET_INDEX_BYTES)?;
                    // Verification must not trust objects named by a damaged
                    // index. Missing-only capture retains its existing strict
                    // parse behavior and never hashes existing dependencies.
                    if mode == CaptureMode::MissingOnly {
                        let value = json(&snapshot.bytes)?;
                        files.extend(manifest::asset_objects(&value)?);
                        captured_index = Some(value);
                    } else if integrity::trusted_bytes(index, &snapshot.bytes) {
                        if let Ok(value) = json(&snapshot.bytes) {
                            if let Ok(objects) = manifest::asset_objects(&value) {
                                files.extend(objects);
                                captured_index = Some(value);
                            }
                        }
                    }
                    state.metadata.push(snapshot);
                } else {
                    state.missing_index = Some(relative.clone());
                }
            }
        }
        let files = manifest::deduplicate(files)?;
        {
            let mut state = state.borrow_mut();
            for file in files.iter().chain(required.asset_index.iter()) {
                if let Some(relative) = &file.relative {
                    state.filesystem.inspect(relative)?;
                }
            }
            state.verify()?;
        }
        Ok(Self {
            id: id.into(),
            files,
            index: required.asset_index,
            captured_index,
            state: Mutex::new(state.into_inner()),
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn verify(&self) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| "缺失补全捕获锁损坏")?
            .verify()
    }

    /// Count the captured required paths that are currently absent. A missing
    /// index counts once; its unknown objects are never guessed or fetched.
    /// Existing files are checked for presence only, as in repair_missing().
    pub fn missing_count(&self) -> Result<usize> {
        let mut state = self.state.lock().map_err(|_| "缺失补全捕获锁损坏")?;
        state.verify()?;
        let mut files = self.files.clone();
        files.extend(self.index.iter().cloned());
        let mut missing = 0;
        for file in manifest::deduplicate(files)? {
            if match file.relative {
                Some(path) => !state.filesystem.inspect(&path)?.present(),
                None => true,
            } {
                missing += 1;
            }
        }
        state.verify()?;
        Ok(missing)
    }
}

fn json(bytes: &[u8]) -> Result<Value> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    serde_json::from_slice(bytes).map_err(|error| format!("实例或资源索引元数据无效：{error}"))
}

struct RepairState {
    filesystem: RepairFilesystem,
    metadata: Vec<FileSnapshot>,
    jar_probes: BTreeMap<String, bool>,
    // Index absence is part of the metadata capture. It may only transition
    // to the verified bytes published by this request, never a new external
    // index that could silently redirect the object enumeration.
    missing_index: Option<String>,
    version_bytes: u64,
}
impl RepairState {
    fn verify_fast(&self) -> Result<()> {
        self.filesystem.verify()?;
        for snapshot in &self.metadata {
            if self.filesystem.stamp(&snapshot.relative)? != Some(snapshot.stamp) {
                return Err("实例元数据或资源索引在补全过程中变化，请重新检查".into());
            }
        }
        for (path, expected) in &self.jar_probes {
            if self.filesystem.existing_state(path)?.present() != *expected {
                return Err("实例继承核心 JAR 选择在补全过程中变化，请重新检查".into());
            }
        }
        if let Some(path) = &self.missing_index {
            if self.filesystem.existing_state(path)?.present() {
                return Err("资源索引在补全期间由其他操作创建，请重新检查".into());
            }
        }
        Ok(())
    }
    fn verify(&self) -> Result<()> {
        self.verify_fast()?;
        for snapshot in &self.metadata {
            let current = self
                .filesystem
                .read_existing(&snapshot.relative, snapshot.bytes.len() as u64)?;
            if current.bytes != snapshot.bytes || current.stamp != snapshot.stamp {
                return Err("实例元数据或资源索引在补全过程中变化，请重新检查".into());
            }
        }
        self.verify_fast()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}
impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}
struct FileSnapshot {
    relative: String,
    bytes: Vec<u8>,
    stamp: Stamp,
}

#[derive(Clone, Copy)]
enum FileState {
    Missing,
    Present,
}
impl FileState {
    fn present(self) -> bool {
        matches!(self, Self::Present)
    }
}

/// Every existing parent is retained, and missing parents are remembered.
/// Reappearing/replaced directories fail closed. Newly created directories are
/// added to the same identity graph before an anonymous file is allocated.
struct RepairFilesystem {
    display: PathBuf,
    directories: BTreeMap<String, Arc<fs::File>>,
    absent: BTreeSet<String>,
    directory_limit: usize,
}
impl RepairFilesystem {
    fn new(root: fs::File, display: &Path) -> Result<Self> {
        if !root.metadata().map_err(error)?.is_dir() {
            return Err("补全根 FD 不是目录".into());
        }
        open_directory(&root, ".")
            .map_err(|e| format!("缺失补全需要 Linux openat2 安全目录能力：{e}"))?;
        let mut directories = BTreeMap::new();
        directories.insert(String::new(), Arc::new(root));
        // Keep room for the desktop, response sockets and the active temporary
        // inode. Dependency cardinality must not exhaust process descriptors.
        let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        let directory_limit =
            if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } == 0 {
                let limit = unsafe { limit.assume_init() }.rlim_cur;
                (limit.saturating_sub(128) as usize).min(MAX_BOUND_DIRECTORIES)
            } else {
                MAX_BOUND_DIRECTORIES
            };
        if directory_limit < 1 {
            return Err("当前进程可用文件描述符不足以安全补全".into());
        }
        let result = Self {
            display: display.into(),
            directories,
            absent: BTreeSet::new(),
            directory_limit,
        };
        result.verify()?;
        Ok(result)
    }
    fn verify(&self) -> Result<()> {
        let expected = self.directories[""].metadata().map_err(error)?;
        let actual =
            fs::symlink_metadata(&self.display).map_err(|e| format!("补全根目录已失效：{e}"))?;
        if !actual.is_dir() || actual.dev() != expected.dev() || actual.ino() != expected.ino() {
            return Err("补全根目录绑定已变化，请重新检查".into());
        }
        for (path, file) in self.directories.iter().filter(|(path, _)| !path.is_empty()) {
            let (parent, leaf) = split(path);
            let expected = file.metadata().map_err(error)?;
            let actual = entry_metadata(&self.directories[parent], leaf)?;
            if actual.is_none_or(|m| {
                m.st_mode & libc::S_IFMT != libc::S_IFDIR
                    || m.st_dev != expected.dev()
                    || m.st_ino != expected.ino()
            }) {
                return Err("补全所需目录已替换或移除，请重新检查".into());
            }
        }
        for path in &self.absent {
            let (parent, leaf) = split(path);
            if let Some(parent) = self.directories.get(parent) {
                if entry_metadata(parent, leaf)?.is_some() {
                    return Err("原先缺失的补全目录已由其他操作创建，请重新检查".into());
                }
            }
        }
        Ok(())
    }
    fn parent(&mut self, relative: &str, create: bool) -> Result<Option<Arc<fs::File>>> {
        let relative = manifest::relative_path(relative)?;
        let (parent_path, _) = split(&relative);
        let mut current = self.directories[""].clone();
        let mut prefix = String::new();
        for name in parent_path.split('/').filter(|s| !s.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            if let Some(dir) = self.directories.get(&prefix) {
                let expected = dir.metadata().map_err(error)?;
                let actual = entry_metadata(&current, name)?;
                if actual.is_none_or(|m| {
                    m.st_mode & libc::S_IFMT != libc::S_IFDIR
                        || m.st_dev != expected.dev()
                        || m.st_ino != expected.ino()
                }) {
                    return Err("补全所需目录已替换或移除，请重新检查".into());
                }
                current = dir.clone();
                continue;
            }
            if self.absent.contains(&prefix) && entry_metadata(&current, name)?.is_some() {
                return Err("原先缺失的补全目录已由其他操作创建，请重新检查".into());
            }
            // Missing prefixes also participate in the bound: otherwise a
            // hostile manifest could create an unbounded absence graph and
            // force its repeated traversal at every publication boundary.
            if self.directories.len() >= self.directory_limit
                || (self.directories.len() + self.absent.len() >= self.directory_limit
                    && !self.absent.contains(&prefix))
            {
                return Err(format!(
                    "补全所需目录数量超过安全上限（{} 个绑定目录）",
                    self.directory_limit
                ));
            }
            match open_directory(&current, name) {
                Ok(dir) => {
                    let dir = Arc::new(dir);
                    self.directories.insert(prefix.clone(), dir.clone());
                    current = dir;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.absent.insert(prefix.clone());
                    if !create {
                        return Ok(None);
                    }
                    // versions and its owner directories were captured while
                    // reading JSON; repair cannot manufacture a lost instance.
                    if prefix == "versions" || prefix.starts_with("versions/") {
                        return Err("核心 JAR 所属实例目录已失效".into());
                    }
                    let name_c = cstring(name)?;
                    if unsafe { libc::mkdirat(current.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
                        return Err(format!(
                            "无法创建缺失补全目录：{}",
                            std::io::Error::last_os_error()
                        ));
                    }
                    let dir = Arc::new(open_directory(&current, name).map_err(error)?);
                    self.absent.remove(&prefix);
                    self.directories.insert(prefix.clone(), dir.clone());
                    current = dir;
                }
                Err(e) => {
                    return Err(format!(
                        "补全路径不是安全目录（禁止链接、挂载和特殊文件）：{e}"
                    ))
                }
            }
        }
        Ok(Some(current))
    }
    fn inspect(&mut self, relative: &str) -> Result<FileState> {
        let Some(parent) = self.parent(relative, false)? else {
            return Ok(FileState::Missing);
        };
        leaf_state(&parent, split(relative).1)
    }
    fn existing_state(&self, relative: &str) -> Result<FileState> {
        let (parent, leaf) = split(relative);
        match self.directories.get(parent) {
            Some(parent) => leaf_state(parent, leaf),
            None => Ok(FileState::Missing),
        }
    }
    fn stamp(&self, relative: &str) -> Result<Option<Stamp>> {
        let (parent, leaf) = split(relative);
        let Some(parent) = self.directories.get(parent) else {
            return Ok(None);
        };
        let Some(metadata) = entry_metadata(parent, leaf)? else {
            return Ok(None);
        };
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err("补全元数据不是普通文件（禁止链接和特殊文件）".into());
        }
        Ok(Some(Stamp {
            dev: metadata.st_dev,
            ino: metadata.st_ino,
            len: metadata.st_size as u64,
            mtime: metadata.st_mtime,
            mtime_nsec: metadata.st_mtime_nsec,
            ctime: metadata.st_ctime,
            ctime_nsec: metadata.st_ctime_nsec,
        }))
    }
    fn snapshot(&mut self, relative: &str, limit: u64) -> Result<FileSnapshot> {
        self.parent(relative, false)?
            .ok_or("实例版本元数据目录缺失")?;
        self.read_existing(relative, limit)
    }
    fn read_existing(&self, relative: &str, limit: u64) -> Result<FileSnapshot> {
        let (parent, leaf) = split(relative);
        let parent = self.directories.get(parent).ok_or("元数据目录未绑定")?;
        let name = cstring(leaf)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(format!(
                "无法读取补全元数据：{}",
                std::io::Error::last_os_error()
            ));
        }
        let mut file = unsafe { fs::File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(error)?;
        if !metadata.is_file() || metadata.len() > limit {
            return Err("补全元数据不是普通文件或超过大小上限".into());
        }
        let stamp = Stamp::of(&metadata);
        let mut bytes = Vec::new();
        (&mut file)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() as u64 > limit || Stamp::of(&file.metadata().map_err(error)?) != stamp {
            return Err("补全元数据读取期间变化或超过大小上限".into());
        }
        Ok(FileSnapshot {
            relative: relative.into(),
            bytes,
            stamp,
        })
    }
}

fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}
fn cstring(raw: &str) -> Result<CString> {
    CString::new(raw).map_err(error)
}
fn entry_metadata(parent: &fs::File, name: &str) -> Result<Option<libc::stat>> {
    let name = cstring(name)?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Ok(Some(unsafe { metadata.assume_init() }));
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(format!("无法检查补全目标：{e}"))
    }
}
fn leaf_state(parent: &fs::File, name: &str) -> Result<FileState> {
    match entry_metadata(parent, name)? {
        None => Ok(FileState::Missing),
        Some(m) if m.st_mode & libc::S_IFMT == libc::S_IFREG => Ok(FileState::Present),
        Some(_) => Err("补全目标不是普通文件（禁止链接和特殊文件）".into()),
    }
}
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn open_directory(parent: &fs::File, name: &str) -> std::io::Result<fs::File> {
    use pcl_core::retained_fs::{Directory, Leaf, Traversal};
    let parent = Directory::from_file(parent.try_clone()?)?;
    let child = if name == "." {
        parent.reopen(Traversal::SameFilesystem)?
    } else {
        parent.child(&Leaf::new(name)?, Traversal::SameFilesystem)?
    };
    child.try_clone_file()
}

/// New bytes use the native staging owner. Linux remains anonymous: no partial
/// artifact or cleanup pathname is exposed. The journalled replacement flow
/// still owns recovery, and an uncertain publication never becomes `false`.
struct RepairStage {
    stage: pcl_core::retained_fs::stage::Stage,
    parent: pcl_core::retained_fs::Directory,
    leaf: pcl_core::retained_fs::Leaf,
    max_bytes: u64,
}
impl RepairStage {
    fn new(parent: Arc<fs::File>, leaf: &str, max_bytes: u64) -> Result<Self> {
        use pcl_core::retained_fs::{stage::Stage, Directory, Leaf};
        let parent = Directory::from_file(parent.try_clone().map_err(error)?).map_err(error)?;
        let leaf = Leaf::new(leaf).map_err(error)?;
        let stage = Stage::new(&parent).map_err(|e| format!("无法安全暂存补全文件：{e}"))?;
        Ok(Self {
            stage,
            parent,
            leaf,
            max_bytes,
        })
    }
    fn file_mut(&mut self) -> &mut fs::File {
        self.stage.file_mut()
    }
    fn publish(self) -> Result<bool> {
        use pcl_core::retained_fs::stage::PublishOutcome;
        let sealed = self
            .stage
            .seal(self.max_bytes)
            .map_err(|e| format!("补全文件封存失败：{e}"))?;
        match sealed
            .publish_new(&self.parent, &self.leaf)
            .map_err(|e| format!("补全文件发布前检查失败：{e}"))?
        {
            PublishOutcome::Published { .. } => Ok(true),
            PublishOutcome::Exists { .. } => {
                // Preserve the missing-only rule: an existing regular file is
                // left alone; links and special entries are still refused.
                let leaf = self.leaf.as_os_str().to_str().ok_or("补全文件名无效")?;
                leaf_state(self.parent.file(), leaf)?;
                Ok(false)
            }
            PublishOutcome::MayHavePublished { error, .. } => {
                // Never roll back or retry an ambiguous namespace commit. The
                // deterministic destination and any existing durable repair
                // records remain for the next inspection/recovery attempt.
                Err(format!("补全文件可能已经发布，但未能完成复核或持久化；文件与修复记录已保留，请重新检查：{error}"))
            }
        }
    }
}

#[derive(Default)]
struct RepairProgress {
    downloaded: u64,
    completed: u64,
    total: u64,
    bytes: u64,
    total_bytes: u64,
    network: u64,
}
impl RepairProgress {
    fn event(&self, stage: &str, message: &str) -> Progress {
        Progress {
            stage: stage.into(),
            message: message.into(),
            completed: self.completed,
            total: self.total,
            bytes_done: self.bytes,
            bytes_total: self.total_bytes,
            network_bytes: self.network,
            steps: [
                ("repair-check", "检查缺失依赖"),
                ("repair-download", "补全缺失文件"),
                ("repair-verify", "复核补全结果"),
            ]
            .into_iter()
            .map(|(id, label)| {
                let index = match stage {
                    "repair-check" => 0,
                    "repair-download" => 1,
                    _ => 2,
                };
                let own = match id {
                    "repair-check" => 0,
                    "repair-download" => 1,
                    _ => 2,
                };
                InstallStep {
                    id: id.into(),
                    label: label.into(),
                    state: if stage == "complete" || own < index {
                        "complete"
                    } else if own == index {
                        "running"
                    } else {
                        "pending"
                    }
                    .into(),
                    progress: Some(if stage == "complete" || own < index {
                        1.0
                    } else if own == 1 {
                        self.completed as f64 / self.total.max(1) as f64
                    } else {
                        0.0
                    }),
                }
            })
            .collect(),
        }
    }
}

fn validate(
    state: &RepairState,
    cancel: &AtomicBool,
    full: &(impl Fn() -> Result<()> + Send + Sync + ?Sized),
    bytes: bool,
) -> Result<()> {
    check(cancel)?;
    full()?;
    // Identity, nanosecond mtime and ctime detect ordinary external writes even
    // when their length and mtime are restored. Full byte comparison brackets
    // the run; rereading a multi-MiB index per object would be quadratic IO.
    if bytes {
        state.verify()?;
    } else {
        state.verify_fast()?;
    }
    check(cancel)
}

impl Installer {
    /// Count currently missing destinations eligible for this same writer.
    /// This does not fetch a missing index or guess its object count, allocate
    /// a temporary inode, or create directories. Admission may therefore skip
    /// an empty download task while retaining the captured filesystem binding.
    pub fn repairable_missing_count(&self, request: &MissingRepair) -> Result<usize> {
        let mut state = request.state.lock().map_err(|_| "缺失补全捕获锁损坏")?;
        state.verify()?;
        let mut files = request.files.clone();
        files.extend(request.index.iter().cloned());
        let mut count = 0;
        for file in manifest::deduplicate(files)? {
            let Some(path) = &file.relative else { continue };
            if !state.filesystem.inspect(path)?.present()
                && self.repair_download_eligibility(&file, &state).is_ok()
            {
                count += 1;
            }
        }
        state.verify()?;
        Ok(count)
    }

    fn repair_download_eligibility(&self, file: &RequiredFile, state: &RepairState) -> Result<()> {
        let download = file.download.as_ref().ok_or_else(|| {
            file.unsupported
                .clone()
                .unwrap_or_else(|| "缺少可信下载声明".into())
        })?;
        self.url(&download.url)?;
        // Publishing a probed-absent child jar would change the frozen launch
        // inheritance choice; both planning and execution reject that target.
        if file
            .relative
            .as_ref()
            .is_some_and(|path| state.jar_probes.get(path) == Some(&false))
        {
            return Err("补全此 JAR 会改变已捕获的继承核心选择".into());
        }
        Ok(())
    }

    /// Caller owns exclusive writer admission. The fast guard checks its cheap
    /// generations during streaming; full binding validation runs at admission,
    /// every publication boundary and completion, outside HTTP scheduling waits.
    pub fn repair_missing(
        &self,
        request: &MissingRepair,
        cancel: &AtomicBool,
        fast_guard: &(impl Fn() -> Result<()> + Send + Sync),
        validate_binding: &(impl Fn() -> Result<()> + Send + Sync),
        on_progress: &(impl Fn(Progress) + Send + Sync),
    ) -> Result<MissingRepairReport> {
        let mut state = request.state.lock().map_err(|_| "缺失补全捕获锁损坏")?;
        validate(&state, cancel, validate_binding, true)?;
        let mut progress = RepairProgress::default();
        on_progress(progress.event("repair-check", "正在检查缺失依赖"));
        let mut files = request.files.clone();
        let mut downloaded_index = None;
        let mut index_enumerated = request.captured_index.is_some();
        let mut published = BTreeSet::new();
        if let Some(index) = &request.index {
            files.push(index.clone());
            if request.captured_index.is_none() {
                if let (Some(relative), Some(download)) = (&index.relative, &index.download) {
                    if state.filesystem.inspect(relative)?.present() {
                        // A repeated attempt may reuse only the index this
                        // same capture previously published and pinned.
                        let snapshot = state
                            .metadata
                            .iter()
                            .find(|s| &s.relative == relative)
                            .ok_or("新出现的资源索引不属于本次捕获")?;
                        files.extend(manifest::asset_objects(&json(&snapshot.bytes)?)?);
                        index_enumerated = true;
                    } else if self.url(&download.url).is_ok() {
                        progress.total = 1;
                        progress.total_bytes = download.size;
                        let mut bytes = Vec::new();
                        self.repair_fetch(
                            download,
                            &state,
                            cancel,
                            fast_guard,
                            &mut progress,
                            on_progress,
                            |chunk| {
                                bytes.extend_from_slice(chunk);
                                Ok(())
                            },
                        )?;
                        validate(&state, cancel, validate_binding, false)?;
                        let value = json(&bytes)?;
                        files.extend(manifest::asset_objects(&value)?);
                        index_enumerated = true;
                        progress.bytes += download.size;
                        downloaded_index = Some((relative.clone(), bytes));
                    }
                }
            }
        }
        files = manifest::deduplicate(files)?;
        let mut missing = Vec::new();
        for file in &mut files {
            let Some(relative) = &file.relative else {
                continue;
            };
            if state.filesystem.inspect(relative)?.present() {
                continue;
            }
            if file.download.is_some() {
                if let Err(reason) = self.repair_download_eligibility(file, &state) {
                    file.download = None;
                    file.unsupported = Some(reason);
                    continue;
                }
                missing.push(file.clone());
            }
        }
        progress.total = missing.len() as u64;
        progress.total_bytes = missing
            .iter()
            .filter_map(|f| f.download.as_ref())
            .try_fold(0u64, |sum, d| {
                sum.checked_add(d.size).ok_or("缺失文件总大小溢出")
            })?;
        if progress.total_bytes > MAX_MISSING_BYTES {
            return Err("缺失下载总大小超过 8 GiB 补全上限".into());
        }
        validate(&state, cancel, validate_binding, false)?;
        for file in &missing {
            let relative = file.relative.as_ref().unwrap();
            let download = file.download.as_ref().unwrap();
            check(cancel)?;
            fast_guard()?;
            state.verify_fast()?;
            if state.filesystem.inspect(relative)?.present() {
                progress.completed += 1;
                progress.bytes += download.size;
                continue;
            }
            let parent = state
                .filesystem
                .parent(relative, true)?
                .ok_or("补全父目录缺失")?;
            let mut temporary = RepairStage::new(parent, split(relative).1, download.size)?;
            if let Some((_, bytes)) = downloaded_index
                .as_ref()
                .filter(|(path, _)| path == relative)
            {
                temporary.file_mut().write_all(bytes).map_err(error)?;
            } else {
                self.repair_fetch(
                    download,
                    &state,
                    cancel,
                    fast_guard,
                    &mut progress,
                    on_progress,
                    |chunk| temporary.file_mut().write_all(chunk).map_err(error),
                )?;
            }
            validate(&state, cancel, validate_binding, false)?;
            if temporary.publish()? {
                progress.downloaded += 1;
                published.insert(relative.clone());
            }
            progress.completed += 1;
            if !downloaded_index
                .as_ref()
                .is_some_and(|(path, _)| path == relative)
            {
                progress.bytes += download.size;
            }
            // The newly downloaded index becomes immutable input for later
            // object publications too; external changes stop the same request.
            if downloaded_index
                .as_ref()
                .is_some_and(|(path, _)| path == relative)
            {
                let snapshot = state
                    .filesystem
                    .snapshot(relative, manifest::MAX_ASSET_INDEX_BYTES)?;
                if snapshot.bytes != downloaded_index.as_ref().unwrap().1 {
                    return Err("资源索引在补全发布期间变化，请重新检查".into());
                }
                state.missing_index = None;
                state.metadata.push(snapshot);
            }
            on_progress(progress.event("repair-download", "正在补全缺失依赖"));
        }
        on_progress(progress.event("repair-verify", "正在复核所需文件存在性"));
        validate(&state, cancel, validate_binding, true)?;
        let mut report = MissingRepairReport {
            id: request.id.clone(),
            files_downloaded: progress.downloaded,
            files_present: 0,
            unsupported: Vec::new(),
            unsupported_total: 0,
        };
        for file in &files {
            let present = match &file.relative {
                Some(path) => state.filesystem.inspect(path)?.present(),
                None => false,
            };
            if present {
                if !file
                    .relative
                    .as_ref()
                    .is_some_and(|path| published.contains(path))
                {
                    report.files_present += 1;
                }
            } else {
                report.unsupported_total += 1;
                if report.unsupported.len() < UNSUPPORTED_LIMIT {
                    report.unsupported.push(RepairUnsupported {
                        path: bounded(&file.label),
                        reason: bounded(
                            file.unsupported
                                .as_deref()
                                .unwrap_or("所需文件仍然缺失，请重新检查"),
                        ),
                    });
                }
            }
        }
        if request.index.is_some() && !index_enumerated {
            report.unsupported_total += 1;
            if report.unsupported.len() < UNSUPPORTED_LIMIT {
                report.unsupported.push(RepairUnsupported {
                    path: "assets/objects".into(),
                    reason: "资源索引缺失且无法可信补全，未能枚举资源对象".into(),
                });
            }
        }
        validate(&state, cancel, validate_binding, true)?;
        on_progress(progress.event("complete", "缺失文件补全检查完成"));
        Ok(report)
    }

    fn repair_fetch(
        &self,
        download: &manifest::FileDownload,
        state: &RepairState,
        cancel: &AtomicBool,
        fast_guard: &(impl Fn() -> Result<()> + Send + Sync),
        progress: &mut RepairProgress,
        on_progress: &(impl Fn(Progress) + Send + Sync),
        mut consume: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        let url = self.url(&download.url)?;
        // Object URLs are constructed by the pure core enumerator. Loopback
        // fixtures replace that official origin only in installer unit tests.
        #[cfg(test)]
        let url = if url.host_str() == Some("resources.download.minecraft.net") {
            match &self.endpoint {
                Some(endpoint) => {
                    let mut local = Url::parse(endpoint).map_err(error)?;
                    local.set_path(url.path());
                    self.url(local.as_str())?
                }
                None => url,
            }
        } else {
            url
        };
        let mut digest = Sha1::new();
        let mut received = 0u64;
        let mut last_check = Instant::now() - Duration::from_secs(1);
        // Reuse the installer's captured queue, pacing and response cancellation.
        // Failed attempts leave no named partial and can be explicitly retried.
        self.transfer(&url, download.size, cancel, |chunk| {
            fast_guard()?;
            if last_check.elapsed() >= Duration::from_millis(100) {
                state.verify_fast()?;
                last_check = Instant::now();
            }
            consume(chunk)?;
            digest.update(chunk);
            received += chunk.len() as u64;
            progress.network += chunk.len() as u64;
            let mut event = progress.event("repair-download", "正在下载缺失依赖");
            event.bytes_done = progress.bytes + received;
            on_progress(event);
            Ok(())
        })?;
        if received != download.size || format!("{:x}", digest.finalize()) != download.sha1 {
            return Err("缺失文件 SHA1 或大小校验失败".into());
        }
        check(cancel)?;
        fast_guard()
    }
}
fn bounded(raw: &str) -> String {
    if raw.len() <= TEXT_LIMIT {
        return raw.into();
    }
    let mut end = TEXT_LIMIT;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &raw[..end])
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;
