use super::{
    binding::{Binding, Selected},
    model::LogKind,
    native_export, reader,
};
use crate::launcher_local::filesystem::Dir;
use flate2::{write::GzEncoder, Compression};
use pcl_core::{
    content_scope::IsolationPolicy,
    retained_fs::{Directory, Leaf},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let parent = Directory::open_absolute(&base).unwrap();
        let root = parent
            .create_private(
                &Leaf::new("game").unwrap(),
                pcl_core::retained_fs::Traversal::AllowMounts,
            )
            .unwrap();
        let versions = root
            .create_private(
                &Leaf::new("versions").unwrap(),
                pcl_core::retained_fs::Traversal::AllowMounts,
            )
            .unwrap();
        let instance = versions
            .create_private(
                &Leaf::new("Example").unwrap(),
                pcl_core::retained_fs::Traversal::AllowMounts,
            )
            .unwrap();
        let mut json = instance
            .create_file(&Leaf::new("Example.json").unwrap())
            .unwrap();
        json.write_all(b"{\"id\":\"Example\"}").unwrap();
        parent
            .create_private(
                &Leaf::new("outside").unwrap(),
                pcl_core::retained_fs::Traversal::AllowMounts,
            )
            .unwrap();
        Self {
            root: base.join("game"),
            outside: base.join("outside"),
            _temp: temp,
        }
    }
    fn bind(&self, policy: IsolationPolicy) -> Binding {
        Binding::capture_with_isolation(&self.root, "Example", policy).unwrap()
    }
    fn write(&self, base: &Path, name: &str, bytes: &[u8]) {
        let dir = Dir::absolute(base).unwrap();
        let logs = dir
            .optional("logs")
            .unwrap()
            .unwrap_or_else(|| dir.create("logs").unwrap());
        logs.write_new(name, bytes).unwrap();
    }
}
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
    gzip.write_all(bytes).unwrap();
    gzip.finish().unwrap()
}

#[test]
fn native_logs_bind_only_the_explicit_scope_and_keep_unrelated_rows_stable() {
    let f = Fixture::new();
    f.write(&f.root, "latest.log", b"shared evidence\n");
    f.write(
        &f.root.join("versions/Example"),
        "latest.log",
        b"isolated evidence\n",
    );
    for (policy, expected) in [
        (IsolationPolicy::Shared, "shared evidence\n"),
        (IsolationPolicy::Isolated, "isolated evidence\n"),
    ] {
        let mut binding = f.bind(policy);
        binding.validate_json().unwrap();
        let signature = binding.signature().unwrap();
        let mut selected = Selected::capture(&binding, LogKind::Game, "latest.log").unwrap();
        let text = selected.read(&AtomicBool::new(false)).unwrap();
        assert_eq!(text.text, expected);
        let base = binding.game_path();
        f.write(&base, "other.log", b"unrelated\n");
        selected.verify(&binding).unwrap();
        binding.verify().unwrap();
        assert_eq!(binding.signature().unwrap(), signature);
    }
}

#[test]
fn native_logs_reject_changed_json_markers_and_selected_file_without_mutating_them() {
    for action in ["json", "marker", "selected"] {
        let f = Fixture::new();
        f.write(&f.root, "latest.log", b"original evidence\n");
        let mut binding = f.bind(IsolationPolicy::Automatic);
        binding.validate_json().unwrap();
        let selected = Selected::capture(&binding, LogKind::Game, "latest.log").unwrap();
        let changed = match action {
            "json" => f.root.join("versions/Example/Example.json"),
            "marker" => f.root.join("versions/Example/options.txt"),
            _ => f.root.join("logs/latest.log"),
        };
        if action == "marker" {
            Dir::absolute(changed.parent().unwrap())
                .unwrap()
                .write_new("options.txt", b"changed")
                .unwrap();
        } else {
            fs::write(&changed, b"external edit\n").unwrap();
        }
        assert!(if action == "selected" {
            selected.verify(&binding)
        } else {
            binding.verify()
        }
        .is_err());
        assert_eq!(
            fs::read(&changed).unwrap(),
            if action == "marker" {
                b"changed".as_slice()
            } else {
                b"external edit\n".as_slice()
            }
        );
    }
}

#[test]
fn native_reader_redacts_complete_bounded_gzip_tail_before_no_replace_export() {
    let f = Fixture::new();
    let mut raw = vec![b'x'; reader::MAX_TEXT_BYTES + 128];
    raw.extend_from_slice(b"\nsecret=synthetic-token\nlast line\n");
    let packed = gzip(&raw);
    f.write(&f.root, "older.log.gz", &packed);
    let binding = f.bind(IsolationPolicy::Shared);
    let mut selected = Selected::capture(&binding, LogKind::Game, "older.log.gz").unwrap();
    let text = selected.read(&AtomicBool::new(false)).unwrap();
    selected.verify(&binding).unwrap();
    assert!(text.truncated);
    assert_eq!(text.text, "secret=synthetic-token\nlast line\n");
    let redacted = reader::bound_redacted(
        text.text.replace("synthetic-token", "[REDACTED]"),
        text.truncated,
    );
    let target = f.outside.join("excerpt.txt");
    let saved = native_export::publish(&target, redacted.text.as_bytes(), || {
        binding.verify()?;
        selected.verify(&binding)
    })
    .unwrap();
    assert!(
        saved.warning.is_none(),
        "export warning: {:?}",
        saved.warning
    );
    assert_eq!(fs::read(&target).unwrap(), redacted.text.as_bytes());
    assert_eq!(fs::read(f.root.join("logs/older.log.gz")).unwrap(), packed);
    assert!(native_export::publish(&target, b"overwrite", || Ok(())).is_err());
    assert_eq!(fs::read(&target).unwrap(), redacted.text.as_bytes());
    assert_eq!(fs::read_dir(&f.outside).unwrap().count(), 1);
}

#[test]
fn native_export_denied_authorization_never_creates_target_or_changes_source() {
    let f = Fixture::new();
    f.write(&f.root, "latest.log", b"kept\n");
    let target = f.outside.join("denied.txt");
    let error = native_export::publish(&target, b"redacted excerpt", || {
        Err::<(), _>("retired selection".into())
    })
    .err()
    .unwrap();
    assert!(error.contains("retired selection"));
    assert!(!target.exists());
    assert_eq!(fs::read(f.root.join("logs/latest.log")).unwrap(), b"kept\n");
    // An uncommitted native named lease is explicit private evidence. It never
    // authorizes deleting a destination or a user's source as rollback.
}

#[test]
fn native_reader_rejects_corrupt_gzip_and_cancelled_reads() {
    let f = Fixture::new();
    let mut packed = gzip(b"protected evidence\n");
    let crc = packed.len() - 8;
    packed[crc] ^= 0xff;
    f.write(&f.root, "broken.log.gz", &packed);
    let binding = f.bind(IsolationPolicy::Shared);
    let mut selected = Selected::capture(&binding, LogKind::Game, "broken.log.gz").unwrap();
    assert!(selected
        .read(&AtomicBool::new(false))
        .err()
        .unwrap()
        .contains("损坏或不完整"));
    assert!(selected
        .read(&AtomicBool::new(true))
        .err()
        .unwrap()
        .contains("取消"));
    assert_eq!(fs::read(f.root.join("logs/broken.log.gz")).unwrap(), packed);
}

#[test]
fn native_destination_ancestry_rejects_source_and_revalidates_chosen_parent() {
    let f = Fixture::new();
    let root_id = Dir::absolute(&f.root).unwrap().identity().unwrap();
    for parent in [&f.root, &f.root.join("versions/Example")] {
        assert!(super::destination::Destination::capture(
            &parent.join("excerpt.txt"),
            root_id.clone()
        )
        .is_err());
    }
    let chosen =
        super::destination::Destination::capture(&f.outside.join("excerpt.txt"), root_id).unwrap();
    chosen.verify().unwrap();
    // Windows legitimately denies renaming some held directories. A denial is
    // safe; otherwise the newly substituted parent must fail revalidation.
    if fs::rename(&f.outside, f.outside.with_file_name("retained-outside")).is_ok() {
        fs::create_dir(&f.outside).unwrap();
        assert!(chosen.verify().is_err());
    }
}

#[test]
fn native_log_hardlinks_and_directory_carriers_are_rejected_without_mutation() {
    for directory in [false, true] {
        let f = Fixture::new();
        let logs = Dir::absolute(&f.root).unwrap().create("logs").unwrap();
        let path = f.root.join("logs/latest.log");
        if directory {
            logs.create("latest.log").unwrap();
        } else {
            Dir::absolute(&f.outside)
                .unwrap()
                .write_new("original", b"kept\n")
                .unwrap();
            fs::hard_link(f.outside.join("original"), &path).unwrap();
        }
        let binding = f.bind(IsolationPolicy::Shared);
        assert!(Selected::capture(&binding, LogKind::Game, "latest.log").is_err());
        assert!(path.exists());
        if !directory {
            assert_eq!(fs::read(f.outside.join("original")).unwrap(), b"kept\n");
        }
    }
}
