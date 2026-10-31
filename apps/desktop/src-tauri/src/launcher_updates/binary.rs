//! Bounded native executable structure checks; these are not signature checks.
use super::model::Architecture;
#[cfg(not(target_os = "linux"))]
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};
pub(super) const PREFIX: usize = if cfg!(target_os = "linux") {
    64
} else {
    64 * 1024
};
pub(super) fn format() -> &'static str {
    if cfg!(windows) {
        "pe"
    } else if cfg!(target_os = "macos") {
        "mach-o"
    } else {
        "elf"
    }
}
pub(super) fn os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}
fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        b.get(at..at + 2).ok_or("程序头截断")?.try_into().unwrap(),
    ))
}
fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(at..at + 4).ok_or("程序头截断")?.try_into().unwrap(),
    ))
}
#[cfg(not(target_os = "linux"))]
fn u64_at(b: &[u8], at: usize) -> Result<u64, String> {
    Ok(u64::from_le_bytes(
        b.get(at..at + 8).ok_or("程序头截断")?.try_into().unwrap(),
    ))
}
fn cpu(a: &Architecture, x64: u32, arm: u32) -> Option<u32> {
    match a {
        Architecture::X86_64 => Some(x64),
        Architecture::Aarch64 => Some(arm),
        _ => None,
    }
}
pub(super) fn prefix(bytes: &[u8], architecture: &Architecture) -> Result<(), String> {
    match format() {
        "elf" => super::manifest::verify_elf(bytes, architecture),
        "pe" => pe_header(bytes, architecture).map(|_| ()),
        _ => macho_header(bytes, architecture).map(|_| ()),
    }
}
fn pe_header(b: &[u8], a: &Architecture) -> Result<(usize, usize, usize), String> {
    if b.get(..2) != Some(b"MZ") {
        return Err("更新资产不是PE可执行文件".into());
    }
    let at = u32_at(b, 60)? as usize;
    if at < 64
        || at > 64 * 1024 - 24
        || b.get(at..at + 4) != Some(b"PE\0\0")
        || Some(u16_at(b, at + 4)? as u32) != cpu(a, 0x8664, 0xaa64)
    {
        return Err("PE程序头或架构不匹配".into());
    }
    let sections = u16_at(b, at + 6)? as usize;
    let optional = u16_at(b, at + 20)? as usize;
    let flags = u16_at(b, at + 22)?;
    if !(1..=96).contains(&sections)
        || !(112..=4096).contains(&optional)
        || flags & 2 == 0
        || flags & 0x2000 != 0
        || u16_at(b, at + 24)? != 0x20b
    {
        return Err("更新PE不是受支持的64位程序".into());
    }
    let end = at
        .checked_add(24 + optional + sections * 40)
        .ok_or("PE程序头溢出")?;
    if end > b.len() || end > 64 * 1024 {
        return Err("PE程序头超过有界前缀".into());
    }
    Ok((at, sections, optional))
}
fn macho_header(b: &[u8], a: &Architecture) -> Result<(usize, usize), String> {
    if u32_at(b, 0)? != 0xfeedfacf
        || Some(u32_at(b, 4)?) != cpu(a, 0x01000007, 0x0100000c)
        || u32_at(b, 12)? != 2
    {
        return Err("更新资产不是匹配架构的薄Mach-O64可执行文件".into());
    }
    let commands = u32_at(b, 16)? as usize;
    let size = u32_at(b, 20)? as usize;
    if !(1..=4096).contains(&commands) || size < commands * 8 || size > 1024 * 1024 {
        return Err("Mach-O加载命令数量或大小越界".into());
    }
    Ok((commands, size))
}
#[cfg(not(target_os = "linux"))]
pub(super) fn verify(file: &mut File, architecture: &Architecture) -> Result<(), String> {
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut header = vec![0u8; (size.min(64 * 1024)) as usize];
    file.read_exact(&mut header).map_err(|_| "更新程序头截断")?;
    match format() {
        "pe" => {
            let (at, count, optional) = pe_header(&header, architecture)?;
            let entry = u32_at(&header, at + 24 + 16)? as u64;
            let image = u32_at(&header, at + 24 + 56)? as u64;
            let heads = u32_at(&header, at + 24 + 60)? as u64;
            if entry == 0
                || entry >= image
                || heads > size
                || heads < (at + 24 + optional + count * 40) as u64
            {
                return Err("PE入口或映像边界无效".into());
            }
            let mut executable = false;
            for n in 0..count {
                let p = at + 24 + optional + n * 40;
                let virtual_size = u32_at(&header, p + 8)? as u64;
                let address = u32_at(&header, p + 12)? as u64;
                let raw_size = u32_at(&header, p + 16)? as u64;
                let offset = u32_at(&header, p + 20)? as u64;
                if offset.checked_add(raw_size).is_none_or(|n| n > size)
                    || address
                        .checked_add(virtual_size.max(raw_size))
                        .is_none_or(|n| n > image)
                {
                    return Err("PE section超过文件或映像边界".into());
                }
                if u32_at(&header, p + 36)? & 0x20000000 != 0
                    && entry >= address
                    && entry < address + virtual_size.max(raw_size)
                {
                    executable = true;
                }
            }
            if !executable {
                return Err("PE入口不属于可执行section".into());
            }
        }
        "mach-o" => {
            let (count, bytes) = macho_header(&header, architecture)?;
            if 32u64 + bytes as u64 > size {
                return Err("Mach-O加载命令超过文件边界".into());
            }
            file.seek(SeekFrom::Start(32)).map_err(|e| e.to_string())?;
            let mut commands = vec![0u8; bytes];
            file.read_exact(&mut commands)
                .map_err(|_| "Mach-O加载命令截断")?;
            let mut at = 0usize;
            let mut entry = None;
            let mut segments = Vec::new();
            for _ in 0..count {
                let kind = u32_at(&commands, at)?;
                let length = u32_at(&commands, at + 4)? as usize;
                let end = at
                    .checked_add(length)
                    .filter(|n| *n <= bytes)
                    .ok_or("Mach-O命令溢出")?;
                if length < 8 || length % 8 != 0 {
                    return Err("Mach-O命令长度无效".into());
                }
                let command = &commands[at..end];
                if kind == 0x19 {
                    if length < 72 {
                        return Err("Mach-O segment截断".into());
                    }
                    let address = u64_at(command, 24)?;
                    let memory = u64_at(command, 32)?;
                    let offset = u64_at(command, 40)?;
                    let length = u64_at(command, 48)?;
                    let sections = u32_at(command, 64)? as usize;
                    if 72usize
                        .checked_add(sections.checked_mul(80).ok_or("Mach-O section溢出")?)
                        .is_none_or(|n| n > command.len())
                        || address.checked_add(memory).is_none()
                        || offset.checked_add(length).is_none_or(|n| n > size)
                        || length > memory
                    {
                        return Err("Mach-O segment/section超过文件边界".into());
                    }
                    if u32_at(command, 60)? & 4 != 0 {
                        segments.push((offset, offset + length));
                    }
                } else if kind == 0x80000028 {
                    if length < 24 || entry.replace(u64_at(command, 8)?).is_some() {
                        return Err("Mach-O入口命令重复或截断".into());
                    }
                }
                at = end;
            }
            if at != bytes
                || entry.is_none_or(|entry| {
                    entry == 0 || !segments.iter().any(|(a, b)| entry >= *a && entry < *b)
                })
            {
                return Err("Mach-O入口不属于可执行文件segment".into());
            }
        }
        _ => return Err("此完整文件检查仅用于原生PE/Mach-O".into()),
    }
    Ok(())
}
