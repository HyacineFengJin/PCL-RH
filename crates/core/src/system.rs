//! Read-only physical memory snapshots for heap planning. These are volatile
//! host estimates, not reservations or process/cgroup allocation guarantees.
//! No shell command, external utility or persistent system change is involved.
use crate::memory::MemorySnapshot;

fn snapshot(total_bytes: u64, available_bytes: u64) -> Result<MemorySnapshot, String> {
    if total_bytes == 0 {
        return Err("系统总内存无效".into());
    }
    Ok(MemorySnapshot {
        total_bytes,
        // Native counters may be sampled independently while memory changes.
        available_bytes: available_bytes.min(total_bytes),
    })
}

/// Collect a fresh estimate using the native host interface. Failure remains
/// explicit so automatic heap planning cannot reuse a stale/manual value.
pub fn memory() -> Result<MemorySnapshot, String> {
    native_memory()
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_memory(text: &str) -> Result<MemorySnapshot, String> {
    let value = |key: &str| {
        text.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != key {
                return None;
            }
            let count = fields.next()?.parse::<u64>().ok()?;
            if fields.next()? != "kB" || fields.next().is_some() {
                return None;
            }
            count.checked_mul(1024)
        })
    };
    snapshot(
        value("MemTotal:").ok_or("无法读取系统总内存")?,
        value("MemAvailable:").ok_or("无法读取系统可用内存")?,
    )
}

#[cfg(target_os = "linux")]
fn native_memory() -> Result<MemorySnapshot, String> {
    let text = std::fs::read_to_string("/proc/meminfo")
        .map_err(|error| format!("无法读取系统内存信息：{error}"))?;
    parse_linux_memory(&text)
}

#[cfg(target_os = "windows")]
fn native_memory() -> Result<MemorySnapshot, String> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(format!(
            "无法读取系统内存信息：{}",
            std::io::Error::last_os_error()
        ));
    }
    // Physical available RAM includes standby/free/zero pages; page-file and
    // virtual-address-space counters are not physical heap headroom.
    snapshot(status.ullTotalPhys, status.ullAvailPhys)
}

#[cfg(any(target_os = "macos", test))]
fn macos_memory(
    total: u64,
    free_pages: u64,
    inactive_pages: u64,
    page_size: u64,
) -> Result<MemorySnapshot, String> {
    if page_size == 0 {
        return Err("系统内存页大小无效".into());
    }
    // Darwin already includes speculative pages in free_count. Inactive pages
    // are reclaimable; adding speculative/purgeable again can double count.
    // Compressed/uncompressed compressor counters are not available RAM.
    let available = free_pages
        .checked_add(inactive_pages)
        .and_then(|pages| pages.checked_mul(page_size))
        .ok_or("系统可用内存计数溢出")?;
    snapshot(total, available)
}

#[cfg(target_os = "macos")]
fn native_memory() -> Result<MemorySnapshot, String> {
    // libc provides the Mach statistics layout but not these two stable Mach
    // functions. Keep the declarations in the native adapter, using libc types.
    unsafe extern "C" {
        fn host_page_size(host: libc::host_t, size: *mut libc::vm_size_t) -> libc::kern_return_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
    }
    struct Host(libc::host_t);
    impl Drop for Host {
        fn drop(&mut self) {
            unsafe { mach_port_deallocate(libc::mach_task_self(), self.0) };
        }
    }

    let mut total = 0u64;
    let mut length = std::mem::size_of::<u64>();
    if unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut total as *mut u64).cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(format!(
            "无法读取系统总内存：{}",
            std::io::Error::last_os_error()
        ));
    }
    if length != std::mem::size_of::<u64>() {
        return Err("系统总内存计数长度无效".into());
    }
    // mach_host_self returns an owned send right. Release it on every path;
    // repeated previews must not grow the task's Mach port references.
    #[allow(deprecated)] // Existing locked libc exposes this stable Mach call.
    let host = Host(unsafe { libc::mach_host_self() });
    if host.0 == libc::MACH_PORT_NULL as libc::mach_port_t {
        return Err("无法取得系统内存统计端口".into());
    }
    let mut page_size: libc::vm_size_t = 0;
    let result = unsafe { host_page_size(host.0, &mut page_size) };
    if result != libc::KERN_SUCCESS {
        return Err(format!("无法读取系统内存页大小：Mach {result}"));
    }
    let mut statistics: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let result = unsafe {
        libc::host_statistics64(
            host.0,
            libc::HOST_VM_INFO64,
            (&mut statistics as *mut libc::vm_statistics64).cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return Err(format!("无法读取系统可用内存：Mach {result}"));
    }
    // Only the original four-word prefix is used. New libc versions can have
    // trailing fields that older macOS kernels do not return.
    if count < 4 || count > libc::HOST_VM_INFO64_COUNT {
        return Err("系统内存统计长度无效".into());
    }
    macos_memory(
        total,
        u64::from(statistics.free_count),
        u64::from(statistics.inactive_count),
        page_size as u64,
    )
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn native_memory() -> Result<MemorySnapshot, String> {
    Err("当前系统不支持原生内存信息查询".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_uses_available_physical_memory_and_exact_kibibytes() {
        let info = parse_linux_memory(
            "MemTotal: 8192 kB\nMemFree: 10 kB\nMemAvailable: 4096 kB\nSwapFree: 99999 kB\n",
        )
        .unwrap();
        assert_eq!(info.total_bytes, 8192 * 1024);
        assert_eq!(info.available_bytes, 4096 * 1024);
        for invalid in [
            "MemTotal: 8192 kB",
            "MemTotal: 0 kB\nMemAvailable: 0 kB",
            "MemTotal: 8192 MB\nMemAvailable: 1 kB",
            "MemTotal: 8192 kB\nMemAvailable: 1 kB extra",
            "MemTotal: 18446744073709551615 kB\nMemAvailable: 1 kB",
        ] {
            assert!(parse_linux_memory(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn darwin_counts_reclaimable_pages_with_native_page_size() {
        for page_size in [4096, 16384] {
            let info = macos_memory(1024 * 1024 * 1024, 10, 20, page_size).unwrap();
            assert_eq!(info.available_bytes, 30 * page_size);
        }
        assert!(macos_memory(1024, 1, 1, 0).is_err());
        assert!(macos_memory(u64::MAX, u64::MAX, 1, 1).is_err());
        assert!(macos_memory(u64::MAX, 1, 1, u64::MAX).is_err());
    }

    #[test]
    fn native_counters_cannot_exceed_physical_total() {
        assert_eq!(snapshot(1024, 2048).unwrap().available_bytes, 1024);
        assert!(snapshot(0, 0).is_err());
    }

    #[test]
    fn native_memory_snapshot_has_valid_physical_bounds() {
        let info = memory().unwrap();
        assert!(info.total_bytes > 0);
        assert!(info.available_bytes <= info.total_bytes);
    }
}
