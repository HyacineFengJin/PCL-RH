//! Actual read-only project admission for deletion/recovery markers.
//! Marker wire, bounded literal reads and identity validation are shared with
//! the deletion service; admission never runs mutation/recovery or opens a game
//! root. An offline root therefore cannot clear an existing project marker.
use super::{
    filesystem::{absolute, canonical, name_ok, read_bytes, Dir, FileToken, Key},
    Result,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
pub(super) const MARKER: &str = "instance-delete-pending.json";
pub(super) const READY: &str = "存在未完成的实例删除或恢复，请先恢复未完成操作";

pub(super) fn scope_ok(root_id: &str) -> Result<()> {
    if root_id.is_empty()
        || root_id.len() > 128
        || !root_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("游戏目录标识无效".into());
    }
    Ok(())
}
pub(super) fn operation_ok(name: &str) -> bool {
    name.len() <= 80
        && name.strip_prefix("d-").is_some_and(|tail| {
            let parts: Vec<_> = tail.split('-').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_hexdigit()))
        })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Marker {
    pub(super) schema: u32,
    pub(super) operation_id: String,
    pub(super) root_id: String,
    pub(super) id: String,
    pub(super) root: PathBuf,
    pub(super) project: PathBuf,
    pub(super) root_key: Key,
    pub(super) project_key: Key,
    pub(super) storage_key: Key,
    pub(super) operation_key: Key,
    pub(super) file_key: Key,
}
pub(super) fn app(project: &Path, create: bool) -> Result<Option<Dir>> {
    let directory = Dir::open(project)?;
    if create {
        directory.ensure(".pcl-rust").map(Some)
    } else {
        directory.optional(".pcl-rust")
    }
}
pub(super) fn read_marker(project: &Path) -> Result<Option<(Marker, FileToken)>> {
    let Some(app) = app(project, false)? else {
        return Ok(None);
    };
    if app.stat(MARKER)?.is_none() {
        return Ok(None);
    }
    let (token, bytes) = read_bytes(&app, MARKER, 64 * 1024)?;
    let marker: Marker =
        serde_json::from_slice(&bytes).map_err(|_| "实例删除待恢复标记损坏，已保留")?;
    name_ok(&marker.id)?;
    scope_ok(&marker.root_id)?;
    absolute(&marker.root)?;
    absolute(&marker.project)?;
    if marker.schema != 1
        || !operation_ok(&marker.operation_id)
        || marker.project != project
        || marker.project_key != Dir::open(project)?.key()?
        || marker.file_key != token.stamp.key
    {
        return Err("实例删除待恢复标记范围或身份无效，已保留".into());
    }
    Ok(Some((marker, token)))
}
pub(crate) fn pending_root(project: &Path) -> Result<Option<PathBuf>> {
    let project = canonical(project)?;
    read_marker(&project).map(|m| m.map(|(m, _)| m.root))
}
pub(crate) fn ensure_project_ready(project: &Path) -> Result<()> {
    if pending_root(project)?.is_some() {
        Err(READY.into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::filesystem::anonymous_key;
    use super::*;
    use pcl_core::retained_fs::{Directory, Leaf, Traversal};
    use std::{fs, io::Write};
    struct Fixture {
        _temporary: tempfile::TempDir,
        project: PathBuf,
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let path = temporary.path().canonicalize().unwrap();
            let parent = Directory::open_absolute(&path).unwrap();
            parent
                .create_private(&Leaf::new("project").unwrap(), Traversal::AllowMounts)
                .unwrap();
            parent
                .create_private(&Leaf::new("game").unwrap(), Traversal::AllowMounts)
                .unwrap();
            Self {
                _temporary: temporary,
                project: path.join("project"),
                root: path.join("game"),
            }
        }
        fn marker(&self, alter: impl FnOnce(&mut Marker)) -> Vec<u8> {
            let directory = app(&self.project, true).unwrap().unwrap();
            let mut file = directory.anonymous().unwrap();
            let app_key = directory.key().unwrap();
            let mut marker = Marker {
                schema: 1,
                operation_id: "d-1-2-3".into(),
                root_id: "root-a".into(),
                id: "Old".into(),
                root: self.root.clone(),
                project: self.project.clone(),
                root_key: Dir::open(&self.root).unwrap().key().unwrap(),
                project_key: Dir::open(&self.project).unwrap().key().unwrap(),
                storage_key: app_key.clone(),
                operation_key: app_key,
                file_key: anonymous_key(&file).unwrap(),
            };
            alter(&mut marker);
            let bytes = serde_json::to_vec(&marker).unwrap();
            file.write_all(&bytes).unwrap();
            file.sync_all().unwrap();
            directory.link(&file, MARKER).unwrap();
            bytes
        }
    }
    #[test]
    fn actual_read_gate_without_marker_has_no_filesystem_side_effects() {
        let fixture = Fixture::new();
        assert_eq!(pending_root(&fixture.project).unwrap(), None);
        ensure_project_ready(&fixture.project).unwrap();
        assert_eq!(fs::read_dir(&fixture.project).unwrap().count(), 0);
    }
    #[test]
    fn actual_read_gate_keeps_valid_marker_blocking_when_game_root_is_offline() {
        let fixture = Fixture::new();
        let bytes = fixture.marker(|_| {});
        let offline = fixture.root.with_file_name("offline-game");
        fs::rename(&fixture.root, &offline).unwrap();
        assert_eq!(
            pending_root(&fixture.project).unwrap(),
            Some(fixture.root.clone())
        );
        assert_eq!(ensure_project_ready(&fixture.project).unwrap_err(), READY);
        assert_eq!(
            fs::read(fixture.project.join(".pcl-rust").join(MARKER)).unwrap(),
            bytes
        );
    }
    #[test]
    fn actual_read_gate_rejects_future_and_invalid_markers_without_repair() {
        for case in 0..3 {
            let fixture = Fixture::new();
            let bytes = fixture.marker(|marker| match case {
                0 => marker.schema = 2,
                1 => marker.operation_id = "d-1-2".into(),
                _ => marker.root_id = "invalid/root".into(),
            });
            assert!(pending_root(&fixture.project).is_err());
            assert!(ensure_project_ready(&fixture.project).is_err());
            assert_eq!(
                fs::read(fixture.project.join(".pcl-rust").join(MARKER)).unwrap(),
                bytes
            );
        }
    }
    #[test]
    fn actual_read_gate_rejects_replaced_self_identifying_marker_and_keeps_both_files() {
        let fixture = Fixture::new();
        let bytes = fixture.marker(|_| {});
        let path = fixture.project.join(".pcl-rust").join(MARKER);
        let preserved = path.with_file_name("preserved-marker.json");
        fs::rename(&path, &preserved).unwrap();
        let directory = app(&fixture.project, false).unwrap().unwrap();
        let mut replacement = directory.anonymous().unwrap();
        replacement.write_all(&bytes).unwrap();
        replacement.sync_all().unwrap();
        directory.link(&replacement, MARKER).unwrap();
        assert!(pending_root(&fixture.project).is_err());
        assert!(ensure_project_ready(&fixture.project).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(fs::read(preserved).unwrap(), bytes);
    }
    #[cfg(unix)]
    #[test]
    fn original_marker_field_order_and_schema_are_preserved_byte_for_byte() {
        let fixture = Fixture::new();
        let bytes = fixture.marker(|_| {});
        let marker: Marker = serde_json::from_slice(&bytes).unwrap();
        let encoded = |key: &Key| serde_json::to_string(key).unwrap();
        let expected=format!("{{\"schema\":1,\"operation_id\":\"d-1-2-3\",\"root_id\":\"root-a\",\"id\":\"Old\",\"root\":{},\"project\":{},\"root_key\":{},\"project_key\":{},\"storage_key\":{},\"operation_key\":{},\"file_key\":{}}}",
            serde_json::to_string(&marker.root).unwrap(),serde_json::to_string(&marker.project).unwrap(),encoded(&marker.root_key),encoded(&marker.project_key),encoded(&marker.storage_key),encoded(&marker.operation_key),encoded(&marker.file_key));
        assert_eq!(bytes, expected.as_bytes());
    }
}
