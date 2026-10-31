//! Native job identities; no filesystem authority is encoded here.
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn id() -> String {
    hash(
        format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
        .as_bytes(),
    )[..24]
        .to_owned()
}
