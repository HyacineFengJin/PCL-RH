pub(crate) mod analysis;
use crate::plugin_host::{Contract, Registration};
use analysis::FailureFinding;
use std::sync::Arc;

pub const ID: &str = "launcher.diagnostics.analysis";
pub const ANALYSIS: Contract<AnalysisService> = Contract::new("launcher.failure-analysis", 1);
pub trait FailureAnalyzer: Send + Sync {
    fn analyze(&self, message: &str, log_tail: Option<&str>) -> Vec<FailureFinding>;
}
pub struct AnalysisService(pub Arc<dyn FailureAnalyzer>);
struct SignatureAnalyzer;
impl FailureAnalyzer for SignatureAnalyzer {
    fn analyze(&self, message: &str, log_tail: Option<&str>) -> Vec<FailureFinding> {
        analysis::analyze(message, log_tail)
    }
}
pub fn start(registration: &mut Registration<'_>) -> Result<(), String> {
    registration.provide(ANALYSIS, AnalysisService(Arc::new(SignatureAnalyzer)))
}
