//! Shared wire types and hashing for the actual log selection service.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LogKind {
    Game,
    Crash,
}
impl LogKind {
    pub(super) fn directory(self) -> &'static str {
        match self {
            Self::Game => "logs",
            Self::Crash => "crash-reports",
        }
    }
    pub(super) fn accepts(self, name: &str) -> bool {
        match self {
            Self::Game => {
                name.ends_with(".log") || name.ends_with(".txt") || name.ends_with(".log.gz")
            }
            Self::Crash => name.ends_with(".txt"),
        }
    }
}

pub(super) fn digest(value: &impl Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|_| "无法绑定日志选择")?)
    ))
}
