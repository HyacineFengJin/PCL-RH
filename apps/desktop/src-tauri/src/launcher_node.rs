//! Read-only native observations for diagnostic and repair admission. These
//! identities do not authorize mutation; the operation keeps its own writer
//! guard and retained installer descriptors. Invalid leaf kinds stay visible
//! to diagnostics instead of being followed or opened as regular files.
use pcl_core::retained_fs::{Directory, Identity, Kind, Leaf, Metadata, Traversal};
use std::{io, path::PathBuf};

#[cfg(all(test, windows))]
#[path = "../../../../crates/core/src/file_ops/junction_fixture.rs"]
#[allow(dead_code)]
mod junction_fixture;

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeIdentity {
    identity: Identity,
    kind: Kind,
    reparse: Option<u32>,
}
impl NodeIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            identity: metadata.identity.clone(),
            kind: metadata.kind,
            reparse: metadata.reparse_tag,
        }
    }
}

pub(crate) struct BoundNode {
    path: PathBuf,
    identity: Option<NodeIdentity>,
    pub(crate) directory: Option<Directory>,
}
impl BoundNode {
    pub(crate) fn capture(path: PathBuf) -> Result<Self, String> {
        pcl_core::retained_fs::validate_absolute_path(&path).map_err(|e| e.to_string())?;
        let (identity, directory) = if let Some(name) = path.file_name() {
            let parent_path = path.parent().ok_or("诊断路径缺少父目录")?;
            let parent = match Directory::open_absolute(parent_path) {
                Ok(parent) => Some(parent),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(format!("无法检查实例目录：{e}")),
            };
            match parent {
                None => (None, None),
                Some(parent) => {
                    let name = Leaf::new(name).map_err(|e| e.to_string())?;
                    match parent.entry(&name).map_err(|e| e.to_string())? {
                        None => (None, None),
                        Some(visible)
                            if visible.kind == Kind::Directory && visible.reparse_tag.is_none() =>
                        {
                            let directory = parent
                                .child(&name, Traversal::AllowMounts)
                                .map_err(|e| e.to_string())?;
                            let held = directory.metadata().map_err(|e| e.to_string())?;
                            if !held.same_object(&visible) {
                                return Err("诊断目录在打开期间已变化".into());
                            }
                            parent
                                .recheck(&name, &held.identity)
                                .map_err(|e| e.to_string())?;
                            (Some(NodeIdentity::of(&held)), Some(directory))
                        }
                        Some(visible) => (Some(NodeIdentity::of(&visible)), None),
                    }
                }
            }
        } else {
            let directory = Directory::open_absolute(&path).map_err(|e| e.to_string())?;
            let identity = NodeIdentity::of(&directory.metadata().map_err(|e| e.to_string())?);
            (Some(identity), Some(directory))
        };
        Ok(Self {
            path,
            identity,
            directory,
        })
    }

    pub(crate) fn verify(&self) -> Result<(), String> {
        let fresh = Self::capture(self.path.clone())?;
        if fresh.identity != self.identity {
            return Err("检查期间文件或目录身份已变化，请重新检查".into());
        }
        if let Some(directory) = &self.directory {
            if Some(NodeIdentity::of(
                &directory.metadata().map_err(|e| e.to_string())?,
            )) != self.identity
            {
                return Err("检查期间保留的目录已变化，请重新检查".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn held_directory_detects_substitution_and_missing_nodes_detect_appearance() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::create_dir(root.join("game")).unwrap();
        let bound = BoundNode::capture(root.join("game")).unwrap();
        assert!(bound.directory.is_some());
        bound.verify().unwrap();
        fs::rename(root.join("game"), root.join("original")).unwrap();
        fs::create_dir(root.join("game")).unwrap();
        assert!(bound.verify().is_err());
        assert!(root.join("original").is_dir());
        let missing = BoundNode::capture(root.join("future")).unwrap();
        missing.verify().unwrap();
        fs::write(root.join("future"), b"appeared").unwrap();
        assert!(missing.verify().is_err());
    }

    #[test]
    fn regular_leaf_is_observed_without_treating_it_as_a_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::write(root.join("versions"), b"invalid layout").unwrap();
        let bound = BoundNode::capture(root.join("versions")).unwrap();
        assert!(bound.directory.is_none());
        assert_eq!(bound.identity.as_ref().unwrap().kind, Kind::Regular);
        bound.verify().unwrap();
        fs::remove_file(root.join("versions")).unwrap();
        fs::create_dir(root.join("versions")).unwrap();
        assert!(bound.verify().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_leaf_is_observed_and_ancestor_is_never_followed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::create_dir(root.join("outside")).unwrap();
        fs::write(root.join("outside/keep"), b"unchanged").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("alias")).unwrap();
        let bound = BoundNode::capture(root.join("alias")).unwrap();
        assert!(bound.directory.is_none());
        bound.verify().unwrap();
        assert!(BoundNode::capture(root.join("alias/keep")).is_err());
        assert_eq!(fs::read(root.join("outside/keep")).unwrap(), b"unchanged");
    }

    #[cfg(windows)]
    #[test]
    fn junction_leaf_is_observed_and_ancestor_is_never_followed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::create_dir(root.join("outside")).unwrap();
        fs::write(root.join("outside/keep"), b"unchanged").unwrap();
        fs::create_dir(root.join("alias")).unwrap();
        junction_fixture::set_existing(&root.join("outside"), &root.join("alias")).unwrap();
        let bound = BoundNode::capture(root.join("alias")).unwrap();
        assert!(bound.directory.is_none());
        assert!(bound.identity.as_ref().unwrap().reparse.is_some());
        bound.verify().unwrap();
        assert!(BoundNode::capture(root.join("alias/keep")).is_err());
        assert_eq!(fs::read(root.join("outside/keep")).unwrap(), b"unchanged");
    }
}
