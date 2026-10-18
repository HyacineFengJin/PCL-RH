use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/launch-readiness-2026-10-18/java-store-bindings")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn directory(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn capture_does_not_create_store_or_lock_and_open_uses_the_original_anchor() {
    let f = Fixture::new();
    let project = f.directory("project");
    let binding = StoreBinding::capture(&project).unwrap();
    assert!(!project.join(".pcl-rust").exists());
    assert_eq!(
        binding.anchor.identity().unwrap(),
        Dir::absolute(&project).unwrap().identity().unwrap()
    );
    let store = binding.open().unwrap();
    assert_eq!(store.path, project.join(".pcl-rust/java"));
    assert_eq!(
        store.dir.identity().unwrap(),
        Dir::absolute(&store.path).unwrap().identity().unwrap()
    );
    assert!(fs::read_dir(&store.path).unwrap().next().is_none());
    assert!(!store.path.join(".java-install.lock").exists());
}

#[test]
fn replacing_an_existing_store_before_open_does_not_adopt_or_write_the_replacement() {
    let f = Fixture::new();
    let project = f.directory("project");
    let path = f.directory("project/.pcl-rust/java");
    fs::write(path.join("original"), b"original store").unwrap();
    let binding = StoreBinding::capture(&project).unwrap();
    let original = project.join(".pcl-rust/original-java");
    fs::rename(&path, &original).unwrap();
    f.directory("project/.pcl-rust/java");
    fs::write(path.join("foreign"), b"replacement store").unwrap();
    assert!(binding.open().is_err());
    assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
    assert_eq!(
        fs::read(path.join("foreign")).unwrap(),
        b"replacement store"
    );
    assert_eq!(fs::read_dir(&original).unwrap().count(), 1);
    assert_eq!(
        fs::read(original.join("original")).unwrap(),
        b"original store"
    );
}

#[test]
fn replacing_a_missing_stores_anchor_refuses_before_any_mkdir() {
    for parent_exists in [false, true] {
        let f = Fixture::new();
        let project = f.directory("project");
        let anchor = if parent_exists {
            f.directory("project/.pcl-rust")
        } else {
            project.clone()
        };
        let binding = StoreBinding::capture(&project).unwrap();
        let original = f.0.join("original-anchor");
        fs::rename(&anchor, &original).unwrap();
        fs::create_dir(&anchor).unwrap();
        assert!(binding.open().is_err());
        assert!(fs::read_dir(&anchor).unwrap().next().is_none());
        assert!(fs::read_dir(&original).unwrap().next().is_none());
    }
}

#[test]
fn a_preceding_worker_may_create_the_missing_suffix_without_rebinding_the_anchor() {
    for parent_exists in [false, true] {
        let f = Fixture::new();
        let project = f.directory("project");
        if parent_exists {
            f.directory("project/.pcl-rust");
        }
        let first = StoreBinding::capture(&project).unwrap();
        let waiting = StoreBinding::capture(&project).unwrap();
        let store = first.open().unwrap();
        fs::write(store.path.join("preceding-worker"), b"published store").unwrap();
        let next = waiting.open().unwrap();
        assert_eq!(store.dir.identity().unwrap(), next.dir.identity().unwrap());
        assert_eq!(
            fs::read(next.path.join("preceding-worker")).unwrap(),
            b"published store"
        );
        assert_eq!(fs::read_dir(&next.path).unwrap().count(), 1);
    }
}

#[test]
fn the_admission_boundary_must_match_but_a_later_worker_creation_is_allowed() {
    let f = Fixture::new();
    let project = f.directory("project");
    let binding = StoreBinding::capture(&project).unwrap();
    binding.verify().unwrap();
    let store = binding.open().unwrap();
    assert!(binding.verify().is_err());
    assert_eq!(
        binding.open().unwrap().dir.identity().unwrap(),
        store.dir.identity().unwrap()
    );
    let existing = StoreBinding::capture(&project).unwrap();
    existing.verify().unwrap();
    assert_eq!(
        existing.open().unwrap().dir.identity().unwrap(),
        store.dir.identity().unwrap()
    );
}

#[test]
fn a_new_symlink_cannot_redirect_a_missing_suffix() {
    for parent_exists in [false, true] {
        let f = Fixture::new();
        let project = f.directory("project");
        let foreign = f.directory("foreign");
        if parent_exists {
            f.directory("project/.pcl-rust");
        }
        let binding = StoreBinding::capture(&project).unwrap();
        symlink(
            &foreign,
            if parent_exists {
                project.join(".pcl-rust/java")
            } else {
                project.join(".pcl-rust")
            },
        )
        .unwrap();
        assert!(binding.open().is_err());
        assert!(fs::read_dir(&foreign).unwrap().next().is_none());
    }
}

#[test]
fn existing_links_and_invalid_paths_are_rejected_without_side_effects() {
    let f = Fixture::new();
    let project = f.directory("project");
    let alias = f.0.join("alias");
    symlink(&project, &alias).unwrap();
    assert!(StoreBinding::capture(&alias).is_err());
    assert!(StoreBinding::capture(&project.join("../project")).is_err());
    assert!(StoreBinding::capture(Path::new("relative")).is_err());
    assert!(StoreBinding::capture(&project.join("x".repeat(4096))).is_err());
    assert!(!project.join(".pcl-rust").exists());
    fs::write(project.join(".pcl-rust"), b"foreign file").unwrap();
    assert!(StoreBinding::capture(&project).is_err());
    assert_eq!(
        fs::read(project.join(".pcl-rust")).unwrap(),
        b"foreign file"
    );
}
