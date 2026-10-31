//! Shared no-overwrite transaction for verified native pack outputs and ZIPs.
//!
//! Inputs own their FDs and cannot be reconstructed from frontend JSON. They
//! are copied to anonymous files owned by this operation before publication;
//! source stability and destination ownership are independent invariants.
use super::*;
use std::{
    io::{Seek, SeekFrom},
    sync::Arc,
};

pub(super) fn publish_prepared(
    root: &Dir,
    operation: &Dir,
    j: &mut Journal,
    final_source_check: impl FnOnce() -> Result<()>,
) -> Result<()> {
    validate_binding(operation, j)?;
    validate_targets(root, &j.targets)?;
    crate::instance_delete::ensure_name_available(&j.root, &j.name)?;
    ensure_destination_dirs(root, operation, j)?;
    let files = owned_dir(operation, "files", &j.files_key)?.ok_or("导入暂存文件目录缺失")?;
    let instance =
        owned_dir(operation, "instance", &j.instance_key)?.ok_or("导入暂存实例目录缺失")?;
    complete_instance(&instance, j)?;
    for (i, f) in j.files.iter().enumerate() {
        if f.reuse || f.target.starts_with(&format!("versions/{}/", j.name)) {
            continue;
        }
        let expected = f.staged.as_ref().ok_or("导入共享文件未登记")?;
        if !expected.owned_matches(&hash_file(files.regular(&slot(i))?, MAX_FILE, None)?) {
            return Err(changed());
        }
        validate_binding(operation, j)?;
        let (parent, name) = destination_parent(root, &f.target, j)?;
        if parent.stat(&name)?.is_some() {
            return Err(changed());
        }
        link_file(&files, &slot(i), &parent, &name)?;
    }
    verify_shared(root, j, true)?;
    verify_destination_dirs(root, j, true)?;
    final_source_check()?;
    let versions = root.child("versions")?;
    if versions.stat(&j.name)?.is_some() {
        return Err(changed());
    }
    if versions.key()?
        != j.destination_dirs["versions"]
            .before
            .clone()
            .or_else(|| j.destination_dirs["versions"].created.clone())
            .ok_or("versions 身份缺失")?
    {
        return Err(changed());
    }
    crate::instance_delete::ensure_name_available(&j.root, &j.name)?;
    validate_binding(operation, j)?;
    move_new(operation, "instance", &versions, &j.name)?;
    // The directory move is the final publication. This durable state decides
    // whether recovery retains the completed instance or removes owned output.
    j.state = State::Committed;
    if let Err(e) = write_journal(operation, j) {
        // A logical/native commit may already be durable when only its
        // verification, directory sync or generated-version GC failed. Preserve
        // the completed output in that case; recovery reopens the actual state.
        if !e.starts_with("MayHaveCommitted") && !e.starts_with("MayHavePublished") {
            j.state = State::Prepared;
        }
        return Err(e);
    }
    Ok(())
}

/// Native verified input. A mutable source FD is only a read capability: the
/// publisher always copies its bytes into its own anonymous inode, verifies
/// that copy, and checks the source snapshot again before committing.
pub(crate) struct VerifiedFile {
    target: String,
    source: InputSource,
    expected: Option<Snapshot>,
    size: u64,
    hash: String,
}
enum InputSource {
    Generated(Vec<u8>),
    #[allow(dead_code)]
    // Native FD inputs are also used by callers outside the private build tree.
    Held(File),
    // Complete build trees can contain tens of thousands of files. Retain one
    // pinned root FD rather than exhausting the process limit with one per
    // source; each source is opened no-follow and bound to its sealed snapshot.
    Build {
        root: Arc<Dir>,
        path: String,
    },
}
impl VerifiedFile {
    pub(super) fn mapped_snapshot(
        target: String,
        path: String,
        root: Arc<Dir>,
        expected: Snapshot,
    ) -> Self {
        Self {
            target,
            source: InputSource::Build { root, path },
            size: expected.stamp.size,
            hash: expected.hash.clone(),
            expected: Some(expected),
        }
    }
    pub(super) fn generated(target: String, bytes: Vec<u8>) -> Result<Self> {
        relative(&target)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("构建输入超过大小限制".into());
        }
        Ok(Self {
            target,
            size: bytes.len() as u64,
            hash: format!("{:x}", Sha256::digest(&bytes)),
            expected: None,
            source: InputSource::Generated(bytes),
        })
    }
    #[allow(dead_code)] // Keep this native capability entry point available to streaming callers.
    pub(crate) fn from_fd(
        target: String,
        source: File,
        expected_size: u64,
        expected_sha256: &str,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        relative(&target)?;
        if expected_size > MAX_FILE
            || expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("构建输入的大小或 SHA256 校验无效".into());
        }
        let expected = hash_file(source.try_clone().map_err(error)?, MAX_FILE, Some(cancel))?;
        if expected.stamp.size != expected_size
            || !expected.hash.eq_ignore_ascii_case(expected_sha256)
        {
            return Err("构建输入的大小或 SHA256 与已校验内容不一致".into());
        }
        Ok(Self {
            target,
            source: InputSource::Held(source),
            size: expected.stamp.size,
            hash: expected.hash.clone(),
            expected: Some(expected),
        })
    }
    pub(crate) fn target(&self) -> &str {
        &self.target
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn sha256(&self) -> &str {
        &self.hash
    }
    pub(super) fn from_build_snapshot(target: String, root: Arc<Dir>, expected: Snapshot) -> Self {
        Self {
            source: InputSource::Build {
                root,
                path: target.clone(),
            },
            target,
            size: expected.stamp.size,
            hash: expected.hash.clone(),
            expected: Some(expected),
        }
    }
    fn open_source(&self) -> Result<File> {
        match &self.source {
            InputSource::Generated(_) => Err("内存构建输入没有文件路径权限".into()),
            InputSource::Held(file) => file.try_clone().map_err(error),
            InputSource::Build { root, path } => root.file(path),
        }
    }
    fn reader(&self) -> Result<Box<dyn Read + '_>> {
        if let InputSource::Generated(bytes) = &self.source {
            return Ok(Box::new(std::io::Cursor::new(bytes.as_slice())));
        }
        let mut file = self.open_source()?;
        file.seek(SeekFrom::Start(0)).map_err(error)?;
        Ok(Box::new(file))
    }
    fn verify(&self, cancel: Option<&AtomicBool>) -> Result<()> {
        if let Some(cancel) = cancel {
            check(cancel)?;
        }
        if let InputSource::Generated(bytes) = &self.source {
            if bytes.len() as u64 == self.size
                && format!("{:x}", Sha256::digest(bytes)) == self.hash
                && self.expected.is_none()
            {
                return Ok(());
            }
            return Err("内存构建输入发生变化".into());
        }
        if Some(hash_file(self.open_source()?, MAX_FILE, cancel)?) != self.expected {
            return Err("已校验的构建来源在发布前发生变化".into());
        }
        Ok(())
    }
}

/// Complete final outputs. Directories are full game-root-relative paths;
/// pack content is mapped below `versions/<name>/`. Overlapping file mappings
/// and file/directory ancestors are rejected before any destination write.
pub(crate) struct VerifiedOutputs {
    pub(crate) files: Vec<VerifiedFile>,
    pub(crate) directories: BTreeSet<String>,
}

pub(super) fn plan_native(root: &Dir, j: &mut Journal, outputs: &VerifiedOutputs) -> Result<()> {
    if j.schema != 2 || j.state != State::Building || j.build.is_none() {
        return Err("构建操作不能进入文件发布准备阶段".into());
    }
    let prefix = format!("versions/{}/", j.name);
    let instance_target = format!("versions/{}", j.name);
    let mut targets = j.targets.clone();
    let mut dirs = BTreeSet::from(["config".to_owned()]);
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    let mut files = Vec::with_capacity(outputs.files.len());
    if outputs.files.len() > MAX_FILES || outputs.directories.len() > MAX_FILES {
        return Err("构建输出文件或目录数量超过安全限制".into());
    }
    for input in &outputs.files {
        let target = input.target();
        relative(target)?;
        if forbidden(target)
            || !names.insert(target.to_owned())
            || (!target.starts_with(&prefix) && (!shared(target) || !target.contains('/')))
        {
            return Err(format!("构建输出路径无效或与核心文件冲突：{target}"));
        }
        let reuse = if let Some(path) = target.strip_prefix(&prefix) {
            let parts = relative(path)?;
            for depth in 1..parts.len() {
                dirs.insert(parts[..depth].join("/"));
            }
            false
        } else {
            add_parents(root, &mut targets, target)?;
            remember_target(root, &mut targets, target)?;
            match &targets[target] {
                Target::Absent => false,
                Target::File(existing)
                    if existing.stamp.size == input.size() && existing.hash == input.sha256() =>
                {
                    true
                }
                _ => {
                    return Err(format!(
                        "目标已有不同内容的共享资源，已保留原文件：{target}"
                    ))
                }
            }
        };
        total = total
            .checked_add(input.size())
            .filter(|n| *n <= MAX_BYTES)
            .ok_or("构建输出内容超过大小限制")?;
        files.push(JournalFile {
            target: target.into(),
            size: input.size(),
            hash: input.sha256().into(),
            reuse,
            staged: None,
        });
    }
    for path in &outputs.directories {
        relative(path)?;
        if forbidden(path) || names.contains(path) {
            return Err(format!("构建目录与文件冲突：{path}"));
        }
        if let Some(path) = path.strip_prefix(&prefix) {
            let parts = relative(path)?;
            for depth in 1..=parts.len() {
                dirs.insert(parts[..depth].join("/"));
            }
        } else if path != &instance_target {
            if !shared(path) {
                return Err("构建输出包含未声明的根目录或其他实例".into());
            }
            add_parents(root, &mut targets, path)?;
            remember_target(root, &mut targets, path)?;
            if matches!(targets[path], Target::File(_)) {
                return Err("目标共享资源目录已被文件占用".into());
            }
        }
    }
    if dirs.len() > MAX_FILES {
        return Err("构建实例目录节点数量超过安全限制".into());
    }
    for dir in &dirs {
        if names.contains(&format!("{prefix}{dir}")) {
            return Err("构建输出包含文件与目录祖先冲突".into());
        }
    }
    // Every shared directory must be checked against the merged file map,
    // including paths contributed only by empty directories.
    for path in targets.keys() {
        if names.contains(path) && matches!(targets[path], Target::Directory(_)) {
            return Err("构建共享输出的目录与文件冲突".into());
        }
    }
    for path in names.iter().chain(outputs.directories.iter()) {
        let parts = relative(path)?;
        for depth in 1..parts.len() {
            if names.contains(&parts[..depth].join("/")) {
                return Err("构建输出包含文件与目录祖先冲突".into());
            }
        }
    }
    let destination_dirs = targets
        .iter()
        .filter(|(path, _)| *path != &instance_target && !names.contains(*path))
        .map(|(path, target)| {
            Ok((
                path.clone(),
                DestinationDirectory {
                    before: match target {
                        Target::Directory(key) => Some(key.clone()),
                        Target::Absent => None,
                        Target::File(_) => return Err("构建共享目录计划无效".to_owned()),
                    },
                    created: None,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut planned = j.clone();
    planned.files = files;
    planned.targets = targets;
    planned.destination_dirs = destination_dirs;
    planned.instance_dirs = dirs.into_iter().map(|path| (path, None)).collect();
    planned.state = State::Staging;
    validate_journal(&planned, &planned.operation_id)?;
    *j = planned;
    Ok(())
}

pub(super) fn stage_native(
    operation: &Dir,
    j: &mut Journal,
    outputs: &VerifiedOutputs,
    cancel: &AtomicBool,
    notify: &impl Fn(u64, u64),
) -> Result<()> {
    let stage = owned_dir(operation, "files", &j.files_key)?.ok_or("构建暂存文件目录缺失")?;
    let mut completed = 0u64;
    for (index, input) in outputs.files.iter().enumerate() {
        check(cancel)?;
        input.verify(Some(cancel))?;
        if j.files[index].reuse {
            continue;
        }
        let mut source = input.reader()?;
        let mut file = stage.stage_for(&slot(index), input.size())?;
        let mut hash = Sha256::new();
        let mut written = 0u64;
        let mut buffer = [0u8; 128 * 1024];
        loop {
            check(cancel)?;
            let n = source.read(&mut buffer).map_err(error)?;
            if n == 0 {
                break;
            }
            written = written
                .checked_add(n as u64)
                .filter(|n| *n <= input.size())
                .ok_or("构建输入大小超过已校验快照")?;
            hash.update(&buffer[..n]);
            file.write_all(&buffer[..n]).map_err(error)?;
            completed = completed.checked_add(n as u64).ok_or("构建输出内容过大")?;
            notify(index as u64, completed);
        }
        if written != input.size() || format!("{:x}", hash.finalize()) != input.sha256() {
            return Err("构建输入复制后的大小或 SHA256 校验失败".into());
        }
        input.verify(Some(cancel))?;
        file.sync_all().map_err(error)?;
        let snapshot = hash_file(file.try_clone().map_err(error)?, MAX_FILE, Some(cancel))?;
        register_ownership(
            operation,
            j,
            OwnershipRecord::File {
                index,
                snapshot: snapshot.clone(),
            },
        )?;
        j.files[index].staged = Some(snapshot);
        check(cancel)?;
        link_anonymous(&mut file, &stage, &slot(index))?;
    }
    Ok(())
}

pub(super) fn verify_inputs(outputs: &VerifiedOutputs, cancel: Option<&AtomicBool>) -> Result<()> {
    for input in &outputs.files {
        input.verify(cancel)?;
    }
    Ok(())
}
