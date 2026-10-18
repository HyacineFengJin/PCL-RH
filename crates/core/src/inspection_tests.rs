//! Deterministic inspection fixtures use an injected catalog or a synthetic
//! executable. No test discovers/probes an installed system Java or starts MC.
use super::*;
use crate::java::{JavaCatalog, JavaRuntime};
use serde_json::{json, Value};
use std::collections::BTreeMap;

struct Fixture(tempfile::TempDir);

fn readiness_fixture() -> Fixture {
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../work/launch-readiness-2026-10-18/core-readiness-tests");
    fs::create_dir_all(&work).unwrap();
    Fixture(
        tempfile::Builder::new()
            .prefix("files-")
            .tempdir_in(work)
            .unwrap(),
    )
}

#[test]
fn file_readiness_never_discovers_java_or_creates_launch_outputs() {
    let fixture = readiness_fixture();
    fixture.complete();
    let report = inspect_files_with_isolation(
        fixture.root(),
        "Example",
        content_scope::IsolationPolicy::Automatic,
    )
    .unwrap();
    assert_eq!(report.missing_total, 0);
    assert!(!fixture.root().join(".pcl-linux").exists());
    fs::remove_file(fixture.root().join("versions/Example/Example.jar")).unwrap();
    let report = inspect_files_with_isolation(
        fixture.root(),
        "Example",
        content_scope::IsolationPolicy::Automatic,
    )
    .unwrap();
    assert_eq!(report.missing_total, 1);
    assert!(report.missing_files[0].ends_with("Example.jar"));
    assert!(!fixture.root().join(".pcl-linux").exists());
}

#[test]
fn file_readiness_reports_missing_index_without_guessing_objects() {
    let fixture = readiness_fixture();
    fixture.complete();
    fixture.instance(
        "Example",
        json!({"id":"Example","mainClass":"Main","libraries":[],"assetIndex":{"id":"fixture"}}),
    );
    let report = inspect_files_with_isolation(
        fixture.root(),
        "Example",
        content_scope::IsolationPolicy::Automatic,
    )
    .unwrap();
    assert_eq!(report.missing_total, 1);
    assert!(report.missing_files[0].ends_with("assets/indexes/fixture.json"));
    assert!(!fixture.root().join("assets").exists());
}

#[test]
fn pure_java_requirement_captures_recommended_major_exact_policy_and_metadata() {
    let fixture = readiness_fixture();
    fixture.complete();
    let vanilla = java_requirement(fixture.root(), "Example").unwrap();
    assert_eq!((vanilla.major, vanilla.exact), (21, false));
    fixture.instance("Example",json!({"id":"Example","mainClass":"Main","javaVersion":{"majorVersion":8},"libraries":[{"name":"net.minecraftforge:forge:1.12.2-14.23.5.2860"}]}));
    let forge = java_requirement(fixture.root(), "Example").unwrap();
    assert_eq!((forge.major, forge.exact), (8, true));
    assert_ne!(forge, vanilla);
    assert!(!fixture.root().join(".pcl-linux").exists());
}
impl Fixture {
    fn new() -> Self {
        let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/core-inspection-tests");
        fs::create_dir_all(&work).unwrap();
        let work = fs::canonicalize(work).unwrap();
        Self(
            tempfile::Builder::new()
                .prefix("inspect-")
                .tempdir_in(work)
                .unwrap(),
        )
    }
    fn root(&self) -> &Path {
        self.0.path()
    }
    fn write(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn instance(&self, id: &str, data: Value) {
        self.write(&format!("versions/{id}/{id}.json"), data.to_string());
    }
    fn complete(&self) {
        self.instance(
            "Example",
            json!({
                "id":"Example", "clientVersion":"1.21.1", "mainClass":"Main",
                "javaVersion":{"majorVersion":21}, "libraries":[]
            }),
        );
        self.write("versions/Example/Example.jar", b"fixture client");
    }
    fn catalog(&self, majors: &[u32]) -> JavaCatalog {
        JavaCatalog {
            runtimes: majors
                .iter()
                .map(|major| JavaRuntime {
                    path: self
                        .root()
                        .join(format!("catalog-java-{major}"))
                        .display()
                        .to_string(),
                    major: *major,
                    vendor: "Fixture".into(),
                    arch: std::env::consts::ARCH.into(),
                })
                .collect(),
            unavailable: Vec::new(),
        }
    }
    fn inspect(&self, id: &str, majors: &[u32]) -> LaunchInspection {
        let catalog = self.catalog(majors);
        inspect_with_java(self.root(), id, |_, required, exact| {
            java::select_from_catalog(&catalog, required, exact)
        })
        .unwrap()
    }
    #[cfg(unix)]
    fn executable(&self, name: &str, body: &str) -> java::JavaSelection {
        use std::os::unix::fs::PermissionsExt;
        self.write(name, format!("#!/bin/sh\n{body}\n"));
        let path = self.root().join(name);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        java::JavaSelection::Manual {
            path: path.display().to_string(),
        }
    }
}

/// Compare all owned directory entries and bytes, including existing native
/// outputs. A diagnostic must not create logs/cache/native/content folders.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let bytes = if metadata.is_file() {
                fs::read(&path).unwrap()
            } else {
                Vec::new()
            };
            result.insert(relative, bytes);
            if metadata.is_dir() {
                visit(root, &path, result)
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn complete_inspection_is_read_only_and_never_expands_or_discloses_arguments() {
    let f = Fixture::new();
    f.instance(
        "Example",
        json!({
            "id":"Example", "clientVersion":"1.21.1", "mainClass":"Main",
            "javaVersion":{"majorVersion":21},
            "arguments":{"game":["--accessToken","private-fixture-token","${unknown}"],"jvm":[42]},
            "assetIndex":{"id":"fixture"},
            "logging":{"client":{"file":{"id":"logging.xml"},"argument":"private-log-arg"}}
        }),
    );
    f.write("versions/Example/Example.jar", b"fixture client");
    let hash = "a".repeat(40);
    f.write(
        "assets/indexes/fixture.json",
        json!({"objects":{"fixture":{"hash":hash}}}).to_string(),
    );
    f.write(&format!("assets/objects/aa/{hash}"), b"fixture asset");
    f.write("assets/log_configs/logging.xml", b"fixture logging");
    f.write(
        ".pcl-linux/natives/Example/existing.so",
        b"running native remains unchanged",
    );
    let before = snapshot(f.root());
    let report = f.inspect("Example", &[25, 21]);
    assert!(report.ready, "{report:?}");
    assert_eq!(report.game_dir, f.root().display().to_string());
    assert_eq!(
        report.java_path,
        Some(f.root().join("catalog-java-21").display().to_string())
    );
    assert_eq!(report.minecraft_version, "1.21.1");
    assert!(!report.java_exact);
    assert_eq!(before, snapshot(f.root()));
    for rendered in [
        serde_json::to_string(&report).unwrap(),
        format!("{report:?}"),
    ] {
        for private in [
            "private-fixture-token",
            "private-log-arg",
            "${unknown}",
            "arguments",
        ] {
            assert!(!rendered.contains(private), "{rendered}");
        }
    }
}

#[test]
fn inherited_metadata_reuses_parent_jar_and_exact_loader_java_with_isolated_directory() {
    let f = Fixture::new();
    f.instance("Base", json!({"id":"Base", "clientVersion":"1.20.1", "mainClass":"Main", "javaVersion":{"majorVersion":17}}));
    f.write("versions/Base/Base.jar", b"base jar");
    f.instance("Example", json!({"id":"Example", "inheritsFrom":"Base", "libraries":[{"name":"net.minecraftforge:forge:1.20.1-47.3.0"}]}));
    f.write(
        "libraries/net/minecraftforge/forge/1.20.1-47.3.0/forge-1.20.1-47.3.0.jar",
        b"forge jar",
    );
    fs::create_dir(f.root().join("versions/Example/saves")).unwrap();
    let before = snapshot(f.root());
    let report = f.inspect("Example", &[21, 17]);
    assert!(report.ready, "{report:?}");
    assert_eq!(report.loader, "Forge 47.3.0");
    assert_eq!(report.minecraft_version, "1.20.1");
    assert_eq!(report.java_major, 17);
    assert!(report.java_exact);
    assert!(report.java_path.unwrap().ends_with("catalog-java-17"));
    assert_eq!(
        report.game_dir,
        f.root().join("versions/Example").display().to_string()
    );
    assert_eq!(before, snapshot(f.root()));
    assert!(!f.root().join(".pcl-linux").exists());
    let incompatible = f.inspect("Example", &[21]);
    assert!(!incompatible.ready);
    assert!(incompatible.java_error.unwrap().contains("相同主版本"));
}

#[test]
fn missing_files_are_unique_bounded_and_include_nonregular_jar_index_and_logging() {
    let f = Fixture::new();
    let mut libraries:Vec<_> = (0..40).map(|i| json!({"name":format!("fixture:library:{i}"),"downloads":{"artifact":{"path":format!("fixture/{i}.jar")}}})).collect();
    libraries.push(libraries[0].clone());
    f.instance("Example", json!({"id":"Example","mainClass":"Main","libraries":libraries,"assetIndex":{"id":"fixture"},"logging":{"client":{"file":{"id":"logging.xml"}}}}));
    fs::create_dir_all(f.root().join("versions/Example/Example.jar")).unwrap();
    fs::create_dir_all(f.root().join("assets/indexes/fixture.json")).unwrap();
    let before = snapshot(f.root());
    let report = f.inspect("Example", &[8]);
    assert!(!report.ready);
    assert_eq!(report.missing_total, 43);
    assert_eq!(report.missing_files.len(), dependencies::MISSING_FILE_LIMIT);
    assert!(report.issues.is_empty());
    assert!(report.java_error.is_none());
    assert_eq!(before, snapshot(f.root()));
}

#[test]
fn platform_rules_and_modern_and_legacy_natives_use_launch_dependency_resolver() {
    use crate::platform::{Architecture, OperatingSystem, Platform};
    let f = Fixture::new();
    f.complete();
    for (os, arch, classifier, foreign) in [
        (
            OperatingSystem::Linux,
            Architecture::X86_64,
            "natives-linux",
            "natives-windows",
        ),
        (
            OperatingSystem::Windows,
            Architecture::X86_64,
            "natives-windows",
            "natives-macos",
        ),
        (
            OperatingSystem::MacOs,
            Architecture::Arm64,
            "natives-macos-arm64",
            "natives-linux",
        ),
    ] {
        let context = RuleContext {
            platform: Platform { os, arch },
            release: String::new(),
            custom_resolution: false,
        };
        let data = json!({
            "libraries":[
                {"name":format!("fixture:modern:1:{classifier}"),"downloads":{"artifact":{"path":"modern.jar"}}},
                {"name":format!("fixture:foreign:1:{foreign}"),"downloads":{"artifact":{"path":"foreign-missing.jar"}}},
                {"name":"fixture:legacy:1","downloads":{"classifiers":{"fixture-native-64":{"path":"legacy.jar"}}},"natives":{os.minecraft_name():"fixture-native-${arch}"}},
                {"name":"fixture:ruled:1","rules":[{"action":"allow","os":{"name":"unmatched"}}]}
            ]
        });
        f.write("libraries/modern.jar", b"intentionally not a zip");
        let before = snapshot(f.root());
        let missing = dependencies::resolve(f.root(), "Example", &data, &context).unwrap();
        assert_eq!(missing.missing.total, 1);
        assert!(missing.missing.paths[0].ends_with("libraries/legacy.jar"));
        f.write("libraries/legacy.jar", b"also not a zip");
        let resolved = dependencies::resolve(f.root(), "Example", &data, &context).unwrap();
        assert_eq!(resolved.missing.total, 0);
        assert_eq!(resolved.native_jars.len(), 2);
        assert_eq!(resolved.classpath.len(), 2);
        fs::remove_file(f.root().join("libraries/legacy.jar")).unwrap();
        assert_eq!(before, snapshot(f.root()));
        assert!(!f.root().join(".pcl-linux").exists());
    }
}

#[test]
fn inspection_checks_native_archives_without_opening_or_extracting_them() {
    let f = Fixture::new();
    f.complete();
    let platform = crate::platform::Platform::current().unwrap();
    let classifier = format!("fixture-native-{}", platform.arch.bits());
    f.instance(
        "Example",
        json!({
            "id":"Example", "mainClass":"Main", "javaVersion":{"majorVersion":21},
            "libraries":[{
                "name":"fixture:legacy:1",
                "natives":{platform.os.minecraft_name():"fixture-native-${arch}"},
                "downloads":{"classifiers":{classifier:{"path":"native.jar"}}}
            }]
        }),
    );
    let missing = f.inspect("Example", &[21]);
    assert!(!missing.ready);
    assert_eq!(missing.missing_total, 1);
    assert!(missing.missing_files[0].ends_with("libraries/native.jar"));
    f.write(
        "libraries/native.jar",
        b"a regular file; intentionally not an extractable zip",
    );
    let before = snapshot(f.root());
    let report = f.inspect("Example", &[21]);
    assert!(report.ready, "{report:?}");
    assert_eq!(before, snapshot(f.root()));
    assert!(!f.root().join(".pcl-linux").exists());
}

#[test]
fn repeated_missing_asset_objects_count_once_without_creating_asset_folders() {
    let f = Fixture::new();
    f.complete();
    f.instance(
        "Example",
        json!({"id":"Example","mainClass":"Main","assetIndex":{"id":"fixture"}}),
    );
    let hash = "a".repeat(40);
    f.write(
        "assets/indexes/fixture.json",
        json!({"objects":{"first":{"hash":hash},"second":{"hash":hash}}}).to_string(),
    );
    let before = snapshot(f.root());
    let report = f.inspect("Example", &[8]);
    assert!(!report.ready);
    assert_eq!(report.missing_total, 1);
    assert_eq!(report.missing_files.len(), 1);
    assert!(report.missing_files[0].ends_with(&format!("assets/objects/aa/{hash}")));
    assert_eq!(before, snapshot(f.root()));
    assert!(!f.root().join("assets/objects").exists());
}

#[test]
fn damaged_missing_and_cyclic_metadata_do_not_probe_or_guess_java() {
    let f = Fixture::new();
    f.instance("Cycle", json!({"inheritsFrom":"Cycle"}));
    f.write("versions/Broken/Broken.json", b"{broken");
    fs::create_dir_all(f.root().join("versions/Directory/Directory.json")).unwrap();
    for id in ["Missing", "Broken", "Cycle", "Directory"] {
        let report = inspect_with_java(f.root(), id, |_, _, _| {
            panic!("damaged metadata cannot select Java")
        })
        .unwrap();
        assert!(!report.ready);
        assert_eq!(report.java_major, 0);
        assert!(report.java_path.is_none());
        assert!(!report.issues.is_empty());
        assert!(report.issues.len() <= ISSUE_LIMIT);
    }
    assert!(inspect_with_java(f.root(), "../escape", |_, _, _| unreachable!()).is_err());
    assert!(inspect_with_java(
        &f.root().join("absent-root"),
        "Example",
        |_, _, _| unreachable!()
    )
    .is_err());
}

#[test]
fn missing_main_class_and_invalid_java_requirement_cannot_be_ready() {
    let f = Fixture::new();
    f.complete();
    f.instance(
        "Example",
        json!({"id":"Example","javaVersion":{"majorVersion":4294967304_u64}}),
    );
    let report = f.inspect("Example", &[8]);
    assert!(!report.ready);
    assert_eq!(report.java_major, 0);
    assert!(report.issues.iter().any(|s| s.contains("mainClass")));
    assert!(report.java_error.unwrap().contains("主版本无效"));
}

#[test]
fn unsafe_dependency_paths_and_asset_hashes_are_reported_without_writes() {
    let f = Fixture::new();
    f.complete();
    for path in ["../outside.jar", "/absolute.jar"] {
        f.instance("Example",json!({"id":"Example","mainClass":"Main","libraries":[{"name":"fixture:unsafe:1","downloads":{"artifact":{"path":path}}}]}));
        let before = snapshot(f.root());
        let report = f.inspect("Example", &[8]);
        assert!(!report.ready);
        assert!(report
            .issues
            .iter()
            .any(|s| s.contains("Unsafe metadata path")));
        assert_eq!(before, snapshot(f.root()));
    }
    f.instance(
        "Example",
        json!({"id":"Example","mainClass":"Main","assetIndex":{"id":"fixture"}}),
    );
    f.write(
        "assets/indexes/fixture.json",
        json!({"objects":{"unsafe":{"hash":"../../escape"}}}).to_string(),
    );
    let report = f.inspect("Example", &[8]);
    assert!(!report.ready);
    assert!(report
        .issues
        .iter()
        .any(|s| s.contains("Invalid asset object hash")));
    // Invalid metadata text can be far longer than an OS path. Public issues
    // remain bounded and truncate on a UTF-8 boundary, without exposing args.
    f.instance("Example",json!({"id":"Example","mainClass":"Main","libraries":[{"name":"fixture:unsafe:1","downloads":{"artifact":{"path":format!("../{}", "中".repeat(3000))}}}]}));
    let report = f.inspect("Example", &[8]);
    assert!(!report.ready);
    assert!(report.issues[0].len() <= ISSUE_TEXT_LIMIT + '…'.len_utf8());
    assert!(report.issues[0].ends_with('…'));
}

#[test]
fn metadata_and_asset_index_changes_during_java_selection_block_ready() {
    let f = Fixture::new();
    f.complete();
    let catalog = f.catalog(&[21]);
    let changed = inspect_with_java(f.root(), "Example", |_, required, exact| {
        f.instance("Example", json!({"id":"Example","mainClass":"Changed"}));
        java::select_from_catalog(&catalog, required, exact)
    })
    .unwrap();
    assert!(!changed.ready);
    assert!(changed
        .issues
        .iter()
        .any(|s| s.contains("元数据在检查期间变化")));
    f.instance(
        "Example",
        json!({"id":"Example","mainClass":"Main","assetIndex":{"id":"fixture"}}),
    );
    f.write(
        "assets/indexes/fixture.json",
        json!({"objects":{}}).to_string(),
    );
    let changed = inspect_with_java(f.root(), "Example", |_, required, exact| {
        f.write(
            "assets/indexes/fixture.json",
            json!({"objects":{},"virtual":true}).to_string(),
        );
        java::select_from_catalog(&catalog, required, exact)
    })
    .unwrap();
    assert!(!changed.ready);
    assert!(changed
        .issues
        .iter()
        .any(|s| s.contains("资源索引在检查期间变化")));
}

#[test]
fn missing_dependency_after_java_selection_is_in_current_report() {
    let f = Fixture::new();
    f.complete();
    let catalog = f.catalog(&[21]);
    let report = inspect_with_java(f.root(), "Example", |_, required, exact| {
        fs::remove_file(f.root().join("versions/Example/Example.jar")).unwrap();
        java::select_from_catalog(&catalog, required, exact)
    })
    .unwrap();
    assert!(!report.ready);
    assert_eq!(report.missing_total, 1);
    assert!(report.missing_files[0].ends_with("Example.jar"));
}

#[test]
fn content_scope_change_during_java_selection_keeps_captured_directory_and_blocks_ready() {
    let f = Fixture::new();
    f.complete();
    let catalog = f.catalog(&[21]);
    let report = inspect_with_java(f.root(), "Example", |_, required, exact| {
        fs::create_dir(f.root().join("versions/Example/config")).unwrap();
        java::select_from_catalog(&catalog, required, exact)
    })
    .unwrap();
    assert!(!report.ready);
    assert_eq!(report.game_dir, f.root().display().to_string());
    assert!(report
        .issues
        .iter()
        .any(|s| s.contains("隔离目录在检查期间变化")));
}

#[cfg(unix)]
#[test]
fn manual_java_errors_never_fall_back_and_probe_only_fixed_version_arguments() {
    let f = Fixture::new();
    f.complete();
    let valid=f.executable("runtime/bin/java", "[ \"$#\" -eq 2 ] || exit 3\n[ \"$1\" = '-XshowSettings:properties' ] || exit 4\n[ \"$2\" = '-version' ] || exit 5\nprintf 'java.specification.version = 21\\n'");
    let before = snapshot(f.root());
    assert!(
        inspect_instance(f.root(), f.root(), "Example", &valid, &[])
            .unwrap()
            .ready
    );
    assert_eq!(before, snapshot(f.root()));
    let incompatible = f.executable("java-17", "printf 'java.specification.version = 17\\n'");
    let broken = f.executable("java-broken", "printf 'private-probe-text\\n'; exit 7");
    let absent = java::JavaSelection::Manual {
        path: f.root().join("absent-java").display().to_string(),
    };
    for choice in [incompatible, broken, absent] {
        let before = snapshot(f.root());
        let report = inspect_instance(f.root(), f.root(), "Example", &choice, &[]).unwrap();
        assert!(!report.ready);
        assert!(report.java_path.is_none());
        assert!(report.java_error.is_some());
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("private-probe-text"));
        assert_eq!(before, snapshot(f.root()));
    }
}

#[cfg(unix)]
#[test]
fn special_metadata_and_escaping_symlinks_fail_read_only() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.complete();
    let outside = Fixture::new();
    outside.write("escape.jar", b"outside");
    f.instance("Example",json!({"id":"Example","mainClass":"Main","libraries":[{"name":"fixture:escape:1","downloads":{"artifact":{"path":"escape.jar"}}}]}));
    fs::create_dir(f.root().join("libraries")).unwrap();
    symlink(
        outside.root().join("escape.jar"),
        f.root().join("libraries/escape.jar"),
    )
    .unwrap();
    let report = f.inspect("Example", &[8]);
    assert!(!report.ready);
    assert!(report.issues.iter().any(|s| s.contains("escapes")));
    f.complete();
    symlink(outside.root(), f.root().join("versions/Example/config")).unwrap();
    let report = f.inspect("Example", &[21]);
    assert!(!report.ready);
    assert!(report.game_dir.is_empty());
    assert!(report.issues.iter().any(|s| s.contains("隔离标记")));
    fs::remove_file(f.root().join("versions/Example/Example.json")).unwrap();
    let fifo = std::ffi::CString::new(
        f.root()
            .join("versions/Example/Example.json")
            .as_os_str()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let report = inspect_with_java(f.root(), "Example", |_, _, _| unreachable!()).unwrap();
    assert!(!report.ready);
    assert!(report.issues.iter().any(|s| s.contains("regular file")));
}
