//! Native-chosen skin snapshots and real PNG generation. A single decoded source
//! lives only in memory behind an opaque expiring ID; IPC cannot submit a source
//! pathname. Canvas exports are decoded and reencoded, stripping ancillary text
//! and profiles. The original skin is read-only and never becomes an export target.
mod pixels;
pub(crate) mod publish;
#[cfg(test)]
mod tests;
use crate::launcher_local::filesystem::{Dir, Snapshot};
use base64::Engine;
use pixels::Pixels;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};
const INPUT_LIMIT: usize = 8 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(600);
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkinSource {
    pub source_id: String,
    pub width: u32,
    pub height: u32,
    pub preview_png_base64: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedImage {
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
}
struct Selected {
    id: String,
    image: Pixels,
    at: Instant,
}
#[derive(Default)]
struct Session {
    generation: u64,
    selected: Option<Selected>,
}
#[derive(Default)]
pub struct ImageSession(Mutex<Session>);
/// Keeps the selected source identity fixed through the new-file publication.
/// Commands acquire application admission first, then this selection guard;
/// choosing a source never acquires application admission in the reverse order.
pub struct ImageGuard<'a> {
    _state: MutexGuard<'a, Session>,
}
impl ImageSession {
    pub fn begin_choice(&self) -> Result<u64, String> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or("皮肤选择已达上限，请重新打开启动器")?;
        Ok(state.generation)
    }
    /// The pathname is supplied only by a completed native PNG chooser. Read a
    /// no-follow FD snapshot before decoding; selection changes only after success.
    pub fn select(&self, path: &Path, generation: u64) -> Result<SkinSource, String> {
        if path
            .extension()
            .and_then(|v| v.to_str())
            .is_none_or(|v| !v.eq_ignore_ascii_case("png"))
        {
            return Err("请选择 PNG 皮肤文件".into());
        }
        let parent = path.parent().ok_or("皮肤文件目录无效")?;
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("皮肤文件名无效")?;
        let folder = Dir::absolute(parent)?;
        let mut snapshot = Snapshot::open(&folder, name, INPUT_LIMIT as u64)?;
        let bytes = snapshot.bytes(&folder, name)?;
        let image = pixels::decode(&bytes)?;
        pixels::skin_dimensions(&image)?;
        if Dir::absolute(parent)?.identity()? != folder.identity()? {
            return Err("皮肤目录已变化，请重新选择".into());
        }
        snapshot.recheck(&folder, name)?;
        let preview = pixels::encode(&pixels::head(&image, 64)?)?;
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return Err("皮肤选择已被较新的请求替代".into());
        }
        let id = format!(
            "skin-{:x}",
            Sha256::digest(format!(
                "{generation}:{}:{}",
                std::process::id(),
                snapshot.digest
            ))
        );
        let result = SkinSource {
            source_id: id.clone(),
            width: image.width,
            height: image.height,
            preview_png_base64: base64::engine::general_purpose::STANDARD.encode(preview),
        };
        state.selected = Some(Selected {
            id,
            image,
            at: Instant::now(),
        });
        Ok(result)
    }
    #[cfg(test)]
    pub fn recheck(&self, id: &str) -> Result<(), String> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        selected(&state, id).map(|_| ())
    }
    pub fn authorize_export(&self, id: &str) -> Result<ImageGuard<'_>, String> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        selected(&state, id)?;
        Ok(ImageGuard { _state: state })
    }
    pub fn avatar_png(&self, id: &str, size: u32) -> Result<Vec<u8>, String> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let source = selected(&state, id)?;
        pixels::encode(&pixels::head(&source.image, size)?)
    }
    pub fn render(&self, id: &str, size: u32) -> Result<RenderedImage, String> {
        Ok(RenderedImage {
            png_base64: base64::engine::general_purpose::STANDARD
                .encode(self.avatar_png(id, size)?),
            width: size,
            height: size,
        })
    }
}
fn selected<'a>(state: &'a Session, id: &str) -> Result<&'a Selected, String> {
    let source = state.selected.as_ref().ok_or("请先选择本地 PNG 皮肤")?;
    if id.len() != 69 || !id.starts_with("skin-") || id != source.id || source.at.elapsed() > TTL {
        return Err("皮肤选择已失效，请重新选择".into());
    }
    Ok(source)
}
pub fn sanitize_canvas_png(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > ((INPUT_LIMIT + 2) / 3) * 4 {
        return Err("生成图片超过 8 MiB 上限".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "生成图片不是有效的 PNG 编码")?;
    if bytes.len() > INPUT_LIMIT {
        return Err("生成图片超过 8 MiB 上限".into());
    }
    pixels::encode(&pixels::decode(&bytes)?)
}

/// Official account textures share the toolbox's bounded PNG decoder and head
/// compositing; renderer previews receive sanitized pixels, never remote URLs.
pub fn account_skin(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let image = pixels::decode(bytes)?;
    if image.width != 64 || !matches!(image.height, 32 | 64) {
        return Err("官方皮肤尺寸无效".into());
    }
    Ok((
        pixels::encode(&image)?,
        pixels::encode(&pixels::head(&image, 64)?)?,
    ))
}
pub fn account_cape(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = pixels::decode(bytes)?;
    if image.width != 64 || image.height != 32 {
        return Err("官方披风尺寸无效".into());
    }
    let mut rgba = Vec::with_capacity(10 * 16 * 4);
    for y in 1..17 {
        let start = (y * 64 + 1) * 4;
        rgba.extend_from_slice(&image.rgba[start..start + 10 * 4]);
    }
    pixels::encode(&Pixels {
        width: 10,
        height: 16,
        rgba,
    })
}
