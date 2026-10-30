//! Only synthetic project-local files are read; no installer or HTTP client is
//! constructed by the local integrity checker.
use super::*;
use serde_json::json;
use std::{os::unix::fs::symlink, sync::atomic::AtomicUsize};

fn fixture() -> tempfile::TempDir {
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../work/launch-readiness-2026-10-18/integrity-tests");
    fs::create_dir_all(&work).unwrap();
    tempfile::tempdir_in(work.canonicalize().unwrap()).unwrap()
}

fn put(root: &Path, path: &str, bytes: &[u8]) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, bytes).unwrap();
}

fn metadata(root: &Path, id: &str, value: Value) {
    put(
        root,
        &format!("versions/{id}/{id}.json"),
        &serde_json::to_vec(&value).unwrap(),
    );
}

fn descriptor(bytes: &[u8]) -> Value {
    json!({"url":"https://libraries.minecraft.net/fixture", "sha1":format!("{:x}", Sha1::digest(bytes)), "size":bytes.len()})
}

fn library(path: &str, bytes: &[u8]) -> Value {
    let mut download = descriptor(bytes);
    download["path"] = path.into();
    json!({"name":format!("org.example:{path}:1"), "downloads":{"artifact":download}})
}

fn capture(root: &Path) -> IntegrityCheck {
    IntegrityCheck::capture_bound(fs::File::open(root).unwrap(), root, "fixture").unwrap()
}

fn verify(request: &IntegrityCheck) -> IntegrityReport {
    request
        .verify(&AtomicBool::new(false), &|| Ok(()), &|| Ok(()), &|p| {
            assert_eq!(p.network_bytes, 0)
        })
        .unwrap()
}

#[test]
fn inherited_jar_libraries_logging_index_and_objects_are_verified_locally() {
    let root = fixture();
    let object = b"object bytes";
    let hash = format!("{:x}", Sha1::digest(object));
    let index = serde_json::to_vec(
        &json!({"objects":{"../../logical-name":{"hash":hash,"size":object.len()}}}),
    )
    .unwrap();
    let mut index_descriptor = descriptor(&index);
    index_descriptor["id"] = "fixture-index".into();
    let mut logging = descriptor(b"logging");
    logging["id"] = "fixture.xml".into();
    metadata(
        root.path(),
        "parent",
        json!({"id":"parent","downloads":{"client":descriptor(b"parent jar")}}),
    );
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","inheritsFrom":"parent","downloads":{"client":descriptor(b"wrong child jar")},"libraries":[library("fixture.jar",b"library")],"assetIndex":index_descriptor,"logging":{"client":{"file":logging}}}),
    );
    put(root.path(), "versions/parent/parent.jar", b"parent jar");
    put(root.path(), "libraries/fixture.jar", b"library");
    put(root.path(), "assets/log_configs/fixture.xml", b"logging");
    put(root.path(), "assets/indexes/fixture-index.json", &index);
    put(
        root.path(),
        &format!("assets/objects/{}/{hash}", &hash[..2]),
        object,
    );
    let request = capture(root.path());
    assert_eq!(request.id(), "fixture");
    let report = verify(&request);
    assert_eq!(
        (
            report.files_verified,
            report.files_missing,
            report.files_corrupt,
            report.files_unverifiable,
            report.problems_total
        ),
        (5, 0, 0, 0, 0)
    );
    assert!(report.assets_enumerated);
    assert!(!root.path().join("versions/fixture/fixture.jar").exists());
    assert!(!root.path().join("logical-name").exists());
}

#[test]
fn size_sha1_missing_and_unverifiable_files_are_distinguished_without_writes() {
    let root = fixture();
    let mut unknown = library("unknown.jar", b"user bytes");
    unknown["downloads"]["artifact"]
        .as_object_mut()
        .unwrap()
        .remove("sha1");
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":descriptor(b"client")}, "libraries":[library("same-size.jar",b"good"),library("wrong-size.jar",b"expected"),library("missing.jar",b"absent"),unknown]}),
    );
    put(root.path(), "versions/fixture/fixture.jar", b"client");
    put(root.path(), "libraries/same-size.jar", b"evil");
    put(root.path(), "libraries/wrong-size.jar", b"x");
    put(root.path(), "libraries/unknown.jar", b"user bytes");
    let report = verify(&capture(root.path()));
    assert_eq!(
        (
            report.files_verified,
            report.files_missing,
            report.files_corrupt,
            report.files_unverifiable,
            report.problems_total
        ),
        (1, 1, 2, 1, 4)
    );
    for kind in [
        IntegrityProblemKind::Missing,
        IntegrityProblemKind::SizeMismatch,
        IntegrityProblemKind::Sha1Mismatch,
        IntegrityProblemKind::Unverifiable,
    ] {
        assert!(report.problems.iter().any(|p| p.kind == kind));
    }
    assert_eq!(
        fs::read(root.path().join("libraries/same-size.jar")).unwrap(),
        b"evil"
    );
    assert_eq!(
        fs::read(root.path().join("libraries/wrong-size.jar")).unwrap(),
        b"x"
    );
    assert_eq!(
        fs::read(root.path().join("libraries/unknown.jar")).unwrap(),
        b"user bytes"
    );
    assert!(!root.path().join("libraries/missing.jar").exists());
}

#[test]
fn corrupt_or_invalid_index_never_authorizes_objects_and_missing_only_stays_strict() {
    for bytes in [
        b"not JSON".as_slice(),
        br#"{"objects":{"injected":{"hash":"1234567890123456789012345678901234567890","size":1}}}"#,
    ] {
        let root = fixture();
        let mut index = descriptor(b"trusted original index");
        index["id"] = "fixture-index".into();
        metadata(
            root.path(),
            "fixture",
            json!({"id":"fixture","downloads":{"client":descriptor(b"client")},"assetIndex":index}),
        );
        put(root.path(), "versions/fixture/fixture.jar", b"client");
        put(root.path(), "assets/indexes/fixture-index.json", bytes);
        let report = verify(&capture(root.path()));
        assert_eq!(
            (
                report.files_verified,
                report.files_corrupt,
                report.files_missing,
                report.problems_total
            ),
            (1, 1, 0, 2)
        );
        assert!(!report.assets_enumerated);
        assert!(!root.path().join("assets/objects").exists());
        assert_eq!(
            fs::read(root.path().join("assets/indexes/fixture-index.json")).unwrap(),
            bytes
        );
        if bytes == b"not JSON" {
            assert!(MissingRepair::capture_bound(
                fs::File::open(root.path()).unwrap(),
                root.path(),
                "fixture"
            )
            .is_err());
        }
    }
}

#[test]
fn missing_index_and_valid_digest_with_invalid_json_report_incomplete_coverage() {
    for existing in [false, true] {
        let root = fixture();
        let mut index = descriptor(b"invalid JSON");
        index["id"] = "fixture-index".into();
        metadata(
            root.path(),
            "fixture",
            json!({"id":"fixture","downloads":{"client":descriptor(b"client")},"assetIndex":index}),
        );
        put(root.path(), "versions/fixture/fixture.jar", b"client");
        if existing {
            put(
                root.path(),
                "assets/indexes/fixture-index.json",
                b"invalid JSON",
            );
        }
        let report = verify(&capture(root.path()));
        assert!(!report.assets_enumerated);
        assert_eq!(
            (
                report.files_verified,
                report.files_missing,
                report.files_corrupt
            ),
            (
                if existing { 2 } else { 1 },
                if existing { 0 } else { 1 },
                0
            )
        );
        assert_eq!(report.problems_total, if existing { 1 } else { 2 });
        assert!(!root.path().join("assets/objects").exists());
    }
}

#[test]
fn cancel_during_hashing_preserves_existing_bytes() {
    let root = fixture();
    let bytes = vec![b'a'; 1024 * 1024];
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":descriptor(&bytes)}}),
    );
    put(root.path(), "versions/fixture/fixture.jar", &bytes);
    let request = capture(root.path());
    let cancel = AtomicBool::new(false);
    let result = request.verify(&cancel, &|| Ok(()), &|| Ok(()), &|p| {
        if p.bytes_done > 0 {
            cancel.store(true, Ordering::Release);
        }
    });
    assert!(result.unwrap_err().contains("取消"));
    assert_eq!(
        fs::read(root.path().join("versions/fixture/fixture.jar")).unwrap(),
        bytes
    );
    assert_eq!(
        fs::read_dir(root.path().join("versions/fixture"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn metadata_root_parent_and_leaf_replacement_invalidate_a_running_check() {
    for change in ["metadata", "root", "parent", "leaf"] {
        let container = fixture();
        let root = container.path().join("root");
        fs::create_dir(&root).unwrap();
        let bytes = vec![b'a'; 1024 * 1024];
        metadata(
            &root,
            "fixture",
            json!({"id":"fixture","libraries":[library("fixture.jar",&bytes)]}),
        );
        put(&root, "versions/fixture/fixture.jar", b"custom jar");
        put(&root, "libraries/fixture.jar", &bytes);
        let request = capture(&root);
        let changed = AtomicBool::new(false);
        let result = request.verify(&AtomicBool::new(false), &|| Ok(()), &|| Ok(()), &|p| {
            if p.bytes_done > 0 && !changed.swap(true, Ordering::Relaxed) {
                match change {
                    "metadata" => {
                        fs::write(root.join("versions/fixture/fixture.json"), b"{}").unwrap()
                    }
                    "root" => {
                        fs::rename(&root, container.path().join("saved")).unwrap();
                        fs::create_dir(&root).unwrap();
                    }
                    "parent" => {
                        fs::rename(root.join("libraries"), container.path().join("saved")).unwrap();
                        fs::create_dir(root.join("libraries")).unwrap();
                    }
                    _ => {
                        fs::rename(
                            root.join("libraries/fixture.jar"),
                            container.path().join("saved.jar"),
                        )
                        .unwrap();
                        fs::write(root.join("libraries/fixture.jar"), &bytes).unwrap();
                    }
                }
            }
        });
        assert!(result.is_err(), "{change}");
    }
}

#[test]
fn an_earlier_verified_file_changed_after_its_hash_is_detected_at_completion() {
    let root = fixture();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":descriptor(b"client")},"libraries":[library("earlier.jar",b"first")]}),
    );
    put(root.path(), "versions/fixture/fixture.jar", b"client");
    put(root.path(), "libraries/earlier.jar", b"first");
    let request = capture(root.path());
    let changed = AtomicBool::new(false);
    let result = request.verify(&AtomicBool::new(false), &|| Ok(()), &|| Ok(()), &|p| {
        if p.completed == 1 && !changed.swap(true, Ordering::Relaxed) {
            fs::write(root.path().join("libraries/earlier.jar"), b"other").unwrap();
        }
    });
    assert!(result.unwrap_err().contains("变化"));
}

#[test]
fn untrusted_files_and_size_mismatches_are_not_hashed_and_findings_are_bounded() {
    let root = fixture();
    let libraries: Vec<_> = (0..40)
        .map(|n| library(&format!("missing-{n}.jar"), b"missing"))
        .collect();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":descriptor(b"expected")},"libraries":libraries}),
    );
    put(root.path(), "versions/fixture/fixture.jar", b"x");
    let max_bytes = AtomicU64::new(0);
    let report = capture(root.path())
        .verify(&AtomicBool::new(false), &|| Ok(()), &|| Ok(()), &|p| {
            max_bytes.fetch_max(p.bytes_done, Ordering::Relaxed);
        })
        .unwrap();
    assert_eq!(
        (
            report.files_missing,
            report.files_corrupt,
            report.problems_total,
            report.problems.len()
        ),
        (40, 1, 41, 32)
    );
    assert_eq!(max_bytes.load(Ordering::Relaxed), 0);
    assert!(!root.path().join("libraries").exists());
}

#[test]
fn unsafe_destinations_and_changed_guards_are_rejected_without_following_links() {
    let root = fixture();
    let outside = fixture();
    metadata(
        root.path(),
        "fixture",
        json!({"id":"fixture","downloads":{"client":descriptor(b"client")}}),
    );
    put(outside.path(), "outside.jar", b"client");
    symlink(
        outside.path().join("outside.jar"),
        root.path().join("versions/fixture/fixture.jar"),
    )
    .unwrap();
    assert!(IntegrityCheck::capture_bound(
        fs::File::open(root.path()).unwrap(),
        root.path(),
        "fixture"
    )
    .is_err());
    fs::remove_file(root.path().join("versions/fixture/fixture.jar")).unwrap();
    put(root.path(), "versions/fixture/fixture.jar", b"client");
    let request = capture(root.path());
    let calls = AtomicUsize::new(0);
    assert!(request
        .verify(
            &AtomicBool::new(false),
            &|| {
                calls.fetch_add(1, Ordering::Relaxed);
                Err("generation changed".into())
            },
            &|| Ok(()),
            &|_| {}
        )
        .unwrap_err()
        .contains("generation changed"));
    assert!(calls.load(Ordering::Relaxed) > 0);
    assert!(request
        .verify(
            &AtomicBool::new(false),
            &|| Ok(()),
            &|| Err("binding changed".into()),
            &|_| {}
        )
        .unwrap_err()
        .contains("binding changed"));
    assert_eq!(
        fs::read(outside.path().join("outside.jar")).unwrap(),
        b"client"
    );
}
