use super::*;
use std::fs;

struct Fixture(#[allow(dead_code)] tempfile::TempDir, std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        Self(temp, path)
    }
    fn request() -> ExportRequest {
        serde_json::from_value(
            serde_json::json!({"name":"Example","version":"1.0.0","checks":{},"excluded":{}}),
        )
        .unwrap()
    }
    fn file(&self) -> std::path::PathBuf {
        self.1
            .join(".pcl-rust/export-presets")
            .join(name("a", "Example"))
    }
}

#[test]
fn preset_save_can_follow_owner_drop_while_duplicate_descriptor_stays_open() {
    let fixture = Fixture::new();
    let dir = directory(&fixture.1, true).unwrap().unwrap();
    let owner = lock_preset(&dir, "a", "Example").unwrap();
    let inherited = owner.0.file().try_clone().unwrap();
    assert!(save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request()
    )
    .is_err());
    drop(owner);
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    assert_eq!(
        read(&fixture.1, "a", Path::new("/game"), "Example")
            .unwrap()
            .unwrap()
            .name,
        "Example"
    );
    drop(inherited);
}
#[test]
fn scoped_presets_survive_restart_and_reject_retargeting() {
    let fixture = Fixture::new();
    assert!(read(&fixture.1, "a", Path::new("/game-a"), "Example")
        .unwrap()
        .is_none());
    assert!(!fixture.1.join(".pcl-rust").exists());
    save(
        &fixture.1,
        "a",
        Path::new("/game-a"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    assert_eq!(
        read(&fixture.1, "a", Path::new("/game-a"), "Example")
            .unwrap()
            .unwrap()
            .name,
        "Example"
    );
    assert!(read(&fixture.1, "b", Path::new("/game-b"), "Example")
        .unwrap()
        .is_none());
    assert!(read(&fixture.1, "a", Path::new("/other"), "Example").is_err());
    assert!(save(
        &fixture.1,
        "a",
        Path::new("/other"),
        "Example",
        Fixture::request()
    )
    .is_err());
    assert_eq!(
        read(&fixture.1, "a", Path::new("/game-a"), "Example")
            .unwrap()
            .unwrap()
            .name,
        "Example"
    );
}
#[test]
fn malformed_preset_is_preserved() {
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    fs::write(fixture.file(), b"malformed").unwrap();
    assert!(save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request()
    )
    .is_err());
    assert_eq!(fs::read(fixture.file()).unwrap(), b"malformed");
}
#[cfg(unix)]
#[test]
fn symlinked_storage_is_rejected_without_touching_its_target() {
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    let other = Fixture::new();
    std::os::unix::fs::symlink(&fixture.1, other.1.join(".pcl-rust")).unwrap();
    assert!(read(&other.1, "a", Path::new("/game"), "Example").is_err());
    assert!(save(
        &other.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request()
    )
    .is_err());
    assert_eq!(
        read(&fixture.1, "a", Path::new("/game"), "Example")
            .unwrap()
            .unwrap()
            .name,
        "Example"
    );
}
#[test]
fn schema_one_wire_and_rename_keep_existing_order_and_scope() {
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    let expected = serde_json::to_vec_pretty(&Preset {
        schema_version: 1,
        root_id: "a".into(),
        root_path: "/game".into(),
        instance_id: "Example".into(),
        request: Fixture::request(),
    })
    .unwrap();
    let bytes = fs::read(fixture.file()).unwrap();
    assert_eq!(bytes, expected);
    assert!(bytes.starts_with(b"{\n  \"schema_version\": 1,\n  \"root_id\": \"a\",\n  \"root_path\": \"/game\",\n  \"instance_id\": \"Example\",\n  \"request\": {"));
    let renamed = rename_bytes(&bytes, "a", Path::new("/game"), "Example", "New").unwrap();
    let preset: Preset = serde_json::from_slice(&renamed).unwrap();
    assert_eq!(preset.instance_id, "New");
    assert_eq!(preset.root_id, "a");
    assert_eq!(preset.root_path, "/game");
    assert_eq!(preset.request, Fixture::request());
    assert!(rename_bytes(&bytes, "a", Path::new("/other"), "Example", "New").is_err());
}
#[test]
fn external_change_before_publish_is_a_conflict_and_preserves_exact_bytes() {
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    let original = fs::read(fixture.file()).unwrap();
    let mut editor: serde_json::Value = serde_json::from_slice(&original).unwrap();
    editor["request"]["version"] = "user-edited".into();
    let edited = serde_json::to_vec_pretty(&editor).unwrap();
    let mut request = Fixture::request();
    request.version = "launcher-edited".into();
    let result = save_checked(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        request,
        || fs::write(fixture.file(), &edited).map_err(|e| e.to_string()),
    );
    assert!(result.is_err());
    assert_eq!(fs::read(fixture.file()).unwrap(), edited);
}
#[cfg(unix)]
#[test]
fn native_unix_updates_existing_preset_with_cas_and_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    let mut request = Fixture::request();
    request.version = "2.0".into();
    save(&fixture.1, "a", Path::new("/game"), "Example", request).unwrap();
    assert_eq!(
        read(&fixture.1, "a", Path::new("/game"), "Example")
            .unwrap()
            .unwrap()
            .version,
        "2.0"
    );
    assert_eq!(
        fs::metadata(fixture.file()).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
#[cfg(windows)]
#[test]
fn windows_existing_dynamic_preset_updates_logically_and_preserves_original_fence() {
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    let original = fs::read(fixture.file()).unwrap();
    let mut request = Fixture::request();
    request.version = "2.0".into();
    save(&fixture.1, "a", Path::new("/game"), "Example", request).unwrap();
    assert_eq!(
        read(&fixture.1, "a", Path::new("/game"), "Example")
            .unwrap()
            .unwrap()
            .version,
        "2.0"
    );
    assert_eq!(fs::read(fixture.file()).unwrap(), original);
    assert!(fixture
        .1
        .join(".pcl-rust/export-presets")
        .join(format!(".local-versions-{}", name("a", "Example")))
        .exists());
}
#[test]
fn changing_directory_before_publish_cannot_redirect_the_save() {
    let fixture = Fixture::new();
    let target = fixture.1.join(".pcl-rust/export-presets");
    let moved = fixture.1.join(".pcl-rust/original-presets");
    let result = save_checked(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
        || {
            fs::rename(&target, &moved).map_err(|e| e.to_string())?;
            fs::create_dir(&target).map_err(|e| e.to_string())
        },
    );
    assert!(result.is_err());
    assert!(!target.join(name("a", "Example")).exists());
    if moved.exists() {
        assert!(!moved.join(name("a", "Example")).exists());
    }
}

#[test]
fn invalid_request_is_rejected_before_creating_storage() {
    let fixture = Fixture::new();
    let mut request = Fixture::request();
    request.name = " invalid ".into();
    assert!(save(&fixture.1, "a", Path::new("/game"), "Example", request).is_err());
    assert!(!fixture.1.join(".pcl-rust").exists());
}
#[test]
fn wrong_schema_and_shared_hardlink_are_preserved() {
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    let original = fs::read(fixture.file()).unwrap();
    let mut wrong: serde_json::Value = serde_json::from_slice(&original).unwrap();
    wrong["schema_version"] = 999.into();
    let wrong = serde_json::to_vec_pretty(&wrong).unwrap();
    fs::write(fixture.file(), &wrong).unwrap();
    assert!(read(&fixture.1, "a", Path::new("/game"), "Example").is_err());
    assert!(save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request()
    )
    .is_err());
    assert_eq!(fs::read(fixture.file()).unwrap(), wrong);
    fs::write(fixture.file(), &original).unwrap();
    let alias = fixture.1.join("outside-alias");
    fs::hard_link(fixture.file(), &alias).unwrap();
    assert!(read(&fixture.1, "a", Path::new("/game"), "Example").is_err());
    assert!(save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request()
    )
    .is_err());
    assert_eq!(fs::read(alias).unwrap(), original);
}

#[test]
fn concurrent_first_file_is_never_overwritten() {
    let fixture = Fixture::new();
    let result = save_checked(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
        || fs::write(fixture.file(), b"concurrent original").map_err(|e| e.to_string()),
    );
    assert!(result.is_err());
    assert_eq!(fs::read(fixture.file()).unwrap(), b"concurrent original");
}
#[test]
fn oversized_existing_preset_is_bounded_and_preserved() {
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    let bytes = vec![b'x'; LIMIT as usize + 1];
    fs::write(fixture.file(), &bytes).unwrap();
    assert!(read(&fixture.1, "a", Path::new("/game"), "Example").is_err());
    assert!(save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request()
    )
    .is_err());
    assert_eq!(fs::read(fixture.file()).unwrap(), bytes);
}

#[cfg(windows)]
#[test]
fn windows_logically_deleted_preset_is_absent_and_can_be_saved_again() {
    let fixture = Fixture::new();
    save(
        &fixture.1,
        "a",
        Path::new("/game"),
        "Example",
        Fixture::request(),
    )
    .unwrap();
    let original = fs::read(fixture.file()).unwrap();
    {
        let scope = directory(&fixture.1, false).unwrap().unwrap();
        let dir = scope.folder.as_ref().unwrap();
        let document = Document::new(dir, &name("a", "Example"), LIMIT).unwrap();
        let state = document.state().unwrap();
        document.compare_write(&state, None).unwrap();
    }
    assert!(read(&fixture.1, "a", Path::new("/game"), "Example")
        .unwrap()
        .is_none());
    let mut request = Fixture::request();
    request.version = "reactivated".into();
    save(&fixture.1, "a", Path::new("/game"), "Example", request).unwrap();
    assert_eq!(
        read(&fixture.1, "a", Path::new("/game"), "Example")
            .unwrap()
            .unwrap()
            .version,
        "reactivated"
    );
    assert_eq!(fs::read(fixture.file()).unwrap(), original);
}
