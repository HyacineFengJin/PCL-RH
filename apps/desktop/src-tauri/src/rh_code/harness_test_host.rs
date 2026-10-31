//! Closed data-only host for the actual Harness source test target. Never
//! compiled by the desktop runtime and never grants filesystem operations.
#[path = "harness.rs"]
mod harness;
#[path = "identity.rs"]
mod identity;
#[path = "models.rs"]
mod models;
use identity::{hash, id};
use models::{BuildPolicy, Project};
mod assets {
    pub(super) struct AssetWrite {
        pub path: String,
        pub bytes: Vec<u8>,
        pub expected: Option<String>,
    }
}
