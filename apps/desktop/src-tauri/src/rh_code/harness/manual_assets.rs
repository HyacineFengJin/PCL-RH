//! Explicit editor saves share the native journal, never an Agent approval.
//! Capture only the requested original assets, including binary PNG exports;
//! all paths, hashes and directory identities are rechecked before publication.
use super::*;
use crate::rh_code::assets::AssetWrite;

pub(in crate::rh_code) fn save_assets(
    store_root: &Path,
    project: &Project,
    files: &[AssetWrite],
) -> Result<Value, String> {
    if files.is_empty() || files.len() > 2 {
        return Err("An asset save contains one document or one model/texture pair".into());
    }
    let store = store(store_root)?;
    let _lease = lock(&store)?;
    recovery_locked(&store, project)?;
    let root = root(project)?;
    let root_identity = root.key()?;
    let mut originals = vec![];
    let mut changes = vec![];
    let mut paths = HashSet::new();
    for file in files {
        source_path(&file.path)?;
        if !paths.insert(&file.path)
            || file.bytes.len() > MAX_FILE
            || file.expected.as_ref().is_some_and(|v| !exact_hash(v))
        {
            return Err("Invalid asset file or expected fingerprint".into());
        }
        let (parent_path, name) = split_path(&file.path);
        let parent = prospective_parent(&root, parent_path)?;
        let before = match &parent {
            Some(dir) if dir.stat(name)?.is_some() => Some(dir.read(name, MAX_FILE)?),
            _ => None,
        };
        if before.as_ref().map(|v| &v.0.sha256) != file.expected.as_ref() {
            return Err(format!("Asset changed or export already exists: {}. Choose another output path or reload before saving.", file.path));
        }
        if let Some((state, _)) = &before {
            originals.push(ManifestEntry {
                path: file.path.clone(),
                sha256: state.sha256.clone(),
                size: state.size,
            });
        }
        changes.push(Change {
            entry: Entry {
                path: file.path.clone(),
                temporary: format!(".rh-harness-{}", id()),
                before: before.as_ref().map(|v| v.0.clone()),
                after_sha256: Some(hash(&file.bytes)),
                after_size: file.bytes.len(),
                staged: None,
                parent_identity: parent.as_ref().map(Dir::key).transpose()?,
            },
            before_bytes: before.map(|v| v.1),
            after_bytes: Some(file.bytes.clone()),
        });
    }
    for first in &paths {
        if paths
            .iter()
            .any(|other| other.starts_with(&format!("{first}/")))
        {
            return Err("An asset output conflicts with another output directory".into());
        }
    }
    originals.sort_by(|a, b| a.path.cmp(&b.path));
    let binding = parse_binding(Binding {
        schema_version: 1,
        job_id: format!("asset-editor-{}", id()),
        project_id: project.id.clone(),
        conversation_id: "manual-asset-editor".into(),
        folder: project.folder.clone(),
        root_identity: root_identity.clone(),
        source_fingerprint: manifest_fingerprint(&originals)?,
        source_manifest: originals,
    })?;
    verify_root(project, &root_identity)?;
    store.ensure_child("harness-bindings")?.immutable(
        &binding_name(&binding.job_id),
        &serde_json::to_vec(&binding).map_err(error)?,
    )?;
    let receipt = json!(files
        .iter()
        .map(|v| json!({"path":v.path,"expected":v.expected,"sha256":hash(&v.bytes)}))
        .collect::<Vec<_>>());
    let digest = hash(&serde_json::to_vec(&receipt).map_err(error)?);
    let proposal_id = digest[..32].to_owned();
    let transaction = store
        .ensure_child("harness-transactions")?
        .ensure_child(&hash(
            format!("{}:{proposal_id}", binding.job_id).as_bytes(),
        ))?;
    let journal = Journal {
        schema_version: 1,
        binding,
        proposal_id,
        digest: digest.clone(),
        export_hash: digest,
        phase: "preparing".into(),
        cleanup_complete: false,
        entries: changes.iter().map(|v| v.entry.clone()).collect(),
        created_directories: vec![],
    };
    commit_changes(project, &root, &transaction, journal, &changes, false)
}
