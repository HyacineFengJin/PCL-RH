use super::*;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use zip::{write::SimpleFileOptions, ZipWriter};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    source: PathBuf,
    target: PathBuf,
    archive: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/snapshot-plugin-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let f = Self {
            source: base.join("source"),
            target: base.join("target"),
            archive: base.join("backup.zip"),
            base,
        };
        fs::create_dir_all(&f.target).unwrap();
        f.write("versions/Source/Source.json", json!({"id":"Source","clientVersion":"1.20.1","mainClass":"net.minecraft.client.main.Main","libraries":[]}).to_string());
        for (path, bytes) in [
            ("Source.jar", "core"),
            ("mods/example.jar", "mod"),
            ("config/example.json", "config"),
            ("saves/World/level.dat", "world"),
            ("options.txt", "settings"),
            ("servers.dat", "servers"),
            ("screenshots/one.png", "image"),
            ("logs/latest.log", "log"),
            ("unknown/retain.txt", "unknown"),
        ] {
            f.write(&format!("versions/Source/{path}"), bytes);
        }
        f
    }
    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let path = self.source.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn export(&self, mode: SnapshotMode) -> instance_export::ExportPlan {
        Snapshots
            .export(
                &self.source,
                "Source",
                mode,
                "fixture-1",
                IsolationPolicy::Automatic,
                "settings-1",
            )
            .unwrap()
    }
    fn backup(&self, mode: SnapshotMode) {
        instance_export::execute(
            self.export(mode),
            &self.archive,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}

#[test]
fn native_presets_own_worlds_and_personal_content_policy() {
    let f = Fixture::new();
    let backup = f.export(SnapshotMode::Backup);
    let share = f.export(SnapshotMode::Share);
    for checks in [&backup.request.checks, &share.request.checks] {
        assert!(
            checks.game
                && checks.mods
                && checks.pack_data
                && checks.mod_settings
                && checks.resourcepacks
                && checks.shaderpacks
        );
        assert!(
            !checks.screenshots
                && !checks.other
                && !checks.launcher
                && !checks.bundle_assets
                && !checks.modrinth
        );
    }
    assert!(
        backup.request.checks.saves
            && backup.request.checks.server
            && backup.request.checks.game_personal
    );
    assert!(
        !share.request.checks.saves
            && !share.request.checks.server
            && !share.request.checks.game_personal
    );
    assert_ne!(backup.revision, share.revision);
    assert!(Snapshots
        .export(
            &f.source,
            "Source",
            SnapshotMode::Backup,
            "",
            IsolationPolicy::Automatic,
            "settings-1"
        )
        .is_err());
}

#[test]
fn backup_restores_saved_content_under_new_name_and_preserves_current_instance() {
    let f = Fixture::new();
    f.backup(SnapshotMode::Backup);
    f.write("versions/Source/saves/World/level.dat", "current world");
    let plan = Snapshots
        .restore(&f.source, &f.archive, "Restored")
        .unwrap();
    let dto = serde_json::to_value(&plan).unwrap();
    assert!(dto.get("source").is_none() && dto.get("root").is_none());
    instance_import::execute(plan, &AtomicBool::new(false), |_| {}).unwrap();
    let copy = f.source.join("versions/Restored");
    assert_eq!(
        fs::read(copy.join("saves/World/level.dat")).unwrap(),
        b"world"
    );
    assert_eq!(
        fs::read(f.source.join("versions/Source/saves/World/level.dat")).unwrap(),
        b"current world"
    );
    for path in [
        "mods/example.jar",
        "config/example.json",
        "options.txt",
        "servers.dat",
    ] {
        assert!(copy.join(path).is_file(), "{path}");
    }
    for path in ["screenshots", "logs", "unknown"] {
        assert!(!copy.join(path).exists(), "{path}");
    }
    fs::write(copy.join("mods/example.jar"), b"changed copy").unwrap();
    assert_eq!(
        fs::read(f.source.join("versions/Source/mods/example.jar")).unwrap(),
        b"mod"
    );
    assert!(Snapshots.restore(&f.source, &f.archive, "Source").is_err());
    assert!(Snapshots
        .restore(&f.source, &f.archive, "Restored")
        .is_err());
}

#[test]
fn sharing_archive_restores_without_worlds_or_personal_game_options() {
    let f = Fixture::new();
    f.backup(SnapshotMode::Share);
    let plan = Snapshots.restore(&f.target, &f.archive, "Shared").unwrap();
    instance_import::execute(plan, &AtomicBool::new(false), |_| {}).unwrap();
    let copy = f.target.join("versions/Shared");
    assert!(copy.join("mods/example.jar").is_file());
    assert!(
        !copy.join("saves").exists()
            && !copy.join("options.txt").exists()
            && !copy.join("servers.dat").exists()
    );
    assert!(copy.join("config").is_dir());
}

#[test]
fn ordinary_network_packs_do_not_enter_offline_backup_restoration() {
    let f = Fixture::new();
    let mut zip = ZipWriter::new(fs::File::create(&f.archive).unwrap());
    zip.start_file("modrinth.index.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"{\"formatVersion\":1,\"game\":\"minecraft\",\"files\":[]}")
        .unwrap();
    zip.finish().unwrap();
    assert!(Snapshots
        .restore(&f.target, &f.archive, "Restored")
        .is_err());
    assert!(!f.target.join("versions/Restored").exists());
}

#[test]
fn changed_backup_and_cancellation_keep_originals_and_remove_partial_restore() {
    let f = Fixture::new();
    f.backup(SnapshotMode::Backup);
    let original = fs::read(&f.archive).unwrap();
    let plan = Snapshots
        .restore(&f.target, &f.archive, "Restored")
        .unwrap();
    fs::write(&f.archive, b"replaced backup").unwrap();
    assert!(instance_import::execute(plan, &AtomicBool::new(false), |_| {}).is_err());
    assert!(!f.target.join("versions/Restored").exists());
    fs::write(&f.archive, &original).unwrap();
    let plan = Snapshots
        .restore(&f.target, &f.archive, "Restored")
        .unwrap();
    let cancel = AtomicBool::new(false);
    assert!(instance_import::execute(plan, &cancel, |p| {
        if p.stage == "import-extract" {
            cancel.store(true, Ordering::SeqCst);
        }
    })
    .is_err());
    instance_import::ensure_ready(&f.target).unwrap();
    assert!(!f.target.join("versions/Restored").exists());
    assert_eq!(fs::read(&f.archive).unwrap(), original);
}

#[test]
fn retiring_snapshot_provider_blocks_new_calls_and_keeps_accepted_lease() {
    let f = Fixture::new();
    f.backup(SnapshotMode::Backup);
    let host = crate::plugins::host();
    host.configure(&[], true, false).unwrap();
    let handle = host.resolve(SNAPSHOTS).unwrap().unwrap();
    let lease = handle.begin().unwrap();
    host.configure(&[ID.to_string()], true, false).unwrap();
    assert!(host.resolve(SNAPSHOTS).unwrap().is_none());
    assert!(handle.begin().is_err());
    let plan = lease.0.restore(&f.target, &f.archive, "Restored").unwrap();
    instance_import::execute(plan, &AtomicBool::new(false), |_| {}).unwrap();
    drop(lease);
    assert!(f.target.join("versions/Restored/Restored.jar").is_file());
}
