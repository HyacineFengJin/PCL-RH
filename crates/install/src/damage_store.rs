//! Durable, append-only per-file replacement records. An intent is synced before
//! moving a target. Commit/rollback markers are immutable and name that exact
//! intent's SHA256. Recovery infers interrupted renames from held inode identity
//! and content, never overwrites a destination, and retains all original copies.
use super::*;
use sha2::Sha256;
use std::io::{Seek, SeekFrom};

pub(super) const STORE: &str = ".pcl-linux/dependency-repairs";
const MAX_OPERATIONS: usize = 4096;
const MAX_RECORD_BYTES: u64 = 64 * 1024;
const MAX_RECORDS: usize = 10_000;
const MAX_RECORD_TOTAL: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    dev: u64,
    ino: u64,
}
impl Key {
    fn of(file: &fs::File) -> Result<Self> {
        let m = file.metadata().map_err(error)?;
        if !m.is_dir() {
            return Err("损坏修复目录不是普通目录".into());
        }
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Fingerprint {
    pub stamp: Stamp,
    pub sha1: String,
    pub sha256: String,
}
impl Fingerprint {
    fn same_object(&self, other: &Self) -> bool {
        self.stamp.dev == other.stamp.dev
            && self.stamp.ino == other.stamp.ino
            && self.stamp.len == other.stamp.len
            && self.sha256 == other.sha256
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Parent {
    path: String,
    key: Key,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema: u32,
    operation_id: String,
    instance_id: String,
    root_display: PathBuf,
    root: Key,
    store: Key,
    operation: Key,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    ordinal: usize,
    path: String,
    parents: Vec<Parent>,
    original: Fingerprint,
    incoming: Fingerprint,
    backup: Fingerprint,
}

pub(super) struct Store {
    filesystem: RepairFilesystem,
    directory: Arc<fs::File>,
}
pub(super) struct Operation {
    directory: Arc<fs::File>,
    header: Header,
    header_hash: String,
    missing_header: bool,
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn open_regular(parent: &fs::File, name: &str) -> Result<fs::File> {
    use pcl_core::retained_fs::{Directory, Leaf, Links};
    let read = || -> std::io::Result<fs::File> {
        Directory::from_file(parent.try_clone()?)?.open_regular(&Leaf::new(name)?, Links::Single)
    };
    read().map_err(|e| format!("无法安全读取损坏修复记录：{e}"))
}
fn regular(parent: &fs::File, name: &str) -> Result<fs::File> {
    open_regular(parent, name)
}
fn read(parent: &fs::File, name: &str, limit: u64) -> Result<Vec<u8>> {
    let mut file = regular(parent, name)?;
    let before = Stamp::of(&file.metadata().map_err(error)?);
    if before.len > limit {
        return Err("损坏修复记录超过大小上限".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 != before.len || Stamp::of(&file.metadata().map_err(error)?) != before {
        return Err("损坏修复记录读取期间变化".into());
    }
    let current = regular(parent, name)?;
    if Stamp::of(&current.metadata().map_err(error)?) != before {
        return Err("损坏修复记录身份变化".into());
    }
    Ok(bytes)
}
fn put(parent: Arc<fs::File>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut temporary = AnonymousFile::new(parent.clone(), name)?;
    temporary.file.write_all(bytes).map_err(error)?;
    if !temporary.publish()? {
        return Err("损坏修复记录已存在，拒绝覆盖".into());
    }
    Ok(())
}
fn marker(parent: Arc<fs::File>, name: &str, expected: &str) -> Result<()> {
    if entry_metadata(&parent, name)?.is_some() {
        if read(&parent, name, 128)? == expected.as_bytes() {
            return Ok(());
        }
        return Err("损坏修复标记内容冲突，原文件与备份已保留".into());
    }
    put(parent, name, expected.as_bytes())
}
fn marked(parent: &fs::File, name: &str, expected: &str) -> Result<bool> {
    if entry_metadata(parent, name)?.is_none() {
        return Ok(false);
    }
    if read(parent, name, 128)? != expected.as_bytes() {
        return Err("损坏修复标记内容无效，备份已保留".into());
    }
    Ok(true)
}
fn names(directory: &fs::File, limit: usize) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for item in fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd())).map_err(error)? {
        if result.len() >= limit {
            return Err("损坏修复记录数量超过安全上限".into());
        }
        result.push(
            item.map_err(error)?
                .file_name()
                .into_string()
                .map_err(|_| "损坏修复记录名称不是 UTF-8")?,
        );
    }
    result.sort();
    Ok(result)
}
fn operation_name(name: &str) -> bool {
    name.starts_with("d-")
        && name.len() <= 80
        && name.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn path_allowed(path: &str) -> Result<()> {
    if manifest::relative_path(path)? != path {
        return Err("损坏修复目标路径无效".into());
    }
    let parts: Vec<_> = path.split('/').collect();
    let allowed = match parts.as_slice() {
        ["libraries", ..] if parts.len() >= 2 => true,
        ["versions", id, leaf] => *leaf == format!("{id}.jar"),
        ["assets", "indexes", _] | ["assets", "log_configs", _] => true,
        ["assets", "objects", prefix, hash] => {
            hash.len() == 40
                && hash.bytes().all(|b| b.is_ascii_hexdigit())
                && prefix.len() == 2
                && &hash[..2] == *prefix
        }
        _ => false,
    };
    if !allowed {
        return Err("损坏修复记录指向不允许替换的文件".into());
    }
    Ok(())
}

pub(super) fn fingerprint(
    file: &mut fs::File,
    poll: &dyn Fn() -> Result<()>,
) -> Result<Fingerprint> {
    let m = file.metadata().map_err(error)?;
    if !m.is_file() || m.nlink() > 1 || m.len() > manifest::MAX_REPAIR_FILE_BYTES {
        return Err("损坏文件不是独占普通文件或超过 512 MiB 备份上限".into());
    }
    let stamp = Stamp::of(&m);
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut received = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    while received < stamp.len {
        poll()?;
        let max = (stamp.len - received).min(buffer.len() as u64) as usize;
        let n = file.read(&mut buffer[..max]).map_err(error)?;
        if n == 0 {
            return Err("损坏文件读取期间变化".into());
        }
        sha1.update(&buffer[..n]);
        sha256.update(&buffer[..n]);
        received += n as u64;
    }
    poll()?;
    if Stamp::of(&file.metadata().map_err(error)?) != stamp {
        return Err("损坏文件读取期间变化".into());
    }
    Ok(Fingerprint {
        stamp,
        sha1: format!("{:x}", sha1.finalize()),
        sha256: format!("{:x}", sha256.finalize()),
    })
}

impl Store {
    pub fn open(root: fs::File, display: &Path, create: bool, lock: bool) -> Result<Option<Self>> {
        let mut filesystem = RepairFilesystem::new(root, display)?;
        let Some(directory) = filesystem.parent(&format!("{STORE}/sentinel"), create)? else {
            return Ok(None);
        };
        if lock && unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
        {
            return Err("另一个进程正在修复损坏依赖，请稍后重试".into());
        }
        // Persist every newly created ancestor before any target can move.
        if create {
            for directory in filesystem.directories.values() {
                directory.sync_all().map_err(error)?;
            }
        }
        filesystem.verify()?;
        Ok(Some(Self {
            filesystem,
            directory,
        }))
    }
    pub fn verify(&self) -> Result<()> {
        self.filesystem.verify()
    }
    fn verify_operation(&self, operation: &Operation) -> Result<()> {
        self.verify()?;
        let actual =
            entry_metadata(&self.directory, operation.id())?.ok_or("损坏修复记录目录已移除")?;
        if actual.st_mode & libc::S_IFMT != libc::S_IFDIR
            || actual.st_dev != operation.header.operation.dev
            || actual.st_ino != operation.header.operation.ino
        {
            return Err("损坏修复记录目录已替换，已保留所有文件".into());
        }
        Ok(())
    }
    pub fn operations(&self) -> Result<Vec<Operation>> {
        self.verify()?;
        let mut result = Vec::new();
        for name in names(&self.directory, MAX_OPERATIONS)? {
            if !operation_name(&name) {
                return Err("存在无法识别的损坏修复记录，已保留所有文件".into());
            }
            let directory = Arc::new(open_directory(&self.directory, &name).map_err(error)?);
            if entry_metadata(&directory, "header.json")?.is_none() {
                if !names(&directory, MAX_RECORDS * 6 + 2)?.is_empty() {
                    return Err("未初始化的损坏修复目录包含未知文件，已保留所有内容".into());
                }
                let header = Header {
                    schema: 1,
                    operation_id: name,
                    instance_id: "recovery".into(),
                    root_display: self.filesystem.display.clone(),
                    root: Key::of(&self.filesystem.directories[""])?,
                    store: Key::of(&self.directory)?,
                    operation: Key::of(&directory)?,
                };
                let header_hash = sha256(&serde_json::to_vec(&header).map_err(error)?);
                result.push(Operation {
                    directory,
                    header,
                    header_hash,
                    missing_header: true,
                });
                continue;
            }
            let bytes = read(&directory, "header.json", MAX_RECORD_BYTES)?;
            let header: Header =
                serde_json::from_slice(&bytes).map_err(|_| "损坏修复记录无效，备份已保留")?;
            if header.schema != 1
                || header.operation_id != name
                || header.root_display != self.filesystem.display
                || header.root != Key::of(&self.filesystem.directories[""])?
                || header.store != Key::of(&self.directory)?
                || header.operation != Key::of(&directory)?
            {
                return Err("损坏修复记录目录身份不符，备份已保留".into());
            }
            pcl_core::identifier(&header.instance_id)?;
            let operation = Operation {
                directory,
                header,
                header_hash: sha256(&bytes),
                missing_header: false,
            };
            // Archived operations keep their backups, but must not retain one
            // descriptor per historical run during a readiness check.
            if !operation.done()? {
                result.push(operation);
            }
        }
        self.verify()?;
        Ok(result)
    }
    pub fn begin(&mut self, name: &str, id: &str) -> Result<Operation> {
        if !operation_name(name) {
            return Err("损坏修复操作标识无效".into());
        }
        if entry_metadata(&self.directory, name)?.is_some() {
            return Err("损坏修复操作记录已存在".into());
        }
        let directory = self
            .filesystem
            .parent(&format!("{STORE}/{name}/header.json"), true)?
            .ok_or("无法创建损坏修复记录")?;
        self.directory.sync_all().map_err(error)?;
        let header = Header {
            schema: 1,
            operation_id: name.into(),
            instance_id: id.into(),
            root_display: self.filesystem.display.clone(),
            root: Key::of(&self.filesystem.directories[""])?,
            store: Key::of(&self.directory)?,
            operation: Key::of(&directory)?,
        };
        let bytes = serde_json::to_vec(&header).map_err(error)?;
        put(directory.clone(), "header.json", &bytes)?;
        self.verify()?;
        Ok(Operation {
            directory,
            header,
            header_hash: sha256(&bytes),
            missing_header: false,
        })
    }
    fn target(&mut self, intent: &Intent) -> Result<Arc<fs::File>> {
        self.verify()?;
        path_allowed(&intent.path)?;
        let directory = self
            .filesystem
            .parent(&intent.path, false)?
            .ok_or("损坏修复目标目录已缺失")?;
        let (parent, _) = split(&intent.path);
        let mut expected_paths = Vec::new();
        let mut prefix = String::new();
        for name in parent.split('/').filter(|p| !p.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            expected_paths.push(prefix.clone());
        }
        if expected_paths.len() != intent.parents.len() {
            return Err("损坏修复目录记录层级无效".into());
        }
        for (expected, record) in expected_paths.iter().zip(&intent.parents) {
            if expected != &record.path
                || self
                    .filesystem
                    .directories
                    .get(expected)
                    .map(|file| Key::of(file))
                    .transpose()?
                    != Some(record.key)
            {
                return Err("损坏修复目标目录已替换，备份已保留".into());
            }
        }
        Ok(directory)
    }
}

impl Operation {
    pub fn id(&self) -> &str {
        &self.header.operation_id
    }
    pub fn directory(&self) -> Arc<fs::File> {
        self.directory.clone()
    }
    pub fn done(&self) -> Result<bool> {
        if self.missing_header {
            Ok(false)
        } else {
            marked(&self.directory, "done", &self.header_hash)
        }
    }
    pub fn finish(&self) -> Result<()> {
        marker(self.directory.clone(), "done", &self.header_hash)
    }
    pub fn prepare(
        &self,
        ordinal: usize,
        path: &str,
        filesystem: &RepairFilesystem,
        original: Fingerprint,
        incoming: Fingerprint,
        backup: Fingerprint,
    ) -> Result<Intent> {
        if ordinal >= MAX_RECORDS {
            return Err("单次损坏修复超过 10000 个文件上限".into());
        }
        path_allowed(path)?;
        let (parent, _) = split(path);
        let mut parents = Vec::new();
        let mut prefix = String::new();
        for name in parent.split('/').filter(|p| !p.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            let file = filesystem
                .directories
                .get(&prefix)
                .ok_or("损坏修复目标目录未绑定")?;
            parents.push(Parent {
                path: prefix.clone(),
                key: Key::of(file)?,
            });
        }
        let intent = Intent {
            ordinal,
            path: path.into(),
            parents,
            original,
            incoming,
            backup,
        };
        let bytes = serde_json::to_vec(&intent).map_err(error)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err("损坏修复目标记录过大".into());
        }
        put(
            self.directory.clone(),
            &format!("intent-{ordinal}.json"),
            &bytes,
        )?;
        Ok(intent)
    }
    pub fn commit(&self, intent: &Intent) -> Result<()> {
        marker(
            self.directory.clone(),
            &format!("commit-{}", intent.ordinal),
            &sha256(&serde_json::to_vec(intent).map_err(error)?),
        )
    }
    fn rolled_back(&self, intent: &Intent) -> Result<()> {
        marker(
            self.directory.clone(),
            &format!("rollback-{}", intent.ordinal),
            &sha256(&serde_json::to_vec(intent).map_err(error)?),
        )
    }
    fn intents(&self) -> Result<Vec<Intent>> {
        let mut result = Vec::new();
        let mut total = 0;
        let mut original_bytes = 0u64;
        let mut incoming_bytes = 0u64;
        for name in names(&self.directory, MAX_RECORDS * 6 + 2)? {
            if name == "header.json" || name == "done" {
                continue;
            }
            let number = name
                .strip_prefix("intent-")
                .and_then(|n| n.strip_suffix(".json"));
            if let Some(number) = number {
                let ordinal = number
                    .parse::<usize>()
                    .map_err(|_| "损坏修复记录序号无效")?;
                if ordinal >= MAX_RECORDS || number != ordinal.to_string() {
                    return Err("损坏修复记录序号无效".into());
                }
                let bytes = read(&self.directory, &name, MAX_RECORD_BYTES)?;
                total += bytes.len();
                if total > MAX_RECORD_TOTAL {
                    return Err("损坏修复记录总量超过安全上限".into());
                }
                let intent: Intent =
                    serde_json::from_slice(&bytes).map_err(|_| "损坏修复文件记录损坏")?;
                if intent.ordinal != ordinal {
                    return Err("损坏修复文件记录序号不符".into());
                }
                path_allowed(&intent.path)?;
                if intent.parents.len() > 64 {
                    return Err("损坏修复目录记录层级超过上限".into());
                }
                for fingerprint in [&intent.original, &intent.incoming, &intent.backup] {
                    if fingerprint.stamp.len > manifest::MAX_REPAIR_FILE_BYTES
                        || !valid_hash(&fingerprint.sha1, 40)
                        || !valid_hash(&fingerprint.sha256, 64)
                    {
                        return Err("损坏修复文件指纹或大小记录无效".into());
                    }
                }
                if intent.backup.sha256 != intent.original.sha256
                    || intent.backup.stamp.len != intent.original.stamp.len
                    || (intent.backup.stamp.dev == intent.original.stamp.dev
                        && intent.backup.stamp.ino == intent.original.stamp.ino)
                {
                    return Err("损坏修复记录不包含独立原件备份".into());
                }
                original_bytes = original_bytes
                    .checked_add(intent.original.stamp.len)
                    .ok_or("恢复原件大小溢出")?;
                incoming_bytes = incoming_bytes
                    .checked_add(intent.incoming.stamp.len)
                    .ok_or("恢复替换大小溢出")?;
                if original_bytes > MAX_MISSING_BYTES || incoming_bytes > MAX_MISSING_BYTES {
                    return Err("损坏修复恢复总输入超过 8 GiB 上限".into());
                }
                result.push(intent);
            } else {
                let allowed = ["backup-", "incoming-", "original-", "commit-", "rollback-"]
                    .iter()
                    .any(|prefix| {
                        name.strip_prefix(prefix).is_some_and(|n| {
                            n.parse::<usize>()
                                .is_ok_and(|i| i < MAX_RECORDS && i.to_string() == n)
                        })
                    });
                if !allowed {
                    return Err("损坏修复目录存在未知文件，已保留所有内容".into());
                }
            }
        }
        result.sort_by_key(|record| record.ordinal);
        Ok(result)
    }
}

fn valid_hash(hash: &str, length: usize) -> bool {
    hash.len() == length && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Healthy launch only needs to know that no transaction store exists. This
/// no-create probe uses ordinary no-follow FDs and rechecks directory identity;
/// kernels/filesystems without the repair engine's openat2 support remain able
/// to launch an otherwise complete installation.
pub(super) fn store_absent(path: &Path) -> Result<bool> {
    if !path.is_absolute() {
        return Err("损坏修复根目录必须为绝对路径".into());
    }
    let raw = path.to_str().ok_or("损坏修复根目录不是 UTF-8")?;
    let fd = unsafe {
        libc::open(
            cstring(raw)?.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(error(std::io::Error::last_os_error()));
    }
    let root = unsafe { fs::File::from_raw_fd(fd) };
    let expected = Key::of(&root)?;
    let verify_root = || -> Result<()> {
        let current = fs::symlink_metadata(path).map_err(error)?;
        if !current.is_dir() || current.dev() != expected.dev || current.ino() != expected.ino {
            return Err("损坏修复根目录身份已变化".into());
        }
        Ok(())
    };
    verify_root()?;
    let Some(parent) = entry_metadata(&root, ".pcl-linux")? else {
        verify_root()?;
        if entry_metadata(&root, ".pcl-linux")?.is_some() {
            return Err("损坏修复记录父目录在检查期间出现，请重试".into());
        }
        return Ok(true);
    };
    if parent.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err("损坏修复记录父目录不是安全目录".into());
    }
    let fd = unsafe {
        libc::openat(
            root.as_raw_fd(),
            cstring(".pcl-linux")?.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(error(std::io::Error::last_os_error()));
    }
    let directory = unsafe { fs::File::from_raw_fd(fd) };
    let bound = Key::of(&directory)?;
    if bound.dev != parent.st_dev || bound.ino != parent.st_ino {
        return Err("损坏修复记录父目录在检查期间变化".into());
    }
    let store = entry_metadata(&directory, "dependency-repairs")?;
    let current = entry_metadata(&root, ".pcl-linux")?.ok_or("损坏修复记录父目录已移除")?;
    verify_root()?;
    if current.st_mode & libc::S_IFMT != libc::S_IFDIR
        || current.st_dev != bound.dev
        || current.st_ino != bound.ino
    {
        return Err("损坏修复记录父目录在检查期间变化".into());
    }
    match store {
        None => {
            if entry_metadata(&directory, "dependency-repairs")?.is_some() {
                return Err("损坏修复记录在检查期间出现，请重试".into());
            }
            Ok(true)
        }
        Some(metadata) if metadata.st_mode & libc::S_IFMT == libc::S_IFDIR => Ok(false),
        Some(_) => Err("损坏修复记录不是安全目录".into()),
    }
}

fn rename_new(
    source: &fs::File,
    source_name: &str,
    target: &fs::File,
    target_name: &str,
) -> Result<()> {
    pcl_core::file_ops::rename_noreplace_at(
        source,
        std::ffi::OsStr::new(source_name),
        target,
        std::ffi::OsStr::new(target_name),
    )
    .map_err(|e| format!("无法无覆盖移动损坏修复文件：{e}"))?;
    source.sync_all().map_err(error)?;
    target.sync_all().map_err(error)
}
fn matches(parent: &fs::File, name: &str, expected: &Fingerprint) -> Result<bool> {
    if entry_metadata(parent, name)?.is_none() {
        return Ok(false);
    }
    let mut file = regular(parent, name)?;
    Ok(expected.same_object(&fingerprint(&mut file, &|| Ok(()))?))
}

pub(super) fn publish(
    store: &mut Store,
    operation: &Operation,
    intent: &Intent,
    before_commit: &dyn Fn() -> Result<()>,
) -> Result<()> {
    store.verify_operation(operation)?;
    let target = store.target(intent)?;
    let leaf = split(&intent.path).1;
    let incoming = format!("incoming-{}", intent.ordinal);
    let original = format!("original-{}", intent.ordinal);
    let backup = format!("backup-{}", intent.ordinal);
    let current = regular(&target, leaf)?;
    if Stamp::of(&current.metadata().map_err(error)?) != intent.original.stamp
        || !matches(&target, leaf, &intent.original)?
        || !matches(&operation.directory, &incoming, &intent.incoming)?
        || !matches(&operation.directory, &backup, &intent.backup)?
    {
        return Err("损坏文件或修复暂存内容已变化，拒绝替换".into());
    }
    rename_new(&target, leaf, &operation.directory, &original)?;
    if !matches(&operation.directory, &original, &intent.original)? {
        return Err("原文件在移动期间变化，已保留原件并等待恢复".into());
    }
    before_commit()?;
    store.verify_operation(operation)?;
    rename_new(&operation.directory, &incoming, &target, leaf)?;
    if !matches(&target, leaf, &intent.incoming)?
        || !matches(&operation.directory, &original, &intent.original)?
        || !matches(&operation.directory, &backup, &intent.backup)?
    {
        return Err("损坏修复发布期间内容变化，等待恢复".into());
    }
    before_commit()?;
    store.verify_operation(operation)?;
    operation.commit(intent)
}

fn rollback(store: &mut Store, operation: &Operation, intent: &Intent) -> Result<()> {
    store.verify_operation(operation)?;
    let hash = sha256(&serde_json::to_vec(intent).map_err(error)?);
    if marked(
        &operation.directory,
        &format!("commit-{}", intent.ordinal),
        &hash,
    )? || marked(
        &operation.directory,
        &format!("rollback-{}", intent.ordinal),
        &hash,
    )? {
        return Ok(());
    }
    let target = store.target(intent)?;
    let leaf = split(&intent.path).1;
    let incoming = format!("incoming-{}", intent.ordinal);
    let original = format!("original-{}", intent.ordinal);
    if !matches(
        &operation.directory,
        &format!("backup-{}", intent.ordinal),
        &intent.backup,
    )? {
        return Err("独立原件备份已变化，停止恢复并保留所有文件".into());
    }
    let original_here = matches(&target, leaf, &intent.original)?;
    let original_saved = matches(&operation.directory, &original, &intent.original)?;
    let replacement_here = matches(&target, leaf, &intent.incoming)?;
    let replacement_saved = matches(&operation.directory, &incoming, &intent.incoming)?;
    if original_here && !original_saved && replacement_saved {
        return operation.rolled_back(intent);
    }
    if !original_saved || original_here || (replacement_here == replacement_saved) {
        return Err("恢复目标存在并发改动或文件身份冲突，原件与备份均已保留".into());
    }
    if replacement_here {
        rename_new(&target, leaf, &operation.directory, &incoming)?;
        if !matches(&operation.directory, &incoming, &intent.incoming)? {
            return Err("恢复时目标发生并发改动，已保留移动内容".into());
        }
    } else if entry_metadata(&target, leaf)?.is_some() {
        return Err("恢复目标已由其他文件占用，拒绝覆盖；原件与备份均已保留".into());
    }
    store.verify_operation(operation)?;
    rename_new(&operation.directory, &original, &target, leaf)?;
    if !matches(&target, leaf, &intent.original)? {
        return Err("原件恢复后内容变化，请保留所有修复记录".into());
    }
    operation.rolled_back(intent)
}

pub(super) fn recover(
    store: &mut Store,
    operation: &Operation,
    poll: &dyn Fn() -> Result<()>,
) -> Result<()> {
    store.verify_operation(operation)?;
    if operation.missing_header {
        if !names(&operation.directory, MAX_RECORDS * 6 + 2)?.is_empty() {
            return Err("未初始化的修复目录变化，已保留所有文件".into());
        }
        put(
            operation.directory.clone(),
            "header.json",
            &serde_json::to_vec(&operation.header).map_err(error)?,
        )?;
    }
    if operation.done()? {
        return Ok(());
    }
    for intent in operation.intents()? {
        poll()?;
        rollback(store, operation, &intent)?;
    }
    store.verify_operation(operation)?;
    operation.finish()
}

pub(super) fn open_root(path: &Path) -> Result<fs::File> {
    if !path.is_absolute() {
        return Err("损坏修复根目录必须为绝对路径".into());
    }
    let slash = cstring("/")?;
    let fd = unsafe {
        libc::open(
            slash.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(error(std::io::Error::last_os_error()));
    }
    let mut directory = unsafe { fs::File::from_raw_fd(fd) };
    for component in path.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => {
                let name = name.to_str().ok_or("损坏修复根目录不是 UTF-8")?;
                let how = OpenHow {
                    flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                        as u64,
                    mode: 0,
                    resolve: 0x04 | 0x08,
                };
                let fd = unsafe {
                    libc::syscall(
                        libc::SYS_openat2,
                        directory.as_raw_fd(),
                        cstring(name)?.as_ptr(),
                        &how,
                        std::mem::size_of::<OpenHow>(),
                    )
                };
                if fd < 0 {
                    return Err(format!(
                        "无法安全打开损坏修复根目录：{}",
                        std::io::Error::last_os_error()
                    ));
                }
                directory = unsafe { fs::File::from_raw_fd(fd as i32) };
            }
            _ => return Err("损坏修复根目录路径无效".into()),
        }
    }
    Ok(directory)
}
