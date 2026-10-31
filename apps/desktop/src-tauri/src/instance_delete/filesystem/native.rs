//! Native retained-directory adapter. User instance trees are only moved with
//! no replacement. Workspace removal below is confined to newly created empty
//! record staging areas, never to an instance or a user file.
use super::*;
use pcl_core::retained_fs::{
    self,
    stage::{PublishOutcome, Stage},
    Directory, Kind, Leaf, Links, Lock, Metadata, Traversal, WorkspaceRemoval,
};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
#[cfg(unix)]
use std::{ffi::CString, os::fd::AsRawFd};

fn native<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|e| e.to_string())
}
fn leaf(name: &str) -> Result<Leaf> {
    name_ok(name)?;
    native(Leaf::new(name))
}
pub(crate) fn name_ok(name: &str) -> Result<()> {
    pcl_core::identifier(name)?;
    if name.len() > 255 || name.chars().any(char::is_control) {
        return Err("实例或文件名称无效".into());
    }
    native(Leaf::new(name)).map(|_| ())
}
pub(crate) fn absolute(path: &Path) -> Result<()> {
    native(retained_fs::validate_absolute_path(path))?;
    if path
        .to_str()
        .is_none_or(|v| v.chars().any(char::is_control))
    {
        return Err("实例操作需要规范的绝对路径".into());
    }
    Ok(())
}
pub(crate) fn canonical(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    absolute(&path)?;
    Ok(path)
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) struct Dir(Directory);
impl Dir {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        absolute(path)?;
        native(Directory::open_absolute(path)).map(Self)
    }
    pub(crate) fn child(&self, name: &str) -> Result<Self> {
        native(self.0.child(&leaf(name)?, Traversal::SameFilesystem)).map(Self)
    }
    pub(crate) fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(crate) fn create(&self, name: &str) -> Result<Self> {
        let dir = native(
            self.0
                .create_private(&leaf(name)?, Traversal::SameFilesystem),
        )?;
        self.sync()?;
        Ok(Self(dir))
    }
    pub(crate) fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(d) => Ok(d),
            None => self.create(name),
        }
    }
    #[cfg(windows)]
    pub(crate) fn document_file(&self) -> Result<File> {
        native(self.0.try_clone_file())
    }
    pub(crate) fn key(&self) -> Result<Key> {
        Key::native(&native(self.0.metadata())?)
    }
    pub(crate) fn sync(&self) -> Result<()> {
        native(self.0.sync())
    }
    pub(crate) fn stat(&self, name: &str) -> Result<Option<Metadata>> {
        let name = leaf(name)?;
        let Some(visible) = native(self.0.entry(&name))? else {
            return Ok(None);
        };
        // Darwin entry() intentionally has only conservative ACL information.
        // Obtain complete evidence from the literal held object and bind it back
        // to the visible identity; never compare that placeholder as full data.
        let actual = match visible.kind {
            Kind::Regular if visible.reparse_tag.is_none() => native(retained_fs::metadata(
                &native(self.0.open_regular(&name, Links::Single))?,
            ))?,
            Kind::Directory if visible.reparse_tag.is_none() => {
                native(native(self.0.child(&name, Traversal::SameFilesystem))?.metadata())?
            }
            _ => return Ok(Some(visible)),
        };
        if !actual.same_object(&visible) {
            return Err("实例文件在检查期间被替换".into());
        }
        native(self.0.recheck(&name, &actual.identity))?;
        Ok(Some(actual))
    }
    pub(crate) fn names(&self) -> Result<Vec<String>> {
        let mut names = native(self.0.names(MAX_FILES))?
            .into_iter()
            .map(|name| {
                let name = name.into_string().map_err(|_| "目录含非UTF-8名称")?;
                name_ok(&name)?;
                Ok(name)
            })
            .collect::<Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }
    pub(crate) fn regular(&self, name: &str) -> Result<File> {
        native(self.0.open_regular(&leaf(name)?, Links::Single))
    }
    pub(crate) fn anonymous(&self) -> Result<TransactionFile> {
        // Native named leases remain isolated from journal/marker names. An
        // interrupted unsealed or altered source is retained as unknown evidence.
        let (name, workspace) = native(self.0.create_private_workspace())?;
        self.sync()?;
        let stage = native(Stage::new(&workspace))?;
        Ok(TransactionFile {
            stage: RefCell::new(Some(stage)),
            parent: self.0.clone(),
            workspace,
            name,
        })
    }
    pub(crate) fn link(&self, file: &TransactionFile, name: &str) -> Result<()> {
        if native(file.parent.identity())? != native(self.0.identity())? {
            return Err("实例记录暂存父目录已变化".into());
        }
        let stage = file
            .stage
            .borrow_mut()
            .take()
            .ok_or("实例记录已尝试发布，必须先检查现存记录")?;
        let sealed = stage.seal(MAX_JOURNAL as u64).map_err(|e| e.to_string())?;
        match sealed
            .publish_new(&self.0, &leaf(name)?)
            .map_err(|e| e.to_string())?
        {
            PublishOutcome::Published { .. } => file.remove_empty_workspace(),
            PublishOutcome::Exists { .. } => Err("实例操作记录已存在，原件与暂存证据已保留".into()),
            PublishOutcome::MayHavePublished { error, .. } => Err(format!(
                "MayHavePublished：实例记录可能已发布，现存记录与暂存证据已保留：{error}"
            )),
        }
    }
    pub(crate) fn unlink_file(&self, name: &str, expected: &FileToken) -> Result<()> {
        // Only business-owned journal/marker records enter this method. Never
        // accept a path to an instance payload as generated-cleanup authority.
        if name != "journal.json"
            && name != "instance-delete-pending.json"
            && !(name.starts_with("journal-") && name.ends_with(".next"))
        {
            return Err("只允许清理已验证的实例事务记录".into());
        }
        #[cfg(windows)]
        {
            let lease = native(self.0.read_lease(&leaf(name)?, true))?;
            let token = file_token(native(lease.file().try_clone())?, None)?;
            if &token != expected {
                return Err("操作记录被外部修改，已保留".into());
            }
            // The caller's durable state and exact digest authorize this
            // generated record, while the lease pins its link and rejects writes.
            match native(lease.remove_generated())? {
                WorkspaceRemoval::Removed => Ok(()),
                WorkspaceRemoval::MayHaveRemoved { error } => {
                    Err(format!("操作记录可能已清理，已停止后续删除：{error}"))
                }
            }
        }
        #[cfg(unix)]
        {
            if &read_bytes(self, name, MAX_JOURNAL)?.0 != expected {
                return Err("操作记录被外部修改，已保留".into());
            }
            let name = CString::new(name).map_err(|e| e.to_string())?;
            if unsafe { libc::unlinkat(self.0.file().as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            self.sync()
        }
    }
    // Production Windows journals never expose a raw swap path. The native
    // test keeps an explicit refusal, with no two-rename approximation.
    #[cfg(any(unix, test))]
    pub(crate) fn exchange(&self, first: &str, second: &str, context: &str) -> Result<()> {
        let first = leaf(first)?;
        let second = leaf(second)?;
        #[cfg(unix)]
        {
            let first =
                CString::new(first.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
            let second =
                CString::new(second.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
            #[cfg(target_os = "macos")]
            let code = unsafe {
                libc::renameatx_np(
                    self.0.file().as_raw_fd(),
                    first.as_ptr(),
                    self.0.file().as_raw_fd(),
                    second.as_ptr(),
                    libc::RENAME_SWAP,
                )
            };
            #[cfg(target_os = "linux")]
            let code = unsafe {
                libc::renameat2(
                    self.0.file().as_raw_fd(),
                    first.as_ptr(),
                    self.0.file().as_raw_fd(),
                    second.as_ptr(),
                    libc::RENAME_EXCHANGE,
                )
            };
            if code != 0 {
                return Err(format!("{context}：{}", std::io::Error::last_os_error()));
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            let _ = (first, second, context);
            Err("Windows 不提供实例删除原始文件交换；仅显式 journal Document CAS 可更新状态".into())
        }
    }
    pub(crate) fn remove_empty(&self, name: &str, expected: &Key) -> Result<()> {
        let current = self.child(name)?;
        if &current.key()? != expected || !current.names()?.is_empty() {
            return Err("空实例恢复记录在清理前已变化，已保留".into());
        }
        // Only a caller-verified empty setup directory is eligible. No game tree
        // and no arbitrary user file is passed to workspace removal.
        match native(
            self.0
                .remove_workspace_entry(&leaf(name)?, &native(current.0.metadata())?),
        )? {
            WorkspaceRemoval::Removed => Ok(()),
            WorkspaceRemoval::MayHaveRemoved { error } => {
                Err(format!("空恢复记录可能已移除，已保留其余证据：{error}"))
            }
        }
    }
}
pub(crate) struct TransactionFile {
    stage: RefCell<Option<Stage>>,
    parent: Directory,
    workspace: Directory,
    name: Leaf,
}
impl TransactionFile {
    pub(crate) fn sync_all(&self) -> std::io::Result<()> {
        self.stage
            .borrow_mut()
            .as_mut()
            .ok_or_else(|| std::io::Error::other("retired instance record"))?
            .file_mut()
            .sync_all()
    }
    fn remove_empty_workspace(&self) -> Result<()> {
        if !native(self.workspace.names(1))?.is_empty() {
            return Err("实例记录暂存目录含未知内容，已保留".into());
        }
        match native(
            self.parent
                .remove_workspace_entry(&self.name, &native(self.workspace.metadata())?),
        )? {
            WorkspaceRemoval::Removed => Ok(()),
            WorkspaceRemoval::MayHaveRemoved { error } => {
                Err(format!("实例记录已发布，暂存目录清理未确认：{error}"))
            }
        }
    }
}
impl Write for TransactionFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.stage
            .get_mut()
            .as_mut()
            .ok_or_else(|| std::io::Error::other("retired instance record"))?
            .file_mut()
            .write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.sync_all()
    }
}
pub(crate) fn anonymous_key(file: &TransactionFile) -> Result<Key> {
    let mut stage = file.stage.borrow_mut();
    let stage = stage.as_mut().ok_or("实例记录已尝试发布")?;
    Key::native(&native(retained_fs::metadata(stage.file_mut()))?)
}
fn stamp(file: &File) -> Result<Stamp> {
    Stamp::native(&native(retained_fs::metadata(file))?)
}
fn file_token(mut file: File, cancel: Option<&AtomicBool>) -> Result<FileToken> {
    let before = stamp(&file)?;
    if !before.is_regular() || before.links != 1 {
        return Err("实例含符号链接、硬链接或特殊文件，已保留".into());
    }
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes = bytes.checked_add(count as u64).ok_or("实例内容过大")?;
        digest.update(&buffer[..count]);
    }
    if before != stamp(&file)? || bytes != before.size {
        return Err("实例内容在读取期间变化，已保留".into());
    }
    Ok(FileToken {
        stamp: before,
        hash: format!("{:x}", digest.finalize()),
    })
}
pub(crate) fn read_bytes(dir: &Dir, name: &str, limit: usize) -> Result<(FileToken, Vec<u8>)> {
    let mut file = dir.regular(name)?;
    let before = stamp(&file)?;
    if before.size > limit as u64 {
        return Err("实例操作记录超过安全上限，已保留".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit
        || before.size != bytes.len() as u64
        || before != stamp(&file)?
        || stamp(&dir.regular(name)?)? != before
    {
        return Err("实例操作记录读取期间变化，原文件已保留".into());
    }
    Ok((
        FileToken {
            stamp: before,
            hash: hash(&bytes),
        },
        bytes,
    ))
}
pub(crate) fn scan(dir: &Dir, cancel: Option<&AtomicBool>) -> Result<Tree> {
    fn visit(
        dir: &Dir,
        prefix: &str,
        tree: &mut Tree,
        depth: usize,
        cancel: Option<&AtomicBool>,
    ) -> Result<()> {
        cancelled(cancel)?;
        if depth > MAX_DEPTH || tree.entries.len() >= MAX_FILES {
            return Err("实例目录深度或文件数量超过安全上限".into());
        }
        let metadata = native(dir.0.metadata())?;
        let before = Stamp::native(&metadata)?;
        tree.entries.push(TreeEntry {
            path: prefix.into(),
            stamp: before.clone(),
            hash: None,
        });
        for name in dir.names()? {
            cancelled(cancel)?;
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let entry = dir.stat(&name)?.ok_or("实例内容在扫描期间消失")?;
            if !Key::native(&entry)?.same_volume(&before.key) {
                return Err("实例内容跨越文件系统，已保留".into());
            }
            match entry.kind {
                Kind::Directory if entry.reparse_tag.is_none() => {
                    visit(&dir.child(&name)?, &path, tree, depth + 1, cancel)?
                }
                Kind::Regular if entry.reparse_tag.is_none() => {
                    if tree.entries.len() >= MAX_FILES {
                        return Err("实例文件数量超过安全上限".into());
                    }
                    let token = file_token(dir.regular(&name)?, cancel)?;
                    let final_entry = dir.stat(&name)?.ok_or("实例内容在读取期间消失")?;
                    if Stamp::native(&final_entry)? != token.stamp {
                        return Err("实例文件路径已被替换或修改".into());
                    }
                    tree.files += 1;
                    tree.bytes = tree
                        .bytes
                        .checked_add(token.stamp.size)
                        .ok_or("实例内容过大")?;
                    tree.entries.push(TreeEntry {
                        path,
                        stamp: token.stamp,
                        hash: Some(token.hash),
                    });
                }
                _ => return Err("实例含符号链接或特殊文件，无法安全删除，原内容已保留".into()),
            }
        }
        if Stamp::native(&native(dir.0.metadata())?)? != before {
            return Err("实例目录在扫描期间变化，已保留".into());
        }
        Ok(())
    }
    let mut tree = Tree {
        entries: vec![],
        files: 0,
        bytes: 0,
    };
    visit(dir, "", &mut tree, 0, cancel)?;
    Ok(tree)
}
pub(crate) struct OperationLock(#[allow(dead_code)] Lock);
impl OperationLock {
    #[cfg(test)]
    pub(crate) fn duplicate_descriptor(&self) -> Result<File> {
        native(self.0.file().try_clone())
    }
}
pub(crate) fn lock(store: &Dir) -> Result<OperationLock> {
    native(store.0.lock(&leaf(".lock")?)).map(OperationLock)
}
pub(crate) fn rename_new(source: &Dir, old: &str, destination: &Dir, new: &str) -> Result<()> {
    if !source.key()?.same_volume(&destination.key()?) {
        return Err("实例恢复目录跨文件系统，原目录已保留".into());
    }
    let expected = source.child(old)?.key()?;
    if source.child(old)?.key()? != expected {
        return Err("待移动实例目录已被替换".into());
    }
    pcl_core::file_ops::rename_noreplace_at(
        source.0.file(),
        leaf(old)?.as_os_str(),
        destination.0.file(),
        leaf(new)?.as_os_str(),
    )
    .map_err(|e| format!("无法无覆盖移动实例目录，可能已移动时请保留复核：{e}"))?;
    if destination.child(new)?.key()? != expected {
        return Err("实例目录已移动但身份复核失败，现存目录已保留".into());
    }
    source.sync()?;
    destination.sync()
}
pub(crate) fn require_mutation_support() -> Result<()> {
    // Windows service journals use an explicit opaque Document CAS. Raw
    // exchange above remains unavailable and has no two-rename fallback.
    Ok(())
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
