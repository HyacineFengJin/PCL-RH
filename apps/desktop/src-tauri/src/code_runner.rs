//! Headless ownership boundary for an explicitly approved, private build copy.
//!
//! The trusted Node host owns frozen inputs and approval receipts. This helper
//! receives only an attempt directory and fixed recipes: it never discovers an
//! original project, reads launcher state, or accepts a command/shell string.
//! Bubblewrap must successfully create the required namespaces before a recipe
//! starts. Source and HOME are pinned directory FDs; only those private copies
//! are writable. Per-process rlimits and a bounded /tmp are useful limits, not
//! an aggregate disk/memory/process-tree quota. No cgroup policy is changed.
//!
//! stdin remains the ownership lease after the request. Cancel, EOF, or parent
//! death stops the owned process group and PID namespace before reaping its
//! leader. Nonblocking input/output prevents abandoned pipes from holding the
//! experimental host's shutdown deadline. stdout is exclusively bounded NDJSON.
#[path = "code_runner/inputs.rs"]
mod inputs;
#[cfg(target_os = "linux")]
#[path = "code_runner/process.rs"]
mod process;
#[path = "code_runner/protocol.rs"]
mod protocol;
#[cfg(target_os = "linux")]
#[path = "code_runner/sandbox.rs"]
mod sandbox;

pub(crate) use protocol::host_pipe;
use protocol::{Input, Writer};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

const FLAG: &str = "--rh-code-build-runner";
const MAX_TIMEOUT: u64 = 600_000;
const MAX_OUTPUT: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Recipe {
    Javac,
    Gradle,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Target {
    Classes,
    Build,
    Test,
}
impl Target {
    fn argument(self) -> &'static str {
        match self {
            Self::Classes => "classes",
            Self::Build => "build",
            Self::Test => "test",
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    schema_version: u32,
    run_id: String,
    root: PathBuf,
    recipe: Recipe,
    target: Target,
    network: bool,
    timeout_ms: u64,
    max_output_bytes: usize,
}
impl Request {
    fn validate(&self) -> Result<(), Failure> {
        if self.schema_version != 1
            || !uuid(&self.run_id)
            || !self.root.is_absolute()
            || !(1..=MAX_TIMEOUT).contains(&self.timeout_ms)
            || !(1..=MAX_OUTPUT).contains(&self.max_output_bytes)
            || (self.recipe == Recipe::Javac && self.target != Target::Classes)
        {
            return Err(Failure::new(
                "INVALID_BUILD_REQUEST",
                "Expected schema 1, a UUID, an absolute private attempt directory, bounded limits and a supported recipe target",
            ));
        }
        Ok(())
    }
}
fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

#[derive(Debug)]
struct Failure {
    code: &'static str,
    message: String,
}
impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into().chars().take(1000).collect(),
        }
    }
    fn io(context: &'static str, error: std::io::Error) -> Self {
        Self::new("BUILD_RUNNER_IO", format!("{context}: {error}"))
    }
    fn value(&self) -> Value {
        json!({"code": self.code, "message": self.message})
    }
}
#[derive(Debug)]
struct Outcome {
    status: &'static str,
    exit_code: Option<i32>,
    signal: Option<String>,
    error: Option<Failure>,
}
impl Outcome {
    fn blocked(error: Failure) -> Self {
        Self {
            status: "blocked",
            exit_code: None,
            signal: None,
            error: Some(error),
        }
    }
    fn finished(&self) -> Value {
        json!({"type":"finished", "status":self.status, "exitCode":self.exit_code,
            "signal":self.signal, "error":self.error.as_ref().map(Failure::value)})
    }
    #[cfg(target_os = "linux")]
    fn failed(error: Failure) -> Self {
        Self {
            status: "failed",
            exit_code: None,
            signal: None,
            error: Some(error),
        }
    }
}

/// Called before any desktop/Tauri initialization; normal launcher invocations
/// return None. No Node preset, account store, WebView, or display is initialized.
pub fn main_if_requested() -> Option<i32> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(FLAG)) {
        return None;
    }
    Some(helper_main())
}
fn helper_main() -> i32 {
    let mut writer = match Writer::stdout() {
        Ok(writer) => writer,
        Err(_) => return 1,
    };
    let mut input = match Input::stdin() {
        Ok(input) => input,
        Err(error) => {
            let _ = writer.event(&Outcome::blocked(error).finished());
            return 1;
        }
    };
    serve(&mut input, &mut writer)
}

fn serve(input: &mut Input, writer: &mut Writer) -> i32 {
    let outcome = (|| {
        #[cfg(target_os = "linux")]
        process::arm_parent_death()?;
        let bytes = input.request(Duration::from_secs(5))?;
        let request: Request = serde_json::from_slice(&bytes)
            .map_err(|_| Failure::new("INVALID_BUILD_REQUEST", "Invalid build request JSON"))?;
        // A valid run identity is echoed even when later validation refuses the
        // recipe. Malformed identities never become protocol authority.
        if uuid(&request.run_id) {
            writer.set_run_id(&request.run_id);
        }
        request.validate()?;
        execute(&request, input, writer)
    })()
    .unwrap_or_else(Outcome::blocked);
    let result = if outcome.status == "completed" { 0 } else { 1 };
    if writer.event(&outcome.finished()).is_err() {
        return 1;
    }
    result
}

#[cfg(target_os = "linux")]
fn execute(request: &Request, input: &mut Input, writer: &mut Writer) -> Result<Outcome, Failure> {
    // The common read-only inventory bounds source and HOME before the
    // unchanged Linux sandbox backend creates any compiler output.
    drop(inputs::Inventory::capture(&request.root)?);
    let prepared = sandbox::Prepared::new(request)?;
    // Availability is an actual sandbox launch, not command-v or a fallback to
    // unsandboxed execution. Cancellation can also retire this short preflight.
    let mut diagnostic = String::new();
    let probe = process::run(
        prepared.command(request, true)?,
        input,
        Duration::from_secs(3),
        8192,
        |_| Ok(()),
        |_, text| {
            let remaining = 1000usize.saturating_sub(diagnostic.chars().count());
            diagnostic.extend(text.chars().take(remaining));
            Ok(())
        },
    )?;
    if probe.status == "cancelled" {
        return Ok(probe);
    }
    if probe.status != "completed" {
        return Ok(Outcome::blocked(Failure::new(
            "BUILD_SANDBOX_UNAVAILABLE",
            format!(
                "Required bubblewrap isolation did not pass preflight: {}",
                diagnostic.trim()
            ),
        )));
    }
    Ok(process::run(
        prepared.command(request, false)?,
        input,
        Duration::from_millis(request.timeout_ms),
        request.max_output_bytes,
        |_| {
            writer.event(&json!({"type":"started", "recipe":match request.recipe { Recipe::Javac => "javac", Recipe::Gradle => "gradle" },
                "target":request.target.argument(), "network":request.network,
                "sandbox":"bubblewrap", "limits":{"timeoutMs":request.timeout_ms,"maxOutputBytes":request.max_output_bytes,
                    "temporaryBytes":sandbox::TMP_BYTES,"fileBytes":process::FILE_BYTES,
                    "uidTaskLimit":process::UID_TASKS,"aggregateResourceQuota":false}}))
        },
        |stream, text| writer.event(&json!({"type":"output", "stream":stream, "text":text})),
    ).unwrap_or_else(Outcome::failed))
}

#[cfg(not(target_os = "linux"))]
fn execute(request: &Request, input: &mut Input, _writer: &mut Writer) -> Result<Outcome, Failure> {
    let _captured = inputs::Inventory::capture(&request.root)?;
    if input.cancelled()? {
        return Ok(Outcome {
            status: "cancelled",
            exit_code: None,
            signal: None,
            error: None,
        });
    }
    // Reading the private input copy establishes no execution confinement.
    // Refuse before creating output directories or launching any author code.
    Err(Failure::new("BUILD_SANDBOX_UNAVAILABLE",
        format!("Author-code execution requires a verified native build sandbox; target {} with network={} is unavailable on this platform", request.target.argument(), request.network)))
}

#[cfg(test)]
#[path = "code_runner/portable_tests.rs"]
mod portable_tests;
#[cfg(all(test, target_os = "linux"))]
#[path = "code_runner/tests.rs"]
mod tests;
