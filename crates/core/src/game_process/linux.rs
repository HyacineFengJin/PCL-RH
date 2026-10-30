//! Linux identities and pidfd cancellation; no signal targets a bare PID.
use super::Identity;
use std::{
    fs::File,
    io::Read,
    os::fd::{AsRawFd, FromRawFd},
};
fn bounded(path: &str, limit: u64) -> Result<String, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "无法读取监控进程身份")?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取监控进程身份")?;
    if bytes.len() as u64 > limit {
        return Err("监控进程身份超过上限".into());
    }
    String::from_utf8(bytes).map_err(|_| "监控进程身份格式无效".into())
}
pub(super) fn identity(pid: u32) -> Result<Option<Identity>, String> {
    if pid == 0 {
        return Err("监控进程身份无效".into());
    }
    let boot_id = bounded("/proc/sys/kernel/random/boot_id", 128)?
        .trim()
        .to_owned();
    if !(Identity {
        pid,
        start_time: 1,
        boot_id: boot_id.clone(),
    })
    .valid()
    {
        return Err("系统启动身份无效".into());
    }
    let text = match bounded(&format!("/proc/{pid}/stat"), 64 * 1024) {
        Ok(text) => text,
        Err(_) if !std::path::Path::new(&format!("/proc/{pid}")).exists() => return Ok(None),
        Err(error) => return Err(error),
    };
    let close = text.rfind(')').ok_or("监控进程身份格式无效")?;
    let fields = text[close + 1..].split_whitespace().collect::<Vec<_>>();
    if matches!(fields.first().copied(), Some("Z" | "X")) {
        return Ok(None);
    }
    let start_time = fields
        .get(19)
        .ok_or("监控进程身份格式无效")?
        .parse::<u64>()
        .map_err(|_| "监控进程身份格式无效")?;
    Ok(Some(Identity {
        pid,
        start_time,
        boot_id,
    }))
}
pub(super) fn terminate(expected: &Identity) -> Result<(), String> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid, 0) } as i32;
    if fd < 0 {
        return Err("无法安全连接游戏进程，请刷新进程状态".into());
    }
    let descriptor = unsafe { File::from_raw_fd(fd) };
    if !super::alive(expected)? {
        return Err("游戏进程身份已变化，未发送停止信号".into());
    }
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            descriptor.as_raw_fd(),
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } != 0
    {
        return Err("未能安全停止游戏进程，请刷新进程状态".into());
    }
    Ok(())
}
