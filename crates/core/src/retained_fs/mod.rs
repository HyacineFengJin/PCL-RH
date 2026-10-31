//! Native directory capabilities. Every descendant lookup accepts one literal
//! name and starts at a held directory, never a reconstructed parent pathname.
//! Identity, ownership and change evidence are native platform data; no Unix
//! uid, inode or permission bits are fabricated for Windows.
//!
//! This layer does not supply a transaction protocol. Callers still authorize
//! roots, retain/revalidate their identity graph, bound inputs, and reconcile
//! publication receipts and durable journals. Same-filesystem admission on
//! Darwin checks the opened directory's dev/fsid before exposing it; Linux uses
//! atomic openat2 NO_XDEV. Neither permits a pathname fallback on failure.
use serde::{Deserialize, Serialize};
pub mod stage;
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::{Component, Path},
    sync::Arc,
};

#[cfg(unix)]
#[path = "unix.rs"]
mod native;
#[cfg(windows)]
#[path = "windows.rs"]
mod native;
#[cfg(windows)]
pub(crate) mod windows_security;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leaf(OsString);
impl Leaf {
    pub fn new(name: impl AsRef<OsStr>) -> io::Result<Self> {
        let name = name.as_ref();
        native::validate_leaf(name)?;
        Ok(Self(name.to_owned()))
    }
    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "lowercase", deny_unknown_fields)]
pub enum Identity {
    Unix { device: u64, inode: u64 },
    Windows { volume: u64, file: [u8; 16] },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Directory,
    Regular,
    Link,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "lowercase", deny_unknown_fields)]
pub enum Security {
    Unix {
        owner: u32,
        mode: u32,
        extended_acl: bool,
    },
    Windows {
        owner: Vec<u8>,
        descriptor: Vec<u8>,
        owner_is_current: bool,
        private: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Time {
    pub seconds: i64,
    pub nanos: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub identity: Identity,
    pub kind: Kind,
    pub len: u64,
    pub links: u64,
    pub modified: Time,
    pub changed: Time,
    pub security: Security,
    pub reparse_tag: Option<u32>,
}
impl Metadata {
    pub fn same_object(&self, other: &Self) -> bool {
        self.identity == other.identity && self.kind == other.kind
    }
    /// Rename can change ctime, but it must not change this content evidence.
    pub fn moved_matches(&self, other: &Self) -> bool {
        self.same_object(other)
            && self.len == other.len
            && self.links == other.links
            && self.modified == other.modified
            && self.security == other.security
            && self.reparse_tag == other.reparse_tag
    }
    pub fn require_owned(&self) -> io::Result<()> {
        native::require_owned(self)
    }
    pub fn require_private(&self) -> io::Result<()> {
        native::require_private(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Traversal {
    AllowMounts,
    SameFilesystem,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Links {
    Single,
    Any,
}

/// Result of removing one caller-authorized private-workspace entry. This is
/// not a conditional delete primitive for user originals or a transaction log.
#[derive(Debug)]
pub enum WorkspaceRemoval {
    Removed,
    /// A native deletion was attempted. The name or durability could not be
    /// confirmed afterwards; retain the remaining tree and inspect it again.
    MayHaveRemoved {
        error: io::Error,
    },
}

/// Validate a physical filesystem path without opening it. Windows disk/UNC
/// roots (including their verbatim forms) are supported; device namespaces,
/// relative paths and explicit dot/parent components are rejected.
pub fn validate_absolute_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(invalid("Expected an absolute filesystem path"));
    }
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes
        .split(|byte| *byte == b'/' || (cfg!(windows) && *byte == b'\\'))
        .any(|part| matches!(part, b"." | b".."))
    {
        return Err(invalid(
            "Explicit dot/parent path components are not allowed",
        ));
    }
    // This is syntax validation only. Capability admission owns its own
    // traversal-depth budget; old durable paths gain no new depth restriction.
    native::absolute_parts(path).map(|_| ())
}

#[derive(Clone)]
pub struct Directory {
    file: Arc<File>,
    // Also retain ancestors. Native I/O never relies on their current names.
    _parent: Option<Arc<Directory>>,
}
impl Directory {
    pub fn from_file(file: File) -> io::Result<Self> {
        let metadata = metadata(&file)?;
        if metadata.kind != Kind::Directory || metadata.reparse_tag.is_some() {
            return Err(invalid("Expected a literal directory handle"));
        }
        Ok(Self {
            file: Arc::new(file),
            _parent: None,
        })
    }
    /// The user-selected root may itself be on a separate volume. Reject all
    /// symlink/reparse components; apply SameFilesystem only below admission.
    pub fn open_absolute(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("Expected an absolute directory path"));
        }
        let (root, names) = native::absolute_parts(path)?;
        if names.len() > 64 {
            return Err(invalid("Directory path is too deep"));
        }
        let mut directory = Self::from_file(native::open_root(&root)?)?;
        for name in names {
            directory = directory.child(&Leaf::new(name)?, Traversal::AllowMounts)?;
        }
        Ok(directory)
    }
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn try_clone_file(&self) -> io::Result<File> {
        self.file.try_clone()
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        metadata(&self.file)
    }
    pub fn identity(&self) -> io::Result<Identity> {
        Ok(self.metadata()?.identity)
    }
    /// Observe the current physical parent for ancestry checks. This does not
    /// freeze a directory graph or authorize writes outside an admitted root.
    /// Unix opens `..` relative to this held inode. Windows queries its current
    /// native name, walks without reparse points, and binds the observed child
    /// back to this full file identity before and after the lookup. Unavailable
    /// names or concurrent changes fail; no lexical caller path is substituted.
    pub fn parent_snapshot(&self) -> io::Result<Self> {
        let before = self.metadata()?;
        if before.kind != Kind::Directory || before.reparse_tag.is_some() {
            return Err(invalid("Ancestry source is not a literal directory"));
        }
        let parent = Self::from_file(native::parent_snapshot(&self.file)?)?;
        let after = self.metadata()?;
        if !before.same_object(&after) || after.reparse_tag.is_some() {
            return Err(conflict("Ancestry source changed during observation"));
        }
        Ok(parent)
    }
    pub fn child(&self, name: &Leaf, policy: Traversal) -> io::Result<Self> {
        let file = native::open_directory(&self.file, name.as_os_str(), policy)?;
        let mut directory = Self::from_file(file)?;
        directory._parent = Some(Arc::new(self.clone()));
        Ok(directory)
    }
    pub fn reopen(&self, policy: Traversal) -> io::Result<Self> {
        let file = native::reopen_directory(&self.file, policy)?;
        let mut directory = Self::from_file(file)?;
        directory._parent = Some(Arc::new(self.clone()));
        Ok(directory)
    }
    /// Creates one new private directory. Existing entries fail; no permission
    /// repair, chmod of an existing object, or recursive path creation occurs.
    pub fn create_private(&self, name: &Leaf, policy: Traversal) -> io::Result<Self> {
        let file = native::create_directory(&self.file, name.as_os_str(), policy)?;
        let mut directory = Self::from_file(file)?;
        directory.metadata()?.require_private()?;
        directory._parent = Some(Arc::new(self.clone()));
        Ok(directory)
    }
    /// Allocate a new private installation workspace with a fixed literal
    /// prefix and 128 bits of system randomness. No mutable parent path or
    /// permission repair is used. The caller owns namespace durability.
    pub fn create_private_workspace(&self) -> io::Result<(Leaf, Self)> {
        let parent = self.metadata()?;
        if parent.kind != Kind::Directory || parent.reparse_tag.is_some() {
            return Err(invalid("Workspace parent is not a literal directory"));
        }
        for _ in 0..32 {
            let mut entropy = [0u8; 16];
            native::workspace_entropy(&mut entropy)?;
            let suffix = entropy
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let name = Leaf::new(format!(".install-{suffix}"))?;
            match self.create_private(&name, Traversal::SameFilesystem) {
                Ok(directory) => {
                    self.recheck(&name, &directory.identity()?)?;
                    return Ok((name, directory));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(conflict("Could not allocate a unique private workspace"))
    }

    /// Remove one literal regular file or empty directory from a temporary
    /// subtree the caller already owns and authorizes for cleanup. Privacy is
    /// established at that workspace root: generated children may have normal
    /// 0644/0755 permissions. Links, multiple file hard links and special nodes
    /// are refused. Callers must preflight the whole tree before recursion.
    ///
    /// POSIX rechecking an identity and unlinkat are separate operations, not
    /// atomic expected-FD deletion. Concurrent replacement can therefore affect
    /// the final name. This API must never be used to delete user originals.
    pub fn remove_workspace_entry(
        &self,
        name: &Leaf,
        expected: &Metadata,
    ) -> io::Result<WorkspaceRemoval> {
        self.remove_workspace_entry_with_sync(name, expected, Self::sync)
    }
    fn remove_workspace_entry_with_sync(
        &self,
        name: &Leaf,
        expected: &Metadata,
        sync_parent: impl FnOnce(&Self) -> io::Result<()>,
    ) -> io::Result<WorkspaceRemoval> {
        self.remove_generated_entry(name, expected, false, sync_parent)
    }
    /// Remove one journal-authorized link to newly generated bytes, including
    /// a file having other hard links. This does not establish that authority:
    /// the caller must first reconcile its durable journal and verify the full
    /// content hash and identity. Never use it for a user's input, an existing
    /// reused dependency, or an unrecognized name. The ordinary workspace API
    /// continues to require single-link files.
    ///
    /// Unix metadata checks and unlinkat remain separate operations; this is
    /// not an atomic expected-FD deletion. Windows removes the held opened link.
    pub fn remove_generated_link(
        &self,
        name: &Leaf,
        expected: &Metadata,
    ) -> io::Result<WorkspaceRemoval> {
        self.remove_generated_entry(name, expected, true, Self::sync)
    }
    fn remove_generated_entry(
        &self,
        name: &Leaf,
        expected: &Metadata,
        multiple_links: bool,
        sync_parent: impl FnOnce(&Self) -> io::Result<()>,
    ) -> io::Result<WorkspaceRemoval> {
        let node = |metadata: &Metadata| {
            if multiple_links {
                if metadata.kind != Kind::Regular
                    || metadata.links == 0
                    || metadata.reparse_tag.is_some()
                {
                    return Err(invalid(
                        "Generated-link cleanup requires a literal regular file",
                    ));
                }
                metadata.require_owned()
            } else {
                workspace_node(metadata)
            }
        };
        let parent = self.metadata()?;
        if parent.kind != Kind::Directory || parent.reparse_tag.is_some() {
            return Err(invalid("Workspace parent is not a literal directory"));
        }
        node(expected)?;
        let visible = self
            .entry(name)?
            .ok_or_else(|| conflict("Workspace entry disappeared before cleanup"))?;
        node(&visible)?;
        if !visible.same_object(expected) {
            return Err(conflict("Workspace entry was replaced before cleanup"));
        }
        let held = native::open_workspace_entry(&self.file, name.as_os_str(), expected.kind)?;
        let actual = metadata(&held)?;
        node(&actual)?;
        if !actual.same_object(expected) {
            return Err(conflict(
                "Workspace entry changed while opening cleanup handle",
            ));
        }
        if multiple_links && actual != *expected {
            return Err(conflict(
                "Generated link content metadata changed before cleanup",
            ));
        }
        if actual.kind == Kind::Directory {
            let directory = Self::from_file(held.try_clone()?)?;
            if !directory.names(1)?.is_empty() {
                return Err(conflict("Workspace directory is not empty"));
            }
        }
        self.recheck(name, &actual.identity)?;
        // The held object must still have the admitted kind and link policy.
        // No chmod, attribute changes, retries or name-based rollback follows.
        let final_metadata = metadata(&held)?;
        node(&final_metadata)?;
        if multiple_links && final_metadata != *expected {
            return Err(conflict("Generated link changed while preparing cleanup"));
        }
        if let Err(error) =
            native::remove_workspace_entry(&self.file, name.as_os_str(), held, actual.kind)
        {
            return Ok(WorkspaceRemoval::MayHaveRemoved { error });
        }
        let confirmation = (|| {
            if self.entry(name)?.is_some() {
                return Err(conflict("Workspace cleanup name remains or was recreated"));
            }
            sync_parent(self)?;
            let current = self.metadata()?;
            if !current.same_object(&parent)
                || current.reparse_tag.is_some()
                || self.entry(name)?.is_some()
            {
                return Err(conflict(
                    "Workspace parent or cleanup name changed during flush",
                ));
            }
            Ok(())
        })();
        match confirmation {
            Ok(()) => Ok(WorkspaceRemoval::Removed),
            Err(error) => Ok(WorkspaceRemoval::MayHaveRemoved { error }),
        }
    }
    pub fn entry(&self, name: &Leaf) -> io::Result<Option<Metadata>> {
        native::entry(&self.file, name.as_os_str())
    }
    pub fn open_regular(&self, name: &Leaf, links: Links) -> io::Result<File> {
        let file = native::open_regular(&self.file, name.as_os_str())?;
        regular(&file, links)?;
        Ok(file)
    }
    /// Windows read lease for one literal private single-link file. The native
    /// handle admits only READ sharing, so existing/new data writers and DELETE
    /// handles are refused for this lease's lifetime. Ordinary readonly handles
    /// opened with READ/WRITE/DELETE sharing remain compatible.
    ///
    /// `allow_generated_delete` additionally requests DELETE for caller-proven
    /// app-generated payload GC. It never establishes that recovery authority;
    /// callers must bind a durable authorization and verify content themselves.
    #[cfg(windows)]
    pub fn read_lease(&self, name: &Leaf, allow_generated_delete: bool) -> io::Result<ReadLease> {
        let parent = self.metadata()?;
        parent.require_private()?;
        if parent.kind != Kind::Directory || parent.reparse_tag.is_some() {
            return Err(invalid("Read lease parent is not a literal directory"));
        }
        let file = native::open_read_lease(&self.file, name.as_os_str(), allow_generated_delete)?;
        let stamp = regular(&file, Links::Single)?;
        stamp.require_private()?;
        self.recheck(name, &stamp.identity)?;
        let lease = ReadLease {
            parent_identity: parent.identity,
            parent: self.clone(),
            name: name.clone(),
            file,
            stamp,
            allow_generated_delete,
        };
        lease.recheck()?;
        Ok(lease)
    }
    pub fn create_file(&self, name: &Leaf) -> io::Result<File> {
        let file = native::create_file(&self.file, name.as_os_str())?;
        regular(&file, Links::Single)?.require_private()?;
        Ok(file)
    }
    /// Append to one private single-link ordinary file, or create it privately
    /// with no replacement. Never truncates; validates the actual opened EOF
    /// before returning write authority. The caller bounds subsequent output.
    /// Initial parent acquisition and later pathname revalidation are caller
    /// responsibilities; this operation uses only this retained directory.
    pub fn open_append(&self, name: &Leaf, max_existing_bytes: u64) -> io::Result<File> {
        let parent = self.metadata()?;
        if parent.kind != Kind::Directory || parent.reparse_tag.is_some() {
            return Err(invalid("Append parent is not a literal directory"));
        }
        let before = self.entry(name)?;
        if let Some(before) = &before {
            if before.kind != Kind::Regular
                || before.reparse_tag.is_some()
                || before.links != 1
                || before.len > max_existing_bytes
            {
                return Err(invalid(
                    "Append target is not a bounded single-link regular file",
                ));
            }
            before.require_owned()?;
        }
        let file = native::open_append(&self.file, name.as_os_str(), before.is_none())?;
        let opened = regular(&file, Links::Single)?;
        opened.require_private()?;
        if opened.len > max_existing_bytes
            || before.as_ref().is_some_and(|old| !opened.same_object(old))
        {
            return Err(conflict("Append file changed or exceeds the allowed EOF"));
        }
        self.recheck(name, &opened.identity)?;
        if !self.metadata()?.same_object(&parent) {
            return Err(conflict("Append parent identity changed"));
        }
        // Real native directory sync, including for newly created log names.
        self.sync()?;
        self.recheck(name, &opened.identity)?;
        let current = regular(&file, Links::Single)?;
        current.require_private()?;
        if current.len > max_existing_bytes || !current.same_object(&opened) {
            return Err(conflict(
                "Append file changed during durability confirmation",
            ));
        }
        Ok(file)
    }
    pub fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
        if limit == 0 || limit > 1_000_000 {
            return Err(invalid("Invalid directory entry limit"));
        }
        native::names(&self.file, limit)
    }
    pub fn recheck(&self, name: &Leaf, expected: &Identity) -> io::Result<()> {
        match self.entry(name)? {
            Some(current)
                if current.identity == *expected
                    && current.reparse_tag.is_none()
                    && !matches!(current.kind, Kind::Link | Kind::Other) =>
            {
                Ok(())
            }
            _ => Err(conflict("Directory entry identity has changed")),
        }
    }
    pub fn lock(&self, name: &Leaf) -> io::Result<Lock> {
        let file = native::open_lock(&self.file, name.as_os_str())?;
        let before = regular(&file, Links::Single)?;
        before.require_private()?;
        native::lock(&file)?;
        let lock = Lock(file);
        self.recheck(name, &before.identity)?;
        if metadata(&lock.0)? != before {
            return Err(conflict("Lock file changed during admission"));
        }
        Ok(lock)
    }
    /// Actual native flush only. Unsupported durability is an error, never a
    /// silent success. A successful flush is not a whole-transaction receipt.
    pub fn sync(&self) -> io::Result<()> {
        native::sync_directory(&self.file)
    }
}

/// Identity-bound Windows READ-sharing lease. This is not a transaction or a
/// proof that a file is launcher generated. Never use generated removal on a
/// user's physical original/base, even if its bytes happen to match a payload.
#[cfg(windows)]
pub struct ReadLease {
    parent_identity: Identity,
    parent: Directory,
    name: Leaf,
    file: File,
    stamp: Metadata,
    allow_generated_delete: bool,
}
#[cfg(windows)]
impl ReadLease {
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn metadata(&self) -> &Metadata {
        &self.stamp
    }
    pub fn recheck(&self) -> io::Result<()> {
        let parent = self.parent.metadata()?;
        parent.require_private()?;
        if parent.identity != self.parent_identity
            || parent.kind != Kind::Directory
            || parent.reparse_tag.is_some()
        {
            return Err(conflict("Read lease parent identity or type changed"));
        }
        self.parent.recheck(&self.name, &self.stamp.identity)?;
        if metadata(&self.file)? != self.stamp {
            return Err(conflict("Read leased file metadata changed"));
        }
        Ok(())
    }
    /// Remove this exact held link only after the caller verifies a durable GC
    /// authorization for an app-generated, non-current payload and its digest.
    /// No name reopen, path deletion or chmod fallback is used.
    pub fn remove_generated(self) -> io::Result<WorkspaceRemoval> {
        if !self.allow_generated_delete {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "This read lease has no generated deletion authority",
            ));
        }
        self.recheck()?;
        let Self {
            parent, name, file, ..
        } = self;
        if let Err(error) =
            native::remove_workspace_entry(parent.file(), name.as_os_str(), file, Kind::Regular)
        {
            return Ok(WorkspaceRemoval::MayHaveRemoved { error });
        }
        match parent.entry(&name).and_then(|entry| {
            if entry.is_some() {
                return Err(conflict("Generated cleanup name remains or was recreated"));
            }
            parent.sync()?;
            if parent.entry(&name)?.is_some() {
                return Err(conflict("Generated cleanup name reappeared during sync"));
            }
            Ok(())
        }) {
            Ok(()) => Ok(WorkspaceRemoval::Removed),
            Err(error) => Ok(WorkspaceRemoval::MayHaveRemoved { error }),
        }
    }
}

pub struct Lock(File);
impl Lock {
    pub fn file(&self) -> &File {
        &self.0
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        native::unlock(&self.0);
    }
}

/// Probe an already-open existing lock with a real nonblocking shared lock.
/// Returns true only when an incompatible exclusive lock is held. A successful
/// shared probe is explicitly unlocked before return, even with duplicated FDs.
/// This function neither opens nor creates a lock filename.
pub fn shared_lock_is_held(file: &File) -> io::Result<bool> {
    let before = regular(file, Links::Single)?;
    before.require_private()?;
    let acquired = native::try_shared_lock(file)?;
    if acquired {
        native::unlock_checked(file)?;
    }
    if metadata(file)? != before {
        return Err(conflict("Lock file changed during shared probe"));
    }
    Ok(!acquired)
}

pub fn metadata(file: &File) -> io::Result<Metadata> {
    native::metadata(file)
}
fn regular(file: &File, links: Links) -> io::Result<Metadata> {
    let metadata = metadata(file)?;
    if metadata.kind != Kind::Regular
        || metadata.reparse_tag.is_some()
        || (links == Links::Single && metadata.links != 1)
    {
        return Err(invalid(
            "Expected a regular file with the required link policy",
        ));
    }
    Ok(metadata)
}
fn workspace_node(metadata: &Metadata) -> io::Result<()> {
    if metadata.reparse_tag.is_some()
        || !matches!(metadata.kind, Kind::Regular | Kind::Directory)
        || (metadata.kind == Kind::Regular && metadata.links != 1)
    {
        return Err(invalid("Workspace cleanup refuses links or special nodes"));
    }
    Ok(())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn conflict(message: &'static str) -> io::Error {
    io::Error::other(message)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod workspace_tests;

#[cfg(test)]
mod append_tests;

#[cfg(all(test, windows))]
#[path = "read_lease_windows_tests.rs"]
mod read_lease_windows_tests;
