//! Export exactly the displayed text, bound to its selection and content hash.
//! Preparation rereads and redacts outside admission; a native chooser supplies
//! the destination. Only short identity checks run under the final operations
//! guard, which is held through no-overwrite publication. Linux retains its
//! anonymous staging; native named leases use a new private workspace.
use super::{prepare_read_with, LogKind, PreparedRead, CHANGED};
use crate::{toolbox_images::publish::ExportOutcome, Shared};
use std::path::Path;
#[path = "destination.rs"]
mod destination;
use destination::Destination;

#[cfg(target_os = "linux")]
use crate::toolbox_images::publish;
#[cfg(not(target_os = "linux"))]
#[path = "native_export.rs"]
mod native_export;

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
        let authorize = || {
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
        };
        #[cfg(target_os = "linux")]
        {
            publish::publish_new(
                target,
                self.read.content.text.as_bytes(),
                "txt",
                1,
                authorize,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let saved =
                native_export::publish(target, self.read.content.text.as_bytes(), authorize)?;
            Ok(ExportOutcome {
                status: "complete".into(),
                path: Some(target.display().to_string()),
                count: 1,
                warning: saved.warning,
            })
        }
    }
}
