//! Actual Modrinth target discovery, fingerprints and typed source backend.
#![allow(dead_code)]
use serde::Serialize;
use std::{path::Path, sync::atomic::AtomicBool};
type Result<T> = std::result::Result<T, String>;
const CANCELLED: &str = "资源下载已取消";
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_BATCH_BYTES: u64 = 8 * 1024 * 1024 * 1024;
#[path = "../../../apps/desktop/src-tauri/src/modrinth_install/model.rs"]
mod model;
use model::{Compatibility, LocalFile};
#[path = "../../../apps/desktop/src-tauri/src/modrinth_install/allocation.rs"]
mod allocation;
#[path = "../../../apps/desktop/src-tauri/src/modrinth_install/provider/identity.rs"]
mod provider_identity;
#[path = "../../../apps/desktop/src-tauri/src/modrinth_install/source.rs"]
mod source;
mod provider {
    pub(crate) use crate::provider_identity::file_name;
}
#[path = "../../../apps/desktop/src-tauri/src/resource_ops/token.rs"]
mod resource_token;
mod resource_ops {
    pub(crate) use crate::resource_token::token_for;
}
#[path = "../../../apps/desktop/src-tauri/src/modrinth_install/target.rs"]
mod target;
