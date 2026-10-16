//! Export exactly the displayed text, bound to its selection and content hash.
//! Preparation rereads and redacts outside admission; a native chooser supplies
//! the destination. Only short identity checks run under the final operations
//! guard, which publish_new retains through anonymous no-overwrite publication.
use super::{prepare_read_with, LogKind, PreparedRead, CHANGED};
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

pub(crate) struct PreparedExport {
    read: PreparedRead,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    shared: &Shared,
    root_id: &str,
    id: &str,
    kind: LogKind,
    name: &str,
    revision: &str,
    listing_revision: &str,
    content_revision: &str,
) -> Result<PreparedExport, String> {
    prepare_with(
        shared,
        root_id,
        id,
        kind,
        name,
        revision,
        listing_revision,
        content_revision,
        |text| shared.accounts.redact(text.to_owned()),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_with(
    shared: &Shared,
    root_id: &str,
    id: &str,
    kind: LogKind,
    name: &str,
    revision: &str,
    listing_revision: &str,
    content_revision: &str,
    redact: impl FnOnce(&str) -> String,
) -> Result<PreparedExport, String> {
    if content_revision.len() != 64 {
        return Err(CHANGED.into());
    }
    let read = prepare_read_with(
        shared,
        root_id,
        id,
        kind,
        name,
        revision,
        listing_revision,
        |selected| selected.read(&shared.closing),
        redact,
    )?;
    if read.content.content_revision != content_revision {
        return Err(CHANGED.into());
    }
    if read.content.text.is_empty() {
        return Err("当前日志文本为空，没有可导出的内容".into());
    }
    Ok(PreparedExport { read })
}

impl PreparedExport {
    pub(crate) fn suggested_name(&self) -> String {
        let name = self
            .read
            .content
            .name
            .strip_suffix(".gz")
            .unwrap_or(&self.read.content.name);
        let name = name
            .strip_suffix(".log")
            .or_else(|| name.strip_suffix(".txt"))
            .unwrap_or(name);
        // A listed basename can already occupy all 255 filename bytes. Keep
        // the chooser's suggestion valid after appending the explicit suffix.
        let mut end = name.len().min(243);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}.excerpt.txt", &name[..end])
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
        let destination =
            Destination::capture(target, self.read.captured.binding.root_identity()?)?;
        publish::publish_new(target, self.read.content.text.as_bytes(), "txt", 1, || {
            before_authorize();
            let guard = self.read.authorize(shared)?;
            // A newly added account may recognize a secret the earlier read did
            // not know. Require a refreshed display rather than changing bytes
            // after the user's content receipt has already been checked.
            if shared.accounts.redact(self.read.content.text.clone()) != self.read.content.text {
                return Err(CHANGED.into());
            }
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

/// Compare real directory identities through parent FDs. This also rejects a
/// bind-mounted alias of the root and a destination moved inside the root while
/// staging; lexical prefixes cannot establish either relationship.
fn outside_source(folder: &Dir, source_root: (u64, u64)) -> Result<(), String> {
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
