use crate::file_ops::rename_noreplace_at;
use std::{
    ffi::OsStr,
    fs,
    path::Path,
    sync::{Arc, Barrier},
};

#[cfg(windows)]
#[path = "junction_fixture.rs"]
mod junction_fixture;

fn link_directory(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    junction_fixture::create(target, link);
}

fn fixture() -> tempfile::TempDir {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/relative-rename-tests");
    fs::create_dir_all(&directory).unwrap();
    tempfile::tempdir_in(directory).unwrap()
}

fn directory(path: &Path) -> fs::File {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap()
    }
    #[cfg(not(windows))]
    {
        fs::File::open(path).unwrap()
    }
}

fn move_entry(source: &fs::File, from: &str, target: &fs::File, to: &str) -> std::io::Result<()> {
    rename_noreplace_at(source, OsStr::new(from), target, OsStr::new(to))
}

#[test]
fn publishes_files_and_nested_directories_between_held_parents() {
    let root = fixture();
    let source_path = root.path().join("source");
    let target_path = root.path().join("target");
    fs::create_dir(&source_path).unwrap();
    fs::create_dir(&target_path).unwrap();
    let source = directory(&source_path);
    let target = directory(&target_path);
    fs::create_dir_all(source_path.join("暂存/nested")).unwrap();
    fs::write(
        source_path.join("暂存/nested/data"),
        b"complete staged content",
    )
    .unwrap();
    move_entry(&source, "暂存", &target, "实例 published").unwrap();
    assert!(!source_path.join("暂存").exists());
    assert_eq!(
        fs::read(target_path.join("实例 published/nested/data")).unwrap(),
        b"complete staged content"
    );
    fs::write(source_path.join("file"), b"verified bytes").unwrap();
    move_entry(&source, "file", &target, "metadata.json").unwrap();
    assert!(!source_path.join("file").exists());
    assert_eq!(
        fs::read(target_path.join("metadata.json")).unwrap(),
        b"verified bytes"
    );
}

#[test]
fn keeps_existing_files_and_empty_or_nonempty_directories() {
    for kind in ["file", "empty", "nonempty"] {
        let root = fixture();
        let parent = directory(root.path());
        fs::create_dir(root.path().join("source")).unwrap();
        fs::write(root.path().join("source/payload"), b"staged").unwrap();
        if kind == "file" {
            fs::write(root.path().join("target"), b"existing").unwrap();
        } else {
            fs::create_dir(root.path().join("target")).unwrap();
            if kind == "nonempty" {
                fs::write(root.path().join("target/old"), b"existing").unwrap();
            }
        }
        assert!(move_entry(&parent, "source", &parent, "target").is_err());
        assert_eq!(
            fs::read(root.path().join("source/payload")).unwrap(),
            b"staged"
        );
        match kind {
            "file" => assert_eq!(fs::read(root.path().join("target")).unwrap(), b"existing"),
            "empty" => assert_eq!(fs::read_dir(root.path().join("target")).unwrap().count(), 0),
            _ => assert_eq!(
                fs::read(root.path().join("target/old")).unwrap(),
                b"existing"
            ),
        }
    }
}

#[test]
fn detached_parents_cannot_be_redirected_by_replacement_paths() {
    let root = fixture();
    let source_path = root.path().join("source");
    let target_path = root.path().join("target");
    fs::create_dir(&source_path).unwrap();
    fs::create_dir(&target_path).unwrap();
    let source = directory(&source_path);
    let target = directory(&target_path);
    fs::write(source_path.join("staged"), b"owned").unwrap();
    fs::rename(&source_path, root.path().join("detached-source")).unwrap();
    fs::rename(&target_path, root.path().join("detached-target")).unwrap();
    fs::create_dir(&source_path).unwrap();
    fs::create_dir(&target_path).unwrap();
    fs::write(source_path.join("staged"), b"replacement source").unwrap();
    fs::write(target_path.join("published"), b"replacement target").unwrap();
    move_entry(&source, "staged", &target, "published").unwrap();
    assert_eq!(
        fs::read(root.path().join("detached-target/published")).unwrap(),
        b"owned"
    );
    assert!(!root.path().join("detached-source/staged").exists());
    assert_eq!(
        fs::read(source_path.join("staged")).unwrap(),
        b"replacement source"
    );
    assert_eq!(
        fs::read(target_path.join("published")).unwrap(),
        b"replacement target"
    );
}

#[test]
fn parent_path_retargeting_to_links_does_not_redirect_either_side() {
    let root = fixture();
    for name in ["source", "target", "outside-source", "outside-target"] {
        fs::create_dir(root.path().join(name)).unwrap();
    }
    let source_path = root.path().join("source");
    let target_path = root.path().join("target");
    let source = directory(&source_path);
    let target = directory(&target_path);
    fs::write(source_path.join("staged"), b"owned").unwrap();
    fs::write(root.path().join("outside-source/staged"), b"outside source").unwrap();
    fs::write(
        root.path().join("outside-target/published"),
        b"outside target",
    )
    .unwrap();
    fs::rename(&source_path, root.path().join("detached-source")).unwrap();
    fs::rename(&target_path, root.path().join("detached-target")).unwrap();
    link_directory(&root.path().join("outside-source"), &source_path);
    link_directory(&root.path().join("outside-target"), &target_path);
    move_entry(&source, "staged", &target, "published").unwrap();
    assert!(!root.path().join("detached-source/staged").exists());
    assert_eq!(
        fs::read(root.path().join("detached-target/published")).unwrap(),
        b"owned"
    );
    assert_eq!(
        fs::read(root.path().join("outside-source/staged")).unwrap(),
        b"outside source"
    );
    assert_eq!(
        fs::read(root.path().join("outside-target/published")).unwrap(),
        b"outside target"
    );
}

#[test]
fn simultaneous_publishers_keep_one_winner_and_the_losers_source() {
    let root = fixture();
    let parent = Arc::new(directory(root.path()));
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for value in ["first", "second"] {
        fs::write(root.path().join(value), value).unwrap();
        let parent = parent.clone();
        let start = barrier.clone();
        workers.push(std::thread::spawn(move || {
            start.wait();
            (value, move_entry(&parent, value, &parent, "target"))
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    for (value, result) in results {
        if result.is_ok() {
            assert!(!root.path().join(value).exists());
            assert_eq!(
                fs::read_to_string(root.path().join("target")).unwrap(),
                value
            );
        } else {
            assert_eq!(fs::read_to_string(root.path().join(value)).unwrap(), value);
        }
    }
}

#[test]
fn rejects_paths_or_nul_before_any_mutation_in_either_name() {
    let root = fixture();
    let parent = directory(root.path());
    fs::write(root.path().join("source"), b"original").unwrap();
    for bad in [
        "",
        ".",
        "..",
        "../outside",
        "/absolute",
        "nested/file",
        "nested\\file",
        "leaf/",
        "./leaf",
        "leaf\0tail",
    ] {
        assert_eq!(
            move_entry(&parent, "source", &parent, bad)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput,
            "target {bad:?}"
        );
        assert_eq!(
            move_entry(&parent, bad, &parent, "published")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput,
            "source {bad:?}"
        );
    }
    assert_eq!(fs::read(root.path().join("source")).unwrap(), b"original");
    assert!(!root.path().join("published").exists());
}

#[test]
fn rejects_missing_source_same_entry_and_nondirectory_parent() {
    let root = fixture();
    let parent = directory(root.path());
    let alias = directory(root.path());
    fs::write(root.path().join("source"), b"original").unwrap();
    assert!(move_entry(&parent, "source", &alias, "source").is_err());
    assert!(move_entry(&parent, "missing", &parent, "published").is_err());
    let regular = fs::File::open(root.path().join("source")).unwrap();
    assert!(move_entry(&regular, "source", &parent, "published").is_err());
    assert!(move_entry(&parent, "source", &regular, "published").is_err());
    assert_eq!(fs::read(root.path().join("source")).unwrap(), b"original");
    assert!(!root.path().join("published").exists());
}

#[cfg(unix)]
#[test]
fn existing_link_destinations_are_preserved_and_source_links_move_without_following() {
    use std::os::unix::fs::symlink;
    let root = fixture();
    let parent = directory(root.path());
    fs::create_dir(root.path().join("outside")).unwrap();
    fs::write(root.path().join("outside/retained"), b"outside").unwrap();
    fs::write(root.path().join("source"), b"original").unwrap();
    for referent in ["outside", "absent"] {
        symlink(referent, root.path().join("target")).unwrap();
        assert!(move_entry(&parent, "source", &parent, "target").is_err());
        assert_eq!(
            fs::read_link(root.path().join("target")).unwrap(),
            Path::new(referent)
        );
        fs::remove_file(root.path().join("target")).unwrap();
    }
    symlink("outside", root.path().join("link")).unwrap();
    move_entry(&parent, "link", &parent, "published").unwrap();
    assert_eq!(
        fs::read_link(root.path().join("published")).unwrap(),
        Path::new("outside")
    );
    assert_eq!(
        fs::read(root.path().join("outside/retained")).unwrap(),
        b"outside"
    );
    assert_eq!(fs::read(root.path().join("source")).unwrap(), b"original");
}

#[cfg(unix)]
#[test]
fn unix_non_utf8_leaf_is_not_lossily_converted() {
    use std::os::unix::ffi::OsStrExt;
    let root = fixture();
    let parent = directory(root.path());
    let from = OsStr::from_bytes(b"source-\xff");
    let to = OsStr::from_bytes(b"target-\xfe");
    fs::write(root.path().join(from), b"owned bytes").unwrap();
    rename_noreplace_at(&parent, from, &parent, to).unwrap();
    assert!(!root.path().join(from).exists());
    assert_eq!(fs::read(root.path().join(to)).unwrap(), b"owned bytes");
}

#[cfg(windows)]
#[test]
fn windows_rejects_stream_device_ambiguous_names_and_case_only_rename() {
    let root = fixture();
    let parent = directory(root.path());
    fs::write(root.path().join("source"), b"original").unwrap();
    for bad in [
        "C:relative",
        "file:stream",
        "NUL",
        "nul.txt",
        "CON .txt",
        "CoM1",
        "lpt9.txt",
        "COM¹",
        "LPT².txt",
        "CONOUT$",
        "tail.",
        "tail ",
        "wild*",
        "wild?",
        "control\n",
        "quote\"",
    ] {
        assert_eq!(
            move_entry(&parent, "source", &parent, bad)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput,
            "{bad:?}"
        );
        assert_eq!(
            move_entry(&parent, bad, &parent, "published")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput,
            "{bad:?}"
        );
    }
    assert!(move_entry(&parent, "source", &parent, "SOURCE").is_err());
    assert_eq!(fs::read(root.path().join("source")).unwrap(), b"original");
    assert!(!root.path().join("published").exists());
}

#[cfg(windows)]
#[test]
fn windows_junction_entries_are_not_followed_or_overwritten() {
    let root = fixture();
    let parent = directory(root.path());
    fs::create_dir(root.path().join("outside")).unwrap();
    fs::write(root.path().join("outside/retained"), b"outside").unwrap();
    fs::write(root.path().join("source"), b"staged").unwrap();
    for dangling in [false, true] {
        let referent = root
            .path()
            .join(if dangling { "absent" } else { "outside" });
        if dangling {
            fs::create_dir(&referent).unwrap();
        }
        link_directory(&referent, &root.path().join("target"));
        let link = fs::read_link(root.path().join("target")).unwrap();
        if dangling {
            fs::remove_dir(&referent).unwrap();
        }
        assert!(move_entry(&parent, "source", &parent, "target").is_err());
        assert_eq!(fs::read_link(root.path().join("target")).unwrap(), link);
        fs::remove_dir(root.path().join("target")).unwrap();
    }
    link_directory(&root.path().join("outside"), &root.path().join("link"));
    let original = fs::read_link(root.path().join("link")).unwrap();
    move_entry(&parent, "link", &parent, "published").unwrap();
    assert!(fs::symlink_metadata(root.path().join("link")).is_err());
    assert_eq!(
        fs::read_link(root.path().join("published")).unwrap(),
        original
    );
    assert_eq!(
        fs::read(root.path().join("outside/retained")).unwrap(),
        b"outside"
    );
    assert_eq!(fs::read(root.path().join("source")).unwrap(), b"staged");
}

#[cfg(windows)]
#[test]
fn windows_utf16_leaf_is_not_lossily_converted() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let root = fixture();
    let parent = directory(root.path());
    let from = OsString::from_wide(&[115, 114, 99, 45, 0xd800]);
    let to = OsString::from_wide(&[100, 115, 116, 45, 0xdfff]);
    fs::write(root.path().join(&from), b"owned bytes").unwrap();
    rename_noreplace_at(&parent, &from, &parent, &to).unwrap();
    assert!(!root.path().join(&from).exists());
    assert_eq!(fs::read(root.path().join(&to)).unwrap(), b"owned bytes");
}
