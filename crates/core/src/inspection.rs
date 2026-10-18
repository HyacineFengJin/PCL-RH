//! Launch-file inspection without preparing a game or consulting accounts.
//! The report describes a point-in-time read of metadata, installed files and
//! local Java selection. It does not establish mod compatibility or verify
//! complete content hashes. No argv, native extraction or directories are made.
use crate::{
    content_scope, dependencies, err, identifier, java, loader_description, metadata,
    minecraft_version, required_java, rules::RuleContext, text, Result,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, path::Path, path::PathBuf};

const ISSUE_LIMIT: usize = 8;
const ISSUE_TEXT_LIMIT: usize = 2048;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchInspection {
    pub id: String,
    pub minecraft_version: String,
    pub loader: String,
    pub game_dir: String,
    /// Zero means damaged metadata prevented determining the requirement.
    pub java_major: u32,
    pub java_exact: bool,
    pub java_path: Option<String>,
    pub java_error: Option<String>,
    pub missing_files: Vec<String>,
    pub missing_total: usize,
    pub issues: Vec<String>,
    pub ready: bool,
}

impl LaunchInspection {
    fn issue(&mut self, error: String) {
        if self.issues.len() < ISSUE_LIMIT {
            self.issues.push(bounded_error(error));
        }
    }
}

fn bounded_error(mut error: String) -> String {
    if error.len() > ISSUE_TEXT_LIMIT {
        let mut end = ISSUE_TEXT_LIMIT;
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
        error.push('…');
    }
    error
}

/// Installed-file readiness only: no JVM discovery/probe, accounts, extraction
/// or directory creation. The launcher uses this before enabling missing repair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReadiness {
    pub missing_total: usize,
    pub missing_files: Vec<String>,
}

/// Includes the resolved metadata so a download can recheck the exact launch
/// requirement it captured without guessing from an error message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaRequirement {
    pub major: u32,
    pub exact: bool,
    metadata: serde_json::Value,
}

pub fn java_requirement(root: &Path, id: &str) -> Result<JavaRequirement> {
    identifier(id)?;
    let root = fs::canonicalize(root).map_err(err)?;
    let data = metadata(&root, id, &mut HashSet::new())?;
    let major = required_java(&data);
    if major == 0 || major > 999 {
        return Err("实例要求的 Java 主版本无效".into());
    }
    let loader = loader_description(&data);
    Ok(JavaRequirement {
        major,
        exact: loader.starts_with("Forge ") || loader.starts_with("NeoForge "),
        metadata: data,
    })
}

pub fn inspect_files_with_isolation(
    root: &Path,
    id: &str,
    policy: content_scope::IsolationPolicy,
) -> Result<FileReadiness> {
    identifier(id)?;
    let root = fs::canonicalize(root).map_err(err)?;
    let data = metadata(&root, id, &mut HashSet::new())?;
    let scope = content_scope::inspect_with_isolation(&root, id, policy)?;
    content_scope::verified_base(&root, id, scope)?;
    let context = RuleContext::current()?;
    let dependencies = dependencies::resolve(&root, id, &data, &context)?;
    if metadata(&root, id, &mut HashSet::new())? != data {
        return Err("实例版本元数据在检查期间变化，请重新检查".into());
    }
    if content_scope::inspect_with_isolation(&root, id, policy)? != scope {
        return Err("实例隔离目录在检查期间变化，请重新检查".into());
    }
    Ok(FileReadiness {
        missing_total: dependencies.missing.total,
        missing_files: dependencies.missing.paths,
    })
}

/// Use the same bounded Java probe and selection policy as launch. The caller
/// captures settings before this call; probing does not need an account lock.
pub fn inspect_instance(
    root: &Path,
    project: &Path,
    id: &str,
    choice: &java::JavaSelection,
    extra_java_paths: &[String],
) -> Result<LaunchInspection> {
    inspect_instance_with_isolation(
        root,
        project,
        id,
        choice,
        extra_java_paths,
        content_scope::IsolationPolicy::Automatic,
    )
}

pub fn inspect_instance_with_isolation(
    root: &Path,
    project: &Path,
    id: &str,
    choice: &java::JavaSelection,
    extra_java_paths: &[String],
    policy: content_scope::IsolationPolicy,
) -> Result<LaunchInspection> {
    inspect_with_java_and_isolation(root, id, policy, |root, required, exact| {
        java::select(
            project,
            Some(root),
            extra_java_paths,
            choice,
            required,
            exact,
        )
    })
}

#[cfg(test)]
fn inspect_with_java(
    root: &Path,
    id: &str,
    select_java: impl FnOnce(&Path, u32, bool) -> Result<PathBuf>,
) -> Result<LaunchInspection> {
    inspect_with_java_and_isolation(
        root,
        id,
        content_scope::IsolationPolicy::Automatic,
        select_java,
    )
}

fn inspect_with_java_and_isolation(
    root: &Path,
    id: &str,
    policy: content_scope::IsolationPolicy,
    select_java: impl FnOnce(&Path, u32, bool) -> Result<PathBuf>,
) -> Result<LaunchInspection> {
    identifier(id)?;
    let root = fs::canonicalize(root).map_err(err)?;
    if !root.is_dir() {
        return Err("Game root is not a directory".into());
    }
    let mut report = LaunchInspection {
        id: id.into(),
        minecraft_version: String::new(),
        loader: String::new(),
        game_dir: String::new(),
        java_major: 0,
        java_exact: false,
        java_path: None,
        java_error: None,
        missing_files: Vec::new(),
        missing_total: 0,
        issues: Vec::new(),
        ready: false,
    };
    let scope = content_scope::inspect_with_isolation(&root, id, policy);
    match scope.as_ref() {
        Ok(scope) => match content_scope::verified_base(&root, id, *scope) {
            Ok(path) => report.game_dir = path.display().to_string(),
            Err(error) => report.issue(error),
        },
        Err(error) => report.issue(error.clone()),
    }
    let data = match metadata(&root, id, &mut HashSet::new()) {
        Ok(data) => data,
        Err(error) => {
            report.issue(error);
            return Ok(report);
        }
    };
    report.minecraft_version = minecraft_version(&data).into();
    report.loader = loader_description(&data);
    report.java_major = required_java(&data);
    report.java_exact =
        report.loader.starts_with("Forge ") || report.loader.starts_with("NeoForge ");
    if text(&data, "mainClass").is_empty() {
        report.issue("Version has no mainClass".into());
    }
    let context = RuleContext::current();
    let dependencies_before = context
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|context| dependencies::resolve(&root, id, &data, context));
    if let Err(error) = &dependencies_before {
        report.issue(error.clone());
    }
    if let Ok(dependencies) = &dependencies_before {
        report.missing_files = dependencies.missing.paths.clone();
        report.missing_total = dependencies.missing.total;
    }
    match select_java(&root, report.java_major, report.java_exact) {
        Ok(path) => report.java_path = Some(path.display().to_string()),
        Err(error) => report.java_error = Some(bounded_error(error)),
    }
    match metadata(&root, id, &mut HashSet::new()) {
        Ok(current) if current != data => {
            report.issue("实例版本元数据在检查期间变化，请重新检查".into())
        }
        Err(error) => report.issue(error),
        Ok(_) => {}
    }
    if let (Ok(context), Ok(before)) = (context, dependencies_before) {
        match dependencies::resolve(&root, id, &data, &context) {
            Ok(current) => {
                if before.asset_index != current.asset_index {
                    report.issue("资源索引在检查期间变化，请重新检查".into());
                }
                report.missing_files = current.missing.paths;
                report.missing_total = current.missing.total;
            }
            Err(error) => report.issue(error),
        }
    }
    // Keep the captured content directory meaningful even if a marker changes
    // during the bounded Java probe. The report never redirects game data.
    if let Ok(scope) = scope {
        match content_scope::inspect_with_isolation(&root, id, policy) {
            Ok(current) if current != scope => {
                report.issue("实例隔离目录在检查期间变化，请重新检查".into())
            }
            Err(error) => report.issue(error),
            Ok(_) => {}
        }
    }
    report.ready = report.issues.is_empty()
        && report.missing_total == 0
        && report.java_error.is_none()
        && report.java_path.is_some();
    Ok(report)
}

#[cfg(test)]
#[path = "inspection_tests.rs"]
mod tests;
