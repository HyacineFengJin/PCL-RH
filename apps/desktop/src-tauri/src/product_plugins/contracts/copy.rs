use crate::plugin_host::Contract;
use pcl_core::content_scope::IsolationPolicy;
use pcl_install::Progress;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::Path,
    sync::{atomic::AtomicBool, Arc},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyRequest {
    pub name: String,
    pub saves: bool,
    pub screenshots: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct CopyPreview {
    pub revision: String,
    pub instance_id: String,
    pub name: String,
    pub file_count: usize,
    pub bytes: u64,
}
/// An in-process capability. Frontend JSON cannot construct a prepared copy.
pub trait PreparedCopy: Send {
    fn preview(&self) -> &CopyPreview;
    fn execute(
        self: Box<Self>,
        cancel: &AtomicBool,
        progress: &(dyn Fn(Progress) + Send + Sync),
        commit: &dyn Fn() -> Result<(), String>,
    ) -> Result<Value, String>;
}
pub trait InstanceCopier: Send + Sync {
    fn prepare(
        &self,
        root: &Path,
        id: &str,
        request: CopyRequest,
        isolation: IsolationPolicy,
        settings_revision: &str,
    ) -> Result<Box<dyn PreparedCopy>, String>;
}
pub struct CopyService(pub Arc<dyn InstanceCopier>);
pub const INSTANCE_COPY: Contract<CopyService> = Contract::new("launcher.instance-copy", 1);
