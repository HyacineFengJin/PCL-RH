//! Native read capabilities and immutable, launcher-owned provenance. User
//! sources remain physical files; only explicit binding records are Documents.
use super::*;
use crate::launcher_local::filesystem::{documents::Document, Dir as LocalDir};
use pcl_core::retained_fs::{self, Directory, Kind, Leaf, Links, Traversal};
use std::{
    io::Read,
    path::{Component, PathBuf},
};

pub(super) struct Dir(Directory);
fn leaf(name: &str) -> Result<Leaf, String> {
    Leaf::new(name).map_err(error)
}
fn key(identity: retained_fs::Identity) -> Result<Identity, String> {
    match identity {
        #[cfg(unix)]
        retained_fs::Identity::Unix { device, inode } => Ok(Identity { device, inode }),
        #[cfg(windows)]
        identity @ retained_fs::Identity::Windows { .. } => Ok(identity),
        _ => Err("Harness identity belongs to another platform".into()),
    }
}
impl Dir {
    pub(super) fn absolute(path: &Path, create: bool) -> Result<Self, String> {
        retained_fs::validate_absolute_path(path).map_err(error)?;
        if !create {
            return Directory::open_absolute(path).map(Self).map_err(error);
        }
        let mut base = PathBuf::new();
        let mut names = Vec::new();
        for part in path.components() {
            match part {
                Component::Prefix(_) | Component::RootDir => base.push(part.as_os_str()),
                Component::Normal(name) => {
                    names.push(leaf(name.to_str().ok_or("Harness paths must be UTF-8")?)?)
                }
                _ => return Err("Harness store contains relative path components".into()),
            }
        }
        if names.len() > 64 {
            return Err("Harness store path is too deep".into());
        }
        let mut directory = Directory::open_absolute(&base).map_err(error)?;
        for name in names {
            directory = if directory.entry(&name).map_err(error)?.is_some() {
                directory
                    .child(&name, Traversal::AllowMounts)
                    .map_err(error)?
            } else {
                let child = directory
                    .create_private(&name, Traversal::AllowMounts)
                    .map_err(error)?;
                directory.sync().map_err(error)?;
                directory
                    .recheck(&name, &child.identity().map_err(error)?)
                    .map_err(error)?;
                child
            };
        }
        directory
            .metadata()
            .map_err(error)?
            .require_private()
            .map_err(error)?;
        Ok(Self(directory))
    }
    pub(super) fn key(&self) -> Result<Identity, String> {
        key(self.0.identity().map_err(error)?)
    }
    pub(super) fn child(&self, path: &str) -> Result<Self, String> {
        let mut directory = self.0.reopen(Traversal::SameFilesystem).map_err(error)?;
        if !path.is_empty() {
            for name in path.split('/') {
                directory = directory
                    .child(&leaf(name)?, Traversal::SameFilesystem)
                    .map_err(error)?;
            }
        }
        Ok(Self(directory))
    }
    pub(super) fn optional_child(&self, name: &str) -> Result<Option<Self>, String> {
        if self.0.entry(&leaf(name)?).map_err(error)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(super) fn ensure_child(&self, name: &str) -> Result<Self, String> {
        self.0
            .metadata()
            .map_err(error)?
            .require_private()
            .map_err(error)?;
        let name = leaf(name)?;
        let directory = if self.0.entry(&name).map_err(error)?.is_some() {
            self.0
                .child(&name, Traversal::SameFilesystem)
                .map_err(error)?
        } else {
            let child = self
                .0
                .create_private(&name, Traversal::SameFilesystem)
                .map_err(error)?;
            self.0.sync().map_err(error)?;
            self.0
                .recheck(&name, &child.identity().map_err(error)?)
                .map_err(error)?;
            child
        };
        directory
            .metadata()
            .map_err(error)?
            .require_private()
            .map_err(error)?;
        Ok(Self(directory))
    }
    pub(super) fn names(&self) -> Result<Vec<String>, String> {
        let mut names = self
            .0
            .names(MAX_ENTRIES)
            .map_err(error)?
            .into_iter()
            .map(|name| {
                name.into_string()
                    .map_err(|_| "Harness tree contains a non-UTF-8 filename".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        Ok(names)
    }
    pub(super) fn facts(&self, name: &str) -> Result<Option<Facts>, String> {
        self.0.entry(&leaf(name)?).map_err(error).map(|value| {
            value.map(|value| Facts {
                kind: match value.kind {
                    Kind::Directory if value.reparse_tag.is_none() => NodeKind::Directory,
                    Kind::Regular if value.reparse_tag.is_none() => NodeKind::Regular,
                    _ => NodeKind::Other,
                },
                size: value.len,
                links: value.links,
            })
        })
    }
    pub(super) fn read(&self, name: &str, limit: usize) -> Result<(FileState, Vec<u8>), String> {
        let name = leaf(name)?;
        let mut file = self.0.open_regular(&name, Links::Single).map_err(error)?;
        let before = retained_fs::metadata(&file).map_err(error)?;
        if before.len > limit as u64 || before.reparse_tag.is_some() {
            return Err("Harness requires a bounded regular file with one link".into());
        }
        self.0.recheck(&name, &before.identity).map_err(error)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        let after = retained_fs::metadata(&file).map_err(error)?;
        self.0.recheck(&name, &after.identity).map_err(error)?;
        if before != after
            || after.links != 1
            || bytes.len() > limit
            || bytes.len() as u64 != after.len
        {
            return Err("Harness file changed while it was read".into());
        }
        #[cfg(unix)]
        let mode = match &after.security {
            retained_fs::Security::Unix { mode, .. } => mode & 0o777,
            _ => return Err("Harness file security belongs to another platform".into()),
        };
        Ok((
            FileState {
                identity: key(after.identity.clone())?,
                sha256: hash(&bytes),
                size: bytes.len(),
                #[cfg(unix)]
                mode,
                #[cfg(windows)]
                metadata: after,
            },
            bytes,
        ))
    }
    fn local(&self) -> Result<LocalDir, String> {
        self.0
            .metadata()
            .map_err(error)?
            .require_private()
            .map_err(error)?;
        Ok(LocalDir(self.0.try_clone_file().map_err(error)?))
    }
    pub(super) fn read_binding(&self, name: &str) -> Result<Vec<u8>, String> {
        let local = self.local()?;
        let document = Document::new(&local, name, MAX_RECORD as u64)?;
        let mut snapshot = document
            .snapshot()?
            .ok_or("Native Harness binding is missing; retained store")?;
        document.bytes(&mut snapshot)
    }
    pub(super) fn immutable(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() || bytes.len() > MAX_RECORD {
            return Err("Harness binding exceeds its record limit".into());
        }
        let local = self.local()?;
        let document = Document::new(&local, name, MAX_RECORD as u64)?;
        #[cfg(windows)]
        {
            use crate::launcher_local::filesystem::documents::DocumentPresence;
            let mut state = document.state()?;
            match document.read_state(&mut state)? {
                Some(previous) if previous == bytes => return Ok(()),
                Some(_) => {
                    return Err("Immutable Harness record differs; retained for recovery".into())
                }
                None if state.presence() != DocumentPresence::NeverCreated => {
                    return Err("Harness binding was deleted; retained for recovery".into())
                }
                None => {}
            }
            document.compare_write(&state, Some(bytes))?;
        }
        #[cfg(unix)]
        {
            if let Some(mut previous) = document.snapshot()? {
                if document.bytes(&mut previous)? == bytes {
                    return Ok(());
                }
                return Err("Immutable Harness record differs; retained for recovery".into());
            }
            document.replace(None, bytes)?;
        }
        if self.read_binding(name)? != bytes {
            return Err(
                "MayHavePublished: Harness binding confirmation differs; retained store".into(),
            );
        }
        Ok(())
    }
}
