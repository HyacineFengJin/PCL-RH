//! Serializable project data shared by the native store and its boundary tests.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: String,
    pub job_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub mode: String,
    pub messages: Vec<Message>,
    pub active_job: Option<String>,
    #[serde(default)]
    pub draft: String,
    #[serde(default)]
    pub context_note: String,
    #[serde(default)]
    pub context_paths: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub minecraft: String,
    pub loaders: Vec<String>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildPolicy {
    pub enabled: bool,
    pub network: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub folder: String,
    pub updated_at: u64,
    pub targets: Vec<Target>,
    pub conversations: Vec<Conversation>,
    pub layout: Value,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub build_policy: BuildPolicy,
}
