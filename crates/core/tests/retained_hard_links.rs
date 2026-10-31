use pcl_core::{
    file_ops::hard_link_noreplace_at,
    retained_fs::{self, Directory, Leaf, Traversal},
};
use std::{
    fs,
    io::{Read, Write},
};

fn fixture() -> (tempfile::TempDir, Directory, Directory, Directory) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Directory::open_absolute(&tmp.path().canonicalize().unwrap()).unwrap();
    let from = root
        .create_private(&Leaf::new("from").unwrap(), Traversal::AllowMounts)
        .unwrap();
    let to = root
        .create_private(&Leaf::new("to").unwrap(), Traversal::AllowMounts)
        .unwrap();
    let mut file = from.create_file(&Leaf::new("original").unwrap()).unwrap();
    file.write_all(b"original bytes").unwrap();
    file.sync_all().unwrap();
    drop(file);
    (tmp, root, from, to)
}
fn link(from: &Directory, source: &str, to: &Directory, target: &str) -> std::io::Result<()> {
    hard_link_noreplace_at(from.file(), source.as_ref(), to.file(), target.as_ref())
}
#[test]
fn actual_link_preserves_source_and_shares_full_native_identity() {
    let (_tmp, _root, from, to) = fixture();
    let original = from
        .open_regular(&Leaf::new("original").unwrap(), retained_fs::Links::Single)
        .unwrap();
    let identity = retained_fs::metadata(&original).unwrap().identity;
    link(&from, "original", &to, "linked-你好").unwrap();
    let mut published = to
        .open_regular(&Leaf::new("linked-你好").unwrap(), retained_fs::Links::Any)
        .unwrap();
    assert_eq!(
        retained_fs::metadata(&published).unwrap().identity,
        identity
    );
    assert_eq!(retained_fs::metadata(&original).unwrap().links, 2);
    let mut bytes = Vec::new();
    published.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original bytes");
    assert!(from
        .entry(&Leaf::new("original").unwrap())
        .unwrap()
        .is_some());
}
#[test]
fn existing_files_directories_aliases_and_invalid_leaves_are_never_replaced() {
    let (tmp, _root, from, to) = fixture();
    to.create_file(&Leaf::new("occupied").unwrap())
        .unwrap()
        .write_all(b"other original")
        .unwrap();
    to.create_private(&Leaf::new("directory").unwrap(), Traversal::AllowMounts)
        .unwrap();
    for name in ["occupied", "directory", "../escape", "nested/leaf"] {
        assert!(link(&from, "original", &to, name).is_err(), "{name}");
    }
    assert!(link(&from, "original", &from, "original").is_err());
    link(&from, "original", &to, "alias").unwrap();
    assert!(link(&from, "original", &to, "alias").is_err());
    assert!(link(&to, "directory", &from, "bad-directory-link").is_err());
    assert_eq!(
        fs::read(tmp.path().join("to/occupied")).unwrap(),
        b"other original"
    );
    assert_eq!(
        fs::read(tmp.path().join("from/original")).unwrap(),
        b"original bytes"
    );
    assert!(!tmp.path().join("escape").exists());
}
#[test]
fn retained_parent_names_remain_authoritative_after_visible_directory_substitution() {
    let (tmp, root, from, to) = fixture();
    pcl_core::file_ops::rename_noreplace_at(
        root.file(),
        "to".as_ref(),
        root.file(),
        "parked".as_ref(),
    )
    .unwrap();
    root.create_private(&Leaf::new("to").unwrap(), Traversal::AllowMounts)
        .unwrap();
    link(&from, "original", &to, "linked").unwrap();
    assert_eq!(
        fs::read(tmp.path().join("parked/linked")).unwrap(),
        b"original bytes"
    );
    assert!(!tmp.path().join("to/linked").exists());
}

#[test]
fn authorized_generated_cleanup_removes_only_one_link_and_default_policy_stays_strict() {
    let (tmp, _root, from, to) = fixture();
    link(&from, "original", &to, "linked").unwrap();
    let target = Leaf::new("linked").unwrap();
    let held = to.open_regular(&target, retained_fs::Links::Any).unwrap();
    let expected = retained_fs::metadata(&held).unwrap();
    assert_eq!(expected.links, 2);
    assert!(to.remove_workspace_entry(&target, &expected).is_err());
    assert!(matches!(
        to.remove_generated_link(&target, &expected).unwrap(),
        retained_fs::WorkspaceRemoval::Removed
    ));
    assert_eq!(
        fs::read(tmp.path().join("from/original")).unwrap(),
        b"original bytes"
    );
    assert_eq!(retained_fs::metadata(&held).unwrap().links, 1);
    assert!(!tmp.path().join("to/linked").exists());
}

#[test]
fn generated_cleanup_rejects_replacement_without_reauthorizing_the_new_name() {
    let (tmp, _root, from, to) = fixture();
    link(&from, "original", &to, "linked").unwrap();
    let target = Leaf::new("linked").unwrap();
    let original = to.open_regular(&target, retained_fs::Links::Any).unwrap();
    let expected = retained_fs::metadata(&original).unwrap();
    pcl_core::file_ops::rename_noreplace_at(
        to.file(),
        "linked".as_ref(),
        to.file(),
        "parked".as_ref(),
    )
    .unwrap();
    to.create_file(&target)
        .unwrap()
        .write_all(b"replacement bytes")
        .unwrap();
    assert!(to.remove_generated_link(&target, &expected).is_err());
    assert_eq!(
        fs::read(tmp.path().join("to/linked")).unwrap(),
        b"replacement bytes"
    );
    assert_eq!(
        fs::read(tmp.path().join("to/parked")).unwrap(),
        b"original bytes"
    );
}
#[cfg(unix)]
#[test]
fn symlink_source_and_destination_are_never_followed() {
    let (tmp, _root, from, to) = fixture();
    std::os::unix::fs::symlink("original", tmp.path().join("from/symlink")).unwrap();
    std::os::unix::fs::symlink("missing", tmp.path().join("to/symlink")).unwrap();
    assert!(link(&from, "symlink", &to, "not-created").is_err());
    assert!(link(&from, "original", &to, "symlink").is_err());
    assert!(!tmp.path().join("to/not-created").exists());
    assert_eq!(
        fs::read_link(tmp.path().join("to/symlink")).unwrap(),
        std::path::Path::new("missing")
    );
}
