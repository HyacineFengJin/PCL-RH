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
    pub fn entry(&self, name: &Leaf) -> io::Result<Option<Metadata>> {
        native::entry(&self.file, name.as_os_str())
    }
    pub fn open_regular(&self, name: &Leaf, links: Links) -> io::Result<File> {
        let file = native::open_regular(&self.file, name.as_os_str())?;
        regular(&file, links)?;
        Ok(file)
    }
    pub fn create_file(&self, name: &Leaf) -> io::Result<File> {
        let file = native::create_file(&self.file, name.as_os_str())?;
        regular(&file, Links::Single)?.require_private()?;
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
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn conflict(message: &'static str) -> io::Error {
    io::Error::other(message)
}

#[cfg(test)]
mod tests;
