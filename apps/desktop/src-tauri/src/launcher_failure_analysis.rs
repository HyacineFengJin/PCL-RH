//! Conservative hints from one failed attempt's already-redacted message and
//! bounded log tail. A signature is evidence of an error family, not proof of a
//! particular mod, driver or fix. No file IO, account access or automatic repair
//! belongs here. Unknown failures remain unknown, including signal-only exits.
use serde::Serialize;

const MAX_INPUT_BYTES: usize = 272 * 1024;
const MAX_LINES: usize = 1024;
const MAX_EVIDENCE_CHARS: usize = 320;
const MAX_EVIDENCE: usize = 2;

#[derive(Debug, Serialize)]
pub(super) struct FailureFinding {
    pub code: String,
    pub evidence: Vec<String>,
}

/// Codes are stable presentation keys. Keep patterns specific: ordinary lines
/// mentioning OpenGL, Java or a mod must not be presented as a failure cause.
fn signature(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("unsupportedclassversionerror")
        || lower.contains("unsupported class file major version")
        || line.contains("此实例需要 Java")
        || line.contains("实例要求的 Java 主版本无效")
    {
        Some("java_version")
    } else if line.contains("手动选择的 Java 不可用")
        || line.contains("Java 路径不可用")
        || line.contains("Java 文件没有执行权限")
        || line.contains("Java 路径必须指向可执行文件")
        || line.contains("无法识别 Java")
        || (line.contains("Java 架构") && line.contains("不兼容"))
    {
        Some("java_runtime")
    } else if lower.contains("java.lang.outofmemoryerror")
        || lower.contains("could not reserve enough space for")
        || lower.contains("insufficient memory for the java runtime environment")
        || lower.contains("invalid maximum heap size")
        || lower.contains("initial heap size set to a larger value than the maximum heap size")
    {
        Some("java_memory")
    } else if lower.contains("unrecognized vm option")
        || lower.contains("unrecognized option:")
        || lower.contains("improperly specified vm option")
    {
        Some("java_option")
    } else if (lower.starts_with("missing ") && lower.contains(" installed dependencies"))
        || lower.contains("java.lang.classnotfoundexception:")
        || (lower.contains("java.lang.noclassdeffounderror:")
            && !lower.contains("could not initialize class"))
        || lower.contains("could not find or load main class")
        || lower.contains("java.lang.unsatisfiedlinkerror:")
    {
        Some("missing_dependency")
    } else if lower.contains("failed to initialize graphics window")
        || lower.contains("glfw error 65542")
        || lower.contains("glfw error 65543")
        || lower.contains("glfw error 65544")
        || lower.contains("failed to create the glfw window")
        || lower.contains("failed to initialize glfw")
    {
        Some("graphics")
    } else if lower.contains("modloadingexception:")
        || lower.contains("modresolutionexception:")
        || lower.contains("incompatible mods found!")
        || lower.contains("mixinapplyerror:")
        || lower.contains("mixininitialisationerror:")
        || lower.contains("mixintransformererror:")
    {
        Some("mod_loading")
    } else {
        None
    }
}

pub(super) fn analyze(message: &str, log_tail: Option<&str>) -> Vec<FailureFinding> {
    let mut findings: Vec<FailureFinding> = Vec::new();
    let mut remaining = MAX_INPUT_BYTES;
    for line in message
        .lines()
        .chain(log_tail.unwrap_or_default().lines())
        .take(MAX_LINES)
    {
        if line.len() > remaining {
            break;
        }
        remaining -= line.len();
        let Some(code) = signature(line.trim()) else {
            continue;
        };
        let evidence: String = line
            .chars()
            .filter(|ch| !ch.is_control())
            .take(MAX_EVIDENCE_CHARS)
            .collect();
        if let Some(finding) = findings.iter_mut().find(|finding| finding.code == code) {
            if finding.evidence.len() < MAX_EVIDENCE && !finding.evidence.contains(&evidence) {
                finding.evidence.push(evidence);
            }
        } else {
            findings.push(FailureFinding {
                code: code.into(),
                evidence: vec![evidence],
            });
        }
    }
    if findings.is_empty() {
        findings.push(FailureFinding {
            code: "unknown".into(),
            evidence: Vec::new(),
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(message: &str, log: &str) -> Vec<String> {
        analyze(message, Some(log))
            .into_iter()
            .map(|finding| finding.code)
            .collect()
    }

    #[test]
    fn preparation_errors_point_to_java_or_missing_dependencies() {
        assert_eq!(
            codes("此实例需要 Java 21；手动选择的是 Java 17", ""),
            ["java_version"]
        );
        assert_eq!(
            codes("手动选择的 Java 不可用：Java 文件没有执行权限", ""),
            ["java_runtime"]
        );
        assert_eq!(
            codes(
                "Missing 12 installed dependencies:\n/libraries/example.jar",
                ""
            ),
            ["missing_dependency"]
        );
    }

    #[test]
    fn each_specific_runtime_family_has_a_stable_hint() {
        for (line, code) in [
            (
                "java.lang.UnsupportedClassVersionError: Example",
                "java_version",
            ),
            ("java.lang.OutOfMemoryError: Java heap space", "java_memory"),
            ("Unrecognized VM option 'Example'", "java_option"),
            (
                "java.lang.ClassNotFoundException: example.Main",
                "missing_dependency",
            ),
            (
                "java.lang.UnsatisfiedLinkError: no lwjgl in java.library.path",
                "missing_dependency",
            ),
            (
                "Failed to initialize graphics window with current settings.",
                "graphics",
            ),
            (
                "GLFW error 65542: GLX: Failed to create context",
                "graphics",
            ),
            (
                "net.fabricmc.loader.impl.discovery.ModResolutionException: conflicting versions",
                "mod_loading",
            ),
            (
                "org.spongepowered.asm.mixin.throwables.MixinApplyError: could not apply",
                "mod_loading",
            ),
        ] {
            assert_eq!(codes("exited 1", line), [code]);
        }
    }

    #[test]
    fn ordinary_environment_logs_and_signal_codes_do_not_diagnose_a_cause() {
        assert_eq!(codes("exited 137", "Java 21\nOpenGL version 4.6\nLoaded 300 mods\nGraphics initialized successfully\nA dependency check completed"), ["unknown"]);
        assert_eq!(
            codes(
                "exited 1",
                "java.lang.NoClassDefFoundError: Could not initialize class example.Registry"
            ),
            ["unknown"]
        );
    }

    #[test]
    fn generic_vm_wrapper_does_not_distract_from_explicit_memory_error() {
        assert_eq!(
            codes(
                "exited 1",
                "Error: Could not create the Java Virtual Machine."
            ),
            ["unknown"]
        );
        assert_eq!(codes("exited 1", "Could not reserve enough space for object heap\nError: Could not create the Java Virtual Machine."), ["java_memory"]);
        assert_eq!(codes("exited 1", "Invalid maximum heap size\nUnrecognized VM option 'Other'\nError: Could not create the Java Virtual Machine."), ["java_memory", "java_option"]);
    }

    #[test]
    fn evidence_is_deduplicated_and_bounded_without_breaking_unicode() {
        let line = format!(
            "java.lang.ClassNotFoundException: {}\u{0007}",
            "示例".repeat(400)
        );
        let log = format!("{line}\n{line}\njava.lang.ClassNotFoundException: second\njava.lang.ClassNotFoundException: third");
        let report = analyze("failed", Some(&log));
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].evidence.len(), 2);
        assert_eq!(report[0].evidence[0].chars().count(), MAX_EVIDENCE_CHARS);
        assert!(!report[0].evidence[0].chars().any(char::is_control));
    }

    #[test]
    fn overlong_input_remains_bounded_and_returns_unknown_without_guessing() {
        let huge = "x".repeat(MAX_INPUT_BYTES + 1);
        assert_eq!(
            codes("failed", &format!("{huge}\nUnrecognized VM option: false")),
            ["unknown"]
        );
        assert_eq!(
            codes(
                "failed",
                &format!(
                    "{}\nUnrecognized VM option: false",
                    "normal\n".repeat(MAX_LINES)
                )
            ),
            ["unknown"]
        );
    }
}
