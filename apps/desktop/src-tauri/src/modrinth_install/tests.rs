use super::{
    provider::{ApiFile, Dependency, FutureResult, Hashes, Project, Provider, Version},
    *,
};
use sha2::{Digest, Sha512};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub(super) fn run<F: std::future::Future>(future: F) -> F::Output {
    tauri::async_runtime::block_on(future)
}
pub(super) fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha512::digest(bytes))
}
struct Fixture {
    path: PathBuf,
    root: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        // The standalone harness supplies its project root explicitly; normal
        // desktop tests derive it from the desktop package manifest location.
        let repository = option_env!("PCL_MODRINTH_TEST_REPOSITORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let path = repository
            .join("work/autonomous-2026-10-04/modrinth/tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(path.join("game/versions/sample/mods")).unwrap();
        fs::create_dir_all(path.join("project/.pcl-rust")).unwrap();
        let path = path.canonicalize().unwrap();
        let root = path.join("game");
        let project = path.join("project");
        fs::write(root.join("versions/sample/sample.json"),br#"{"id":"sample","clientVersion":"1.20.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]}"#).unwrap();
        Self {
            path,
            root,
            project,
        }
    }
    fn target(&self) -> TargetSnapshot {
        target::capture(
            &self.root,
            &self.project,
            "root-fixture",
            "sample",
            &AtomicBool::new(false),
        )
        .unwrap()
    }
    fn local(&self, kind: &str, name: &str, bytes: &[u8]) {
        let path = self.root.join("versions/sample").join(kind).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
#[derive(Default)]
pub(super) struct FakeProvider {
    pub projects: BTreeMap<String, Project>,
    pub versions: BTreeMap<String, Version>,
    pub hashes: BTreeMap<String, Version>,
}
impl FakeProvider {
    pub(super) fn add(
        &mut self,
        project: &str,
        version: &str,
        kind: &str,
        filename: &str,
        bytes: &[u8],
    ) -> &mut Version {
        self.projects.insert(
            project.into(),
            Project {
                id: project.into(),
                title: project.into(),
                project_type: kind.into(),
            },
        );
        self.versions.insert(
            version.into(),
            Version {
                id: version.into(),
                project_id: project.into(),
                name: version.into(),
                version_number: "1.0.0".into(),
                date_published: "2026-10-04T00:00:00Z".into(),
                version_type: "release".into(),
                game_versions: vec!["1.20.1".into()],
                loaders: vec![if kind == "mod" {
                    "fabric"
                } else if kind == "resourcepack" {
                    "minecraft"
                } else {
                    "iris"
                }
                .into()],
                files: vec![ApiFile {
                    filename: filename.into(),
                    size: bytes.len() as u64,
                    url: format!(
                        "https://cdn.modrinth.com/data/{project}/versions/{version}/{filename}"
                    ),
                    primary: true,
                    hashes: Hashes {
                        sha512: sha(bytes),
                        sha1: None,
                    },
                    file_type: None,
                }],
                dependencies: vec![],
            },
        );
        self.versions.get_mut(version).unwrap()
    }
    fn plan(&self, f: &Fixture, file: Option<&str>) -> Result<InstallPlan> {
        run(plan::prepare(
            self,
            f.target(),
            InstallRequest {
                project_id: "Proj0001".into(),
                version_id: "Vers0001".into(),
                file_name: file.map(str::to_owned),
            },
            &AtomicBool::new(false),
        ))
    }
}
impl Provider for FakeProvider {
    fn project<'a>(&'a self, id: &'a str) -> FutureResult<'a, Project> {
        Box::pin(async move {
            self.projects
                .get(id)
                .cloned()
                .ok_or("unknown project".into())
        })
    }
    fn version<'a>(&'a self, id: &'a str) -> FutureResult<'a, Version> {
        Box::pin(async move {
            self.versions
                .get(id)
                .cloned()
                .ok_or("unknown version".into())
        })
    }
    fn versions<'a>(
        &'a self,
        project: &'a str,
        _: &'a Compatibility,
    ) -> FutureResult<'a, Vec<Version>> {
        Box::pin(async move {
            Ok(self
                .versions
                .values()
                .filter(|v| v.project_id == project)
                .cloned()
                .collect())
        })
    }
    fn from_hashes<'a>(
        &'a self,
        hashes: &'a [String],
    ) -> FutureResult<'a, BTreeMap<String, Version>> {
        Box::pin(async move {
            Ok(self
                .hashes
                .iter()
                .filter(|(key, _)| hashes.contains(key))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        })
    }
}
pub(super) fn dependency(project: Option<&str>, version: Option<&str>, kind: &str) -> Dependency {
    Dependency {
        project_id: project.map(str::to_owned),
        version_id: version.map(str::to_owned),
        file_name: None,
        dependency_type: kind.into(),
    }
}
fn basic() -> FakeProvider {
    let mut provider = FakeProvider::default();
    provider.add("Proj0001", "Vers0001", "mod", "sample.jar", b"sample mod");
    provider
}

/// Command fixtures obtain native provider-prepared authority through this
/// helper; they never manufacture a public revision or upload a plan DTO.
pub(crate) fn test_install_plan(
    root: &Path,
    project: &Path,
    root_id: &str,
    id: &str,
    request: InstallRequest,
) -> InstallPlan {
    let mut provider = FakeProvider::default();
    provider.add(
        &request.project_id,
        &request.version_id,
        "mod",
        "queued.jar",
        b"queued fixture",
    );
    provider
        .projects
        .get_mut(&request.project_id)
        .unwrap()
        .title = "Queued official resource".into();
    let cancel = AtomicBool::new(false);
    run(plan::prepare(
        &provider,
        target::capture(root, project, root_id, id, &cancel).unwrap(),
        request,
        &cancel,
    ))
    .unwrap()
}

#[test]
fn required_graph_resolves_nullable_version_ids_and_skips_optional_embedded() {
    let f = Fixture::new();
    let mut p = basic();
    p.add(
        "Proj0002",
        "Vers0002",
        "mod",
        "dependency.jar",
        b"dependency",
    );
    p.versions.get_mut("Vers0001").unwrap().dependencies = vec![
        dependency(Some("Proj0002"), None, "required"),
        dependency(Some("Proj0003"), None, "optional"),
        dependency(None, None, "embedded"),
    ];
    let plan = p.plan(&f, None).unwrap();
    assert_eq!(plan.files.len(), 2);
    assert_eq!(plan.files.iter().filter(|f| f.required).count(), 1);
    assert_eq!(plan.dependencies.len(), 3);
    assert!(plan.warnings.iter().any(|w| w.contains("可选")));
}
#[test]
fn cycles_and_multiple_required_versions_of_one_project_are_rejected() {
    let f = Fixture::new();
    let mut p = basic();
    p.add(
        "Proj0002",
        "Vers0002",
        "mod",
        "dependency.jar",
        b"dependency",
    );
    p.versions.get_mut("Vers0001").unwrap().dependencies =
        vec![dependency(Some("Proj0002"), Some("Vers0002"), "required")];
    p.versions.get_mut("Vers0002").unwrap().dependencies =
        vec![dependency(Some("Proj0001"), Some("Vers0001"), "required")];
    assert!(p.plan(&f, None).unwrap_err().contains("循环"));
    p.versions.get_mut("Vers0002").unwrap().dependencies.clear();
    p.add("Proj0002", "Vers0003", "mod", "other.jar", b"other");
    p.versions
        .get_mut("Vers0001")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0002"), Some("Vers0003"), "required"));
    assert!(p.plan(&f, None).unwrap_err().contains("多个版本"));
}
#[test]
fn required_external_dependency_cannot_be_silently_omitted() {
    let f = Fixture::new();
    let mut p = basic();
    let mut external = dependency(None, None, "required");
    external.file_name = Some("manual.jar".into());
    p.versions
        .get_mut("Vers0001")
        .unwrap()
        .dependencies
        .push(external);
    assert!(p.plan(&f, None).unwrap_err().contains("外部依赖"));
    assert!(!f.root.join("versions/sample/mods/sample.jar").exists());
}
#[test]
fn version_project_and_loader_game_compatibility_are_authoritative() {
    let f = Fixture::new();
    let mut p = basic();
    p.versions.get_mut("Vers0001").unwrap().project_id = "Proj0002".into();
    assert!(p.plan(&f, None).is_err());
    p.versions.get_mut("Vers0001").unwrap().project_id = "Proj0001".into();
    p.versions.get_mut("Vers0001").unwrap().loaders = vec!["forge".into()];
    assert!(p.plan(&f, None).unwrap_err().contains("加载器"));
    p.versions.get_mut("Vers0001").unwrap().loaders = vec!["fabric".into()];
    p.versions.get_mut("Vers0001").unwrap().game_versions = vec!["1.21".into()];
    assert!(p.plan(&f, None).is_err());
}
#[test]
fn selected_nonprimary_runtime_file_and_required_resource_pack_are_planned_together() {
    let f = Fixture::new();
    let mut p = basic();
    let version = p.versions.get_mut("Vers0001").unwrap();
    let mut alternate = version.files[0].clone();
    alternate.filename = "alternate.jar".into();
    alternate.primary = false;
    alternate.url = "https://cdn.modrinth.com/alternate.jar".into();
    version.files.push(alternate);
    let pack = ApiFile {
        filename: "required.zip".into(),
        size: 4,
        url: "https://cdn.modrinth.com/required.zip".into(),
        primary: false,
        hashes: Hashes {
            sha512: sha(b"pack"),
            sha1: None,
        },
        file_type: Some("required-resource-pack".into()),
    };
    version.files.push(pack);
    let plan = p.plan(&f, Some("alternate.jar")).unwrap();
    assert_eq!(plan.files.len(), 2);
    assert!(plan
        .files
        .iter()
        .any(|f| f.kind == "mods" && f.file_name == "alternate.jar"));
    assert!(plan
        .files
        .iter()
        .any(|f| f.kind == "resourcepacks" && f.required));
    assert!(p.plan(&f, Some("frontend-made-up.jar")).is_err());
}

#[test]
fn authoritative_plan_anonymous_download_and_mixed_kind_publication_work_together() {
    use crate::resource_ops::{import_verified_batch, VerifiedImport};
    use std::os::unix::fs::MetadataExt;

    fn archive(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap().into_inner()
    }

    let f = Fixture::new();
    let instance = f.root.join("versions/sample");
    // Isolation remains stable while both destination directories are missing.
    // The transaction must not create either directory before application
    // admission rechecks the exact snapshot captured by the planner.
    fs::remove_dir(instance.join("mods")).unwrap();
    fs::write(instance.join("options.txt"), b"preserved options").unwrap();
    f.local("shaderpacks", "existing.zip", b"preserved shader");
    fs::create_dir_all(f.root.join("libraries/shared")).unwrap();
    fs::write(
        f.root.join("libraries/shared/cache.jar"),
        b"preserved cache",
    )
    .unwrap();

    let mod_bytes = archive(
        "fabric.mod.json",
        br#"{"schemaVersion":1,"id":"sample","version":"1.0.0"}"#,
    );
    let pack_bytes = archive(
        "pack.mcmeta",
        br#"{"pack":{"pack_format":15,"description":"sample"}}"#,
    );
    let mut fake = FakeProvider::default();
    fake.add("Proj0001", "Vers0001", "mod", "sample.jar", &mod_bytes);
    fake.versions
        .get_mut("Vers0001")
        .unwrap()
        .files
        .push(ApiFile {
            filename: "required.zip".into(),
            size: pack_bytes.len() as u64,
            url: "https://cdn.modrinth.com/required.zip".into(),
            primary: false,
            hashes: Hashes {
                sha512: sha(&pack_bytes),
                sha1: None,
            },
            file_type: Some("required-resource-pack".into()),
        });
    let plan = fake.plan(&f, None).unwrap();
    assert_eq!(plan.files.len(), 2);
    assert!(plan
        .files
        .iter()
        .any(|file| file.kind == "resourcepacks" && file.required));
    let cancel = AtomicBool::new(false);
    let http = provider::HttpProvider::new(&cancel).unwrap();
    let (staging, _) = temporary(&f);
    let mut verified = Vec::new();
    for item in &plan.files {
        let body = match item.kind.as_str() {
            "mods" => &mod_bytes,
            "resourcepacks" => &pack_bytes,
            _ => panic!("unexpected planned kind"),
        };
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let (url, worker) = server(&response, Duration::ZERO);
        let mut file = staging.anonymous().unwrap();
        run(transfer::response_into_file(
            &http,
            http.client.get(url),
            item.size,
            &item.sha512,
            &mut file,
            &cancel,
            |_| {},
        ))
        .unwrap();
        worker.join().unwrap();
        assert_eq!(file.metadata().unwrap().nlink(), 0);
        verified.push(transfer::VerifiedFile {
            kind: item.kind.clone(),
            file_name: item.file_name.clone(),
            size: item.size,
            sha512: item.sha512.clone(),
            file,
        });
    }
    assert_eq!(
        http.network_bytes.load(Ordering::Relaxed),
        plan.download_bytes
    );
    let mut inputs: Vec<VerifiedImport> = verified
        .into_iter()
        .map(|file| VerifiedImport {
            kind: file.kind,
            file_name: file.file_name,
            file: file.file,
            size: file.size,
            sha512: file.sha512,
        })
        .collect();
    let mut commits = 0;
    let result = import_verified_batch(
        &f.root,
        "sample",
        &mut inputs,
        &cancel,
        &mut || {
            assert!(!instance.join("mods").exists());
            assert!(!instance.join("resourcepacks").exists());
            recheck_target(&plan, &cancel)?;
            commits += 1;
            Ok(())
        },
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(commits, 1);
    assert_eq!(result.changed, 2);
    for item in &plan.files {
        let bytes = fs::read(instance.join(&item.kind).join(&item.file_name)).unwrap();
        assert_eq!(bytes.len() as u64, item.size);
        assert_eq!(sha(&bytes), item.sha512);
    }
    assert_eq!(
        fs::read(instance.join("shaderpacks/existing.zip")).unwrap(),
        b"preserved shader"
    );
    assert_eq!(
        fs::read(instance.join("options.txt")).unwrap(),
        b"preserved options"
    );
    assert_eq!(
        fs::read(f.root.join("libraries/shared/cache.jar")).unwrap(),
        b"preserved cache"
    );
    assert_eq!(
        fs::read_dir(f.project.join(".pcl-rust/resource-downloads"))
            .unwrap()
            .count(),
        0
    );
    crate::resource_ops::ensure_verified_batches_ready(&f.root).unwrap();
}
#[test]
fn identical_enabled_content_reuses_but_disabled_content_is_never_enabled() {
    let f = Fixture::new();
    let p = basic();
    f.local("mods", "custom-name.jar", b"sample mod");
    let plan = p.plan(&f, None).unwrap();
    assert!(plan.files[0].reused);
    assert_eq!(plan.download_bytes, 0);
    assert_eq!(
        plan.files[0].existing_file_name.as_deref(),
        Some("custom-name.jar")
    );
    fs::rename(
        f.root.join("versions/sample/mods/custom-name.jar"),
        f.root.join("versions/sample/mods/custom-name.jar.disabled"),
    )
    .unwrap();
    assert!(p.plan(&f, None).unwrap_err().contains("禁用"));
}
#[test]
fn installed_old_versions_and_declared_incompatibilities_are_rejected() {
    let f = Fixture::new();
    let mut p = basic();
    let old = p
        .add("Proj0001", "Vers0002", "mod", "old.jar", b"old mod")
        .clone();
    f.local("mods", "old.jar.disabled", b"old mod");
    p.hashes.insert(sha(b"old mod"), old);
    assert!(p.plan(&f, None).unwrap_err().contains("其他版本"));
    fs::remove_file(f.root.join("versions/sample/mods/old.jar.disabled")).unwrap();
    p.hashes.clear();
    let other = p
        .add("Proj0002", "Vers0003", "mod", "other.jar", b"other mod")
        .clone();
    f.local("mods", "other.jar", b"other mod");
    p.hashes.insert(sha(b"other mod"), other);
    p.versions
        .get_mut("Vers0001")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0002"), None, "incompatible"));
    assert!(p.plan(&f, None).unwrap_err().contains("不兼容"));
    // Nullable IDs may describe a conflict by exact external file name. That
    // declaration also applies to new required files in this same plan.
    fs::remove_file(f.root.join("versions/sample/mods/other.jar")).unwrap();
    p.hashes.clear();
    let mut external = dependency(None, None, "incompatible");
    external.file_name = Some("other.jar".into());
    p.versions.get_mut("Vers0001").unwrap().dependencies = vec![
        dependency(Some("Proj0002"), Some("Vers0003"), "required"),
        external,
    ];
    assert!(p.plan(&f, None).unwrap_err().contains("不兼容"));
}
#[test]
fn malformed_hashes_urls_names_and_source_files_are_rejected() {
    let f = Fixture::new();
    let mut p = basic();
    p.versions.get_mut("Vers0001").unwrap().files[0]
        .hashes
        .sha512 = "wrong".into();
    assert!(p.plan(&f, None).is_err());
    p.versions.get_mut("Vers0001").unwrap().files[0]
        .hashes
        .sha512 = sha(b"sample mod");
    p.versions.get_mut("Vers0001").unwrap().files[0].url = "https://evil.example/file.jar".into();
    assert!(p.plan(&f, None).is_err());
    p.versions.get_mut("Vers0001").unwrap().files[0].url =
        "https://cdn.modrinth.com/file.jar".into();
    p.versions.get_mut("Vers0001").unwrap().files[0].filename = "../outside.jar".into();
    assert!(p.plan(&f, None).is_err());
    p.versions.get_mut("Vers0001").unwrap().files[0].filename = ".pcl-reserved.jar".into();
    assert!(p.plan(&f, None).unwrap_err().contains("保留名称"));
    p.versions.get_mut("Vers0001").unwrap().files[0].filename = "sample.jar".into();
    p.versions.get_mut("Vers0001").unwrap().files[0].file_type = Some("sources-jar".into());
    assert!(p.plan(&f, None).is_err());
    assert!(provider::cdn_url("http://cdn.modrinth.com/a.jar").is_err());
    assert!(provider::cdn_url("https://cdn.modrinth.com@evil.example/a.jar").is_err());
}
#[test]
fn target_edits_including_resource_collisions_invalidate_preparation() {
    let f = Fixture::new();
    let target = f.target();
    fs::create_dir(f.root.join("versions/sample/resourcepacks")).unwrap();
    assert!(target::check(&target, &AtomicBool::new(false)).is_err());
    fs::remove_dir(f.root.join("versions/sample/resourcepacks")).unwrap();
    f.local("mods", "late.jar", b"late");
    assert!(target::check(&target, &AtomicBool::new(false)).is_err());
    let p = basic();
    f.local("mods", "sample.jar", b"external conflicting mod");
    assert!(p.plan(&f, None).unwrap_err().contains("已存在"));
}

#[test]
fn unsafe_isolation_marker_returns_its_error_and_scope_changes_retire_the_target() {
    let fixture = Fixture::new();
    let before = fixture.target();
    let marker = fixture.root.join("versions/sample/options.txt");
    std::os::unix::fs::symlink("missing", &marker).unwrap();
    let error = target::capture(
        &fixture.root,
        &fixture.project,
        "root-fixture",
        "sample",
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("options.txt"));
    assert!(target::check(&before, &AtomicBool::new(false))
        .unwrap_err()
        .contains("options.txt"));
    fs::remove_file(marker).unwrap();
    fs::remove_dir(fixture.root.join("versions/sample/mods")).unwrap();
    assert!(target::check(&before, &AtomicBool::new(false)).is_err());
}

#[test]
fn queued_confirmation_rejects_new_profile_and_inventory_before_artifact_transfer() {
    use crate::tasks::{TaskKind, TaskOutcome, TaskScope, TaskTarget, Tasks};
    for profile_change in [false, true] {
        let fixture = Fixture::new();
        let provider = basic();
        let confirmed = provider.plan(&fixture, None).unwrap();
        let manager = Arc::new(Tasks::new());
        let target = TaskTarget {
            root_id: "root-fixture".into(),
            root_path: fixture.root.display().to_string(),
            instance_id: Some("sample".into()),
        };
        let blocker = manager
            .admit_queued(
                target.clone(),
                TaskKind::Install,
                TaskScope::root(&fixture.root).unwrap(),
            )
            .unwrap();
        blocker.wait_turn().unwrap();
        let queued = manager
            .admit_queued(
                target,
                TaskKind::ResourceDownload,
                TaskScope::root(&fixture.root).unwrap(),
            )
            .unwrap();
        if profile_change {
            // The same compatibility/id remains valid; only exact confirmation
            // identity should detect this external edit to the profile bytes.
            fs::write(fixture.root.join("versions/sample/sample.json"), br#"{"id":"sample","clientVersion":"1.20.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}],"fixture":"changed"}"#).unwrap();
        } else {
            fixture.local("mods", "other.jar", b"external resource");
        }
        blocker.finish(TaskOutcome::Complete {
            result: None,
            message: "fixture finished".into(),
            error: None,
        });
        queued.wait_turn().unwrap();
        let fresh = provider.plan(&fixture, None).unwrap();
        assert!(require_confirmed_plan(&fresh, &confirmed.revision).is_err());
        assert!(recheck_target(&confirmed, &AtomicBool::new(false)).is_err());
        assert!(!fixture
            .root
            .join("versions/sample/mods/sample.jar")
            .exists());
        assert!(!fixture
            .project
            .join(".pcl-rust/resource-downloads")
            .exists());
        queued.finish(TaskOutcome::Error("fixture confirmation refused".into()));
    }
}
#[test]
fn shader_download_plans_warn_about_unconfirmed_engine() {
    let f = Fixture::new();
    let mut p = FakeProvider::default();
    p.add("Proj0001", "Vers0001", "shader", "shader.zip", b"shader");
    let plan = p.plan(&f, None).unwrap();
    assert_eq!(plan.files[0].kind, "shaderpacks");
    assert!(plan.warnings.iter().any(|w| w.contains("尚未确认")));
}
#[test]
fn changed_authoritative_file_metadata_changes_the_confirmation_revision() {
    let f = Fixture::new();
    let mut p = basic();
    let before = p.plan(&f, None).unwrap();
    p.versions.get_mut("Vers0001").unwrap().files[0]
        .hashes
        .sha512 = sha(b"new modxxx");
    let after = p.plan(&f, None).unwrap();
    assert_ne!(before.revision, after.revision);
}
pub(super) fn server(response: &[u8], delay: Duration) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let response = response.to_owned();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = [0u8; 2048];
        let _ = socket.read(&mut bytes);
        thread::sleep(delay);
        let _ = socket.write_all(&response);
    });
    (format!("http://{address}/fixture"), worker)
}
fn temporary(f: &Fixture) -> (target::Dir, File) {
    let dir = target::Dir::open(&f.project)
        .unwrap()
        .ensure(".pcl-rust")
        .unwrap()
        .ensure("resource-downloads")
        .unwrap();
    let file = dir.anonymous().unwrap();
    (dir, file)
}
#[test]
fn transfer_checks_size_hash_and_counts_only_received_bytes() {
    let f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let provider = provider::HttpProvider::new(&cancel).unwrap();
    let (dir, mut file) = temporary(&f);
    let (url, worker) = server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ngood",
        Duration::ZERO,
    );
    run(transfer::response_into_file(
        &provider,
        provider.client.get(url),
        4,
        &sha(b"good"),
        &mut file,
        &cancel,
        |_| {},
    ))
    .unwrap();
    worker.join().unwrap();
    assert_eq!(provider.network_bytes.load(Ordering::Relaxed), 4);
    assert_eq!(
        fs::read_dir(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&dir.0)
        ))
        .unwrap()
        .count(),
        0
    );
    let (url, worker) = server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbad!",
        Duration::ZERO,
    );
    let mut bad = dir.anonymous().unwrap();
    assert!(run(transfer::response_into_file(
        &provider,
        provider.client.get(url),
        4,
        &sha(b"good"),
        &mut bad,
        &cancel,
        |_| {}
    ))
    .unwrap_err()
    .contains("SHA512"));
    worker.join().unwrap();
    drop(bad);
}
#[test]
fn transfer_rejects_redirects_and_bodies_larger_than_the_authoritative_size() {
    let f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let provider = provider::HttpProvider::new(&cancel).unwrap();
    let (dir, mut file) = temporary(&f);
    let(url,worker)=server(b"HTTP/1.1 302 Found\r\nLocation: https://evil.example/file.jar\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",Duration::ZERO);
    assert!(run(transfer::response_into_file(
        &provider,
        provider.client.get(url),
        4,
        &sha(b"good"),
        &mut file,
        &cancel,
        |_| {}
    ))
    .unwrap_err()
    .contains("重定向"));
    worker.join().unwrap();
    let(url,worker)=server(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nlarge\r\n0\r\n\r\n",Duration::ZERO);
    let mut oversized = dir.anonymous().unwrap();
    assert!(run(transfer::response_into_file(
        &provider,
        provider.client.get(url),
        4,
        &sha(b"good"),
        &mut oversized,
        &cancel,
        |_| {}
    ))
    .is_err());
    worker.join().unwrap();
    assert_eq!(provider.network_bytes.load(Ordering::Relaxed), 5);
}
#[test]
fn cancellation_during_response_headers_returns_promptly_and_leaves_no_named_partial() {
    let f = Fixture::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let provider = provider::HttpProvider::new(&cancel).unwrap();
    let (dir, mut file) = temporary(&f);
    let (url, worker) = server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ngood",
        Duration::from_millis(600),
    );
    let signal = cancel.clone();
    let trigger = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        signal.store(true, Ordering::Relaxed)
    });
    let start = Instant::now();
    assert_eq!(
        run(transfer::response_into_file(
            &provider,
            provider.client.get(url),
            4,
            &sha(b"good"),
            &mut file,
            &cancel,
            |_| {}
        ))
        .unwrap_err(),
        CANCELLED
    );
    assert!(start.elapsed() < Duration::from_millis(400));
    drop(file);
    trigger.join().unwrap();
    worker.join().unwrap();
    assert_eq!(
        fs::read_dir(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&dir.0)
        ))
        .unwrap()
        .count(),
        0
    );
    assert_eq!(provider.network_bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn cancellation_during_stalled_response_body_drops_transfer_before_named_publication() {
    let f = Fixture::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let provider = provider::HttpProvider::new(&cancel).unwrap();
    let (dir, mut file) = temporary(&f);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut bytes = [0u8; 2048];
        let _ = socket.read(&mut bytes);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n")
            .unwrap();
        thread::sleep(Duration::from_millis(600));
        let _ = socket.write_all(b"good");
    });
    let signal = cancel.clone();
    let trigger = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        signal.store(true, Ordering::Relaxed)
    });
    let start = Instant::now();
    assert_eq!(
        run(transfer::response_into_file(
            &provider,
            provider.client.get(format!("http://{address}/body")),
            4,
            &sha(b"good"),
            &mut file,
            &cancel,
            |_| {}
        ))
        .unwrap_err(),
        CANCELLED
    );
    assert!(start.elapsed() < Duration::from_millis(400));
    drop(file);
    trigger.join().unwrap();
    worker.join().unwrap();
    assert_eq!(
        fs::read_dir(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&dir.0)
        ))
        .unwrap()
        .count(),
        0
    );
}

#[test]
fn incompatible_declarations_from_existing_enabled_projects_are_checked() {
    let f = Fixture::new();
    let mut p = basic();
    let mut existing = p
        .add("Proj0002", "Vers0002", "mod", "existing.jar", b"existing")
        .clone();
    existing
        .dependencies
        .push(dependency(Some("Proj0001"), None, "incompatible"));
    f.local("mods", "existing.jar", b"existing");
    p.hashes.insert(sha(b"existing"), existing);
    assert!(p.plan(&f, None).unwrap_err().contains("不兼容"));
}

#[test]
#[ignore = "Explicit read-only probe of public Modrinth metadata; not part of offline regression gates"]
fn official_public_provider_metadata_probe() {
    run(async {
        let cancel = AtomicBool::new(false);
        let provider = provider::HttpProvider::new(&cancel).unwrap();
        let project = provider.project("P7dR8mSH").await.unwrap();
        assert_eq!(project.project_type, "mod");
        let compatibility = Compatibility {
            minecraft_version: "1.20.1".into(),
            loader: "fabric".into(),
            shader_engines: vec![],
        };
        let versions = provider
            .versions(&project.id, &compatibility)
            .await
            .unwrap();
        let candidate = versions
            .iter()
            .find(|v| v.loaders.iter().any(|l| l == "fabric"))
            .unwrap();
        let version = provider.version(&candidate.id).await.unwrap();
        let file = version
            .files
            .iter()
            .find(|f| f.primary)
            .unwrap_or(&version.files[0]);
        assert!(file.filename.ends_with(".jar"));
        let hashes = vec![file.hashes.sha512.clone()];
        let recognized = provider.from_hashes(&hashes).await.unwrap();
        assert_eq!(recognized[&hashes[0]].project_id, project.id);
        println!(
            "Official read-only metadata: project={}, version={}, file={}, size={}",
            project.title, version.id, file.filename, file.size
        );
    });
}

#[test]
fn explicit_isolation_binds_modrinth_inventory_and_configuration_generation() {
    use pcl_core::content_scope::IsolationPolicy;
    let f = Fixture::new();
    fs::create_dir(f.root.join("mods")).unwrap();
    fs::write(f.root.join("mods/shared.jar"), b"shared mod").unwrap();
    f.local("mods", "isolated.jar", b"isolated mod");
    let cancel = AtomicBool::new(false);
    let capture = |policy, revision| {
        target::capture_with_isolation(
            &f.root,
            &f.project,
            "root-fixture",
            "sample",
            &cancel,
            policy,
            revision,
        )
        .unwrap()
    };
    let shared = capture(IsolationPolicy::Shared, "one");
    assert_eq!(shared.local_files.len(), 1);
    assert_eq!(shared.local_files[0].file_name, "shared.jar");
    target::check(&shared, &cancel).unwrap();
    let isolated = capture(IsolationPolicy::Isolated, "two");
    assert_eq!(isolated.local_files.len(), 1);
    assert_eq!(isolated.local_files[0].file_name, "isolated.jar");
    assert!(!shared.permits_inventory_extension(&isolated));
    let roundtrip = capture(IsolationPolicy::Shared, "three");
    assert!(!shared.permits_inventory_extension(&roundtrip));
    assert_ne!(
        serde_json::to_vec(&shared).unwrap(),
        serde_json::to_vec(&roundtrip).unwrap()
    );
    std::os::unix::fs::symlink("missing", f.root.join("versions/sample/config")).unwrap();
    assert!(target::capture_with_isolation(
        &f.root,
        &f.project,
        "root-fixture",
        "sample",
        &cancel,
        IsolationPolicy::Shared,
        "four"
    )
    .unwrap_err()
    .contains("config"));
}
