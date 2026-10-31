use super::*;
use std::{
    fs,
    os::unix::fs::symlink,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|path| {
                path.join("crates/core/Cargo.toml").is_file()
                    && path.join("apps/desktop/src-tauri/Cargo.toml").is_file()
            })
            .expect("test workspace root")
            .join("work/launcher-service-fixtures")
            .join(format!(
                "assets-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> AssetStore {
        AssetStore::new(self.0.clone())
    }
    fn folder(&self, collection: AssetCollection) -> PathBuf {
        let path = self.0.join(".pcl-rust").join(collection.folder());
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn png() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend(1u32.to_be_bytes());
        bytes.extend(1u32.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn listing_is_read_only_and_ids_are_stable_until_file_changes() {
    let f = Fixture::new();
    let store = f.store();
    assert!(store.list(AssetCollection::Backgrounds).unwrap().is_empty());
    assert!(!f.0.join(".pcl-rust").exists());
    let folder = f.folder(AssetCollection::Backgrounds);
    let path = folder.join("wall.png");
    fs::write(&path, Fixture::png()).unwrap();
    let first = store.list(AssetCollection::Backgrounds).unwrap();
    let second = store.list(AssetCollection::Backgrounds).unwrap();
    assert_eq!(first[0].id, second[0].id);
    let response = store
        .read_asset(AssetCollection::Backgrounds, &first[0].id, None)
        .unwrap();
    assert!(!response.partial);
    assert_eq!(response.bytes, Fixture::png());
    assert_eq!(response.mime, "image/png");
    let head = store
        .head_asset(AssetCollection::Backgrounds, &first[0].id, None)
        .unwrap();
    assert_eq!(head.mime, "image/png");
    assert_eq!(head.content_length, Fixture::png().len() as u64);
    assert_eq!(head.bytes.capacity(), 0);
    fs::write(&path, Fixture::png()).unwrap();
    assert!(store
        .read_asset(AssetCollection::Backgrounds, &first[0].id, None)
        .is_err());
    assert!(store
        .head_asset(AssetCollection::Backgrounds, &first[0].id, None)
        .is_err());
    let changed = store.list(AssetCollection::Backgrounds).unwrap();
    assert_ne!(first[0].id, changed[0].id);
}
#[test]
fn fixed_catalog_rejects_arbitrary_paths_symlinks_hardlinks_and_wrong_magic() {
    for kind in ["symlink", "hardlink", "wrong"] {
        let f = Fixture::new();
        let folder = f.folder(AssetCollection::Backgrounds);
        fs::write(f.0.join("private.json"), Fixture::png()).unwrap();
        match kind {
            "symlink" => symlink(f.0.join("private.json"), folder.join("wall.png")).unwrap(),
            "hardlink" => fs::hard_link(f.0.join("private.json"), folder.join("wall.png")).unwrap(),
            _ => fs::write(folder.join("wall.png"), b"{\"credential\":\"fake\"}").unwrap(),
        }
        let store = f.store();
        assert!(store.list(AssetCollection::Backgrounds).is_err());
        assert!(store
            .read_asset(AssetCollection::Backgrounds, "../../private.json", None)
            .is_err());
        assert!(store
            .head_asset(AssetCollection::Backgrounds, "../../private.json", None)
            .is_err());
        assert!(store.read_title(&f.0.join("private.json")).is_err());
    }
}
#[test]
fn old_media_id_cannot_read_a_replacement_or_rebound_directory() {
    let f = Fixture::new();
    let folder = f.folder(AssetCollection::Backgrounds);
    fs::write(folder.join("wall.png"), Fixture::png()).unwrap();
    let store = f.store();
    let row = store.list(AssetCollection::Backgrounds).unwrap().remove(0);
    fs::rename(&folder, f.0.join("retained-backgrounds")).unwrap();
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("wall.png"), Fixture::png()).unwrap();
    assert!(store
        .read_asset(AssetCollection::Backgrounds, &row.id, None)
        .is_err());
    assert!(store
        .head_asset(AssetCollection::Backgrounds, &row.id, None)
        .is_err());
    assert!(f.0.join("retained-backgrounds/wall.png").exists());
}
#[test]
fn range_requests_report_exact_bytes_and_normal_get_remains_full() {
    let f = Fixture::new();
    let folder = f.folder(AssetCollection::Backgrounds);
    let bytes = Fixture::png();
    fs::write(folder.join("wall.png"), &bytes).unwrap();
    let store = f.store();
    let row = store.list(AssetCollection::Backgrounds).unwrap().remove(0);
    for (range, start, end) in [
        ("bytes=1-3", 1, 3),
        ("bytes=30-", 30, bytes.len() - 1),
        ("bytes=-3", bytes.len() - 3, bytes.len() - 1),
    ] {
        let response = store
            .read_asset(AssetCollection::Backgrounds, &row.id, Some(range))
            .unwrap();
        assert!(response.partial);
        assert_eq!(response.start, start as u64);
        assert_eq!(response.end, end as u64);
        assert_eq!(response.bytes, bytes[start..=end]);
        assert_eq!(response.total, bytes.len() as u64);
    }
    for range in [
        "bytes=0-2,4-6",
        "bytes=-0",
        "bytes=99-",
        "bytes=3-2",
        "bytes=x-3",
        "items=0-3",
    ] {
        assert!(store
            .read_asset(AssetCollection::Backgrounds, &row.id, Some(range))
            .is_err());
    }
    assert_eq!(
        store
            .read_asset(AssetCollection::Backgrounds, &row.id, None)
            .unwrap()
            .bytes,
        bytes
    );
    assert_eq!(
        parse_range(Some("bytes=0-"), MAX_MEDIA_BYTES).unwrap(),
        (0, MAX_RANGE_BYTES - 1, true)
    );
}
#[test]
fn title_import_snapshots_source_publishes_owned_copy_and_refuses_stale_input() {
    let f = Fixture::new();
    let path = f.0.join("picked.png");
    fs::write(&path, Fixture::png()).unwrap();
    let plan = prepare_title_import(&path).unwrap();
    let store = f.store();
    let imported = store.import_title(&path, &plan.revision).unwrap();
    assert_eq!(imported.kind, AssetKind::Image);
    assert!(Path::new(&imported.path)
        .parent()
        .unwrap()
        .ends_with(".pcl-rust/titles"));
    assert_eq!(
        store
            .read_asset(AssetCollection::Titles, &imported.id, None)
            .unwrap()
            .bytes,
        Fixture::png()
    );
    fs::write(&path, Fixture::png()).unwrap();
    assert!(store.import_title(&path, &plan.revision).is_err());
    assert!(store.read_title(&path).is_err());
    assert_eq!(
        fs::read_dir(f.0.join(".pcl-rust/titles")).unwrap().count(),
        1
    );
}
#[test]
fn type_checks_refuse_html_svg_and_video_in_title_or_music() {
    let f = Fixture::new();
    let path = f.0.join("picked.png");
    fs::write(&path, b"<svg onload='alert(1)'></svg>").unwrap();
    assert!(prepare_title_import(&path).is_err());
    let mp4 = b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isommp42";
    assert!(media::classify(
        "video.mp4",
        mp4,
        mp4.len() as u64,
        AssetCollection::Backgrounds
    )
    .is_ok());
    assert!(media::classify("video.mp4", mp4, mp4.len() as u64, AssetCollection::Titles).is_err());
    assert!(media::classify("track.mp3", mp4, mp4.len() as u64, AssetCollection::Music).is_err());
}

#[test]
fn malformed_and_truncated_media_headers_never_panic() {
    let ebml = b"\x1a\x45\xdf\xa3\x01\x00\x00\x00\x00\x00\x00\x00";
    assert!(media::classify(
        "bad.webm",
        ebml,
        ebml.len() as u64,
        AssetCollection::Backgrounds
    )
    .is_err());
    for size in 0..64 {
        let header = vec![0xff; size];
        for (name, collection) in [
            ("x.png", AssetCollection::Backgrounds),
            ("x.jpg", AssetCollection::Backgrounds),
            ("x.gif", AssetCollection::Backgrounds),
            ("x.webp", AssetCollection::Backgrounds),
            ("x.mp4", AssetCollection::Backgrounds),
            ("x.webm", AssetCollection::Backgrounds),
            ("x.mp3", AssetCollection::Music),
            ("x.ogg", AssetCollection::Music),
            ("x.wav", AssetCollection::Music),
            ("x.flac", AssetCollection::Music),
        ] {
            assert!(media::classify(name, &header, size as u64, collection).is_err());
        }
    }
}

#[test]
fn only_explicit_folder_open_creates_media_directories_and_bounds_apply_first() {
    let f = Fixture::new();
    let store = f.store();
    let path = store.ensure_folder(AssetCollection::Music).unwrap();
    assert!(path.ends_with(".pcl-rust/music"));
    assert!(path.is_dir());
    let background = f.folder(AssetCollection::Backgrounds);
    let large = background.join("too-large.mp4");
    fs::File::create(&large)
        .unwrap()
        .set_len(MAX_MEDIA_BYTES + 1)
        .unwrap();
    assert!(store.list(AssetCollection::Backgrounds).is_err());
    let title = f.0.join("title.png");
    fs::File::create(&title)
        .unwrap()
        .set_len(MAX_IMAGE_BYTES + 1)
        .unwrap();
    assert!(prepare_title_import(&title).is_err());
    assert!(!f.0.join(".pcl-rust/titles").exists());
}
#[test]
fn declarative_homepage_keeps_html_as_text_and_rejects_unknowns_and_credentials() {
    let page=parse_homepage(br#"{"schema_version":1,"sections":[{"text":"<script>fake()</script>","links":[{"label":"Website","url":"https://example.org/docs"}]}]}"#).unwrap();
    assert_eq!(page.sections[0].text, "<script>fake()</script>");
    for input in [br#"{"schema_version":1,"sections":[],"script":"fake()"}"#.as_slice(),br#"{"schema_version":1,"sections":[{"text":"ok","links":[{"label":"x","url":"javascript:fake()"}]}]}"#,br#"{"schema_version":1,"sections":[{"text":"ok","links":[{"label":"x","url":"https://user:secret@example.org"}]}]}"#] {assert!(parse_homepage(input).is_err());}
    let oversized = vec![b' '; 256 * 1024 + 1];
    assert!(parse_homepage(&oversized).is_err());
}
#[test]
fn local_homepage_is_stable_nofollow_and_private_configuration_names_are_rejected() {
    let f = Fixture::new();
    let bytes = br#"{"schema_version":1,"title":"Custom","sections":[{"text":"Hello"}]}"#;
    let path = f.0.join("home.json");
    fs::write(&path, bytes).unwrap();
    assert_eq!(
        read_homepage(&path).unwrap().page.title.as_deref(),
        Some("Custom")
    );
    symlink(&path, f.0.join("alias.json")).unwrap();
    assert!(read_homepage(&f.0.join("alias.json")).is_err());
    fs::write(f.0.join("accounts.json"), bytes).unwrap();
    assert!(read_homepage(&f.0.join("accounts.json")).is_err());
    let error = parse_homepage(br#"{"fake-access-token":"private"}"#).unwrap_err();
    assert!(!error.contains("fake-access-token"));
    assert!(!error.contains("private"));
}
#[test]
fn remote_url_and_each_dns_or_redirect_destination_must_be_public_https() {
    for url in [
        "file:///etc/passwd",
        "http://example.org/home.json",
        "https://user:pass@example.org/",
        "https://127.1/",
        "https://[::1]/",
        "https://localhost/",
        "https://service.local/home.json",
    ] {
        assert!(validate_homepage_url(url).is_err(), "{url}");
    }
    let public = validate_homepage_url("https://example.org/home.json").unwrap();
    assert_eq!(public.host, "example.org");
    assert_eq!(public.port, 443);
    let normalized = validate_homepage_url("https://EXAMPLE.ORG./home.json").unwrap();
    assert_eq!(normalized.host, "example.org");
    assert_eq!(normalized.url, "https://example.org/home.json");
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "169.254.169.254",
        "192.168.1.1",
        "100.64.0.1",
        "::1",
        "fd00::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "2001:db8::1",
    ] {
        assert!(
            validate_public_addresses(&[ip.parse().unwrap()]).is_err(),
            "{ip}"
        );
    }
    assert!(validate_public_addresses(&[
        "8.8.8.8".parse().unwrap(),
        "2606:4700:4700::1111".parse().unwrap()
    ])
    .is_ok());
    assert!(
        validate_public_addresses(&["8.8.8.8".parse().unwrap(), "127.0.0.1".parse().unwrap()])
            .is_err()
    );
}

#[test]
#[ignore = "read-only installed-fontconfig smoke check, explicitly opt in"]
fn system_fontconfig_probe() {
    let families = discover_font_families().unwrap();
    assert!(!families.is_empty());
    assert!(families.len() <= 4096);
    assert!(families.windows(2).all(|pair| pair[0] < pair[1]));
}
