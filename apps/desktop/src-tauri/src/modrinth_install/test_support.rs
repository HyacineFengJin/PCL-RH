//! Shared real native fixtures; no project checkout paths or mirrored service.
use super::*;
use pcl_core::retained_fs::{Directory, Identity, Leaf, Traversal};
use std::{fs, path::PathBuf};
pub(super) fn tree() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().canonicalize().unwrap();
    let directory = Directory::open_absolute(&path).unwrap();
    for name in ["game", "project"] {
        directory
            .create_private(&Leaf::new(name).unwrap(), Traversal::SameFilesystem)
            .unwrap();
    }
    let root = path.join("game");
    let project = path.join("project");
    fs::create_dir_all(root.join("versions/sample/mods")).unwrap();
    Directory::open_absolute(&project)
        .unwrap()
        .create_private(&Leaf::new(".pcl-rust").unwrap(), Traversal::SameFilesystem)
        .unwrap();
    (temporary, path, root, project)
}
pub(super) fn staging(project: &Path) -> target::Dir {
    target::Dir::open(project)
        .unwrap()
        .ensure(".pcl-rust")
        .unwrap()
        .ensure("resource-downloads")
        .unwrap()
        .with_input_sources()
        .unwrap()
}
pub(super) fn assert_source(file: &source::Input) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(file.metadata().unwrap().nlink(), 0);
    }
    #[cfg(not(target_os = "linux"))]
    file.verify_owned().unwrap();
}
pub(super) fn assert_staging(directory: &target::Dir, pending: usize) {
    #[cfg(target_os = "linux")]
    {
        let _ = pending;
        assert!(directory.names().unwrap().is_empty());
    }
    #[cfg(not(target_os = "linux"))]
    {
        directory.check_cleanup().unwrap();
        let area = source::open_area(&directory.0, "registered-sources", MAX_BATCH_BYTES).unwrap();
        assert_eq!(area.pending().unwrap(), pending);
        assert_eq!(directory.names().unwrap(), ["registered-sources"]);
    }
}
pub(super) fn assert_staging_path(project: &Path, pending: usize) {
    assert_staging(&staging(project), pending)
}
pub(super) fn identity(path: impl AsRef<Path>) -> Identity {
    pcl_core::retained_fs::metadata(&File::open(path).unwrap())
        .unwrap()
        .identity
}
#[cfg(unix)]
pub(super) use std::os::unix::fs::symlink as symlink_file;
#[cfg(unix)]
pub(super) use std::os::unix::fs::symlink as symlink_dir;
#[cfg(windows)]
pub(super) use std::os::windows::fs::{symlink_dir, symlink_file};
