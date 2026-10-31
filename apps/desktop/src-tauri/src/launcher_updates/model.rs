//! Wire types contain public release metadata and an opaque selection token.
//! URLs, HTTP response bodies and authentication data never enter this DTO.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
    Unsupported,
}
impl Architecture {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            _ => Self::Unsupported,
        }
    }
    pub(super) fn artifact_name(&self) -> Option<&'static str> {
        match self {
            Self::X86_64 => Some(if cfg!(windows) {
                "pcl-desktop-windows-x86_64.exe"
            } else if cfg!(target_os = "macos") {
                "pcl-desktop-macos-x86_64"
            } else {
                "pcl-desktop-linux-x86_64"
            }),
            Self::Aarch64 => Some(if cfg!(windows) {
                "pcl-desktop-windows-aarch64.exe"
            } else if cfg!(target_os = "macos") {
                "pcl-desktop-macos-aarch64"
            } else {
                "pcl-desktop-linux-aarch64"
            }),
            Self::Unsupported => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallChannel {
    Portable,
    PackageManaged,
    Unmanaged,
}

/// The application supplies build identity; the service never guesses a commit
/// from the checkout or treats the crate version as a complete build identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurrentBuild {
    pub version: String,
    pub commit: Option<String>,
    pub executable: String,
    pub install_channel: InstallChannel,
    pub architecture: Architecture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    Stable,
    Beta,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Idle,
    Checking,
    Available,
    Latest,
    NoCompatibleRelease,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Idle,
    Downloading,
    Staged,
    Cancelled,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReleaseView {
    pub token: String,
    pub tag: String,
    pub version: String,
    pub commit: String,
    pub prerelease: bool,
    pub artifact_name: String,
    pub architecture: Architecture,
    pub bytes: u64,
    pub sha256: String,
    pub elf_header_verified: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DownloadView {
    pub state: DownloadState,
    pub received_bytes: u64,
    pub total_bytes: u64,
    pub message: Option<String>,
}
impl Default for DownloadView {
    fn default() -> Self {
        Self {
            state: DownloadState::Idle,
            received_bytes: 0,
            total_bytes: 0,
            message: None,
        }
    }
}

/// An anonymous verified file stays owned by this process and disappears on
/// discard/exit. can_apply requires the fixed portable destination and no older
/// pending transaction. apply rechecks the original inode/content before use.
#[derive(Clone, Debug, Serialize)]
pub struct ApplyPlan {
    pub version: String,
    pub commit: String,
    pub artifact_name: String,
    pub architecture: Architecture,
    pub bytes: u64,
    pub sha256: String,
    pub destination: Option<String>,
    pub install_channel: InstallChannel,
    pub original_sha256: Option<String>,
    pub can_apply: bool,
    pub restart_required: bool,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationState {
    Idle,
    Applying,
    Applied,
    RollingBack,
    RolledBack,
    RecoveryRequired,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskBuild {
    pub version: String,
    pub commit: Option<String>,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RollbackView {
    pub token: String,
    pub original_version: String,
    pub applied_version: String,
    pub can_rollback: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstallationView {
    pub state: InstallationState,
    pub disk_build: Option<DiskBuild>,
    pub restart_required: bool,
    pub rollback: Option<RollbackView>,
    pub warning: Option<String>,
}
impl Default for InstallationView {
    fn default() -> Self {
        Self {
            state: InstallationState::Idle,
            disk_build: None,
            restart_required: false,
            rollback: None,
            warning: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateView {
    pub current: CurrentBuild,
    pub state: CheckState,
    pub message: Option<String>,
    pub release: Option<ReleaseView>,
    pub download: DownloadView,
    pub plan: Option<ApplyPlan>,
    pub installation: InstallationView,
}
