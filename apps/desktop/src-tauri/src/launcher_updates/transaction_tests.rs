//! Installation fixtures use generic fake ELF data and never launch a process.
use super::*;
use crate::launcher_updates::{files::Staging, tests::elf};
use std::fs;
#[cfg(target_os = "macos")]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
struct Fixture(PathBuf, tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let (path, temporary) = crate::launcher_updates::test_support::tree();
        crate::launcher_updates::test_support::write_new(
            &path.join(".pcl-rust/bin/pcl-desktop"),
            &elf(Architecture::X86_64),
        );
        #[cfg(unix)]
        fs::set_permissions(
            path.join(".pcl-rust/bin/pcl-desktop"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        Self(path, temporary)
    }
    fn binary(&self) -> PathBuf {
        self.0.join(".pcl-rust/bin/pcl-desktop")
    }
    fn current(&self) -> CurrentBuild {
        CurrentBuild {
            version: "0.2.0".into(),
            commit: Some("a".repeat(40)),
            executable: self.binary().to_string_lossy().into(),
            install_channel: InstallChannel::Portable,
            architecture: Architecture::X86_64,
        }
    }
    fn stage(&self) -> (ReleaseView, Stage) {
        let staging = Staging::prepare(&self.0).unwrap();
        let mut stage = staging.anonymous().unwrap();
        stage.original = Some(staging.original_token().unwrap());
        let mut replacement = elf(Architecture::X86_64);
        replacement[200] = 1;
        stage
            .append(&replacement, replacement.len() as u64)
            .unwrap();
        let release = ReleaseView {
            token: "d".repeat(32),
            tag: "v0.3.0".into(),
            version: "0.3.0".into(),
            commit: "b".repeat(40),
            prerelease: false,
            artifact_name: "pcl-desktop-linux-x86_64".into(),
            architecture: Architecture::X86_64,
            bytes: replacement.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&replacement)),
            elf_header_verified: true,
        };
        stage
            .finish(release.bytes, &release.sha256, &release.architecture)
            .unwrap();
        (release, stage)
    }
    fn applied(&self) -> (InstallationView, Journal) {
        let (release, stage) = self.stage();
        let view = apply(
            &self.0,
            &self.current(),
            &release,
            stage,
            &AtomicBool::new(false),
            &|| true,
        );
        assert_eq!(view.state, InstallationState::Applied, "{:?}", view.warning);
        let journal = Fs::open(&self.0).unwrap().journal().unwrap().unwrap();
        (view, journal)
    }
    fn interrupted(&self, publish: bool, exchange: bool) -> Journal {
        let (release, mut stage) = self.stage();
        let nonce = super::super::opaque_token().unwrap();
        let fs = Fs::open(&self.0).unwrap();
        #[cfg(not(target_os = "linux"))]
        stage
            .bind_target(&fs.bin, &format!(".pcl-desktop-update-{nonce}.swap"))
            .unwrap();
        #[cfg(unix)]
        stage
            .file_mut()
            .set_permissions(fs::Permissions::from_mode(if cfg!(target_os = "linux") {
                0o755
            } else {
                0o700
            }))
            .unwrap();
        stage.file().sync_all().unwrap();
        let original = stage.original.clone().unwrap();
        let replacement = FileToken::from_file(stage.file(), release.sha256.clone()).unwrap();
        let journal = Journal {
            schema_version: SCHEMA,
            nonce,
            project: fs.project.identity().unwrap(),
            storage: fs.storage.identity().unwrap(),
            bin: fs.bin.identity().unwrap(),
            original_build: DiskBuild {
                version: "0.2.0".into(),
                commit: Some("a".repeat(40)),
                sha256: original.sha256.clone(),
            },
            replacement_build: DiskBuild {
                version: release.version,
                commit: Some(release.commit),
                sha256: replacement.sha256.clone(),
            },
            original,
            replacement,
        };
        fs.publish(&fs.storage, JOURNAL, &serde_json::to_vec(&journal).unwrap())
            .unwrap();
        if publish {
            #[cfg(target_os = "linux")]
            link_anonymous(stage.file(), &fs.bin, &journal.swap_name()).unwrap();
            #[cfg(not(target_os = "linux"))]
            stage.publish(&fs.bin, &journal.swap_name()).unwrap();
            fs.bin.sync().unwrap();
        }
        if exchange {
            fs.exchange(&journal).unwrap();
        }
        journal
    }
}

#[test]
#[cfg(not(windows))]
fn apply_preserves_original_inode_and_running_build_then_rollback_restores_it() {
    let fixture = Fixture::new();
    let before = crate::launcher_updates::test_support::identity(fixture.binary());
    let original = fs::read(fixture.binary()).unwrap();
    let (view, journal) = fixture.applied();
    assert!(view.restart_required);
    assert_eq!(view.disk_build.as_ref().unwrap().version, "0.3.0");
    let fs = Fs::open(&fixture.0).unwrap();
    let (current, backup) = fs.tokens(&journal).unwrap();
    assert_eq!(current.unwrap(), journal.replacement);
    assert_eq!(backup.unwrap().identity(), before);
    assert_eq!(
        crate::launcher_updates::test_support::links(
            fixture.0.join(".pcl-rust/bin").join(journal.swap_name())
        ),
        1
    );
    let rolled = rollback(&fixture.0, &fixture.current(), &journal.nonce);
    assert_eq!(
        rolled.state,
        InstallationState::RolledBack,
        "{:?}",
        rolled.warning
    );
    assert!(!rolled.restart_required);
    assert_eq!(
        crate::launcher_updates::test_support::identity(fixture.binary()),
        before
    );
    assert_eq!(fs::read(fixture.binary()).unwrap(), original);
    assert!(fs.marker(&journal, "rollback-requested").unwrap());
    assert!(fs.marker(&journal, "rolled-back").unwrap());
}

#[test]
#[cfg(not(windows))]
fn startup_before_publish_after_publish_and_after_exchange_restores_original() {
    for (publish, exchange) in [(false, false), (true, false), (true, true)] {
        let fixture = Fixture::new();
        let original = fs::read(fixture.binary()).unwrap();
        let original_inode = crate::launcher_updates::test_support::identity(fixture.binary());
        let journal = fixture.interrupted(publish, exchange);
        let view = recover(&fixture.0, &fixture.current());
        assert_eq!(
            view.state,
            InstallationState::RolledBack,
            "{:?}",
            view.warning
        );
        assert_eq!(
            crate::launcher_updates::test_support::identity(fixture.binary()),
            original_inode
        );
        assert_eq!(fs::read(fixture.binary()).unwrap(), original);
        assert_eq!(
            recover(&fixture.0, &fixture.current()).state,
            InstallationState::RolledBack
        );
        assert!(Fs::open(&fixture.0)
            .unwrap()
            .marker(&journal, "rolled-back")
            .unwrap());
    }
}

#[test]
#[cfg(not(windows))]
fn committed_startup_keeps_new_disk_build_and_new_process_can_acknowledge() {
    let fixture = Fixture::new();
    let (_, journal) = fixture.applied();
    let view = recover(&fixture.0, &fixture.current());
    assert_eq!(view.state, InstallationState::Applied);
    assert!(view.restart_required);
    assert_eq!(
        acknowledge(&fixture.0, &fixture.current(), &journal.nonce).state,
        InstallationState::RecoveryRequired
    );
    let mut current = fixture.current();
    current.version = "0.3.0".into();
    current.commit = Some("b".repeat(40));
    assert!(!recover(&fixture.0, &current).restart_required);
    let view = acknowledge(&fixture.0, &current, &journal.nonce);
    assert_eq!(view.state, InstallationState::Applied, "{:?}", view.warning);
    assert!(!has_pending(&fixture.0).unwrap());
    assert!(fixture
        .0
        .join(".pcl-rust")
        .join(format!("launcher-update-journal-{}.json", journal.nonce))
        .exists());
    assert!(fixture
        .0
        .join(".pcl-rust/bin")
        .join(journal.swap_name())
        .exists());
}

#[test]
#[cfg(not(windows))]
fn persisted_rollback_intent_recovers_crash_on_each_side_of_exchange() {
    for exchange in [false, true] {
        let fixture = Fixture::new();
        let (_, journal) = fixture.applied();
        let fs = Fs::open(&fixture.0).unwrap();
        fs.mark(&journal, "rollback-requested").unwrap();
        if exchange {
            fs.exchange(&journal).unwrap();
        }
        let view = recover(&fixture.0, &fixture.current());
        assert_eq!(
            view.state,
            InstallationState::RolledBack,
            "{:?}",
            view.warning
        );
        assert_eq!(fs.tokens(&journal).unwrap().0.unwrap(), journal.original);
    }
}

#[test]
#[cfg(not(windows))]
fn external_destination_or_backup_content_edit_blocks_recovery_without_overwrite() {
    for backup in [false, true] {
        let fixture = Fixture::new();
        let (_, journal) = fixture.applied();
        let edited = if backup {
            fixture.0.join(".pcl-rust/bin").join(journal.swap_name())
        } else {
            fixture.binary()
        };
        let mut bytes = fs::read(&edited).unwrap();
        bytes[201] = 23;
        fs::write(&edited, &bytes).unwrap();
        let before_inode = crate::launcher_updates::test_support::identity(&edited);
        let view = rollback(&fixture.0, &fixture.current(), &journal.nonce);
        assert_eq!(view.state, InstallationState::RecoveryRequired);
        assert_eq!(
            crate::launcher_updates::test_support::identity(&edited),
            before_inode
        );
        assert_eq!(fs::read(&edited).unwrap(), bytes);
        assert!(fixture.0.join(".pcl-rust").join(JOURNAL).exists());
    }
}

#[test]
#[cfg(not(windows))]
fn external_inode_replacement_and_symlink_are_retained() {
    for use_link in [false, true] {
        let fixture = Fixture::new();
        let (_, journal) = fixture.applied();
        let replacement = fixture.0.join("external-file");
        fs::write(&replacement, b"external user content").unwrap();
        fs::rename(fixture.binary(), fixture.0.join("retained-new")).unwrap();
        if use_link {
            crate::launcher_updates::test_support::symlink_file(&replacement, fixture.binary())
                .unwrap();
        } else {
            fs::rename(&replacement, fixture.binary()).unwrap();
        }
        assert_eq!(
            recover(&fixture.0, &fixture.current()).state,
            InstallationState::RecoveryRequired
        );
        if use_link {
            assert!(fs::symlink_metadata(fixture.binary()).unwrap().is_symlink());
            assert_eq!(fs::read(replacement).unwrap(), b"external user content");
        } else {
            assert_eq!(
                fs::read(fixture.binary()).unwrap(),
                b"external user content"
            );
        }
        assert!(fixture
            .0
            .join(".pcl-rust/bin")
            .join(journal.swap_name())
            .exists());
    }
}

#[test]
#[cfg(not(windows))]
fn original_edit_after_plan_refuses_apply_before_journal_and_stage_is_released() {
    let fixture = Fixture::new();
    let (release, stage) = fixture.stage();
    let identity = crate::launcher_updates::test_support::identity(fixture.binary());
    let staging_path = fixture.0.join(".pcl-rust/launcher-update-staging");
    #[cfg(not(target_os = "linux"))]
    let cleanup = {
        let staging = Staging::prepare(&fixture.0).unwrap();
        assert_eq!(files::pending(&staging_path).unwrap(), 1);
        staging.cleanup()
    };
    let mut original = fs::read(fixture.binary()).unwrap();
    original[203] = 91;
    fs::write(fixture.binary(), &original).unwrap();
    let view = apply(
        &fixture.0,
        &fixture.current(),
        &release,
        stage,
        &AtomicBool::new(false),
        &|| true,
    );
    assert_eq!(view.state, InstallationState::Error);
    assert_eq!(fs::read(fixture.binary()).unwrap(), original);
    assert_eq!(
        crate::launcher_updates::test_support::identity(fixture.binary()),
        identity
    );
    assert!(!has_pending(&fixture.0).unwrap());
    assert_eq!(
        fs::read_dir(fixture.0.join(".pcl-rust/bin"))
            .unwrap()
            .count(),
        1
    );
    // Native stages retain the registry directory and its retirement receipts.
    // Check the owned Ticket lifecycle rather than requiring an empty registry.
    crate::launcher_updates::test_support::no_partial(&staging_path);
    #[cfg(not(target_os = "linux"))]
    cleanup.check().unwrap();
}

#[test]
#[cfg(not(windows))]
fn malformed_future_or_externally_reformatted_journal_is_retained() {
    for change in [0, 1, 2] {
        let fixture = Fixture::new();
        let _ = fixture.interrupted(true, true);
        let journal = fixture.0.join(".pcl-rust").join(JOURNAL);
        let bytes = match change {
            0 => b"{malformed".to_vec(),
            1 => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
                value["schema_version"] = 99.into();
                serde_json::to_vec(&value).unwrap()
            }
            _ => {
                let value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
                serde_json::to_vec_pretty(&value).unwrap()
            }
        };
        fs::write(&journal, &bytes).unwrap();
        let binary = fs::read(fixture.binary()).unwrap();
        assert_eq!(
            recover(&fixture.0, &fixture.current()).state,
            InstallationState::RecoveryRequired
        );
        assert_eq!(fs::read(journal).unwrap(), bytes);
        assert_eq!(fs::read(fixture.binary()).unwrap(), binary);
    }
}

#[test]
#[cfg(not(windows))]
fn directory_substitution_and_marker_collision_preserve_external_data() {
    let fixture = Fixture::new();
    let journal = fixture.interrupted(true, true);
    let fs = Fs::open(&fixture.0).unwrap();
    let name = journal.marker_name("rolled-back");
    fs::write(fixture.0.join(".pcl-rust").join(&name), b"external marker").unwrap();
    assert_eq!(
        recover(&fixture.0, &fixture.current()).state,
        InstallationState::RecoveryRequired
    );
    assert_eq!(
        std::fs::read(fixture.0.join(".pcl-rust").join(name)).unwrap(),
        b"external marker"
    );
    fs::rename(
        fixture.0.join(".pcl-rust/bin"),
        fixture.0.join("retained-bin"),
    )
    .unwrap();
    fs::create_dir(fixture.0.join(".pcl-rust/bin")).unwrap();
    fs::write(fixture.binary(), b"external binary").unwrap();
    assert!(fs.bind_journal(&journal).is_err());
    assert_eq!(
        recover(&fixture.0, &fixture.current()).state,
        InstallationState::RecoveryRequired
    );
    assert_eq!(std::fs::read(fixture.binary()).unwrap(), b"external binary");
}

#[test]
#[cfg(target_os = "linux")]
fn process_lock_is_nonblocking_and_explicitly_unlocks_duplicated_description() {
    let fixture = Fixture::new();
    let fs = Fs::open(&fixture.0).unwrap();
    let lock = fs.lock().unwrap();
    let duplicate = unsafe { libc::dup(lock.0.as_raw_fd()) };
    assert!(duplicate >= 0);
    assert!(fs.lock().is_err());
    drop(lock);
    assert!(fs.lock().is_ok());
    unsafe {
        libc::close(duplicate);
    }
}

#[test]
fn no_journal_startup_does_not_create_launcher_data_or_touch_package_binary() {
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.0.join(".pcl-rust")).unwrap();
    let mut current = fixture.current();
    current.install_channel = InstallChannel::PackageManaged;
    current.executable = "/usr/bin/pcl-desktop".into();
    assert_eq!(recover(&fixture.0, &current).state, InstallationState::Idle);
    assert!(!fixture.0.join(".pcl-rust").exists());
}

#[test]
#[cfg(not(windows))]
fn cancellation_wins_final_commit_gate_and_original_inode_is_preserved() {
    let fixture = Fixture::new();
    let before = crate::launcher_updates::test_support::identity(fixture.binary());
    let (release, stage) = fixture.stage();
    let view = apply(
        &fixture.0,
        &fixture.current(),
        &release,
        stage,
        &AtomicBool::new(false),
        &|| false,
    );
    assert_eq!(
        view.state,
        InstallationState::RolledBack,
        "{:?}",
        view.warning
    );
    assert_eq!(
        crate::launcher_updates::test_support::identity(fixture.binary()),
        before
    );
    let fs = Fs::open(&fixture.0).unwrap();
    let journal = fs.journal().unwrap().unwrap();
    assert!(!fs.marker(&journal, "committed").unwrap());
    assert!(fs.marker(&journal, "rolled-back").unwrap());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn native_process_lock_is_nonblocking_and_releases_after_drop() {
    let fixture = Fixture::new();
    let fs = Fs::open(&fixture.0).unwrap();
    let lock = fs.lock().unwrap();
    assert!(Fs::open(&fixture.0).unwrap().lock().is_err());
    drop(lock);
    assert!(Fs::open(&fixture.0).unwrap().lock().is_ok());
}
#[cfg(windows)]
#[test]
fn native_windows_apply_refuses_before_creating_installation_journal_or_swap() {
    let fixture = Fixture::new();
    let original = fs::read(fixture.binary()).unwrap();
    let identity = crate::launcher_updates::test_support::identity(fixture.binary());
    let (release, stage) = fixture.stage();
    let result = apply(
        &fixture.0,
        &fixture.current(),
        &release,
        stage,
        &AtomicBool::new(false),
        &|| panic!("no Windows exchange admission"),
    );
    assert_eq!(result.state, InstallationState::Error);
    assert!(result.warning.unwrap().contains("独立应用器"));
    assert_eq!(fs::read(fixture.binary()).unwrap(), original);
    assert_eq!(
        crate::launcher_updates::test_support::identity(fixture.binary()),
        identity
    );
    assert!(!has_pending(&fixture.0).unwrap());
    crate::launcher_updates::test_support::no_partial(
        &fixture.0.join(".pcl-rust/launcher-update-staging"),
    );
}
#[cfg(windows)]
#[test]
fn native_windows_recovers_known_pre_exchange_evidence_without_touching_original() {
    for published in [false, true] {
        let fixture = Fixture::new();
        let original = fs::read(fixture.binary()).unwrap();
        let identity = crate::launcher_updates::test_support::identity(fixture.binary());
        let journal = fixture.interrupted(published, false);
        let result = recover(&fixture.0, &fixture.current());
        assert_eq!(
            result.state,
            InstallationState::RolledBack,
            "{:?}",
            result.warning
        );
        assert_eq!(fs::read(fixture.binary()).unwrap(), original);
        assert_eq!(
            crate::launcher_updates::test_support::identity(fixture.binary()),
            identity
        );
        let fs = Fs::open(&fixture.0).unwrap();
        assert!(fs.marker(&journal, "rolled-back").unwrap());
    }
}
#[cfg(target_os = "macos")]
#[test]
fn native_macos_published_executable_has_actual_owner_only_mode_and_retains_original() {
    let fixture = Fixture::new();
    let original = fs::read(fixture.binary()).unwrap();
    let (_, journal) = fixture.applied();
    let metadata = pcl_core::retained_fs::metadata(&File::open(fixture.binary()).unwrap()).unwrap();
    metadata.require_private().unwrap();
    match metadata.security {
        pcl_core::retained_fs::Security::Unix { mode, .. } => assert_eq!(mode & 0o7777, 0o700),
        _ => panic!("Mac security"),
    };
    assert_eq!(
        fs::read(fixture.0.join(".pcl-rust/bin").join(journal.swap_name())).unwrap(),
        original
    );
}
