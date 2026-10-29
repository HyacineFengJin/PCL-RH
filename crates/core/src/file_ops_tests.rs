use super::*;
use std::fs;

fn fixture() -> tempfile::TempDir {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/native-file-ops-tests");
    fs::create_dir_all(&directory).unwrap();
    tempfile::tempdir_in(directory).unwrap()
}
#[test]
fn directory_publication_preserves_nested_contents_and_unicode_names() {
    let root = fixture();
    let source = root.path().join("private staged");
    let target = root.path().join("实例 published");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("nested/data"), b"complete staged content").unwrap();
    rename_noreplace(&source, &target).unwrap();
    assert!(!source.exists());
    assert_eq!(
        fs::read(target.join("nested/data")).unwrap(),
        b"complete staged content"
    );
}
#[test]
fn existing_empty_nonempty_or_file_destination_never_changes() {
    for kind in ["empty", "nonempty", "file"] {
        let root = fixture();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("payload"), b"staged").unwrap();
        match kind {
            "file" => fs::write(&target, b"existing file").unwrap(),
            _ => {
                fs::create_dir(&target).unwrap();
                if kind == "nonempty" {
                    fs::write(target.join("old"), b"original").unwrap();
                }
            }
        }
        assert!(rename_noreplace(&source, &target).is_err());
        assert_eq!(fs::read(source.join("payload")).unwrap(), b"staged");
        match kind {
            "file" => assert_eq!(fs::read(&target).unwrap(), b"existing file"),
            "empty" => assert_eq!(fs::read_dir(&target).unwrap().count(), 0),
            _ => assert_eq!(fs::read(target.join("old")).unwrap(), b"original"),
        }
    }
}
#[test]
fn missing_source_and_same_path_do_not_publish_or_remove_entries() {
    let root = fixture();
    let source = root.path().join("source");
    let target = root.path().join("target");
    assert!(rename_noreplace(&source, &target).is_err());
    assert!(!target.exists());
    fs::create_dir(&source).unwrap();
    fs::write(source.join("retained"), b"original").unwrap();
    assert!(rename_noreplace(&source, &source).is_err());
    assert_eq!(fs::read(source.join("retained")).unwrap(), b"original");
}
#[test]
fn simultaneous_publishers_have_one_winner_and_preserve_the_loser() {
    use std::sync::{Arc, Barrier};
    let root = fixture();
    let target = root.path().join("target");
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for value in ["first", "second"] {
        let source = root.path().join(value);
        fs::create_dir(&source).unwrap();
        fs::write(source.join("payload"), value).unwrap();
        let target = target.clone();
        let start = barrier.clone();
        workers.push(std::thread::spawn(move || {
            start.wait();
            (value, source.clone(), rename_noreplace(&source, &target))
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|(_, _, result)| result.is_ok())
            .count(),
        1
    );
    for (value, source, result) in results {
        if result.is_ok() {
            assert!(!source.exists());
            assert_eq!(fs::read_to_string(target.join("payload")).unwrap(), value);
        } else {
            assert_eq!(fs::read_to_string(source.join("payload")).unwrap(), value);
        }
    }
}
#[cfg(unix)]
#[test]
fn an_existing_symlink_destination_is_not_followed_or_replaced() {
    let root = fixture();
    let source = root.path().join("source");
    let outside = root.path().join("outside");
    let target = root.path().join("target");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("retained"), b"original").unwrap();
    std::os::unix::fs::symlink(&outside, &target).unwrap();
    assert!(rename_noreplace(&source, &target).is_err());
    assert!(source.is_dir());
    assert!(fs::symlink_metadata(&target)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(outside.join("retained")).unwrap(), b"original");
}
