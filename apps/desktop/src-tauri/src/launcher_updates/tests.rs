//! Generic fixtures only. No test launches an ELF or reads a real installation.
use super::*;
use provider::{Asset, AssetStream, BoxFuture, Comparison, Release, REPOSITORY};
use std::{collections::HashMap, fs, time::Duration};

struct Fixture(PathBuf, tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let (path, temporary) = crate::launcher_updates::test_support::tree();
        crate::launcher_updates::test_support::write_new(
            &path.join(".pcl-rust/bin/pcl-desktop"),
            &elf(Architecture::X86_64),
        );
        Self(path, temporary)
    }
    fn binary(&self) -> PathBuf {
        self.0.join(".pcl-rust/bin/pcl-desktop")
    }
    fn staging(&self) -> PathBuf {
        self.0.join(".pcl-rust/launcher-update-staging")
    }
    fn build(&self) -> CurrentBuild {
        CurrentBuild {
            version: "0.2.0".into(),
            commit: Some("a".repeat(40)),
            executable: self.binary().to_string_lossy().into_owned(),
            install_channel: InstallChannel::Portable,
            architecture: Architecture::X86_64,
        }
    }
    fn service(&self, provider: FakeProvider) -> UpdateService {
        let mut service = UpdateService::new(&self.0, self.build()).unwrap();
        service.test_provider = Some(Arc::new(provider));
        service
    }
    fn no_partial_names(&self) {
        super::test_support::no_partial(&self.staging());
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn elf(architecture: Architecture) -> Vec<u8> {
    super::test_support::native_binary(architecture)
}
#[cfg(target_os = "linux")]
pub(super) fn elf(architecture: Architecture) -> Vec<u8> {
    let mut bytes = vec![0u8; 256];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
    let machine: u16 = if architecture == Architecture::Aarch64 {
        183
    } else {
        62
    };
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x400080u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
    bytes[68..72].copy_from_slice(&5u32.to_le_bytes());
    bytes[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
    bytes[96..104].copy_from_slice(&256u64.to_le_bytes());
    bytes[104..112].copy_from_slice(&256u64.to_le_bytes());
    bytes
}

struct FakeProvider {
    releases: Vec<Release>,
    assets: HashMap<u64, Vec<u8>>,
    commit: String,
    comparison: (String, u64, u64),
    delay: Duration,
    failure: Option<String>,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl FakeProvider {
    fn release(version: &str, prerelease: bool) -> Self {
        let binary = elf(Architecture::X86_64);
        let commit = "b".repeat(40);
        let tag = format!("v{version}");
        let mut manifest = serde_json::json!({"schema_version":1,"repository":REPOSITORY,"version":version,"tag":tag,"commit":commit,
            "artifacts":[{"name":"pcl-desktop-linux-x86_64","architecture":"x86_64","format":"elf","size":binary.len(),"sha256":format!("{:x}",Sha256::digest(&binary))}]});
        if !cfg!(target_os = "linux") {
            manifest["schema_version"] = 2.into();
            manifest["artifacts"][0]["os"] = binary::os().into();
            manifest["artifacts"][0]["format"] = binary::format().into();
            manifest["artifacts"][0]["name"] = Architecture::X86_64.artifact_name().unwrap().into();
        }
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let releases = vec![Release {
            id: 10,
            tag_name: tag,
            draft: false,
            prerelease,
            assets: vec![
                Asset {
                    id: 1,
                    name: MANIFEST_NAME.into(),
                    state: "uploaded".into(),
                    size: manifest.len() as u64,
                    digest: None,
                },
                Asset {
                    id: 2,
                    name: Architecture::X86_64.artifact_name().unwrap().into(),
                    state: "uploaded".into(),
                    size: binary.len() as u64,
                    digest: None,
                },
            ],
        }];
        Self {
            releases,
            assets: HashMap::from([(1, manifest), (2, binary)]),
            commit,
            comparison: ("ahead".into(), 1, 0),
            delay: Duration::ZERO,
            failure: None,
            hook: Mutex::new(None),
        }
    }
    fn edit_manifest(&mut self, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut manifest = serde_json::from_slice(&self.assets[&1]).unwrap();
        edit(&mut manifest);
        let bytes = serde_json::to_vec(&manifest).unwrap();
        self.releases[0].assets[0].size = bytes.len() as u64;
        self.assets.insert(1, bytes);
    }
}
impl ReleaseProvider for FakeProvider {
    fn releases(&self) -> BoxFuture<'_, Vec<Release>> {
        Box::pin(async move {
            if let Some(error) = &self.failure {
                return Err(error.clone());
            }
            Ok(self.releases.clone())
        })
    }
    fn tag_commit<'a>(&'a self, _tag: &'a str) -> BoxFuture<'a, String> {
        Box::pin(async move { Ok(self.commit.clone()) })
    }
    fn compare<'a>(&'a self, _base: &'a str, _head: &'a str) -> BoxFuture<'a, Comparison> {
        Box::pin(async move {
            Ok(Comparison {
                status: self.comparison.0.clone(),
                ahead_by: self.comparison.1,
                behind_by: self.comparison.2,
            })
        })
    }
    fn asset(&self, id: u64, prefix: bool) -> BoxFuture<'_, Box<dyn AssetStream>> {
        Box::pin(async move {
            let mut bytes = self.assets.get(&id).ok_or("fake missing asset")?.clone();
            let delay = if id == 2 && !prefix {
                self.delay
            } else {
                Duration::ZERO
            };
            if prefix {
                bytes.truncate(binary::PREFIX);
            }
            if id == 2 && !prefix {
                if let Some(hook) = self.hook.lock().unwrap().take() {
                    hook();
                }
            }
            Ok(Box::new(FakeStream {
                bytes,
                position: 0,
                delay,
            }) as Box<dyn AssetStream>)
        })
    }
}
struct FakeStream {
    bytes: Vec<u8>,
    position: usize,
    delay: Duration,
}
impl AssetStream for FakeStream {
    fn next_chunk(&mut self) -> BoxFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            if self.position == self.bytes.len() {
                return Ok(None);
            }
            let end = (self.position + 32).min(self.bytes.len());
            let chunk = self.bytes[self.position..end].to_vec();
            self.position = end;
            Ok(Some(chunk))
        })
    }
}

#[tokio::test]
async fn empty_releases_are_not_latest_and_checks_create_no_storage() {
    let fixture = Fixture::new();
    let mut provider = FakeProvider::release("0.3.0", false);
    provider.releases.clear();
    let service = fixture.service(provider);
    assert_eq!(
        service.check(UpdateChannel::Stable).await.unwrap().state,
        CheckState::NoCompatibleRelease
    );
    assert!(!fixture.staging().exists());
}

#[tokio::test]
async fn api_failure_is_visible_error_not_a_latest_claim() {
    let fixture = Fixture::new();
    let mut provider = FakeProvider::release("0.3.0", false);
    provider.failure = Some("GitHub public rate limit".into());
    let service = fixture.service(provider);
    let view = service.check(UpdateChannel::Stable).await.unwrap();
    assert_eq!(view.state, CheckState::Error);
    assert!(view.release.is_none());
    assert!(!service.is_busy());
}

#[tokio::test]
async fn stable_excludes_prereleases_beta_accepts_and_drafts_never_appear() {
    let fixture = Fixture::new();
    let service = fixture.service(FakeProvider::release("0.3.0-beta.1", true));
    assert_eq!(
        service.check(UpdateChannel::Stable).await.unwrap().state,
        CheckState::NoCompatibleRelease
    );
    assert_eq!(
        service.check(UpdateChannel::Beta).await.unwrap().state,
        CheckState::Available
    );
    let mut provider = FakeProvider::release("0.3.0", false);
    provider.releases[0].draft = true;
    assert_eq!(
        fixture
            .service(provider)
            .check(UpdateChannel::Beta)
            .await
            .unwrap()
            .state,
        CheckState::NoCompatibleRelease
    );
}

#[tokio::test]
async fn same_semver_needs_commit_ancestry_and_metadata_does_not_add_precedence() {
    let fixture = Fixture::new();
    let service = fixture.service(FakeProvider::release("0.2.0+release.1", false));
    assert_eq!(
        service.check(UpdateChannel::Stable).await.unwrap().state,
        CheckState::Available
    );
    let mut provider = FakeProvider::release("0.2.0+release.1", false);
    provider.comparison = ("behind".into(), 0, 4);
    assert_eq!(
        fixture
            .service(provider)
            .check(UpdateChannel::Stable)
            .await
            .unwrap()
            .state,
        CheckState::Latest
    );
    let mut provider = FakeProvider::release("0.2.0", false);
    provider.comparison = ("diverged".into(), 1, 1);
    assert_eq!(
        fixture
            .service(provider)
            .check(UpdateChannel::Stable)
            .await
            .unwrap()
            .state,
        CheckState::NoCompatibleRelease
    );
}

#[tokio::test]
async fn equal_unknown_build_or_equal_same_commit_never_claims_new_version() {
    let fixture = Fixture::new();
    let mut build = fixture.build();
    build.commit = None;
    let mut service = UpdateService::new(&fixture.0, build).unwrap();
    service.test_provider = Some(Arc::new(FakeProvider::release("0.2.0", false)));
    assert_eq!(
        service.check(UpdateChannel::Stable).await.unwrap().state,
        CheckState::NoCompatibleRelease
    );
    let mut build = fixture.build();
    build.commit = Some("b".repeat(40));
    let mut service = UpdateService::new(&fixture.0, build).unwrap();
    service.test_provider = Some(Arc::new(FakeProvider::release("0.2.0", false)));
    assert_eq!(
        service.check(UpdateChannel::Stable).await.unwrap().state,
        CheckState::Latest
    );
}

#[tokio::test]
async fn manifest_is_bound_to_repository_version_asset_size_and_actual_tag_commit() {
    let fixture = Fixture::new();
    for edit in 0..4 {
        let mut provider = FakeProvider::release("0.3.0", false);
        provider.edit_manifest(|m| match edit {
            0 => m["repository"] = "other/project".into(),
            1 => m["version"] = "0.4.0".into(),
            2 => m["artifacts"][0]["size"] = 1024.into(),
            _ => m["commit"] = "c".repeat(40).into(),
        });
        let view = fixture
            .service(provider)
            .check(UpdateChannel::Stable)
            .await
            .unwrap();
        assert_eq!(view.state, CheckState::NoCompatibleRelease);
        assert!(view.release.is_none());
    }
}

#[tokio::test]
async fn missing_duplicate_and_nonuploaded_assets_are_rejected() {
    let fixture = Fixture::new();
    for case in 0..3 {
        let mut provider = FakeProvider::release("0.3.0", false);
        match case {
            0 => {
                provider.releases[0].assets.remove(0);
            }
            1 => {
                let duplicate = provider.releases[0].assets[0].clone();
                provider.releases[0].assets.push(duplicate);
            }
            _ => provider.releases[0].assets[1].state = "starter".into(),
        }
        assert_eq!(
            fixture
                .service(provider)
                .check(UpdateChannel::Stable)
                .await
                .unwrap()
                .state,
            CheckState::NoCompatibleRelease
        );
    }
}

#[tokio::test]
async fn pe_and_wrong_machine_are_rejected_before_a_download_token_is_issued() {
    let fixture = Fixture::new();
    for wrong in [vec![b'M'; 256], elf(Architecture::Aarch64)] {
        let mut provider = FakeProvider::release("0.3.0", false);
        provider.assets.insert(2, wrong);
        assert_eq!(
            fixture
                .service(provider)
                .check(UpdateChannel::Stable)
                .await
                .unwrap()
                .state,
            CheckState::NoCompatibleRelease
        );
    }
}

#[tokio::test]
async fn successful_stage_verifies_hash_and_elf_without_replacing_original_inode() {
    let fixture = Fixture::new();
    let before = crate::launcher_updates::test_support::identity(fixture.binary());
    let original = fs::read(fixture.binary()).unwrap();
    let service = fixture.service(FakeProvider::release("0.3.0", false));
    let checked = service.check(UpdateChannel::Stable).await.unwrap();
    let release = checked.release.unwrap();
    assert_eq!(release.elf_header_verified, cfg!(target_os = "linux"));
    assert_eq!(
        release.artifact_name,
        Architecture::X86_64.artifact_name().unwrap()
    );
    let view = service.download(&release.token).await.unwrap();
    assert_eq!(view.download.state, DownloadState::Staged);
    assert_eq!(
        view.download.received_bytes,
        elf(Architecture::X86_64).len() as u64
    );
    assert_eq!(view.plan.as_ref().unwrap().can_apply, !cfg!(windows));
    assert_eq!(
        view.plan.as_ref().unwrap().original_sha256.is_some(),
        !cfg!(windows)
    );
    assert_eq!(
        crate::launcher_updates::test_support::identity(fixture.binary()),
        before
    );
    assert_eq!(fs::read(fixture.binary()).unwrap(), original);
    #[cfg(target_os = "linux")]
    fixture.no_partial_names();
    assert!(service.discard_staged().unwrap().plan.is_none());
    assert!(service.state.lock().unwrap().staged.is_none());
    fixture.no_partial_names();
}

#[tokio::test]
async fn wrong_hash_truncated_or_oversized_downloads_leave_no_partial_file() {
    let fixture = Fixture::new();
    let original = fs::read(fixture.binary()).unwrap();
    for case in 0..3 {
        let mut provider = FakeProvider::release("0.3.0", false);
        let mut bytes = provider.assets[&2].clone();
        match case {
            0 => bytes[200] ^= 1,
            1 => {
                bytes.truncate(if cfg!(windows) { 256 } else { 128 });
            }
            _ => bytes.push(0),
        }
        provider.assets.insert(2, bytes);
        let service = fixture.service(provider);
        let token = service
            .check(UpdateChannel::Stable)
            .await
            .unwrap()
            .release
            .unwrap()
            .token;
        assert_eq!(
            service.download(&token).await.unwrap().download.state,
            DownloadState::Error
        );
        assert!(service.state.lock().unwrap().staged.is_none());
        fixture.no_partial_names();
        assert_eq!(fs::read(fixture.binary()).unwrap(), original);
    }
}

#[tokio::test]
async fn full_elf_program_bounds_and_executable_entry_are_checked_after_hash() {
    let fixture = Fixture::new();
    let mut provider = FakeProvider::release("0.3.0", false);
    let mut binary = provider.assets[&2].clone();
    if cfg!(windows) {
        binary[216..220].copy_from_slice(&4096u32.to_le_bytes());
    } else {
        binary[96..104].copy_from_slice(&4096u64.to_le_bytes());
    }
    let hash = format!("{:x}", Sha256::digest(&binary));
    provider.assets.insert(2, binary);
    provider.edit_manifest(|m| m["artifacts"][0]["sha256"] = hash.into());
    let service = fixture.service(provider);
    let token = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    assert_eq!(
        service.download(&token).await.unwrap().download.state,
        DownloadState::Error
    );
    fixture.no_partial_names();
}

#[tokio::test]
async fn cancellation_interrupts_stalled_stream_and_keeps_busy_until_cleanup() {
    let fixture = Fixture::new();
    let mut provider = FakeProvider::release("0.3.0", false);
    provider.delay = Duration::from_secs(10);
    let service = fixture.service(provider);
    let token = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    let operation = service.download(&token);
    let cancellation = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(service.is_busy());
        assert!(service.cancel());
        assert!(service.is_busy());
        assert!(service.discard_staged().is_err());
    };
    let (view, ()) = tokio::join!(operation, cancellation);
    assert_eq!(view.unwrap().download.state, DownloadState::Cancelled);
    assert!(!service.is_busy());
    fixture.no_partial_names();
}

#[tokio::test]
async fn dropping_an_operation_future_releases_admission_and_anonymous_stage() {
    let fixture = Fixture::new();
    let mut provider = FakeProvider::release("0.3.0", false);
    provider.delay = Duration::from_secs(10);
    let service = fixture.service(provider);
    let token = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), service.download(&token))
            .await
            .is_err()
    );
    assert!(!service.is_busy());
    assert_eq!(service.snapshot().download.state, DownloadState::Cancelled);
    fixture.no_partial_names();
}

#[tokio::test]
async fn symlink_and_replaced_staging_directory_are_refused_without_writing_target() {
    let fixture = Fixture::new();
    let original = fs::read(fixture.binary()).unwrap();
    let original_identity = super::test_support::identity(fixture.binary());
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    super::test_support::symlink_dir(&outside, fixture.staging()).unwrap();
    let service = fixture.service(FakeProvider::release("0.3.0", false));
    let token = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    assert_eq!(
        service.download(&token).await.unwrap().download.state,
        DownloadState::Error
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    super::test_support::unlink_dir_alias(&fixture.staging());
    let provider = FakeProvider::release("0.3.0", false);
    let staging = fixture.staging();
    let old = fixture.0.join("retained-staging");
    *provider.hook.lock().unwrap() = Some(Box::new(move || {
        #[cfg(not(windows))]
        {
            fs::rename(&staging, &old).unwrap();
            fs::create_dir(&staging).unwrap();
        }
        #[cfg(windows)]
        {
            // The live source is a retained descendant. Windows refuses this
            // ancestor replacement while it is held; no directory was swapped.
            let identity = files::Directory::open(&staging)
                .unwrap()
                .identity()
                .unwrap();
            assert_eq!(files::pending(&staging).unwrap(), 1);
            let error = fs::rename(&staging, &old).unwrap_err();
            assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
            assert_eq!(
                files::Directory::open(&staging)
                    .unwrap()
                    .identity()
                    .unwrap(),
                identity
            );
            assert!(!old.exists());
        }
    }));
    let service = fixture.service(provider);
    let token = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    let view = service.download(&token).await.unwrap();
    #[cfg(not(windows))]
    assert_eq!(view.download.state, DownloadState::Error);
    #[cfg(windows)]
    {
        assert_eq!(view.download.state, DownloadState::Staged);
        assert!(!view.plan.unwrap().can_apply);
        assert_eq!(files::pending(&fixture.staging()).unwrap(), 1);
        assert!(service.discard_staged().unwrap().plan.is_none());
        assert!(service.state.lock().unwrap().staged.is_none());
        assert!(!fixture.0.join("retained-staging").exists());
    }
    fixture.no_partial_names();
    assert_eq!(fs::read(fixture.binary()).unwrap(), original);
    assert_eq!(
        super::test_support::identity(fixture.binary()),
        original_identity
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    #[cfg(not(windows))]
    super::test_support::no_partial(&fixture.0.join("retained-staging"));
}

#[tokio::test]
async fn package_install_has_explicit_package_manager_plan_and_never_reads_system_binary() {
    let fixture = Fixture::new();
    let mut build = fixture.build();
    build.install_channel = InstallChannel::PackageManaged;
    build.executable = if cfg!(windows) {
        fixture
            .0
            .join("package-owned-not-read.exe")
            .display()
            .to_string()
    } else {
        "/usr/bin/pcl-desktop".into()
    };
    let mut service = UpdateService::new(&fixture.0, build).unwrap();
    service.test_provider = Some(Arc::new(FakeProvider::release("0.3.0", false)));
    let token = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    let plan = service.download(&token).await.unwrap().plan.unwrap();
    assert!(plan.destination.is_none());
    assert!(plan.original_sha256.is_none());
    assert!(!plan.can_apply);
    assert!(plan.reason.contains("包管理"));
}

#[tokio::test]
async fn stale_token_rejected_and_rechecking_invalidates_previous_selection() {
    let fixture = Fixture::new();
    let service = fixture.service(FakeProvider::release("0.3.0", false));
    assert!(service.download("arbitrary").await.is_err());
    let old = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    let new = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    assert_ne!(old, new);
    assert!(service.download(&old).await.is_err());
    assert!(service.download(&new).await.is_ok());
}

#[tokio::test]
async fn service_apply_and_rollback_keep_running_identity_separate_from_disk() {
    let fixture = Fixture::new();
    let original_inode = crate::launcher_updates::test_support::identity(fixture.binary());
    let service = fixture.service(FakeProvider::release("0.3.0", false));
    let token = service
        .check(UpdateChannel::Stable)
        .await
        .unwrap()
        .release
        .unwrap()
        .token;
    if cfg!(windows) {
        let staged = service.download(&token).await.unwrap();
        assert!(!staged.plan.unwrap().can_apply);
        assert!(service.apply(&token).is_err());
        service.discard_staged().unwrap();
        assert_eq!(
            crate::launcher_updates::test_support::identity(fixture.binary()),
            original_inode
        );
        fixture.no_partial_names();
        return;
    }
    assert!(
        service
            .download(&token)
            .await
            .unwrap()
            .plan
            .unwrap()
            .can_apply
    );
    let applied = service.apply(&token).unwrap();
    assert_eq!(applied.current.version, "0.2.0");
    assert_eq!(applied.current.commit, Some("a".repeat(40)));
    assert_eq!(applied.installation.state, InstallationState::Applied);
    assert_eq!(
        applied.installation.disk_build.as_ref().unwrap().version,
        "0.3.0"
    );
    assert!(applied.installation.restart_required);
    assert!(applied.plan.is_none());
    assert!(service.apply(&token).is_err());
    let rollback_token = applied.installation.rollback.unwrap().token;
    let rolled = service.rollback(&rollback_token).unwrap();
    assert_eq!(rolled.installation.state, InstallationState::RolledBack);
    assert_eq!(rolled.current.version, "0.2.0");
    assert!(!rolled.installation.restart_required);
    assert_eq!(
        crate::launcher_updates::test_support::identity(fixture.binary()),
        original_inode
    );
    assert_eq!(
        service.recover().unwrap().installation.state,
        InstallationState::RolledBack
    );
}

#[test]
fn redirects_allow_only_https_official_asset_hosts_and_never_credentials() {
    for url in [
        "https://release-assets.githubusercontent.com/path?signature=opaque",
        "https://objects.githubusercontent.com/path",
    ] {
        assert!(provider::trusted_asset_redirect(
            &reqwest::Url::parse(url).unwrap()
        ));
    }
    for url in [
        "http://objects.githubusercontent.com/path",
        "https://objects.githubusercontent.com.attacker.example/path",
        "https://user:secret@objects.githubusercontent.com/path",
        "https://objects.githubusercontent.com:8443/path",
        "https://github.com/other/project",
        "https://127.0.0.1/path",
    ] {
        assert!(!provider::trusted_asset_redirect(
            &reqwest::Url::parse(url).unwrap()
        ));
    }
}

#[test]
fn manifest_has_no_arbitrary_url_or_unknown_fields_and_current_requires_full_commit() {
    let fixture = Fixture::new();
    let mut provider = FakeProvider::release("0.3.0", false);
    provider.edit_manifest(|m| m["artifacts"][0]["url"] = "https://attacker.example/elf".into());
    assert!(parse_manifest(
        &provider.assets[&1],
        &provider.releases[0],
        &Version::parse("0.3.0").unwrap(),
        &Architecture::X86_64
    )
    .is_err());
    let mut build = fixture.build();
    build.commit = Some("shortsha".into());
    assert!(UpdateService::new(&fixture.0, build).is_err());
}

#[tokio::test]
async fn durable_retirement_runs_without_state_lock_and_keeps_operation_busy() {
    for discard in [true, false] {
        let fixture = Fixture::new();
        let service = Arc::new(fixture.service(FakeProvider::release("0.3.0", false)));
        let token = service
            .check(UpdateChannel::Stable)
            .await
            .unwrap()
            .release
            .unwrap()
            .token;
        assert_eq!(
            service.download(&token).await.unwrap().download.state,
            DownloadState::Staged
        );
        let observed = Arc::new(AtomicBool::new(false));
        let flag = observed.clone();
        let weak = Arc::downgrade(&service);
        service
            .state
            .lock()
            .unwrap()
            .staged
            .as_mut()
            .unwrap()
            .retire_hook = Some(Box::new(move || {
            let service = weak.upgrade().unwrap();
            assert!(
                service.state.try_lock().is_ok(),
                "retirement held State mutex"
            );
            assert!(
                service.is_busy(),
                "admission released before actual retirement"
            );
            flag.store(true, AtomicOrdering::Release);
        }));
        if discard {
            service.discard_staged().unwrap();
        } else {
            service.check(UpdateChannel::Stable).await.unwrap();
        }
        assert!(observed.load(AtomicOrdering::Acquire));
        fixture.no_partial_names();
        assert!(!service.is_busy());
    }
}
