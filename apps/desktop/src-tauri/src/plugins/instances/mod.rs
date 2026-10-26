//! Optional official instance-copy provider. Selection policy belongs here;
//! the host owns admission and the shared importer owns durable publication.
use crate::{
    instance_export::{ExportChecks, ExportRequest},
    instance_import::local_snapshot::LocalSnapshot,
    plugin_host::Registration,
    product_plugins::contracts::copy::*,
};
use pcl_core::content_scope::IsolationPolicy;
use std::{
    path::Path,
    sync::{atomic::AtomicBool, Arc},
};
pub const ID: &str = "launcher.instances.copy";
struct Copier;
struct Prepared {
    preview: CopyPreview,
    snapshot: LocalSnapshot,
}
impl InstanceCopier for Copier {
    fn prepare(
        &self,
        root: &Path,
        id: &str,
        request: CopyRequest,
        isolation: IsolationPolicy,
        settings_revision: &str,
    ) -> Result<Box<dyn PreparedCopy>, String> {
        let selection = crate::instance_export::prepare_with_isolation(
            root,
            id,
            ExportRequest {
                name: request.name.clone(),
                version: "copy-1".into(),
                checks: ExportChecks {
                    game: true,
                    game_settings: true,
                    game_personal: true,
                    mods: true,
                    pack_data: true,
                    mod_settings: true,
                    maps: true,
                    jei_personal: true,
                    guide_personal: true,
                    resourcepacks: true,
                    shaderpacks: true,
                    screenshots: request.screenshots,
                    saves: request.saves,
                    server: true,
                    other: false,
                    launcher: false,
                    bundle_assets: false,
                    modrinth: false,
                },
                excluded: Default::default(),
            },
            isolation,
            settings_revision,
        )?;
        let revision = selection.revision.clone();
        let snapshot = LocalSnapshot::prepare(selection, &request.name)?;
        let (file_count, bytes) = snapshot.counts();
        let preview = CopyPreview {
            revision,
            instance_id: id.into(),
            name: request.name.clone(),
            file_count,
            bytes,
        };
        Ok(Box::new(Prepared { preview, snapshot }))
    }
}
impl PreparedCopy for Prepared {
    fn preview(&self) -> &CopyPreview {
        &self.preview
    }
    fn execute(
        self: Box<Self>,
        cancel: &AtomicBool,
        progress: &(dyn Fn(pcl_install::Progress) + Send + Sync),
        commit: &dyn Fn() -> Result<(), String>,
    ) -> Result<serde_json::Value, String> {
        let notify = |mut p: pcl_install::Progress| {
            p.message = match p.stage.as_str() {
                "import-extract" => "正在复制并校验独立实例文件",
                "import-commit" => "正在发布实例副本",
                "import-cleanup" => "正在清理复制暂存文件",
                _ => "正在检查实例来源与目标目录",
            }
            .into();
            for step in &mut p.steps {
                step.label = match step.id.as_str() {
                    "import-extract" => "复制并校验文件",
                    "import-commit" => "发布实例副本",
                    "import-cleanup" => "清理暂存文件",
                    _ => "检查实例来源与目标目录",
                }
                .into();
            }
            progress(p);
        };
        self.snapshot
            .execute(cancel, &notify, commit)
            .map_err(|error| match error.as_str() {
                "实例导入已取消" | "导出已取消" => "实例复制已取消".into(),
                "导入包或目标游戏目录已经变化，请重新检查导入计划" => {
                    "实例来源或目标目录已经变化，请重新检查复制计划".into()
                }
                _ => error,
            })
    }
}
pub fn start(registration: &mut Registration<'_>) -> Result<(), String> {
    registration.provide(INSTANCE_COPY, CopyService(Arc::new(Copier)))
}

#[cfg(test)]
mod tests;
