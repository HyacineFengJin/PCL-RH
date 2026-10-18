//! Native text export of one displayed failure report. The chooser and bulk
//! staging run outside operations; final authorization holds the admission lock
//! through anonymous no-overwrite publication, just like instance log export.
use super::PreparedDiagnostic;
use crate::{
    launcher_local::filesystem::Dir,
    toolbox_images::publish::{self, ExportOutcome},
    Shared,
};
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
};

impl PreparedDiagnostic {
    pub(crate) fn suggested_name(&self) -> &'static str {
        // Instance names and raw paths do not become chooser filenames.
        "launch-failure-report.txt"
    }

    pub(crate) fn publish(self, shared: &Shared, target: &Path) -> Result<ExportOutcome, String> {
        self.publish_with(shared, target, || {})
    }

    pub(super) fn publish_with(
        self,
        shared: &Shared,
        target: &Path,
        before_authorize: impl FnOnce(),
    ) -> Result<ExportOutcome, String> {
        // Recheck immediately after the chooser, before preparing output.
        self.verify(shared)?;
        let destination = Destination::capture(target, self.captured.root_directory.identity()?)?;
        publish::publish_new(target, self.text().as_bytes(), "txt", 1, || {
            before_authorize();
            let guard = self.authorize(shared)?;
            destination.verify()?;
            Ok(guard)
        })
    }
}

struct Destination {
    parent: PathBuf,
    folder: Dir,
    source_root: (u64, u64),
}
impl Destination {
    fn capture(target: &Path, source_root: (u64, u64)) -> Result<Self, String> {
        let parent = target.parent().ok_or("保存目录无效")?.to_owned();
        let folder = Dir::absolute(&parent)?;
        outside_source(&folder, source_root)?;
        Ok(Self {
            parent,
            folder,
            source_root,
        })
    }
    fn verify(&self) -> Result<(), String> {
        if Dir::absolute(&self.parent)?.identity()? != self.folder.identity()? {
            return Err("保存目录已被替换，请重新选择".into());
        }
        outside_source(&self.folder, self.source_root)
    }
}

/// Follow real directory parents via FDs: lexical prefixes cannot reject a
/// bind-mount alias or a selected folder moved into the game root while staging.
fn outside_source(folder: &Dir, source_root: (u64, u64)) -> Result<(), String> {
    let mut current = Dir(folder.0.try_clone().map_err(|_| "无法检查保存目录")?);
    for _ in 0..4096 {
        let identity = current.identity()?;
        if identity == source_root {
            return Err("请将诊断报告保存到游戏根目录以外的位置".into());
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
