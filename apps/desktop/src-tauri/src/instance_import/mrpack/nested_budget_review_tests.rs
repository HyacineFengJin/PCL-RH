//! Independent budget/lifetime regressions. Large reservations model capacity
//! without writing multi-GiB fixtures; actual small nested authorities exercise
//! the cache→claimed lease and CRC/cancellation error paths.
use super::*;
use serde_json::json;
use std::{fs, io::Cursor};
use zip::{write::SimpleFileOptions, ZipWriter};
struct Fixture {
    _directory: crate::integration_tests::Fixture,
    project: PathBuf,
    root: GameRoot,
    source: PathBuf,
    inner_size: u64,
}
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (path, bytes) in entries {
        zip.start_file(*path, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
impl Fixture {
    fn new() -> Self {
        let work = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/nested-pack-2026-10-06/review/budget-fixtures");
        fs::create_dir_all(&work).unwrap();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let project = work.join(format!(
            "budget-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let directory = crate::integration_tests::Fixture(project.clone());
        fs::create_dir(project.join("root")).unwrap();
        let metadata=serde_json::to_vec(&json!({"id":"1.21.1","mainClass":"net.minecraft.client.main.Main","javaVersion":{"majorVersion":21},"libraries":[],"arguments":{"game":[],"jvm":[]}})).unwrap();
        let inner = zip(&[
            ("versions/1.21.1/1.21.1.json", &metadata),
            ("versions/1.21.1/1.21.1.jar", b"private core"),
            ("config/local.cfg", b"private configuration"),
        ]);
        let source = project.join("selected.zip");
        fs::write(
            &source,
            zip(&[
                ("modpack.zip", &inner),
                ("launcher.exe", b"ignored launcher"),
            ]),
        )
        .unwrap();
        let root = GameRoot {
            id: "fixture-root".into(),
            name: "Fixture Root".into(),
            path: project.join("root").to_string_lossy().into_owned(),
            selected: None,
            overrides: Default::default(),
            java_overrides: Default::default(),
            isolation_overrides: Default::default(),
            window_title_overrides: Default::default(),
        };
        Self {
            _directory: directory,
            project,
            root,
            source,
            inner_size: inner.len() as u64,
        }
    }
    fn prepare(&self, cache: &ConfirmationCache, name: &str) -> PackPlan {
        cache
            .prepare_admitted(
                cache.preparation().unwrap(),
                &self.root,
                &self.project,
                &self.source,
                name,
                None,
                &AtomicBool::new(false),
            )
            .unwrap()
    }
    fn claim(&self, cache: &ConfirmationCache, name: &str) -> ConfirmedPack {
        let plan = self.prepare(cache, name);
        assert!(plan.installable);
        cache
            .claim(
                &self.root,
                &self.project,
                &self.source,
                name,
                &plan.revision,
            )
            .unwrap()
    }
}
fn usage(cache: &ConfirmationCache) -> (usize, usize, u64, usize) {
    let usage = cache.budget.lock().unwrap();
    (usage.fds, usage.bytes, usage.inner_bytes, usage.preparing)
}
#[test]
fn review_prepare_window_is_four_and_drop_returns_all_reserved_resources() {
    let cache = ConfirmationCache::default();
    let mut guards = Vec::new();
    for _ in 0..MAX_PREPARING {
        guards.push(cache.preparation().unwrap());
    }
    assert_eq!(usage(&cache), (4, 4 * MAX_ONE, 0, 4));
    assert!(cache.preparation().is_err());
    assert_eq!(usage(&cache), (4, 4 * MAX_ONE, 0, 4));
    drop(guards.pop());
    assert_eq!(usage(&cache), (3, 3 * MAX_ONE, 0, 3));
    let replacement = cache.preparation().unwrap();
    assert_eq!(usage(&cache), (4, 4 * MAX_ONE, 0, 4));
    drop(replacement);
    drop(guards);
    assert_eq!(usage(&cache), (0, 0, 0, 0));
}
#[test]
fn review_claimed_inner_disk_and_fds_survive_ttl_and_unclaimed_eviction() {
    let fixture = Fixture::new();
    let cache = ConfirmationCache::default();
    let claimed = fixture.claim(&cache, "claimed");
    assert_eq!(usage(&cache).0, 2);
    assert_eq!(usage(&cache).2, fixture.inner_size);
    assert_eq!(usage(&cache).3, 0);
    let charged = claimed._lease.bytes;
    for n in 0..60 {
        fixture.prepare(&cache, &format!("unclaimed-{n}"));
    }
    assert!(
        cache.entries.lock().unwrap().len() < 60,
        "FD cap must evict some unclaimed authorities"
    );
    {
        let mut entries = cache.entries.lock().unwrap();
        for record in entries.iter_mut() {
            record.expires = Instant::now() - TTL;
        }
    }
    let preparing = cache.preparation().unwrap();
    assert!(cache.entries.lock().unwrap().is_empty());
    assert_eq!(usage(&cache), (3, charged + MAX_ONE, fixture.inner_size, 1));
    claimed
        .checked
        .recheck_source(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(claimed.checked.retained_source_fds(), 2);
    assert_eq!(
        claimed
            .checked
            .content_file()
            .unwrap()
            .metadata()
            .unwrap()
            .len(),
        fixture.inner_size
    );
    drop(preparing);
    assert_eq!(usage(&cache), (2, charged, fixture.inner_size, 0));
    drop(claimed);
    assert_eq!(usage(&cache), (0, 0, 0, 0));
}
#[test]
fn review_full_claimed_disk_refuses_before_decoding_and_returns_prepare_reservation() {
    let fixture = Fixture::new();
    let cache = ConfirmationCache::default();
    let mut claimed = fixture.claim(&cache, "claimed");
    let (fds, bytes) = (claimed._lease.fds, claimed._lease.bytes);
    cache
        .resize_lease(&mut claimed._lease, fds, bytes, MAX_INNER_BYTES)
        .unwrap();
    // A corrupt inner body would produce a ZIP parser error if decoded before
    // reserving disk. Capacity must win, and no budget may be revoked from claim.
    let rejected = fixture.project.join("rejected.zip");
    fs::write(&rejected, zip(&[("modpack.zip", b"not a ZIP body")])).unwrap();
    let error = cache
        .prepare_admitted(
            cache.preparation().unwrap(),
            &fixture.root,
            &fixture.project,
            &rejected,
            "rejected",
            None,
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
    assert_eq!(error, CAPACITY);
    assert_eq!(usage(&cache), (fds, bytes, MAX_INNER_BYTES, 0));
    claimed
        .checked
        .recheck_source(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        fs::read_dir(fixture.project.join(".pcl-linux/pack-inputs"))
            .unwrap()
            .count(),
        0
    );
    drop(claimed);
    assert_eq!(usage(&cache), (0, 0, 0, 0));
}
#[test]
fn review_nested_cancel_crc_and_wrong_cache_errors_return_disk_fd_and_window_charges() {
    let fixture = Fixture::new();
    let cache = ConfirmationCache::default();
    assert!(cache
        .prepare_admitted(
            cache.preparation().unwrap(),
            &fixture.root,
            &fixture.project,
            &fixture.source,
            "cancelled",
            None,
            &AtomicBool::new(true)
        )
        .is_err());
    assert_eq!(usage(&cache), (0, 0, 0, 0));
    let rejected = fixture.project.join("corrupt-inner.zip");
    fs::write(&rejected, zip(&[("modpack.zip", b"invalid inner zip")])).unwrap();
    assert!(cache
        .prepare_admitted(
            cache.preparation().unwrap(),
            &fixture.root,
            &fixture.project,
            &rejected,
            "invalid",
            None,
            &AtomicBool::new(false)
        )
        .is_err());
    assert_eq!(usage(&cache), (0, 0, 0, 0));
    assert_eq!(
        fs::read_dir(fixture.project.join(".pcl-linux/pack-inputs"))
            .unwrap()
            .count(),
        0
    );
    let other = ConfirmationCache::default();
    assert!(cache
        .prepare_admitted(
            other.preparation().unwrap(),
            &fixture.root,
            &fixture.project,
            &fixture.source,
            "wrong-cache",
            None,
            &AtomicBool::new(false)
        )
        .is_err());
    assert_eq!(usage(&cache), (0, 0, 0, 0));
    assert_eq!(usage(&other), (0, 0, 0, 0));
    assert_eq!(
        fs::read_dir(Path::new(&fixture.root.path)).unwrap().count(),
        0
    );
}
