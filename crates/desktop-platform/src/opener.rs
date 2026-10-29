//! Invoke the default application with a literal target. Never interpolate a
//! URL/path into cmd, PowerShell, AppleScript or another command shell.
use std::path::Path;

pub fn open_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.chars().any(char::is_control)
    {
        return Err("网页地址无效".into());
    }
    open(std::ffi::OsStr::new(url))
}
pub fn open_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("打开文件需要绝对路径".into());
    }
    open(path.as_os_str())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open(target: &std::ffi::OsStr) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    // Every caller validates a URL or absolute path, so targets cannot become
    // options. Keep the established Linux default-app integration.
    std::process::Command::new(program)
        .arg(target)
        .spawn()
        .map_err(|_| "无法打开默认应用".to_owned())?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open(target: &std::ffi::OsStr) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let mut target: Vec<u16> = target.encode_wide().collect();
    if target.contains(&0) {
        return Err("打开目标包含无效字符".into());
    }
    target.push(0);
    // ShellExecute consumes the target directly as lpFile, not as shell code.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize <= 32 {
        Err("无法打开默认应用".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_targets_never_invoke_a_default_application() {
        for url in [
            "file:///private",
            "javascript:alert(1)",
            "--args",
            "https://example.invalid/\nsecret",
        ] {
            assert!(open_url(url).is_err());
        }
        assert!(open_path(Path::new("relative/file")).is_err());
    }
}
