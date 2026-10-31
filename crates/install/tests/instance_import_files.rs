//! Actual import filesystem and bounded ZIP parser, synthetic files only.
#![allow(dead_code)]
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Write,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
type Result<T> = std::result::Result<T, String>;
const MAX_FILES: usize = 100_000;
const MAX_DEPTH: usize = 96;
const MAX_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const MAX_FILE: u64 = 8 * 1024 * 1024 * 1024;
const MAX_JSON: u64 = 32 * 1024 * 1024;
const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_JOURNAL: u64 = 64 * 1024 * 1024;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[cfg(target_os = "linux")]
fn os_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}
fn nonce() -> String {
    format!(
        "fixture-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
fn changed() -> String {
    "import source changed".into()
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}
fn component(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', ':', '\0'])
        || name.chars().any(char::is_control)
    {
        Err("invalid component".into())
    } else {
        pcl_core::retained_fs::Leaf::new(name)
            .map(|_| ())
            .map_err(error)
    }
}
fn relative(path: &str) -> Result<Vec<&str>> {
    let parts: Vec<_> = path.split('/').collect();
    if path.is_empty() || path.len() > 4096 || parts.len() > MAX_DEPTH {
        return Err("invalid relative path".into());
    }
    for part in &parts {
        component(part)?;
    }
    Ok(parts)
}
fn name_ok(name: &str) -> Result<()> {
    component(name)
}
fn forbidden(path: &str) -> bool {
    path.split('/').any(|p| {
        p.starts_with(".pcl-")
            || p.starts_with(".install-")
            || p.eq_ignore_ascii_case("accounts.json")
    })
}
#[cfg(target_os = "linux")]
fn cstring(name: &str) -> Result<std::ffi::CString> {
    std::ffi::CString::new(name).map_err(error)
}
#[path = "../../../apps/desktop/src-tauri/src/instance_import/archive.rs"]
mod archive;
#[allow(unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/instance_import/filesystem.rs"]
mod filesystem;
fn zip(path: &Path, extra: &str) {
    let file = File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("pcl-export.json", options).unwrap();
    zip.write_all(br#"{"format":"pcl-local-instance","format_version":1,"name":"Fixture","version":"1","game":{"minecraft":"1.20.1","instance":"example","isolated":false},"loaders":[],"bundled_assets":false,"content_root":".minecraft"}"#).unwrap();
    zip.start_file(extra, options).unwrap();
    zip.write_all(b"fixture config").unwrap();
    zip.finish().unwrap();
}
#[test]
fn actual_native_zip_scan_hashes_every_body_and_preserves_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("input.zip");
    zip(&path, ".minecraft/config/example.toml");
    let before = std::fs::read(&path).unwrap();
    let scan = archive::scan_archive(
        filesystem::open_source(&path).unwrap(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        scan.entries[".minecraft/config/example.toml"].hash,
        format!("{:x}", Sha256::digest(b"fixture config"))
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}
#[test]
fn actual_zip_scan_rejects_traversal_and_cancellation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("input.zip");
    zip(&path, ".minecraft/../escape");
    assert!(archive::scan_archive(
        filesystem::open_source(&path).unwrap(),
        &AtomicBool::new(false)
    )
    .is_err());
    assert!(!root.join("escape").exists());
    zip(&path, ".minecraft/config/example.toml");
    assert!(archive::scan_archive(
        filesystem::open_source(&path).unwrap(),
        &AtomicBool::new(true)
    )
    .is_err());
}
#[cfg(unix)]
#[test]
fn unix_import_snapshot_wire_is_unchanged() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let meta = std::fs::metadata(temp.path()).unwrap();
    let stamp = filesystem::Stamp::of(&meta);
    let expected=format!("{{\"key\":{{\"dev\":{},\"ino\":{}}},\"mode\":{},\"size\":{},\"mtime\":[{},{}],\"ctime\":[{},{}]}}",meta.dev(),meta.ino(),meta.mode(),meta.len(),meta.mtime(),meta.mtime_nsec(),meta.ctime(),meta.ctime_nsec());
    assert_eq!(serde_json::to_string(&stamp).unwrap(), expected);
}

#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/launcher_local/filesystem.rs"]
pub(crate) mod local_filesystem;
mod launcher_local {
    pub(crate) use crate::local_filesystem as filesystem;
}
