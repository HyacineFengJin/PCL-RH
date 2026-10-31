//! Handle-relative source reads and private native ZIP staging. No pathname
//! fallback, source mutation, destination replacement or permission repair.
use super::{component, relative, MAX_NODES, MAX_TOTAL_BYTES};
use pcl_core::retained_fs::{
    self,
    stage::{PublishOutcome, Sealed, Stage as NativeStage},
    Directory, Identity, Kind, Leaf, Links, Metadata, Security, Traversal, WorkspaceRemoval,
};
use serde::Serialize;
use std::{fs::File, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum IdentityKey {
    Unix(u64, u64),
    Windows(u64, [u8; 16]),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Stamp {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(windows)]
    identity: Identity,
    #[cfg(windows)]
    kind: Kind,
    #[cfg(windows)]
    security: Security,
    #[cfg(windows)]
    links: u64,
    #[cfg(windows)]
    reparse_tag: Option<u32>,
    pub(super) bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn native(m: Metadata) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let Identity::Unix { device, inode } = m.identity else {
                return Err("导出对象身份不属于当前平台".into());
            };
            let Security::Unix { mode, .. } = m.security else {
                return Err("导出对象安全信息不属于当前平台".into());
            };
            Ok(Self {
                device,
                inode,
                mode,
                bytes: m.len,
                modified: (m.modified.seconds, i64::from(m.modified.nanos)),
                changed: (m.changed.seconds, i64::from(m.changed.nanos)),
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                identity: m.identity,
                kind: m.kind,
                security: m.security,
                links: m.links,
                reparse_tag: m.reparse_tag,
                bytes: m.len,
                modified: (m.modified.seconds, i64::from(m.modified.nanos)),
                changed: (m.changed.seconds, i64::from(m.changed.nanos)),
            })
        }
    }
    pub(super) fn from_file(file: &File) -> Result<Self, String> {
        Self::native(retained_fs::metadata(file).map_err(|e| e.to_string())?)
    }
    pub(super) fn identity(&self) -> Identity {
        #[cfg(unix)]
        {
            Identity::Unix {
                device: self.device,
                inode: self.inode,
            }
        }
        #[cfg(windows)]
        {
            self.identity.clone()
        }
    }
    pub(super) fn identity_key(&self) -> IdentityKey {
        match self.identity() {
            Identity::Unix { device, inode } => IdentityKey::Unix(device, inode),
            Identity::Windows { volume, file } => IdentityKey::Windows(volume, file),
        }
    }
    pub(super) fn is_dir(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
        }
        #[cfg(windows)]
        {
            self.kind == Kind::Directory && self.reparse_tag.is_none()
        }
    }
    pub(super) fn is_file(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
        }
        #[cfg(windows)]
        {
            self.kind == Kind::Regular && self.reparse_tag.is_none()
        }
    }
    pub(super) fn same_kind(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.mode == other.mode
        }
        #[cfg(windows)]
        {
            self.kind == other.kind
                && self.security == other.security
                && self.reparse_tag == other.reparse_tag
        }
    }
}

pub(super) struct Dir(Directory);
fn leaf(name: &str) -> Result<Leaf, String> {
    Leaf::new(name).map_err(|e| e.to_string())
}
impl Dir {
    pub(super) fn open_absolute(path: &Path) -> Result<Self, String> {
        Directory::open_absolute(path)
            .map(Self)
            .map_err(|e| e.to_string())
    }
    pub(super) fn clone_dir(&self) -> Result<Self, String> {
        Ok(Self(self.0.clone()))
    }
    pub(super) fn child(&self, name: &str) -> Result<Self, String> {
        self.0
            .child(&leaf(name)?, Traversal::AllowMounts)
            .map(Self)
            .map_err(|e| format!("无法打开目录（符号链接不允许导出）：{e}"))
    }
    pub(super) fn ancestor(&self) -> Result<Self, String> {
        self.0
            .parent_snapshot()
            .map(Self)
            .map_err(|e| e.to_string())
    }
    pub(super) fn parent(&self, path: &str) -> Result<(Self, String), String> {
        let pieces = relative(path)?;
        let name = pieces.last().ok_or("文件路径不能为空")?.to_string();
        let mut dir = self.clone_dir()?;
        for part in &pieces[..pieces.len() - 1] {
            dir = dir.child(part)?;
        }
        Ok((dir, name))
    }
    pub(super) fn stamp(&self) -> Result<Stamp, String> {
        Stamp::native(self.0.metadata().map_err(|e| e.to_string())?)
    }
    pub(super) fn metadata(&self, name: &str) -> Result<Option<Stamp>, String> {
        self.0
            .entry(&leaf(name)?)
            .map_err(|e| e.to_string())?
            .map(Stamp::native)
            .transpose()
    }
    pub(super) fn open_file(&self, path: &str) -> Result<File, String> {
        let (dir, name) = self.parent(path)?;
        dir.0
            .open_regular(&leaf(&name)?, Links::Any)
            .map_err(|e| format!("无法打开导出普通文件（符号链接不允许导出）：{e}"))
    }
    pub(super) fn names(&self) -> Result<Vec<String>, String> {
        let mut names = self
            .0
            .names(MAX_NODES)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|name| {
                name.into_string()
                    .map_err(|_| "导出文件名必须是 UTF-8".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        Ok(names)
    }
    pub(super) fn sync(&self) -> Result<(), String> {
        self.0.sync().map_err(|e| e.to_string())
    }
}

pub(super) struct Stage {
    pub(super) parent: Dir,
    workspace: Directory,
    workspace_name: Leaf,
    workspace_stamp: Metadata,
    source_name: Leaf,
    source_identity: Identity,
    writable: Option<NativeStage>,
    sealed: Option<Sealed>,
    uncertain: bool,
    armed: bool,
    pub(super) warning: Option<String>,
}
impl Stage {
    pub(super) fn new(parent: Dir) -> Result<(Self, File), String> {
        let (workspace_name, workspace) = parent
            .0
            .create_private_workspace()
            .map_err(|e| e.to_string())?;
        parent.sync()?;
        let workspace_stamp = workspace.metadata().map_err(|e| e.to_string())?;
        let mut writable = NativeStage::new(&workspace).map_err(|e| e.to_string())?;
        let source_name = writable
            .source_name()
            .ok_or("原生 ZIP 暂存缺少命名租约")?
            .clone();
        let source_identity = retained_fs::metadata(writable.file_mut())
            .map_err(|e| e.to_string())?
            .identity;
        let file = writable.file_mut().try_clone().map_err(|e| e.to_string())?;
        Ok((
            Self {
                parent,
                workspace,
                workspace_name,
                workspace_stamp,
                source_name,
                source_identity,
                writable: Some(writable),
                sealed: None,
                uncertain: false,
                armed: true,
                warning: None,
            },
            file,
        ))
    }
    fn evidence(&self) -> String {
        format!(
            "私有 ZIP 暂存目录 {:?} 已保留",
            self.workspace_name.as_os_str()
        )
    }
    pub(super) fn seal(&mut self, bytes: u64, sha256: &str) -> Result<(), String> {
        let writable = self.writable.take().ok_or("ZIP 暂存已封存")?;
        let sealed = match writable.seal(MAX_TOTAL_BYTES * 2) {
            Ok(sealed) => sealed,
            Err(error) => {
                self.writable = Some(error.stage);
                return Err(format!(
                    "无法封存 ZIP：{}；{}",
                    error.error,
                    self.evidence()
                ));
            }
        };
        let digest: String = sealed
            .evidence()
            .sha256
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let matches = sealed.evidence().len == bytes && digest == sha256;
        self.sealed = Some(sealed);
        if !matches {
            return Err("ZIP 暂存长度或 SHA-256 在封存期间发生变化".into());
        }
        Ok(())
    }
    fn cleanup_workspace(&self) -> Result<(), String> {
        match self
            .parent
            .0
            .remove_workspace_entry(&self.workspace_name, &self.workspace_stamp)
            .map_err(|e| e.to_string())?
        {
            WorkspaceRemoval::Removed => Ok(()),
            WorkspaceRemoval::MayHaveRemoved { error } => {
                Err(format!("ZIP 私有暂存目录清理未确认：{error}"))
            }
        }
    }
    pub(super) fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        if self.uncertain {
            return Err(format!("ZIP 发布结果不确定；{}", self.evidence()));
        }
        // Withdraw every writable/exclusive handle before opening the generated
        // cleanup entry. Only this newly allocated private workspace is owned.
        drop(self.writable.take());
        drop(self.sealed.take());
        if let Some(stamp) = self
            .workspace
            .entry(&self.source_name)
            .map_err(|e| e.to_string())?
        {
            if stamp.identity != self.source_identity
                || stamp.kind != Kind::Regular
                || stamp.reparse_tag.is_some()
            {
                return Err(format!("ZIP 暂存已被外部替换；{}", self.evidence()));
            }
            match self
                .workspace
                .remove_workspace_entry(&self.source_name, &stamp)
                .map_err(|e| e.to_string())?
            {
                WorkspaceRemoval::Removed => {}
                WorkspaceRemoval::MayHaveRemoved { error } => {
                    return Err(format!("ZIP 暂存清理未确认：{error}；{}", self.evidence()))
                }
            }
        }
        self.cleanup_workspace()?;
        self.armed = false;
        Ok(())
    }
    pub(super) fn publish(&mut self, destination: &str) -> Result<(), String> {
        component(destination)?;
        let sealed = self.sealed.take().ok_or("ZIP 暂存尚未封存")?;
        let outcome = match sealed.publish_new(&self.parent.0, &leaf(destination)?) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.sealed = Some(error.stage);
                return Err(format!("无法发布 ZIP：{}", error.error));
            }
        };
        match outcome {
            PublishOutcome::Exists { stage } => {
                self.sealed = Some(stage);
                Err("目标文件已经存在，请使用新的文件名".into())
            }
            PublishOutcome::MayHavePublished { stage, error, .. } => {
                self.sealed = Some(stage);
                self.uncertain = true;
                Err(format!(
                    "ZIP 可能已发布，请检查目标文件后再操作：{error}；{}",
                    self.evidence()
                ))
            }
            PublishOutcome::Published { .. } => {
                self.armed = false;
                if let Err(error) = self.cleanup_workspace() {
                    self.warning = Some(format!(
                        "ZIP 已创建，但私有暂存目录清理失败：{error}；{}",
                        self.evidence()
                    ));
                }
                Ok(())
            }
        }
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
