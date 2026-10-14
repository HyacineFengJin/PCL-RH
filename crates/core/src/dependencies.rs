//! Read-only installed-file resolution shared by preparation and inspection.
//! This keeps platform admission, inheritance-selected jars and asset checks in
//! one place. Native extraction and argument expansion belong to launch only.
use crate::{
    deduplicate_launch_paths, identifier, library_path, read_json, rules::RuleContext, safe_join,
    text, Result,
};
use serde_json::Value;
use std::{collections::HashSet, path::PathBuf};

pub(crate) const MISSING_FILE_LIMIT: usize = 32;

#[derive(Default)]
pub(crate) struct MissingFiles {
    pub paths: Vec<String>,
    pub total: usize,
    seen: HashSet<PathBuf>,
}

impl MissingFiles {
    fn present(&mut self, path: &std::path::Path) -> bool {
        if path.is_file() {
            return true;
        }
        // Repeated library declarations refer to the same installed file.
        // Count it once, while retaining only a bounded public preview.
        if self.seen.insert(path.to_path_buf()) {
            self.total += 1;
            if self.paths.len() < MISSING_FILE_LIMIT {
                self.paths.push(path.display().to_string());
            }
        }
        false
    }
}

pub(crate) struct InstalledDependencies {
    pub classpath: Vec<PathBuf>,
    pub native_jars: Vec<(PathBuf, Value)>,
    pub asset_id: String,
    pub log_config: Option<PathBuf>,
    pub missing: MissingFiles,
    // The parsed index is retained only for the inspector's after-probe
    // comparison; it never becomes part of the public report.
    pub asset_index: Option<Value>,
}

pub(crate) fn resolve(
    root: &std::path::Path,
    id: &str,
    data: &Value,
    context: &RuleContext,
) -> Result<InstalledDependencies> {
    let mut classpath = vec![];
    let mut native_jars = vec![];
    let mut missing = MissingFiles::default();
    let mut asset_index = None;
    for lib in data["libraries"].as_array().into_iter().flatten() {
        if !context.library_allowed(lib)? {
            continue;
        }
        let name = text(lib, "name");
        let artifact = &lib["downloads"]["artifact"];
        if !artifact.is_null() || lib.get("downloads").is_none() {
            let path = library_path(root, artifact, name)?;
            if missing.present(&path) {
                // Modern native artifacts also remain on the classpath.
                classpath.push(path.clone());
                if context.platform.native_artifact(name)? {
                    native_jars.push((path, lib.clone()));
                }
            }
        }
        if let Some(classifier) = context.platform.legacy_native_classifier(lib)? {
            let native = &lib["downloads"]["classifiers"][&classifier];
            let path = library_path(root, native, &format!("{name}:{classifier}"))?;
            if missing.present(&path) {
                native_jars.push((path, lib.clone()));
            }
        }
    }
    let jar_id = data["jar"].as_str().unwrap_or(id);
    identifier(jar_id)?;
    let jar = safe_join(root, format!("versions/{jar_id}/{jar_id}.jar"))?;
    if missing.present(&jar) {
        classpath.push(jar);
    }
    deduplicate_launch_paths(&mut classpath, &mut native_jars);
    let asset_id = data["assetIndex"]["id"]
        .as_str()
        .or_else(|| data["assets"].as_str())
        .unwrap_or("");
    if !asset_id.is_empty() {
        identifier(asset_id)?;
        let index = safe_join(root, format!("assets/indexes/{asset_id}.json"))?;
        if missing.present(&index) {
            let index_data = read_json(&index)?;
            for object in index_data["objects"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(_, v)| v)
            {
                let hash = text(object, "hash");
                if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("Invalid asset object hash".into());
                }
                let path = safe_join(root, format!("assets/objects/{}/{}", &hash[..2], hash))?;
                missing.present(&path);
            }
            asset_index = Some(index_data);
        }
    }
    let logging = &data["logging"]["client"];
    let log_config = if let Some(log_id) = logging["file"]["id"].as_str() {
        identifier(log_id)?;
        let path = safe_join(root, format!("assets/log_configs/{log_id}"))?;
        missing.present(&path);
        Some(path)
    } else {
        None
    };
    Ok(InstalledDependencies {
        classpath,
        native_jars,
        asset_id: asset_id.into(),
        log_config,
        missing,
        asset_index,
    })
}
