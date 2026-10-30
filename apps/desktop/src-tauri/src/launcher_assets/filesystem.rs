//! Asset files use retained native directories and real platform identities.
//! Source snapshots own their handle until reading finishes. New media bytes
//! are sealed and published without replacement; uncertain publication retains
//! its native receipt and never triggers name-based rollback or retry.
#[cfg(unix)]
use pcl_core::retained_fs::Security;
use pcl_core::retained_fs::{
    self,
    stage::{PublishOutcome, Stage},
    Directory, Identity, Kind, Leaf, Links, Metadata, Traversal,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

pub(super) const MAX_ENTRIES: usize = 256;
pub(super) fn component(name: &str) -> Result<(), String> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.len() > 255
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        Err("媒体文件名无效".into())
    } else {
        Ok(())
    }
}
fn leaf(name: &str) -> Result<Leaf, String> {
    component(name)?;
    Leaf::new(name).map_err(|e| format!("媒体文件名无效：{e}"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Stamp {
    // Preserve the exact Unix stamp field names/order/values used by existing
    // media IDs and picker revisions. Windows serializes its native metadata,
    // including the complete 128-bit ID and security descriptor, separately.
    #[cfg(unix)]
    pub device: u64,
    #[cfg(unix)]
    pub inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    owner: u32,
    #[cfg(unix)]
    links: u64,
    pub bytes: u64,
    #[cfg(unix)]
    modified: (i64, i64),
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg_attr(unix, serde(skip))]
    #[cfg_attr(windows, serde(flatten))]
    native: Metadata,
}
impl Stamp {
    pub fn of(file: &File) -> Result<Self, String> {
        let m = retained_fs::metadata(file).map_err(|e| e.to_string())?;
        if m.kind != Kind::Regular || m.links != 1 || m.reparse_tag.is_some() {
            return Err("只允许当前用户独占的普通媒体文件".into());
        }
        m.require_owned()
            .map_err(|_| "只允许当前用户独占的普通媒体文件")?;
        #[cfg(unix)]
        let (device, inode, mode, owner) = match (&m.identity, &m.security) {
            (Identity::Unix { device, inode }, Security::Unix { mode, owner, .. }) => {
                (*device, *inode, *mode, *owner)
            }
            _ => return Err("媒体元数据平台不匹配".into()),
        };
        Ok(Self {
            #[cfg(unix)]
            device,
            #[cfg(unix)]
            inode,
            #[cfg(unix)]
            mode,
            #[cfg(unix)]
            owner,
            #[cfg(unix)]
            links: m.links,
            bytes: m.len,
            #[cfg(unix)]
            modified: (m.modified.seconds, i64::from(m.modified.nanos)),
            #[cfg(unix)]
            changed: (m.changed.seconds, i64::from(m.changed.nanos)),
            native: m,
        })
    }
}

pub(super) struct Dir(pub Directory);
impl Dir {
    pub fn absolute(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("媒体路径必须是绝对路径".into());
        }
        for part in path.components() {
            match part {
                Component::Normal(name) => component(name.to_str().ok_or("媒体路径必须是 UTF-8")?)?,
                Component::CurDir | Component::ParentDir => {
                    return Err("媒体路径不能包含相对路径跳转".into())
                }
                _ => {}
            }
        }
        Directory::open_absolute(path)
            .map(Self)
            .map_err(|e| format!("无法打开媒体目录（不允许符号链接或重解析点）：{e}"))
    }
    pub fn optional(&self, name: &str) -> Result<Option<Self>, String> {
        match self.0.child(&leaf(name)?, Traversal::AllowMounts) {
            Ok(directory) => Ok(Some(Self(directory))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "无法打开媒体目录（不允许符号链接或重解析点）：{error}"
            )),
        }
    }
    pub fn create(&self, name: &str) -> Result<Self, String> {
        let name = leaf(name)?;
        let dir = match self.0.create_private(&name, Traversal::AllowMounts) {
            Ok(directory) => Self(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Self(
                self.0
                    .child(&name, Traversal::AllowMounts)
                    .map_err(|e| e.to_string())?,
            ),
            Err(error) => return Err(format!("无法创建媒体目录：{error}")),
        };
        dir.0
            .metadata()
            .and_then(|m| m.require_owned())
            .map_err(|_| "媒体目录不属于当前用户")?;
        self.0
            .recheck(&name, &dir.identity()?)
            .map_err(|e| e.to_string())?;
        self.sync()?;
        Ok(dir)
    }
    pub fn identity(&self) -> Result<Identity, String> {
        self.0.identity().map_err(|e| e.to_string())
    }
    pub fn file(&self, name: &str) -> Result<Option<File>, String> {
        let name = leaf(name)?;
        let file = match self.0.open_regular(&name, Links::Single) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("无法读取媒体（不允许链接或特殊文件）：{error}")),
        };
        let stamp = Stamp::of(&file)?;
        self.0
            .recheck(&name, &stamp.native.identity)
            .map_err(|e| e.to_string())?;
        Ok(Some(file))
    }
    pub fn names(&self) -> Result<Vec<String>, String> {
        let native = self.0.names(MAX_ENTRIES + 1).map_err(|e| e.to_string())?;
        if native.len() > MAX_ENTRIES {
            return Err("媒体目录最多允许 256 个文件".into());
        }
        let mut names = native
            .into_iter()
            .map(|name| {
                name.into_string()
                    .map_err(|_| "媒体文件名必须是 UTF-8".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        Ok(names)
    }
    pub fn sync(&self) -> Result<(), String> {
        self.0.sync().map_err(|e| format!("媒体目录保存失败：{e}"))
    }
    pub fn write_new(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let name = leaf(name)?;
        #[cfg(target_os = "linux")]
        let source = self.0.clone();
        #[cfg(not(target_os = "linux"))]
        let (source, workspace) = {
            if self
                .0
                .metadata()
                .map_err(|e| e.to_string())?
                .require_private()
                .is_ok()
            {
                (self.0.clone(), None)
            } else {
                // Existing owned catalogs may be 0755 or have inherited ACLs.
                // Keep their permissions intact: named leases originate in a
                // new private workspace, never in a broadened Stage parent.
                let (name, directory) = self
                    .0
                    .create_private_workspace()
                    .map_err(|e| format!("无法创建私有媒体暂存目录：{e}"))?;
                let metadata = directory.metadata().map_err(|e| {
                    format!(
                        "媒体暂存目录已创建，保留 {}：{e}",
                        name.as_os_str().to_string_lossy()
                    )
                })?;
                self.0.sync().map_err(|e| {
                    format!(
                        "媒体暂存目录已创建但同步失败，保留 {}：{e}",
                        name.as_os_str().to_string_lossy()
                    )
                })?;
                (directory, Some((name, metadata)))
            }
        };
        #[cfg(target_os = "linux")]
        let workspace_location = "当前目录的匿名媒体暂存".to_owned();
        #[cfg(not(target_os = "linux"))]
        let workspace_location = workspace
            .as_ref()
            .map(|(name, _)| name.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_else(|| "当前私有媒体目录".into());
        let mut stage = Stage::new(&source)
            .map_err(|e| format!("无法创建媒体暂存，现存目录已保留 {workspace_location}：{e}"))?;
        let location = stage
            .source_name()
            .map_or(workspace_location.clone(), |name| {
                format!(
                    "{workspace_location}/{}",
                    name.as_os_str().to_string_lossy()
                )
            });
        #[cfg(not(target_os = "linux"))]
        source
            .sync()
            .map_err(|e| format!("命名媒体暂存已创建但同步失败，保留 {location}：{e}"))?;
        stage
            .file_mut()
            .write_all(bytes)
            .map_err(|e| format!("媒体暂存写入失败，现存条目已保留；暂存 {location}：{e}"))?;
        let sealed = stage.seal(bytes.len() as u64).map_err(|failed| {
            format!(
                "媒体暂存封存失败，现存条目已保留；暂存 {location}：{}",
                failed.error
            )
        })?;
        match sealed.publish_new(&self.0, &name) {
            Ok(PublishOutcome::Published { receipt }) => {
                #[cfg(target_os = "linux")]
                let _ = receipt;
                #[cfg(not(target_os = "linux"))]
                if let Some((name, expected)) = workspace {
                    let cleanup = (|| {
                        if source.identity().map_err(|e| e.to_string())? != expected.identity
                            || !source.names(1).map_err(|e| e.to_string())?.is_empty()
                        {
                            return Err("私有媒体暂存目录身份或内容已变化".to_owned());
                        }
                        self.0
                            .recheck(&name, &expected.identity)
                            .map_err(|e| e.to_string())?;
                        match self
                            .0
                            .remove_workspace_entry(&name, &expected)
                            .map_err(|e| e.to_string())?
                        {
                            retained_fs::WorkspaceRemoval::Removed => Ok(()),
                            retained_fs::WorkspaceRemoval::MayHaveRemoved { error } => {
                                Err(format!("媒体暂存目录清理或持久化结果不确定：{error}"))
                            }
                        }
                    })();
                    if let Err(error) = cleanup {
                        let evidence =
                            serde_json::to_string(&receipt).map_err(|e| e.to_string())?;
                        return Err(format!("媒体已保存，暂存目录清理未确认，请保留现存条目 {location}：{error}；发布证据：{evidence}"));
                    }
                }
                Ok(())
            }
            Ok(PublishOutcome::Exists { .. }) => Err(format!(
                "无法发布媒体（目标文件必须不存在），现存文件已保留；暂存 {location}"
            )),
            Ok(PublishOutcome::MayHavePublished { receipt, error, .. }) => {
                let evidence = serde_json::to_string(&receipt).map_err(|e| e.to_string())?;
                Err(format!("媒体可能已保存，但发布复核或同步失败，请保留现存文件并刷新；暂存 {location}：{error}；发布证据：{evidence}"))
            }
            Err(failed) => Err(format!(
                "媒体发布准入失败，现存条目已保留；暂存 {location}：{}",
                failed.error
            )),
        }
    }
}

pub(super) struct Snapshot {
    pub file: File,
    pub stamp: Stamp,
    pub digest: String,
    pub header: Vec<u8>,
}
impl Snapshot {
    pub fn open(dir: &Dir, name: &str, max: u64) -> Result<Self, String> {
        let mut file = dir.file(name)?.ok_or("媒体文件不存在")?;
        let stamp = Stamp::of(&file)?;
        if stamp.bytes == 0 || stamp.bytes > max {
            return Err("媒体文件为空或超过大小上限".into());
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut count = 0u64;
        let mut header = Vec::new();
        loop {
            let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            count = count.checked_add(read as u64).ok_or("媒体文件过大")?;
            if count > max {
                return Err("媒体读取期间超过大小上限".into());
            }
            hash.update(&buffer[..read]);
            let keep = read.min((128 * 1024usize).saturating_sub(header.len()));
            header.extend_from_slice(&buffer[..keep]);
        }
        let snapshot = Self {
            file,
            stamp,
            digest: format!("{:x}", hash.finalize()),
            header,
        };
        if count != snapshot.stamp.bytes {
            return Err("媒体读取期间发生变化".into());
        }
        snapshot.recheck(dir, name)?;
        Ok(snapshot)
    }
    pub fn recheck(&self, dir: &Dir, name: &str) -> Result<(), String> {
        check_file(&self.file, &self.stamp, dir, name)
    }
    pub fn bytes(&mut self, dir: &Dir, name: &str) -> Result<Vec<u8>, String> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(self.stamp.bytes as usize);
        (&mut self.file)
            .take(self.stamp.bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        self.recheck(dir, name)?;
        if bytes.len() as u64 != self.stamp.bytes {
            return Err("媒体读取期间发生变化".into());
        }
        Ok(bytes)
    }
}
pub(super) fn check_file(file: &File, stamp: &Stamp, dir: &Dir, name: &str) -> Result<(), String> {
    let current = dir.file(name)?.ok_or("媒体文件已消失")?;
    if Stamp::of(file)? != *stamp || Stamp::of(&current)? != *stamp {
        Err("媒体文件已变化，请刷新媒体列表".into())
    } else {
        Ok(())
    }
}
pub(super) fn source(path: &Path, max: u64) -> Result<(Dir, String, Snapshot), String> {
    let parent = Dir::absolute(path.parent().ok_or("文件路径无效")?)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("文件名必须是 UTF-8")?
        .to_owned();
    component(&name)?;
    let snapshot = Snapshot::open(&parent, &name, max)?;
    Ok((parent, name, snapshot))
}
pub(super) fn revision(path: &Path, snapshot: &Snapshot) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(path, &snapshot.stamp, &snapshot.digest))
                .map_err(|e| e.to_string())?
        )
    ))
}
pub(super) struct Scope {
    pub path: PathBuf,
    pub root: Dir,
    pub owner: Option<Dir>,
    pub folder: Option<Dir>,
    pub folder_name: &'static str,
}
impl Scope {
    pub fn open(project: &Path, folder_name: &'static str) -> Result<Self, String> {
        let root = Dir::absolute(project)?;
        let owner = root.optional(".pcl-rust")?;
        let folder = owner
            .as_ref()
            .map(|d| d.optional(folder_name))
            .transpose()?
            .flatten();
        Ok(Self {
            path: project.to_owned(),
            root,
            owner,
            folder,
            folder_name,
        })
    }
    pub fn path(&self) -> PathBuf {
        self.path.join(".pcl-rust").join(self.folder_name)
    }
    pub fn recheck(&self) -> Result<(), String> {
        let fresh = Self::open(&self.path, self.folder_name)?;
        if fresh.root.identity()? != self.root.identity()?
            || fresh.owner.as_ref().map(Dir::identity).transpose()?
                != self.owner.as_ref().map(Dir::identity).transpose()?
            || fresh.folder.as_ref().map(Dir::identity).transpose()?
                != self.folder.as_ref().map(Dir::identity).transpose()?
        {
            Err("媒体目录发生变化，请重新读取".into())
        } else {
            Ok(())
        }
    }
    pub fn create(project: &Path, folder_name: &'static str) -> Result<Self, String> {
        let root = Dir::absolute(project)?;
        let owner = root.create(".pcl-rust")?;
        let folder = owner.create(folder_name)?;
        let scope = Self {
            path: project.to_owned(),
            root,
            owner: Some(owner),
            folder: Some(folder),
            folder_name,
        };
        scope.recheck()?;
        Ok(scope)
    }
}
