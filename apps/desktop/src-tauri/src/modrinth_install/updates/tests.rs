//! Update fixtures use generic, isolated roots and no production HTTP. Bulk
//! counters prove directory-wide checking is independent of graph-size limits.
use super::super::{
    provider::{ApiFile, FutureResult, Hashes, Project, Provider, Version},
    tests::{dependency, run, sha, FakeProvider},
};
use super::*;
use std::{collections::BTreeMap, fs, path::PathBuf, sync::atomic::AtomicUsize};
pub(super) struct Fixture {
    _temporary: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) project: PathBuf,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let (temporary, _path, root, project) = super::super::test_support::tree();
        fs::write(root.join("versions/sample/sample.json"),br#"{"id":"sample","clientVersion":"1.20.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]}"#).unwrap();
        Self {
            _temporary: temporary,
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
    fn put(&self, name: &str, bytes: &[u8]) {
        fs::write(self.root.join("versions/sample/mods").join(name), bytes).unwrap()
    }
    fn selection(&self, names: &[&str]) -> Vec<ResourceFile> {
        names
            .iter()
            .map(|name| ResourceFile {
                file_name: (*name).into(),
                fingerprint: crate::resource_ops::fingerprint(
                    &self.root.join("versions/sample/mods").join(name),
                )
                .unwrap(),
            })
            .collect()
    }
    fn prepare(&self, provider: &impl Provider, names: &[&str]) -> Result<UpdatePlan> {
        run(prepare_with(
            provider,
            self.target(),
            self.selection(names),
            &AtomicBool::new(false),
        ))
    }
}

fn pair(
    provider: &mut FakeProvider,
    fixture: &Fixture,
    project: &str,
    old_id: &str,
    new_id: &str,
    old_name: &str,
    new_name: &str,
    old_bytes: &[u8],
    new_bytes: &[u8],
) {
    let old = provider.add(project, old_id, "mod", old_name, old_bytes);
    old.date_published = "2026-01-01T00:00:00Z".into();
    old.version_number = "1.0.0".into();
    let old = old.clone();
    fixture.put(old_name, old_bytes);
    provider.hashes.insert(sha(old_bytes), old);
    let new = provider.add(project, new_id, "mod", new_name, new_bytes);
    new.date_published = "2026-02-01T00:00:00Z".into();
    new.version_number = "2.0.0".into();
}
fn basic(f: &Fixture) -> FakeProvider {
    let mut provider = FakeProvider::default();
    pair(
        &mut provider,
        f,
        "Proj0001",
        "Vers0001",
        "Vers0002",
        "old.jar",
        "new.jar",
        b"old",
        b"new",
    );
    provider
}
#[test]
fn check_recognizes_hashes_under_custom_disabled_names_and_returns_resource_tokens() {
    let f = Fixture::new();
    let p = basic(&f);
    fs::rename(
        f.root.join("versions/sample/mods/old.jar"),
        f.root.join("versions/sample/mods/custom.jar.disabled"),
    )
    .unwrap();
    f.put("named-new.jar", b"unknown bytes");
    let check = run(check::check(&p, &f.target(), &AtomicBool::new(false))).unwrap();
    let entry = check
        .entries
        .iter()
        .find(|e| e.file_name == "custom.jar.disabled")
        .unwrap();
    assert_eq!(entry.status, UpdateStatus::UpdateAvailable);
    assert!(!entry.enabled);
    assert_eq!(entry.project_id.as_deref(), Some("Proj0001"));
    assert_eq!(
        entry.fingerprint,
        f.selection(&["custom.jar.disabled"])[0].fingerprint
    );
    assert_eq!(
        check
            .entries
            .iter()
            .find(|e| e.file_name == "named-new.jar")
            .unwrap()
            .status,
        UpdateStatus::Unknown
    );
    let plan = f.prepare(&p, &["custom.jar.disabled"]).unwrap();
    let r = &plan.replacements[0];
    assert_eq!(r.old_file_name, "custom.jar.disabled");
    assert_eq!(r.new_file_name, "new.jar.disabled");
    assert!(!r.enabled);
    assert!(!r.required);
    recheck_update_target(&plan, &AtomicBool::new(false)).unwrap();
    assert!(f
        .root
        .join("versions/sample/mods/custom.jar.disabled")
        .exists());
    assert!(!f
        .root
        .join("versions/sample/mods/new.jar.disabled")
        .exists());
}
#[test]
fn latest_release_uses_parsed_time_and_never_installed_channel_or_older_release() {
    let f = Fixture::new();
    let mut p = basic(&f);
    p.hashes.get_mut(&sha(b"old")).unwrap().date_published = "2026-01-01T00:30:00+01:00".into();
    p.versions.get_mut("Vers0002").unwrap().date_published = "2025-12-31T23:45:00Z".into();
    assert_eq!(
        f.prepare(&p, &["old.jar"]).unwrap().replacements[0].new_version_id,
        "Vers0002"
    );
    p.versions.get_mut("Vers0002").unwrap().date_published = "2025-12-31T23:00:00Z".into();
    assert!(f.prepare(&p, &["old.jar"]).is_err());
    p.versions.get_mut("Vers0002").unwrap().date_published = "malformed date".into();
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("发布日期"));
    p.versions.get_mut("Vers0002").unwrap().date_published = "2026-02-01T00:00:00Z".into();
    p.hashes.get_mut(&sha(b"old")).unwrap().version_type = "beta".into();
    let checked = run(check::check(&p, &f.target(), &AtomicBool::new(false))).unwrap();
    assert_eq!(checked.entries[0].status, UpdateStatus::Blocked);
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("alpha或beta"));
}
#[test]
fn duplicate_project_including_disabled_is_blocked_before_replacement() {
    let f = Fixture::new();
    let p = basic(&f);
    f.put("duplicate.jar.disabled", b"old");
    let checked = run(check::check(&p, &f.target(), &AtomicBool::new(false))).unwrap();
    assert!(checked
        .entries
        .iter()
        .all(|e| e.status == UpdateStatus::Blocked));
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("多个本地"));
}
#[test]
fn matched_old_hash_with_authoritative_size_mismatch_rejects_before_update_lookup() {
    let f = Fixture::new();
    let mut inner = basic(&f);
    inner.hashes.get_mut(&sha(b"old")).unwrap().files[0].size += 1;
    let p = BulkProvider::new(inner, BTreeMap::new());
    let error = run(check::check(&p, &f.target(), &AtomicBool::new(false))).unwrap_err();
    assert!(error.contains("已有文件校验响应不符"));
    assert_eq!(p.hash_calls.load(Ordering::Relaxed), 1);
    assert_eq!(p.update_calls.load(Ordering::Relaxed), 0);
    assert_eq!(p.project_calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        fs::read(f.root.join("versions/sample/mods/old.jar")).unwrap(),
        b"old"
    );
}
#[test]
fn old_and_new_updates_require_explicit_primary_file_authority() {
    let f = Fixture::new();
    let mut p = basic(&f);
    p.hashes.get_mut(&sha(b"old")).unwrap().files[0].primary = false;
    let checked = run(check::check(&p, &f.target(), &AtomicBool::new(false))).unwrap();
    assert_eq!(checked.entries[0].status, UpdateStatus::Blocked);
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("主文件声明"));
    p.hashes.get_mut(&sha(b"old")).unwrap().files[0].primary = true;
    p.versions.get_mut("Vers0002").unwrap().files[0].primary = false;
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("新版没有官方主文件"));

    // A recognized runtime file can share a release with a different primary.
    // Its hash identifies the project, but never authorizes a replacement.
    p.versions.get_mut("Vers0002").unwrap().files[0].primary = true;
    let old = p.hashes.get_mut(&sha(b"old")).unwrap();
    old.files[0].primary = false;
    let mut primary = old.files[0].clone();
    primary.filename = "official-main.jar".into();
    primary.hashes.sha512 = sha(b"official main");
    primary.size = b"official main".len() as u64;
    primary.primary = true;
    old.files.push(primary);
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("不是官方主文件"));
}
#[test]
fn required_extra_replacement_cannot_bypass_old_primary_file_policy() {
    let f = Fixture::new();
    let mut p = basic(&f);
    pair(
        &mut p,
        &f,
        "Proj0002",
        "Vers0003",
        "Vers0004",
        "dep-old.jar",
        "dep-new.jar",
        b"dep old",
        b"dep new",
    );
    p.hashes.get_mut(&sha(b"dep old")).unwrap().files[0].primary = false;
    p.versions
        .get_mut("Vers0002")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0002"), Some("Vers0004"), "required"));
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("主文件声明"));
    assert_eq!(
        fs::read(f.root.join("versions/sample/mods/dep-old.jar")).unwrap(),
        b"dep old"
    );
}
#[test]
fn required_pinned_upgrade_is_explicit_and_multi_selection_shares_one_graph() {
    let f = Fixture::new();
    let mut p = basic(&f);
    pair(
        &mut p,
        &f,
        "Proj0002",
        "Vers0003",
        "Vers0004",
        "dep-old.jar",
        "dep-new.jar",
        b"dep old",
        b"dep new",
    );
    p.versions
        .get_mut("Vers0002")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0002"), Some("Vers0004"), "required"));
    let plan = f.prepare(&p, &["old.jar"]).unwrap();
    assert_eq!(plan.replacements.len(), 2);
    assert!(
        plan.replacements
            .iter()
            .find(|r| r.old_file_name == "dep-old.jar")
            .unwrap()
            .required
    );
    let plan = f.prepare(&p, &["old.jar", "dep-old.jar"]).unwrap();
    assert_eq!(plan.replacements.len(), 2);
    assert!(plan.replacements.iter().all(|r| !r.required));
    let reverse = f.prepare(&p, &["dep-old.jar", "old.jar"]).unwrap();
    assert_eq!(reverse.revision, plan.revision);
}
#[test]
fn surviving_installed_exact_required_pin_blocks_new_graph_until_both_are_updated() {
    let f = Fixture::new();
    let mut p = basic(&f);
    pair(
        &mut p,
        &f,
        "Proj0002",
        "Vers0003",
        "Vers0004",
        "dep-old.jar",
        "dep-new.jar",
        b"dep old",
        b"dep new",
    );
    p.hashes
        .get_mut(&sha(b"old"))
        .unwrap()
        .dependencies
        .push(dependency(None, Some("Vers0003"), "required"));
    p.versions
        .get_mut("Vers0002")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0002"), Some("Vers0004"), "required"));
    assert!(f
        .prepare(&p, &["dep-old.jar"])
        .unwrap_err()
        .contains("未更新模组"));
    assert_eq!(
        f.prepare(&p, &["old.jar", "dep-old.jar"])
            .unwrap()
            .replacements
            .len(),
        2
    );
}
#[test]
fn surviving_reverse_incompatible_and_filename_required_constraints_remain_active() {
    let f = Fixture::new();
    let mut p = basic(&f);
    let mut survivor = p
        .add("Proj0002", "Vers0003", "mod", "survivor.jar", b"survivor")
        .clone();
    survivor.dependencies.push(dependency(
        Some("Proj0001"),
        Some("Vers0002"),
        "incompatible",
    ));
    f.put("survivor.jar", b"survivor");
    p.hashes.insert(sha(b"survivor"), survivor.clone());
    assert!(f.prepare(&p, &["old.jar"]).unwrap_err().contains("不兼容"));
    let mut by_name = dependency(None, None, "required");
    by_name.file_name = Some("old.jar".into());
    survivor.dependencies = vec![by_name];
    p.hashes.insert(sha(b"survivor"), survivor);
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("仍要求旧前置文件"));
}
#[test]
fn disabled_mandatory_dependencies_are_never_enabled_and_inactive_roots_add_no_mods() {
    let f = Fixture::new();
    let mut p = basic(&f);
    pair(
        &mut p,
        &f,
        "Proj0002",
        "Vers0003",
        "Vers0004",
        "dep-old.jar",
        "dep-new.jar",
        b"dep old",
        b"dep new",
    );
    fs::rename(
        f.root.join("versions/sample/mods/dep-old.jar"),
        f.root.join("versions/sample/mods/dep-old.jar.disabled"),
    )
    .unwrap();
    p.versions
        .get_mut("Vers0002")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0002"), Some("Vers0004"), "required"));
    assert!(f.prepare(&p, &["old.jar"]).unwrap_err().contains("禁用"));
    assert!(f
        .prepare(&p, &["old.jar", "dep-old.jar.disabled"])
        .unwrap_err()
        .contains("禁用"));
    fs::remove_file(f.root.join("versions/sample/mods/dep-old.jar.disabled")).unwrap();
    p.hashes.remove(&sha(b"dep old"));
    fs::rename(
        f.root.join("versions/sample/mods/old.jar"),
        f.root.join("versions/sample/mods/old.jar.disabled"),
    )
    .unwrap();
    assert!(f
        .prepare(&p, &["old.jar.disabled"])
        .unwrap_err()
        .contains("包含禁用模组且需要新增模组前置"));
}
#[test]
fn mixed_enabled_and_disabled_roots_with_new_mod_dependency_require_separate_batches() {
    let f = Fixture::new();
    let mut p = basic(&f);
    pair(
        &mut p,
        &f,
        "Proj0002",
        "Vers0003",
        "Vers0004",
        "other-old.jar",
        "other-new.jar",
        b"other old",
        b"other new",
    );
    fs::rename(
        f.root.join("versions/sample/mods/other-old.jar"),
        f.root.join("versions/sample/mods/other-old.jar.disabled"),
    )
    .unwrap();
    p.add(
        "Proj0003",
        "Vers0005",
        "mod",
        "new-dependency.jar",
        b"new dependency",
    );
    p.versions
        .get_mut("Vers0002")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0003"), Some("Vers0005"), "required"));
    let error = f
        .prepare(&p, &["old.jar", "other-old.jar.disabled"])
        .unwrap_err();
    assert!(error.contains("包含禁用模组且需要新增模组前置"));
    let enabled = f.prepare(&p, &["old.jar"]).unwrap();
    assert_eq!(enabled.adds.len(), 1);
    assert_eq!(enabled.adds[0].file_name, "new-dependency.jar");
    let disabled = f.prepare(&p, &["other-old.jar.disabled"]).unwrap();
    assert!(disabled.adds.is_empty());
    assert_eq!(
        disabled.replacements[0].new_file_name,
        "other-new.jar.disabled"
    );
}
#[test]
fn adds_required_resource_pack_and_reuses_unchanged_enabled_dependency() {
    let f = Fixture::new();
    let mut p = basic(&f);
    let dep = p
        .add("Proj0002", "Vers0003", "mod", "dep.jar", b"dependency")
        .clone();
    f.put("custom-dep.jar", b"dependency");
    p.hashes.insert(sha(b"dependency"), dep);
    p.versions
        .get_mut("Vers0002")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0002"), None, "required"));
    p.versions.get_mut("Vers0002").unwrap().files.push(ApiFile {
        filename: "required.zip".into(),
        size: 4,
        url: "https://cdn.modrinth.com/required.zip".into(),
        primary: false,
        hashes: Hashes {
            sha512: sha(b"pack"),
            sha1: None,
        },
        file_type: Some("required-resource-pack".into()),
    });
    let plan = f.prepare(&p, &["old.jar"]).unwrap();
    assert_eq!(plan.replacements.len(), 1);
    assert_eq!(plan.adds.len(), 1);
    assert_eq!(plan.adds[0].kind, "resourcepacks");
    assert_eq!(plan.reuse.len(), 1);
    assert_eq!(
        plan.reuse[0].existing_file_name.as_deref(),
        Some("custom-dep.jar")
    );
    assert_eq!(
        plan.total_bytes,
        plan.download_bytes + b"dependency".len() as u64
    );
    assert_eq!(
        plan.download_bytes,
        plan.replacements[0].size + plan.adds[0].size
    );
    assert!(!serde_json::to_string(&plan)
        .unwrap()
        .contains("cdn.modrinth"));
}
#[test]
fn replacement_filename_conflicts_reject_and_same_name_authorized_replacement_works() {
    let f = Fixture::new();
    let mut p = basic(&f);
    f.put("new.jar", b"external collision");
    assert!(f.prepare(&p, &["old.jar"]).unwrap_err().contains("已存在"));
    fs::remove_file(f.root.join("versions/sample/mods/new.jar")).unwrap();
    p.versions.get_mut("Vers0002").unwrap().files[0].filename = "old.jar".into();
    let plan = f.prepare(&p, &["old.jar"]).unwrap();
    assert_eq!(
        plan.replacements[0].old_file_name,
        plan.replacements[0].new_file_name
    );
    f.put("other.jar", b"changed after confirmation");
    assert!(recheck_update_target(&plan, &AtomicBool::new(false)).is_err());
}
#[test]
fn update_graph_rejects_prerelease_required_pin_and_conflicting_selected_versions() {
    let f = Fixture::new();
    let mut p = basic(&f);
    pair(
        &mut p,
        &f,
        "Proj0002",
        "Vers0003",
        "Vers0004",
        "dep-old.jar",
        "dep-new.jar",
        b"dep old",
        b"dep new",
    );
    p.versions.get_mut("Vers0004").unwrap().version_type = "alpha".into();
    p.versions
        .get_mut("Vers0002")
        .unwrap()
        .dependencies
        .push(dependency(Some("Proj0002"), Some("Vers0004"), "required"));
    assert!(f
        .prepare(&p, &["old.jar"])
        .unwrap_err()
        .contains("alpha/beta"));
    p.versions.get_mut("Vers0004").unwrap().version_type = "release".into();
    p.versions.get_mut("Vers0002").unwrap().dependencies[0].version_id = Some("Vers0003".into());
    assert!(f
        .prepare(&p, &["old.jar", "dep-old.jar"])
        .unwrap_err()
        .contains("多个版本"));
}
struct BulkProvider {
    inner: FakeProvider,
    candidates: BTreeMap<String, Version>,
    single: AtomicUsize,
    hash_calls: AtomicUsize,
    update_calls: AtomicUsize,
    project_calls: AtomicUsize,
}
impl BulkProvider {
    fn new(inner: FakeProvider, candidates: BTreeMap<String, Version>) -> Self {
        Self {
            inner,
            candidates,
            single: AtomicUsize::new(0),
            hash_calls: AtomicUsize::new(0),
            update_calls: AtomicUsize::new(0),
            project_calls: AtomicUsize::new(0),
        }
    }
}
impl Provider for BulkProvider {
    fn project<'a>(&'a self, id: &'a str) -> FutureResult<'a, Project> {
        self.single.fetch_add(1, Ordering::Relaxed);
        self.inner.project(id)
    }
    fn version<'a>(&'a self, id: &'a str) -> FutureResult<'a, Version> {
        self.single.fetch_add(1, Ordering::Relaxed);
        self.inner.version(id)
    }
    fn versions<'a>(&'a self, id: &'a str, c: &'a Compatibility) -> FutureResult<'a, Vec<Version>> {
        self.single.fetch_add(1, Ordering::Relaxed);
        self.inner.versions(id, c)
    }
    fn from_hashes<'a>(
        &'a self,
        hashes: &'a [String],
    ) -> FutureResult<'a, BTreeMap<String, Version>> {
        self.hash_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.from_hashes(hashes)
    }
    fn projects<'a>(&'a self, ids: &'a [String]) -> FutureResult<'a, Vec<Project>> {
        self.project_calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            assert!(ids.len() <= 256);
            Ok(ids
                .iter()
                .filter_map(|id| self.inner.projects.get(id).cloned())
                .collect())
        })
    }
    fn updates<'a>(
        &'a self,
        hashes: &'a [String],
        c: &'a Compatibility,
    ) -> FutureResult<'a, BTreeMap<String, Version>> {
        self.update_calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            assert!(hashes.len() <= 256);
            assert_eq!(c.minecraft_version, "1.20.1");
            assert_eq!(c.loader, "fabric");
            Ok(hashes
                .iter()
                .filter_map(|hash| {
                    self.candidates
                        .get(hash)
                        .cloned()
                        .map(|v| (hash.clone(), v))
                })
                .collect())
        })
    }
}
#[test]
fn four_hundred_mod_check_uses_six_bulk_calls_and_no_individual_project_gets() {
    let f = Fixture::new();
    let mut inner = FakeProvider::default();
    let mut candidates = BTreeMap::new();
    for i in 0..400 {
        let project = format!("P{i:07}");
        let old = format!("O{i:07}");
        let new = format!("N{i:07}");
        let old_bytes = format!("old mod {i}");
        let new_bytes = format!("new mod {i}");
        pair(
            &mut inner,
            &f,
            &project,
            &old,
            &new,
            &format!("old-{i}.jar"),
            &format!("new-{i}.jar"),
            old_bytes.as_bytes(),
            new_bytes.as_bytes(),
        );
        candidates.insert(sha(old_bytes.as_bytes()), inner.versions[&new].clone());
    }
    let p = BulkProvider::new(inner, candidates);
    let result = run(check::check(&p, &f.target(), &AtomicBool::new(false))).unwrap();
    assert_eq!(result.entries.len(), 400);
    assert!(result
        .entries
        .iter()
        .all(|e| e.status == UpdateStatus::UpdateAvailable));
    assert_eq!(p.single.load(Ordering::Relaxed), 0);
    assert_eq!(p.hash_calls.load(Ordering::Relaxed), 2);
    assert_eq!(p.update_calls.load(Ordering::Relaxed), 2);
    assert_eq!(p.project_calls.load(Ordering::Relaxed), 2);
}
#[test]
fn invalid_selection_fails_before_network_and_bulk_identity_mismatch_is_blocked() {
    let f = Fixture::new();
    let inner = basic(&f);
    let mut new = inner.versions["Vers0002"].clone();
    new.project_id = "Proj9999".into();
    let p = BulkProvider::new(inner, BTreeMap::from([(sha(b"old"), new)]));
    let mut selected = f.selection(&["old.jar"]);
    selected[0].fingerprint = "stale token".into();
    assert!(run(prepare_with(
        &p,
        f.target(),
        selected,
        &AtomicBool::new(false)
    ))
    .unwrap_err()
    .contains("已变化"));
    assert_eq!(p.hash_calls.load(Ordering::Relaxed), 0);
    let checked = run(check::check(&p, &f.target(), &AtomicBool::new(false))).unwrap();
    assert_eq!(checked.entries[0].status, UpdateStatus::Blocked);
    assert!(checked.entries[0]
        .reason
        .as_ref()
        .unwrap()
        .contains("项目ID"));
    assert!(provider::validate_projects(
        &["Proj0001".into()],
        &[
            p.inner.projects["Proj0001"].clone(),
            p.inner.projects["Proj0001"].clone()
        ]
    )
    .is_err());
    assert!(provider::validate_updates(
        &[sha(b"old")],
        &BTreeMap::from([(sha(b"unrequested"), p.inner.versions["Vers0002"].clone())])
    )
    .is_err());
}

#[test]
#[ignore = "Explicit read-only public Modrinth bulk metadata probe; excluded from offline gates"]
fn official_public_bulk_update_metadata_probe() {
    run(async {
        let cancel = AtomicBool::new(false);
        let provider = provider::HttpProvider::new(&cancel).unwrap();
        // Public Fabric API metadata supplies its own SHA512; no installed
        // directory, account, file download or guessed local identity is read.
        let ids = vec!["P7dR8mSH".into()];
        let projects = provider.projects(&ids).await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, ids[0]);
        assert_eq!(projects[0].project_type, "mod");
        let compatibility = Compatibility {
            minecraft_version: "1.20.1".into(),
            loader: "fabric".into(),
            shader_engines: vec![],
        };
        let versions = provider.versions(&ids[0], &compatibility).await.unwrap();
        let oldest = versions
            .iter()
            .filter(|version| {
                version.version_type == "release"
                    && plan::compatible(version, "mods", &compatibility)
                    && version.files.iter().any(|file| file.primary)
            })
            .min_by_key(|version| check::published(version).unwrap())
            .unwrap();
        let old_file = oldest.files.iter().find(|file| file.primary).unwrap();
        let hashes = vec![old_file.hashes.sha512.clone()];
        let recognized = provider.from_hashes(&hashes).await.unwrap();
        assert_eq!(recognized[&hashes[0]].project_id, ids[0]);
        assert!(recognized[&hashes[0]]
            .files
            .iter()
            .any(|file| file.hashes.sha512 == hashes[0] && file.size == old_file.size));
        let candidates = provider.updates(&hashes, &compatibility).await.unwrap();
        let candidate = &candidates[&hashes[0]];
        assert_eq!(candidate.project_id, ids[0]);
        assert!(plan::compatible(candidate, "mods", &compatibility));
        assert!(check::newer(candidate, oldest).unwrap());
        assert!(candidate.files.iter().any(|file| file.primary));
        println!(
            "Official bulk metadata: project={}, old={}, new={}, received_bytes={}",
            ids[0],
            oldest.id,
            candidate.id,
            provider.network_bytes.load(Ordering::Relaxed)
        );
    });
}
