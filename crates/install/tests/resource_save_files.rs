//! Real captured-target filesystem provider and official digest implementation.
//! No network, Tauri runtime, game, user data or authority resolver is invoked.
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/resource_save/files.rs"]
mod files;
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/resource_save/model.rs"]
mod model;
#[allow(dead_code)]
#[path = "../../../apps/desktop/src-tauri/src/resource_digest.rs"]
mod resource_digest;
use model::{SavePlan, SaveRequest, SaveSource};
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use sha2::{Digest, Sha512};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
type Result<T> = std::result::Result<T, String>;
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err("资源文件下载已取消".into())
    } else {
        Ok(())
    }
}
// The harness supplies the parent's strict source/algorithm selection. Hashing,
// official digest verification, staging and publication are production code.
mod authority {
    use super::*;
    pub fn expected_digest(plan: &SavePlan) -> Result<resource_digest::ExpectedDigest<'_>> {
        let digest = match plan.request.source {
            SaveSource::Modrinth if plan.sha1.is_none() => {
                resource_digest::ExpectedDigest::Sha512(&plan.sha512)
            }
            SaveSource::CurseForge if plan.sha512.is_empty() => {
                resource_digest::ExpectedDigest::Sha1(
                    plan.sha1.as_deref().ok_or("CurseForge 缺少 SHA1 校验值")?,
                )
            }
            _ => return Err("保存文件的来源与校验算法不匹配".into()),
        };
        digest.validate()?;
        Ok(digest)
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    chosen: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let directory = Directory::open_absolute(&root).unwrap();
        for name in ["project", "chosen"] {
            directory
                .create_private(&Leaf::new(name).unwrap(), Traversal::AllowMounts)
                .unwrap();
        }
        Self {
            project: root.join("project"),
            chosen: root.join("chosen"),
            root,
            _temp: temp,
        }
    }
    fn path(&self) -> PathBuf {
        self.chosen.join("selected.jar")
    }
    fn capture(&self) -> files::SaveTarget {
        files::SaveTarget::capture(&self.project, &self.path()).unwrap()
    }
}
fn plan(bytes: &[u8], sha1: bool) -> SavePlan {
    SavePlan {
        request: SaveRequest {
            source: if sha1 {
                SaveSource::CurseForge
            } else {
                SaveSource::Modrinth
            },
            project_id: "fixture".into(),
            version_id: "fixture".into(),
            file_name: "fixture.jar".into(),
        },
        project_title: "fixture".into(),
        version_name: "fixture".into(),
        file_name: "fixture.jar".into(),
        size: bytes.len() as u64,
        sha512: if sha1 {
            String::new()
        } else {
            format!("{:x}", Sha512::digest(bytes))
        },
        sha1: sha1.then(|| format!("{:x}", sha1::Sha1::digest(bytes))),
        revision: "fixture".into(),
    }
}
fn verified(target: &files::SaveTarget, bytes: &[u8], sha1: bool) -> files::VerifiedAnonymous {
    let mut stage = target.anonymous().unwrap();
    files::staging_file(&mut stage).write_all(bytes).unwrap();
    files::verify_anonymous(stage, &plan(bytes, sha1), &AtomicBool::new(false)).unwrap()
}
#[test]
fn official_sha512_and_sha1_publish_only_selected_target_and_preserve_project() {
    for sha1 in [false, true] {
        let f = Fixture::new();
        let target = f.capture();
        let file = verified(&target, b"official fixture", sha1);
        assert_eq!(
            file.sha512,
            format!("{:x}", Sha512::digest(b"official fixture"))
        );
        assert!(target.publish(&file).unwrap().is_none());
        assert_eq!(fs::read(f.path()).unwrap(), b"official fixture");
        assert_eq!(fs::read_dir(&f.chosen).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&f.project).unwrap().count(), 0);
    }
}
#[test]
fn hash_size_and_cancellation_fail_before_publication_and_clean_only_owned_stage() {
    for case in 0..3 {
        let f = Fixture::new();
        let target = f.capture();
        let mut stage = target.anonymous().unwrap();
        files::staging_file(&mut stage)
            .write_all(if case == 0 { b"bad!" } else { b"good" })
            .unwrap();
        let mut expected = plan(b"good", false);
        if case == 1 {
            expected.size += 1;
        }
        assert!(files::verify_anonymous(stage, &expected, &AtomicBool::new(case == 2)).is_err());
        assert!(!f.path().exists());
        assert_eq!(fs::read_dir(&f.chosen).unwrap().count(), 0);
    }
}
#[test]
fn preexisting_file_directory_hardlink_and_final_collision_never_overwrite() {
    for case in 0..3 {
        let f = Fixture::new();
        let original = f.root.join("original");
        fs::write(&original, b"original").unwrap();
        match case {
            0 => fs::write(f.path(), b"existing").unwrap(),
            1 => fs::create_dir(f.path()).unwrap(),
            _ => fs::hard_link(&original, f.path()).unwrap(),
        }
        assert!(files::SaveTarget::capture(&f.project, &f.path()).is_err());
        assert_eq!(fs::read(original).unwrap(), b"original");
    }
    let f = Fixture::new();
    let target = f.capture();
    let file = verified(&target, b"candidate", false);
    fs::write(f.path(), b"external").unwrap();
    assert!(target.publish(&file).is_err());
    drop(file);
    assert_eq!(fs::read(f.path()).unwrap(), b"external");
    assert_eq!(fs::read_dir(&f.chosen).unwrap().count(), 1);
}
#[test]
fn abandoned_download_and_verified_gate_rejection_remove_owned_stages() {
    let f = Fixture::new();
    let target = f.capture();
    let mut stage = target.anonymous().unwrap();
    files::staging_file(&mut stage)
        .write_all(b"partial body")
        .unwrap();
    drop(stage);
    assert_eq!(fs::read_dir(&f.chosen).unwrap().count(), 0);
    drop(verified(&target, b"verified but gate rejected", false));
    assert_eq!(fs::read_dir(&f.chosen).unwrap().count(), 0);
}
#[test]
fn altered_sealed_bytes_are_rejected_without_publishing_or_touching_original() {
    let f = Fixture::new();
    let target = f.capture();
    let mut stage = target.anonymous().unwrap();
    files::staging_file(&mut stage).write_all(b"good").unwrap();
    let mut escaped = stage.try_clone().unwrap();
    let file =
        files::verify_anonymous(stage, &plan(b"good", false), &AtomicBool::new(false)).unwrap();
    escaped.seek(SeekFrom::Start(0)).unwrap();
    escaped.write_all(b"bad!").unwrap();
    assert!(target.publish(&file).is_err());
    assert!(!f.path().exists());
    drop(file);
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        fs::read_dir(&f.chosen).unwrap().count(),
        1,
        "changed named stage is retained as evidence"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(fs::read_dir(&f.chosen).unwrap().count(), 0);
}
#[test]
fn published_bytes_survive_reported_durability_failure() {
    let f = Fixture::new();
    let target = f.capture();
    let file = verified(&target, b"saved", false);
    let warning = target.publish_with_failed_sync(&file).unwrap().unwrap();
    assert!(warning.contains("持久性未确认"));
    assert_eq!(fs::read(f.path()).unwrap(), b"saved");
}
#[cfg(unix)]
#[test]
fn symlink_target_or_ancestor_and_replaced_visible_parent_are_rejected() {
    let f = Fixture::new();
    let outside = f.root.join("outside");
    fs::write(&outside, b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, f.path()).unwrap();
    assert!(files::SaveTarget::capture(&f.project, &f.path()).is_err());
    fs::remove_file(f.path()).unwrap();
    let target = f.capture();
    let file = verified(&target, b"candidate", false);
    fs::rename(&f.chosen, f.root.join("held")).unwrap();
    std::os::unix::fs::symlink(f.root.join("held"), &f.chosen).unwrap();
    assert!(target.publish(&file).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"outside");
    assert!(!f.root.join("held/selected.jar").exists());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn abandoned_named_workspace_with_unknown_entry_is_preserved_without_scanning_cleanup() {
    let f = Fixture::new();
    let target = f.capture();
    let mut stage = target.anonymous().unwrap();
    files::staging_file(&mut stage)
        .write_all(b"partial")
        .unwrap();
    let workspace = fs::read_dir(&f.chosen)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(workspace.join("unknown-evidence"), b"external evidence").unwrap();
    drop(stage);
    assert!(!f.path().exists());
    assert_eq!(
        fs::read(workspace.join("unknown-evidence")).unwrap(),
        b"external evidence"
    );
    assert!(fs::read_dir(&workspace).unwrap().count() >= 2);
}

#[cfg(unix)]
#[test]
fn ordinary_readable_save_parent_is_not_chmodded_or_required_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    fs::set_permissions(&f.chosen, fs::Permissions::from_mode(0o755)).unwrap();
    let target = f.capture();
    let file = verified(&target, b"public chooser parent", false);
    target.publish(&file).unwrap();
    assert_eq!(
        fs::metadata(&f.chosen).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(fs::read(f.path()).unwrap(), b"public chooser parent");
}
