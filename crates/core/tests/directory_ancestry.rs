//! Physical parent observations, distinct from an admitted write capability.
use pcl_core::retained_fs::{Directory, Leaf, Traversal};

fn child(parent: &Directory, name: &str) -> Directory {
    parent
        .create_private(&Leaf::new(name).unwrap(), Traversal::AllowMounts)
        .unwrap()
}

#[test]
fn parent_observations_reach_the_actual_volume_root() {
    let fixture = tempfile::tempdir().unwrap();
    let root = Directory::open_absolute(&fixture.path().canonicalize().unwrap()).unwrap();
    let parent = child(&root, "parent");
    let leaf = child(&parent, "leaf");
    assert_eq!(
        leaf.parent_snapshot().unwrap().identity().unwrap(),
        parent.identity().unwrap()
    );
    assert_eq!(
        parent.parent_snapshot().unwrap().identity().unwrap(),
        root.identity().unwrap()
    );
    let mut current = leaf;
    for _ in 0..256 {
        let next = current.parent_snapshot().unwrap();
        if next.identity().unwrap() == current.identity().unwrap() {
            return;
        }
        current = next;
    }
    panic!("native parent observations did not reach a volume root");
}

#[test]
fn moved_directory_observes_new_parent_instead_of_cached_ancestry() {
    let fixture = tempfile::tempdir().unwrap();
    let root = Directory::open_absolute(&fixture.path().canonicalize().unwrap()).unwrap();
    let original_parent = child(&root, "original");
    let destination = child(&root, "destination");
    let leaf = child(&original_parent, "leaf");
    let expected = leaf.identity().unwrap();
    pcl_core::file_ops::rename_noreplace_at(
        original_parent.file(),
        "leaf".as_ref(),
        destination.file(),
        "moved".as_ref(),
    )
    .unwrap();
    assert_eq!(leaf.identity().unwrap(), expected);
    assert_eq!(
        leaf.parent_snapshot().unwrap().identity().unwrap(),
        destination.identity().unwrap()
    );
    assert_ne!(
        leaf.parent_snapshot().unwrap().identity().unwrap(),
        original_parent.identity().unwrap()
    );
}

#[cfg(windows)]
#[path = "../src/file_ops/junction_fixture.rs"]
mod junction_fixture;

#[cfg(windows)]
#[test]
fn an_in_place_junction_cannot_supply_an_ancestry_capability() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().canonicalize().unwrap();
    let root = Directory::open_absolute(&path).unwrap();
    let target = child(&root, "target");
    let outside = child(&root, "outside");
    junction_fixture::set_existing(&path.join("outside"), &path.join("target")).unwrap();
    assert!(target.parent_snapshot().is_err());
    assert!(outside.names(1).unwrap().is_empty());
    junction_fixture::clear_existing(&path.join("target")).unwrap();
}
