//! Fixed, read-only binary identity before project or desktop initialization.
//! This reports compile-time facts, never runtime environment or user paths.
use serde::Serialize;
use std::{
    ffi::{OsStr, OsString},
    io::Write,
};

#[derive(Serialize)]
pub(crate) struct BuildInfo {
    schema_version: u32,
    product_id: &'static str,
    version: &'static str,
    commit: Option<&'static str>,
    profile: &'static str,
    target: &'static str,
    target_os: &'static str,
    target_arch: &'static str,
    debug_assertions: bool,
    custom_protocol: bool,
    curseforge_embedded: bool,
}

fn commit(value: Option<&'static str>) -> Option<&'static str> {
    value.filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn compiled(custom_protocol: bool, curseforge_embedded: bool) -> BuildInfo {
    BuildInfo {
        schema_version: 1,
        product_id: "dev.pcl-rh.desktop",
        version: env!("CARGO_PKG_VERSION"),
        commit: commit(option_env!("PCL_BUILD_COMMIT")),
        // The real desktop build script sets both. A Tauri-free source test
        // wrapper has no such script; it must not invent the desktop identity.
        profile: option_env!("PCL_BUILD_PROFILE").unwrap_or("unknown"),
        target: option_env!("PCL_BUILD_TARGET").unwrap_or("unknown"),
        target_os: std::env::consts::OS,
        target_arch: std::env::consts::ARCH,
        debug_assertions: cfg!(debug_assertions),
        custom_protocol,
        curseforge_embedded,
    }
}

pub(crate) fn dispatch(custom_protocol: bool, embedded: impl FnOnce() -> bool) -> Option<i32> {
    let code = write_for_args(
        std::env::args_os().skip(1),
        || compiled(custom_protocol, embedded()),
        &mut std::io::stdout().lock(),
    );
    match code {
        Some(2) => eprintln!("--build-info accepts no additional arguments"),
        Some(1) => eprintln!("Cannot write build information"),
        _ => {}
    }
    code
}

fn write_for_args(
    args: impl IntoIterator<Item = OsString>,
    info: impl FnOnce() -> BuildInfo,
    output: &mut impl Write,
) -> Option<i32> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some(OsStr::new("--build-info")) {
        return None;
    }
    if args.next().is_some() {
        return Some(2);
    }
    let result = serde_json::to_writer(&mut *output, &info())
        .map_err(|_| ())
        .and_then(|_| output.write_all(b"\n").map_err(|_| ()))
        .and_then(|_| output.flush().map_err(|_| ()));
    Some(if result.is_ok() { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> BuildInfo {
        BuildInfo {
            schema_version: 1,
            product_id: "dev.pcl-rh.desktop",
            version: "0.2.0",
            commit: Some("0123456789abcdef0123456789abcdef01234567"),
            profile: "release",
            target: "x86_64-unknown-linux-gnu",
            target_os: "linux",
            target_arch: "x86_64",
            debug_assertions: true,
            custom_protocol: true,
            curseforge_embedded: false,
        }
    }
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn exact_flag_writes_one_closed_json_object_with_independent_profile_and_assertions() {
        let mut output = Vec::new();
        assert_eq!(
            write_for_args(args(&["--build-info"]), fixture, &mut output),
            Some(0)
        );
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "schema_version":1, "product_id":"dev.pcl-rh.desktop", "version":"0.2.0",
                "commit":"0123456789abcdef0123456789abcdef01234567", "profile":"release",
                "target":"x86_64-unknown-linux-gnu", "target_os":"linux", "target_arch":"x86_64",
                "debug_assertions":true, "custom_protocol":true, "curseforge_embedded":false,
            })
        );
    }

    #[test]
    fn other_roles_and_invalid_arity_never_construct_identity_or_echo_arguments() {
        for (values, expected) in [
            (vec![], None),
            (vec!["--version"], None),
            (vec!["-V"], None),
            (vec!["--game-monitor", "--build-info"], None),
            (vec!["--rh-code-build-runner", "--build-info"], None),
            (vec!["--build-info", "/synthetic/private/path"], Some(2)),
            (vec!["--build-info", "synthetic-credential"], Some(2)),
        ] {
            let mut output = Vec::new();
            assert_eq!(
                write_for_args(
                    args(&values),
                    || panic!("non-report path constructed identity"),
                    &mut output
                ),
                expected
            );
            assert!(output.is_empty());
        }
    }

    #[test]
    fn only_a_full_compile_time_commit_is_reported() {
        for value in [
            None,
            Some(""),
            Some("unknown"),
            Some("/synthetic/private/path"),
            Some("0123456789abcdef0123456789abcdef0123456g"),
        ] {
            assert_eq!(commit(value), None);
        }
        let valid = Some("0123456789abcdef0123456789abcdef01234567");
        assert_eq!(commit(valid), valid);
        let mut info = fixture();
        info.commit = commit(None);
        assert!(serde_json::to_value(info).unwrap()["commit"].is_null());
    }

    #[test]
    fn closed_stdout_and_flush_failures_return_failure_instead_of_panicking() {
        struct Broken {
            flush: bool,
        }
        impl Write for Broken {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.flush {
                    Ok(bytes.len())
                } else {
                    Err(std::io::ErrorKind::BrokenPipe.into())
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        for flush in [false, true] {
            assert_eq!(
                write_for_args(args(&["--build-info"]), fixture, &mut Broken { flush }),
                Some(1)
            );
        }
    }
}
