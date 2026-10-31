//! Reuse strict ZIP central-directory preflight, then stream every body once.
//! Server and unknown extras are never authorized as client outputs, but still
//! receive path/node/compression/size/ratio/CRC checks within the decoded budget.
use super::*;
use std::io::Read;
use zip::CompressionMethod;

#[derive(Clone, Serialize)]
pub(super) struct OverrideFile {
    pub path: String,
    pub size: u64,
    pub hash: String,
    pub client: bool,
    pub(super) archive_path: String,
}
pub(super) struct ArchiveScan {
    pub index: Vec<u8>,
    pub common: BTreeMap<String, OverrideFile>,
    pub client: BTreeMap<String, OverrideFile>,
    pub directories: BTreeSet<String>,
    pub server_files: usize,
    pub ignored_files: usize,
    pub foreign_files: usize,
}
pub(super) fn validate_outputs(
    files: &BTreeSet<String>,
    directories: &BTreeSet<String>,
) -> Result<()> {
    for path in files {
        if directories.contains(path) {
            return Err("mrpack 输出同一路径同时声明文件与目录".into());
        }
        let parts = super::super::relative(path)?;
        for end in 1..parts.len() {
            if files.contains(&parts[..end].join("/")) {
                return Err("mrpack 输出文件与父目录路径冲突".into());
            }
        }
    }
    for path in directories {
        let parts = super::super::relative(path)?;
        for end in 1..parts.len() {
            if files.contains(&parts[..end].join("/")) {
                return Err("mrpack 输出目录的父路径是文件".into());
            }
        }
    }
    Ok(())
}
pub(super) fn scan_cancellable(
    file: File,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<ArchiveScan> {
    super::super::check(cancel)?;
    let mut zip = super::super::archive::checked_zip(file)?;
    if zip.len() > MAX_ENTRIES {
        return Err("mrpack ZIP 文件/目录节点超过 10000 项限制".into());
    }
    let mut nodes = BTreeMap::new();
    let mut common = BTreeMap::new();
    let mut client = BTreeMap::new();
    let mut directories = BTreeSet::new();
    let mut index_bytes = None;
    let mut server_files = 0;
    let mut ignored_files = 0;
    let mut foreign_files = 0;
    let mut total = 0u64;
    let mut path_bytes = 0usize;
    for i in 0..zip.len() {
        super::super::check(cancel)?;
        let mut entry = zip
            .by_index(i)
            .map_err(|_| "mrpack ZIP 文件内容、加密或压缩方式无效")?;
        let raw =
            std::str::from_utf8(entry.name_raw()).map_err(|_| "mrpack ZIP 路径必须是 UTF-8")?;
        let directory = entry.is_dir();
        let path = raw.strip_suffix('/').unwrap_or(raw).to_owned();
        path_bytes = path_bytes
            .checked_add(path.len())
            .filter(|n| *n <= MAX_PATH_BYTES)
            .ok_or("mrpack ZIP 路径文本总计超过 8 MiB 内存限额")?;
        super::super::relative(&path)?;
        if entry.is_symlink()
            || entry.unix_mode().is_some_and(|mode| {
                let kind = mode & 0o170000;
                kind != 0 && kind != if directory { 0o040000 } else { 0o100000 }
            })
        {
            return Err("mrpack ZIP 包含链接或特殊文件".into());
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err("mrpack ZIP 使用暂不支持的压缩方式".into());
        }
        if nodes.insert(path.clone(), directory).is_some() {
            return Err("mrpack ZIP 包含重复路径".into());
        }
        if path == "pcl-export.json" {
            return Err("ZIP 同时包含 PCL 与 Modrinth 根清单，格式有歧义".into());
        }
        let layer = if let Some(p) = path.strip_prefix("overrides/") {
            Some((p, false, false))
        } else if let Some(p) = path.strip_prefix("client-overrides/") {
            Some((p, true, false))
        } else {
            path.strip_prefix("server-overrides/")
                .map(|p| (p, false, true))
        };
        if matches!(
            path.as_str(),
            "overrides" | "client-overrides" | "server-overrides"
        ) && !directory
        {
            return Err("mrpack 覆盖层必须是目录".into());
        }
        let foreign = layer.is_some_and(|(output, _, _)| foreign::candidate(output, directory));
        if let Some((output, _, server)) = layer {
            if !foreign {
                manifest::output_path(output)?;
                if directory && !server {
                    directories.insert(output.into());
                }
            }
        }
        let size = entry.size();
        let limit = if path == INDEX {
            MAX_INDEX
        } else if foreign {
            foreign::MAX_BYTES
        } else {
            MAX_ENTRY
        };
        if size > limit
            || (directory && size != 0)
            || (size > 1024 * 1024 && size > entry.compressed_size().max(1).saturating_mul(1024))
        {
            return Err("mrpack 文件/清单大小、目录声明或压缩比超过安全限制".into());
        }
        total = total
            .checked_add(size)
            .filter(|s| *s <= MAX_DECODED)
            .ok_or("mrpack ZIP 解压内容超过 1 GiB 限制")?;
        let mut bytes = Vec::new();
        let mut hash = Sha256::new();
        let mut decoded = 0u64;
        let mut buffer = [0u8; 128 * 1024];
        loop {
            super::super::check(cancel)?;
            let n = entry
                .read(&mut buffer)
                .map_err(|_| "mrpack ZIP 解压或 CRC 校验失败")?;
            if n == 0 {
                break;
            }
            decoded = decoded
                .checked_add(n as u64)
                .filter(|s| *s <= size && *s <= limit)
                .ok_or("mrpack ZIP 实际解压大小超过声明")?;
            hash.update(&buffer[..n]);
            if path == INDEX || foreign {
                bytes.extend_from_slice(&buffer[..n]);
            }
        }
        if decoded != size {
            return Err("mrpack ZIP 实际文件大小与声明不符".into());
        }
        if path == INDEX {
            if directory {
                return Err("mrpack 根清单必须是普通文件".into());
            }
            index_bytes = Some(bytes);
        } else if foreign {
            if !directory {
                foreign::validate(layer.unwrap().0, &bytes)?;
                foreign_files += 1;
            }
        } else if !directory {
            match layer {
                Some((_, _, true)) => server_files += 1,
                Some((output, is_client, false)) => {
                    let fact = OverrideFile {
                        path: output.into(),
                        size,
                        hash: format!("{:x}", hash.finalize()),
                        client: is_client,
                        archive_path: path.clone(),
                    };
                    if is_client {
                        client.insert(output.into(), fact);
                    } else {
                        common.insert(output.into(), fact);
                    }
                }
                None => ignored_files += 1,
            }
        }
    }
    let files = nodes
        .iter()
        .filter(|(_, directory)| !**directory)
        .map(|(p, _)| p.clone())
        .collect();
    let dirs = nodes
        .iter()
        .filter(|(_, directory)| **directory)
        .map(|(p, _)| p.clone())
        .collect();
    validate_outputs(&files, &dirs)?;
    Ok(ArchiveScan {
        index: index_bytes.ok_or("ZIP 根目录缺少 modrinth.index.json")?,
        common,
        client,
        directories,
        server_files,
        ignored_files,
        foreign_files,
    })
}
