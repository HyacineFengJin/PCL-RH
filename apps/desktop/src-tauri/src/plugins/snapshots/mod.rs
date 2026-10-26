//! Official backup/sharing policies and local backup restoration. Host commands
//! own admission; the common file transactions own integrity and recovery.
use crate::{
    instance_export::{self, ExportChecks, ExportRequest},
    instance_import,
    plugin_host::Registration,
    product_plugins::contracts::snapshots::*,
};
use pcl_core::content_scope::IsolationPolicy;
use std::{path::Path, sync::Arc};
pub const ID: &str = "launcher.instances.snapshots";
struct Snapshots;
impl SnapshotProvider for Snapshots {
    fn export(
        &self,
        root: &Path,
        id: &str,
        mode: SnapshotMode,
        version: &str,
        isolation: IsolationPolicy,
        settings_revision: &str,
    ) -> Result<instance_export::ExportPlan, String> {
        let backup = matches!(mode, SnapshotMode::Backup);
        instance_export::prepare_with_isolation(
            root,
            id,
            ExportRequest {
                name: id.into(),
                version: version.into(),
                excluded: Default::default(),
                checks: ExportChecks {
                    game: true,
                    game_settings: backup,
                    game_personal: backup,
                    mods: true,
                    pack_data: true,
                    mod_settings: true,
                    maps: backup,
                    jei_personal: backup,
                    guide_personal: backup,
                    resourcepacks: true,
                    shaderpacks: true,
                    screenshots: false,
                    saves: backup,
                    server: backup,
                    other: false,
                    launcher: false,
                    bundle_assets: false,
                    modrinth: false,
                },
            },
            isolation,
            settings_revision,
        )
    }
    fn restore(
        &self,
        root: &Path,
        source: &Path,
        name: &str,
    ) -> Result<instance_import::ImportPlan, String> {
        // Only our exported local backup format reaches this offline workflow.
        // Other pack formats retain their existing dedicated installer.
        if !instance_import::mrpack::is_local_export(source)? {
            return Err("请选择 PCL RH 导出的本地备份 ZIP".into());
        }
        let mut plan = instance_import::prepare(root, source, name)?;
        plan.warnings
            .retain(|warning| !warning.starts_with("此导入仅支持本启动器导出的本地 ZIP"));
        Ok(plan)
    }
}
pub fn start(registration: &mut Registration<'_>) -> Result<(), String> {
    registration.provide(SNAPSHOTS, SnapshotService(Arc::new(Snapshots)))
}

#[cfg(test)]
mod tests;
