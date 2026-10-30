//! New-byte staging and no-replace publication relative to retained directories.
//!
//! Linux owns an anonymous O_TMPFILE inode. Darwin owns a randomly named private
//! lease, which is deliberately not described as anonymous. Windows owns the
//! original writable/DELETE handle with write/delete sharing denied. Drop never
//! unlinks a name: interrupted named leases remain recovery evidence.
//!
//! This is not a durable transaction journal or an immutable-file sandbox. The
//! caller owns admission, recovery records, and every handle it clones while
//! writing. Sealing withdraws this API's mutable access, brackets SHA-256 reads
//! with native metadata, and publication repeats those checks. An attempted
//! commit must be reconciled after any uncertain result, never blindly retried.
use super::{metadata, Directory, Identity, Kind, Leaf, Links, Metadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};

#[cfg(unix)]
#[path = "stage_unix.rs"]
mod native;
#[cfg(windows)]
#[path = "stage_windows.rs"]
mod native;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    AnonymousFd,
    NamedLease,
    ExclusiveHandleLease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceResidue {
    None,
    LeasePresent,
    NameReplaced,
    Unknown,
}

/// Native names use Serde's OsString representation (Unix bytes / Windows
/// UTF-16), preserving non-UTF8 and unpaired UTF-16 units in recovery records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub parent_identity: Identity,
    pub source_name: Option<OsString>,
    pub identity: Identity,
    pub len: u64,
    pub sha256: [u8; 32],
    pub source_kind: SourceKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub source: Evidence,
    pub target_parent: Identity,
    pub target_name: OsString,
    pub observed_target: Option<Identity>,
    pub source_residue: SourceResidue,
    /// True only after target binding, source disappearance and full content
    /// checks. False does not prove that the namespace commit did not happen.
    pub publication_confirmed: bool,
    pub directories_synced: bool,
}

pub struct Stage {
    parent: Directory,
    parent_identity: Identity,
    source_name: Option<Leaf>,
    file: File,
}

pub struct Sealed {
    stage: Stage,
    stamp: Metadata,
    evidence: Evidence,
    attempted: bool,
}

pub struct SealError {
    pub stage: Stage,
    pub error: io::Error,
}

pub struct PublishError {
    pub stage: Sealed,
    pub error: io::Error,
}

pub enum PublishOutcome {
    Published {
        receipt: Receipt,
    },
    Exists {
        stage: Sealed,
    },
    MayHavePublished {
        stage: Sealed,
        receipt: Receipt,
        error: io::Error,
    },
}

macro_rules! stage_error {
    ($name:ident) => {
        impl std::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("error", &self.error)
                    .finish()
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.error.fmt(formatter)
            }
        }
        impl std::error::Error for $name {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.error)
            }
        }
    };
}
stage_error!(SealError);
stage_error!(PublishError);

impl Stage {
    pub fn new(parent: &Directory) -> io::Result<Self> {
        let parent_metadata = parent.metadata()?;
        check_parent(&parent_metadata)?;
        let (file, source_name) = native::new(parent)?;
        let stage = Self {
            parent: parent.clone(),
            parent_identity: parent_metadata.identity,
            source_name,
            file,
        };
        stage.check_source()?;
        Ok(stage)
    }

    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }
    pub fn source_name(&self) -> Option<&Leaf> {
        self.source_name.as_ref()
    }
    pub fn parent(&self) -> &Directory {
        &self.parent
    }

    pub fn seal(mut self, max_bytes: u64) -> Result<Sealed, SealError> {
        let result = (|| {
            self.check_source()?;
            self.file.sync_all()?;
            let (stamp, sha256) = hash(&mut self.file, max_bytes)?;
            self.check_source()?;
            if metadata(&self.file)? != stamp {
                return Err(changed("Stage changed while sealing"));
            }
            Ok((stamp, sha256))
        })();
        match result {
            Ok((stamp, sha256)) => {
                let evidence = Evidence {
                    parent_identity: self.parent_identity.clone(),
                    source_name: self
                        .source_name
                        .as_ref()
                        .map(|name| name.as_os_str().to_owned()),
                    identity: stamp.identity.clone(),
                    len: stamp.len,
                    sha256,
                    source_kind: native::SOURCE_KIND,
                };
                Ok(Sealed {
                    stage: self,
                    stamp,
                    evidence,
                    attempted: false,
                })
            }
            Err(error) => Err(SealError { stage: self, error }),
        }
    }

    fn check_source(&self) -> io::Result<()> {
        let parent = self.parent.metadata()?;
        check_parent(&parent)?;
        if parent.identity != self.parent_identity {
            return Err(changed("Stage parent identity changed"));
        }
        let current = metadata(&self.file)?;
        current.require_private()?;
        if current.kind != Kind::Regular
            || current.reparse_tag.is_some()
            || current.links != native::INITIAL_LINKS
        {
            return Err(changed(
                "Stage is not a new private file with its original link count",
            ));
        }
        if let Some(name) = &self.source_name {
            self.parent.recheck(name, &current.identity)?;
        }
        Ok(())
    }
}

impl Sealed {
    pub fn evidence(&self) -> &Evidence {
        &self.evidence
    }
    pub fn source_name(&self) -> Option<&Leaf> {
        self.stage.source_name()
    }
    pub fn parent(&self) -> &Directory {
        self.stage.parent()
    }

    pub fn publish_new(
        self,
        target: &Directory,
        leaf: &Leaf,
    ) -> Result<PublishOutcome, PublishError> {
        self.publish_with(target, leaf, native::sync_parent)
    }

    fn publish_with(
        mut self,
        target: &Directory,
        leaf: &Leaf,
        mut sync_parent: impl FnMut(&Directory) -> io::Result<()>,
    ) -> Result<PublishOutcome, PublishError> {
        let preparation = (|| {
            if self.attempted {
                return Err(changed(
                    "An attempted stage requires receipt reconciliation before any retry",
                ));
            }
            self.check_seal()?;
            let target_metadata = target.metadata()?;
            if !same_filesystem(&self.evidence.identity, &target_metadata.identity) {
                return Err(changed("Publication crosses filesystems"));
            }
            let exists = target.entry(leaf)?.is_some();
            Ok((target_metadata.identity, exists))
        })();
        let (target_identity, exists) = match preparation {
            Ok(value) => value,
            Err(error) => return Err(PublishError { stage: self, error }),
        };
        if exists {
            return Ok(PublishOutcome::Exists { stage: self });
        }
        let mut receipt = Receipt {
            source: self.evidence.clone(),
            target_parent: target_identity,
            target_name: leaf.as_os_str().to_owned(),
            observed_target: None,
            source_residue: self.residue(),
            publication_confirmed: false,
            directories_synced: false,
        };
        self.attempted = true;
        if let Err(error) = native::publish(&self.stage, target, leaf) {
            if error.kind() == io::ErrorKind::AlreadyExists {
                // A no-replace collision did not commit this call. Keeping the
                // sealed handle lets a caller choose another target explicitly.
                self.attempted = false;
                return Ok(PublishOutcome::Exists { stage: self });
            }
            receipt.source_residue = self.residue();
            receipt.observed_target = target.entry(leaf).ok().flatten().map(|m| m.identity);
            return Ok(PublishOutcome::MayHavePublished {
                stage: self,
                receipt,
                error,
            });
        }
        let committed_stamp = match self.verify_publication(target, leaf, &mut receipt) {
            Ok(stamp) => stamp,
            Err(error) => {
                receipt.source_residue = self.residue();
                return Ok(PublishOutcome::MayHavePublished {
                    stage: self,
                    receipt,
                    error,
                });
            }
        };
        // No rollback follows any post-commit failure. File and both affected
        // directories receive real native flushes; unsupported flushes are Err.
        let synced = (|| {
            self.stage.file.sync_all()?;
            sync_parent(target)?;
            if self.stage.source_name.is_some()
                && self.evidence.parent_identity != receipt.target_parent
            {
                sync_parent(&self.stage.parent)?;
            }
            Ok(())
        })();
        if let Err(error) = synced {
            receipt.source_residue = self.residue();
            return Ok(PublishOutcome::MayHavePublished {
                stage: self,
                receipt,
                error,
            });
        }
        let final_check = self
            .verify_publication(target, leaf, &mut receipt)
            .and_then(|stamp| {
                // Rename/link may change ctime once. Subsequent flushes cannot
                // authorize another metadata revision, even if bytes were restored.
                if stamp == committed_stamp {
                    Ok(())
                } else {
                    Err(changed(
                        "Published file metadata changed across durability checks",
                    ))
                }
            });
        if let Err(error) = final_check {
            receipt.publication_confirmed = false;
            receipt.source_residue = self.residue();
            return Ok(PublishOutcome::MayHavePublished {
                stage: self,
                receipt,
                error,
            });
        }
        receipt.directories_synced = true;
        Ok(PublishOutcome::Published { receipt })
    }

    fn check_seal(&mut self) -> io::Result<()> {
        self.stage.check_source()?;
        let (stamp, digest) = hash(&mut self.stage.file, self.evidence.len)?;
        if stamp != self.stamp || digest != self.evidence.sha256 {
            return Err(changed("Sealed stage content or metadata changed"));
        }
        Ok(())
    }

    fn residue(&self) -> SourceResidue {
        let Some(name) = &self.stage.source_name else {
            return SourceResidue::None;
        };
        match self.stage.parent.entry(name) {
            Ok(None) => SourceResidue::None,
            Ok(Some(current)) if current.identity == self.evidence.identity => {
                SourceResidue::LeasePresent
            }
            Ok(Some(_)) => SourceResidue::NameReplaced,
            Err(_) => SourceResidue::Unknown,
        }
    }

    fn verify_publication(
        &mut self,
        target: &Directory,
        leaf: &Leaf,
        receipt: &mut Receipt,
    ) -> io::Result<Metadata> {
        receipt.publication_confirmed = false;
        let visible = target
            .entry(leaf)?
            .ok_or_else(|| changed("Published target is absent"))?;
        receipt.observed_target = Some(visible.identity.clone());
        if visible.identity != self.evidence.identity
            || visible.kind != Kind::Regular
            || visible.reparse_tag.is_some()
        {
            return Err(changed("Published target is not the sealed source object"));
        }
        // Retain the target's own literal handle across the final checks. Reads
        // hash the original stage handle; identity proves they name one inode.
        let target_file = target.open_regular(leaf, Links::Single)?;
        let target_metadata = metadata(&target_file)?;
        let (current, digest) = hash(&mut self.stage.file, self.evidence.len)?;
        if current.identity != self.stamp.identity
            || current.kind != Kind::Regular
            || current.len != self.stamp.len
            || current.links != 1
            || current.modified != self.stamp.modified
            || current.security != self.stamp.security
            || current.reparse_tag.is_some()
            || digest != self.evidence.sha256
            || target_metadata != current
        {
            return Err(changed(
                "Published file failed source identity/content verification",
            ));
        }
        target.recheck(leaf, &self.evidence.identity)?;
        if target.identity()? != receipt.target_parent {
            return Err(changed("Target parent identity changed"));
        }
        receipt.source_residue = self.residue();
        if receipt.source_residue != SourceResidue::None {
            return Err(changed(
                "Published source lease remains, was replaced, or cannot be checked",
            ));
        }
        receipt.publication_confirmed = true;
        Ok(current)
    }
}

fn hash(file: &mut File, max_bytes: u64) -> io::Result<(Metadata, [u8; 32])> {
    let before = metadata(file)?;
    if before.kind != Kind::Regular || before.reparse_tag.is_some() || before.len > max_bytes {
        return Err(changed(
            "Stage exceeds its byte bound or is not a regular file",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| changed("Stage size overflow"))?;
        if total > max_bytes || total > before.len {
            return Err(changed("Stage grew during hashing"));
        }
        digest.update(&buffer[..count]);
    }
    let after = metadata(file)?;
    if total != before.len || after != before {
        return Err(changed("Stage changed during hashing"));
    }
    Ok((after, digest.finalize().into()))
}

fn same_filesystem(left: &Identity, right: &Identity) -> bool {
    match (left, right) {
        (Identity::Unix { device: a, .. }, Identity::Unix { device: b, .. }) => a == b,
        (Identity::Windows { volume: a, .. }, Identity::Windows { volume: b, .. }) => a == b,
        _ => false,
    }
}
fn check_parent(parent: &Metadata) -> io::Result<()> {
    if parent.kind != Kind::Directory || parent.reparse_tag.is_some() {
        return Err(changed("Stage parent is not a literal directory"));
    }
    // Anonymous Linux staging has no source name to guess, replace or expose.
    // Preserve existing O_TMPFILE repair of ordinary 0755 library directories;
    // a held directory plus native no-replace publication is the authority.
    // Named Darwin/Windows leases still require an actually private parent.
    if native::SOURCE_KIND != SourceKind::AnonymousFd {
        parent.require_private()?;
    }
    Ok(())
}
fn changed(message: &'static str) -> io::Error {
    io::Error::other(message)
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
