//! One authorized media response. HEAD shares native scope/file admission and
//! Range semantics with GET, while never seeking or reading the media body.
use super::filesystem::{check_file, Scope, Stamp};
use pcl_core::retained_fs::Identity;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

pub(super) const MAX_MEDIA_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const MAX_RANGE_BYTES: u64 = 1024 * 1024;
#[derive(Clone, Copy)]
pub(super) enum Method {
    Get,
    Head,
}
pub(super) struct Binding<'a> {
    pub name: &'a str,
    pub stamp: &'a Stamp,
    pub root: &'a Identity,
    pub owner: &'a Identity,
    pub folder: &'a Identity,
}
#[derive(Clone, Copy)]
pub(super) struct Span {
    pub start: u64,
    pub end: u64,
    pub total: u64,
    pub partial: bool,
}
impl Span {
    pub fn content_length(self) -> u64 {
        self.end - self.start + 1
    }
}
pub(super) struct Body {
    pub bytes: Vec<u8>,
    pub span: Span,
}
pub(super) struct Authorized<'a> {
    pub file: File,
    pub span: Span,
    scope: &'a Scope,
    binding: Binding<'a>,
}
pub(super) fn authorize<'a>(
    scope: &'a Scope,
    binding: Binding<'a>,
    range: Option<&str>,
) -> Result<Authorized<'a>, String> {
    let folder = scope.folder.as_ref().ok_or("媒体目录不存在")?;
    if scope.root.identity()? != *binding.root
        || scope.owner.as_ref().ok_or("媒体目录不存在")?.identity()? != *binding.owner
        || folder.identity()? != *binding.folder
    {
        return Err("媒体目录已变化，请刷新媒体列表".into());
    }
    let file = folder.file(binding.name)?.ok_or("媒体文件已消失")?;
    check_file(&file, binding.stamp, folder, binding.name)?;
    let (start, end, partial) = parse_range(range, binding.stamp.bytes)?;
    Ok(Authorized {
        file,
        span: Span {
            start,
            end,
            partial,
            total: binding.stamp.bytes,
        },
        scope,
        binding,
    })
}
impl Authorized<'_> {
    pub fn respond(&mut self, method: Method) -> Result<Body, String> {
        let bytes = read_body(&mut self.file, self.span, method)?;
        check_file(
            &self.file,
            self.binding.stamp,
            self.scope.folder.as_ref().ok_or("媒体目录不存在")?,
            self.binding.name,
        )?;
        self.scope.recheck()?;
        if matches!(method, Method::Get) && bytes.len() as u64 != self.span.content_length() {
            return Err("媒体内容已变化，请刷新列表".into());
        }
        Ok(Body {
            bytes,
            span: self.span,
        })
    }
}
pub(super) fn read_body(
    reader: &mut (impl Read + Seek),
    span: Span,
    method: Method,
) -> Result<Vec<u8>, String> {
    if matches!(method, Method::Head) {
        return Ok(Vec::new());
    }
    let length = span.content_length();
    reader
        .seek(SeekFrom::Start(span.start))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(length as usize);
    reader
        .take(length)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

pub(super) fn parse_range(range: Option<&str>, total: u64) -> Result<(u64, u64, bool), String> {
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
