//! Compile and run the actual desktop export service on every native runner.
//! Fixtures are private TempDirs; no chooser, Tauri, account or real game runs.
#![allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/instance_export.rs"]
mod instance_export;

// Only the reference-service admission boundary is synthetic here. It refuses
// pending markers; this target does not claim to test reference replay. The
// export selector, transaction, presets and document backend are production.
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
mod instance_rename_refs {
    pub(crate) fn ensure_project_ready(project: &std::path::Path) -> Result<(), String> {
        use pcl_core::retained_fs::{Directory, Leaf, Traversal};
        let root = Directory::open_absolute(project).map_err(|e| e.to_string())?;
        let storage = match root.child(&Leaf::new(".pcl-rust").unwrap(), Traversal::AllowMounts) {
            Ok(storage) => storage,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        for name in [
            "instance-rename-pending.json",
            "instance-delete-pending.json",
        ] {
            if storage
                .entry(&Leaf::new(name).unwrap())
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("synthetic reference admission: pending recovery".into());
            }
        }
        Ok(())
    }
}
#[path = "../../../apps/desktop/src-tauri/src/export_presets.rs"]
mod export_presets;
