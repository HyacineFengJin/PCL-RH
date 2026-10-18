//! Known launcher display/migration metadata may accompany game overrides.
//! These exact bounded documents are validated then omitted, never installed
//! or interpreted as launcher policy. Other reserved paths remain errors.
use super::Result;
use serde::Deserialize;
use std::collections::BTreeSet;

pub(super) const MAX_BYTES: u64 = 64 * 1024;

pub(super) fn candidate(path: &str, directory: bool) -> bool {
    (directory && path == "PCL")
        || (!directory && matches!(path, "PCL/config.json" | "PCL/Setup.ini"))
}

pub(super) fn archive_candidate(path: &str) -> bool {
    ["overrides/", "client-overrides/", "server-overrides/"]
        .iter()
        .any(|layer| {
            let suffixes = [
                format!("{layer}PCL/config.json"),
                format!("{layer}PCL/Setup.ini"),
            ];
            suffixes
                .iter()
                .any(|suffix| path == suffix || path.ends_with(&format!("/{suffix}")))
        })
}

pub(super) fn warning(count: usize) -> String {
    format!("已忽略 {count} 个已识别的 PCL 启动器显示或迁移元数据文件；原启动器设置不会应用")
}

pub(super) fn validate(path: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("整合包 PCL 元数据超过 64 KiB 限制".into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "整合包 PCL 元数据必须是 UTF-8")?
        .trim_start_matches('\u{feff}');
    match path {
        "PCL/config.json" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Migration {
                #[serde(rename = "InstanceMigratedJava")]
                _migrated: bool,
            }
            serde_json::from_str::<Migration>(text)
                .map_err(|_| "整合包 PCL config.json 包含未知设置或无效迁移元数据")?;
        }
        "PCL/Setup.ini" => {
            let mut keys = BTreeSet::new();
            for line in text.lines().filter(|line| !line.is_empty()) {
                let (key, value) = line
                    .split_once(':')
                    .ok_or("整合包 PCL Setup.ini 元数据格式无效")?;
                if !keys.insert(key)
                    || keys.len() > 32
                    || value.len() > 2048
                    || value.chars().any(char::is_control)
                {
                    return Err("整合包 PCL Setup.ini 元数据重复、无效或过长".into());
                }
                let valid = match key {
                    "State" => value.parse::<u8>().is_ok_and(|value| value <= 9),
                    "IsStar" | "VersionLiteLoader" | "VersionArgumentIndieV2" => {
                        matches!(value, "True" | "False")
                    }
                    "VersionVanillaName" | "VersionVanilla" | "VersionForge" => {
                        !value.is_empty()
                            && value.len() <= 160
                            && value
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || b".-_+".contains(&byte))
                    }
                    "Info" | "Logo" | "ReleaseTime" => true,
                    _ => false,
                };
                if !valid {
                    return Err("整合包 PCL Setup.ini 包含未知设置或无效显示元数据".into());
                }
            }
            if keys.is_empty() {
                return Err("整合包 PCL Setup.ini 显示元数据为空".into());
            }
        }
        _ => return Err("整合包包含未识别的启动器元数据".into()),
    }
    Ok(())
}
