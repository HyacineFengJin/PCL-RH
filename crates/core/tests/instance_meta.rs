//! Actual MetadataStore business logic and native document port, without Tauri
//! or a user project. The reference-service boundary below rejects every pending
//! synthetic marker; reference replay/parsing is covered by its own provider.
#![allow(dead_code)]
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
#[cfg(not(target_os = "linux"))]
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
mod instance_rename_refs {
    pub(crate) fn ensure_project_ready(project: &std::path::Path) -> Result<(), String> {
        use pcl_core::retained_fs::{Directory, Leaf, Traversal};
        let project = Directory::open_absolute(project).map_err(|e| e.to_string())?;
        let storage = match project.child(&Leaf::new(".pcl-rust").unwrap(), Traversal::AllowMounts)
        {
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
                return Err("合成reference admission拒绝待恢复标记".into());
            }
        }
        Ok(())
    }
}
#[path = "../../../apps/desktop/src-tauri/src/instance_meta.rs"]
mod instance_meta;
