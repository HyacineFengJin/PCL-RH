//! Bounded ZIP/ZIP64 parsing, v1 manifest validation and portable inheritance.
//! The central directory is checked before zip 2 can allocate from an untrusted
//! count or silently deduplicate paths. Entries are decompressed with byte/ratio
//! limits and hashed before a typed import plan is authorized. Version folding
//! mirrors core library override and argument append semantics.
use super::{
    check, component, error, forbidden, name_ok, relative, Result, MAX_BYTES, MAX_DEPTH, MAX_FILE,
    MAX_FILES, MAX_JOURNAL, MAX_JSON, MAX_MANIFEST,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::atomic::AtomicBool,
};
use zip::{CompressionMethod, ZipArchive};
#[derive(Clone, Debug, Serialize)]
pub(super) struct ArchiveEntry {
    pub(super) index: usize,
    pub(super) path: String,
    pub(super) directory: bool,
    pub(super) size: u64,
    pub(super) hash: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub(super) format: String,
    pub(super) format_version: u32,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) game: ManifestGame,
    pub(super) loaders: Vec<ManifestLoader>,
    pub(super) bundled_assets: bool,
    pub(super) content_root: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestGame {
    pub(super) minecraft: String,
    pub(super) instance: String,
    pub(super) isolated: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestLoader {
    pub(super) name: String,
    pub(super) version: String,
}

pub(super) struct ArchiveScan {
    pub(super) entries: BTreeMap<String, ArchiveEntry>,
    pub(super) json: BTreeMap<String, Vec<u8>>,
    pub(super) manifest: Manifest,
}
/// zip 2 deduplicates equal names in its IndexMap before exposing entries.
/// Inspect the bounded central directory first, both to catch those duplicates
/// and to cap allocations before ZipArchive trusts the declared entry count.
fn preflight_zip(file: &mut File) -> Result<usize> {
    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes(b[i..i + 2].try_into().expect("bounded ZIP field"))
    }
    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().expect("bounded ZIP field"))
    }
    fn u64_at(b: &[u8], i: usize) -> u64 {
        u64::from_le_bytes(b[i..i + 8].try_into().expect("bounded ZIP field"))
    }
    let size = file.metadata().map_err(error)?.len();
    let tail_len = size.min(65557) as usize;
    if tail_len < 22 {
        return Err("ZIP 目录记录缺失".into());
    }
    file.seek(SeekFrom::Start(size - tail_len as u64))
        .map_err(error)?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail).map_err(error)?;
    let at = (0..=tail.len() - 22)
        .rev()
        .find(|i| {
            tail[*i..*i + 4] == [0x50, 0x4b, 0x05, 0x06]
                && *i + 22 + u16_at(&tail, *i + 20) as usize == tail.len()
        })
        .ok_or("ZIP 目录记录无效")?;
    let end = size - tail_len as u64 + at as u64;
    let eocd = &tail[at..at + 22];
    if u16_at(eocd, 4) != 0 || u16_at(eocd, 6) != 0 {
        return Err("不支持分卷 ZIP".into());
    }
    let mut count = u16_at(eocd, 10) as u64;
    let mut cd_size = u32_at(eocd, 12) as u64;
    let mut cd_offset = u32_at(eocd, 16) as u64;
    let mut cd_end = end;
    if count == u16::MAX as u64 || cd_size == u32::MAX as u64 || cd_offset == u32::MAX as u64 {
        if end < 20 {
            return Err("ZIP64 定位记录缺失".into());
        }
        file.seek(SeekFrom::Start(end - 20)).map_err(error)?;
        let mut locator = [0u8; 20];
        file.read_exact(&mut locator).map_err(error)?;
        if locator[..4] != [0x50, 0x4b, 0x06, 0x07]
            || u32_at(&locator, 4) != 0
            || u32_at(&locator, 16) != 1
        {
            return Err("ZIP64 定位或分卷声明无效".into());
        }
        let offset = u64_at(&locator, 8);
        if offset.checked_add(56).is_none_or(|n| n > end - 20) {
            return Err("ZIP64 目录偏移无效".into());
        }
        file.seek(SeekFrom::Start(offset)).map_err(error)?;
        let mut record = [0u8; 56];
        file.read_exact(&mut record).map_err(error)?;
        let record_size = u64_at(&record, 4);
        if record[..4] != [0x50, 0x4b, 0x06, 0x06]
            || !(44..=MAX_MANIFEST).contains(&record_size)
            || offset.checked_add(12 + record_size) != Some(end - 20)
            || u32_at(&record, 16) != 0
            || u32_at(&record, 20) != 0
            || u64_at(&record, 24) != u64_at(&record, 32)
        {
            return Err("ZIP64 目录声明无效".into());
        }
        count = u64_at(&record, 32);
        cd_size = u64_at(&record, 40);
        cd_offset = u64_at(&record, 48);
        cd_end = offset;
    } else if u16_at(eocd, 8) != u16_at(eocd, 10) {
        return Err("ZIP 文件数量声明无效".into());
    }
    if count > MAX_FILES as u64
        || cd_size > MAX_JOURNAL
        || cd_offset.checked_add(cd_size) != Some(cd_end)
    {
        return Err("ZIP 目录数量、大小或偏移超过安全限制".into());
    }
    file.seek(SeekFrom::Start(cd_offset)).map_err(error)?;
    let mut central = vec![0; cd_size as usize];
    file.read_exact(&mut central).map_err(error)?;
    let mut cursor = 0usize;
    let mut names = BTreeSet::new();
    for _ in 0..count {
        if cursor.checked_add(46).is_none_or(|n| n > central.len())
            || central[cursor..cursor + 4] != [0x50, 0x4b, 0x01, 0x02]
        {
            return Err("ZIP 中央目录记录无效".into());
        }
        let header = &central[cursor..cursor + 46];
        let name_len = u16_at(header, 28) as usize;
        let extra = u16_at(header, 30) as usize;
        let comment = u16_at(header, 32) as usize;
        if name_len == 0 || name_len > 4096 || u16_at(header, 34) != 0 || u16_at(header, 8) & 1 != 0
        {
            return Err("ZIP 包含无效路径、分卷或加密文件".into());
        }
        let next = cursor
            .checked_add(46 + name_len + extra + comment)
            .filter(|n| *n <= central.len())
            .ok_or("ZIP 中央目录字段越界")?;
        let raw = std::str::from_utf8(&central[cursor + 46..cursor + 46 + name_len])
            .map_err(|_| "ZIP 路径必须是 UTF-8")?;
        let path = raw.strip_suffix('/').unwrap_or(raw);
        relative(path)?;
        if !names.insert(path.to_owned()) {
            return Err("ZIP 包含重复文件或目录路径".into());
        }
        cursor = next;
    }
    if cursor != central.len() {
        return Err("ZIP 中央目录包含未声明记录".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    Ok(count as usize)
}
pub(super) fn checked_zip(mut file: File) -> Result<ZipArchive<File>> {
    let count = preflight_zip(&mut file)?;
    let zip = ZipArchive::new(file).map_err(|e| format!("无法读取本启动器导出的 ZIP：{e}"))?;
    if zip.len() != count {
        return Err("ZIP 路径解码冲突或重复，拒绝导入".into());
    }
    Ok(zip)
}
pub(super) fn scan_archive(file: File, cancel: &AtomicBool) -> Result<ArchiveScan> {
    let mut zip = checked_zip(file)?;
    if zip.len() > MAX_FILES {
        return Err("ZIP 文件节点数量超过 100000 限制".into());
    }
    let mut entries = BTreeMap::new();
    let mut jsons = BTreeMap::new();
    let mut captured_json_bytes = 0u64;
    let mut total = 0u64;
    for index in 0..zip.len() {
        check(cancel)?;
        let mut entry = zip
            .by_index(index)
            .map_err(|e| format!("ZIP 内容无效、加密或压缩方式不支持：{e}"))?;
        let raw = std::str::from_utf8(entry.name_raw())
            .map_err(|_| "ZIP 路径必须是 UTF-8")?
            .to_owned();
        let directory = entry.is_dir();
        let path = if directory {
            raw.strip_suffix('/').ok_or("ZIP 目录路径无效")?
        } else {
            raw.as_str()
        }
        .to_owned();
        relative(&path)?;
        if entry.is_symlink()
            || entry.unix_mode().is_some_and(|mode| {
                let kind = mode & 0o170000;
                kind != 0 && kind != if directory { 0o040000 } else { 0o100000 }
            })
        {
            return Err("ZIP 包含链接或特殊文件，拒绝导入".into());
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err("ZIP 使用暂不支持的压缩方式".into());
        }
        if forbidden(&path) || (!(path == "pcl-export.json") && !path.starts_with(".minecraft/")) {
            return Err("仅支持本启动器导出的本地 ZIP；包中包含不支持的文件布局".into());
        }
        if entries.contains_key(&path) {
            return Err("ZIP 包含重复文件或目录路径".into());
        }
        let size = entry.size();
        if size > MAX_FILE || (directory && size != 0) {
            return Err("ZIP 单个文件大小或目录声明无效".into());
        }
        if size > 1024 * 1024 && size > entry.compressed_size().max(1).saturating_mul(1024) {
            return Err("ZIP 压缩比超过安全限制，可能是压缩炸弹".into());
        }
        total = total
            .checked_add(size)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or("ZIP 解压内容超过 256 GiB 限制")?;
        let capture = path == "pcl-export.json"
            || (path.starts_with(".minecraft/versions/")
                && path.ends_with(".json")
                && relative(&path)?.len() == 4)
            || (path.starts_with(".minecraft/assets/indexes/")
                && path.ends_with(".json")
                && relative(&path)?.len() == 4);
        let limit = if path == "pcl-export.json" {
            MAX_MANIFEST
        } else if capture {
            MAX_JSON
        } else {
            MAX_FILE
        };
        if size > limit {
            return Err("ZIP 清单或版本 JSON 超过大小限制".into());
        }
        if capture {
            // A count/total-uncompressed limit alone does not bound memory:
            // JSON metadata is retained for inheritance and legacy indexes.
            captured_json_bytes = captured_json_bytes
                .checked_add(size)
                .filter(|n| *n <= 64 * 1024 * 1024)
                .ok_or("ZIP 清单、版本和资源索引 JSON 总计超过 64 MiB 限制")?;
        }
        let mut hash = Sha256::new();
        let mut bytes = Vec::new();
        let mut count = 0u64;
        let mut buf = [0u8; 128 * 1024];
        loop {
            check(cancel)?;
            let n = entry
                .read(&mut buf)
                .map_err(|e| format!("ZIP 校验或解压失败：{e}"))?;
            if n == 0 {
                break;
            }
            count = count
                .checked_add(n as u64)
                .filter(|n| *n <= size && *n <= limit)
                .ok_or("ZIP 实际解压大小超过声明或限制")?;
            hash.update(&buf[..n]);
            if capture {
                bytes.extend_from_slice(&buf[..n]);
            }
        }
        if count != size {
            return Err("ZIP 实际内容大小与声明不一致".into());
        }
        entries.insert(
            path.clone(),
            ArchiveEntry {
                index,
                path: path.clone(),
                directory,
                size,
                hash: format!("{:x}", hash.finalize()),
            },
        );
        if capture {
            jsons.insert(path, bytes);
        }
    }
    // Detect file/directory prefix collisions even when ZIP omits directories.
    for path in entries.keys() {
        let parts = relative(path)?;
        for depth in 1..parts.len() {
            if entries
                .get(&parts[..depth].join("/"))
                .is_some_and(|e| !e.directory)
            {
                return Err("ZIP 文件与目录路径冲突".into());
            }
        }
    }
    let manifest: Manifest = serde_json::from_slice(jsons.get("pcl-export.json").ok_or(
        "仅支持本启动器导出的本地 ZIP（缺少 pcl-export.json）；尚不支持 mrpack 或 CurseForge",
    )?)
    .map_err(|e| format!("导出包清单无效：{e}"))?;
    if manifest.format != "pcl-local-instance" || manifest.format_version != 1 {
        return Err(
            "仅支持本启动器 pcl-local-instance v1 本地 ZIP，尚不支持 mrpack 或 CurseForge".into(),
        );
    }
    name_ok(&manifest.game.instance)?;
    component(&manifest.game.minecraft)?;
    for (value, limit) in [(&manifest.name, 200), (&manifest.version, 128)] {
        if value.is_empty()
            || value.trim() != value
            || value.chars().count() > limit
            || value.chars().any(char::is_control)
        {
            return Err("导出包名称或版本无效".into());
        }
    }
    for loader in &manifest.loaders {
        if loader.name.len() > 80
            || loader.version.len() > 160
            || loader.name.chars().any(char::is_control)
            || loader.version.chars().any(char::is_control)
        {
            return Err("导出包加载器声明无效".into());
        }
    }
    let expected = if manifest.game.isolated {
        format!(".minecraft/versions/{}", manifest.game.instance)
    } else {
        ".minecraft".into()
    };
    if manifest.content_root != expected {
        return Err("导出包游戏内容目录与隔离声明不一致".into());
    }
    Ok(ArchiveScan {
        entries,
        json: jsons,
        manifest,
    })
}

/// Old Minecraft asset indexes require a root-level resources mirror. For a
/// shared-content source, those same bytes also belong to the isolated game
/// content copy; the importer publishes both explicit destinations.
pub(super) fn legacy_resources(scan: &ArchiveScan) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    for (path, bytes) in &scan.json {
        if !path.starts_with(".minecraft/assets/indexes/") {
            continue;
        }
        let data: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("导出包资源索引无效：{e}"))?;
        if data["map_to_resources"].as_bool() != Some(true) {
            continue;
        }
        for name in data["objects"]
            .as_object()
            .ok_or("旧版资源索引缺少 objects")?
            .keys()
        {
            relative(name)?;
            let path = format!("resources/{name}");
            if forbidden(&path) {
                return Err("旧版资源索引包含启动器保留路径".into());
            }
            paths.insert(path);
        }
    }
    Ok(paths)
}

// Match core's inheritance semantics: child libraries replace group/artifact/
// classifier, while game/JVM argument arrays append. This produces one portable
// JSON without publishing auxiliary versions into the user's versions list.
pub(super) fn merge(mut base: Value, child: Value) -> Value {
    fn key(v: &Value) -> String {
        let name = v["name"].as_str().unwrap_or("");
        let p: Vec<_> = name.split(':').collect();
        if p.len() >= 3 {
            format!("{}:{}:{}", p[0], p[1], p.get(3).unwrap_or(&""))
        } else {
            name.into()
        }
    }
    let mut libraries = base["libraries"].as_array().cloned().unwrap_or_default();
    for library in child["libraries"].as_array().into_iter().flatten() {
        let k = key(library);
        libraries.retain(|v| key(v) != k);
        libraries.push(library.clone());
    }
    let has_arguments = base.get("arguments").is_some() || child.get("arguments").is_some();
    let mut arguments = serde_json::Map::new();
    for k in ["game", "jvm"] {
        let mut a = base["arguments"][k].as_array().cloned().unwrap_or_default();
        a.extend(
            child["arguments"][k]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
        arguments.insert(k.into(), Value::Array(a));
    }
    for (k, v) in child.as_object().expect("validated object") {
        base[k] = v.clone();
    }
    base["libraries"] = libraries.into();
    if has_arguments {
        base["arguments"] = arguments.into();
    }
    base
}
pub(super) fn portable(value: &Value, depth: usize) -> Result<()> {
    fn account_argument(flag: &str, value: &str) -> bool {
        matches!(
            flag,
            "--username" | "--uuid" | "--accessToken" | "--clientId" | "--xuid"
        ) && !(value.starts_with("${") && value.ends_with('}'))
    }
    if depth > MAX_DEPTH {
        return Err("版本 JSON 层级过深".into());
    }
    match value {
        Value::String(text) => {
            if text.contains("file://")
                || text.split(['=', ';', ' ', ',']).any(|s| {
                    s.starts_with('/')
                        || (s.as_bytes().get(1) == Some(&b':')
                            && s.as_bytes()
                                .get(2)
                                .is_some_and(|b| *b == b'/' || *b == b'\\'))
                })
            {
                return Err("版本 JSON 包含本机绝对路径，无法安全迁移".into());
            }
            let words: Vec<_> = text.split_whitespace().collect();
            if words.windows(2).any(|p| account_argument(p[0], p[1]))
                || words.iter().any(|s| {
                    s.split_once('=')
                        .is_some_and(|(f, v)| account_argument(f, v))
                })
            {
                return Err("版本 JSON 包含固定账户参数，拒绝导入".into());
            }
        }
        Value::Array(values) => {
            if values.windows(2).any(|p| {
                p[0].as_str()
                    .zip(p[1].as_str())
                    .is_some_and(|(f, v)| account_argument(f, v))
            }) {
                return Err("版本 JSON 包含固定账户参数，拒绝导入".into());
            }
            for v in values {
                portable(v, depth + 1)?;
            }
        }
        Value::Object(values) => {
            for (k, v) in values {
                if [
                    "accessToken",
                    "refreshToken",
                    "clientToken",
                    "accounts",
                    "authenticationDatabase",
                ]
                .contains(&k.as_str())
                {
                    return Err("版本 JSON 包含账户信息，拒绝导入".into());
                }
                portable(v, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(super) fn resolve_version(
    scan: &ArchiveScan,
    id: &str,
    visiting: &mut BTreeSet<String>,
    used: &mut BTreeSet<String>,
) -> Result<(Value, String)> {
    component(id)?;
    if visiting.len() >= 64 || !visiting.insert(id.into()) {
        return Err("导出包版本继承循环或层级过深".into());
    }
    let path = format!(".minecraft/versions/{id}/{id}.json");
    let mut data: Value =
        serde_json::from_slice(scan.json.get(&path).ok_or("导出包缺少继承版本 JSON")?)
            .map_err(|e| format!("导出包版本 JSON 无效：{e}"))?;
    if !data.is_object() || data["id"].as_str() != Some(id) {
        return Err("导出包版本 JSON 标识与文件路径不一致".into());
    }
    portable(&data, 0)?;
    used.insert(path);
    let own = format!(".minecraft/versions/{id}/{id}.jar");
    let own_exists = scan.entries.get(&own).is_some_and(|e| !e.directory);
    if own_exists {
        used.insert(own.clone());
    }
    let parent = data
        .get("inheritsFrom")
        .map(|v| v.as_str().ok_or("版本 inheritsFrom 字段无效"))
        .transpose()?;
    let (base, inherited_jar) = match parent {
        Some(parent) => {
            let (base, jar) = resolve_version(scan, parent, visiting, used)?;
            (Some(base), Some(jar))
        }
        None => (None, None),
    };
    let jar = if let Some(jar) = data.get("jar") {
        let jar = jar.as_str().ok_or("版本 jar 字段无效")?;
        component(jar)?;
        let path = format!(".minecraft/versions/{jar}/{jar}.jar");
        if scan.entries.get(&path).is_none_or(|e| e.directory) {
            return Err("导出包缺少引用的游戏 JAR".into());
        }
        used.insert(path.clone());
        // Export may include the jar profile's JSON, even if it is not in the
        // inheritance chain. Validate it, but do not merge its runtime options.
        let profile = format!(".minecraft/versions/{jar}/{jar}.json");
        if jar != id && !visiting.contains(jar) && scan.json.contains_key(&profile) {
            let _ = resolve_version(scan, jar, visiting, used)?;
        }
        path
    } else if own_exists {
        own
    } else {
        inherited_jar.ok_or("导出包缺少游戏本体 JAR")?
    };
    if let Some(base) = base {
        data = merge(base, data);
    }
    visiting.remove(id);
    Ok((data, jar))
}
