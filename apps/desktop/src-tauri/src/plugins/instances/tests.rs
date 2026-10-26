use super::*;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/instance-copy-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let f = Self(root.canonicalize().unwrap());
        f.write("versions/Base/Base.json", json!({"id":"Base","clientVersion":"1.20.1","mainClass":"net.minecraft.client.main.Main","libraries":[],"arguments":{"game":["--base"]}}).to_string());
        f.write("versions/Base/Base.jar", "game core");
        f.write(
            "versions/Source/Source.json",
            json!({"id":"Source","inheritsFrom":"Base","arguments":{"game":["--child"]}})
                .to_string(),
        );
        f.write("versions/Source/mods/example.jar", "mod");
        f.write("versions/Source/config/example.json", "settings");
        f.write("versions/Source/saves/World/level.dat", "world");
        f.write("versions/Source/screenshots/picture.png", "picture");
        f.write("versions/Source/logs/latest.log", "private log");
        f
    }
    fn write(&self, path: &str, value: impl AsRef<[u8]>) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }
    fn prepare(&self, saves: bool, screenshots: bool) -> Box<dyn PreparedCopy> {
        Copier
            .prepare(
                &self.0,
                "Source",
                CopyRequest {
                    name: "Copy".into(),
                    saves,
                    screenshots,
                },
                IsolationPolicy::Automatic,
                "settings-1",
            )
            .unwrap()
    }
    fn execute(&self, saves: bool, screenshots: bool) -> Value {
        self.prepare(saves, screenshots)
            .execute(&AtomicBool::new(false), &|_| {}, &|| Ok(()))
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn copy_folds_inheritance_and_has_independent_writable_files() {
    let f = Fixture::new();
    let result = f.execute(true, true);
    assert_eq!(result["id"], "Copy");
    let copy = f.0.join("versions/Copy");
    let metadata: Value =
        serde_json::from_slice(&fs::read(copy.join("Copy.json")).unwrap()).unwrap();
    assert_eq!(metadata["id"], "Copy");
    assert_eq!(metadata["jar"], "Copy");
    assert_eq!(metadata["clientVersion"], "1.20.1");
    assert!(metadata.get("inheritsFrom").is_none());
    assert_eq!(metadata["arguments"]["game"], json!(["--base", "--child"]));
    assert_eq!(fs::read(copy.join("Copy.jar")).unwrap(), b"game core");
    assert_eq!(
        fs::read(copy.join("saves/World/level.dat")).unwrap(),
        b"world"
    );
    assert!(copy.join("screenshots/picture.png").is_file());
    assert!(!copy.join("logs").exists());
    let source = f.0.join("versions/Source/mods/example.jar");
    let target = copy.join("mods/example.jar");
    assert_ne!(
        fs::metadata(&source).unwrap().ino(),
        fs::metadata(&target).unwrap().ino()
    );
    fs::write(target, "changed copy").unwrap();
    assert_eq!(fs::read(source).unwrap(), b"mod");
    assert!(copy.join("config").is_dir());
    crate::instance_import::ensure_ready(&f.0).unwrap();
}
#[test]
fn worlds_and_screenshots_are_opt_in_and_preview_counts_match() {
    let f = Fixture::new();
    let plan = f.prepare(false, false);
    let count = plan.preview().file_count;
    let result = plan
        .execute(&AtomicBool::new(false), &|_| {}, &|| Ok(()))
        .unwrap();
    assert_eq!(result["files"], count);
    assert!(!f.0.join("versions/Copy/saves").exists());
    assert!(!f.0.join("versions/Copy/screenshots").exists());
    assert!(f.prepare_existing().is_err());
}
impl Fixture {
    fn prepare_existing(&self) -> Result<Box<dyn PreparedCopy>, String> {
        Copier.prepare(
            &self.0,
            "Source",
            CopyRequest {
                name: "Copy".into(),
                saves: false,
                screenshots: false,
            },
            IsolationPolicy::Automatic,
            "settings-1",
        )
    }
}
#[test]
fn changed_sources_and_links_never_publish_a_copy() {
    let f = Fixture::new();
    let plan = f.prepare(false, false);
    f.write("versions/Source/mods/example.jar", "changed source");
    assert!(plan
        .execute(&AtomicBool::new(false), &|_| {}, &|| Ok(()))
        .is_err());
    assert!(!f.0.join("versions/Copy").exists());
    symlink("example.jar", f.0.join("versions/Source/mods/link.jar")).unwrap();
    assert!(f.prepare_existing().is_err());
}
#[test]
fn cancellation_and_reference_conflicts_roll_back() {
    for conflict in [false, true] {
        let f = Fixture::new();
        let plan = f.prepare(true, true);
        let cancel = AtomicBool::new(false);
        let result = plan.execute(
            &cancel,
            &|p| {
                if !conflict && p.stage == "import-extract" {
                    cancel.store(true, Ordering::SeqCst);
                }
            },
            &|| Err("reference changed".into()),
        );
        assert!(result.is_err());
        assert!(!f.0.join("versions/Copy").exists());
        assert_eq!(
            fs::read(f.0.join("versions/Source/saves/World/level.dat")).unwrap(),
            b"world"
        );
        crate::instance_import::ensure_ready(&f.0).unwrap();
    }
}
#[test]
fn interrupted_copy_uses_existing_import_recovery() {
    let f = Fixture::new();
    let plan = f.prepare(true, true);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = plan.execute(
            &AtomicBool::new(false),
            &|p| {
                if p.stage == "import-extract" {
                    panic!("simulated interruption");
                }
            },
            &|| Ok(()),
        );
    }));
    assert!(result.is_err());
    assert!(crate::instance_import::ensure_ready(&f.0).is_err());
    crate::instance_import::recover_pending(&f.0).unwrap();
    crate::instance_import::ensure_ready(&f.0).unwrap();
    assert!(!f.0.join("versions/Copy").exists());
    f.execute(true, false);
}
#[test]
fn retired_plugin_rejects_new_calls_but_keeps_accepted_copy_alive() {
    let f = Fixture::new();
    let host = crate::plugins::host();
    host.configure(&[], true, false).unwrap();
    let service = host
        .resolve(INSTANCE_COPY)
        .unwrap()
        .unwrap()
        .begin()
        .unwrap();
    let plan = service
        .0
        .prepare(
            &f.0,
            "Source",
            CopyRequest {
                name: "Copy".into(),
                saves: false,
                screenshots: false,
            },
            IsolationPolicy::Automatic,
            "settings-1",
        )
        .unwrap();
    host.configure(&[ID.to_string()], true, false).unwrap();
    assert!(host.resolve(INSTANCE_COPY).unwrap().is_none());
    plan.execute(&AtomicBool::new(false), &|_| {}, &|| Ok(()))
        .unwrap();
    drop(service);
    assert!(f.0.join("versions/Copy/Copy.json").is_file());
}

#[test]
fn explicitly_shared_source_content_is_mapped_into_the_new_instance() {
    let f = Fixture::new();
    f.write("mods/shared.jar", "shared mod");
    f.write("saves/Shared/level.dat", "shared world");
    let plan = Copier
        .prepare(
            &f.0,
            "Source",
            CopyRequest {
                name: "Copy".into(),
                saves: true,
                screenshots: false,
            },
            IsolationPolicy::Shared,
            "settings-1",
        )
        .unwrap();
    plan.execute(&AtomicBool::new(false), &|_| {}, &|| Ok(()))
        .unwrap();
    assert_eq!(
        fs::read(f.0.join("versions/Copy/mods/shared.jar")).unwrap(),
        b"shared mod"
    );
    assert!(!f.0.join("versions/Copy/mods/example.jar").exists());
    assert!(f.0.join("versions/Copy/saves/Shared/level.dat").is_file());
    assert!(f.0.join("versions/Copy/config").is_dir());
}

#[test]
fn committed_copy_is_retained_when_cleanup_is_interrupted() {
    let f = Fixture::new();
    let plan = f.prepare(false, false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = plan.execute(
            &AtomicBool::new(false),
            &|p| {
                if p.stage == "import-cleanup" {
                    panic!("simulated interruption after commit");
                }
            },
            &|| Ok(()),
        );
    }));
    assert!(result.is_err());
    assert!(crate::instance_import::ensure_ready(&f.0).is_err());
    crate::instance_import::recover_pending(&f.0).unwrap();
    crate::instance_import::ensure_ready(&f.0).unwrap();
    assert_eq!(
        fs::read(f.0.join("versions/Copy/Copy.jar")).unwrap(),
        b"game core"
    );
    assert!(f.0.join("versions/Source/Source.json").is_file());
}
