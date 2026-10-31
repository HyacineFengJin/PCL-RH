//! Release contract is intentionally small and contains no download URLs.
//! The manifest and executable must be unique uploaded assets of the same
//! official repository release; the tag API binds its commit independently.
use super::{
    model::Architecture,
    provider::{hex, Asset, Release, MAX_BINARY_BYTES, MAX_MANIFEST_BYTES, REPOSITORY},
};
use semver::Version;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub schema_version: u32,
    pub repository: String,
    pub version: String,
    pub tag: String,
    pub commit: String,
    pub artifacts: Vec<Artifact>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    #[serde(default)]
    pub os: Option<String>,
    pub name: String,
    pub architecture: Architecture,
    pub format: String,
    pub size: u64,
    pub sha256: String,
}

pub(super) fn parse_manifest(
    bytes: &[u8],
    release: &Release,
    version: &Version,
    architecture: &Architecture,
) -> Result<(Manifest, Artifact, Asset), String> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err("更新清单超过 64 KiB 限制".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|_| "更新清单格式无效或包含不支持的字段")?;
    if !matches!(manifest.schema_version, 1 | 2)
        || manifest.repository != REPOSITORY
        || manifest.tag != release.tag_name
        || manifest.version != version.to_string()
        || !hex(&manifest.commit, 40)
        || manifest.artifacts.is_empty()
        || manifest.artifacts.len() > if manifest.schema_version == 1 { 2 } else { 6 }
    {
        return Err("更新清单的项目、版本、标签或提交不匹配".into());
    }
    let mut names = std::collections::HashSet::new();
    for artifact in &manifest.artifacts {
        let valid_target = if manifest.schema_version == 1 {
            artifact.os.is_none()
                && artifact.format == "elf"
                && match artifact.architecture {
                    Architecture::X86_64 => artifact.name == "pcl-desktop-linux-x86_64",
                    Architecture::Aarch64 => artifact.name == "pcl-desktop-linux-aarch64",
                    _ => false,
                }
        } else {
            let cpu = match artifact.architecture {
                Architecture::X86_64 => "x86_64",
                Architecture::Aarch64 => "aarch64",
                _ => return Err("更新清单架构无效".into()),
            };
            match artifact.os.as_deref() {
                Some("linux") => {
                    artifact.format == "elf" && artifact.name == format!("pcl-desktop-linux-{cpu}")
                }
                Some("macos") => {
                    artifact.format == "mach-o"
                        && artifact.name == format!("pcl-desktop-macos-{cpu}")
                }
                Some("windows") => {
                    artifact.format == "pe"
                        && artifact.name == format!("pcl-desktop-windows-{cpu}.exe")
                }
                _ => false,
            }
        };
        if !valid_target
            || artifact.size < 64
            || artifact.size > MAX_BINARY_BYTES
            || !hex(&artifact.sha256, 64)
            || !names.insert(&artifact.name)
        {
            return Err("更新清单的 Linux 资产格式、大小、校验值或唯一性无效".into());
        }
    }
    if manifest.schema_version == 1 {
        let raw: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| "更新清单格式无效")?;
        if raw["artifacts"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item.as_object()
                    .is_some_and(|object| object.contains_key("os"))
            })
        }) {
            return Err("Linux schema1清单不接受原生OS扩展字段".into());
        }
    }
    let artifact = manifest
        .artifacts
        .iter()
        .find(|artifact| {
            &artifact.architecture == architecture
                && architecture.artifact_name() == Some(artifact.name.as_str())
                && if manifest.schema_version == 1 {
                    super::binary::os() == "linux"
                } else {
                    artifact.os.as_deref() == Some(super::binary::os())
                }
        })
        .cloned()
        .ok_or("发布不包含当前 Linux 架构的更新资产")?;
    let asset = unique_asset(release, &artifact.name)?.clone();
    if asset.size != artifact.size {
        return Err("发布资产大小与更新清单不一致".into());
    }
    if let Some(digest) = &asset.digest {
        if digest != &format!("sha256:{}", artifact.sha256.to_ascii_lowercase()) {
            return Err("GitHub 资产校验值与更新清单不一致".into());
        }
    }
    Ok((manifest, artifact, asset))
}

pub(super) fn unique_asset<'a>(release: &'a Release, name: &str) -> Result<&'a Asset, String> {
    let mut matching = release.assets.iter().filter(|asset| asset.name == name);
    let asset = matching
        .next()
        .ok_or("发布缺少受支持的 Linux 更新清单或资产")?;
    if matching.next().is_some() || asset.id == 0 || asset.state != "uploaded" {
        return Err("发布更新资产重复、尚未上传完成或标识无效".into());
    }
    Ok(asset)
}

pub(super) fn verify_elf(header: &[u8], architecture: &Architecture) -> Result<(), String> {
    // ELF64 little endian, current ELF version, executable or PIE/shared ELF.
    // Architecture is verified again after the full SHA256 download completes.
    if header.len() < 64
        || &header[..4] != b"\x7fELF"
        || header[4..7] != [2, 1, 1]
        || !matches!(header[7], 0 | 3)
        || !matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
        || u32::from_le_bytes(header[20..24].try_into().unwrap()) != 1
        || u16::from_le_bytes([header[52], header[53]]) != 64
        || u16::from_le_bytes([header[54], header[55]]) != 56
        || !matches!(u16::from_le_bytes([header[56], header[57]]), 1..=128)
        || u64::from_le_bytes(header[24..32].try_into().unwrap()) == 0
    {
        return Err("更新资产不是受支持的 Linux ELF64 可执行文件".into());
    }
    let machine = u16::from_le_bytes([header[18], header[19]]);
    if !matches!(
        (architecture, machine),
        (Architecture::X86_64, 62) | (Architecture::Aarch64, 183)
    ) {
        return Err("更新 ELF 资产的架构与当前启动器不一致".into());
    }
    let program_offset = u64::from_le_bytes(header[32..40].try_into().unwrap());
    if program_offset < 64 || program_offset > MAX_BINARY_BYTES - 56 {
        return Err("更新 ELF 程序头偏移无效".into());
    }
    Ok(())
}
