//! Asset-index IDs such as `5` are reused for different Mojang index revisions.
//! Give the private instance a content-addressed index before sealing it. This
//! leaves existing games' indexes untouched and keeps publication's normal
//! no-overwrite/content checks in charge of every destination.
use super::*;
use sha1::{Digest, Sha1};
use std::io::{Read, Write};

pub(super) fn namespace_index(
    operation: &BuildOperation,
    build: &Dir,
    cancel: &AtomicBool,
) -> Result<()> {
    let metadata_path = format!("versions/{0}/{0}.json", operation.journal.name);
    let before = hash_file(build.file(&metadata_path)?, MAX_JSON, Some(cancel))?;
    let mut bytes = Vec::new();
    build
        .file(&metadata_path)?
        .take(MAX_JSON + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > MAX_JSON {
        return Err("构建后的版本 JSON 超过大小限制".into());
    }
    let mut metadata: Value = serde_json::from_slice(&bytes).map_err(error)?;
    let Some(id) = metadata["assetIndex"]["id"]
        .as_str()
        .or_else(|| metadata["assets"].as_str())
        .map(str::to_owned)
    else {
        return Ok(());
    };
    component(&id)?;
    let indexes = build.at("assets/indexes")?;
    let original = format!("{id}.json");
    let source = hash_file(
        indexes.regular(&original)?,
        pcl_core::repair::MAX_ASSET_INDEX_BYTES,
        Some(cancel),
    )?;
    let mut index = Vec::new();
    indexes
        .regular(&original)?
        .take(pcl_core::repair::MAX_ASSET_INDEX_BYTES + 1)
        .read_to_end(&mut index)
        .map_err(error)?;
    if index.len() as u64 > pcl_core::repair::MAX_ASSET_INDEX_BYTES {
        return Err("构建后的资源索引超过大小限制".into());
    }
    if source.hash != format!("{:x}", sha2::Sha256::digest(&index))
        || source.stamp.size != index.len() as u64
    {
        return Err("构建后的资源索引在读取期间已变化".into());
    }
    let hash = format!("{:x}", Sha1::digest(&index));
    if metadata["assetIndex"]["sha1"]
        .as_str()
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(&hash))
        || metadata["assetIndex"]["size"]
            .as_u64()
            .is_some_and(|expected| expected != index.len() as u64)
    {
        return Err("构建后的资源索引与版本校验信息不符".into());
    }
    let alias = format!("pcl-{hash}");
    if alias == id {
        return Ok(());
    }
    let renamed = format!("{alias}.json");
    check(cancel)?;
    operation.verify_binding()?;
    if indexes.stat(&renamed)?.is_some() {
        let existing = hash_file(
            indexes.regular(&renamed)?,
            pcl_core::repair::MAX_ASSET_INDEX_BYTES,
            Some(cancel),
        )?;
        if existing.hash != source.hash || existing.stamp.size != source.stamp.size {
            return Err("构建中的资源索引命名发生冲突，拒绝发布".into());
        }
    } else {
        let mut output = indexes.stage_for(&renamed, pcl_core::repair::MAX_ASSET_INDEX_BYTES)?;
        output.write_all(&index).map_err(error)?;
        output.sync_all().map_err(error)?;
        link_anonymous(&mut output, &indexes, &renamed)?;
    }
    if hash_file(
        indexes.regular(&original)?,
        pcl_core::repair::MAX_ASSET_INDEX_BYTES,
        Some(cancel),
    )? != source
    {
        return Err("资源索引重命名期间原索引已变化".into());
    }
    indexes.unlink_snapshot(&original, &source)?;
    indexes.sync()?;
    if metadata["assetIndex"].is_object() {
        metadata["assetIndex"]["id"] = Value::String(alias.clone());
    }
    if metadata["assets"].as_str() == Some(id.as_str()) {
        metadata["assets"] = Value::String(alias);
    }
    let (parent, leaf) = build.parent(&metadata_path)?;
    let mut output = parent.stage_for(&leaf, MAX_JSON)?;
    serde_json::to_writer(&mut output, &metadata).map_err(error)?;
    output.flush().map_err(error)?;
    output.sync_all().map_err(error)?;
    let current = hash_file(parent.regular(&leaf)?, MAX_JSON, Some(cancel))?;
    if current != before {
        return Err("资源索引重命名期间版本 JSON 已变化".into());
    }
    check(cancel)?;
    operation.verify_binding()?;
    // Only the durably owned, unsealed build is edited. A failure between these
    // two operations leaves a recoverable build, never a partial real instance.
    parent.unlink_snapshot(&leaf, &before)?;
    link_anonymous(&mut output, &parent, &leaf)?;
    parent.sync()
}
