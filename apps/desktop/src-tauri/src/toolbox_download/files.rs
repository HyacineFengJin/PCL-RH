//! Chosen-directory staging: Linux keeps its original anonymous FD backend;
//! Darwin/Windows retain a native stage and its private cleanup capability.
use super::*;
#[cfg(target_os = "linux")]
#[path = "files/linux.rs"]
mod native;
#[cfg(not(target_os = "linux"))]
#[path = "files/native.rs"]
mod native;
pub(crate) use native::*;
#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "files/native.rs"]
mod portable;

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 240
        || name.trim() != name
        || matches!(name, "." | "..")
        || name.chars().any(|ch| {
            ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err("下载文件名须为1..240字节的单个名称，不能含路径或无效字符".into());
    }
    Ok(())
}
