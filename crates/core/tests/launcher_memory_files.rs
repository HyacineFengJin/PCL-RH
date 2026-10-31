//! Actual bounded read-only mod-demand backend, without Tauri or system RAM IO.
#![allow(dead_code)]
const MAX_MOD_ENTRIES: usize = 8192;
const CHANGED: &str = "内存预览期间设置或游戏目录已变化，请刷新后重试";
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
#[cfg(target_os = "linux")]
#[path = "../../../apps/desktop/src-tauri/src/launcher_memory/filesystem_linux.rs"]
mod filesystem;
#[cfg(not(target_os = "linux"))]
#[path = "../../../apps/desktop/src-tauri/src/launcher_memory/filesystem_native.rs"]
mod filesystem;
#[cfg(windows)]
#[path = "../src/file_ops/junction_fixture.rs"]
mod junction_fixture;
use filesystem::enabled_mods;
use pcl_core::content_scope::IsolationPolicy;
use std::{
    fs,
    path::{Path, PathBuf},
};
struct Fixture {
    root: PathBuf,
    _temporary: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap().join("game");
        fs::create_dir_all(root.join("versions/Example")).unwrap();
        fs::create_dir(root.join("mods")).unwrap();
        for name in [
            "enabled.jar",
            "UPPER.JAR",
            "disabled.jar.disabled",
            "other.zip",
        ] {
            fs::write(root.join("mods").join(name), b"never opened").unwrap();
        }
        fs::create_dir(root.join("mods/directory.jar")).unwrap();
        Self {
            root,
            _temporary: temporary,
        }
    }
}
fn alias(source: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, target).unwrap();
    #[cfg(windows)]
    {
        fs::create_dir(target).unwrap();
        junction_fixture::set_existing(source, target).unwrap();
    }
}
#[test]
fn enabled_counts_real_literal_jars_and_explicit_isolation_preserves_selected_base() {
    let f = Fixture::new();
    assert_eq!(
        enabled_mods(&f.root, "Example", IsolationPolicy::Automatic).unwrap(),
        2
    );
    let isolated = f.root.join("versions/Example/mods");
    fs::create_dir(&isolated).unwrap();
    fs::write(isolated.join("own.jar"), b"not opened").unwrap();
    assert_eq!(
        enabled_mods(&f.root, "Example", IsolationPolicy::Automatic).unwrap(),
        1
    );
    assert_eq!(
        enabled_mods(&f.root, "Example", IsolationPolicy::Shared).unwrap(),
        2
    );
    assert_eq!(
        enabled_mods(&f.root, "Example", IsolationPolicy::Isolated).unwrap(),
        1
    );
}
#[test]
fn registered_root_alias_is_resolved_but_unsafe_child_alias_is_never_counted() {
    let f = Fixture::new();
    let link = f.root.with_file_name("alias");
    alias(&f.root, &link);
    assert_eq!(
        enabled_mods(&link, "Example", IsolationPolicy::Automatic).unwrap(),
        2
    );
    let external = f.root.with_file_name("outside");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("keep.jar"), b"unchanged").unwrap();
    alias(&external, &f.root.join("versions/Example/config"));
    assert!(enabled_mods(&link, "Example", IsolationPolicy::Shared).is_err());
    assert_eq!(fs::read(external.join("keep.jar")).unwrap(), b"unchanged");
}
