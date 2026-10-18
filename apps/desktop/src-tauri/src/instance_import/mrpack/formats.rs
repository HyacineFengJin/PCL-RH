//! Native adapters for bounded, local ZIP pack layouts.
//!
//! Classification accepts a root or one wrapper and rejects competing packs.
//! Every entry is checked and streamed through CRC/SHA256/SHA1, including
//! ignored launcher files. Only a recognized content layer becomes an output.
//! CurseForge IDs are declarations, not download authority: unresolved files
//! remain blockers until an authorized provider can supply exact descriptors.
use super::{archive::OverrideFile, manifest::Client, Output, PreviewFile, Result};
use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize,
};
use serde_json::Value;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
use zip::CompressionMethod;

const ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub(super) const DECODED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const JSON_BYTES: u64 = 4 * 1024 * 1024;
const CAPTURE_BYTES: u64 = 32 * 1024 * 1024;
const NODE_COUNT: usize = 100_000;
const PATH_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format {
    Pcl,
    Modrinth,
    CurseForge,
    Mcbbs,
    Hmcl,
    Multimc,
    ReadyGame,
    Launcher,
}
impl Format {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pcl => "pcl-local-instance",
            Self::Modrinth => "modrinth",
            Self::CurseForge => "curseforge",
            Self::Mcbbs => "mcbbs",
            Self::Hmcl => "hmcl",
            Self::Multimc => "multimc",
            Self::ReadyGame => "ready_game",
            Self::Launcher => "launcher_archive",
        }
    }
}
pub(super) struct BundledCore {
    pub metadata: Vec<u8>,
    pub jar: OverrideFile,
    pub shared: BTreeMap<String, OverrideFile>,
}
pub(super) struct PreparedArchive {
    pub pack_name: String,
    pub version: String,
    pub minecraft: String,
    pub dependencies: BTreeMap<String, String>,
    pub outputs: BTreeMap<String, Output>,
    pub directories: BTreeSet<String>,
    pub warnings: Vec<String>,
    pub blockers: Vec<String>,
    pub summary: Option<String>,
    pub preview_files: Vec<PreviewFile>,
    pub optional: BTreeSet<String>,
    pub bundled: Option<BundledCore>,
    pub shadowed_files: usize,
    /// HMCL runtime declarations need proof against the captured provider
    /// profile before a rebuild can receive a confirmation token.
    pub rebuild_profile: Option<Vec<u8>>,
    pub curseforge_files: Vec<(u32, u32)>,
}
struct Node {
    directory: bool,
    size: u64,
    hash: String,
    sha1: String,
    bytes: Option<Vec<u8>>,
}
struct Layout {
    format: Format,
    prefix: String,
}
struct Scan {
    nodes: BTreeMap<String, Node>,
    layout: Layout,
    decoded_bytes: u64,
}

pub(super) struct NestedEntry {
    pub archive_path: String,
    pub size: u64,
}

#[cfg(test)]
pub(super) fn classify(source: &Path) -> Result<Format> {
    classify_cancellable(source, &AtomicBool::new(false))
}
pub(super) fn classify_cancellable(source: &Path, cancel: &AtomicBool) -> Result<Format> {
    let file = super::super::open_source(source)?;
    let before = super::Stamp::of(&file.metadata().map_err(super::super::error)?);
    if !before.regular() || before.size > super::super::MAX_BYTES {
        return Err("导入文件类型无效或超过大小限制".into());
    }
    let format = classify_held(&file, cancel)?;
    if super::Stamp::of(&file.metadata().map_err(super::super::error)?) != before
        || super::Stamp::of(
            &super::super::open_source(source)?
                .metadata()
                .map_err(super::super::error)?,
        ) != before
    {
        return Err(super::super::changed());
    }
    Ok(format)
}
pub(super) fn classify_held(file: &File, cancel: &AtomicBool) -> Result<Format> {
    Ok(classification(file, cancel)?.format)
}
pub(super) fn nested_entry(file: &File, cancel: &AtomicBool) -> Result<NestedEntry> {
    let layout = classification(file, cancel)?;
    if layout.format != Format::Launcher {
        return Err("ZIP 不包含唯一内嵌整合包".into());
    }
    let mut zip =
        super::super::archive::checked_zip(file.try_clone().map_err(super::super::error)?)?;
    for name in ["modpack.zip", "modpack.mrpack"] {
        let path = format!("{}{name}", layout.prefix);
        if let Ok(entry) = zip.by_name(&path) {
            if entry.size() > FILE_BYTES {
                return Err("内嵌整合包压缩文件超过 2 GiB 限制".into());
            }
            return Ok(NestedEntry {
                archive_path: path,
                size: entry.size(),
            });
        }
    }
    Err("内嵌整合包条目在识别后发生变化".into())
}
fn classification(file: &File, cancel: &AtomicBool) -> Result<Layout> {
    super::super::check(cancel)?;
    let before = super::Stamp::of(&file.metadata().map_err(super::super::error)?);
    let mut zip =
        super::super::archive::checked_zip(file.try_clone().map_err(super::super::error)?)?;
    let mut nodes = BTreeMap::new();
    let mut captured = 0u64;
    let mut paths = 0usize;
    for i in 0..zip.len() {
        super::super::check(cancel)?;
        let entry = zip.by_index(i).map_err(|_| "ZIP 格式识别目录无效")?;
        let path = std::str::from_utf8(entry.name_raw())
            .map_err(|_| "ZIP 路径必须是 UTF-8")?
            .trim_end_matches('/')
            .to_owned();
        paths = paths
            .checked_add(path.len())
            .ok_or("ZIP 路径文本长度溢出")?;
        let directory = entry.is_dir();
        nodes.insert(
            path,
            Node {
                directory,
                size: entry.size(),
                hash: String::new(),
                sha1: String::new(),
                bytes: None,
            },
        );
    }
    // A launcher candidate must be selected/charged before any body is read.
    // Its competing marker declarations can be rejected from the directory.
    if nested_candidates(&nodes, cancel)?.is_empty() {
        for i in 0..zip.len() {
            super::super::check(cancel)?;
            let mut entry = zip.by_index(i).map_err(|_| "ZIP 格式识别目录无效")?;
            let path = std::str::from_utf8(entry.name_raw())
                .map_err(|_| "ZIP 路径必须是 UTF-8")?
                .trim_end_matches('/')
                .to_owned();
            let parts = path.split('/').collect::<Vec<_>>();
            let marker = !entry.is_dir()
                && parts.len() <= 2
                && parts.last() == Some(&"manifest.json")
                && !(parts.len() == 2 && game_root(parts[0]));
            let bytes = if marker {
                let size = entry.size();
                if size > JSON_BYTES {
                    return Err("ZIP 格式识别清单超过 4 MiB 限制".into());
                }
                captured = captured
                    .checked_add(size)
                    .filter(|bytes| *bytes <= CAPTURE_BYTES)
                    .ok_or("ZIP 格式识别清单总量超过 32 MiB 限制")?;
                Some(read_metadata(&mut entry, size, cancel)?)
            } else {
                None
            };
            nodes
                .get_mut(&path)
                .ok_or("ZIP 目录在识别中发生变化")?
                .bytes = bytes;
        }
    }
    let layout = layout(&nodes, cancel)?;
    if layout.format != Format::Pcl && paths > PATH_BYTES {
        return Err("ZIP 路径文本超过 8 MiB 限制".into());
    }
    if super::Stamp::of(&file.metadata().map_err(super::super::error)?) != before {
        return Err(super::super::changed());
    }
    super::super::check(cancel)?;
    Ok(layout)
}
#[cfg(test)]
pub(super) fn prepare(
    file: File,
    name: &str,
    format: Format,
    optional_paths: Option<&[String]>,
) -> Result<PreparedArchive> {
    prepare_with_budget(
        file,
        name,
        format,
        optional_paths,
        &AtomicBool::new(false),
        DECODED_BYTES,
    )
}
pub(super) fn prepare_with_budget(
    file: File,
    name: &str,
    format: Format,
    optional_paths: Option<&[String]>,
    cancel: &AtomicBool,
    decoded_limit: u64,
) -> Result<PreparedArchive> {
    super::super::name_ok(name)?;
    let scan = scan(file, cancel, decoded_limit, None)?;
    if scan.layout.format != format {
        return Err("整合包格式在识别后发生变化".into());
    }
    if format != Format::Modrinth && optional_paths.is_some_and(|paths| !paths.is_empty()) {
        return Err("此整合包格式未提供可按路径选择的可选文件".into());
    }
    let mut prepared = match format {
        Format::Pcl => return Err("PCL 本地导出包应使用原生导入服务".into()),
        Format::Modrinth => modrinth(&scan, optional_paths)?,
        Format::CurseForge => curseforge(&scan)?,
        Format::Mcbbs => mcbbs(&scan)?,
        Format::Hmcl => hmcl(&scan)?,
        Format::Multimc => multimc(&scan, name)?,
        Format::ReadyGame => ready(&scan, name)?,
        Format::Launcher => return Err("仅支持一层内嵌整合包；内层不能再次包含带启动器归档".into()),
    };
    for path in prepared.outputs.keys() {
        if path == &format!("{name}.jar") || path == &format!("{name}.json") {
            return Err("整合包输出与目标实例核心文件名冲突".into());
        }
    }
    super::archive::validate_outputs(
        &prepared.outputs.keys().cloned().collect(),
        &prepared.directories,
    )?;
    if !prepared.dependencies.contains_key("minecraft") {
        return Err("整合包未提供 Minecraft 版本".into());
    }
    prepared.minecraft = prepared.dependencies["minecraft"].clone();
    if prepared.bundled.is_none() {
        prepared
            .blockers
            .extend(super::manifest::dependencies(&prepared.dependencies).1);
    }
    for (path, output) in &prepared.outputs {
        if let Output::Remote { downloads, .. } = output {
            if !downloads
                .iter()
                .any(|url| super::mirror::declaration(url).is_ok_and(|url| url.is_some()))
            {
                prepared
                    .blockers
                    .push(format!("文件 {path} 没有受支持的 HTTPS 下载镜像"));
            }
        }
    }
    if prepared.preview_files.is_empty() {
        prepared.preview_files = prepared
            .outputs
            .iter()
            .map(|(path, output)| PreviewFile {
                path: path.clone(),
                size: output.size(),
                client: Client::Required,
                selected: true,
                overridden: matches!(output, Output::Override(_)),
            })
            .collect();
    }
    Ok(prepared)
}

fn metadata_path(path: &str) -> bool {
    let parts = path.split('/').collect::<Vec<_>>();
    (parts.len() <= 2
        && matches!(
            parts.last(),
            Some(
                &("manifest.json"
                    | "mcbbs.packmeta"
                    | "modpack.json"
                    | "modrinth.index.json"
                    | "mmc-pack.json"
                    | "instance.cfg")
            )
        )
        && !(parts.len() == 2
            && matches!(
                parts[0],
                ".minecraft" | "minecraft" | "versions" | "libraries" | "assets"
            )))
        || (parts.len() >= 2
            && parts.len() <= 3
            && parts[parts.len() - 2] == "minecraft"
            && matches!(parts.last(), Some(&("pack.json" | "version.json"))))
        || (parts.len() >= 3
            && parts.len() <= 5
            && parts[parts.len() - 3] == "versions"
            && parts[parts.len() - 1] == format!("{}.json", parts[parts.len() - 2]))
}
struct InnerCopy<'a> {
    entry: &'a NestedEntry,
    file: &'a mut File,
}
pub(super) struct OuterScan {
    pub entry: OverrideFile,
    pub decoded_bytes: u64,
}
pub(super) fn scan_outer(
    file: File,
    entry: &NestedEntry,
    destination: &mut File,
    cancel: &AtomicBool,
) -> Result<OuterScan> {
    let scan = scan(
        file,
        cancel,
        DECODED_BYTES,
        Some(InnerCopy {
            entry,
            file: destination,
        }),
    )?;
    let node = scan
        .nodes
        .get(&entry.archive_path)
        .ok_or("内嵌条目在扫描后缺失")?;
    Ok(OuterScan {
        entry: fact(&entry.archive_path, &entry.archive_path, node, false),
        decoded_bytes: scan.decoded_bytes,
    })
}
fn read_metadata(reader: &mut impl Read, size: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(size as usize);
    let mut buffer = [0u8; 128 * 1024];
    loop {
        super::super::check(cancel)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|_| "整合包清单解压或 CRC 校验失败")?;
        if count == 0 {
            break;
        }
        if (bytes.len() as u64).saturating_add(count as u64) > size {
            return Err("整合包清单实际大小超过声明".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    if bytes.len() as u64 != size {
        return Err("整合包清单实际大小与声明不符".into());
    }
    Ok(bytes)
}
fn scan(
    file: File,
    cancel: &AtomicBool,
    decoded_limit: u64,
    mut copy: Option<InnerCopy<'_>>,
) -> Result<Scan> {
    super::super::check(cancel)?;
    let metadata = file.metadata().map_err(|_| "无法检查整合包文件")?;
    if !metadata.is_file() || metadata.len() > ARCHIVE_BYTES {
        return Err("整合包源必须是普通 ZIP 文件且不超过 8 GiB".into());
    }
    let mut zip = super::super::archive::checked_zip(file)?;
    if zip.len() > NODE_COUNT {
        return Err("整合包 ZIP 节点超过 100000 项限制".into());
    }
    let mut nodes = BTreeMap::new();
    let mut total = 0u64;
    let mut paths = 0usize;
    let mut captured = 0u64;
    // Capture only bounded descriptor candidates first. This determines the
    // format before a large ignored body can bypass the smaller mrpack budget.
    for i in 0..zip.len() {
        super::super::check(cancel)?;
        let mut entry = zip
            .by_index(i)
            .map_err(|_| "整合包 ZIP 内容或加密声明无效")?;
        let raw =
            std::str::from_utf8(entry.name_raw()).map_err(|_| "整合包 ZIP 路径必须是 UTF-8")?;
        let path = raw.strip_suffix('/').unwrap_or(raw).to_owned();
        super::super::relative(&path)?;
        paths = paths
            .checked_add(path.len())
            .filter(|n| *n <= PATH_BYTES)
            .ok_or("整合包路径文本超过 8 MiB 限制")?;
        let directory = entry.is_dir();
        if entry.is_symlink()
            || entry.unix_mode().is_some_and(|mode| {
                let kind = mode & libc::S_IFMT;
                kind != 0
                    && kind
                        != if directory {
                            libc::S_IFDIR
                        } else {
                            libc::S_IFREG
                        }
            })
        {
            return Err("整合包 ZIP 包含链接或特殊文件".into());
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err("整合包 ZIP 使用暂不支持的压缩方式".into());
        }
        let size = entry.size();
        if size > FILE_BYTES
            || (directory && size != 0)
            || (size > 1024 * 1024 && size > entry.compressed_size().max(1).saturating_mul(1024))
        {
            return Err("整合包文件大小、目录声明或压缩比超过安全限制".into());
        }
        total = total
            .checked_add(size)
            .filter(|n| *n <= DECODED_BYTES.min(decoded_limit))
            .ok_or("整合包解压内容超过 16 GiB 限制")?;
        let foreign = super::foreign::archive_candidate(&path);
        if foreign && size > super::foreign::MAX_BYTES {
            return Err("整合包 PCL 元数据超过 64 KiB 限制".into());
        }
        let bytes = if !directory && (metadata_path(&path) || foreign) {
            if size > JSON_BYTES {
                return Err("整合包单项清单或版本 JSON 超过 4 MiB 限制".into());
            }
            captured = captured
                .checked_add(size)
                .filter(|n| *n <= CAPTURE_BYTES)
                .ok_or("整合包清单捕获总量超过 32 MiB 限制")?;
            Some(read_metadata(&mut entry, size, cancel)?)
        } else {
            None
        };
        if nodes
            .insert(
                path,
                Node {
                    directory,
                    size,
                    hash: String::new(),
                    sha1: String::new(),
                    bytes,
                },
            )
            .is_some()
        {
            return Err("整合包 ZIP 包含重复路径".into());
        }
    }
    super::archive::validate_outputs(
        &nodes
            .iter()
            .filter(|(_, node)| !node.directory)
            .map(|(path, _)| path.clone())
            .collect(),
        &nodes
            .iter()
            .filter(|(_, node)| node.directory)
            .map(|(path, _)| path.clone())
            .collect(),
    )?;
    let layout = layout(&nodes, cancel)?;
    if let Some(copy) = &copy {
        let node = nodes
            .get(&copy.entry.archive_path)
            .ok_or("内嵌条目在扫描前缺失")?;
        if layout.format != Format::Launcher || node.directory || node.size != copy.entry.size {
            return Err("内嵌整合包条目在识别后发生变化".into());
        }
    } else if layout.format == Format::Launcher {
        return Err("仅支持一层内嵌整合包；内层不能再次包含带启动器归档".into());
    }
    if layout.format == Format::Modrinth
        && (metadata.len() > super::MAX_ARCHIVE
            || total > super::MAX_DECODED
            || zip.len() > super::MAX_ENTRIES
            || nodes.values().any(|node| node.size > super::MAX_ENTRY))
    {
        return Err("mrpack 归档大小、节点或解压内容超过安全限制".into());
    }
    // Stream every body without retaining payloads. zip's reader verifies CRC
    // at EOF; hashes bind the original archive path to later held-FD extraction.
    for i in 0..zip.len() {
        super::super::check(cancel)?;
        let mut entry = zip.by_index(i).map_err(|_| "整合包 ZIP 内容无效")?;
        let path = std::str::from_utf8(entry.name_raw())
            .map_err(|_| "整合包 ZIP 路径无效")?
            .trim_end_matches('/')
            .to_owned();
        let node = nodes
            .get_mut(&path)
            .ok_or("整合包 ZIP 目录在扫描中发生变化")?;
        let mut size = 0u64;
        let mut sha256 = Sha256::new();
        let mut sha1 = Sha1::new();
        let mut buffer = [0u8; 128 * 1024];
        loop {
            super::super::check(cancel)?;
            let count = entry
                .read(&mut buffer)
                .map_err(|_| "整合包 ZIP 解压或 CRC 校验失败")?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(count as u64)
                .filter(|n| *n <= node.size)
                .ok_or("整合包实际解压大小超过声明")?;
            sha256.update(&buffer[..count]);
            sha1.update(&buffer[..count]);
            if let Some(copy) = &mut copy {
                if copy.entry.archive_path == path {
                    copy.file
                        .write_all(&buffer[..count])
                        .map_err(|_| "无法写入匿名内嵌整合包")?;
                }
            }
        }
        if size != node.size {
            return Err("整合包实际文件大小与声明不符".into());
        }
        node.hash = format!("{:x}", sha256.finalize());
        node.sha1 = format!("{:x}", sha1.finalize());
    }
    super::super::check(cancel)?;
    Ok(Scan {
        nodes,
        layout,
        decoded_bytes: total,
    })
}

fn game_root(value: &str) -> bool {
    matches!(
        value,
        ".minecraft" | "minecraft" | "versions" | "libraries" | "assets"
    )
}
fn nested_candidates(nodes: &BTreeMap<String, Node>, cancel: &AtomicBool) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for (path, node) in nodes {
        super::super::check(cancel)?;
        let parts = path.split('/').collect::<Vec<_>>();
        if !matches!(parts.last(), Some(&("modpack.zip" | "modpack.mrpack"))) {
            continue;
        }
        let direct =
            parts.len() == 1 || (parts.len() == 2 && !game_root(parts[0]) && parts[0] != ".hmcl");
        let hmcl = parts.len() == 3 && parts[0] == ".hmcl" && parts[1] == "modpack"
            || (parts.len() == 4
                && !game_root(parts[0])
                && parts[0] != ".hmcl"
                && parts[1] == ".hmcl"
                && parts[2] == "modpack");
        if direct || hmcl {
            super::super::relative(path)?;
            if node.directory {
                return Err("内嵌整合包条目必须是普通文件".into());
            }
            found.push(path.clone());
        }
    }
    Ok(found)
}

// Each ready-game subtree belongs to at most the root or one wrapper. Range
// lookup avoids rescanning every unrelated launcher path for every wrapper.
fn has_ready_nodes(
    nodes: &BTreeMap<String, Node>,
    prefix: &str,
    cancel: &AtomicBool,
) -> Result<bool> {
    for root in [".minecraft/versions/", "minecraft/versions/", "versions/"] {
        let start = format!("{prefix}{root}");
        for (path, _) in nodes
            .range(start.clone()..)
            .take_while(|(path, _)| path.starts_with(&start))
        {
            super::super::check(cancel)?;
            if path.ends_with(".json") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn layout(nodes: &BTreeMap<String, Node>, cancel: &AtomicBool) -> Result<Layout> {
    let mut prefixes = BTreeSet::from([String::new()]);
    for path in nodes.keys() {
        super::super::check(cancel)?;
        if let Some((first, _)) = path.split_once('/') {
            if !matches!(
                first,
                ".minecraft" | "minecraft" | "versions" | "libraries" | "assets"
            ) {
                prefixes.insert(format!("{first}/"));
            }
        }
    }
    let nested = nested_candidates(nodes, cancel)?;
    if !nested.is_empty() {
        let markers = [
            "pcl-export.json",
            "modrinth.index.json",
            "manifest.json",
            "mcbbs.packmeta",
            "modpack.json",
            "mmc-pack.json",
        ];
        if nested.len() != 1 {
            return Err("ZIP 内嵌整合包候选或其他格式清单存在歧义".into());
        }
        for prefix in &prefixes {
            super::super::check(cancel)?;
            if markers.iter().any(|name| {
                nodes
                    .get(&format!("{prefix}{name}"))
                    .is_some_and(|node| !node.directory)
            }) || has_ready_nodes(nodes, prefix, cancel)?
            {
                return Err("ZIP 内嵌整合包候选或其他格式清单存在歧义".into());
            }
        }
        let path = &nested[0];
        let prefix = path
            .rsplit_once('/')
            .map_or(String::new(), |(prefix, _)| format!("{prefix}/"));
        return Ok(Layout {
            format: Format::Launcher,
            prefix,
        });
    }
    let mut candidates = Vec::new();
    for prefix in prefixes {
        super::super::check(cancel)?;
        let has = |name: &str| {
            nodes
                .get(&format!("{prefix}{name}"))
                .is_some_and(|node| !node.directory)
        };
        let mut formats = Vec::new();
        if has("pcl-export.json") {
            formats.push(Format::Pcl);
        }
        if has("modrinth.index.json") {
            formats.push(Format::Modrinth);
        }
        if has("modpack.json") {
            formats.push(Format::Hmcl);
        }
        if has("mmc-pack.json") {
            formats.push(Format::Multimc);
        }
        let manifest = if has("manifest.json") {
            Some(node_json(nodes, &format!("{prefix}manifest.json"))?)
        } else {
            None
        };
        if has("mcbbs.packmeta")
            || manifest
                .as_ref()
                .is_some_and(|data| data.get("addons").is_some())
        {
            formats.push(Format::Mcbbs);
        } else if manifest.is_some() {
            formats.push(Format::CurseForge);
        }
        if formats.len() > 1 {
            return Err("ZIP 同一目录包含多个独立整合包清单，格式有歧义".into());
        }
        if let Some(format) = formats.first() {
            candidates.push(Layout {
                format: *format,
                prefix,
            });
        } else if has_ready_nodes(nodes, &prefix, cancel)? {
            candidates.push(Layout {
                format: Format::ReadyGame,
                prefix,
            });
        }
    }
    if candidates.len() != 1 {
        return Err(if candidates.is_empty() {
            "ZIP 未包含受支持的整合包清单或完整游戏版本"
        } else {
            "ZIP 根目录或多个包装目录包含不同整合包，格式有歧义"
        }
        .into());
    }
    Ok(candidates.remove(0))
}
fn node_json(nodes: &BTreeMap<String, Node>, path: &str) -> Result<Value> {
    json(
        nodes
            .get(path)
            .and_then(|node| node.bytes.as_deref())
            .ok_or("整合包缺少必需的清单或版本 JSON")?,
    )
}
fn descriptor(scan: &Scan, name: &str) -> Result<Value> {
    node_json(&scan.nodes, &format!("{}{name}", scan.layout.prefix))
}
fn text(value: &Value, key: &str, fallback: Option<&str>) -> Result<String> {
    let text = value
        .get(key)
        .map(|v| v.as_str().ok_or("整合包文本字段类型无效"))
        .transpose()?
        .or(fallback)
        .ok_or("整合包缺少必需的文本字段")?;
    if text.is_empty() || text.len() > 8192 || text.chars().any(char::is_control) {
        return Err("整合包文本字段无效或过长".into());
    }
    Ok(text.into())
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    let values = value
        .get(key)
        .map(|value| {
            value
                .as_array()
                .map(Vec::as_slice)
                .ok_or("整合包列表字段类型无效")
        })
        .transpose()?
        .unwrap_or(&[]);
    if values.len() > NODE_COUNT {
        return Err("整合包列表项超过安全限制".into());
    }
    Ok(values)
}
fn empty(pack_name: String, version: String) -> PreparedArchive {
    PreparedArchive {
        pack_name,
        version,
        minecraft: String::new(),
        dependencies: BTreeMap::new(),
        outputs: BTreeMap::new(),
        directories: BTreeSet::new(),
        warnings: Vec::new(),
        blockers: Vec::new(),
        summary: None,
        preview_files: Vec::new(),
        optional: BTreeSet::new(),
        bundled: None,
        shadowed_files: 0,
        rebuild_profile: None,
        curseforge_files: Vec::new(),
    }
}
fn summary(data: &Value) -> Result<Option<String>> {
    let Some(value) = data.get("description") else {
        return Ok(None);
    };
    let value = value.as_str().ok_or("整合包描述字段类型无效")?;
    if value.len() > 8192
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("整合包描述字段无效或过长".into());
    }
    Ok((!value.is_empty()).then(|| value.into()))
}
fn dependency(prepared: &mut PreparedArchive, id: &str, version: String) -> Result<()> {
    super::super::component(id)?;
    super::super::component(&version)?;
    if prepared.dependencies.len() >= 16
        || prepared.dependencies.insert(id.into(), version).is_some()
    {
        return Err("整合包依赖数量超限或重复声明".into());
    }
    Ok(())
}
fn fact(path: &str, archive_path: &str, node: &Node, client: bool) -> OverrideFile {
    OverrideFile {
        path: path.into(),
        size: node.size,
        hash: node.hash.clone(),
        client,
        archive_path: archive_path.into(),
    }
}
fn safe_output(path: &str) -> Result<()> {
    super::manifest::output_path(path)?;
    if path
        .rsplit('/')
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".exe"))
    {
        return Err("整合包客户端内容包含可执行程序".into());
    }
    Ok(())
}
fn layer(
    scan: &Scan,
    prepared: &mut PreparedArchive,
    root: &str,
    client: bool,
    skip: &BTreeSet<String>,
) -> Result<()> {
    let root = format!("{}{root}", scan.layout.prefix);
    if scan
        .nodes
        .get(root.trim_end_matches('/'))
        .is_some_and(|node| !node.directory)
    {
        return Err("整合包内容层必须是目录".into());
    }
    let mut foreign_count = 0;
    for (archive_path, node) in &scan.nodes {
        let Some(path) = archive_path.strip_prefix(&root) else {
            continue;
        };
        if path.is_empty() || skip.contains(path) {
            continue;
        }
        if scan.layout.format == Format::Modrinth && super::foreign::candidate(path, node.directory)
        {
            if !node.directory {
                super::foreign::validate(
                    path,
                    node.bytes.as_deref().ok_or("PCL 元数据未被完整捕获")?,
                )?;
                foreign_count += 1;
            }
            continue;
        }
        safe_output(path)?;
        if node.directory {
            prepared.directories.insert(path.into());
        } else {
            if prepared
                .outputs
                .insert(
                    path.into(),
                    Output::Override(fact(path, archive_path, node, client)),
                )
                .is_some()
            {
                prepared.shadowed_files += 1;
            }
        }
    }
    if foreign_count > 0 {
        prepared
            .warnings
            .push(super::foreign::warning(foreign_count));
    }
    Ok(())
}

fn modrinth(scan: &Scan, optional_paths: Option<&[String]>) -> Result<PreparedArchive> {
    if optional_paths.is_some_and(|paths| paths.len() > super::MAX_ENTRIES) {
        return Err("可选文件选择超过 10000 项限制".into());
    }
    let bytes = scan
        .nodes
        .get(&format!("{}modrinth.index.json", scan.layout.prefix))
        .and_then(|node| node.bytes.as_deref())
        .ok_or("缺少 Modrinth 清单")?;
    let manifest = super::manifest::parse(bytes)?;
    let mut prepared = empty(manifest.name, manifest.version_id);
    prepared.summary = manifest.summary;
    prepared.optional = optional_paths.unwrap_or(&[]).iter().cloned().collect();
    if prepared.optional.len() != optional_paths.unwrap_or(&[]).len()
        || prepared.optional.iter().any(|path| {
            !manifest
                .files
                .iter()
                .any(|file| file.path == *path && file.env.client == Client::Optional)
        })
    {
        return Err("只能选择清单中声明为客户端可选的文件，且不得重复".into());
    }
    for file in manifest.files {
        let selected = file.env.client == Client::Required
            || (file.env.client == Client::Optional && prepared.optional.contains(&file.path));
        prepared.preview_files.push(PreviewFile {
            path: file.path.clone(),
            size: file.file_size,
            client: file.env.client,
            selected,
            overridden: false,
        });
        if selected {
            prepared.outputs.insert(
                file.path.clone(),
                Output::Remote {
                    path: file.path,
                    size: file.file_size,
                    sha1: file.hashes["sha1"].clone(),
                    sha512: file.hashes["sha512"].clone(),
                    downloads: file.downloads,
                },
            );
        }
    }
    prepared.dependencies = manifest.dependencies;
    layer(scan, &mut prepared, "overrides/", false, &BTreeSet::new())?;
    layer(
        scan,
        &mut prepared,
        "client-overrides/",
        true,
        &BTreeSet::new(),
    )?;
    let server = format!("{}server-overrides/", scan.layout.prefix);
    if scan
        .nodes
        .get(server.trim_end_matches('/'))
        .is_some_and(|node| !node.directory)
    {
        return Err("mrpack 覆盖层必须是目录".into());
    }
    let mut server_count = 0;
    for (path, node) in &scan.nodes {
        if let Some(path) = path.strip_prefix(&server) {
            if super::foreign::candidate(path, node.directory) {
                if !node.directory {
                    super::foreign::validate(
                        path,
                        node.bytes.as_deref().ok_or("PCL 元数据未被完整捕获")?,
                    )?;
                    server_count += 1;
                }
                continue;
            }
            safe_output(path)?;
            if !node.directory {
                server_count += 1;
            }
        }
    }
    if server_count > 0 {
        prepared.warnings.push(format!(
            "忽略 {server_count} 个 server-overrides 文件；客户端不应用该层"
        ));
    }
    for file in &mut prepared.preview_files {
        file.overridden = matches!(prepared.outputs.get(&file.path), Some(Output::Override(_)));
    }
    Ok(prepared)
}

fn curse_data(data: &Value) -> Result<PreparedArchive> {
    if data["manifestType"].as_str() != Some("minecraftModpack")
        || data["manifestVersion"].as_u64() != Some(1)
    {
        return Err("仅支持 CurseForge minecraftModpack 清单版本 1".into());
    }
    let mut prepared = empty(
        text(data, "name", None)?,
        text(data, "version", Some("未注明"))?,
    );
    dependency(
        &mut prepared,
        "minecraft",
        text(&data["minecraft"], "version", None)?,
    )?;
    for loader in array(&data["minecraft"], "modLoaders")? {
        let id = text(loader, "id", None)?;
        let (kind, version) = id.split_once('-').ok_or("CurseForge 加载器标识无效")?;
        let kind = match kind {
            "fabric" => "fabric-loader",
            "quilt" => "quilt-loader",
            other => other,
        };
        dependency(&mut prepared, kind, version.into())?;
        if loader.get("primary").is_some_and(|v| !v.is_boolean()) {
            return Err("CurseForge primary 字段类型无效".into());
        }
    }
    let mut ids = BTreeSet::new();
    for file in array(data, "files")? {
        ids.insert(curse_ids(file)?);
    }
    if ids.len() != array(data, "files")?.len() {
        return Err("CurseForge 文件 ID 重复".into());
    }
    if !ids.is_empty() {
        prepared.curseforge_files = ids
            .iter()
            .map(|(project, file)| (*project as u32, *file as u32))
            .collect();
        prepared
            .blockers
            .push(super::curseforge::blocker(ids.len()));
        let optional = array(data, "files")?
            .iter()
            .filter(|file| file["required"].as_bool() == Some(false))
            .count();
        if optional > 0 {
            prepared.warnings.push(format!(
                "CurseForge 清单包含 {optional} 个可选文件；本次按完整清单安装全部列项"
            ));
        }
    }
    Ok(prepared)
}
fn curse_ids(file: &Value) -> Result<(u64, u64)> {
    let id = |key: &str| {
        file[key]
            .as_u64()
            .filter(|id| (1..=u32::MAX as u64).contains(id))
            .ok_or("CurseForge projectID/fileID 必须是有效正整数")
    };
    if file
        .get("required")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("CurseForge required 字段类型无效".into());
    }
    Ok((id("projectID")?, id("fileID")?))
}
fn curseforge(scan: &Scan) -> Result<PreparedArchive> {
    let data = descriptor(scan, "manifest.json")?;
    let mut prepared = curse_data(&data)?;
    let root = data
        .get("overrides")
        .map(|root| root.as_str().ok_or("CurseForge overrides 字段类型无效"))
        .transpose()?
        .unwrap_or("overrides");
    if matches!(root, "." | "./") {
        layer(
            scan,
            &mut prepared,
            "",
            false,
            &BTreeSet::from(["manifest.json".into()]),
        )?;
    } else if !root.is_empty() {
        super::super::relative(root.trim_end_matches('/'))?;
        layer(
            scan,
            &mut prepared,
            &format!("{}/", root.trim_end_matches('/')),
            false,
            &BTreeSet::new(),
        )?;
    }
    Ok(prepared)
}
fn mcbbs(scan: &Scan) -> Result<PreparedArchive> {
    let marker = if scan
        .nodes
        .contains_key(&format!("{}mcbbs.packmeta", scan.layout.prefix))
    {
        "mcbbs.packmeta"
    } else {
        "manifest.json"
    };
    let data = descriptor(scan, marker)?;
    if data["manifestType"].as_str() != Some("minecraftModpack")
        || !matches!(data["manifestVersion"].as_u64(), Some(1 | 2))
    {
        return Err("仅支持 MCBBS minecraftModpack 清单版本 1 或 2".into());
    }
    let mut prepared = empty(
        text(&data, "name", None)?,
        text(&data, "version", Some("未注明"))?,
    );
    prepared.summary = summary(&data)?;
    for addon in array(&data, "addons")? {
        let id = text(addon, "id", None)?;
        let id = match id.as_str() {
            "game" => "minecraft",
            "fabric" => "fabric-loader",
            "quilt" => "quilt-loader",
            other => other,
        };
        dependency(&mut prepared, id, text(addon, "version", None)?)?;
    }
    let mut paths = BTreeSet::new();
    let mut curse = BTreeSet::new();
    for file in array(&data, "files")? {
        match text(file, "type", None)?.as_str() {
            "addon" => {
                let path = text(file, "path", None)?;
                safe_output(&path)?;
                if !paths.insert(path.clone()) {
                    return Err("MCBBS addon 文件路径重复".into());
                }
                let hash = text(file, "hash", None)?;
                if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err("MCBBS addon 必须提供有效 SHA1".into());
                }
                let node = scan
                    .nodes
                    .get(&format!("{}overrides/{path}", scan.layout.prefix))
                    .filter(|node| !node.directory)
                    .ok_or("MCBBS 包缺少声明的 addon 文件")?;
                if !hash.eq_ignore_ascii_case(&node.sha1) {
                    return Err("MCBBS addon SHA1 与归档内容不符".into());
                }
            }
            "curse" => {
                if !curse.insert(curse_ids(file)?) {
                    return Err("MCBBS CurseForge 文件 ID 重复".into());
                }
                if let Some(url) = file.get("url") {
                    let url = url.as_str().ok_or("MCBBS 下载 URL 类型无效")?;
                    if !url.is_empty() {
                        super::mirror::declaration(url)?;
                    }
                }
            }
            _ => return Err("MCBBS 包含未知文件来源类型".into()),
        }
    }
    if !curse.is_empty() {
        prepared.blockers.push(format!(
            "此包包含 {} 个未解析的 CurseForge 文件；缺少获授权的官方文件元数据，无法完整安装",
            curse.len()
        ));
    }
    if marker == "mcbbs.packmeta"
        && scan
            .nodes
            .contains_key(&format!("{}manifest.json", scan.layout.prefix))
    {
        let compatibility = descriptor(scan, "manifest.json")?;
        let compatible = curse_data(&compatibility)?;
        let compatible_ids = array(&compatibility, "files")?
            .iter()
            .map(curse_ids)
            .collect::<Result<BTreeSet<_>>>()?;
        if compatible.dependencies != prepared.dependencies
            || compatible.pack_name != prepared.pack_name
            || compatible.version != prepared.version
            || text(&compatibility, "overrides", Some("overrides"))? != "overrides"
            || compatible_ids != curse
        {
            return Err("MCBBS 主清单与兼容 CurseForge 清单声明不一致".into());
        }
    }
    for key in ["javaArgument", "launchArgument"] {
        if !array(&data["launchInfo"], key)?.is_empty() {
            prepared
                .blockers
                .push("MCBBS 自定义启动参数尚未纳入当前安装计划，无法保证完整启动行为".into());
            break;
        }
    }
    if !array(&data, "libraries")?.is_empty() {
        prepared
            .blockers
            .push("MCBBS 包含额外运行库声明，当前安装器无法完整重建".into());
    }
    layer(scan, &mut prepared, "overrides/", false, &BTreeSet::new())?;
    Ok(prepared)
}

fn profile_dependencies(
    data: &Value,
    minecraft: Option<&str>,
    prepared: &mut PreparedArchive,
    rebuilding: bool,
) -> Result<()> {
    let game = data["clientVersion"]
        .as_str()
        .or_else(|| data["jar"].as_str().filter(|value| game_identifier(value)))
        .or_else(|| {
            data["inheritsFrom"]
                .as_str()
                .filter(|value| game_identifier(value))
        })
        .or(minecraft)
        .ok_or("游戏版本 JSON 未提供可核实的 Minecraft 身份")?;
    dependency(prepared, "minecraft", game.into())?;
    for library in profile_array(data, "libraries")? {
        let coordinate = text(library, "name", None)?;
        let parts = coordinate.split(':').collect::<Vec<_>>();
        if parts.len() < 3 {
            return Err("游戏版本库坐标无效".into());
        }
        let id = match (parts[0], parts[1]) {
            ("net.fabricmc", "fabric-loader") => Some("fabric-loader"),
            ("net.minecraftforge", "forge" | "minecraftforge") => Some("forge"),
            ("net.neoforged", "neoforge" | "forge") => Some("neoforge"),
            ("org.quiltmc", "quilt-loader") => Some("quilt-loader"),
            ("optifine", "OptiFine") => Some("optifine"),
            _ => None,
        };
        if let Some(id) = id {
            let version = parts[2]
                .strip_prefix(&format!("{game}-"))
                .unwrap_or(parts[2]);
            if prepared
                .dependencies
                .get(id)
                .is_some_and(|old| old == version)
            {
                continue;
            }
            dependency(prepared, id, version.into())?;
        }
    }
    for patch in profile_array(data, "patches")? {
        let id = text(patch, "id", None)?;
        let id = match id.as_str() {
            "game" => "minecraft",
            "fabric" => "fabric-loader",
            "quilt" => "quilt-loader",
            other => other,
        };
        let version = text(patch, "version", None)?;
        if prepared
            .dependencies
            .get(id)
            .is_some_and(|old| old == &version)
        {
            continue;
        }
        dependency(prepared, id, version)?;
    }
    let main = data["mainClass"].as_str().unwrap_or("");
    if rebuilding
        && prepared.dependencies.len() == 1
        && !main.is_empty()
        && !matches!(
            main,
            "net.minecraft.client.main.Main"
                | "net.minecraft.launchwrapper.Launch"
                | "net.minecraft.client.Minecraft"
        )
    {
        prepared
            .blockers
            .push("版本使用未识别的自定义游戏主类，无法按原版重建".into());
    }
    Ok(())
}
fn game_identifier(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || value.starts_with("rd-")
        || value.starts_with("inf-")
        || (value.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
            && value
                .as_bytes()
                .first()
                .is_some_and(|prefix| matches!(prefix, b'a' | b'b' | b'c')))
}

/// A pack's component versions alone cannot prove its runtime declarations.
/// Compare the fields it supplies to the actual captured provider launch
/// profile. Sparse/default HMCL serialization is allowed; custom runtime
/// values remain visible blockers instead of silently disappearing on rebuild.
pub(super) fn rebuild_blockers(raw: &[u8], expected: &Value) -> Result<Vec<String>> {
    if raw.len() as u64 > JSON_BYTES || !expected.is_object() {
        return Err("HMCL 重建版本校验信息无效或超限".into());
    }
    let mut declared = json(raw)?;
    if !declared.is_object() {
        return Err("HMCL 重建版本校验信息必须是对象".into());
    }
    let patches = profile_array(&declared, "patches")?;
    if patches.len() > 64 {
        return Err("HMCL 版本补丁数量超过安全限制".into());
    }
    let mut patches = patches.iter().cloned().collect::<Vec<_>>();
    for patch in &patches {
        if !patch.is_object()
            || patch
                .get("priority")
                .is_some_and(|p| !p.is_null() && !p.is_i64())
            || !profile_array(patch, "patches")?.is_empty()
        {
            return Err("HMCL 版本补丁结构无效".into());
        }
    }
    // HMCL applies ascending priority, preserving declaration order for ties.
    // Reuse our bounded version fold, rather than treating patch IDs as proof
    // that their libraries and arguments are standard provider content.
    patches.sort_by_key(|p| p["priority"].as_i64().unwrap_or(0));
    declared.as_object_mut().unwrap().remove("patches");
    for mut patch in patches {
        // Null optional fields do not override the previous patch's runtime.
        patch
            .as_object_mut()
            .unwrap()
            .retain(|_, value| !value.is_null());
        declared = super::super::archive::merge(declared, patch);
    }
    let mut blockers = BTreeSet::new();
    for field in [
        "mainClass",
        "minecraftArguments",
        "javaVersion",
        "assetIndex",
        "assets",
        "downloads",
        "logging",
        "compatibilityRules",
        "rules",
        "minimumLauncherVersion",
        "complianceLevel",
    ] {
        let value = &declared[field];
        let matches = if field == "minecraftArguments" {
            value
                .as_str()
                .zip(expected[field].as_str())
                .is_some_and(|(a, b)| a.split_ascii_whitespace().eq(b.split_ascii_whitespace()))
        } else {
            runtime_subset(value, &expected[field], 0)
        };
        if !runtime_empty(value) && !matches {
            blockers.insert(format!(
                "HMCL 版本的 {field} 与所选官方核心不一致，当前无法完整重建"
            ));
        }
    }
    if let Some(arguments) = declared.get("arguments").filter(|v| !runtime_empty(v)) {
        let object = arguments.as_object().ok_or("HMCL 启动参数字段类型无效")?;
        for (kind, values) in object {
            if runtime_empty(values) {
                continue;
            }
            let matches = matches!(kind.as_str(), "game" | "jvm")
                && argument_fragment(values, &expected["arguments"][kind]);
            if !matches {
                blockers.insert("HMCL 包含官方核心以外的自定义启动参数，当前无法完整重建".into());
            }
        }
    }
    let mut official_libraries: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for library in profile_array(expected, "libraries")? {
        let Some(coordinate) = library["name"]
            .as_str()
            .and_then(|name| library_coordinate(name).ok())
        else {
            return Err("已解析官方核心的库坐标无效".into());
        };
        official_libraries
            .entry(coordinate)
            .or_default()
            .push(library);
    }
    let mut coordinates = BTreeSet::new();
    for library in profile_array(&declared, "libraries")? {
        let coordinate = text(library, "name", None)?;
        let coordinate = library_coordinate(&coordinate)?;
        if !coordinates.insert(coordinate.clone()) {
            return Err("HMCL 版本库坐标重复".into());
        }
        if !official_libraries
            .get(&coordinate)
            .is_some_and(|candidates| {
                candidates
                    .iter()
                    .any(|official| library_runtime_matches(library, official))
            })
        {
            blockers
                .insert("HMCL 包含未由所选官方核心提供的运行库或库规则，当前无法完整重建".into());
        }
    }
    // These explicit alternate runtime fields are not supported by the native
    // launcher profile. Unrelated descriptive metadata is intentionally ignored.
    for field in [
        "processors",
        "tweakers",
        "gameArguments",
        "jvmArguments",
        "javaArguments",
        "launchArgument",
        "javaArgument",
        "extraLibraries",
        "classPath",
        "classpath",
    ] {
        if !runtime_empty(&declared[field]) {
            blockers.insert(format!(
                "HMCL 包含未支持的 {field} 运行时声明，当前无法完整重建"
            ));
        }
    }
    Ok(blockers.into_iter().collect())
}

fn profile_array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value]> {
    if value[field].is_null() {
        Ok(&[])
    } else {
        array(value, field)
    }
}

fn runtime_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.is_empty(),
        Value::Array(value) => value.is_empty(),
        Value::Object(value) => value.is_empty(),
        _ => false,
    }
}
fn runtime_subset(declared: &Value, expected: &Value, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    match declared {
        Value::Object(fields) => fields.iter().all(|(key, value)| {
            runtime_empty(value)
                || expected
                    .get(key)
                    .is_some_and(|other| runtime_subset(value, other, depth + 1))
        }),
        _ => declared == expected,
    }
}
fn library_coordinate(value: &str) -> Result<String> {
    let value = value.strip_suffix("@jar").unwrap_or(value);
    let parts = value.split(':').collect::<Vec<_>>();
    if !(3..=4).contains(&parts.len()) || parts.iter().any(|p| p.is_empty()) {
        return Err("HMCL 游戏版本库坐标无效".into());
    }
    Ok(value.into())
}
fn library_runtime_matches(declared: &Value, expected: &Value) -> bool {
    let Some(fields) = declared.as_object() else {
        return false;
    };
    fields.iter().all(|(field, value)| match field.as_str() {
        "name" | "description" | "comment" => true,
        "hint" | "MMC-hint" => runtime_empty(value) || value == &expected[field],
        "url" if !runtime_empty(value) => {
            if value == &expected["url"] {
                return true;
            }
            let Some(base) = value.as_str().and_then(|v| reqwest::Url::parse(v).ok()) else {
                return false;
            };
            let artifact = &expected["downloads"]["artifact"];
            artifact["path"]
                .as_str()
                .and_then(|path| base.join(path).ok())
                .zip(
                    artifact["url"]
                        .as_str()
                        .and_then(|url| reqwest::Url::parse(url).ok()),
                )
                .is_some_and(|(wanted, actual)| wanted == actual)
        }
        "sha1" | "sha256" | "sha512" | "md5" | "size" => {
            let other = expected
                .get(field)
                .unwrap_or(&expected["downloads"]["artifact"][field]);
            match (value.as_str(), other.as_str()) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => runtime_empty(value) || value == other,
            }
        }
        "checksums" if !runtime_empty(value) => value.as_array().is_some_and(|values| {
            values.iter().all(|value| {
                value.as_str().is_some_and(|hash| {
                    ["sha1", "md5", "sha256", "sha512"].iter().any(|kind| {
                        expected[*kind]
                            .as_str()
                            .or_else(|| expected["downloads"]["artifact"][*kind].as_str())
                            .is_some_and(|other| hash.eq_ignore_ascii_case(other))
                    })
                })
            })
        }),
        "filename" | "MMC-filename" if !runtime_empty(value) => {
            value == &expected[field]
                || value
                    .as_str()
                    .zip(expected["downloads"]["artifact"]["path"].as_str())
                    .is_some_and(|(file, path)| path.rsplit('/').next() == Some(file))
        }
        // Empty library descriptors are HMCL defaults, but an explicitly empty
        // rule list cannot remove nonempty provider OS/native constraints.
        "rules" | "natives" | "extract" => {
            if runtime_empty(value) {
                runtime_empty(&expected[field])
            } else {
                runtime_subset(value, &expected[field], 0)
            }
        }
        "downloads" => runtime_empty(value) || runtime_subset(value, &expected[field], 0),
        _ => runtime_empty(value) || runtime_subset(value, &expected[field], 0),
    })
}

/// Match whole argument units in order. Flags stay with their following value,
/// and ruled arguments remain indivisible. A set/subsequence comparison would
/// incorrectly authorize --username ${version_name} from unrelated tokens.
fn argument_fragment(declared: &Value, expected: &Value) -> bool {
    fn units(value: &Value) -> Option<Vec<Vec<Value>>> {
        let values = value.as_array()?;
        let mut result = Vec::new();
        let mut at = 0;
        while at < values.len() {
            let value = &values[at];
            if !value.is_string() && !value.is_object() {
                return None;
            }
            let mut unit = vec![value.clone()];
            at += 1;
            if value
                .as_str()
                .is_some_and(|v| v.starts_with('-') && !v.contains('='))
                && values
                    .get(at)
                    .and_then(Value::as_str)
                    .is_some_and(|v| !v.starts_with('-'))
            {
                unit.push(values[at].clone());
                at += 1;
            }
            result.push(unit);
        }
        Some(result)
    }
    let (Some(declared), Some(expected)) = (units(declared), units(expected)) else {
        return false;
    };
    declared.is_empty()
        || (declared.len() <= expected.len()
            && expected
                .windows(declared.len())
                .any(|part| part == declared))
}
fn hmcl(scan: &Scan) -> Result<PreparedArchive> {
    let data = descriptor(scan, "modpack.json")?;
    let mut prepared = empty(
        text(&data, "name", None)?,
        text(&data, "version", Some("未注明"))?,
    );
    prepared.summary = summary(&data)?;
    let profiles = ["minecraft/pack.json", "minecraft/version.json"]
        .iter()
        .filter(|path| {
            scan.nodes
                .contains_key(&format!("{}{path}", scan.layout.prefix))
        })
        .collect::<Vec<_>>();
    if profiles.len() > 1 {
        return Err("HMCL 同时包含不同游戏版本描述文件，无法确定安装来源".into());
    }
    if let Some(path) = profiles.first() {
        let profile = descriptor(scan, path)?;
        profile_dependencies(&profile, data["gameVersion"].as_str(), &mut prepared, true)?;
        prepared.rebuild_profile = Some(
            scan.nodes[&format!("{}{path}", scan.layout.prefix)]
                .bytes
                .as_ref()
                .ok_or("HMCL 游戏版本描述未捕获")?
                .clone(),
        );
        if data["gameVersion"]
            .as_str()
            .is_some_and(|version| prepared.dependencies["minecraft"] != version)
        {
            return Err("HMCL 整合包游戏版本与核心描述不一致".into());
        }
    } else {
        dependency(
            &mut prepared,
            "minecraft",
            text(&data, "gameVersion", None)?,
        )?;
    }
    layer(
        scan,
        &mut prepared,
        "minecraft/",
        false,
        &BTreeSet::from([
            "pack.json".into(),
            "version.json".into(),
            "minecraft.jar".into(),
        ]),
    )?;
    Ok(prepared)
}
fn multimc(scan: &Scan, name: &str) -> Result<PreparedArchive> {
    let data = descriptor(scan, "mmc-pack.json")?;
    if data["formatVersion"].as_u64() != Some(1) {
        return Err("仅支持 MultiMC 格式版本 1".into());
    }
    let cfg = scan
        .nodes
        .get(&format!("{}instance.cfg", scan.layout.prefix))
        .and_then(|node| node.bytes.as_deref())
        .ok_or("MultiMC 包缺少 instance.cfg")?;
    let cfg = std::str::from_utf8(cfg).map_err(|_| "MultiMC instance.cfg 必须是 UTF-8")?;
    let mut values = BTreeMap::new();
    for line in cfg.lines() {
        if let Some((key, value)) = line.trim().split_once('=') {
            if values.insert(key.trim(), value.trim()).is_some() {
                return Err("MultiMC instance.cfg 包含重复设置".into());
            }
        }
    }
    let mut prepared = empty(
        values
            .get("name")
            .filter(|value| !value.is_empty())
            .copied()
            .unwrap_or(name)
            .into(),
        "未注明".into(),
    );
    if values.iter().any(|(key, value)| {
        matches!(
            *key,
            "PreLaunchCommand" | "PostExitCommand" | "WrapperCommand" | "JvmArgs"
        ) && !value.is_empty()
    }) {
        prepared
            .blockers
            .push("MultiMC 自定义命令或 JVM 参数尚未纳入当前安装计划".into());
    }
    for component in array(&data, "components")? {
        let uid = text(component, "uid", None)?;
        let version = text(component, "version", None)?;
        let id = match uid.as_str() {
            "net.minecraft" => "minecraft",
            "net.minecraftforge" => "forge",
            "net.neoforged" => "neoforge",
            "net.fabricmc.fabric-loader" => "fabric-loader",
            "org.quiltmc.quilt-loader" => "quilt-loader",
            "org.lwjgl" | "org.lwjgl3" => continue,
            other => other,
        };
        dependency(&mut prepared, id, version)?;
    }
    if scan
        .nodes
        .keys()
        .any(|path| path.starts_with(&format!("{}patches/", scan.layout.prefix)))
    {
        prepared
            .blockers
            .push("MultiMC 包含自定义组件补丁，当前安装器无法完整重建".into());
    }
    let roots = [".minecraft/", "minecraft/"]
        .into_iter()
        .filter(|root| {
            scan.nodes
                .keys()
                .any(|path| path.starts_with(&format!("{}{root}", scan.layout.prefix)))
        })
        .collect::<Vec<_>>();
    if roots.len() > 1 {
        return Err("MultiMC 同时包含两个游戏内容根目录".into());
    }
    if let Some(root) = roots.first() {
        layer(scan, &mut prepared, root, false, &BTreeSet::new())?;
    }
    Ok(prepared)
}

fn ready(scan: &Scan, name: &str) -> Result<PreparedArchive> {
    let roots = [".minecraft/", "minecraft/", ""]
        .into_iter()
        .filter(|root| {
            scan.nodes.keys().any(|path| {
                path.starts_with(&format!("{}{root}versions/", scan.layout.prefix))
                    && path.ends_with(".json")
            })
        })
        .collect::<Vec<_>>();
    if roots.len() != 1 {
        return Err("完整游戏 ZIP 的游戏根目录不唯一".into());
    }
    let game = format!("{}{}", scan.layout.prefix, roots[0]);
    let profiles = format!("{game}versions/");
    let mut ids = BTreeSet::new();
    let mut parents = BTreeSet::new();
    for (path, node) in &scan.nodes {
        let Some(tail) = path.strip_prefix(&profiles) else {
            continue;
        };
        let parts = tail.split('/').collect::<Vec<_>>();
        if !node.directory && parts.len() == 2 && parts[1] == format!("{}.json", parts[0]) {
            ids.insert(parts[0].to_owned());
            let profile = node_json(&scan.nodes, path)?;
            for key in ["inheritsFrom", "jar"] {
                if let Some(parent) = profile[key].as_str() {
                    if parent != parts[0] {
                        parents.insert(parent.to_owned());
                    }
                }
            }
        }
    }
    let leaves = ids.difference(&parents).cloned().collect::<Vec<_>>();
    if leaves.len() != 1 {
        return Err("完整游戏 ZIP 必须只包含一个可确定的实例版本".into());
    }
    let leaf = &leaves[0];
    let (mut metadata, jar_path, base_id) = resolve(scan, &game, leaf, &mut BTreeSet::new())?;
    let mut prepared = empty(leaf.clone(), "本地完整游戏".into());
    profile_dependencies(
        &metadata,
        Some(base_id.as_str()).filter(|value| game_identifier(value)),
        &mut prepared,
        false,
    )?;
    metadata
        .as_object_mut()
        .ok_or("版本 JSON 必须是对象")?
        .remove("inheritsFrom");
    metadata["id"] = name.into();
    metadata["jar"] = name.into();
    metadata["clientVersion"] = prepared.dependencies["minecraft"].clone().into();
    let jar_node = scan
        .nodes
        .get(&jar_path)
        .filter(|node| !node.directory)
        .ok_or("完整游戏 ZIP 缺少核心 JAR")?;
    let jar = fact(&format!("{name}.jar"), &jar_path, jar_node, false);
    let mut shared = BTreeMap::new();
    let isolated = format!("{game}versions/{leaf}/");
    let isolation = scan.nodes.keys().any(|path| {
        ["config", "mods", "resourcepacks", "shaderpacks"]
            .iter()
            .any(|folder| {
                path == &format!("{isolated}{folder}")
                    || path.starts_with(&format!("{isolated}{folder}/"))
            })
    });
    for (archive_path, node) in &scan.nodes {
        let Some(path) = archive_path.strip_prefix(&game) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        if matches!(path, "libraries" | "assets") && node.directory {
            continue;
        }
        if path.starts_with("libraries/") || path.starts_with("assets/") {
            super::super::relative(path)?;
            if super::super::forbidden(path) || path.to_ascii_lowercase().ends_with(".exe") {
                return Err("完整游戏共享资源包含启动器保留路径或可执行程序".into());
            }
            if !node.directory {
                shared.insert(path.into(), fact(path, archive_path, node, false));
            }
            continue;
        }
        let path = if let Some(path) = archive_path.strip_prefix(&isolated) {
            if path == format!("{leaf}.json")
                || path == format!("{leaf}.jar")
                || path.starts_with("natives/")
                || path == "natives"
            {
                continue;
            }
            path
        } else if path.starts_with("versions/") || path == "versions" || isolation {
            continue;
        } else {
            path
        };
        if super::super::forbidden(path)
            || matches!(
                path,
                "launcher_profiles.json"
                    | "launcher_accounts.json"
                    | "accounts.json"
                    | "usercache.json"
                    | "usernamecache.json"
            )
            || path
                .split('/')
                .next()
                .is_some_and(|root| matches!(root, "runtime" | "runtimes" | "natives" | "bin"))
            || path.to_ascii_lowercase().ends_with(".exe")
        {
            prepared
                .warnings
                .push("已忽略原启动器设置、账户缓存或可执行程序".into());
            continue;
        }
        safe_output(path)?;
        if node.directory {
            prepared.directories.insert(path.into());
        } else if prepared
            .outputs
            .insert(
                path.into(),
                Output::Override(fact(path, archive_path, node, false)),
            )
            .is_some()
        {
            return Err("完整游戏共享内容与隔离实例内容重名".into());
        }
    }
    prepared.bundled = Some(BundledCore {
        metadata: serde_json::to_vec(&metadata).map_err(|_| "无法生成扁平版本 JSON")?,
        jar,
        shared,
    });
    prepared.warnings.sort();
    prepared.warnings.dedup();
    Ok(prepared)
}
fn resolve(
    scan: &Scan,
    game: &str,
    id: &str,
    visiting: &mut BTreeSet<String>,
) -> Result<(Value, String, String)> {
    super::super::component(id)?;
    if visiting.len() >= 64 || !visiting.insert(id.into()) {
        return Err("完整游戏版本继承循环或层级过深".into());
    }
    let path = format!("{game}versions/{id}/{id}.json");
    let mut data = node_json(&scan.nodes, &path)?;
    if !data.is_object() || data["id"].as_str() != Some(id) {
        return Err("版本 JSON 标识与归档路径不一致".into());
    }
    super::super::archive::portable(&data, 0)?;
    let own = format!("{game}versions/{id}/{id}.jar");
    let parent = data
        .get("inheritsFrom")
        .map(|value| value.as_str().ok_or("版本 inheritsFrom 类型无效"))
        .transpose()?;
    let (base, parent_jar, base_id) = if let Some(parent) = parent {
        let (data, jar, identity) = resolve(scan, game, parent, visiting)?;
        (Some(data), Some(jar), identity)
    } else {
        (None, None, id.to_owned())
    };
    let jar = if let Some(jar) = data.get("jar") {
        let jar = jar.as_str().ok_or("版本 jar 类型无效")?;
        super::super::component(jar)?;
        let path = format!("{game}versions/{jar}/{jar}.jar");
        if scan.nodes.get(&path).is_none_or(|node| node.directory) {
            return Err("完整游戏缺少引用的核心 JAR".into());
        }
        let profile = format!("{game}versions/{jar}/{jar}.json");
        if jar != id && !visiting.contains(jar) && scan.nodes.contains_key(&profile) {
            let _ = resolve(scan, game, jar, visiting)?;
        }
        path
    } else if scan.nodes.get(&own).is_some_and(|node| !node.directory) {
        own
    } else {
        parent_jar.ok_or("完整游戏缺少核心 JAR")?
    };
    if let Some(base) = base {
        data = super::super::archive::merge(base, data);
    }
    visiting.remove(id);
    Ok((data, jar, base_id))
}

// Validate decoded object keys before Value collapses duplicates, including
// escaped-equivalent keys inside arbitrary nested provider extension fields.
fn json(bytes: &[u8]) -> Result<Value> {
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    Unique::deserialize(&mut reader).map_err(|_| "整合包 JSON 无效、键重复或嵌套过深")?;
    reader.end().map_err(|_| "整合包 JSON 包含后续内容")?;
    serde_json::from_slice(bytes).map_err(|_| "整合包 JSON 字段无效".into())
}
struct Unique;
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(reader: D) -> std::result::Result<Self, D::Error> {
        reader.deserialize_any(UniqueVisitor)
    }
}
struct UniqueVisitor;
impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Unique;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded JSON with unique keys")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Unique, M::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > 4096 || keys.len() >= NODE_COUNT || !keys.insert(key) {
                return Err(serde::de::Error::custom("duplicate or excessive keys"));
            }
            map.next_value::<Unique>()?;
        }
        Ok(Unique)
    }
    fn visit_seq<S: SeqAccess<'de>>(
        self,
        mut sequence: S,
    ) -> std::result::Result<Unique, S::Error> {
        let mut count = 0;
        while sequence.next_element::<Unique>()?.is_some() {
            count += 1;
            if count > NODE_COUNT {
                return Err(serde::de::Error::custom("excessive entries"));
            }
        }
        Ok(Unique)
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
}

#[cfg(test)]
#[path = "formats_tests.rs"]
mod tests;
