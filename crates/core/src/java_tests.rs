//! Executable fixtures exercise process lifetime, discovery and launch policy.
#![cfg(unix)]
use super::*;
use std::os::unix::fs::PermissionsExt;

struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/core-java-tests");
        fs::create_dir_all(&work).unwrap();
        let work = fs::canonicalize(work).unwrap();
        Self(
            tempfile::Builder::new()
                .prefix("java-")
                .tempdir_in(work)
                .unwrap(),
        )
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn executable(&self, relative: &str, body: &str) -> PathBuf {
        let path = self.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    fn runtime(&self, relative: &str, version: &str) -> PathBuf {
        self.executable(relative, &format!("printf 'java.specification.version = {version}\\njava.vendor = Fixture JVM\\nos.arch = {}\\n' >&2", std::env::consts::ARCH))
    }
}

fn candidates(extras: &[String], discovered: Vec<PathBuf>) -> JavaCatalog {
    catalog_candidates(extras, discovered, Instant::now() + Duration::from_secs(2)).unwrap()
}

#[test]
fn managed_discovery_excludes_stages_unmarked_folders_and_links() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let name = format!("java-runtime-delta-{}", "a".repeat(40));
    let runtime = f.runtime(&format!(".pcl-rust/java/{name}/bin/java"), "21");
    let owner = runtime
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(".pcl-java-owner.json");
    assert!(managed_runtime_paths(f.path()).is_empty());
    fs::write(&owner, b"{}").unwrap();
    f.runtime(".pcl-rust/java/.stage-unfinished/bin/java", "25");
    fs::write(
        f.path()
            .join(".pcl-rust/java/.stage-unfinished/.pcl-java-owner.json"),
        b"{}",
    )
    .unwrap();
    let alias = f.path().join(format!(
        ".pcl-rust/java/java-runtime-epsilon-{}",
        "b".repeat(40)
    ));
    symlink(runtime.parent().unwrap().parent().unwrap(), alias).unwrap();
    assert_eq!(managed_runtime_paths(f.path()), [runtime.clone()]);
    assert!(discovery_paths(f.path(), None).contains(&runtime));
    let discovered = candidates(&[], managed_runtime_paths(f.path()));
    assert!(discovered
        .runtimes
        .iter()
        .any(|j| j.path == runtime.to_str().unwrap() && j.major == 21));
    fs::remove_file(&owner).unwrap();
    symlink("bin/java", owner).unwrap();
    assert!(managed_runtime_paths(f.path()).is_empty());
}

#[test]
fn strict_selection_schema_has_auto_default() {
    assert_eq!(JavaSelection::default(), JavaSelection::Auto);
    assert_eq!(
        serde_json::to_value(JavaSelection::Auto).unwrap(),
        serde_json::json!({"mode":"auto"})
    );
    assert_eq!(
        serde_json::from_str::<JavaSelection>(r#"{"mode":"manual","path":"/fixture/java"}"#)
            .unwrap(),
        JavaSelection::Manual {
            path: "/fixture/java".into()
        }
    );
    for invalid in [
        r#"{"mode":"auto","path":"ignored"}"#,
        r#"{"mode":"manual"}"#,
        r#"{"mode":"manual","path":"x","unknown":true}"#,
        r#"{"mode":"unknown"}"#,
    ] {
        assert!(serde_json::from_str::<JavaSelection>(invalid).is_err());
    }
}

#[test]
fn properties_and_legacy_version_outputs_are_parsed() {
    let fixture = Fixture::new();
    for (index, version, expected) in [(0, "1.8", 8), (1, "17", 17), (2, "21-ea", 21)] {
        let java = fixture.runtime(&format!("java-{index}"), version);
        let runtime = inspect(&java).unwrap();
        assert_eq!(runtime.major, expected);
        assert_eq!(runtime.vendor, "Fixture JVM");
        assert_eq!(
            runtime.path,
            fs::canonicalize(java).unwrap().to_str().unwrap()
        );
        assert_eq!(
            runtime.arch,
            normalized_arch(std::env::consts::ARCH).unwrap()
        );
    }
    for (index, line, expected) in [
        (0, "java version \"1.8.0_402\"", 8),
        (1, "openjdk version \"17.0.12\"", 17),
        (2, "openjdk 25-ea 2025-09-16", 25),
    ] {
        let java = fixture.executable(
            &format!("legacy-{index}"),
            &format!("printf '%s\\n' '{line}' >&2"),
        );
        assert_eq!(inspect(&java).unwrap().major, expected);
    }
}

#[test]
fn probe_uses_literal_path_fixed_arguments_and_no_option_injection() {
    let fixture = Fixture::new();
    let java = fixture.executable("java space ' $(touch injected)", "[ \"$#\" -eq 2 ] || exit 3\n[ \"$1\" = '-XshowSettings:properties' ] || exit 4\n[ \"$2\" = '-version' ] || exit 5\nprintf 'openjdk version \"21.0.1\"\\n'\n");
    assert_eq!(inspect(&java).unwrap().major, 21);
    assert!(!fixture.path().join("injected").exists());
}

#[test]
fn malformed_nonzero_nonexecutable_and_wrong_arch_are_unavailable() {
    let fixture = Fixture::new();
    let bad = fixture.executable("bad", "printf 'not a JVM\\n'");
    assert!(inspect(&bad).unwrap_err().contains("版本"));
    let invalid = fixture.executable("invalid", "");
    fs::write(&invalid, b"not an executable format").unwrap();
    // Linux reports ENOEXEC at spawn; Darwin may create a child that then
    // rejects the image. Both must fail probing, never yield a Java runtime.
    let invalid_error = inspect(&invalid).unwrap_err();
    assert!(
        invalid_error.contains("无法执行") || invalid_error.contains("异常退出"),
        "{invalid_error}"
    );
    let failed = fixture.executable(
        "failure",
        "printf 'java.specification.version = 21\\n'; exit 7",
    );
    assert!(inspect(&failed).unwrap_err().contains("退出"));
    let noexec = fixture.runtime("not-executable", "21");
    fs::set_permissions(&noexec, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(inspect(&noexec).unwrap_err().contains("执行权限"));
    assert!(inspect(fixture.path()).unwrap_err().contains("文件"));
    let different = if std::env::consts::ARCH == "aarch64" {
        "amd64"
    } else {
        "aarch64"
    };
    let wrong = fixture.executable(
        "wrong",
        &format!("printf 'java.specification.version = 21\\nos.arch = {different}\\n'"),
    );
    assert!(inspect(&wrong).unwrap_err().contains("不兼容"));
    let unknown = fixture.executable(
        "unknown",
        "printf 'java.specification.version = 21\\nos.arch = mystery\\n'",
    );
    assert!(inspect(&unknown).unwrap_err().contains("架构"));
}

#[test]
fn stdout_and_stderr_floods_fail_without_pipe_deadlock() {
    let fixture = Fixture::new();
    for (name, stream) in [("stdout", ""), ("stderr", " >&2")] {
        let java = fixture.executable(name, &format!("while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'{stream}; done"));
        let started = Instant::now();
        assert!(inspect(&java).unwrap_err().contains("64 KiB"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn hanging_process_is_timed_out_and_direct_child_reaped() {
    let fixture = Fixture::new();
    let java = fixture.executable(
        "hang",
        "printf '%s' \"$$\" > \"$0.pid\"\nwhile :; do sleep 1; done",
    );
    let started = Instant::now();
    assert!(
        inspect_until(&java, Instant::now() + Duration::from_millis(150))
            .unwrap_err()
            .contains("超时")
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let pid = fs::read_to_string(java.with_extension("pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[cfg(target_os = "linux")]
fn process_running(pid: i32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat.rsplit_once(") ").unwrap().1.chars().next().unwrap();
    !matches!(state, 'Z' | 'X')
}

#[cfg(target_os = "linux")]
#[test]
fn exited_parent_and_hanging_descendant_cannot_keep_inherited_pipes_alive() {
    let fixture = Fixture::new();
    for (name, tail) in [("exit", "exit 0"), ("wait", "wait")] {
        let java=fixture.executable(name,&format!("sleep 30 &\nprintf '%s' \"$!\" > \"$0.child\"\nprintf 'openjdk version \"21.0.1\"\\n' >&2\n{tail}"));
        let started = Instant::now();
        let result = inspect_until(&java, Instant::now() + Duration::from_millis(200));
        if name == "exit" {
            assert_eq!(result.unwrap().major, 21);
        } else {
            assert!(result.unwrap_err().contains("超时"));
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        let pid = fs::read_to_string(java.with_extension("child"))
            .unwrap()
            .parse()
            .unwrap();
        for _ in 0..20 {
            if !process_running(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!process_running(pid));
    }
}

#[test]
fn discovery_deduplicates_aliases_and_only_reports_registered_failures() {
    let fixture = Fixture::new();
    let java = fixture.executable(
        "java",
        "printf 'x' >> \"$0.runs\"\nprintf 'openjdk version \"21.0.1\"\\n'",
    );
    let alias = fixture.path().join("alias");
    std::os::unix::fs::symlink(&java, &alias).unwrap();
    let missing = fixture.path().join("registered-missing");
    let bad = fixture.executable("bad", "exit 1");
    let catalog = candidates(
        &[
            alias.to_str().unwrap().into(),
            java.to_str().unwrap().into(),
            missing.to_str().unwrap().into(),
            bad.to_str().unwrap().into(),
        ],
        vec![java.clone(), fixture.path().join("generated-missing")],
    );
    assert_eq!(catalog.runtimes.len(), 1);
    assert_eq!(catalog.unavailable.len(), 2);
    assert_eq!(fs::read(java.with_extension("runs")).unwrap(), b"x");
    assert!(catalog
        .unavailable
        .iter()
        .any(|e| e.path == missing.to_str().unwrap()));
}

#[test]
fn catalog_budget_bounds_stalled_candidates_and_retains_offline_extras() {
    let fixture = Fixture::new();
    let first = fixture.executable("first", "sleep 30");
    let second = fixture.runtime("second", "21");
    let extras = vec![
        first.to_str().unwrap().into(),
        second.to_str().unwrap().into(),
    ];
    let started = Instant::now();
    let catalog =
        catalog_candidates(&extras, vec![], Instant::now() + Duration::from_millis(150)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(catalog.runtimes.is_empty());
    assert_eq!(catalog.unavailable.len(), 2);
    assert!(catalog
        .unavailable
        .iter()
        .find(|e| e.path == second.to_str().unwrap())
        .unwrap()
        .error
        .contains("总时间"));
    assert!(super::catalog(
        fixture.path(),
        None,
        &vec!["missing".into(); MAX_CANDIDATES + 1]
    )
    .is_err());
}

#[test]
fn manual_selection_never_falls_back_and_exact_requirement_is_enforced() {
    let fixture = Fixture::new();
    fixture.runtime("runtime/bin/java", "17");
    let newer = fixture.runtime("custom-java", "21");
    let manual = JavaSelection::Manual {
        path: newer.to_str().unwrap().into(),
    };
    assert_eq!(
        select(fixture.path(), None, &[], &manual, 17, false).unwrap(),
        newer
    );
    assert!(select(fixture.path(), None, &[], &manual, 17, true)
        .unwrap_err()
        .contains("相同主版本"));
    let offline = JavaSelection::Manual {
        path: fixture.path().join("absent").to_str().unwrap().into(),
    };
    assert!(select(fixture.path(), None, &[], &offline, 17, false)
        .unwrap_err()
        .contains("手动"));
    assert!(select(fixture.path(), None, &[], &manual, 25, false).is_err());
}

#[test]
fn auto_uses_lowest_compatible_major_and_exact_loader_major() {
    let fixture = Fixture::new();
    let low = fixture.runtime("runtime/bin/java", "123");
    let high = fixture.runtime("runtime-25/bin/java", "125");
    // Discovery coverage supplies an explicit candidate set. Calling the
    // production catalog here would also execute the developer's system JVMs.
    let catalog = candidates(&[], vec![high.clone(), low.clone()]);
    assert_eq!(select_from_catalog(&catalog, 123, false).unwrap(), low);
    assert_eq!(select_from_catalog(&catalog, 124, false).unwrap(), high);
    assert_eq!(select_from_catalog(&catalog, 125, true).unwrap(), high);
    assert!(select_from_catalog(&catalog, 124, true).is_err());
}

#[test]
fn game_root_and_project_nested_runtime_locations_are_discovered() {
    let fixture = Fixture::new();
    let nested = fixture.runtime("PCL-Linux/runtime-21/bin/java", "21");
    let rooted = fixture.runtime("game/runtime/bin/java", "17");
    let paths = discovery_paths(fixture.path(), Some(&fixture.path().join("game")));
    assert!(paths.contains(&nested));
    assert!(paths.contains(&rooted));
    let extras = vec![
        nested.to_str().unwrap().into(),
        rooted.to_str().unwrap().into(),
    ];
    let catalog = candidates(&extras, vec![nested, rooted]);
    assert!(catalog.unavailable.is_empty(), "{:?}", catalog.unavailable);
    assert_eq!(
        catalog.runtimes.iter().map(|j| j.major).collect::<Vec<_>>(),
        vec![17, 21]
    );
}

#[test]
fn launch_plans_use_manual_java_and_keep_loader_major_and_token_contracts() {
    let fixture = Fixture::new();
    let root = fixture.path().join("game");
    let version = root.join("versions/Fixture");
    fs::create_dir_all(version.join("mods")).unwrap();
    fs::write(version.join("Fixture.jar"), b"fixture client").unwrap();
    let library_path =
        root.join("libraries/net/minecraftforge/forge/1.20.1-47.3.0/forge-1.20.1-47.3.0.jar");
    fs::create_dir_all(library_path.parent().unwrap()).unwrap();
    fs::write(&library_path, b"fixture loader").unwrap();
    fs::write(
        version.join("Fixture.json"),
        serde_json::json!({
            "id":"Fixture", "clientVersion":"1.20.1", "mainClass":"Main",
            "javaVersion":{"majorVersion":17},
            "libraries":[{"name":"net.minecraftforge:forge:1.20.1-47.3.0"}],
            "arguments":{"game":["--accessToken","${auth_access_token}"],"jvm":[]}
        })
        .to_string(),
    )
    .unwrap();
    let java17 = fixture.runtime("chosen-17", "17");
    let java21 = fixture.runtime("chosen-21", "21");
    let manual = JavaSelection::Manual {
        path: java17.to_str().unwrap().into(),
    };
    let plan = crate::build_launch_plan_with_java(
        &root,
        fixture.path(),
        "Fixture",
        "Player",
        4,
        &manual,
        &[],
    )
    .unwrap();
    assert_eq!(plan.java, java17);
    assert_eq!(plan.game_dir, version);
    let newer = JavaSelection::Manual {
        path: java21.to_str().unwrap().into(),
    };
    assert!(crate::build_launch_plan_with_java(
        &root,
        fixture.path(),
        "Fixture",
        "Player",
        4,
        &newer,
        &[]
    )
    .unwrap_err()
    .contains("相同主版本"));
    let identity = crate::OnlineIdentity {
        name: "Player".into(),
        uuid: "1234567890abcdef1234567890abcdef".into(),
        access_token: "fixture-private-token".into(),
        xuid: "1234".into(),
        client_id: "fixture-client".into(),
    };
    let online = crate::build_launch_plan_authenticated_with_java(
        &root,
        fixture.path(),
        "Fixture",
        &identity,
        4,
        &manual,
        &[],
    )
    .unwrap();
    assert!(online.args.contains(&identity.access_token));
    assert!(!serde_json::to_string(&online)
        .unwrap()
        .contains(&identity.access_token));
    assert!(!format!("{online:?}").contains(&identity.access_token));
    let secret_path = JavaSelection::Manual {
        path: fixture
            .path()
            .join(&identity.access_token)
            .to_str()
            .unwrap()
            .into(),
    };
    let error = crate::build_launch_plan_authenticated_with_java(
        &root,
        fixture.path(),
        "Fixture",
        &identity,
        4,
        &secret_path,
        &[],
    )
    .unwrap_err();
    assert!(!error.contains(&identity.access_token));
}
