//! Native held-directory reads and app-owned reference documents. Windows
//! selects immutable optional revisions; Darwin physically exchanges entries.
use super::*;
#[cfg(windows)]
use crate::launcher_local::filesystem::documents::DocumentPresence;
use crate::launcher_local::filesystem::{documents::Document, Dir as LocalDir};
use pcl_core::retained_fs::stage::{PublishOutcome, Stage};
use pcl_core::retained_fs::{
    self as retained, Directory, Kind, Leaf, Links, Lock, Traversal, WorkspaceRemoval,
};
#[cfg(unix)]
use std::{ffi::CString, os::fd::AsRawFd};
use std::{
    fs::File,
    io::{Read, Write},
};

pub(crate) struct Dir(Directory);
pub(crate) struct ReferenceLock(#[allow(dead_code)] Lock);
fn io<T>(result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}
fn leaf(name: &str) -> Result<Leaf, String> {
    io(Leaf::new(name))
}
fn may(error: impl std::fmt::Display) -> String {
    format!("MayHaveCommitted：引用资料可能已发布，原件与证据已保留，请重新读取：{error}")
}
fn identity(metadata: &retained::Metadata) -> Result<Identity, String> {
    match &metadata.identity {
        #[cfg(unix)]
        retained::Identity::Unix { device, inode } => Ok(Identity {
            device: *device,
            inode: *inode,
        }),
        #[cfg(windows)]
        retained::Identity::Windows { volume, file } => Ok(Identity {
            format: WindowsFormat::FileId128V1,
            volume: *volume,
            file_id: *file,
        }),
        _ => Err("引用资料身份不属于当前平台".into()),
    }
}
fn stamp(file: &File, metadata: &retained::Metadata) -> Result<Stamp, String> {
    #[cfg(unix)]
    {
        let _ = metadata;
        Ok(Stamp::of(&io(file.metadata())?))
    }
    #[cfg(windows)]
    {
        let _ = file;
        Ok(Stamp {
            identity: identity(metadata)?,
            metadata: metadata.clone(),
        })
    }
}
impl Dir {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        absolute(path)?;
        io(Directory::open_absolute(path)).map(Self)
    }
    pub(crate) fn identity(&self) -> Result<Identity, String> {
        identity(&io(self.0.metadata())?)
    }
    pub(crate) fn child(&self, name: &str) -> Result<Self, String> {
        io(self.0.child(&leaf(name)?, Traversal::AllowMounts)).map(Self)
    }
    pub(crate) fn optional(&self, name: &str) -> Result<Option<Self>, String> {
        match io(self.0.entry(&leaf(name)?))? {
            None => Ok(None),
            Some(_) => self.child(name).map(Some),
        }
    }
    pub(crate) fn ensure(&self, name: &str) -> Result<Self, String> {
        let dir = match self.optional(name)? {
            Some(dir) => dir.0,
            None => match self.0.create_private(&leaf(name)?, Traversal::AllowMounts) {
                Ok(dir) => dir,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.child(name)?.0
                }
                Err(error) => return Err(error.to_string()),
            },
        };
        io(io(dir.metadata())?.require_private())?;
        self.sync()?;
        Ok(Self(dir))
    }
    fn local(&self) -> Result<LocalDir, String> {
        Ok(LocalDir(io(self.0.try_clone_file())?))
    }
    pub(crate) fn sync(&self) -> Result<(), String> {
        io(self.0.sync())
    }
    pub(crate) fn names(&self) -> Result<Vec<String>, String> {
        let mut values = io(self.0.names(MAX_HISTORY))?
            .into_iter()
            .map(|name| {
                name.into_string()
                    .map_err(|_| "资源历史名称不是 UTF-8".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        values.sort();
        Ok(values)
    }
    pub(crate) fn lock(&self, name: &str) -> Result<ReferenceLock, String> {
        io(self.0.lock(&leaf(name)?)).map(ReferenceLock)
    }
    // Physical reads are reserved for the generated marker and backup files.
    // Full held metadata, not Darwin entry()'s ACL placeholder, is compared.
    pub(crate) fn read(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Option<(Token, Vec<u8>)>, String> {
        let name = leaf(name)?;
        if io(self.0.entry(&name))?.is_none() {
            return Ok(None);
        }
        let mut file = io(self.0.open_regular(&name, Links::Single))?;
        let metadata = io(retained::metadata(&file))?;
        io(metadata.require_private())?;
        if metadata.kind != Kind::Regular
            || metadata.reparse_tag.is_some()
            || metadata.len > limit as u64
        {
            return Err("引用资料不是有界私有普通文件".into());
        }
        let before = stamp(&file, &metadata)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 != metadata.len || io(retained::metadata(&file))? != metadata {
            return Err("引用资料在读取期间变化".into());
        }
        io(self.0.recheck(&name, &metadata.identity))?;
        Ok(Some((
            Token {
                stamp: before,
                hash: hash(&bytes),
            },
            bytes,
        )))
    }
    pub(crate) fn snapshot(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<(DiskSnapshot, Option<Vec<u8>>), String> {
        #[cfg(unix)]
        {
            let value = self.read(name, limit)?;
            Ok((
                DiskSnapshot {
                    directory: Some(self.identity()?),
                    file: value.as_ref().map(|(token, _)| token.clone()),
                },
                value.map(|(_, bytes)| bytes),
            ))
        }
        #[cfg(windows)]
        {
            let local = self.local()?;
            let document = Document::new(&local, name, limit as u64)?;
            let mut state = document.state()?;
            let bytes = document.read_state(&mut state)?;
            let file = if let Some(bytes) = &bytes {
                let snapshot = document.snapshot()?.ok_or("引用文档选择的内容消失")?;
                let metadata = io(retained::metadata(snapshot.file()))?;
                if snapshot.digest() != hash(bytes) {
                    return Err("引用文档在快照期间变化".into());
                }
                Some(Token {
                    stamp: stamp(snapshot.file(), &metadata)?,
                    hash: hash(bytes),
                })
            } else {
                None
            };
            let digest = state.fact_digest();
            if document.state()?.fact_digest() != digest {
                return Err("引用文档 generation 在读取期间变化".into());
            }
            let presence = match state.presence() {
                DocumentPresence::NeverCreated => "never_created",
                DocumentPresence::Present => "present",
                DocumentPresence::Tombstone => "tombstone",
            }
            .into();
            Ok((
                DiskSnapshot {
                    directory: Some(self.identity()?),
                    file,
                    logical: Some(LogicalState {
                        format: LogicalFormat::V2,
                        presence,
                        digest,
                    }),
                },
                bytes,
            ))
        }
    }
    pub(crate) fn update_checked(
        &self,
        name: &str,
        before: &DiskSnapshot,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<(), String> {
        if self.snapshot(name, limit)?.0 != *before {
            return Err("引用文档 CAS 冲突，原件已保留".into());
        }
        #[cfg(windows)]
        {
            let local = self.local()?;
            let document = Document::new(&local, name, limit as u64)?;
            let state = document.state()?;
            if before
                .logical
                .as_ref()
                .map(|logical| logical.digest.as_str())
                != Some(state.fact_digest().as_str())
            {
                return Err("引用文档 logical CAS 冲突".into());
            }
            document.compare_write(&state, after)
        }
        #[cfg(unix)]
        {
            self.update(name, before.file.as_ref(), after, limit)
        }
    }
    #[cfg(unix)]
    pub(crate) fn update(
        &self,
        name: &str,
        before: Option<&Token>,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<(), String> {
        if self.read(name, limit)?.map(|(token, _)| token).as_ref() != before {
            return Err("引用资料已被外部修改".into());
        }
        if let Some(bytes) = after {
            let local = self.local()?;
            let document = Document::new(&local, name, limit as u64)?;
            let mut snapshot = document.snapshot()?;
            let now = snapshot
                .as_mut()
                .map(|snapshot| {
                    let bytes = document.bytes(snapshot)?;
                    let metadata = io(retained::metadata(snapshot.file()))?;
                    Ok::<_, String>(Token {
                        stamp: stamp(snapshot.file(), &metadata)?,
                        hash: hash(&bytes),
                    })
                })
                .transpose()?;
            if now.as_ref() != before {
                return Err("引用文档在交换前变化".into());
            }
            // The existing Darwin Document backend uses actual RENAME_SWAP.
            return document.replace(snapshot.as_ref(), bytes);
        }
        let Some(before) = before else {
            return Ok(());
        };
        let temporary = format!(".rename-reference-{}.tmp", nonce());
        let parent = io(self.0.try_clone_file())?;
        io(pcl_core::file_ops::rename_noreplace_at(
            &parent,
            name.as_ref(),
            &parent,
            temporary.as_ref(),
        ))
        .map_err(may)?;
        let (moved, _) = self
            .read(&temporary, limit)?
            .ok_or_else(|| may("删除中的引用资料消失"))?;
        if !before.stamp.moved_matches(&moved.stamp) || before.hash != moved.hash {
            io(pcl_core::file_ops::rename_noreplace_at(
                &parent,
                temporary.as_ref(),
                &parent,
                name.as_ref(),
            ))
            .map_err(may)?;
            return Err("引用资料在删除期间变化，原件已还原".into());
        }
        let name = CString::new(temporary).map_err(|e| e.to_string())?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(may(std::io::Error::last_os_error()));
        }
        self.sync().map_err(may)
    }
    #[cfg(all(windows, test))]
    pub(crate) fn update(
        &self,
        name: &str,
        before: Option<&Token>,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<(), String> {
        let current = self.snapshot(name, limit)?.0;
        if before.is_none() || current.file.as_ref() != before {
            return Err("测试调用缺少准确 present token".into());
        }
        self.update_checked(name, &current, after, limit)
    }
    pub(crate) fn write_backup(
        &self,
        name: &str,
        bytes: &[u8],
        limit: usize,
    ) -> Result<(), String> {
        if bytes.len() > limit {
            return Err("备份过大".into());
        }
        self.local()?.write_new(name, bytes).map_err(may)?;
        if self.read(name, limit)?.map(|(_, saved)| saved).as_deref() != Some(bytes) {
            return Err(may("备份内容变化"));
        }
        self.sync().map_err(may)
    }
    pub(crate) fn publish_marker(
        &self,
        build: impl FnOnce(Identity) -> Result<Vec<u8>, String>,
    ) -> Result<(), String> {
        let mut stage = io(Stage::new(&self.0))?;
        let identity = identity(&io(retained::metadata(stage.file()))?)?;
        let bytes = build(identity.clone())?;
        if bytes.is_empty() || bytes.len() > PRESET_LIMIT {
            return Err("重命名标记过大或为空".into());
        }
        stage
            .file_mut()
            .write_all(&bytes)
            .map_err(|e| e.to_string())?;
        let sealed = stage.seal(PRESET_LIMIT as u64).map_err(|e| e.to_string())?;
        match sealed
            .publish_new(&self.0, &leaf(MARKER)?)
            .map_err(|e| e.to_string())?
        {
            PublishOutcome::Published { .. } => {}
            PublishOutcome::Exists { .. } => return Err("已有重命名标记，来源及原件已保留".into()),
            PublishOutcome::MayHavePublished { error, .. } => return Err(may(error)),
        }
        let (token, actual) = self
            .read(MARKER, PRESET_LIMIT)?
            .ok_or_else(|| may("发布的标记消失"))?;
        if token.stamp.identity != identity || actual != bytes {
            return Err(may("发布的标记变化"));
        }
        self.sync().map_err(may)
    }
    pub(crate) fn clear_marker(&self, token: &Token, bytes: &[u8]) -> Result<(), String> {
        #[cfg(windows)]
        {
            let lease = io(self.0.read_lease(&leaf(MARKER)?, true))?;
            if lease.metadata() != &token.stamp.metadata {
                return Err("重命名标记被替换，已保留".into());
            }
            if self.read(MARKER, PRESET_LIMIT)?.as_ref() != Some(&(token.clone(), bytes.to_vec())) {
                return Err("重命名标记内容变化，已保留".into());
            }
            match io(lease.remove_generated())? {
                WorkspaceRemoval::Removed => Ok(()),
                WorkspaceRemoval::MayHaveRemoved { error } => Err(may(error)),
            }
        }
        #[cfg(unix)]
        {
            let (current, actual) = self.read(MARKER, PRESET_LIMIT)?.ok_or("重命名标记消失")?;
            if &current != token || actual != bytes {
                return Err("重命名标记变化，已保留".into());
            }
            let file = io(self.0.open_regular(&leaf(MARKER)?, Links::Single))?;
            let expected = io(retained::metadata(&file))?;
            if stamp(&file, &expected)? != token.stamp {
                return Err("重命名标记身份变化，已保留".into());
            }
            // Only this exact operation-authorized generated marker is removed.
            match io(self.0.remove_workspace_entry(&leaf(MARKER)?, &expected))? {
                WorkspaceRemoval::Removed => Ok(()),
                WorkspaceRemoval::MayHaveRemoved { error } => Err(may(error)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Dir) {
        let temporary = tempfile::tempdir().unwrap();
        let root = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
        let directory = root
            .create_private(&leaf("native-references").unwrap(), Traversal::AllowMounts)
            .unwrap();
        (temporary, Dir(directory))
    }
    #[test]
    fn actual_native_adapter_reopens_existing_private_namespaces_and_updates_documents() {
        let (_temporary, dir) = fixture();
        let child = dir.ensure("storage").unwrap();
        assert_eq!(
            dir.ensure("storage").unwrap().identity().unwrap(),
            child.identity().unwrap()
        );
        let first = child.snapshot("settings.json", 65536).unwrap().0;
        child
            .update_checked("settings.json", &first, Some(b"base"), 65536)
            .unwrap();
        let current = child.snapshot("settings.json", 65536).unwrap().0;
        child
            .update_checked("settings.json", &current, Some(b"replacement"), 65536)
            .unwrap();
        assert_eq!(
            child.snapshot("settings.json", 65536).unwrap().1.unwrap(),
            b"replacement"
        );
        assert!(child
            .update_checked("settings.json", &current, Some(b"stale"), 65536)
            .is_err());
    }
    #[test]
    fn actual_native_stage_marker_publishes_self_identity_and_clears_exact_generated_entry() {
        let (_temporary, dir) = fixture();
        dir.publish_marker(|identity| serde_json::to_vec(&identity).map_err(|e| e.to_string()))
            .unwrap();
        let (token, bytes) = dir.read(MARKER, PRESET_LIMIT).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Identity>(&bytes).unwrap(),
            token.stamp.identity
        );
        dir.clear_marker(&token, &bytes).unwrap();
        assert!(dir.read(MARKER, PRESET_LIMIT).unwrap().is_none());
    }
}
