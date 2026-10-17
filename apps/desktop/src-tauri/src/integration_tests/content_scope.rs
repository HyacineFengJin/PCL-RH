//! Cross-service content binding. Public scan/resource paths, actual ZIP export
//! and metadata export must select the same fixture bytes and refuse an unsafe
//! marker without falling back to the other directory's similarly named mod.
use super::*;
use std::io::Read;
use std::os::unix::fs::symlink;

struct ContentFixture {
    _fixture: Fixture,
    root: PathBuf,
    destination: PathBuf,
}
impl ContentFixture {
    fn new() -> Self {
        let fixture = Fixture::new();
        let root = fixture.0.join("scope-game");
        fs::create_dir_all(root.join("versions/Example")).unwrap();
        fs::write(
            root.join("versions/Example/Example.json"),
            br#"{
            "id":"Example","mainClass":"FixtureMain","clientVersion":"1.20.1","libraries":[]
        }"#,
        )
        .unwrap();
        fs::write(root.join("versions/Example/Example.jar"), b"client fixture").unwrap();
        let destination = fixture.0.join("export.zip");
        Self {
            _fixture: fixture,
            root,
            destination,
        }
    }
    fn request(&self) -> instance_export::ExportRequest {
        instance_export::ExportRequest {
            name: "Example".into(),
            version: "1.0".into(),
            checks: Default::default(),
            excluded: Default::default(),
        }
    }
    fn selected(&self, base: &Path) -> resource_ops::ResourceFile {
        resource_ops::ResourceFile {
            file_name: "example.jar".into(),
            fingerprint: resource_ops::fingerprint(&base.join("mods/example.jar")).unwrap(),
        }
    }
}

#[test]
fn shared_and_isolated_sources_match_resource_paths_metadata_and_exported_zip_contents() {
    for isolated in [false, true] {
        let fixture = ContentFixture::new();
        let version = fixture.root.join("versions/Example");
        if isolated {
            fs::write(version.join("options.txt"), b"music:0.5").unwrap();
        }
        let selected = if isolated {
            version.clone()
        } else {
            fixture.root.clone()
        };
        fs::create_dir_all(selected.join("mods")).unwrap();
        fs::write(
            selected.join("mods/example.jar"),
            b"selected fixture content",
        )
        .unwrap();
        let other = if isolated {
            fixture.root.clone()
        } else {
            version.clone()
        };
        // resourcepacks is not an automatic marker: a same-name file there
        // must not make scan silently select a different resource directory.
        fs::create_dir_all(other.join("resourcepacks")).unwrap();
        fs::write(other.join("resourcepacks/other.zip"), b"unselected content").unwrap();
        let scan = pcl_core::scan_instance(&fixture.root, "Example")
            .unwrap()
            .unwrap();
        assert_eq!(scan.isolated, isolated);
        for (kind, child) in [
            ("mods", "mods"),
            ("saves", "saves"),
            ("screenshots", "screenshots"),
            ("resourcepacks", "resourcepacks"),
            ("shaderpacks", "shaderpacks"),
            ("litematics", "schematics"),
            ("server", ""),
        ] {
            assert_eq!(
                ui_data::resource_dir(&fixture.root, "Example", kind).unwrap(),
                selected.join(child)
            );
        }
        let information = local_resource_info::prepare(
            &fixture.root,
            "Example",
            "mods",
            &[fixture.selected(&selected)],
            &AtomicBool::new(false),
        )
        .unwrap();
        information.rehash(&AtomicBool::new(false)).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&information.bytes).unwrap();
        assert_eq!(
            document["files"][0]["sizeBytes"],
            b"selected fixture content".len()
        );
        let plan = instance_export::prepare(&fixture.root, "Example", fixture.request()).unwrap();
        instance_export::execute(plan, &fixture.destination, &AtomicBool::new(false), |_| {})
            .unwrap();
        let mut zip = zip::ZipArchive::new(fs::File::open(&fixture.destination).unwrap()).unwrap();
        let mut manifest = Vec::new();
        zip.by_name("pcl-export.json")
            .unwrap()
            .read_to_end(&mut manifest)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        assert_eq!(manifest["game"]["isolated"], isolated);
        let content_root = if isolated {
            ".minecraft/versions/Example"
        } else {
            ".minecraft"
        };
        assert_eq!(manifest["content_root"], content_root);
        let mut contents = Vec::new();
        zip.by_name(&format!("{content_root}/mods/example.jar"))
            .unwrap()
            .read_to_end(&mut contents)
            .unwrap();
        assert_eq!(contents, b"selected fixture content");
        assert!(!zip.file_names().any(|name| name.ends_with("other.zip")));
    }
}

#[test]
fn links_and_special_markers_fail_across_consumers_without_using_shared_contents() {
    for linked in [true, false] {
        let fixture = ContentFixture::new();
        fs::create_dir(fixture.root.join("mods")).unwrap();
        fs::write(
            fixture.root.join("mods/example.jar"),
            b"must remain unchanged",
        )
        .unwrap();
        let marker = fixture.root.join("versions/Example/config");
        if linked {
            symlink("missing", &marker).unwrap();
        } else {
            let name = std::ffi::CString::new(marker.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        }
        for error in [
            pcl_core::scan_instance(&fixture.root, "Example").unwrap_err(),
            ui_data::resource_dir(&fixture.root, "Example", "mods").unwrap_err(),
            instance_export::prepare(&fixture.root, "Example", fixture.request()).unwrap_err(),
            local_resource_info::prepare(
                &fixture.root,
                "Example",
                "mods",
                &[fixture.selected(&fixture.root)],
                &AtomicBool::new(false),
            )
            .err()
            .unwrap(),
        ] {
            assert!(error.contains("config"), "{error}");
            assert!(error.contains("符号链接或特殊文件"), "{error}");
        }
        assert_eq!(
            fs::read(fixture.root.join("mods/example.jar")).unwrap(),
            b"must remain unchanged"
        );
        assert!(!fixture.destination.exists());
        assert!(!fixture.root.join(".pcl-linux").exists());
    }
}

#[test]
fn added_marker_invalidates_prepared_zip_and_metadata_without_rebinding_the_source() {
    let fixture = ContentFixture::new();
    fs::create_dir(fixture.root.join("mods")).unwrap();
    fs::write(
        fixture.root.join("mods/example.jar"),
        b"original shared mod",
    )
    .unwrap();
    let information = local_resource_info::prepare(
        &fixture.root,
        "Example",
        "mods",
        &[fixture.selected(&fixture.root)],
        &AtomicBool::new(false),
    )
    .unwrap();
    let plan = instance_export::prepare(&fixture.root, "Example", fixture.request()).unwrap();
    fs::write(
        fixture.root.join("versions/Example/options.txt"),
        b"music:0.5",
    )
    .unwrap();
    assert!(information.check().is_err());
    assert!(
        instance_export::execute(plan, &fixture.destination, &AtomicBool::new(false), |_| {})
            .is_err()
    );
    assert!(!fixture.destination.exists());
    assert_eq!(
        fs::read(fixture.root.join("mods/example.jar")).unwrap(),
        b"original shared mod"
    );
}

#[test]
fn explicit_isolation_selects_the_same_resource_metadata_zip_and_mutation_sources() {
    use pcl_core::content_scope::IsolationPolicy;
    for policy in [IsolationPolicy::Shared, IsolationPolicy::Isolated] {
        let f = ContentFixture::new();
        let isolated = policy == IsolationPolicy::Isolated;
        let version = f.root.join("versions/Example");
        for base in [&f.root, &version] {
            fs::create_dir(base.join("mods")).unwrap();
            fs::write(
                base.join("mods/example.jar"),
                if base == &f.root {
                    b"shared".as_slice()
                } else {
                    b"isolated content".as_slice()
                },
            )
            .unwrap();
        }
        let selected = if isolated { &version } else { &f.root };
        let other = if isolated { &f.root } else { &version };
        assert_eq!(
            ui_data::resource_dir_with_isolation(&f.root, "Example", "mods", policy).unwrap(),
            selected.join("mods")
        );
        let information = local_resource_info::prepare_with_isolation(
            &f.root,
            "Example",
            "mods",
            &[f.selected(selected)],
            &AtomicBool::new(false),
            policy,
        )
        .unwrap();
        information.rehash(&AtomicBool::new(false)).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&information.bytes).unwrap();
        assert_eq!(
            doc["files"][0]["sizeBytes"],
            fs::metadata(selected.join("mods/example.jar"))
                .unwrap()
                .len()
        );
        let plan = instance_export::prepare_with_isolation(
            &f.root,
            "Example",
            f.request(),
            policy,
            "generation-one",
        )
        .unwrap();
        let old_revision = plan.revision.clone();
        let new = instance_export::prepare_with_isolation(
            &f.root,
            "Example",
            f.request(),
            policy,
            "generation-two",
        )
        .unwrap();
        assert_ne!(old_revision, new.revision);
        instance_export::execute(plan, &f.destination, &AtomicBool::new(false), |_| {}).unwrap();
        let mut zip = zip::ZipArchive::new(fs::File::open(&f.destination).unwrap()).unwrap();
        let path = if isolated {
            ".minecraft/versions/Example/mods/example.jar"
        } else {
            ".minecraft/mods/example.jar"
        };
        let mut bytes = Vec::new();
        zip.by_name(path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, fs::read(selected.join("mods/example.jar")).unwrap());
        let other_before = fs::read(other.join("mods/example.jar")).unwrap();
        resource_ops::set_enabled_with_isolation(
            &f.root,
            "Example",
            "mods",
            &[f.selected(selected)],
            false,
            &AtomicBool::new(false),
            &mut || Ok(()),
            &mut |_, _| {},
            policy,
        )
        .unwrap();
        assert!(selected.join("mods/example.jar.disabled").is_file());
        assert_eq!(
            fs::read(other.join("mods/example.jar")).unwrap(),
            other_before
        );
    }
}

#[test]
fn explicit_scope_rejects_unsafe_markers_in_every_resource_reader() {
    use pcl_core::content_scope::IsolationPolicy;
    for policy in [IsolationPolicy::Shared, IsolationPolicy::Isolated] {
        let f = ContentFixture::new();
        fs::create_dir(f.root.join("mods")).unwrap();
        fs::write(f.root.join("mods/example.jar"), b"untouched").unwrap();
        symlink("missing", f.root.join("versions/Example/config")).unwrap();
        assert!(
            ui_data::resource_dir_with_isolation(&f.root, "Example", "mods", policy)
                .unwrap_err()
                .contains("config")
        );
        assert!(instance_export::prepare_with_isolation(
            &f.root,
            "Example",
            f.request(),
            policy,
            "rev"
        )
        .err()
        .unwrap()
        .contains("config"));
        assert!(local_resource_info::prepare_with_isolation(
            &f.root,
            "Example",
            "mods",
            &[f.selected(&f.root)],
            &AtomicBool::new(false),
            policy
        )
        .err()
        .unwrap()
        .contains("config"));
        assert!(resource_ops::remove_with_isolation(
            &f.root,
            "Example",
            "mods",
            &[f.selected(&f.root)],
            &AtomicBool::new(false),
            &mut || Ok(()),
            &mut |_, _| {},
            policy
        )
        .unwrap_err()
        .contains("config"));
        assert_eq!(
            fs::read(f.root.join("mods/example.jar")).unwrap(),
            b"untouched"
        );
    }
}

#[test]
fn retired_configuration_refuses_final_zip_publication_after_archiving_and_cleans_stage() {
    use pcl_core::content_scope::IsolationPolicy;
    let f = ContentFixture::new();
    fs::create_dir(f.root.join("mods")).unwrap();
    fs::write(f.root.join("mods/example.jar"), b"source remains intact").unwrap();
    let plan = instance_export::prepare_with_isolation(
        &f.root,
        "Example",
        f.request(),
        IsolationPolicy::Shared,
        "captured",
    )
    .unwrap();
    let reached_finalize = AtomicBool::new(false);
    let called_authority = AtomicBool::new(false);
    let error = instance_export::execute_with_guard(
        plan,
        &f.destination,
        &AtomicBool::new(false),
        |p| {
            if p.stage == "export-finalize" {
                reached_finalize.store(true, Ordering::SeqCst);
            }
        },
        || {
            called_authority.store(true, Ordering::SeqCst);
            assert!(reached_finalize.load(Ordering::SeqCst));
            Err::<(), String>("configuration retired".into())
        },
    )
    .unwrap_err();
    assert_eq!(error, "configuration retired");
    assert!(called_authority.load(Ordering::SeqCst));
    assert!(!f.destination.exists());
    assert!(!fs::read_dir(f.destination.parent().unwrap())
        .unwrap()
        .any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pcl-export-")));
    assert_eq!(
        fs::read(f.root.join("mods/example.jar")).unwrap(),
        b"source remains intact"
    );
}
