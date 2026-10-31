//! Compile-time identity for the existing clean source scope. Only tracked
//! leaves are watched: runtime node_modules and generated trees stay excluded.
use std::{
    path::{Component, Path},
    process::Command,
};

const SOURCES: &[&str] = &[
    "apps/desktop",
    "crates",
    "Cargo.toml",
    "Cargo.lock",
    "assets",
    "code",
    "experimental",
    "LICENSE",
    "NOTICE",
    "COPYRIGHT.md",
    "THIRD_PARTY_NOTICES.md",
    "THIRDPARTY.generated.json",
    "LICENSES",
    "tools/licenses",
];

fn git(project: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    Command::new("git")
        .current_dir(project)
        // status still checks actual worktree/index state, but must not refresh
        // the watched index as an optional cache write during this build.
        .arg("--no-optional-locks")
        .args(arguments)
        .output()
        .ok()
        .filter(|result| result.status.success())
        .map(|result| result.stdout)
}

fn watch(project: &Path, path: &str, git_metadata: bool) -> Option<()> {
    if path.is_empty() || path.contains(['\n', '\r', '\0']) {
        return None;
    }
    let path = project.join(path);
    // A tracked submodule/directory must not accidentally expand this into a
    // recursive watch of its ignored dependencies. Missing tracked files stay
    // watched so deletion/restoration also recomputes the clean guard.
    if !git_metadata && path.is_dir() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", path.display());
    Some(())
}

fn git_path(project: &Path, name: &str) -> Option<()> {
    let path = git(project, &["rev-parse", "--git-path", name])?;
    let path = std::str::from_utf8(&path)
        .ok()?
        .trim_end_matches(['\n', '\r']);
    // Missing optional metadata must not force every unchanged Cargo build to
    // rerun. The real refs directory also observes loose-ref creation/removal;
    // packing the current loose ref changes that watched tree.
    if (name == "packed-refs" || name.starts_with("refs/")) && !project.join(path).exists() {
        return Some(());
    }
    watch(project, path, true)
}

pub fn identity(project: &Path) -> Option<String> {
    let top = git(project, &["rev-parse", "--show-toplevel"])?;
    let top = std::str::from_utf8(&top)
        .ok()?
        .trim_end_matches(['\n', '\r']);
    if Path::new(top).canonicalize().ok()? != project.canonicalize().ok()? {
        // A Git-less source archive inside another checkout is not that outer
        // repository's clean build and must never borrow its HEAD identity.
        return None;
    }
    let mut arguments = vec!["ls-files", "-z", "--"];
    arguments.extend_from_slice(SOURCES);
    let tracked = git(project, &arguments)?;
    for path in tracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).ok()?;
        if Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return None;
        }
        watch(project, path, false)?;
    }
    // Resolve Git's actual paths instead of assuming the active branch is
    // master or that .git is a directory. Detached HEAD still has its own file.
    for name in ["index", "HEAD", "packed-refs", "refs"] {
        git_path(project, name)?;
    }
    if let Some(reference) = git(project, &["symbolic-ref", "--quiet", "HEAD"]) {
        let reference = std::str::from_utf8(&reference)
            .ok()?
            .trim_end_matches(['\n', '\r']);
        git_path(project, reference)?;
    }
    // Preserve the pre-existing cleanliness/unknown identity contract. A
    // source archive without Git builds normally but cannot name a commit.
    let mut arguments = vec!["status", "--porcelain", "--untracked-files=normal", "--"];
    arguments.extend_from_slice(SOURCES);
    if !git(project, &arguments)?.is_empty() {
        return None;
    }
    String::from_utf8(git(project, &["rev-parse", "HEAD"])?)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
