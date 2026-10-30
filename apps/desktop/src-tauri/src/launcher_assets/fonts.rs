//! Read-only native font-family discovery with bounded output and delivery.
//! Linux retains its fixed fontconfig executable and owned process group.
//! CoreText/GDI run in one admitted worker; a timeout cannot cancel native code
//! and never releases that worker's lease before the native call returns.
use std::{collections::BTreeSet, time::Duration};
#[cfg(target_os = "macos")]
#[path = "fonts_macos.rs"]
mod native;
#[cfg(windows)]
#[path = "fonts_windows.rs"]
mod native;
#[cfg(target_os = "linux")]
use std::{
    io::Read,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::Instant,
};

const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_FAMILIES: usize = 4096;
pub fn discover_font_families() -> Result<Vec<String>, String> {
    #[cfg(target_os = "linux")]
    {
        discover(Path::new("/usr/bin/fc-list"), Duration::from_secs(3))
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        static FLIGHT: std::sync::OnceLock<std::sync::Arc<std::sync::atomic::AtomicBool>> =
            std::sync::OnceLock::new();
        bounded_native(
            FLIGHT
                .get_or_init(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))
                .clone(),
            Duration::from_secs(3),
            native::enumerate,
        )
    }
}

#[cfg(target_os = "linux")]
fn discover(executable: &Path, timeout: Duration) -> Result<Vec<String>, String> {
    let mut command = Command::new(executable);
    command
        .arg("--format=%{family}\n")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|_| "无法运行 fontconfig，请安装系统字体工具")?;
    let mut child = ProbeChild {
        group: child.id() as i32,
        child: Some(child),
    };
    let mut stdout = child
        .child
        .as_mut()
        .unwrap()
        .stdout
        .take()
        .ok_or("无法读取 fontconfig 输出")?;
    let mut stderr = child
        .child
        .as_mut()
        .unwrap()
        .stderr
        .take()
        .ok_or("无法读取 fontconfig 错误输出")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let deadline = Instant::now() + timeout;
    let status = loop {
        drain(&mut stdout, &mut output, MAX_OUTPUT, deadline)?;
        drain(&mut stderr, &mut errors, 64 * 1024, deadline)?;
        match child.child.as_mut().unwrap().try_wait() {
            Ok(Some(status)) => {
                drain(&mut stdout, &mut output, MAX_OUTPUT, deadline)?;
                drain(&mut stderr, &mut errors, 64 * 1024, deadline)?;
                break status;
            }
            Err(_) => return Err("无法等待 fontconfig 任务".into()),
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return Err("fontconfig 字体扫描超时，请检查系统字体配置".into());
        }
        thread::sleep(Duration::from_millis(10));
    };
    // Nonblocking pipes never wait for a descendant to close a copied writer.
    // Stop the process group, then drop both pipes before returning font data.
    child.stop();
    if !status.success() {
        return Err("fontconfig 无法完成字体扫描".into());
    }
    families(&output)
}
#[cfg(target_os = "linux")]
fn nonblocking(file: &impl AsRawFd) -> Result<(), String> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err("无法设置 fontconfig 输出读取方式".into())
    } else {
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn drain(
    reader: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    deadline: Instant,
) -> Result<(), String> {
    let mut buffer = [0u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Err("fontconfig 字体扫描超时，请检查系统字体配置".into());
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(length) => {
                if bytes.len() + length > limit {
                    return Err("fontconfig 输出超过安全上限".into());
                }
                bytes.extend_from_slice(&buffer[..length]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("无法读取 fontconfig 输出".into()),
        }
    }
}

#[cfg(target_os = "linux")]
struct ProbeChild {
    group: i32,
    child: Option<Child>,
}
#[cfg(target_os = "linux")]
impl ProbeChild {
    fn stop(&mut self) {
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // A killed process normally reaps immediately. Keep a rare kernel
            // I/O stall out of the caller while retaining one explicit reaper.
            let until = Instant::now() + Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(10)),
                    Ok(None) => {
                        thread::spawn(move || {
                            let _ = child.wait();
                        });
                        break;
                    }
                }
            }
        }
    }
}
#[cfg(target_os = "linux")]
impl Drop for ProbeChild {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.stop();
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn families(bytes: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "fontconfig 输出不是有效 UTF-8")?;
    let mut families = BTreeSet::new();
    for family in text.lines().flat_map(|line| line.split(',')) {
        insert_family(&mut families, family)?;
    }
    Ok(families.into_iter().collect())
}

fn insert_family(families: &mut BTreeSet<String>, family: &str) -> Result<(), String> {
    let family = family.trim();
    if family.is_empty() {
        return Ok(());
    }
    if family.chars().count() > 128
        || family.chars().any(char::is_control)
        || family.contains([';', '{', '}', '<', '>', '\\'])
    {
        return Err("字体枚举包含无效字体名称".into());
    }
    families.insert(family.to_owned());
    if families.len() > MAX_FAMILIES {
        return Err("字体数量超过 4096 个上限".into());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows, test))]
#[derive(Default)]
struct NativeFamilies {
    families: BTreeSet<String>,
    bytes: usize,
}
#[cfg(any(target_os = "macos", windows, test))]
impl NativeFamilies {
    fn push(&mut self, family: &str) -> Result<(), String> {
        // Account for both the native UTF-16 payload and its UTF-8 rendering,
        // including duplicates and a separator. Empty/duplicate callbacks cannot
        // evade the total budget; neither representation can exceed 1 MiB.
        let native_bytes = family
            .encode_utf16()
            .count()
            .checked_mul(2)
            .ok_or("系统字体枚举字节计数溢出")?;
        self.bytes = self
            .bytes
            .checked_add(family.len().max(native_bytes))
            .and_then(|n| n.checked_add(1))
            .ok_or("系统字体枚举字节计数溢出")?;
        if self.bytes > MAX_OUTPUT {
            return Err("系统字体枚举超过 1 MiB 上限".into());
        }
        insert_family(&mut self.families, family)
    }
    fn finish(self) -> Vec<String> {
        self.families.into_iter().collect()
    }
}

#[cfg(any(target_os = "macos", windows, test))]
fn bounded_native(
    flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    timeout: Duration,
    enumerate: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static,
) -> Result<Vec<String>, String> {
    use std::sync::{atomic::Ordering, mpsc};
    if flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("系统字体枚举仍在进行，请等待原任务返回".into());
    }
    struct Lease(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let lease = Lease(flight);
    let (sender, receiver) = mpsc::sync_channel(1);
    let _worker = std::thread::Builder::new()
        .name("pcl-font-catalog".into())
        .spawn(move || {
            // This owner remains alive across an uninterruptible native call and
            // across caller timeout/disconnect. No replacement worker is admitted.
            let _lease = lease;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(enumerate))
                .unwrap_or_else(|_| Err("系统字体枚举工作线程异常".into()));
            let _ = sender.send(result);
        })
        .map_err(|_| "无法启动系统字体枚举工作线程")?;
    receiver
        .recv_timeout(timeout)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                "系统字体枚举超时；原生任务返回前不会启动新任务".to_string()
            }
            mpsc::RecvTimeoutError::Disconnected => "系统字体枚举工作线程未返回结果".to_string(),
        })?
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Script(std::path::PathBuf);
    impl Script {
        fn new(body: &str) -> Self {
            let folder = std::env::current_dir()
                .unwrap()
                .join("fixtures")
                .join(format!(
                    "fonts-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(&folder).unwrap();
            let path = folder.join("fake-fontconfig");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }
    impl Drop for Script {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }
    #[test]
    fn font_families_are_bounded_deduplicated_and_utf8() {
        assert_eq!(
            families("字体 A,Family B\nFamily B\n".as_bytes()).unwrap(),
            vec!["Family B", "字体 A"]
        );
        assert!(families(b"invalid\xff").is_err());
        assert!(families(b"Family;injection").is_err());
    }
    #[test]
    fn failing_or_timed_out_probe_returns_no_partial_list() {
        let failed = Script::new("printf 'Partial Font\\n'; exit 1");
        assert!(discover(&failed.0, Duration::from_millis(200)).is_err());
        let slow = Script::new("sleep 30");
        let started = Instant::now();
        assert!(discover(&slow.0, Duration::from_millis(60)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn descendant_pipe_holders_are_stopped_even_when_leader_exits() {
        let script = Script::new("sleep 30 &\nprintf 'Family A\\n'\nexit 0");
        let started = Instant::now();
        assert_eq!(
            discover(&script.0, Duration::from_millis(200)).unwrap(),
            vec!["Family A"]
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn probe_output_is_bounded_before_parsing_names() {
        let script = Script::new("head -c 1048577 /dev/zero");
        let error = discover(&script.0, Duration::from_secs(1)).unwrap_err();
        assert!(error.contains("超过安全上限"));
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    };
    #[test]
    fn native_names_keep_literal_commas_unicode_and_reject_unsafe_names() {
        let mut catalog = NativeFamilies::default();
        catalog.push("字族 A").unwrap();
        catalog.push("Family, Literal").unwrap();
        catalog.push("字族 A").unwrap();
        assert_eq!(catalog.finish(), vec!["Family, Literal", "字族 A"]);
        for invalid in [
            "Bad;css",
            "Bad\\path",
            "Bad{css}",
            "Bad<markup>",
            "Bad\0name",
            "Bad\nname",
        ] {
            assert!(NativeFamilies::default().push(invalid).is_err());
        }
        assert!(NativeFamilies::default().push(&"字".repeat(129)).is_err());
        assert!(families(b"invalid\xff").is_err());
    }
    #[test]
    fn native_delivery_counts_duplicates_and_bounds_unique_families() {
        let mut catalog = NativeFamilies::default();
        for index in 0..MAX_FAMILIES {
            catalog.push(&format!("Family {index}")).unwrap();
        }
        assert!(catalog.push("One Too Many").is_err());
        let mut catalog = NativeFamilies::default();
        let name = "A".repeat(128);
        for _ in 0..MAX_OUTPUT / 257 {
            catalog.push(&name).unwrap();
        }
        assert!(catalog.push(&name).is_err());
    }
    #[test]
    fn native_timeout_retains_single_worker_lease_until_actual_return() {
        let flight = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, wait) = mpsc::channel();
        let count = calls.clone();
        let result = bounded_native(flight.clone(), Duration::from_millis(20), move || {
            count.fetch_add(1, Ordering::SeqCst);
            wait.recv().unwrap();
            Ok(vec!["Old result".into()])
        });
        assert!(result.unwrap_err().contains("超时"));
        assert!(flight.load(Ordering::Acquire));
        let count = calls.clone();
        assert!(
            bounded_native(flight.clone(), Duration::from_millis(20), move || {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(vec![])
            })
            .unwrap_err()
            .contains("仍在进行")
        );
        release.send(()).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while flight.load(Ordering::Acquire) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!flight.load(Ordering::Acquire));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            bounded_native(flight, Duration::from_secs(1), || Ok(vec![
                "New result".into()
            ]))
            .unwrap(),
            vec!["New result"]
        );
    }
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn actual_native_catalog_is_bounded_sorted_and_valid_without_a_window() {
        let families = discover_font_families().expect("actual native system font enumeration");
        assert!(
            !families.is_empty(),
            "native runner has no system font families"
        );
        assert!(families.len() <= MAX_FAMILIES);
        assert!(families.windows(2).all(|pair| pair[0] < pair[1]));
        let mut verified = NativeFamilies::default();
        for family in &families {
            verified.push(family).unwrap();
        }
        assert_eq!(families, verified.finish());
    }
}
