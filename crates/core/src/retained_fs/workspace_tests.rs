use super::*;
use std::{
    fs,
    io::{Read, Write},
};

fn fixture() -> tempfile::TempDir {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/retained-workspace-tests");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap()
}
fn leaf(name: &str) -> Leaf {
    Leaf::new(name).unwrap()
}
fn directory(path: &Path) -> Directory {
    Directory::open_absolute(path).unwrap()
}
fn removed(result: io::Result<WorkspaceRemoval>) {
    match result.unwrap() {
        WorkspaceRemoval::Removed => {}
        WorkspaceRemoval::MayHaveRemoved { error } => {
            panic!("workspace cleanup was uncertain: {error}")
        }
    }
}

#[cfg(windows)]
#[path = "../file_ops/junction_fixture.rs"]
mod junction_fixture;
fn link(target: &Path, path: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path).unwrap();
    #[cfg(windows)]
    junction_fixture::create(target, path);
}

#[test]
fn workspace_names_use_fixed_prefix_full_randomness_and_private_exclusive_creation() {
    let fixture = fixture();
    let parent = directory(fixture.path());
    let mut names = std::collections::BTreeSet::new();
    for _ in 0..16 {
        let (name, child) = parent.create_private_workspace().unwrap();
        let text = name.as_os_str().to_str().unwrap();
        let suffix = text.strip_prefix(".install-").unwrap();
        assert_eq!(suffix.len(), 32);
        assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(names.insert(text.to_owned()));
        let metadata = child.metadata().unwrap();
        metadata.require_private().unwrap();
        parent.recheck(&name, &metadata.identity).unwrap();
        assert!(parent
            .create_private(&name, Traversal::SameFilesystem)
            .is_err());
        removed(parent.remove_workspace_entry(&name, &metadata));
    }
    assert!(parent.names(32).unwrap().is_empty());
}

#[test]
fn authorized_file_and_empty_directory_cleanup_retains_open_objects() {
    let fixture = fixture();
    let parent = directory(fixture.path());
    let (name, workspace) = parent.create_private_workspace().unwrap();
    let file_name = leaf("generated");
    let mut file = workspace.create_file(&file_name).unwrap();
    file.write_all(b"temporary bytes").unwrap();
    drop(file); // The writable stage-style handle denies Windows delete sharing.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Privacy belongs to the enclosing workspace; ordinary generated leaf
        // permissions must not be rewritten or prevent authorized cleanup.
        fs::set_permissions(
            fixture.path().join(name.as_os_str()).join("generated"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    let mut held_file = workspace.open_regular(&file_name, Links::Single).unwrap();
    let file_metadata = metadata(&held_file).unwrap();
    removed(workspace.remove_workspace_entry(&file_name, &file_metadata));
    let mut bytes = Vec::new();
    held_file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"temporary bytes");
    assert!(workspace.entry(&file_name).unwrap().is_none());
    let workspace_metadata = workspace.metadata().unwrap();
    removed(parent.remove_workspace_entry(&name, &workspace_metadata));
    assert_eq!(workspace.identity().unwrap(), workspace_metadata.identity);
    assert!(parent.entry(&name).unwrap().is_none());
}

#[test]
fn wrong_identity_hard_links_special_nodes_and_nonempty_directories_are_retained() {
    let fixture = fixture();
    let parent = directory(fixture.path());
    let (workspace_name, workspace) = parent.create_private_workspace().unwrap();
    let workspace_path = fixture.path().join(workspace_name.as_os_str());
    let file_name = leaf("file");
    let mut file = workspace.create_file(&file_name).unwrap();
    file.write_all(b"original").unwrap();
    let expected = metadata(&file).unwrap();
    drop(file);
    fs::rename(workspace_path.join("file"), workspace_path.join("retained")).unwrap();
    fs::write(workspace_path.join("file"), b"replacement").unwrap();
    assert!(workspace
        .remove_workspace_entry(&file_name, &expected)
        .is_err());
    assert_eq!(
        fs::read(workspace_path.join("file")).unwrap(),
        b"replacement"
    );
    fs::hard_link(
        workspace_path.join("file"),
        fixture.path().join("outside-alias"),
    )
    .unwrap();
    let linked = workspace.entry(&file_name).unwrap().unwrap();
    assert!(workspace
        .remove_workspace_entry(&file_name, &linked)
        .is_err());
    assert_eq!(
        fs::read(fixture.path().join("outside-alias")).unwrap(),
        b"replacement"
    );
    let child = workspace
        .create_private(&leaf("nonempty"), Traversal::SameFilesystem)
        .unwrap();
    drop(child.create_file(&leaf("keep")).unwrap());
    assert!(workspace
        .remove_workspace_entry(&leaf("nonempty"), &child.metadata().unwrap())
        .is_err());
    assert!(child.entry(&leaf("keep")).unwrap().is_some());
    assert!(workspace
        .remove_workspace_entry(&leaf("nonempty"), &linked)
        .is_err());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let name =
            std::ffi::CString::new(workspace_path.join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let special = workspace.entry(&leaf("fifo")).unwrap().unwrap();
        assert!(workspace
            .remove_workspace_entry(&leaf("fifo"), &special)
            .is_err());
    }
}

#[test]
fn links_are_never_followed_or_removed_as_workspace_nodes() {
    let fixture = fixture();
    let outside = fixture.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"outside").unwrap();
    let parent = directory(fixture.path());
    let (name, workspace) = parent.create_private_workspace().unwrap();
    let link_path = fixture.path().join(name.as_os_str()).join("linked");
    link(&outside, &link_path);
    let metadata = workspace.entry(&leaf("linked")).unwrap().unwrap();
    assert!(workspace
        .remove_workspace_entry(&leaf("linked"), &metadata)
        .is_err());
    assert!(fs::symlink_metadata(&link_path).is_ok());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
    assert_eq!(
        directory(&outside).names(8).unwrap(),
        vec![OsString::from("sentinel")]
    );
}

#[test]
fn workspace_creation_and_cleanup_ignore_replaced_parent_paths() {
    for replace_with_link in [false, true] {
        let fixture = fixture();
        let original = fixture.path().join("parent");
        let moved = fixture.path().join("held-parent");
        let outside = fixture.path().join("outside");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"outside").unwrap();
        let parent = directory(&original);
        fs::rename(&original, &moved).unwrap();
        if replace_with_link {
            link(&outside, &original);
        } else {
            fs::create_dir(&original).unwrap();
        }
        let (name, workspace) = parent.create_private_workspace().unwrap();
        assert!(moved.join(name.as_os_str()).is_dir());
        assert!(!original.join(name.as_os_str()).exists());
        let mut file = workspace.create_file(&leaf("owned")).unwrap();
        file.write_all(b"owned bytes").unwrap();
        let file_metadata = metadata(&file).unwrap();
        drop(file);
        removed(workspace.remove_workspace_entry(&leaf("owned"), &file_metadata));
        removed(parent.remove_workspace_entry(&name, &workspace.metadata().unwrap()));
        assert!(fs::read_dir(&moved).unwrap().next().is_none());
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
        assert_eq!(
            directory(&outside).names(8).unwrap(),
            vec![OsString::from("sentinel")]
        );
    }
}

#[test]
fn uncertain_cleanup_retains_recreated_names_and_reports_real_flush_failure() {
    for recreate in [false, true] {
        let fixture = fixture();
        let parent = directory(fixture.path());
        let (_, workspace) = parent.create_private_workspace().unwrap();
        let name = leaf("cleanup");
        let mut file = workspace.create_file(&name).unwrap();
        file.write_all(b"owned temporary bytes").unwrap();
        let expected = metadata(&file).unwrap();
        drop(file);
        let outcome = workspace
            .remove_workspace_entry_with_sync(&name, &expected, |parent| {
                assert!(parent.entry(&name)?.is_none());
                if recreate {
                    let mut replacement = parent.create_file(&name)?;
                    replacement.write_all(b"new entry must be retained")?;
                    drop(replacement);
                    parent.sync()
                } else {
                    Err(io::Error::other("injected native flush failure"))
                }
            })
            .unwrap();
        assert!(matches!(outcome, WorkspaceRemoval::MayHaveRemoved { .. }));
        if recreate {
            let mut replacement = workspace.open_regular(&name, Links::Single).unwrap();
            let mut bytes = Vec::new();
            replacement.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"new entry must be retained");
        } else {
            assert!(workspace.entry(&name).unwrap().is_none());
        }
    }
}
