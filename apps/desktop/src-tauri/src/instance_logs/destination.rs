//! Chosen destination ancestry is observed through retained directory handles.
use crate::launcher_local::filesystem::{Dir, DirectoryIdentity};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
};

pub(super) struct Destination {
    parent: PathBuf,
    folder: Dir,
    source_root: DirectoryIdentity,
}
impl Destination {
    pub(super) fn capture(target: &Path, source_root: DirectoryIdentity) -> Result<Self, String> {
        let parent = target.parent().ok_or("保存目录无效")?.to_owned();
        let folder = Dir::absolute(&parent)?;
        outside_source(&folder, source_root.clone())?;
        Ok(Self {
            parent,
            folder,
            source_root,
        })
    }
    pub(super) fn verify(&self) -> Result<(), String> {
        if Dir::absolute(&self.parent)?.identity()? != self.folder.identity()? {
            return Err("保存目录已被替换，请重新选择".into());
        }
        outside_source(&self.folder, self.source_root.clone())
    }
}

/// Compare real directory identities through parent FDs. This also rejects a
/// bind-mounted alias of the root and a destination moved inside the root while
/// staging; lexical prefixes cannot establish either relationship.
#[cfg(target_os = "linux")]
fn outside_source(folder: &Dir, source_root: DirectoryIdentity) -> Result<(), String> {
    let mut current = Dir(folder.0.try_clone().map_err(|_| "无法检查保存目录")?);
    for _ in 0..4096 {
        let identity = current.identity()?;
        if identity == source_root {
            return Err("请将日志文本保存到游戏根目录以外的位置".into());
        }
        let fd = unsafe {
            libc::openat(
                current.0.as_raw_fd(),
                c"..".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err("无法检查保存目录的真实祖先".into());
        }
        let parent = Dir(unsafe { File::from_raw_fd(fd) });
        if parent.identity()? == identity {
            return Ok(());
        }
        current = parent;
    }
    Err("保存目录祖先层级超过安全上限".into())
}

/// Native parent observations bind the current real name back to the held
/// directory. A failed/changed snapshot refuses export; these observations
/// never replace the retained target capability used for publication.
#[cfg(not(target_os = "linux"))]
fn outside_source(folder: &Dir, source_root: DirectoryIdentity) -> Result<(), String> {
    use pcl_core::retained_fs::Directory;
    let mut current = Directory::from_file(folder.0.try_clone().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    for _ in 0..4096 {
        let identity = Dir(current.try_clone_file().map_err(|e| e.to_string())?).identity()?;
        if identity == source_root {
            return Err("请将日志文本保存到游戏根目录以外的位置".into());
        }
        let parent = current
            .parent_snapshot()
            .map_err(|e| format!("无法检查保存目录的真实祖先：{e}"))?;
        if parent.identity().map_err(|e| e.to_string())?
            == current.identity().map_err(|e| e.to_string())?
        {
            return Ok(());
        }
        current = parent;
    }
    Err("保存目录祖先层级超过安全上限".into())
}
