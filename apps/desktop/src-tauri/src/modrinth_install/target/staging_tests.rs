//! Force the shared-stage creation race rather than depending on scheduling.
use super::*;
use std::{fs, io::Write, sync::Arc, sync::Barrier, thread};
struct Fixture(std::path::PathBuf, tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        Self(temporary.path().canonicalize().unwrap(), temporary)
    }
}

#[test]
fn shared_anonymous_stage_accepts_two_creators_after_both_observe_missing() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(2));
    let jobs = (0..2)
        .map(|number| {
            let directory = Dir::open(&fixture.0).unwrap();
            let barrier = barrier.clone();
            thread::spawn(move || {
                assert!(directory.optional("shared-stage").unwrap().is_none());
                barrier.wait();
                // Both enter the production creation branch using their
                // already-held parent FD. One mkdir must encounter EEXIST.
                let stage = directory.create_shared("shared-stage").unwrap();
                let mut temporary = stage.anonymous().unwrap();
                temporary.write_all(&[number]).unwrap();
                assert_eq!(temporary.metadata().unwrap().nlink(), 0);
                assert!(stage.names().unwrap().is_empty());
                stage.key().unwrap()
            })
        })
        .collect::<Vec<_>>();
    let keys = jobs
        .into_iter()
        .map(|job| job.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(keys[0], keys[1]);
    assert!(fs::read_dir(fixture.0.join("shared-stage"))
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn raced_shared_stage_refuses_links_and_regular_files() {
    let fixture = Fixture::new();
    let parent = Dir::open(&fixture.0).unwrap();
    fs::create_dir(fixture.0.join("other")).unwrap();
    std::os::unix::fs::symlink("other", fixture.0.join("symlink-stage")).unwrap();
    fs::write(fixture.0.join("file-stage"), b"retained").unwrap();
    assert!(parent.create_shared("symlink-stage").is_err());
    assert!(parent.create_shared("file-stage").is_err());
    assert_eq!(fs::read(fixture.0.join("file-stage")).unwrap(), b"retained");
    assert!(fs::read_dir(fixture.0.join("other"))
        .unwrap()
        .next()
        .is_none());
}
