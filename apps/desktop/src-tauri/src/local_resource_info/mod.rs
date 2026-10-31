//! Export selected resource metadata, not archive payloads or launcher/account
//! data. A prepared snapshot owns source FDs and exact fingerprints; after the
//! chooser, all content is rehashed outside admission. A final short recheck is
//! held through atomic publication of one bounded JSON document.
mod files;
#[cfg(test)]
mod tests;
use crate::resource_ops::ResourceFile;
use files::{Binding, Budget, Pinned, FILE_LIMIT};
use serde::Serialize;
use std::{
    collections::HashSet,
    io::{self, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    file_name: String,
    name: String,
    enabled: bool,
    version: Option<String>,
    description: Option<serde_json::Value>,
    size_bytes: u64,
    fingerprint: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    schema_version: u32,
    kind: String,
    files: Vec<Row>,
}
const JSON_LIMIT: usize = 4 * 1024 * 1024;
// Serde may escape one input byte into six output bytes. Bound the writer while
// encoding rather than building an oversized Vec and checking it afterwards.
struct JsonBuffer(Vec<u8>);
impl Write for JsonBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > JSON_LIMIT.saturating_sub(self.0.len()) {
            return Err(io::Error::other(
                "resource information exceeds output limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode(document: &Document) -> Result<Vec<u8>, String> {
    let mut output = JsonBuffer(Vec::new());
    serde_json::to_writer_pretty(&mut output, document)
        .map_err(|_| "资源信息 JSON 超过 4 MiB 或无法编码，请减少选择")?;
    Ok(output.0)
}
struct Source {
    name: String,
    pinned: Pinned,
}
pub struct Prepared {
    binding: Binding,
    sources: Vec<Source>,
    pub bytes: Vec<u8>,
    pub warning: Option<String>,
    pub count: usize,
}
pub fn supported(kind: &str, files: &[ResourceFile]) -> bool {
    matches!(kind, "mods" | "resourcepacks" | "shaderpacks")
        && !files.is_empty()
        && files.iter().all(|file| {
            if kind == "mods" {
                file.file_name.ends_with(".jar") || file.file_name.ends_with(".jar.disabled")
            } else {
                file.file_name.ends_with(".zip")
            }
        })
}
#[cfg(test)]
pub fn prepare(
    root: &Path,
    id: &str,
    kind: &str,
    selected: &[ResourceFile],
    closing: &AtomicBool,
) -> Result<Prepared, String> {
    prepare_with_isolation(
        root,
        id,
        kind,
        selected,
        closing,
        pcl_core::content_scope::IsolationPolicy::Automatic,
    )
}

pub fn prepare_with_isolation(
    root: &Path,
    id: &str,
    kind: &str,
    selected: &[ResourceFile],
    closing: &AtomicBool,
    policy: pcl_core::content_scope::IsolationPolicy,
) -> Result<Prepared, String> {
    if !supported(kind, selected) || selected.len() > 128 {
        return Err("请选择最多 128 个有指纹的模组、资源包或光影文件".into());
    }
    let mut budget = Budget::new();
    let binding = Binding::capture(root, id, kind, &mut budget, closing, policy)?;
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    let mut rows = Vec::new();
    let mut unavailable = 0;
    for choice in selected {
        budget.check(closing)?;
        if !seen.insert(choice.file_name.as_str())
            || choice.fingerprint.len() > crate::resource_ops::token::MAX_TOKEN_BYTES
        {
            return Err("资源选择重复或指纹无效，请刷新列表".into());
        }
        let mut pinned = Pinned::read(
            &binding.folder,
            &choice.file_name,
            FILE_LIMIT,
            &mut budget,
            closing,
        )?;
        if pinned.token != choice.fingerprint {
            return Err("资源文件已变化，请刷新列表后重新导出".into());
        }
        let metadata = match files::bounded_zip(&mut pinned.file) {
            Ok(()) => {
                let mut file = pinned.file.try_clone().map_err(|_| "无法读取资源元数据")?;
                file.seek(SeekFrom::Start(0))
                    .map_err(|_| "无法读取资源元数据")?;
                crate::ui_data::resource_metadata_from_file(file, &choice.file_name, kind).ok()
            }
            Err(_) => None,
        };
        budget.check(closing)?;
        pinned.check(&binding.folder, &choice.file_name)?;
        let name = metadata
            .as_ref()
            .and_then(|v| v["name"].as_str())
            .unwrap_or(&choice.file_name)
            .to_owned();
        let string = |field: &str| {
            metadata
                .as_ref()
                .and_then(|v| v[field].as_str())
                .map(str::to_owned)
        };
        if !metadata
            .as_ref()
            .is_some_and(|value| value["metadataAvailable"] == true)
        {
            unavailable += 1;
        }
        rows.push(Row {
            file_name: choice.file_name.clone(),
            name,
            enabled: !choice.file_name.ends_with(".disabled"),
            version: string("version"),
            description: metadata
                .as_ref()
                .and_then(|v| v.get("description"))
                .filter(|v| !v.is_null())
                .cloned(),
            size_bytes: pinned.stamp.bytes,
            fingerprint: choice.fingerprint.clone(),
        });
        sources.push(Source {
            name: choice.file_name.clone(),
            pinned,
        });
    }
    binding.check()?;
    budget.check(closing)?;
    let count = rows.len();
    let bytes = encode(&Document {
        schema_version: 1,
        kind: kind.into(),
        files: rows,
    })?;
    budget.check(closing)?;
    let prepared = Prepared {
        binding,
        sources,
        bytes,
        count,
        warning: (unavailable > 0)
            .then(|| format!("{unavailable} 个文件的压缩元数据不可用，已保留原文件名和文件状态")),
    };
    prepared.check()?;
    Ok(prepared)
}
impl Prepared {
    pub fn check(&self) -> Result<(), String> {
        self.binding.check()?;
        for source in &self.sources {
            source.pinned.check(&self.binding.folder, &source.name)?;
        }
        Ok(())
    }
    pub fn rehash(&self, closing: &AtomicBool) -> Result<(), String> {
        self.check()?;
        let mut budget = Budget::new();
        for source in &self.sources {
            let fresh = Pinned::read(
                &self.binding.folder,
                &source.name,
                FILE_LIMIT,
                &mut budget,
                closing,
            )?;
            if fresh.token != source.pinned.token || fresh.digest != source.pinned.digest {
                return Err("资源文件内容已变化，请刷新后重新导出".into());
            }
        }
        self.check()
    }
}
