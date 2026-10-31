//! Dynamic inode bindings, complete immediate resource inventories and history discovery.
//! Instance names appear only in the live Context, allowing permanent history
//! to travel with a physical rename or delete/restore without reference replay.
use super::*;
use sha2::{Digest, Sha512};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub(super) isolated: bool,
    pub(super) before: Option<Key>,
    pub(super) created: Option<Key>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scope {
    pub(super) root: Key,
    pub(super) versions: Key,
    pub(super) instance: Key,
    pub(super) history: Option<Key>,
    pub(super) operation: Option<Key>,
    pub(super) pcl: Option<Key>,
    pub(super) markers: Option<Key>,
    pub(super) resources: BTreeMap<String, Binding>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Node {
    File {
        owned: Owned,
    },
    Directory {
        key: Key,
        #[cfg(unix)]
        mode: u32,
        #[cfg(windows)]
        security: pcl_core::retained_fs::Security,
    },
}
pub(super) type Inventory = BTreeMap<String, BTreeMap<String, Node>>;
pub(super) struct Context {
    pub(super) path: PathBuf,
    pub(super) id: String,
    pub(super) root: Dir,
    pub(super) versions: Dir,
    pub(super) instance: Dir,
    pub(super) history: Option<Dir>,
    pub(super) operation: Option<Dir>,
    pub(super) operation_id: String,
    profile_token: Option<String>,
    policy: Option<IsolationPolicy>,
}
impl Context {
    pub(super) fn base(root: &Path, id: &str) -> Result<Self> {
        super::super::safe_name(id)?;
        let path = root.canonicalize().map_err(err)?;
        let root = Dir::open(&path)?;
        let versions = root.child("versions")?;
        let instance = versions.child(id)?;
        Ok(Self {
            path,
            id: id.into(),
            root,
            versions,
            instance,
            history: None,
            operation: None,
            operation_id: String::new(),
            profile_token: None,
            policy: None,
        })
    }
    pub(super) fn existing(root: &Path, id: &str, operation: &str) -> Result<Self> {
        if !valid_id(operation) {
            return Err("更新历史标识无效".into());
        }
        let mut c = Self::base(root, id)?;
        let history = c.instance.child(HISTORY)?;
        c.operation = Some(
            history
                .child(operation)?
                .with_documents_max(MAX_JOURNAL_BYTES)?,
        );
        c.history = Some(history);
        c.operation_id = operation.into();
        Ok(c)
    }
    pub(super) fn check(&self, j: &Journal) -> Result<()> {
        let root = Dir::open(&self.path)?;
        if root.key()? != j.scope.root
            || self.root.key()? != j.scope.root
            || root.child("versions")?.key()? != j.scope.versions
            || root.child("versions")?.child(&self.id)?.key()? != j.scope.instance
            || self.instance.key()? != j.scope.instance
        {
            return Err("绑定的更新实例或游戏目录已经变化".into());
        }
        if let Some(history) = &self.history {
            if Some(history.key()?) != j.scope.history
                || self.instance.child(HISTORY)?.key()? != history.key()?
            {
                return Err("资源更新历史命名空间已经变化".into());
            }
        }
        if let Some(op) = &self.operation {
            if Some(op.key()?) != j.scope.operation
                || self
                    .history
                    .as_ref()
                    .ok_or("历史目录缺失")?
                    .child(&j.id)?
                    .key()?
                    != op.key()?
            {
                return Err("资源更新事务目录已经变化".into());
            }
        }
        if let Some(key) = &j.scope.pcl {
            if root.child(".pcl-linux")?.key()? != *key
                || Some(root.child(".pcl-linux")?.child(MARKERS)?.key()?) != j.scope.markers
            {
                return Err("资源更新根标记命名空间已经变化".into());
            }
        }
        Ok(())
    }
    pub(super) fn base_for(&self, binding: &Binding) -> Result<Dir> {
        if binding.isolated {
            self.root.child("versions")?.child(&self.id)
        } else {
            Dir::open(&self.path)
        }
    }
    pub(super) fn resources(&self, j: &Journal, kind: &str, created: bool) -> Result<Option<Dir>> {
        self.check(j)?;
        let b = j.scope.resources.get(kind).ok_or("更新资源类型未绑定")?;
        let actual = self.base_for(b)?.optional(kind)?;
        let expected = b
            .before
            .as_ref()
            .or(if created { b.created.as_ref() } else { None });
        if let Some(dir) = &actual {
            if Some(dir.key()?).as_ref() != expected {
                return Err("资源更新目录 inode 已变化，已保留".into());
            }
        } else if b.before.is_some() {
            return Err("原资源目录已经移走，已保留".into());
        }
        Ok(actual)
    }
    pub(super) fn validate_actual_scope(&self, j: &Journal) -> Result<()> {
        self.check(j)?;
        for (kind, b) in &j.scope.resources {
            let expected = if b.isolated {
                self.path.join("versions").join(&self.id).join(kind)
            } else {
                self.path.join(kind)
            };
            // Live preparations follow their captured policy. Undo/recovery
            // follows each journal's recorded base, independent of new settings.
            let policy = self.policy.unwrap_or(if b.isolated {
                IsolationPolicy::Isolated
            } else {
                IsolationPolicy::Shared
            });
            if crate::ui_data::resource_dir_with_isolation(&self.path, &self.id, kind, policy)?
                != expected
            {
                return Err("实例资源隔离规则已变化，拒绝更新或撤销".into());
            }
        }
        Ok(())
    }
    pub(super) fn inventory_matches(
        &self,
        j: &Journal,
        before: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<()> {
        self.check(j)?;
        let actual = inventory(self, &j.scope, cancel)?;
        let wanted = if before { &j.before } else { &j.after };
        if &actual != wanted {
            return Err("资源目录文件清单或内容已变化，拒绝更新或撤销".into());
        }
        for (kind, b) in &j.scope.resources {
            let current = self
                .base_for(b)?
                .optional(kind)?
                .map(|d| d.key())
                .transpose()?;
            let expected = if before {
                b.before.as_ref()
            } else {
                b.before.as_ref().or(b.created.as_ref())
            };
            if current.as_ref() != expected {
                return Err("资源目录存在状态或 inode 已变化".into());
            }
        }
        Ok(())
    }
    pub(super) fn live(&self, j: &Journal, before: bool, cancel: &AtomicBool) -> Result<()> {
        self.validate_actual_scope(j)?;
        if let Some(token) = &self.profile_token {
            if super::super::token_for(&self.instance.regular(&format!("{}.json", self.id))?)?
                != *token
            {
                return Err("实例版本文件在更新期间已经变化".into());
            }
        }
        self.inventory_matches(j, before, Some(cancel))?;
        if before {
            for item in &j.items {
                if let Some(old) = &item.old {
                    let dir = self
                        .resources(j, &item.kind, false)?
                        .ok_or("原资源目录缺失")?;
                    if super::super::token_for(&dir.regular(&old.name)?)? != old.fingerprint {
                        return Err("旧资源 fingerprint 已变化".into());
                    }
                }
            }
        }
        Ok(())
    }
}
fn snapshot(dir: &Dir, name: &str, cancel: Option<&AtomicBool>) -> Result<Owned> {
    let mut file = dir.regular(name)?;
    let m = file.metadata().map_err(err)?;
    let token = super::super::token_for(&file)?;
    if !filesystem::single(&file)? || m.len() > MAX_FILE_BYTES {
        return Err("本地资源不是独立普通文件或超过 2 GiB".into());
    }
    let mut digest = Sha512::new();
    let mut count = 0;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if let Some(c) = cancel {
            check(c)?;
        }
        let n = file.read(&mut buffer).map_err(err)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > m.len() {
            return Err("本地资源读取期间大小已变化".into());
        }
        digest.update(&buffer[..n]);
    }
    if count != m.len()
        || super::super::token_for(&file)? != token
        || super::super::token_for(&dir.regular(name)?)? != token
    {
        return Err("本地资源读取期间被替换或修改".into());
    }
    Owned::from_file(&file, count, format!("{:x}", digest.finalize()))
}
pub(super) fn inventory(
    context: &Context,
    scope: &Scope,
    cancel: Option<&AtomicBool>,
) -> Result<Inventory> {
    let mut result = BTreeMap::new();
    let mut count = 0;
    let mut size = 0u64;
    for (kind, b) in &scope.resources {
        let mut nodes = BTreeMap::new();
        if let Some(dir) = context.base_for(b)?.optional(kind)? {
            for name in dir.names()? {
                if let Some(c) = cancel {
                    check(c)?;
                }
                count += 1;
                if count > 2048 {
                    return Err("资源目录条目超过更新安全上限".into());
                }
                let stat = dir.stat(&name)?.ok_or("资源文件读取期间消失")?;
                let node = if filesystem::stat_regular(&stat) {
                    let owned = snapshot(&dir, &name, cancel)?;
                    size = size
                        .checked_add(owned.size)
                        .filter(|s| *s <= 16 * 1024 * 1024 * 1024)
                        .ok_or("本地资源总量超过 16 GiB")?;
                    Node::File { owned }
                } else if filesystem::stat_directory(&stat) {
                    Node::Directory {
                        key: dir.child(&name)?.key()?,
                        #[cfg(unix)]
                        mode: filesystem::stat_mode(&stat),
                        #[cfg(windows)]
                        security: filesystem::stat_security(&stat),
                    }
                } else {
                    return Err("资源目录包含链接或特殊文件，已保留".into());
                };
                nodes.insert(name, node);
            }
        }
        result.insert(kind.clone(), nodes);
    }
    Ok(result)
}
#[cfg(test)]
pub(super) fn prepare(
    root: &Path,
    id: &str,
    files: &[VerifiedImport],
    replacements: &[Replacement],
    cancel: &AtomicBool,
) -> Result<(Context, Journal)> {
    prepare_with_isolation(
        root,
        id,
        files,
        replacements,
        cancel,
        IsolationPolicy::Automatic,
    )
}
pub(super) fn prepare_with_isolation(
    root: &Path,
    id: &str,
    files: &[VerifiedImport],
    replacements: &[Replacement],
    cancel: &AtomicBool,
    policy: IsolationPolicy,
) -> Result<(Context, Journal)> {
    pcl_core::identifier(id)?;
    let mut context = Context::base(root, id)?;
    context.policy = Some(policy);
    context.profile_token = Some(super::super::token_for(
        &context.instance.regular(&format!("{id}.json"))?,
    )?);
    let mut resources = BTreeMap::new();
    for kind in KINDS {
        let path = crate::ui_data::resource_dir_with_isolation(root, id, kind, policy)?;
        let isolated = if path == root.join(kind) {
            false
        } else if path == root.join("versions").join(id).join(kind) {
            true
        } else {
            return Err("资源隔离目录不受支持".into());
        };
        let base = if isolated {
            context.root.child("versions")?.child(id)?
        } else {
            Dir::open(root)?
        };
        resources.insert(
            kind.into(),
            Binding {
                isolated,
                before: base.optional(kind)?.map(|d| d.key()).transpose()?,
                created: None,
            },
        );
    }
    let mut scope = Scope {
        root: context.root.key()?,
        versions: context.versions.key()?,
        instance: context.instance.key()?,
        history: None,
        operation: None,
        pcl: None,
        markers: None,
        resources,
    };
    let before = inventory(&context, &scope, Some(cancel))?;
    let mut items = Vec::new();
    let mut old_bytes = 0u64;
    for f in files {
        let old = if let Some(r) = replacements
            .iter()
            .find(|r| r.kind == f.kind && r.new_file_name == f.file_name)
        {
            let owned = match before.get(&r.kind).and_then(|n| n.get(&r.old_file_name)) {
                Some(Node::File { owned }) => owned.clone(),
                _ => return Err("待更新旧文件不存在或不是普通文件".into()),
            };
            if owned.sha512 != r.old_sha512 {
                return Err("待更新旧文件 SHA512 已变化".into());
            }
            old_bytes = old_bytes
                .checked_add(owned.size)
                .filter(|s| *s <= MAX_BATCH_BYTES)
                .ok_or("更新旧文件备份超过 8 GiB")?;
            let dir = context
                .base_for(&scope.resources[&r.kind])?
                .child(&r.kind)?;
            if super::super::token_for(&dir.regular(&r.old_file_name)?)? != r.old_fingerprint {
                return Err("待更新旧文件 fingerprint 已变化".into());
            }
            Some(Old {
                name: r.old_file_name.clone(),
                fingerprint: r.old_fingerprint.clone(),
                owned,
                backup: None,
            })
        } else {
            None
        };
        let nodes = &before[&f.kind];
        if nodes.contains_key(&counterpart(&f.file_name))
            || nodes.contains_key(&f.file_name)
                && old.as_ref().is_none_or(|o| o.name != f.file_name)
        {
            return Err("更新目标或其禁用名称已占用，拒绝覆盖".into());
        }
        items.push(Item {
            kind: f.kind.clone(),
            name: f.file_name.clone(),
            size: f.size,
            sha512: f.sha512.clone(),
            new: None,
            old,
        });
    }
    let id = new_id();
    let mut j = Journal {
        schema: SCHEMA,
        id: id.clone(),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(err)?
            .as_secs(),
        scope: scope.clone(),
        marker: None,
        state: State::Staging,
        before,
        after: BTreeMap::new(),
        items,
    };
    context.live(&j, true, cancel)?;
    if files.is_empty() {
        return Ok((context, j));
    }
    let history = context.instance.ensure(HISTORY)?;
    if history.names()?.len() >= MAX_HISTORIES {
        return Err("更新历史达到256条，请先处理旧历史".into());
    }
    let operation = history.mkdir(&id)?.with_documents_max(MAX_JOURNAL_BYTES)?;
    scope.history = Some(history.key()?);
    scope.operation = Some(operation.key()?);
    let pcl = context.root.ensure(".pcl-linux")?;
    let markers = pcl.ensure(MARKERS)?;
    scope.pcl = Some(pcl.key()?);
    scope.markers = Some(markers.key()?);
    j.scope = scope;
    context.history = Some(history);
    context.operation = Some(operation);
    context.operation_id = id;
    // The initial manifest registers the private directory before any named
    // ownership receipt. Root marker publication then protects all later staging.
    let result = (|| {
        journal::write(&context, &j)?;
        journal::create_marker(&context, &mut j)
    })();
    if let Err(e) = result {
        return match recovery::setup_failed(&context, &j) {
            Ok(()) => Err(e),
            Err(c) => Err(format!("资源更新恢复失败：{c}；原错误：{e}")),
        };
    }
    Ok((context, j))
}
pub(super) fn expected_after(j: &Journal) -> Result<Inventory> {
    let mut after = j.before.clone();
    for item in &j.items {
        let nodes = after.get_mut(&item.kind).ok_or("资源 inventory 缺失")?;
        if let Some(old) = &item.old {
            nodes.remove(&old.name);
        }
        let owned = item.new.clone().ok_or("新资源所有权未登记")?;
        if nodes
            .insert(item.name.clone(), Node::File { owned })
            .is_some()
        {
            return Err("更新映射覆盖现有资源".into());
        }
    }
    Ok(after)
}
pub(super) fn create_targets(context: &Context, j: &mut Journal) -> Result<()> {
    let kinds: BTreeSet<_> = j.items.iter().map(|i| i.kind.clone()).collect();
    for kind in kinds {
        context.check(j)?;
        if j.scope.resources[&kind].before.is_some() {
            context.resources(j, &kind, false)?;
            continue;
        }
        let operation = context.operation.as_ref().ok_or("更新目录缺失")?;
        let private = format!("target-{kind}");
        let dir = operation.mkdir(&private)?;
        let key = dir.key()?;
        journal::register_directory(context, j, &kind, &key)?;
        j.scope
            .resources
            .get_mut(&kind)
            .ok_or("资源目录登记缺失")?
            .created = Some(key.clone());
        let base = context.base_for(&j.scope.resources[&kind])?;
        base.absent(&kind)?;
        context.check(j)?;
        if operation.child(&private)?.key()? != key {
            return Err("新资源目录被替换".into());
        }
        operation.move_directory(&private, &base, &kind)?;
    }
    Ok(())
}
pub(super) fn remove_created_targets(context: &Context, j: &Journal) -> Result<()> {
    for (kind, b) in &j.scope.resources {
        if b.before.is_some() || b.created.is_none() {
            continue;
        }
        if let Some(dir) = context.resources(j, kind, true)? {
            if !dir.names()?.is_empty() {
                return Err("新建资源目录包含外部内容，已保留".into());
            }
            let base = context.base_for(b)?;
            context.check(j)?;
            if base.child(kind)?.key()? != dir.key()? {
                return Err("资源目录清理前被替换".into());
            }
            base.unlink(kind, true)?;
        }
    }
    Ok(())
}
pub(super) fn contexts(root: &Path) -> Result<Vec<Context>> {
    let path = root.canonicalize().map_err(err)?;
    let root_dir = Dir::open(&path)?;
    let Some(versions) = root_dir.optional("versions")? else {
        return Ok(Vec::new());
    };
    let mut contexts = Vec::new();
    for name in versions.names_with_limit(4096)? {
        let entry = versions.stat(&name)?.ok_or("versions 目录变化")?;
        if !filesystem::stat_directory(&entry) {
            continue;
        }
        let instance = versions.child(&name)?;
        let Some(history) = instance.optional(HISTORY)? else {
            continue;
        };
        let names = history.names()?;
        if names.len() > MAX_HISTORIES {
            return Err("更新历史数量越界".into());
        }
        for operation_id in names {
            if !valid_id(&operation_id) {
                return Err("更新历史包含未知记录，已保留".into());
            }
            let operation = history
                .child(&operation_id)?
                .with_documents_max(MAX_JOURNAL_BYTES)?;
            contexts.push(Context {
                path: path.clone(),
                id: name.clone(),
                root: Dir::open(&path)?,
                versions: root_dir.child("versions")?,
                instance: versions.child(&name)?,
                history: Some(instance.child(HISTORY)?),
                operation: Some(operation),
                operation_id,
                profile_token: None,
                policy: None,
            });
        }
    }
    Ok(contexts)
}
pub(super) fn marker_store(root: &Path) -> Result<Option<Dir>> {
    let root = Dir::open(root)?;
    let Some(pcl) = root.optional(".pcl-linux")? else {
        return Ok(None);
    };
    pcl.optional(MARKERS)
}
pub(super) fn ensure_ready(root: &Path) -> Result<()> {
    let path = root.canonicalize().map_err(err)?;
    if let Some(markers) = marker_store(&path)? {
        if !markers.names()?.is_empty() {
            return Err("存在未完成的资源更新或撤销，请先恢复后再操作或启动".into());
        }
    }
    for context in contexts(&path)? {
        if !journal::status_ready(&context)? {
            return Err("存在未完成的资源更新或撤销，请先恢复后再操作或启动".into());
        }
    }
    Ok(())
}
pub(super) fn history(root: &Path, id: &str) -> Result<Vec<UpdateHistory>> {
    let context = Context::base(root, id)?;
    let Some(history) = context.instance.optional(HISTORY)? else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for operation in history.names()? {
        let c = Context::existing(&context.path, id, &operation)?;
        let j = journal::read(&c)?;
        c.check(&j)?;
        if j.state == State::Committed {
            result.push(UpdateHistory {
                id: j.id,
                files: j
                    .items
                    .iter()
                    .map(|i| match &i.old {
                        Some(old) => format!("{} → {}", old.name, i.name),
                        None => i.name.clone(),
                    })
                    .collect(),
                created_at: j.created_at,
            });
        }
    }
    result.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    Ok(result)
}
