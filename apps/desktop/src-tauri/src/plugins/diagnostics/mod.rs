pub(crate) mod analysis;
use crate::plugin_host::Registration;
pub use crate::product_plugins::contracts::{
    AnalysisService, FailureAnalyzer, FAILURE_ANALYSIS as ANALYSIS,
};
use analysis::FailureFinding;
use std::sync::Arc;
pub const ID: &str = "launcher.diagnostics.analysis";
struct SignatureAnalyzer;
impl FailureAnalyzer for SignatureAnalyzer {
    fn analyze(&self, message: &str, log_tail: Option<&str>) -> Vec<FailureFinding> {
        analysis::analyze(message, log_tail)
    }
}
pub fn start(registration: &mut Registration<'_>) -> Result<(), String> {
    registration.provide(ANALYSIS, AnalysisService(Arc::new(SignatureAnalyzer)))
}
