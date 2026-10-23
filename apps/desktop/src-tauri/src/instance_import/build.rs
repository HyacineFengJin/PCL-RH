//! Recoverable private game-root construction for native pack installations.
//!
//! The installer owns only a named private root after its inode key is durable.
//! This is reset's subtree ownership boundary: intermediate regular files and
//! directories below that root belong to the operation. Published files use the
//! stronger per-inode and content journal in `publication`; unknown operation
//! entries, replaced roots, links, mounts and special nodes are never removed.
use super::filesystem::Stamp;
use super::publication::{VerifiedFile, VerifiedOutputs};
use super::*;
use std::sync::Arc;
#[path = "build_assets.rs"]
mod assets;

#[derive(Clone, Debug, PartialEq, Eq)]
enum BuildNode {
    Directory(Key),
    File(Snapshot),
}
type BuildTree = BTreeMap<String, BuildNode>;

/// One root writer must own this object until publish or abort finishes. The
/// process flock and pinned FDs survive installer calls; no operations mutex is
/// held by this type. Dropping it after a panic leaves a durable recovery guard.
pub(crate) struct BuildOperation {
    root: Dir,
    build: Dir,
    operation: Dir,
    journal: Journal,
    _lock: ImportLock,
    #[cfg(test)]
    path: PathBuf,
    sealed: Option<BuildTree>,
}
impl BuildOperation {
    #[cfg(test)]
    pub(crate) fn begin(root: &Path, name: &str) -> Result<Self> {
        Self::begin_checked(root, name, None, None)
    }

    /// Check the caller's captured physical root before even creating the
    /// transaction store. Long source/metadata checks may have yielded enough
    /// time for a pathname to be rebound to a different directory.
    pub(super) fn begin_bound(
        root: &Path,
        name: &str,
        expected_root_key: &Key,
        expected_versions_key: Option<&Key>,
    ) -> Result<Self> {
        Self::begin_checked(root, name, Some(expected_root_key), expected_versions_key)
    }

    fn begin_checked(
        root: &Path,
        name: &str,
        expected_root_key: Option<&Key>,
        expected_versions_key: Option<&Key>,
    ) -> Result<Self> {
        name_ok(name)?;
        let path = root.canonicalize().map_err(error)?;
        let root = Dir::open(&path)?;
        if expected_root_key.is_some_and(|expected| root.key().as_ref() != Ok(expected)) {
            return Err(changed());
        }
        guards(&path)?;
        crate::instance_delete::ensure_name_available(&path, name)?;
        let mut targets = BTreeMap::new();
        remember_target(&root, &mut targets, "versions")?;
        remember_target(&root, &mut targets, &format!("versions/{name}"))?;
        if !matches!(targets[&format!("versions/{name}")], Target::Absent) {
            return Err("目标实例名称已存在，请使用其他名称".into());
        }
        let before = match &targets["versions"] {
            Target::Absent => None,
            Target::Directory(key) => Some(key.clone()),
            _ => return Err("versions 路径不是目录".into()),
        };
        // A replaced versions tree is a different destination even when its
        // root is unchanged. Check the captured identity before storage writes;
        // later transaction checks retain this same observed target snapshot.
        if expected_versions_key.is_some_and(|expected| before.as_ref() != Some(expected)) {
            return Err(changed());
        }
        if Dir::open(&path)?.key()? != root.key()? {
            return Err(changed());
        }
        let store = storage(&root, true)?.ok_or("构建记录目录缺失")?;
        let lock = lock(&store)?;
        // Recheck after acquiring the cross-process lock, before creating an
        // operation. A pending operation cannot be hidden by this new writer.
        ensure_ready(&path)?;
        validate_targets(&root, &targets)?;
        let id = nonce();
        let operation = store.mkdir(&id)?;
        let mut journal = Journal {
            schema: 2,
            build: Some(BuildRegistration { key: None }),
            operation_id: id,
            root: path.clone(),
            root_key: root.key()?,
            operation_key: operation.key()?,
            name: name.into(),
            state: State::Building,
            files_key: None,
            instance_key: None,
            files: Vec::new(),
            targets,
            destination_dirs: BTreeMap::from([(
                "versions".into(),
                DestinationDirectory {
                    before,
                    created: None,
                },
            )]),
            instance_dirs: BTreeMap::new(),
        };
        if let Err(original) = write_journal(&operation, &journal) {
            return match store.unlink(&journal.operation_id, true) {
                Ok(()) => Err(original),
                Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{original}")),
            };
        }
        let result = (|| {
            let build = operation.mkdir("build")?;
            journal.build.as_mut().ok_or("构建登记缺失")?.key = Some(build.key()?);
            // No installer or caller receives a writable path until this key is
            // durable. If this write fails, only our still-empty root is removed.
            write_journal(&operation, &journal)?;
            Ok(build)
        })();
        let build = match result {
            Ok(build) => build,
            Err(original) => {
                return match recover_one(&operation, &mut journal) {
                    Ok(()) => Err(original),
                    Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{original}")),
                };
            }
        };
        #[cfg(test)]
        let build_path = path
            .join(".pcl-linux")
            .join(STORE)
            .join(&journal.operation_id)
            .join("build");
        Ok(Self {
            root,
            build,
            operation,
            journal,
            _lock: lock,
            #[cfg(test)]
            path: build_path,
            sealed: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn root_path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn operation_id(&self) -> &str {
        &self.journal.operation_id
    }
    pub(crate) fn root_fd(&self) -> Result<File> {
        self.build_dir()?.0.try_clone().map_err(error)
    }
    pub(crate) fn verify_binding(&self) -> Result<()> {
        self.build_dir().map(|_| ())
    }
    pub(crate) fn open_file(&self, path: &str) -> Result<File> {
        self.build_dir()?.file(path)
    }
    fn build_dir(&self) -> Result<Dir> {
        validate_binding(&self.operation, &self.journal)?;
        let key = &self.journal.build.as_ref().ok_or("构建登记缺失")?.key;
        let named = owned_dir(&self.operation, "build", key)?.ok_or("私有构建目录缺失")?;
        if named.key()? != self.build.key()? {
            return Err("持有的私有构建根与路径已不匹配".into());
        }
        self.build.duplicate()
    }

    /// Seal the complete installer output, including processor-generated
    /// runtime libraries absent from launch JSON. A second tree walk before
    /// commit detects pathname replacements and additions after this snapshot.
    pub(crate) fn seal(&mut self, cancel: &AtomicBool) -> Result<VerifiedOutputs> {
        if self.journal.state != State::Building || self.sealed.is_some() {
            return Err("私有构建已经封存或结束".into());
        }
        check(cancel)?;
        let build = self.build_dir()?;
        assets::namespace_index(self, &build, cancel)?;
        let (tree, outputs) = scan_build(&build, &self.journal.name, cancel)?;
        self.verify_binding()?;
        self.sealed = Some(tree);
        Ok(outputs)
    }

    fn verify_sealed(&self, cancel: &AtomicBool) -> Result<()> {
        let expected = self.sealed.as_ref().ok_or("私有构建尚未封存")?;
        let (actual, _) = scan_build(&self.build_dir()?, &self.journal.name, cancel)?;
        if &actual != expected {
            return Err("私有构建在封存后发生变化，拒绝发布".into());
        }
        self.verify_binding()
    }

    fn verify_complete_outputs(&self, outputs: &VerifiedOutputs) -> Result<()> {
        let sealed = self.sealed.as_ref().ok_or("私有构建尚未封存")?;
        let mut files = BTreeMap::new();
        for file in &outputs.files {
            if files.insert(file.target(), file).is_some() {
                return Err("发布计划文件路径重复或与核心文件冲突".into());
            }
        }
        for (path, node) in sealed {
            match node {
                BuildNode::File(expected) => {
                    let input = files
                        .get(path.as_str())
                        .ok_or("发布计划缺少已封存的完整构建文件")?;
                    if input.size() != expected.stamp.size || input.sha256() != expected.hash {
                        return Err("发布计划与已封存构建内容不一致".into());
                    }
                }
                BuildNode::Directory(_)
                    if path != "versions" && !outputs.directories.contains(path) =>
                {
                    return Err("发布计划缺少已封存的构建目录".into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `bind_source` checks the caller's held archive and its current pathname,
    /// both before entering commit and before the final instance move.
    /// `enter_commit` closes cancellation admission and rechecks references in
    /// the caller's short operations section. Hash/copy I/O runs outside it.
    pub(crate) fn publish_checked(
        mut self,
        outputs: VerifiedOutputs,
        cancel: &AtomicBool,
        callback: impl Fn(Progress),
        bind_source: impl Fn() -> Result<()>,
        enter_commit: impl FnOnce() -> Result<()>,
    ) -> Result<Value> {
        let result = (|| {
            check(cancel)?;
            self.verify_sealed(cancel)?;
            self.verify_complete_outputs(&outputs)?;
            publication::plan_native(&self.root, &mut self.journal, &outputs)?;
            write_journal(&self.operation, &self.journal)?;
            self.journal.files_key = Some(self.operation.mkdir("files")?.key()?);
            write_journal(&self.operation, &self.journal)?;
            self.journal.instance_key = Some(self.operation.mkdir("instance")?.key()?);
            write_journal(&self.operation, &self.journal)?;
            let bytes = self.journal.files.iter().map(|f| f.size).sum::<u64>();
            let count = self.journal.files.len();
            publication::stage_native(
                &self.operation,
                &mut self.journal,
                &outputs,
                cancel,
                &|index, done| {
                    callback(progress(
                        "import-extract",
                        "正在复制并校验独立实例与资源",
                        index,
                        count as u64,
                        done,
                        bytes,
                    ));
                },
            )?;
            assemble_instance(&self.operation, &mut self.journal, cancel)?;
            self.verify_sealed(cancel)?;
            publication::verify_inputs(&outputs, Some(cancel))?;
            bind_source()?;
            validate_targets(&self.root, &self.journal.targets)?;
            self.journal.state = State::Prepared;
            write_journal(&self.operation, &self.journal)?;
            check(cancel)?;
            callback(progress(
                "import-commit",
                "正在发布新实例与共享资源",
                0,
                1,
                bytes,
                bytes,
            ));
            enter_commit()?;
            // Honor only cancellation accepted before the gate. After this
            // check, publish and durable recovery run without late-cancel tests.
            check(cancel)?;
            let mut journal = self.journal.clone();
            let commit =
                publication::publish_prepared(&self.root, &self.operation, &mut journal, || {
                    self.verify_sealed(&AtomicBool::new(false))?;
                    publication::verify_inputs(&outputs, None)?;
                    bind_source()
                });
            self.journal = journal;
            commit?;
            callback(progress(
                "import-cleanup",
                "正在清理私有构建与导入暂存文件",
                0,
                1,
                bytes,
                bytes,
            ));
            let reused = self.journal.files.iter().filter(|f| f.reuse).count();
            self.abort()?;
            Ok(
                json!({"id":self.journal.name,"files":count,"bytes":bytes,"reused_files":reused,"message":"整合包安装完成"}),
            )
        })();
        match result {
            Ok(value) => Ok(value),
            Err(original) => match self.abort() {
                Ok(()) => Err(original),
                Err(cleanup) => Err(format!("取消清理失败：{cleanup}；原错误：{original}")),
            },
        }
    }

    /// Building abort never reads the source or touches real instance/cache
    /// outputs. After publication this dispatches the durable rollback/retain
    /// rules, preserving cleanup failures for explicit recovery.
    pub(crate) fn abort(&mut self) -> Result<()> {
        recover_one(&self.operation, &mut self.journal)
    }
}

fn scan_build(
    build: &Dir,
    name: &str,
    cancel: &AtomicBool,
) -> Result<(BuildTree, VerifiedOutputs)> {
    fn walk(
        dir: &Dir,
        prefix: &str,
        tree: &mut BuildTree,
        outputs: &mut VerifiedOutputs,
        bytes: &mut u64,
        pinned_root: &Arc<Dir>,
        cancel: &AtomicBool,
    ) -> Result<()> {
        for name in dir.names()? {
            check(cancel)?;
            let path = format!("{prefix}/{name}");
            relative(&path)?;
            if tree.len() >= MAX_FILES * 2 {
                return Err("私有构建节点数量超过安全限制".into());
            }
            let stamp = dir.stat(&name)?.ok_or_else(changed)?;
            if stamp.directory() {
                let child = dir.child(&name)?;
                if child.key()? != stamp.key {
                    return Err(changed());
                }
                tree.insert(path.clone(), BuildNode::Directory(child.key()?));
                outputs.directories.insert(path.clone());
                if outputs.directories.len() > MAX_FILES {
                    return Err("构建目录数量超过安全限制".into());
                }
                walk(&child, &path, tree, outputs, bytes, pinned_root, cancel)?;
                if dir.child(&name)?.key()? != stamp.key {
                    return Err(changed());
                }
            } else if stamp.regular() {
                if outputs.files.len() >= MAX_FILES {
                    return Err("构建文件数量超过安全限制".into());
                }
                let file = dir.regular(&name)?;
                let snapshot = hash_file(file.try_clone().map_err(error)?, MAX_FILE, Some(cancel))?;
                if snapshot.stamp != stamp {
                    return Err(changed());
                }
                *bytes = bytes
                    .checked_add(snapshot.stamp.size)
                    .filter(|n| *n <= MAX_BYTES)
                    .ok_or("私有构建内容超过大小限制")?;
                tree.insert(path.clone(), BuildNode::File(snapshot.clone()));
                outputs.files.push(VerifiedFile::from_build_snapshot(
                    path,
                    Arc::clone(pinned_root),
                    snapshot,
                ));
            } else {
                return Err("私有构建包含符号链接或特殊节点，拒绝发布".into());
            }
        }
        Ok(())
    }
    let mut tree = BuildTree::new();
    let mut outputs = VerifiedOutputs {
        files: Vec::new(),
        directories: BTreeSet::new(),
    };
    let mut bytes = 0;
    let pinned_root = Arc::new(build.duplicate()?);
    for child in build.names()? {
        check(cancel)?;
        if !matches!(child.as_str(), "versions" | "libraries" | "assets") {
            return Err(format!("私有构建包含不支持的根目录：{child}"));
        }
        let dir = build.child(&child)?;
        tree.insert(child.clone(), BuildNode::Directory(dir.key()?));
        if child == "versions" {
            let versions = dir.names()?;
            if versions != [name] {
                return Err("私有构建包含缺失实例、额外实例或未完成安装目录".into());
            }
            let instance = dir.child(name)?;
            let path = format!("versions/{name}");
            tree.insert(path.clone(), BuildNode::Directory(instance.key()?));
            outputs.directories.insert(path.clone());
            walk(
                &instance,
                &path,
                &mut tree,
                &mut outputs,
                &mut bytes,
                &pinned_root,
                cancel,
            )?;
        } else {
            outputs.directories.insert(child.clone());
            walk(
                &dir,
                &child,
                &mut tree,
                &mut outputs,
                &mut bytes,
                &pinned_root,
                cancel,
            )?;
        }
        if build.child(&child)?.key()? != dir.key()? {
            return Err(changed());
        }
    }
    if !tree.contains_key("versions") {
        return Err("私有构建缺少 versions 目录".into());
    }
    Ok((tree, outputs))
}

pub(super) fn verify_terminal_operation(operation: &Dir, j: &Journal) -> Result<()> {
    validate_next(operation, j)?;
    if operation
        .names()?
        .iter()
        .any(|name| name != "journal.json" && name != "journal.next")
    {
        return Err("已完成构建记录目录包含未知内容，已保留供检查".into());
    }
    Ok(())
}

/// Preflight the full registered subtree before unlinking any node. This keeps
/// malformed intermediate content available for inspection and gives retries a
/// bounded, FD-relative plan. Descendant inode keys are rechecked at deletion.
pub(super) fn cleanup_build(operation: &Dir, j: &Journal) -> Result<()> {
    let Some(build) = &j.build else {
        return Ok(());
    };
    let Some(root) = owned_dir(operation, "build", &build.key)? else {
        return Ok(());
    };
    let key = root.key()?;
    fn inspect(dir: &Dir, prefix: &str, entries: &mut BTreeMap<String, Stamp>) -> Result<()> {
        for name in dir.names()? {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            relative(&path)?;
            if entries.len() >= MAX_FILES * 4 {
                return Err("私有构建清理节点数量超过安全限制".into());
            }
            let stamp = dir.stat(&name)?.ok_or("私有构建在清理前发生变化")?;
            if stamp.directory() {
                let child = dir.child(&name)?;
                if child.key()? != stamp.key {
                    return Err("私有构建子目录已被替换".into());
                }
                inspect(&child, &path, entries)?;
            } else if stamp.regular() {
                let file = dir.regular(&name)?;
                if Stamp::of(&file.metadata().map_err(error)?) != stamp {
                    return Err("私有构建文件在清理前发生变化".into());
                }
            } else {
                return Err("私有构建包含符号链接或特殊节点，已保留待恢复内容".into());
            }
            entries.insert(path, stamp);
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    inspect(&root, "", &mut entries)?;
    for path in ordered_paths(entries.keys(), true) {
        validate_binding(operation, j)?;
        if operation.child("build")?.key()? != key {
            return Err("私有构建根已被替换，拒绝清理".into());
        }
        let (parent, name) = root.parent(&path)?;
        let expected = &entries[&path];
        let current = parent.stat(&name)?.ok_or("私有构建在清理期间发生变化")?;
        if current.key != expected.key || current.mode != expected.mode {
            return Err("私有构建节点在清理期间已被替换，拒绝清理".into());
        }
        if expected.directory() {
            if parent.child(&name)?.key()? != expected.key {
                return Err(changed());
            }
        } else {
            let file = parent.regular(&name)?;
            let actual = Stamp::of(&file.metadata().map_err(error)?);
            if actual.key != expected.key || actual.mode != expected.mode {
                return Err(changed());
            }
        }
        parent.unlink(&name, expected.directory())?;
    }
    if operation.child("build")?.key()? != key {
        return Err("私有构建清理期间已被替换".into());
    }
    operation.unlink("build", true)
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
