//! proc_pidinfo provides observation only. There is deliberately no bare-PID
//! signalling API; only the monitor's unreaped `Owner` may stop a macOS game.
use super::Identity;
use std::{io, mem::size_of};

fn boot_id() -> Result<String, String> {
    let mut bytes = [0u8; 128];
    let mut length = bytes.len();
    if unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || length > bytes.len()
    {
        return Err("无法读取系统启动身份".into());
    }
    let bytes = &bytes[..length];
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    let value = std::str::from_utf8(bytes)
        .map_err(|_| "系统启动身份无效")?
        .to_ascii_lowercase();
    if !(Identity {
        pid: 1,
        start_time: 1,
        boot_id: value.clone(),
    })
    .valid()
    {
        return Err("系统启动身份无效".into());
    }
    Ok(value)
}
pub(super) fn identity(pid: u32) -> Result<Option<Identity>, String> {
    let boot_id = boot_id()?;
    let mut information = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            information.as_mut_ptr().cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if result <= 0 {
        return match io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH | libc::ENOENT) => Ok(None),
            _ => Err("无法读取监控进程身份".into()),
        };
    }
    if result as usize != size_of::<libc::proc_bsdinfo>() {
        return Err("监控进程身份格式无效".into());
    }
    let information = unsafe { information.assume_init() };
    if information.pbi_pid != pid || information.pbi_status == libc::SZOMB as u32 {
        return Ok(None);
    }
    let start_time = information
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(information.pbi_start_tvusec))
        .filter(|value| *value > 0)
        .ok_or("监控进程身份格式无效")?;
    Ok(Some(Identity {
        pid,
        start_time,
        boot_id,
    }))
}
