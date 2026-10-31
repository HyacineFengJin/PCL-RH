//! Native publication behind the captured chooser authority. Linux retains its
//! anonymous inode implementation; Darwin/Windows stage in a private workspace.
#[cfg(target_os = "linux")]
#[path = "files/linux.rs"]
mod native;
#[cfg(not(target_os = "linux"))]
#[path = "files/native.rs"]
mod native;
pub(crate) use native::*;

// Type-check the real portable implementation in Linux fixtures as well. Its
// native runtime behavior is exercised by the same wrapper on Darwin/Windows.
#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "files/native.rs"]
mod portable_compile;

// Same closed filename validation used by the official provider. Local native
// Leaf validation adds the target platform's literal-name constraints.
fn file_name(value: &str) -> Result<(), String> {
    pcl_core::identifier(value)?;
    if value.len() > 255 || value.trim() != value || value.chars().any(char::is_control) {
        return Err("Modrinth文件名无效".into());
    }
    Ok(())
}
