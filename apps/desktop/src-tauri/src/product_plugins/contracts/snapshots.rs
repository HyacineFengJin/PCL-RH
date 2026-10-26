//! Snapshot policy contracts over the common checked export/import formats.
//! Plans are native values; their JSON projections never grant file authority.
use crate::{instance_export::ExportPlan, instance_import::ImportPlan, plugin_host::Contract};
use pcl_core::content_scope::IsolationPolicy;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotMode {
    Backup,
    Share,
}
pub trait SnapshotProvider: Send + Sync {
    fn export(
        &self,
        root: &Path,
        id: &str,
        mode: SnapshotMode,
        version: &str,
        isolation: IsolationPolicy,
        settings_revision: &str,
    ) -> Result<ExportPlan, String>;
    fn restore(&self, root: &Path, source: &Path, name: &str) -> Result<ImportPlan, String>;
}
pub struct SnapshotService(pub Arc<dyn SnapshotProvider>);
pub const SNAPSHOTS: Contract<SnapshotService> = Contract::new("launcher.instance-snapshots", 1);
