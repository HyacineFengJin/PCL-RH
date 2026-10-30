//! Launcher-owned media and declarative homepages, independent of game roots.
//! Listing never creates folders. Native commands authorize source pickers and
//! preference updates; this module snapshots media, publishes owned title
//! copies, and serves only opaque IDs previously admitted by its catalog.
//!
//! No filesystem asset-protocol scope is required. The caller's custom protocol
//! serves `read_asset` bytes; external preference paths cannot authorize reads.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
};
#[path = "launcher_assets/filesystem.rs"]
mod filesystem;
#[path = "launcher_assets/fonts.rs"]
mod fonts;
#[path = "launcher_assets/homepage.rs"]
mod homepage;
pub use fonts::discover_font_families;
#[path = "launcher_assets/media.rs"]
mod media;
use filesystem::{check_file, component, revision, source, Scope, Snapshot, Stamp};
pub use homepage::*;

const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MEDIA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RANGE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetCollection {
    Backgrounds,
    Music,
    Titles,
}
impl AssetCollection {
    pub fn folder(self) -> &'static str {
        match self {
            Self::Backgrounds => "backgrounds",
            Self::Music => "music",
            Self::Titles => "titles",
        }
    }
    fn accepts(self, kind: AssetKind) -> bool {
        match self {
            Self::Backgrounds => kind != AssetKind::Audio,
            Self::Music => kind == AssetKind::Audio,
            Self::Titles => kind == AssetKind::Image,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetKind {
    Image,
    Video,
    Audio,
}
#[derive(Clone, Debug, Serialize)]
pub struct AssetRow {
    pub id: String,
    pub name: String,
    pub path: String,
    pub kind: AssetKind,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleImportPlan {
    pub revision: String,
    pub bytes: u64,
    pub suggested_name: String,
}

/// Exact bytes for a custom protocol response: 200 for an ordinary GET, 206 only
/// for a valid caller-supplied Range. Multipart ranges are intentionally refused.
pub struct AssetResponse {
    pub mime: String,
    pub bytes: Vec<u8>,
    pub start: u64,
    pub end: u64,
    pub total: u64,
    pub partial: bool,
}

#[derive(Clone)]
struct CachedAsset {
    row: AssetRow,
    stamp: Stamp,
    mime: String,
    root: pcl_core::retained_fs::Identity,
    owner: pcl_core::retained_fs::Identity,
    folder: pcl_core::retained_fs::Identity,
}

/// Owns the authorization map. Refresh replaces one collection atomically only
/// after the entire bounded scan succeeds; failed scans cannot admit new IDs.
pub struct AssetStore {
    project: PathBuf,
    catalog: Mutex<BTreeMap<(AssetCollection, String), CachedAsset>>,
}
impl AssetStore {
    pub fn new(project: PathBuf) -> Self {
        Self {
            project,
            catalog: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn list(&self, collection: AssetCollection) -> Result<Vec<AssetRow>, String> {
        let scope = Scope::open(&self.project, collection.folder())?;
        let mut entries = Vec::new();
        let mut total = 0u64;
        if let Some(folder) = &scope.folder {
            for name in folder.names()? {
                if !media::recognized(&name, collection) {
                    continue;
                }
                let snapshot = Snapshot::open(folder, &name, MAX_MEDIA_BYTES)?;
                total = total
                    .checked_add(snapshot.stamp.bytes)
                    .ok_or("媒体总大小过大")?;
                if total > MAX_TOTAL_BYTES {
                    return Err("媒体目录总大小超过 512 MiB，请减少文件后刷新".into());
                }
                entries.push(cached(&scope, &name, &snapshot, collection)?);
            }
        }
        scope.recheck()?;
        let rows = entries.iter().map(|entry| entry.row.clone()).collect();
        let mut catalog = self.catalog.lock().map_err(|_| "媒体目录状态暂时不可用")?;
        catalog.retain(|(kind, _), _| *kind != collection);
        for entry in entries {
            catalog.insert((collection, entry.row.id.clone()), entry);
        }
        Ok(rows)
    }
    pub fn ensure_folder(&self, collection: AssetCollection) -> Result<PathBuf, String> {
        let scope = Scope::create(&self.project, collection.folder())?;
        Ok(scope.path())
    }
    pub fn read_asset(
        &self,
        collection: AssetCollection,
        id: &str,
        range: Option<&str>,
    ) -> Result<AssetResponse, String> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("无效媒体标识".into());
        }
        let cached = self
            .catalog
            .lock()
            .map_err(|_| "媒体目录状态暂时不可用")?
            .get(&(collection, id.to_owned()))
            .cloned()
            .ok_or("媒体标识未登记或已过期，请刷新媒体列表")?;
        let scope = Scope::open(&self.project, collection.folder())?;
        let folder = scope.folder.as_ref().ok_or("媒体目录不存在")?;
        if scope.root.identity()? != cached.root
            || scope.owner.as_ref().ok_or("媒体目录不存在")?.identity()? != cached.owner
            || folder.identity()? != cached.folder
        {
            return Err("媒体目录已变化，请刷新媒体列表".into());
        }
        let mut file = folder.file(&cached.row.name)?.ok_or("媒体文件已消失")?;
        check_file(&file, &cached.stamp, folder, &cached.row.name)?;
        let (start, end, partial) = parse_range(range, cached.stamp.bytes)?;
        let length = end - start + 1;
        file.seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(length as usize);
        (&mut file)
            .take(length)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        check_file(&file, &cached.stamp, folder, &cached.row.name)?;
        scope.recheck()?;
        if bytes.len() as u64 != length {
            return Err("媒体内容已变化，请刷新列表".into());
        }
        Ok(AssetResponse {
            mime: cached.mime,
            bytes,
            start,
            end,
            total: cached.stamp.bytes,
            partial,
        })
    }
    pub fn read_title(&self, owned_path: &Path) -> Result<AssetRow, String> {
        let expected = self.project.join(".pcl-rust/titles");
        if owned_path.parent() != Some(expected.as_path()) {
            return Err("标题图片必须来自启动器的标题图片目录，请重新导入".into());
        }
        let name = owned_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("标题图片路径无效")?;
        component(name)?;
        self.list(AssetCollection::Titles)?
            .into_iter()
            .find(|row| row.name == name)
            .ok_or("已保存的标题图片不存在或格式不受支持".into())
    }
    pub fn import_title(&self, path: &Path, expected_revision: &str) -> Result<AssetRow, String> {
        let (parent, name, mut snapshot) = source(path, MAX_IMAGE_BYTES)?;
        media::classify(
            &name,
            &snapshot.header,
            snapshot.stamp.bytes,
            AssetCollection::Titles,
        )?;
        if revision(path, &snapshot)? != expected_revision {
            return Err("标题图片已变化，请重新选择并确认".into());
        }
        let bytes = snapshot.bytes(&parent, &name)?;
        // The preference owns the resulting path, never the external picker
        // source. Failed later preference updates retain this harmless copy.
        let extension = name
            .rsplit_once('.')
            .ok_or("图片扩展名无效")?
            .1
            .to_ascii_lowercase();
        let owned_name = format!("title-{}.{}", snapshot.digest, extension);
        let scope = Scope::create(&self.project, AssetCollection::Titles.folder())?;
        let folder = scope.folder.as_ref().unwrap();
        if let Some(existing) = folder.file(&owned_name)? {
            drop(existing);
            let existing = Snapshot::open(folder, &owned_name, MAX_IMAGE_BYTES)?;
            if existing.digest != snapshot.digest {
                return Err("已有标题图片与导入内容不一致，已保留原文件".into());
            }
        } else {
            if folder.names()?.len() >= filesystem::MAX_ENTRIES {
                return Err("标题图片目录已达到 256 个文件上限，请整理后再导入".into());
            }
            snapshot.recheck(&parent, &name)?;
            folder.write_new(&owned_name, &bytes)?;
        }
        scope.recheck()?;
        self.read_title(&scope.path().join(owned_name))
    }
}

pub fn prepare_title_import(path: &Path) -> Result<TitleImportPlan, String> {
    let (_, name, snapshot) = source(path, MAX_IMAGE_BYTES)?;
    media::classify(
        &name,
        &snapshot.header,
        snapshot.stamp.bytes,
        AssetCollection::Titles,
    )?;
    Ok(TitleImportPlan {
        revision: revision(path, &snapshot)?,
        bytes: snapshot.stamp.bytes,
        suggested_name: name,
    })
}

fn cached(
    scope: &Scope,
    name: &str,
    snapshot: &Snapshot,
    collection: AssetCollection,
) -> Result<CachedAsset, String> {
    let (kind, mime) = media::classify(name, &snapshot.header, snapshot.stamp.bytes, collection)?;
    if kind == AssetKind::Image && snapshot.stamp.bytes > MAX_IMAGE_BYTES {
        return Err("图片超过 16 MiB 大小上限".into());
    }
    let id = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(collection, name, &snapshot.stamp, &snapshot.digest))
                .map_err(|e| e.to_string())?
        )
    );
    Ok(CachedAsset {
        row: AssetRow {
            id,
            name: name.to_owned(),
            path: scope.path().join(name).display().to_string(),
            kind,
        },
        stamp: snapshot.stamp.clone(),
        mime: mime.into(),
        root: scope.root.identity()?,
        owner: scope.owner.as_ref().ok_or("媒体目录不存在")?.identity()?,
        folder: scope.folder.as_ref().ok_or("媒体目录不存在")?.identity()?,
    })
}

fn parse_range(range: Option<&str>, total: u64) -> Result<(u64, u64, bool), String> {
    if total == 0 || total > MAX_MEDIA_BYTES {
        return Err("媒体大小超出安全上限".into());
    }
    let Some(range) = range else {
        return Ok((0, total - 1, false));
    };
    if range.len() > 128 {
        return Err("媒体 Range 请求过长".into());
    }
    let value = range
        .strip_prefix("bytes=")
        .ok_or("媒体只支持 bytes 单段 Range")?;
    let (first, last) = value.split_once('-').ok_or("媒体 Range 无效")?;
    if value.contains(',')
        || !first.bytes().all(|b| b.is_ascii_digit())
        || !last.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("媒体只支持一个有效的 bytes Range".into());
    }
    let (start, end) = if first.is_empty() {
        let suffix = last.parse::<u64>().map_err(|_| "媒体 Range 无效")?;
        if suffix == 0 {
            return Err("媒体 Range 超出文件范围".into());
        }
        (total.saturating_sub(suffix), total - 1)
    } else {
        let start = first.parse::<u64>().map_err(|_| "媒体 Range 无效")?;
        let end = if last.is_empty() {
            total - 1
        } else {
            last.parse::<u64>()
                .map_err(|_| "媒体 Range 无效")?
                .min(total - 1)
        };
        (start, end)
    };
    if start >= total || end < start {
        return Err(format!("媒体 Range 超出文件范围（总大小 {total}）"));
    }
    // RFC 9110 §15.3.7 permits serving a subset of the requested data. A legal
    // large/open-ended range is served in a smaller valid subrange.
    // The reported Content-Range describes exactly these bytes, never a 200 GET.
    Ok((start, end.min(start + MAX_RANGE_BYTES - 1), true))
}

#[cfg(test)]
#[path = "launcher_assets/tests.rs"]
mod tests;
