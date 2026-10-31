use super::*;

struct Fixture {
    _temporary: tempfile::TempDir,
    base: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        Self {
            _temporary: temporary,
            base,
        }
    }
    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn portable(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        Self::write(
            &root.join("experimental/host.mjs"),
            b"// synthetic host marker",
        );
        Self::write(
            &root.join("experimental/runtime/package.json"),
            br#"{"name":"pcl-rh-experimental-runtime"}"#,
        );
        Self::write(&root.join("code/plugins.json"), br#"[{"id":"rh.project"}]"#);
        Self::write(&root.join("assets/pcl-rh.png"), b"\x89PNG\r\n\x1a\nfixture");
        root.canonicalize().unwrap()
    }
    fn executable(root: &Path, nested: bool, name: &str) -> PathBuf {
        let path = if nested {
            root.join(".pcl-rust/bin").join(name)
        } else {
            root.join(name)
        };
        Self::write(&path, b"synthetic executable; never executed");
        path
    }
    fn development(&self) -> PathBuf {
        let root = self.portable("source-tree");
        Self::write(&root.join("Cargo.toml"), b"[workspace]\nmembers = []\n");
        Self::write(
            &root.join("apps/desktop/src-tauri/Cargo.toml"),
            b"[package]\nname = \"pcl-desktop\"\n",
        );
        Self::write(
            &root.join("apps/desktop/src-tauri/src/main.rs"),
            b"fn main() {}\n",
        );
        root.join("apps/desktop/src-tauri")
    }
    fn absent_manifest(&self) -> PathBuf {
        self.base
            .join("absent-build-machine/apps/desktop/src-tauri")
    }
}

#[test]
fn explicit_current_home_wins_without_requiring_the_build_machine_or_executable() {
    let fixture = Fixture::new();
    let home = fixture.portable("portable 根目录");
    assert_eq!(
        resolve_from(
            Some(home.as_os_str()),
            Some(OsStr::new("invalid legacy setting")),
            None,
            &fixture.absent_manifest(),
        )
        .unwrap(),
        home
    );
    // Root discovery does not allocate any legacy or new data directories.
    assert!(!home.join(".pcl-rust").exists());
    assert!(!home.join(".pcl-linux").exists());
    assert!(!home.join("Minecraft").exists());
}

#[test]
fn legacy_explicit_home_keeps_the_existing_linux_root() {
    let fixture = Fixture::new();
    let home = fixture.portable("existing-root");
    Fixture::write(&home.join(".pcl-rust/settings.json"), b"existing settings");
    Fixture::write(&home.join(".pcl-linux/kept"), b"existing provenance");
    assert_eq!(
        resolve_from(
            None,
            Some(home.as_os_str()),
            None,
            &fixture.absent_manifest()
        )
        .unwrap(),
        home
    );
    assert_eq!(
        fs::read(home.join(".pcl-rust/settings.json")).unwrap(),
        b"existing settings"
    );
    assert_eq!(
        fs::read(home.join(".pcl-linux/kept")).unwrap(),
        b"existing provenance"
    );
}

#[test]
fn invalid_empty_or_relative_explicit_home_never_falls_back() {
    let fixture = Fixture::new();
    let portable = fixture.portable("valid-portable");
    let executable = Fixture::executable(&portable, true, "pcl-desktop");
    let development = fixture.development();
    let missing = fixture.base.join("missing");
    for invalid in [
        missing.as_os_str(),
        OsStr::new(""),
        OsStr::new("relative-root"),
    ] {
        let error = resolve_from(
            Some(invalid),
            Some(portable.as_os_str()),
            Some(&executable),
            &development,
        )
        .unwrap_err();
        assert!(error.contains("PCL_RH_HOME"), "{error}");
    }
    assert!(resolve_from(
        None,
        Some(missing.as_os_str()),
        Some(&executable),
        &development,
    )
    .unwrap_err()
    .contains("PCL_LINUX_HOME"));
}

#[test]
fn both_portable_executable_layouts_locate_the_resources_before_development_fallback() {
    let fixture = Fixture::new();
    let development = fixture.development();
    for (index, (nested, name)) in [
        (true, "pcl-desktop"),
        (true, "pcl-desktop.exe"),
        (false, "pcl-desktop"),
        (false, "pcl-desktop.exe"),
    ]
    .into_iter()
    .enumerate()
    {
        let root = fixture.portable(&format!("archive-{index}"));
        let executable = Fixture::executable(&root, nested, name);
        assert_eq!(
            resolve_from(None, None, Some(&executable), &development).unwrap(),
            root
        );
    }
}

#[test]
fn filename_or_existing_directory_alone_cannot_identify_a_release() {
    let fixture = Fixture::new();
    let root = fixture.base.join("arbitrary-directory");
    let executable = Fixture::executable(&root, false, "pcl-desktop.exe");
    let error =
        resolve_from(None, None, Some(&executable), &fixture.absent_manifest()).unwrap_err();
    assert!(error.contains("无法定位"), "{error}");
    assert!(!root.join(".pcl-rust").exists());
}

#[test]
fn mismatching_or_incomplete_release_markers_are_rejected_without_writes() {
    let fixture = Fixture::new();
    for case in 0..4 {
        let root = fixture.portable(&format!("invalid-{case}"));
        match case {
            0 => fs::remove_file(root.join("experimental/host.mjs")).unwrap(),
            1 => Fixture::write(
                &root.join("experimental/runtime/package.json"),
                br#"{"name":"other"}"#,
            ),
            2 => Fixture::write(&root.join("code/plugins.json"), b"[]"),
            _ => Fixture::write(&root.join("assets/pcl-rh.png"), b"not a png"),
        }
        assert!(resolve_from(
            Some(root.as_os_str()),
            None,
            None,
            &fixture.absent_manifest()
        )
        .is_err());
        assert!(!root.join(".pcl-rust").exists());
    }
}

#[test]
fn executable_search_does_not_walk_arbitrary_ancestors_or_cwd() {
    let fixture = Fixture::new();
    let root = fixture.portable("ancestor-with-resources");
    let executable =
        Fixture::executable(&root.join("unrelated/subdirectory"), false, "pcl-desktop");
    assert!(resolve_from(None, None, Some(&executable), &fixture.absent_manifest()).is_err());
    assert!(resolve_from(None, None, None, &fixture.absent_manifest()).is_err());
    assert!(resolve_from(
        None,
        None,
        Some(Path::new("pcl-desktop")),
        &fixture.absent_manifest(),
    )
    .is_err());
}

#[test]
fn development_fallback_requires_the_actual_desktop_source_layout() {
    let fixture = Fixture::new();
    let manifest = fixture.development();
    let expected = manifest.ancestors().nth(3).unwrap();
    assert_eq!(resolve_from(None, None, None, &manifest).unwrap(), expected);
    fs::remove_file(manifest.join("src/main.rs")).unwrap();
    assert!(resolve_from(None, None, None, &manifest).is_err());
    assert!(resolve_from(None, None, None, &fixture.absent_manifest()).is_err());
}

#[test]
fn source_like_directory_with_another_package_is_not_a_development_fallback() {
    let fixture = Fixture::new();
    let manifest = fixture.development();
    Fixture::write(
        &manifest.join("Cargo.toml"),
        b"[package]\nname = \"other\"\n",
    );
    assert!(resolve_from(None, None, None, &manifest).is_err());
}

#[test]
fn core_archive_marker_locates_the_root_without_optional_code_or_ai_resources() {
    let fixture = Fixture::new();
    let root = fixture.base.join("core-only-portable");
    Fixture::write(
        &root.join(PORTABLE_MARKER),
        include_bytes!("../../../../../pcl-rh.json"),
    );
    let root = root.canonicalize().unwrap();
    assert_eq!(
        resolve_from(
            Some(root.as_os_str()),
            None,
            None,
            &fixture.absent_manifest(),
        )
        .unwrap(),
        root
    );
    for nested in [false, true] {
        let executable = Fixture::executable(&root, nested, "pcl-desktop.exe");
        assert_eq!(
            resolve_from(None, None, Some(&executable), &fixture.absent_manifest()).unwrap(),
            root
        );
    }
    assert!(!root.join("code").exists());
    assert!(!root.join("experimental").exists());
    assert!(!root.join("assets").exists());
}

#[test]
fn invalid_core_marker_never_downgrades_to_legacy_resources_or_another_development_root() {
    let fixture = Fixture::new();
    let development = fixture.development();
    for (case, bytes) in [
        b"invalid json".as_slice(),
        br#"{"schemaVersion":2,"productId":"dev.pcl-rh.portable"}"#,
        br#"{"schemaVersion":1,"productId":"another.product"}"#,
        br#"{"schemaVersion":1,"productId":"dev.pcl-rh.portable","unknown":true}"#,
    ]
    .into_iter()
    .enumerate()
    {
        // All old archive resources remain valid beside the rejected marker.
        let root = fixture.portable(&format!("bad-core-marker-{case}"));
        Fixture::write(&root.join(PORTABLE_MARKER), bytes);
        let executable = Fixture::executable(&root, true, "pcl-desktop");
        for home in [Some(root.as_os_str()), None] {
            let error = resolve_from(home, None, Some(&executable), &development).unwrap_err();
            assert!(error.contains(PORTABLE_MARKER), "{error}");
        }
        assert_eq!(fs::read(root.join(PORTABLE_MARKER)).unwrap(), bytes);
        assert!(!root.join(".pcl-rust/settings.json").exists());
    }
}
