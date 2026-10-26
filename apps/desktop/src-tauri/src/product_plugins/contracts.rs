//! Product service contracts, independent of any bundled implementation.
//! Providers own behavior and resources; consumers depend only on these ports.
use crate::plugin_host::Contract;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};
pub(crate) mod copy;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnnouncementScope {
    All,
    Important,
    None,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AnnouncementState {
    Disabled,
    Ready,
    Empty,
    RateLimited,
    Unavailable,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Announcement {
    pub id: String,
    pub title: String,
    pub text: String,
    pub url: String,
    pub published_at: Option<String>,
    pub prerelease: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementView {
    pub state: AnnouncementState,
    pub scope: AnnouncementScope,
    pub items: Vec<Announcement>,
    pub fetched_at: Option<u64>,
    pub retry_at: Option<u64>,
    pub cached: bool,
    pub truncated: bool,
    pub message: Option<String>,
}
impl AnnouncementView {
    pub(crate) fn disabled() -> Self {
        Self {
            state: AnnouncementState::Disabled,
            scope: AnnouncementScope::None,
            items: vec![],
            fetched_at: None,
            retry_at: None,
            cached: false,
            truncated: false,
            message: None,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct FailureFinding {
    pub code: String,
    pub evidence: Vec<String>,
}

pub const ANNOUNCEMENTS: Contract<AnnouncementService> = Contract::new("launcher.announcements", 1);
pub const FAILURE_ANALYSIS: Contract<AnalysisService> =
    Contract::new("launcher.failure-analysis", 1);
pub trait AnnouncementProvider: Send + Sync {
    fn read(
        &self,
        scope: AnnouncementScope,
        refresh: bool,
    ) -> Pin<Box<dyn Future<Output = AnnouncementView> + Send + '_>>;
}
pub struct AnnouncementService(pub Arc<dyn AnnouncementProvider>);
pub trait FailureAnalyzer: Send + Sync {
    fn analyze(&self, message: &str, log_tail: Option<&str>) -> Vec<FailureFinding>;
}
pub struct AnalysisService(pub Arc<dyn FailureAnalyzer>);
