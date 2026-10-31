use super::*;

#[path = "../../file_ops/junction_fixture.rs"]
#[allow(dead_code)]
mod junction_fixture;

#[test]
fn reparse_mutation_after_native_name_capture_rejects_before_anchor_or_spawn() {
    let temporary = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(temporary.path()).unwrap();
    let root = base.join("cwd");
    let outside = base.join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("canary"), b"outside unchanged").unwrap();
    let input = retained_fs::Directory::open_absolute(&root).unwrap();
    let expected = input.identity().unwrap();
    let result = CwdLease::prepare(input.file(), || {
        // The empty directory can be tagged through a concurrently opened
        // handle before its graph/anchor lease exists. An ID-only check would
        // still see the same directory object: type/noReparse must reject it.
        junction_fixture::set_existing(&outside, &root).unwrap();
    });
    assert!(result.is_err(), "mutated cwd must not reach CreateProcessW");
    let changed = retained_fs::metadata(input.file()).unwrap();
    assert_eq!(changed.identity, expected);
    assert_eq!(changed.kind, Kind::Link);
    assert!(changed.reparse_tag.is_some());
    assert_eq!(
        std::fs::read(outside.join("canary")).unwrap(),
        b"outside unchanged"
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    junction_fixture::clear_existing(&root).unwrap();
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
}
