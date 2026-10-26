//! Adapter from a checked local selection to the common import publisher.
//! No ZIP, writable hard links or second full build tree is needed. Native
//! inputs share one pinned source root; the publisher owns independent copies.
use super::*;
use publication::{VerifiedFile, VerifiedOutputs};
use std::sync::Arc;

pub(crate) struct LocalSnapshot {
    selection: crate::instance_export::ExportPlan,
    root: Arc<Dir>,
    root_key: Key,
    versions_key: Key,
    names: Vec<String>,
    name: String,
    outputs: VerifiedOutputs,
}
fn names(root: &Dir) -> Result<Vec<String>> {
    Ok(root
        .names()?
        .into_iter()
        .filter(|n| n != ".pcl-linux")
        .collect())
}
impl LocalSnapshot {
    pub(crate) fn counts(&self) -> (usize, u64) {
        (
            self.outputs.files.len(),
            self.outputs.files.iter().map(|f| f.size()).sum(),
        )
    }
    pub(crate) fn prepare(
        selection: crate::instance_export::ExportPlan,
        name: &str,
    ) -> Result<Self> {
        name_ok(name)?;
        guards(&selection.root)?;
        crate::instance_delete::ensure_name_available(&selection.root, name)?;
        let root = Arc::new(Dir::open(&selection.root)?);
        if root.child("versions")?.stat(name)?.is_some() {
            return Err("目标实例名称已存在，请使用其他名称".into());
        }
        selection.validate_copy_source(&AtomicBool::new(false), true)?;
        let names = names(&root)?;
        let mut scan = archive::ArchiveScan {
            manifest: serde_json::from_slice(selection.manifest_bytes()).map_err(error)?,
            entries: BTreeMap::new(),
            json: BTreeMap::new(),
        };
        for (index, file) in selection.selected_files().into_iter().enumerate() {
            let path = format!(".minecraft/{}", file.path);
            if !file.directory && file.path.starts_with("versions/") && file.path.ends_with(".json")
            {
                let parts = file.path.split('/').collect::<Vec<_>>();
                if parts.len() == 3 && parts[2] == format!("{}.json", parts[1]) {
                    let mut bytes = Vec::new();
                    root.file(&file.path)?
                        .take(MAX_JSON + 1)
                        .read_to_end(&mut bytes)
                        .map_err(error)?;
                    if bytes.len() as u64 > MAX_JSON
                        || format!("{:x}", Sha256::digest(&bytes)) != file.hash
                    {
                        return Err(changed());
                    }
                    scan.json.insert(path.clone(), bytes);
                }
            }
            scan.entries.insert(
                path.clone(),
                ArchiveEntry {
                    index,
                    path,
                    directory: file.directory,
                    size: file.bytes,
                    hash: file.hash,
                },
            );
        }
        let mut used = BTreeSet::new();
        let (mut metadata, jar) = resolve_version(
            &scan,
            &scan.manifest.game.instance,
            &mut BTreeSet::new(),
            &mut used,
        )?;
        let object = metadata.as_object_mut().ok_or("版本 JSON 无效")?;
        object.remove("inheritsFrom");
        object.insert("id".into(), name.into());
        object.insert("jar".into(), name.into());
        object.insert(
            "clientVersion".into(),
            scan.manifest.game.minecraft.clone().into(),
        );
        let metadata = serde_json::to_vec_pretty(&metadata).map_err(error)?;
        if metadata.len() as u64 > MAX_JSON {
            return Err("合并后的版本 JSON 过大".into());
        }
        let prefix = format!("versions/{name}");
        let mut outputs = VerifiedOutputs {
            files: Vec::new(),
            directories: BTreeSet::from([format!("{prefix}/config")]),
        };
        let mut generated = root.anonymous()?;
        generated.write_all(&metadata).map_err(error)?;
        outputs.files.push(VerifiedFile::from_fd(
            format!("{prefix}/{name}.json"),
            generated,
            metadata.len() as u64,
            &format!("{:x}", Sha256::digest(&metadata)),
            &AtomicBool::new(false),
        )?);
        let mut add_file = |target: String, entry: &ArchiveEntry| -> Result<()> {
            let path = entry
                .path
                .strip_prefix(".minecraft/")
                .ok_or("实例来源布局无效")?;
            let expected = hash_file(root.file(path)?, MAX_FILE, None)?;
            if expected.stamp.size != entry.size || expected.hash != entry.hash {
                return Err(changed());
            }
            outputs.files.push(VerifiedFile::mapped_snapshot(
                target,
                path.into(),
                root.clone(),
                expected,
            ));
            Ok(())
        };
        add_file(format!("{prefix}/{name}.jar"), &scan.entries[&jar])?;
        for (path, entry) in &scan.entries {
            if used.contains(path) {
                continue;
            }
            let content = if scan.manifest.game.isolated {
                path.strip_prefix(&format!("{}/", scan.manifest.content_root))
            } else {
                path.strip_prefix(".minecraft/")
                    .filter(|p| !p.starts_with("versions/"))
            };
            let Some(content) = content.filter(|p| !p.is_empty()) else {
                continue;
            };
            let target = format!("{prefix}/{content}");
            if entry.directory {
                outputs.directories.insert(target);
            } else {
                add_file(target, entry)?;
            }
        }
        let snapshot = Self {
            root_key: root.key()?,
            versions_key: root.child("versions")?.key()?,
            root,
            names,
            selection,
            name: name.into(),
            outputs,
        };
        snapshot.verify(&AtomicBool::new(false))?;
        Ok(snapshot)
    }
    fn verify(&self, cancel: &AtomicBool) -> Result<()> {
        if names(&self.root)? != self.names {
            return Err(changed());
        }
        self.selection.validate_copy_source(cancel, false)
    }
    pub(crate) fn execute(
        self,
        cancel: &AtomicBool,
        callback: &(dyn Fn(Progress) + Send + Sync),
        commit: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        self.verify(cancel)?;
        let operation = build::BuildOperation::begin_bound(
            &self.selection.root,
            &self.name,
            &self.root_key,
            Some(&self.versions_key),
        )?;
        let Self {
            selection,
            root,
            names: expected_names,
            outputs,
            ..
        } = self;
        operation.publish_external_checked(
            outputs,
            cancel,
            callback,
            || {
                if names(&root)? != expected_names {
                    return Err(changed());
                }
                selection.validate_copy_source(&AtomicBool::new(false), false)
            },
            commit,
        )
    }
}
