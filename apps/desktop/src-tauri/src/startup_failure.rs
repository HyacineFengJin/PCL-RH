//! Fatal desktop-entry diagnostics. Helper/monitor/build-info modes return
//! before this module is called and retain their existing pipe protocols.
use std::{fmt::Display, io, io::Write};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Stage {
    Project,
    DownloadPolicy,
    Updater,
    Desktop,
}

impl Stage {
    fn message(self) -> &'static str {
        match self {
            Self::Project => {
                "无法定位或访问启动器资源。请检查便携发行包及目录访问权限。\nLauncher resources could not be located or accessed. Check the portable distribution and directory permissions."
            }
            Self::DownloadPolicy => {
                "下载策略初始化失败，启动器无法继续启动。\nThe download policy could not initialize. PCL RH cannot start."
            }
            Self::Updater => {
                "更新服务初始化失败，启动器无法安全启动。\nThe update service could not initialize. PCL RH cannot start safely."
            }
            Self::Desktop => {
                "桌面界面初始化或运行失败，启动器已停止。\nThe desktop interface could not initialize or run. PCL RH has stopped."
            }
        }
    }
}

const TITLE: &str = "PCL RH 启动失败 / Startup failed";

fn report(
    stage: Stage,
    cause: impl Display,
    stderr: &mut impl Write,
    present: impl FnOnce(&str, &str) -> io::Result<()>,
) {
    // Explorer may supply no stderr. A failed diagnostic write must not prevent
    // the native dialog; detailed errors are never passed to that dialog.
    let _ = writeln!(stderr, "PCL RH startup failed ({stage:?}): {cause}");
    if let Err(error) = present(TITLE, stage.message()) {
        let _ = writeln!(stderr, "PCL RH startup error dialog failed: {error}");
    }
}

#[cfg(windows)]
fn present(title: &str, message: &str) -> io::Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let title = title.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let message = message.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    // Both buffers are NUL-terminated and retained throughout this synchronous
    // call. No Tauri window or console is needed during early initialization.
    if unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn present(_title: &str, _message: &str) -> io::Result<()> {
    Ok(())
}

pub(crate) fn exit(stage: Stage, cause: impl Display) -> ! {
    report(stage, cause, &mut std::io::stderr().lock(), present);
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialogs_use_fixed_stage_messages_while_stderr_retains_the_cause() {
        let cause = "secret-key=private-value; unknown argument; /private/user/path";
        for stage in [
            Stage::Project,
            Stage::DownloadPolicy,
            Stage::Updater,
            Stage::Desktop,
        ] {
            let mut stderr = Vec::new();
            let mut presented = false;
            report(stage, cause, &mut stderr, |title, message| {
                assert_eq!(title, TITLE);
                assert_eq!(message, stage.message());
                assert!(!message.contains(cause));
                for private in [
                    "secret-key",
                    "private-value",
                    "unknown argument",
                    "/private",
                ] {
                    assert!(!message.contains(private));
                    assert!(!title.contains(private));
                }
                assert!(message.contains('\n'));
                presented = true;
                Ok(())
            });
            assert!(presented);
            assert!(String::from_utf8(stderr).unwrap().contains(cause));
        }
    }

    #[test]
    fn broken_stderr_does_not_suppress_presentation() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        let mut presented = false;
        report(Stage::Project, "failure", &mut Broken, |_, _| {
            presented = true;
            Ok(())
        });
        assert!(presented);
    }

    #[test]
    fn native_presentation_failure_is_recorded_without_panicking() {
        let mut stderr = Vec::new();
        report(Stage::Desktop, "original failure", &mut stderr, |_, _| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native failure",
            ))
        });
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("original failure"));
        assert!(stderr.contains("startup error dialog failed: native failure"));
    }
}
