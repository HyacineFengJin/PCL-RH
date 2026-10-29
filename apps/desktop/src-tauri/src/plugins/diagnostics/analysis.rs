//! Conservative hints from one failed attempt's already-redacted message and
//! bounded log tail. A signature is evidence of an error family, not proof of a
//! particular mod, driver or fix. No file IO, account access or automatic repair
//! belongs here. Unknown failures remain unknown, including signal-only exits.

const MAX_INPUT_BYTES: usize = 272 * 1024;
const MAX_LINES: usize = 1024;
const MAX_EVIDENCE_CHARS: usize = 320;
const MAX_EVIDENCE: usize = 2;

pub use crate::product_plugins::contracts::FailureFinding;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    JavaVersion,
    JavaRuntime,
    JavaMemory,
    JavaOption,
    MissingDependency,
    Graphics,
    GraphicsHandoff,
    ModDependency,
    ModIncompatible,
    ModDuplicate,
    Mixin,
    ModLoading,
}

impl Family {
    fn code(self) -> &'static str {
        match self {
            Self::JavaVersion => "java_version",
            Self::JavaRuntime => "java_runtime",
            Self::JavaMemory => "java_memory",
            Self::JavaOption => "java_option",
            Self::MissingDependency => "missing_dependency",
            Self::Graphics => "graphics",
            Self::GraphicsHandoff => "graphics_handoff",
            Self::ModDependency => "mod_dependency",
            Self::ModIncompatible => "mod_incompatible",
            Self::ModDuplicate => "mod_duplicate",
            Self::Mixin => "mixin",
            Self::ModLoading => "mod_loading",
        }
    }

    fn specificity(self) -> u8 {
        match self {
            Self::JavaVersion
            | Self::JavaRuntime
            | Self::JavaMemory
            | Self::JavaOption
            | Self::ModDependency
            | Self::ModIncompatible
            | Self::ModDuplicate => 0,
            Self::MissingDependency => 1,
            Self::Graphics | Self::Mixin => 2,
            Self::GraphicsHandoff => 3,
            Self::ModLoading => 4,
        }
    }
}

/// Loader translations contain Minecraft formatting between the diagnostic
/// words. Ignore those codes only for matching; retain the original redacted
/// line as evidence. No paths, mod metadata or external state are inspected.
fn diagnostic_lowercase(line: &str) -> String {
    let mut chars = line.chars().peekable();
    let mut text = String::with_capacity(line.len());
    while let Some(ch) = chars.next() {
        if ch == '§'
            && chars
                .peek()
                .is_some_and(|next| "0123456789abcdefklmnorxABCDEFKLMNORX".contains(*next))
        {
            chars.next();
        } else {
            text.push(ch.to_ascii_lowercase());
        }
    }
    text
}

fn mod_dependency_failure(lower: &str) -> bool {
    // NeoForge ModSorter emits these only for unmet required dependencies or
    // installed optional dependencies outside their supported version range.
    // Its structured detail also works when the header fell outside the tail.
    lower.ends_with("missing or unsupported mandatory dependencies:")
        || lower.ends_with("unsupported installed optional dependencies:")
        || (lower.contains("mod id:")
            && lower.contains("requested by:")
            && lower.contains("expected range:")
            && lower.contains("actual version:"))
        || (lower.contains("failure message: mod ")
            && (lower.contains(" requires ") || lower.contains(" only supports ")))
        // Fabric's recommends/conflicts messages are warnings. Match only the
        // required-dependency rejection, including missing, wrong and disabled.
        || (lower.contains("mod ")
            && lower.contains(" requires ")
            && (lower.contains(", which is missing!")
                || lower.contains(", but only the wrong version")
                || lower.contains(", which is disabled for this environment (client/server only)!")
                || lower.contains(", which can't be loaded due to other constraints!")))
        || (lower.contains("some mods have requested conflicting versions of:")
            && lower.contains("requested by:"))
}

fn mod_incompatible_failure(lower: &str) -> bool {
    lower.ends_with("incompatibilities between mods:")
        || (lower.contains("mod ")
            && lower.contains(" is incompatible with ")
            && ((lower.contains("versions:") && lower.contains("version found:"))
                || (lower.contains("conflicting version") && lower.contains(" present:"))))
        || (lower.contains("file ")
            && (lower.contains(" is a fabric mod and cannot be loaded")
                || lower.contains(" is a liteloader mod and cannot be loaded")
                || lower
                    .contains(" is for an old version of minecraft forge and cannot be loaded")
                || lower.contains(
                    " is for minecraft forge or an older version of neoforge, and cannot be loaded",
                )))
}

/// Codes are stable presentation keys. Keep patterns specific: ordinary lines
/// mentioning OpenGL, Java or a mod must not be presented as a failure cause.
fn signature(line: &str) -> Option<Family> {
    let lower = diagnostic_lowercase(line);
    if lower.contains("unsupportedclassversionerror")
        || lower.contains("unsupported class file major version")
        || line.contains("此实例需要 Java")
        || line.contains("实例要求的 Java 主版本无效")
    {
        Some(Family::JavaVersion)
    } else if line.contains("手动选择的 Java 不可用")
        || line.contains("Java 路径不可用")
        || line.contains("Java 文件没有执行权限")
        || line.contains("Java 路径必须指向可执行文件")
        || line.contains("无法识别 Java")
        || (line.contains("Java 架构") && line.contains("不兼容"))
    {
        Some(Family::JavaRuntime)
    } else if lower.contains("java.lang.outofmemoryerror")
        || lower.contains("could not reserve enough space for")
        || lower.contains("insufficient memory for the java runtime environment")
        || lower.contains("invalid maximum heap size")
        || lower.contains("invalid initial heap size")
        || lower.contains("native memory allocation (malloc) failed to allocate")
        || lower.contains("native memory allocation (mmap) failed to map")
        || lower.contains("initial heap size set to a larger value than the maximum heap size")
    {
        Some(Family::JavaMemory)
    } else if lower.contains("unrecognized vm option")
        || lower.contains("unrecognized option:")
        || lower.contains("improperly specified vm option")
    {
        Some(Family::JavaOption)
    } else if mod_dependency_failure(&lower) {
        Some(Family::ModDependency)
    } else if mod_incompatible_failure(&lower) {
        Some(Family::ModIncompatible)
    } else if lower.contains("duplicatemodsfoundexception:")
        || lower.ends_with("found duplicate mods:")
        || (lower.contains("mod ") && lower.contains(" is present in multiple files:"))
    {
        Some(Family::ModDuplicate)
    } else if (lower.starts_with("missing ") && lower.contains(" installed dependencies"))
        // SecureJarHandler throws this when every supplied input path is
        // absent. Match its IOException, not an ordinary path/stack-frame
        // mention; the paths may belong to loader libraries or other JARs.
        // Upstream: McModLauncher/securejarhandler, JarContentsImpl constructor.
        || lower.contains("java.io.ioexception: invalid paths argument, contained no existing paths: [")
        || lower.contains("java.lang.classnotfoundexception:")
        || (lower.contains("java.lang.noclassdeffounderror:")
            && !lower.contains("could not initialize class"))
        || lower.contains("could not find or load main class")
        || lower.contains("java.lang.unsatisfiedlinkerror:")
    {
        Some(Family::MissingDependency)
    } else if lower.contains("we seem to be having trouble handing off the window, tried for 1 second")
        || lower.contains("cannot hand over rendering to minecraft! the background loading screen renderer seems stuck.")
    {
        // FancyModLoader DisplayWindow times out waiting for its early display
        // renderer to hand over to Minecraft. This is an observed stage, not
        // evidence identifying a graphics driver, OS backend or particular mod.
        Some(Family::GraphicsHandoff)
    } else if lower.contains("failed to initialize graphics window")
        || lower.contains("glfw error 65542")
        || lower.contains("glfw error 65543")
        || lower.contains("glfw error 65544")
        || lower.contains("failed to create the glfw window")
        || lower.contains("failed to initialize glfw")
        || lower.contains("failed to find any valid glfw profile.")
        || lower.contains("failed to find a valid glfw profile.")
        || lower.contains("failed to create a glfw window with any profile")
        || lower.contains("timed out trying to setup the game window.")
        || lower.contains("we seem to be having trouble initializing the window, waited for 30 seconds")
    {
        Some(Family::Graphics)
    } else if lower.contains("mixinapplyerror:")
        || lower.contains("mixininitialisationerror:")
        || lower.contains("mixintransformererror:")
        || lower.contains("mixinprepareerror:")
        || lower.contains("org.spongepowered.asm.mixin.injection.throwables.injectionerror:")
    {
        // InvalidMixinException and "Mixin apply failed" can be logged for
        // optional mixins and continue loading. Require a fatal Error signature.
        Some(Family::Mixin)
    } else if lower.contains("modloadingexception:")
        || lower.contains("modresolutionexception:")
        || lower.contains("incompatible mods found!")
    {
        Some(Family::ModLoading)
    } else {
        None
    }
}

pub(crate) fn analyze(message: &str, log_tail: Option<&str>) -> Vec<FailureFinding> {
    let mut findings: Vec<(Family, FailureFinding)> = Vec::new();
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
        let Some(family) = signature(line.trim()) else {
            continue;
        };
        let evidence: String = line
            .chars()
            .filter(|ch| !ch.is_control())
            .take(MAX_EVIDENCE_CHARS)
            .collect();
        if let Some((_, finding)) = findings.iter_mut().find(|(found, _)| *found == family) {
            if finding.evidence.len() < MAX_EVIDENCE && !finding.evidence.contains(&evidence) {
                finding.evidence.push(evidence);
            }
        } else {
            findings.push((
                family,
                FailureFinding {
                    code: family.code().into(),
                    evidence: vec![evidence],
                },
            ));
        }
    }
    if findings.is_empty() {
        return vec![FailureFinding {
            code: "unknown".into(),
            evidence: Vec::new(),
        }];
    }
    // Stack traces print outer exceptions before their nested causes. Preserve
    // all matching families, but present explicit unmet constraints and nested
    // runtime failures before transformation errors, timeouts and loader
    // wrappers. A stable sort retains encounter order among equally specific
    // evidence; this ranking makes no claim to reconstruct the exception tree.
    findings.sort_by_key(|(family, _)| family.specificity());
    findings.into_iter().map(|(_, finding)| finding).collect()
}

#[cfg(test)]
#[path = "../../launcher_failure_analysis/tests.rs"]
mod tests;
