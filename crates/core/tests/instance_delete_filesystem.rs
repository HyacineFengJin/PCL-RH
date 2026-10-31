//! Actual instance deletion filesystem source, without GUI or user data.
//! Both the production Linux implementation and the portable native adapter are
//! compiled; the latter's native tests run on each host (also on Linux).
type Result<T> = std::result::Result<T, String>;
const MAX_JOURNAL: usize = 64 * 1024 * 1024;
fn cancelled(cancel: Option<&std::sync::atomic::AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
        Err("实例删除已取消".into())
    } else {
        Ok(())
    }
}
#[allow(dead_code, unused_imports)]
#[path = "../../../apps/desktop/src-tauri/src/instance_delete/filesystem.rs"]
mod filesystem;

#[cfg(unix)]
#[test]
fn legacy_unix_identity_and_stamp_wire_order_remain_unchanged() {
    use std::os::unix::fs::MetadataExt;
    let fixture = tempfile::tempdir().unwrap();
    let meta = std::fs::metadata(fixture.path()).unwrap();
    let stamp = filesystem::Stamp::of(&meta);
    let expected = format!(
        "{{\"key\":{{\"dev\":{},\"ino\":{}}},\"size\":{},\"mode\":{},\"links\":{},\"modified\":[{},{}],\"changed\":[{},{}]}}",
        meta.dev(), meta.ino(), meta.len(), meta.mode(), meta.nlink(), meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec(),
    );
    assert_eq!(serde_json::to_string(&stamp).unwrap(), expected);
}
