//! New chosen files are staged anonymously in the chosen directory. A final
//! caller guard pins admission through link publication; an existing pathname
//! (including symlink) is never opened, overwritten or removed. Once published,
//! durability/late-edit errors become warnings because the saved file exists.
use super::ExportOutcome;
use crate::launcher_local::filesystem::Dir;
use std::{
    ffi::CString,
    fs::File,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
};

pub fn publish_new<G>(
    path: &Path,
    bytes: &[u8],
    extension: &str,
    count: usize,
    authorize: impl FnOnce() -> Result<G, String>,
) -> Result<ExportOutcome, String> {
    if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
        return Err("导出文件为空或超过 8 MiB 上限".into());
    }
    if path
        .extension()
        .and_then(|v| v.to_str())
        .is_none_or(|v| !v.eq_ignore_ascii_case(extension))
    {
        return Err(format!("保存文件应使用 .{extension} 扩展名"));
    }
    let parent = path.parent().ok_or("保存目录无效")?;
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("保存文件名无效")?;
    crate::launcher_local::filesystem::component(name)?;
    let folder = Dir::absolute(parent)?;
    let destination = CString::new(name).map_err(|_| "保存文件名无效")?;
    let mut existing = std::mem::MaybeUninit::<libc::stat>::uninit();
    let exists = unsafe {
        libc::fstatat(
            folder.0.as_raw_fd(),
            destination.as_ptr(),
            existing.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if exists == 0 {
        return Err("保存位置已有文件，不会覆盖，请选择新的文件名".into());
    }
    if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
        return Err("无法检查保存位置".into());
    }
    let fd = unsafe {
        libc::openat(
            folder.0.as_raw_fd(),
            c".".as_ptr(),
            libc::O_WRONLY | libc::O_TMPFILE | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err("保存目录不支持安全匿名暂存，请选择其他本地目录".into());
    }
    let mut staged = unsafe { File::from_raw_fd(fd) };
    staged
        .write_all(bytes)
        .map_err(|_| "无法写入导出暂存文件")?;
    staged.flush().map_err(|_| "无法写入导出暂存文件")?;
    staged.sync_all().map_err(|_| "无法保存导出暂存文件")?;
    let expected = staged.metadata().map_err(|_| "无法检查导出暂存文件")?;
    if expected.len() != bytes.len() as u64 {
        return Err("导出暂存长度已变化".into());
    }
    // The application admission guard is acquired after all bulk work, held
    // through publication. The caller also rechecks its opaque source selection.
    let _guard = authorize()?;
    if Dir::absolute(parent)?.identity()? != folder.identity()? {
        return Err("保存目录已被替换，请重新选择".into());
    }
    let source = CString::new(format!("/proc/self/fd/{}", staged.as_raw_fd())).unwrap();
    if unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            folder.0.as_raw_fd(),
            destination.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    } != 0
    {
        return Err("无法发布导出文件；已有文件不会覆盖".into());
    }
    let warning = folder
        .sync()
        .err()
        .map(|_| "文件已保存，但目录同步失败，请检查存储设备".into());
    // Reopening a published name cannot authorize cleanup. A late replacement
    // stays intact and is reported as a warning; the caller never deletes it.
    let current = folder.file(name);
    let same_file = current
        .ok()
        .flatten()
        .and_then(|v| v.metadata().ok())
        .is_some_and(|actual| {
            actual.dev() == expected.dev()
                && actual.ino() == expected.ino()
                && actual.len() == bytes.len() as u64
                && actual.mtime() == expected.mtime()
                && actual.mtime_nsec() == expected.mtime_nsec()
        });
    let same_folder = Dir::absolute(parent)
        .and_then(|v| v.identity())
        .ok()
        .is_some_and(|current| folder.identity().is_ok_and(|original| current == original));
    let warning = if same_file && same_folder {
        warning
    } else {
        Some("文件已发布，但保存位置随后发生变化；已有内容已保留".into())
    };
    Ok(ExportOutcome {
        status: "complete".into(),
        path: Some(path.display().to_string()),
        count,
        warning,
    })
}
