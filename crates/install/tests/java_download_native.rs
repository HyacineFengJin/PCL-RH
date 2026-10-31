//! Actual desktop Java filesystem/catalog ports without GUI or a real JVM.
//! HTTP is unreachable in this filesystem-only harness; all payloads are owned
//! synthetic bytes below TempDir. Native CI runs the same source on each host.
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local.rs"]
mod launcher_local;
type Result<T> = std::result::Result<T, String>;
mod network {
    pub struct Http;
    impl Http {
        pub async fn metadata(
            &self,
            _: &str,
            _: Option<(&str, u64)>,
            _: &std::sync::atomic::AtomicBool,
        ) -> super::Result<Vec<u8>> {
            panic!("Java filesystem fixture must never access the network")
        }
    }
}
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/java_download/catalog.rs"]
mod catalog;
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/java_download/files_native.rs"]
mod files;
use files::Store;
#[path = "../../../apps/desktop/src-tauri/src/java_download/binding.rs"]
mod binding;
use binding::StoreBinding;

#[path = "../../../apps/desktop/src-tauri/src/java_download/native_tests.rs"]
mod native_tests;
