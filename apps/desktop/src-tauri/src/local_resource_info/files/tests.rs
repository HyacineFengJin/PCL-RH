use super::*;
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use std::{fs, io::Write};

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let directory = Directory::open_absolute(&root).unwrap();
        let versions = directory
            .create_private(&Leaf::new("versions").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let instance = versions
            .create_private(&Leaf::new("Example").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let mut json = instance
            .create_file(&Leaf::new("Example.json").unwrap())
            .unwrap();
        json.write_all(br#"{"id":"Example"}"#).unwrap();
        json.sync_all().unwrap();
        let mods = directory
            .create_private(&Leaf::new("mods").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let mut resource = mods
            .create_file(&Leaf::new("fixture.jar").unwrap())
            .unwrap();
        resource.write_all(b"resource bytes").unwrap();
        resource.sync_all().unwrap();
        Self {
            _temporary: temporary,
            root,
        }
    }
    fn binding(&self) -> Binding {
        Binding::capture(
            &self.root,
            "Example",
            "mods",
            &mut Budget::new(),
            &AtomicBool::new(false),
            IsolationPolicy::Automatic,
        )
        .unwrap()
    }
}

#[test]
fn actual_binding_and_pinned_resource_match_production_panel_tokens() {
    let fixture = Fixture::new();
    let binding = fixture.binding();
    let pinned = Pinned::read(
        &binding.folder,
        "fixture.jar",
        FILE_LIMIT,
        &mut Budget::new(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        pinned.token,
        crate::resource_ops::token_for(&pinned.file).unwrap()
    );
    assert!(crate::resource_ops::valid_token(&pinned.token));
    assert_eq!(
        pinned.digest,
        format!("{:x}", Sha256::digest(b"resource bytes"))
    );
    binding.check().unwrap();
    pinned.check(&binding.folder, "fixture.jar").unwrap();
}

#[test]
fn byte_deadline_and_cancel_limits_never_change_original_resources() {
    let fixture = Fixture::new();
    let binding = fixture.binding();
    let closed = AtomicBool::new(false);
    for mut budget in [
        Budget::limited(1, Duration::from_secs(30)),
        Budget::limited(1024, Duration::ZERO),
    ] {
        assert!(Pinned::read(
            &binding.folder,
            "fixture.jar",
            FILE_LIMIT,
            &mut budget,
            &closed
        )
        .is_err());
    }
    assert!(Pinned::read(
        &binding.folder,
        "fixture.jar",
        1,
        &mut Budget::new(),
        &closed
    )
    .is_err());
    assert!(Pinned::read(
        &binding.folder,
        "fixture.jar",
        FILE_LIMIT,
        &mut Budget::new(),
        &AtomicBool::new(true)
    )
    .is_err());
    assert_eq!(
        fs::read(fixture.root.join("mods/fixture.jar")).unwrap(),
        b"resource bytes"
    );
}

#[test]
fn isolation_marker_appearance_and_metadata_edit_invalidate_confirmation() {
    let fixture = Fixture::new();
    let binding = fixture.binding();
    fs::create_dir(fixture.root.join("versions/Example/config")).unwrap();
    assert!(binding.check().is_err());
    drop(binding);
    fs::remove_dir(fixture.root.join("versions/Example/config")).unwrap();
    let binding = fixture.binding();
    fs::write(
        fixture.root.join("versions/Example/Example.json"),
        br#"{"id":"Changed"}"#,
    )
    .unwrap();
    assert!(binding.check().is_err());
}

#[test]
fn hardlinked_resource_and_outside_directory_alias_are_refused() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let outside_dir = Directory::open_absolute(&outside.path().canonicalize().unwrap()).unwrap();
    let mut outside_file = outside_dir
        .create_file(&Leaf::new("source.jar").unwrap())
        .unwrap();
    outside_file.write_all(b"outside bytes").unwrap();
    outside_file.sync_all().unwrap();
    drop(outside_file);
    fs::hard_link(
        outside.path().join("source.jar"),
        fixture.root.join("mods/hard.jar"),
    )
    .unwrap();
    let binding = fixture.binding();
    assert!(Pinned::read(
        &binding.folder,
        "hard.jar",
        FILE_LIMIT,
        &mut Budget::new(),
        &AtomicBool::new(false)
    )
    .is_err());
    drop(binding);
    let config = fixture.root.join("versions/Example/config");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &config).unwrap();
    #[cfg(windows)]
    {
        let result = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&config)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "junction fixture: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert!(Binding::capture(
        &fixture.root,
        "Example",
        "mods",
        &mut Budget::new(),
        &AtomicBool::new(false),
        IsolationPolicy::Automatic
    )
    .is_err());
    assert_eq!(
        fs::read(outside.path().join("source.jar")).unwrap(),
        b"outside bytes"
    );
}

#[cfg(windows)]
#[test]
fn game_metadata_uses_physical_bytes_even_for_a_launcher_document_basename() {
    use crate::launcher_local::filesystem::Snapshot;
    let temporary = tempfile::tempdir().unwrap();
    let parent = temporary.path().canonicalize().unwrap();
    Directory::open_absolute(&parent)
        .unwrap()
        .create_private(&Leaf::new("game").unwrap(), Traversal::SameFilesystem)
        .unwrap();
    let path = parent.join("game");
    let directory = Dir::absolute(&path).unwrap();
    let base = br#"{"id":"active","source":"physical"}"#;
    directory.write_new("active.json", base).unwrap();
    let before = Snapshot::open(&directory, "active.json", 1024).unwrap();
    directory
        .replace(
            "active.json",
            Some(&before),
            br#"{"source":"logical"}"#,
            1024,
        )
        .unwrap();
    assert_ne!(
        Snapshot::open(&directory, "active.json", 1024)
            .unwrap()
            .digest,
        before.digest
    );
    let mut pinned = Pinned::read(
        &directory,
        "active.json",
        1024,
        &mut Budget::new(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(pinned.bytes(1024).unwrap(), base);
    pinned.check(&directory, "active.json").unwrap();
    assert_eq!(fs::read(path.join("active.json")).unwrap(), base);
}
