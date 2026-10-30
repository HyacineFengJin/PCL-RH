//! Private installs retain directory capabilities through each write. Proc paths
//! are only syscall adapters for libraries accepting paths; they refer to held
//! descriptors in the launcher process, including while Java children execute.
//! A scoped Landlock worker confines processor-derived destination paths to the
//! captured private inode. This is pathname confinement, not isolation from
//! other processes under the same user: a concurrent external hard-link alias
//! can still share an inode. Archive links are rejected; captured official
//! processors run before pack overrides. Neither restriction changes the caller
//! or legacy install API.
//!
//! Bound streams, native extraction and private-workspace cleanup use retained
//! directories directly. Java's path adapters and processor confinement still
//! need native backends outside Linux.
use super::*;
use pcl_core::retained_fs::stage::{PublishOutcome, Stage};
use pcl_core::retained_fs::{Directory, Kind, Leaf, Links, Traversal, WorkspaceRemoval};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};

#[derive(Clone)]
pub(super) struct InstallDir {
    path: Option<PathBuf>,
    fd: Option<Arc<fs::File>>,
}
#[derive(Clone)]
pub(super) struct InstallPath {
    path: Option<PathBuf>,
    relative: PathBuf,
    // Native streams use this literal leaf and held parent. `path` remains an
    // adapter only for consumers not yet migrated away from path-only APIs.
    bound: Option<BoundLeaf>,
}
#[derive(Clone)]
struct BoundLeaf {
    parent: Directory,
    name: Leaf,
}

/// A newly generated/downloaded file. Bound output never turns a proc adapter
/// into write authority; it owns a native stage and the literal final leaf.
pub(super) struct InstallOutput {
    inner: OutputInner,
    max_bytes: u64,
}
enum OutputInner {
    Bound {
        stage: Stage,
        target: BoundLeaf,
    },
    Legacy {
        file: tempfile::NamedTempFile,
        target: PathBuf,
    },
}

impl InstallOutput {
    pub fn file_mut(&mut self) -> &mut fs::File {
        match &mut self.inner {
            OutputInner::Bound { stage, .. } => stage.file_mut(),
            OutputInner::Legacy { file, .. } => file.as_file_mut(),
        }
    }

    pub fn publish(self) -> Result<()> {
        match self.inner {
            OutputInner::Bound { stage, target } => {
                let sealed = match stage.seal(self.max_bytes) {
                    Ok(sealed) => sealed,
                    Err(failed) => {
                        return Err(close_stage(
                            failed.stage,
                            format!("无法封存安装暂存文件：{}", failed.error),
                        ))
                    }
                };
                match sealed.publish_new(&target.parent, &target.name) {
                    Ok(PublishOutcome::Published { .. }) => Ok(()),
                    Ok(PublishOutcome::Exists { stage }) => {
                        let residue = stage.source_name().map(|name| name.as_os_str().to_owned());
                        drop(stage);
                        Err(with_residue("安装目标已存在，未覆盖原文件".into(), residue))
                    }
                    Ok(PublishOutcome::MayHavePublished {
                        stage,
                        receipt,
                        error,
                    }) => {
                        // Keep the actual receipt in the error: callers must
                        // reconcile it, never infer that Err means no commit.
                        // Dropping only releases the held handle; no target or
                        // named source is removed to manufacture rollback.
                        let evidence = serde_json::to_string(&receipt)
                            .unwrap_or_else(|encoding| format!("发布证据编码失败：{encoding}"));
                        drop(stage);
                        Err(format!("安装发布结果不确定，可能已发布，禁止自动重试：{error}；发布证据：{evidence}"))
                    }
                    Err(failed) => {
                        let residue = failed
                            .stage
                            .source_name()
                            .map(|name| name.as_os_str().to_owned());
                        drop(failed.stage);
                        Err(with_residue(
                            format!("无法发布安装暂存文件：{}", failed.error),
                            residue,
                        ))
                    }
                }
            }
            OutputInner::Legacy { file, target } => {
                let checked = (|| {
                    if file.as_file().metadata().map_err(error)?.len() > self.max_bytes {
                        return Err("安装暂存文件超过允许大小".to_string());
                    }
                    file.as_file().sync_all().map_err(error)
                })();
                if let Err(original) = checked {
                    return Err(close_legacy(file, original));
                }
                match file.persist(target) {
                    Ok(_) => Ok(()),
                    Err(failed) => Err(close_legacy(failed.file, error(failed.error))),
                }
            }
        }
    }

    pub fn discard(self) -> Result<()> {
        match self.inner {
            OutputInner::Bound { stage, .. } => discard_stage(stage),
            OutputInner::Legacy { file, .. } => file
                .close()
                .map_err(|cleanup| format!("取消清理失败：无法移除未完成文件：{cleanup}")),
        }
    }

    pub fn close(self, original: String) -> String {
        match self.discard() {
            Ok(()) => original,
            Err(cleanup) => format!("{cleanup}；原错误：{original}"),
        }
    }
}

fn close_legacy(file: tempfile::NamedTempFile, original: String) -> String {
    match file.close() {
        Ok(()) => original,
        Err(cleanup) => format!("取消清理失败：无法移除未完成文件：{cleanup}；原错误：{original}"),
    }
}
fn discard_stage(stage: Stage) -> Result<()> {
    let residue = stage.source_name().map(|name| name.as_os_str().to_owned());
    drop(stage);
    match residue {
        None => Ok(()), // Anonymous kernel storage is reclaimed on last close.
        Some(name) => Err(format!(
            "取消清理失败：命名暂存未自动删除，已保留待恢复：{}",
            name.to_string_lossy()
        )),
    }
}
fn close_stage(stage: Stage, original: String) -> String {
    match discard_stage(stage) {
        Ok(()) => original,
        Err(cleanup) => format!("{cleanup}；原错误：{original}"),
    }
}
fn with_residue(original: String, residue: Option<std::ffi::OsString>) -> String {
    match residue {
        Some(name) => format!(
            "{original}；命名暂存已保留待恢复：{}",
            name.to_string_lossy()
        ),
        None => original,
    }
}
impl InstallPath {
    #[cfg(test)]
    pub(super) fn redirect_adapter_for_test(&mut self, path: PathBuf) {
        self.path = Some(path);
    }
    pub fn process_path(&self) -> Result<&Path> {
        self.path
            .as_deref()
            .ok_or_else(|| "此平台尚无保留句柄的 Java 路径适配器".into())
    }
    pub fn relative(&self) -> &Path {
        &self.relative
    }
    pub fn prepare_parent(&self) -> Result<()> {
        if let Some(bound) = &self.bound {
            let parent = bound.parent.metadata().map_err(error)?;
            if parent.kind != Kind::Directory || parent.reparse_tag.is_some() {
                return Err("安装目标父目录不是原生普通目录".into());
            }
            Ok(())
        } else {
            fs::create_dir_all(self.process_path()?.parent().ok_or("安装目标父目录无效")?)
                .map_err(error)
        }
    }
    pub fn open_optional(&self) -> Result<Option<fs::File>> {
        if let Some(bound) = &self.bound {
            let Some(observed) = bound.parent.entry(&bound.name).map_err(error)? else {
                return Ok(None);
            };
            if observed.kind != Kind::Regular
                || observed.links != 1
                || observed.reparse_tag.is_some()
            {
                return Err("安装文件不是独占普通文件".into());
            }
            let file = bound
                .parent
                .open_regular(&bound.name, Links::Single)
                .map_err(error)?;
            let held = pcl_core::retained_fs::metadata(&file).map_err(error)?;
            if !held.same_object(&observed) {
                return Err("安装文件在打开期间已替换，保留待恢复".into());
            }
            bound
                .parent
                .recheck(&bound.name, &held.identity)
                .map_err(error)?;
            return Ok(Some(file));
        }
        match fs::File::open(self.process_path()?) {
            Ok(file) if file.metadata().map_err(error)?.is_file() => Ok(Some(file)),
            Ok(_) => Err("安装文件不是普通文件".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(error(e)),
        }
    }
    pub fn output(&self, max_bytes: u64) -> Result<InstallOutput> {
        let inner = match &self.bound {
            Some(target) => OutputInner::Bound {
                // Named native stage parents must already be private. Stage
                // reports unsupported/broad parents; never chmod or fall back
                // to NamedTempFile through this path adapter.
                stage: Stage::new(&target.parent)
                    .map_err(|e| format!("无法建立原生安装暂存：{e}"))?,
                target: target.clone(),
            },
            None => {
                self.prepare_parent()?;
                let parent = self.process_path()?.parent().ok_or("安装目标父目录无效")?;
                OutputInner::Legacy {
                    file: tempfile::NamedTempFile::new_in(parent).map_err(error)?,
                    target: self.process_path()?.to_path_buf(),
                }
            }
        };
        Ok(InstallOutput { inner, max_bytes })
    }
    pub fn open(&self) -> Result<fs::File> {
        if let Some(bound) = &self.bound {
            return bound
                .parent
                .open_regular(&bound.name, Links::Single)
                .map_err(error);
        }
        let file = fs::File::open(self.process_path()?).map_err(error)?;
        let metadata = file.metadata().map_err(error)?;
        if !metadata.is_file() {
            return Err("安装文件不是独占普通文件".into());
        }
        Ok(file)
    }
    /// New-file-only writer, without truncating or reopening an existing inode.
    /// Bound callers receive native private creation relative to the retained
    /// parent. Legacy `write` below preserves its historical overwrite behavior.
    pub fn open_new_writer(&self) -> Result<fs::File> {
        if let Some(bound) = &self.bound {
            return bound.parent.create_file(&bound.name).map_err(error);
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.process_path()?)
            .map_err(error)?;
        if !file.metadata().map_err(error)?.is_file() {
            return Err("安装目标不是独占普通文件".into());
        }
        Ok(file)
    }
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut file = if self.bound.is_some() {
            // Never truncate a pre-existing inode: it may be an outside file's
            // hard-link alias. Private metadata and embedded artifacts are new.
            self.open_new_writer()?
        } else {
            let file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(self.process_path()?)
                .map_err(error)?;
            if !file.metadata().map_err(error)?.is_file() {
                return Err("安装目标不是独占普通文件".into());
            }
            file
        };
        file.write_all(bytes).map_err(error)
    }
}
#[cfg(target_os = "linux")]
pub(super) fn anchor(file: &fs::File) -> Result<PathBuf> {
    // The owning confined thread, rather than the unconfined GUI's FD table.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    Ok(PathBuf::from(format!(
        "/proc/{tid}/fd/{}",
        file.as_raw_fd()
    )))
}
#[cfg(not(target_os = "linux"))]
pub(super) fn anchor(_file: &fs::File) -> Result<PathBuf> {
    Err("此平台尚无保留句柄的 Java 文件路径适配器".into())
}
fn path_adapter(file: &fs::File) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        anchor(file).ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        None
    }
}

fn parts(relative: &Path) -> Result<Vec<&std::ffi::OsStr>> {
    let mut names = Vec::new();
    for part in relative.components() {
        match part {
            std::path::Component::Normal(name) => {
                Leaf::new(name).map_err(error)?;
                names.push(name)
            }
            _ => return Err("安装相对路径无效".into()),
        }
    }
    if names.len() > 64 {
        return Err("安装路径层级过深".into());
    }
    Ok(names)
}
fn open_dir(parent: &fs::File, name: &std::ffi::OsStr, create: bool) -> Result<fs::File> {
    // NO_XDEV | NO_SYMLINKS | BENEATH. Device numbers alone do not detect bind
    // mounts; openat2 is a required fail-closed private-install capability.
    let open = || -> std::io::Result<fs::File> {
        let parent = Directory::from_file(parent.try_clone()?)?;
        let child = if name == "." {
            parent.reopen(Traversal::SameFilesystem)?
        } else {
            let leaf = Leaf::new(name)?;
            if create {
                match parent.create_private(&leaf, Traversal::SameFilesystem) {
                    Ok(child) => child,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        parent.child(&leaf, Traversal::SameFilesystem)?
                    }
                    Err(e) => return Err(e),
                }
            } else {
                parent.child(&leaf, Traversal::SameFilesystem)?
            }
        };
        child.try_clone_file()
    };
    open()
        .map_err(|e| format!("无法绑定私有安装子目录（需要原生目录能力，禁止链接或挂载替换）：{e}"))
}
impl InstallDir {
    #[cfg(test)]
    pub(super) fn redirect_adapter_for_test(&mut self, path: PathBuf) {
        self.path = Some(path);
    }
    pub fn legacy(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            fd: None,
        }
    }
    pub fn bound(file: fs::File) -> Result<Self> {
        if !file.metadata().map_err(error)?.is_dir() {
            return Err("私有安装根 FD 不是目录".into());
        }
        // Probe openat2 before a private install can write anything.
        let probe = open_dir(&file, std::ffi::OsStr::new("."), false)?;
        drop(probe);
        Ok(Self {
            path: path_adapter(&file),
            fd: Some(Arc::new(file)),
        })
    }
    pub fn reanchor(&self) -> Self {
        match &self.fd {
            Some(fd) => Self {
                path: path_adapter(fd),
                fd: Some(fd.clone()),
            },
            None => self.clone(),
        }
    }
    pub fn process_path(&self) -> Result<&Path> {
        self.path
            .as_deref()
            .ok_or_else(|| "此平台尚无保留句柄的 Java 目录路径适配器".into())
    }
    pub fn entry_exists(&self, relative: impl AsRef<Path>) -> Result<bool> {
        let relative = relative.as_ref();
        let names = parts(relative)?;
        let leaf = names.last().ok_or("安装检查路径为空")?;
        if let Some(root) = &self.fd {
            let mut parent =
                Directory::from_file(root.try_clone().map_err(error)?).map_err(error)?;
            for name in &names[..names.len() - 1] {
                parent = match parent
                    .child(&Leaf::new(name).map_err(error)?, Traversal::SameFilesystem)
                {
                    Ok(child) => child,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                    Err(e) => return Err(error(e)),
                };
            }
            return parent
                .entry(&Leaf::new(leaf).map_err(error)?)
                .map(|entry| entry.is_some())
                .map_err(error);
        }
        match fs::symlink_metadata(pcl_core::safe_join(self.process_path()?, relative)?) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(error(e)),
        }
    }
    pub fn is_bound(&self) -> bool {
        self.fd.is_some()
    }
    pub fn fd(&self) -> Option<&fs::File> {
        self.fd.as_deref()
    }
    pub fn directory(&self, relative: impl AsRef<Path>, create: bool) -> Result<Self> {
        let relative = relative.as_ref();
        let names = parts(relative)?;
        if let Some(root) = &self.fd {
            let mut dir = root.clone();
            for name in names {
                dir = Arc::new(open_dir(&dir, name, create)?);
            }
            Ok(Self {
                path: path_adapter(&dir),
                fd: Some(dir),
            })
        } else {
            let path = pcl_core::safe_join(self.process_path()?, relative)?;
            if create {
                fs::create_dir_all(&path).map_err(error)?;
            }
            Ok(Self::legacy(path))
        }
    }
    pub fn file(&self, relative: impl AsRef<Path>) -> Result<InstallPath> {
        let relative = relative.as_ref();
        let names = parts(relative)?;
        let leaf = names.last().ok_or("安装文件路径为空")?;
        if let Some(root) = &self.fd {
            let mut parent = root.clone();
            for name in &names[..names.len() - 1] {
                parent = Arc::new(open_dir(&parent, name, true)?);
            }
            let parent = Directory::from_file(parent.try_clone().map_err(error)?).map_err(error)?;
            let name = Leaf::new(leaf).map_err(error)?;
            let path = path_adapter(parent.file()).map(|path| path.join(leaf));
            // Refuse existing linked/special leaves before hashes or writes.
            match parent.entry(&name).map_err(error)? {
                Some(m) if m.kind != Kind::Regular || m.links != 1 || m.reparse_tag.is_some() => {
                    return Err("私有安装目标不是独占普通文件".into())
                }
                _ => {}
            }
            Ok(InstallPath {
                path,
                relative: relative.into(),
                bound: Some(BoundLeaf { parent, name }),
            })
        } else {
            Ok(InstallPath {
                path: Some(pcl_core::safe_join(self.process_path()?, relative)?),
                relative: relative.into(),
                bound: None,
            })
        }
    }
    pub fn files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        self.walk(Path::new(""), 0, &mut 0usize, &mut files)?;
        Ok(files)
    }
    fn walk(
        &self,
        prefix: &Path,
        depth: usize,
        count: &mut usize,
        files: &mut Vec<PathBuf>,
    ) -> Result<()> {
        if depth > 64 {
            return Err("安装树层级过深".into());
        }
        if let Some(fd) = &self.fd {
            let directory = Directory::from_file(fd.try_clone().map_err(error)?).map_err(error)?;
            return walk_bound(&directory, prefix, depth, count, files);
        }
        for entry in fs::read_dir(self.process_path()?).map_err(error)? {
            let entry = entry.map_err(error)?;
            *count += 1;
            if *count > 200_000 {
                return Err("安装树文件数量过多".into());
            }
            let kind = entry.file_type().map_err(error)?;
            let relative = prefix.join(entry.file_name());
            if kind.is_dir() {
                self.directory(Path::new(&entry.file_name()), false)?.walk(
                    &relative,
                    depth + 1,
                    count,
                    files,
                )?;
            } else if kind.is_file() {
                files.push(relative);
            } else {
                return Err("组件处理器生成了不允许的链接或特殊文件".into());
            }
        }
        Ok(())
    }
    pub fn remove_directory(&self, relative: &Path) -> Result<()> {
        if self.fd.is_none() {
            return fs::remove_dir_all(pcl_core::safe_join(self.process_path()?, relative)?)
                .map_err(error);
        }
        let names = parts(relative)?;
        let leaf = names.last().ok_or("清理目录路径为空")?;
        let parent_relative: PathBuf = names[..names.len() - 1].iter().collect();
        let parent = self.directory(parent_relative, false)?;
        let child = parent.directory(Path::new(leaf), false)?;
        // Preflight the full owned subtree before deleting anything. Unknown
        // symlinks, mounts and special nodes retain the private journal/tree.
        child.files()?;
        let parent = native_directory(&parent)?;
        let child = native_directory(&child)?;
        let leaf = Leaf::new(leaf).map_err(error)?;
        parent
            .recheck(&leaf, &child.identity().map_err(error)?)
            .map_err(error)?;
        clear_bound(&child, 0, &mut 0)?;
        remove_workspace_entry(&parent, &leaf, &child.metadata().map_err(error)?)
    }
}

fn native_directory(directory: &InstallDir) -> Result<Directory> {
    Directory::from_file(
        directory
            .fd()
            .ok_or("安装目录未绑定原生句柄")?
            .try_clone()
            .map_err(error)?,
    )
    .map_err(error)
}

/// This operation owns only an already-authorized temporary workspace tree.
/// Each node is inspected and held again after full-tree preflight. POSIX's
/// final identity recheck and unlinkat are not an atomic expected-inode delete.
fn clear_bound(directory: &Directory, depth: usize, count: &mut usize) -> Result<()> {
    if depth > 64 {
        return Err("清理安装树层级过深".into());
    }
    for name in directory.names(200_001).map_err(error)? {
        *count += 1;
        if *count > 200_000 {
            return Err("清理安装树文件数量过多".into());
        }
        let leaf = Leaf::new(name).map_err(error)?;
        let observed = directory
            .entry(&leaf)
            .map_err(error)?
            .ok_or("清理安装树内容已消失，保留待恢复")?;
        match observed.kind {
            Kind::Directory if observed.reparse_tag.is_none() => {
                let child = directory
                    .child(&leaf, Traversal::SameFilesystem)
                    .map_err(error)?;
                let held = child.metadata().map_err(error)?;
                if !held.same_object(&observed) {
                    return Err("清理安装目录已替换，保留待恢复".into());
                }
                directory.recheck(&leaf, &held.identity).map_err(error)?;
                clear_bound(&child, depth + 1, count)?;
                remove_workspace_entry(directory, &leaf, &child.metadata().map_err(error)?)?;
            }
            Kind::Regular if observed.reparse_tag.is_none() && observed.links == 1 => {
                let file = directory
                    .open_regular(&leaf, Links::Single)
                    .map_err(error)?;
                let held = pcl_core::retained_fs::metadata(&file).map_err(error)?;
                if !held.same_object(&observed) {
                    return Err("清理安装文件已替换，保留待恢复".into());
                }
                directory.recheck(&leaf, &held.identity).map_err(error)?;
                remove_workspace_entry(directory, &leaf, &held)?;
            }
            _ => return Err("私有安装清理发现链接、多重硬链接或特殊文件，保留待恢复".into()),
        }
    }
    Ok(())
}

fn remove_workspace_entry(
    parent: &Directory,
    leaf: &Leaf,
    expected: &pcl_core::retained_fs::Metadata,
) -> Result<()> {
    match parent
        .remove_workspace_entry(leaf, expected)
        .map_err(error)?
    {
        WorkspaceRemoval::Removed => Ok(()),
        WorkspaceRemoval::MayHaveRemoved { error } => Err(format!(
            "私有安装清理可能已移除条目，但未能完成复核或持久化；剩余内容已保留待恢复：{error}"
        )),
    }
}
/// Enumerate and inspect each child relative to its retained parent. A native
/// name snapshot is not authority: literal child opens and identity rechecks
/// bracket every recursive descent/file admission. Proc adapter paths are not
/// consulted by this read-only traversal.
fn walk_bound(
    directory: &Directory,
    prefix: &Path,
    depth: usize,
    count: &mut usize,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > 64 {
        return Err("安装树层级过深".into());
    }
    let limit = 200_000usize.saturating_sub(*count) + 1;
    for name in directory.names(limit).map_err(error)? {
        *count += 1;
        if *count > 200_000 {
            return Err("安装树文件数量过多".into());
        }
        let leaf = Leaf::new(&name).map_err(error)?;
        let observed = directory
            .entry(&leaf)
            .map_err(error)?
            .ok_or("安装树内容在枚举期间已变化，保留待恢复")?;
        let relative = prefix.join(&name);
        match observed.kind {
            Kind::Directory if observed.reparse_tag.is_none() => {
                let child = directory
                    .child(&leaf, Traversal::SameFilesystem)
                    .map_err(error)?;
                let held = child.metadata().map_err(error)?;
                if !held.same_object(&observed) {
                    return Err("安装目录在枚举期间已替换，保留待恢复".into());
                }
                walk_bound(&child, &relative, depth + 1, count, files)?;
                directory.recheck(&leaf, &held.identity).map_err(error)?;
            }
            Kind::Regular if observed.reparse_tag.is_none() && observed.links == 1 => {
                let file = directory
                    .open_regular(&leaf, Links::Single)
                    .map_err(error)?;
                let held = pcl_core::retained_fs::metadata(&file).map_err(error)?;
                if !held.same_object(&observed) {
                    return Err("安装文件在枚举期间已替换，保留待恢复".into());
                }
                directory.recheck(&leaf, &held.identity).map_err(error)?;
                files.push(relative);
            }
            Kind::Regular => return Err("私有安装树发现多重硬链接，保留待恢复".into()),
            _ => return Err("组件处理器生成了不允许的链接或特殊文件".into()),
        }
    }
    Ok(())
}

pub(super) struct InstallTemporary {
    legacy: Option<tempfile::TempDir>,
    parent: InstallDir,
    name: std::ffi::OsString,
    dir: InstallDir,
}
impl InstallTemporary {
    pub fn new(parent: &InstallDir) -> Result<Self> {
        if parent.is_bound() {
            let parent_native = native_directory(parent)?;
            let (name, directory) = parent_native.create_private_workspace().map_err(error)?;
            // The returned directory is retained before any child is written.
            // A sync failure leaves the new private name for inspection.
            parent_native.sync().map_err(error)?;
            let dir = InstallDir::bound(directory.try_clone_file().map_err(error)?)?;
            return Ok(Self {
                legacy: None,
                parent: parent.clone(),
                name: name.as_os_str().to_owned(),
                dir,
            });
        }
        let temporary = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(parent.process_path()?)
            .map_err(error)?;
        let name = temporary
            .path()
            .file_name()
            .ok_or("临时目录名称无效")?
            .to_os_string();
        let dir = InstallDir::legacy(temporary.path().into());
        Ok(Self {
            legacy: Some(temporary),
            parent: parent.clone(),
            name,
            dir,
        })
    }
    pub fn dir(&self) -> &InstallDir {
        &self.dir
    }
    /// Flatten native archives into this owned, unpublished workspace. Legacy
    /// extraction used the last occurrence of a basename, including across
    /// different JARs. Preserve that policy only inside this temporary tree;
    /// shared dependency downloads remain strictly new-file-only.
    pub fn extract_natives(
        &self,
        source: impl Read + std::io::Seek,
        excludes: &[Value],
        platform: pcl_core::platform::Platform,
    ) -> Result<()> {
        let natives = self.dir.directory("natives", true)?;
        pcl_core::extract_natives_with(source, excludes, platform, |name, size, input| {
            let destination = natives.file(name)?;
            let mut output = destination.output(size)?;
            let copied = std::io::copy(&mut input.take(size + 1), output.file_mut()).map_err(error);
            match copied {
                Ok(count) if count == size => {
                    // The reader validates size and ZIP CRC before delivering
                    // its final bytes. A bad later entry leaves this whole
                    // private workspace unpublished and eligible for cleanup.
                    if let Some(bound) = &destination.bound {
                        if let Some(existing) = bound.parent.entry(&bound.name).map_err(error)? {
                            output.file_mut().sync_all().map_err(error)?;
                            remove_workspace_entry(&bound.parent, &bound.name, &existing)?;
                        }
                    }
                    output.publish()?;
                    Ok(count)
                }
                Ok(_) => Err(output.close("运行库解压大小不符".into())),
                Err(e) => Err(output.close(e)),
            }
        })
    }
    pub fn name(&self) -> &std::ffi::OsStr {
        &self.name
    }
    pub fn publish(&mut self, name: &str) -> Result<()> {
        if let Some(fd) = self.parent.fd() {
            let parent = native_directory(&self.parent)?;
            let directory = native_directory(&self.dir)?;
            let source = Leaf::new(&self.name).map_err(error)?;
            let target = Leaf::new(name).map_err(error)?;
            let identity = directory.identity().map_err(error)?;
            parent.recheck(&source, &identity).map_err(error)?;
            directory.sync().map_err(error)?;
            pcl_core::file_ops::rename_noreplace_at(fd, &self.name, fd, std::ffi::OsStr::new(name))
                .map_err(|e| {
                    format!("无法完成版本安装（目标可能已存在或可能已发布，保留待复核）：{e}")
                })?;
            let confirmation = (|| {
                parent.recheck(&target, &identity).map_err(error)?;
                if parent.entry(&source).map_err(error)?.is_some() {
                    return Err("安装暂存源名称在发布后重新出现".to_string());
                }
                parent.sync().map_err(error)
            })();
            confirmation.map_err(|e| {
                format!("版本可能已发布，但复核或持久化失败；所有现存条目已保留：{e}")
            })?;
        } else {
            publish_directory(
                self.dir.process_path()?,
                &self.parent.process_path()?.join(name),
            )?;
        }
        if let Some(temporary) = self.legacy.take() {
            let _ = temporary.keep();
        }
        Ok(())
    }
    pub fn close(self) -> Result<()> {
        match self.legacy {
            Some(temporary) => temporary.close().map_err(error),
            None => {
                let parent = native_directory(&self.parent)?;
                let directory = native_directory(&self.dir)?;
                let name = Leaf::new(&self.name).map_err(error)?;
                parent
                    .recheck(&name, &directory.identity().map_err(error)?)
                    .map_err(error)?;
                self.dir.files()?;
                parent
                    .recheck(&name, &directory.identity().map_err(error)?)
                    .map_err(error)?;
                clear_bound(&directory, 0, &mut 0)?;
                remove_workspace_entry(&parent, &name, &directory.metadata().map_err(error)?)
            }
        }
    }
}

/// Apply only in a new scoped worker. Children and its download threads inherit
/// this inode-based write boundary; the caller keeps its normal permissions.
#[cfg(target_os = "linux")]
pub(super) fn confine_writes(root: &fs::File) -> Result<()> {
    #[repr(C)]
    struct Ruleset {
        handled_access_fs: u64,
    }
    #[repr(C, packed)]
    struct Beneath {
        allowed_access: u64,
        parent_fd: i32,
    }
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0,
            1,
        )
    };
    if abi < 3 {
        let detail = if abi < 0 {
            error(std::io::Error::last_os_error())
        } else {
            format!("当前 ABI 为 {abi}")
        };
        return Err(format!("私有整合包安装需要启用 Linux Landlock ABI 3（Linux 6.2 或更新）；当前能力不可用：{detail}"));
    }
    let writes = (1 << 1)
        | (1 << 4)
        | (1 << 5)
        | (1 << 6)
        | (1 << 7)
        | (1 << 8)
        | (1 << 9)
        | (1 << 10)
        | (1 << 11)
        | (1 << 12)
        | (1 << 13)
        | (1 << 14);
    let rules = Ruleset {
        handled_access_fs: writes,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &rules,
            std::mem::size_of::<Ruleset>(),
            0,
        )
    };
    if fd < 0 {
        return Err(format!(
            "无法建立私有安装写入边界：{}",
            std::io::Error::last_os_error()
        ));
    }
    let ruleset = unsafe { fs::File::from_raw_fd(fd as i32) };
    let beneath = Beneath {
        allowed_access: writes,
        parent_fd: root.as_raw_fd(),
    };
    if unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            1,
            &beneath,
            0,
        )
    } != 0
        || unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        || unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0) } != 0
    {
        return Err(format!(
            "无法启用私有安装写入边界（Landlock）：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn confine_writes(_root: &fs::File) -> Result<()> {
    Err("此平台的 Java 安装处理器写入隔离尚未可用".into())
}
