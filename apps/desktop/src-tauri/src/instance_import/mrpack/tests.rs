use super::*;
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::{symlink, MetadataExt},
    sync::atomic::{AtomicU64, Ordering},
};
use zip::{write::SimpleFileOptions, ZipWriter};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    source: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/mrpack-preview-2026-10-05/services/fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("root")).unwrap();
        let base = base.canonicalize().unwrap();
        Self {
            root: base.join("root"),
            source: base.join("pack.data"),
            base,
        }
    }
    fn zip(&self, index: &[u8], extras: &[(&str, &[u8])]) {
        let mut writer = ZipWriter::new(fs::File::create(&self.source).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file(INDEX, options).unwrap();
        writer.write_all(index).unwrap();
        for (path, bytes) in extras {
            writer.start_file(*path, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    fn manifest(&self, files: Vec<Value>, extras: &[(&str, &[u8])]) {
        self.zip(
            serde_json::to_string(&index(files)).unwrap().as_bytes(),
            extras,
        );
    }
    fn inspect(&self, optional: Option<&[String]>) -> Result<PackPlan> {
        inspect(&self.root, &self.source, "Preview", optional)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}
fn index(files: Vec<Value>) -> Value {
    json!({"formatVersion":1,"game":"minecraft","versionId":"1.0","name":"Fixture Pack","summary":"Local preview","files":files,"dependencies":{"minecraft":"1.20.1","fabric-loader":"0.16.0"}})
}
fn remote(path: &str, client: Option<&str>, size: u64) -> Value {
    let mut file = json!({"path":path,"hashes":{"sha1":"a".repeat(40),"sha512":"b".repeat(128)},"downloads":["https://cdn.modrinth.com/fixture/file.jar"],"fileSize":size});
    if let Some(client) = client {
        file["env"] = json!({"client":client,"server":"required"});
    }
    file
}

#[test]
fn marker_recognition_and_read_only_default_client_plan() {
    let f = Fixture::new();
    f.manifest(
        vec![
            remote("mods/required.jar", None, 20),
            remote("mods/optional.jar", Some("optional"), 30),
            remote("mods/server.jar", Some("unsupported"), 40),
        ],
        &[],
    );
    let before = fs::read(&f.source).unwrap();
    let metadata = fs::metadata(&f.source).unwrap();
    let root_key = fs::metadata(&f.root).unwrap().ino();
    assert!(recognizes(&f.source).unwrap());
    let plan = f.inspect(None).unwrap();
    assert_eq!(plan.format, "modrinth");
    assert!(!plan.installable);
    assert_eq!(plan.file_count, 1);
    assert_eq!(plan.bytes, 20);
    assert_eq!(plan.reused_files, 0);
    assert_eq!(
        (
            plan.preview.required_files,
            plan.preview.optional_files,
            plan.preview.excluded_files
        ),
        (1, 1, 2)
    );
    assert_eq!(plan.preview.download_bytes, 20);
    assert!(plan.preview.blockers.is_empty());
    let dto = serde_json::to_value(&plan).unwrap();
    assert!(dto.get("binding").is_none());
    assert!(!dto.to_string().contains("cdn.modrinth.com"));
    assert!(!dto.to_string().contains(&f.source.display().to_string()));
    assert_eq!(fs::read(&f.source).unwrap(), before);
    assert_eq!(fs::metadata(&f.source).unwrap().ino(), metadata.ino());
    assert_eq!(fs::metadata(&f.root).unwrap().ino(), root_key);
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 0);
    plan.recheck().unwrap();
}
#[test]
fn exact_optional_choices_and_overlay_have_distinct_authority() {
    let f = Fixture::new();
    f.manifest(
        vec![
            remote("mods/a.jar", None, 20),
            remote("mods/b.jar", Some("optional"), 30),
            remote("mods/server.jar", Some("unsupported"), 40),
        ],
        &[
            ("overrides/mods/a.jar", b"common"),
            ("client-overrides/mods/a.jar", b"client"),
            ("overrides/config/x.cfg", b"abc"),
            ("server-overrides/config/x.cfg", b"server"),
        ],
    );
    let plan = f.inspect(None).unwrap();
    assert_eq!(plan.file_count, 2);
    assert_eq!(plan.bytes, 9);
    assert_eq!(plan.preview.download_bytes, 0);
    assert_eq!(
        (
            plan.preview.override_files,
            plan.preview.override_bytes,
            plan.preview.client_overrides,
            plan.preview.shadowed_files
        ),
        (2, 9, 1, 2)
    );
    assert!(plan.preview.files[0].selected && plan.preview.files[0].overridden);
    let chosen = f.inspect(Some(&["mods/b.jar".into()])).unwrap();
    assert_eq!(chosen.file_count, 3);
    assert_eq!(chosen.bytes, 39);
    assert_eq!(chosen.preview.download_bytes, 30);
    assert_ne!(plan.revision, chosen.revision);
    for paths in [
        vec!["mods/a.jar".into()],
        vec!["mods/server.jar".into()],
        vec!["unknown.jar".into()],
        vec!["mods/b.jar".into(), "mods/b.jar".into()],
        vec!["../escape".into()],
    ] {
        assert!(f.inspect(Some(&paths)).is_err());
    }
    assert!(plan
        .warnings
        .iter()
        .any(|warning| warning.contains("server-overrides")));
}

#[test]
fn format_env_hash_uri_and_duplicate_key_schema_are_strict() {
    let f = Fixture::new();
    for scenario in [
        "format",
        "game",
        "env",
        "sha1",
        "sha512",
        "http",
        "credentials",
        "fragment",
        "space",
        "percent",
        "duplicate-path",
    ] {
        let mut value = index(vec![remote("mods/a.jar", None, 20)]);
        match scenario {
            "format" => value["formatVersion"] = 2.into(),
            "game" => value["game"] = "other".into(),
            "env" => value["files"][0]["env"] = json!({"client":"sometimes","server":"required"}),
            "sha1" => {
                value["files"][0]["hashes"]
                    .as_object_mut()
                    .unwrap()
                    .remove("sha1");
            }
            "sha512" => value["files"][0]["hashes"]["sha512"] = "bad".into(),
            "duplicate-path" => value["files"].as_array_mut().unwrap().push(remote(
                "mods/a.jar",
                Some("optional"),
                30,
            )),
            _ => {
                value["files"][0]["downloads"] = json!([match scenario {
                    "http" => "http://cdn.modrinth.com/file.jar",
                    "credentials" => "https://user:pass@cdn.modrinth.com/file.jar",
                    "fragment" => "https://cdn.modrinth.com/file.jar#secret",
                    "space" => "https://cdn.modrinth.com/file name.jar",
                    _ => "https://cdn.modrinth.com/file%ZZ.jar",
                }])
            }
        }
        f.zip(value.to_string().as_bytes(), &[]);
        assert!(f.inspect(None).is_err(), "{scenario}");
    }
    let base = index(vec![remote("mods/a.jar", Some("optional"), 20)]).to_string();
    for malformed in [
        base.replace(
            "\"formatVersion\":1",
            "\"formatVersion\":1,\"formatVersion\":1",
        ),
        base.replace(
            "\"client\":\"optional\"",
            "\"client\":\"optional\",\"cl\\u0069ent\":\"required\"",
        ),
        base.replace(
            "\"minecraft\":\"1.20.1\"",
            "\"minecraft\":\"1.20.1\",\"m\\u0069necraft\":\"1.20.1\"",
        ),
        base.replace(
            &format!("\"sha1\":\"{}\"", "a".repeat(40)),
            &format!(
                "\"sha1\":\"{}\",\"sha1\":\"{}\"",
                "a".repeat(40),
                "a".repeat(40)
            ),
        ),
    ] {
        assert_ne!(malformed, base);
        f.zip(malformed.as_bytes(), &[]);
        assert!(f.inspect(None).is_err());
    }
}

#[test]
fn unsupported_mirrors_and_loader_capabilities_are_explicit_blockers() {
    let f = Fixture::new();
    let mut file = remote("mods/a.jar", None, 20);
    file["downloads"] = json!(["https://example.invalid/file.jar"]);
    f.manifest(vec![file], &[]);
    assert_eq!(f.inspect(None).unwrap().preview.blockers.len(), 1);
    for (minecraft, id, version, expected) in [
        ("1.7.10", "forge", "10.13.4.1614", false),
        ("1.12.2", "forge", "14.23.5.2860", true),
        ("1.20.1", "neoforge", "47.1.0", true),
        ("1.21.1", "neoforge", "21.1.234", true),
        ("26.1", "neoforge", "26.1.0.16-beta", true),
        ("26.1.2", "neoforge", "26.1.2.1-beta", true),
        ("26.1", "neoforge", "26.1.2.1-beta", false),
        ("26.1", "neoforge", "26.1.0.16-arbitrary", false),
        ("1.20.1", "quilt-loader", "0.27.0", false),
    ] {
        let mut value = index(vec![]);
        value["dependencies"] = json!({"minecraft":minecraft,id:version});
        f.zip(value.to_string().as_bytes(), &[]);
        let plan = f.inspect(None).unwrap();
        assert_eq!(
            plan.preview
                .dependencies
                .iter()
                .find(|d| d.id == id)
                .unwrap()
                .supported,
            expected,
            "{minecraft}/{id}/{version}"
        );
        assert_eq!(plan.preview.blockers.is_empty(), expected);
    }
    let mut value = index(vec![]);
    value["dependencies"] = json!({"minecraft":"1.20.1","forge":"47.1.0","fabric-loader":"0.16.0"});
    f.zip(value.to_string().as_bytes(), &[]);
    assert!(f
        .inspect(None)
        .unwrap()
        .preview
        .blockers
        .iter()
        .any(|b| b.contains("多个")));
}

#[test]
fn traversal_reserved_paths_and_final_overlay_prefix_conflicts_are_refused() {
    let f = Fixture::new();
    for path in [
        "../escape",
        "/absolute",
        "C:/absolute",
        "mods/../escape",
        "mods\\escape",
        ".pcl-rust/config",
        "libraries/x.jar",
        "versions/x.json",
        "launcher_accounts.json",
        "Preview.json",
    ] {
        f.manifest(vec![remote(path, None, 20)], &[]);
        assert!(f.inspect(None).is_err(), "{path}");
    }
    for extras in [
        vec![
            ("overrides/config", b"file".as_slice()),
            ("client-overrides/config/a.cfg", b"child".as_slice()),
        ],
        vec![
            ("overrides/config/a.cfg", b"child".as_slice()),
            ("client-overrides/config", b"file".as_slice()),
        ],
        vec![("overrides/../escape", b"escape".as_slice())],
        vec![("overrides/.pcl-rust/x", b"reserved".as_slice())],
    ] {
        f.manifest(vec![], &extras);
        assert!(f.inspect(None).is_err());
    }
    for remote_file in [true, false] {
        let mut writer = ZipWriter::new(fs::File::create(&f.source).unwrap());
        writer
            .start_file(INDEX, SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(
                index(if remote_file {
                    vec![remote("config", None, 2)]
                } else {
                    vec![]
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap();
        if !remote_file {
            writer
                .start_file("overrides/config", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"file").unwrap();
        }
        writer
            .add_directory(
                "client-overrides/config/subdir/",
                SimpleFileOptions::default(),
            )
            .unwrap();
        writer.finish().unwrap();
        assert!(f.inspect(None).is_err());
    }
}

#[test]
fn source_and_target_recheck_bind_physical_identity_and_do_not_create_directories() {
    for scenario in [
        "source-edit",
        "source-replace",
        "root-replace",
        "versions-created",
        "name-occupied",
    ] {
        let f = Fixture::new();
        f.manifest(vec![], &[]);
        let plan = f.inspect(None).unwrap();
        match scenario {
            "source-edit" => {
                let mut bytes = fs::read(&f.source).unwrap();
                bytes[0] ^= 1;
                fs::write(&f.source, bytes).unwrap();
            }
            "source-replace" => {
                let replacement = f.base.join("other");
                fs::copy(&f.source, &replacement).unwrap();
                fs::rename(replacement, &f.source).unwrap();
            }
            "root-replace" => {
                fs::rename(&f.root, f.base.join("previous-root")).unwrap();
                fs::create_dir(&f.root).unwrap();
            }
            "versions-created" => fs::create_dir(f.root.join("versions")).unwrap(),
            _ => fs::create_dir_all(f.root.join("versions/Preview")).unwrap(),
        }
        assert!(plan.recheck().is_err(), "{scenario}");
    }
    let f = Fixture::new();
    f.manifest(vec![], &[]);
    fs::hard_link(&f.source, f.base.join("alias")).unwrap();
    assert!(f.inspect(None).is_ok());
    let link = f.base.join("link");
    symlink(&f.source, &link).unwrap();
    assert!(inspect(&f.root, &link, "Preview", None).is_err());
}

#[test]
fn every_archive_body_including_server_data_checks_crc_and_node_type() {
    let f = Fixture::new();
    f.manifest(
        vec![],
        &[("server-overrides/config/a", b"server-fixture-body")],
    );
    let mut bytes = fs::read(&f.source).unwrap();
    let offset = bytes
        .windows(19)
        .position(|v| v == b"server-fixture-body")
        .unwrap();
    bytes[offset] ^= 1;
    fs::write(&f.source, &bytes).unwrap();
    assert!(f.inspect(None).is_err());
    let mut writer = ZipWriter::new(fs::File::create(&f.source).unwrap());
    writer
        .start_file(INDEX, SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(index(vec![]).to_string().as_bytes())
        .unwrap();
    writer
        .add_symlink("overrides/link", "outside", SimpleFileOptions::default())
        .unwrap();
    writer.finish().unwrap();
    assert!(f.inspect(None).is_err());
    f.manifest(vec![], &[("overrides/special", b"node")]);
    let mut bytes = fs::read(&f.source).unwrap();
    let central = bytes
        .windows(4)
        .enumerate()
        .filter(|(_, v)| *v == b"PK\x01\x02")
        .map(|(i, _)| i)
        .last()
        .unwrap();
    bytes[central + 38..central + 42]
        .copy_from_slice(&((libc::S_IFIFO | 0o600) << 16).to_le_bytes());
    fs::write(&f.source, bytes).unwrap();
    assert!(f.inspect(None).is_err());
}

#[test]
fn marker_ambiguity_and_duplicate_zip_names_are_refused() {
    let f = Fixture::new();
    f.manifest(vec![], &[("pcl-export.json", b"{}")]);
    assert!(recognizes(&f.source).is_err());
    assert!(f.inspect(None).is_err());
    f.manifest(
        vec![],
        &[("overrides/a", b"first"), ("overrides/b", b"second")],
    );
    let mut bytes = fs::read(&f.source).unwrap();
    let needle = b"overrides/b";
    for i in 0..=bytes.len() - needle.len() {
        if &bytes[i..i + needle.len()] == needle {
            bytes[i + needle.len() - 1] = b'a';
        }
    }
    fs::write(&f.source, bytes).unwrap();
    assert!(recognizes(&f.source).is_err());
}

#[test]
fn recognition_preserves_large_legacy_zip_bounds_before_format_dispatch() {
    use std::io::{Seek, SeekFrom};
    let f = Fixture::new();
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file("pcl-export.json", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"{}").unwrap();
    let mut zip = writer.finish().unwrap().into_inner();
    let prefix = MAX_ARCHIVE + 1;
    let cd = zip.windows(4).position(|v| v == b"PK\x01\x02").unwrap();
    let eocd = zip.windows(4).position(|v| v == b"PK\x05\x06").unwrap();
    let old = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap());
    zip[eocd + 16..eocd + 20].copy_from_slice(&(old + prefix as u32).to_le_bytes());
    zip[cd + 42..cd + 46].copy_from_slice(&(prefix as u32).to_le_bytes());
    let mut source = fs::File::create(&f.source).unwrap();
    source.set_len(prefix).unwrap();
    source.seek(SeekFrom::End(0)).unwrap();
    source.write_all(&zip).unwrap();
    drop(source);
    assert!(!recognizes(&f.source).unwrap());
    assert!(f.inspect(None).is_err());
}

#[test]
fn decoded_manifest_remote_size_and_real_deflate_ratio_limits_are_enforced() {
    let f = Fixture::new();
    f.zip(&vec![b' '; MAX_INDEX as usize + 1], &[]);
    assert!(f.inspect(None).is_err());
    f.manifest(
        vec![remote("mods/huge.jar", None, 2 * 1024 * 1024 * 1024 + 1)],
        &[],
    );
    assert!(f.inspect(None).is_err());
    let mut writer = ZipWriter::new(fs::File::create(&f.source).unwrap());
    writer
        .start_file(INDEX, SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(index(vec![]).to_string().as_bytes())
        .unwrap();
    writer
        .start_file(
            "overrides/config/bomb",
            SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(9)),
        )
        .unwrap();
    for _ in 0..64 {
        writer.write_all(&[0; 128 * 1024]).unwrap();
    }
    writer.finish().unwrap();
    assert!(f.inspect(None).err().unwrap().contains("压缩比"));
}

#[test]
fn mixed_mirrors_keep_supported_install_and_redact_ignored_declarations() {
    let f = Fixture::new();
    let mut file = remote("mods/client.jar", None, 5);
    file["downloads"] = json!([
        "https://cdn.modrinth.com/fixture/file.jar",
        "https://unsupported.example/file.jar?private-query=secret",
    ]);
    f.manifest(vec![file], &[]);
    let plan = f.inspect(None).unwrap();
    assert!(plan.preview.blockers.is_empty());
    assert!(plan
        .warnings
        .iter()
        .any(|warning| warning.contains("1 个不受支持的下载镜像声明")));
    assert!(!plan.warnings.join(" ").contains("secret"));
    assert!(!plan.warnings.join(" ").contains("unsupported.example"));
}

#[test]
fn exact_foreign_pcl_display_and_migration_documents_are_ignored_with_a_warning() {
    let fixture = Fixture::new();
    fixture.manifest(vec![], &[
        ("overrides/PCL/config.json", br#"{"InstanceMigratedJava":true}"#),
        ("client-overrides/PCL/Setup.ini", b"State:6\r\nVersionVanillaName:1.20.1\r\nVersionForge:Example-1\r\nVersionArgumentIndieV2:True\r\nInfo:Synthetic pack\r\nLogo:example\r\nIsStar:False\r\nVersionLiteLoader:False\r\nReleaseTime:2026-01-01 00:00\r\n"),
        ("overrides/config/example.cfg", b"game configuration"),
    ]);
    let plan = fixture.inspect(None).unwrap();
    assert_eq!(plan.file_count, 1);
    assert_eq!(plan.preview.override_files, 1);
    assert!(plan
        .warnings
        .iter()
        .any(|value| value.contains("2 个已识别的 PCL")));
    assert!(!plan
        .preview
        .files
        .iter()
        .any(|file| file.path.starts_with("PCL/")));
}

#[test]
fn foreign_pcl_exceptions_do_not_authorize_unknown_settings_paths_or_remote_files() {
    let fixture = Fixture::new();
    for (path, bytes) in [
        ("overrides/PCL/private.json", b"{}".as_slice()),
        (
            "overrides/PCL/config.json",
            br#"{"InstanceMigratedJava":true,"JavaPath":"example"}"#.as_slice(),
        ),
        (
            "overrides/PCL/config.json",
            br#"{"InstanceMigratedJava":true,"InstanceMigratedJava":false}"#.as_slice(),
        ),
        (
            "overrides/PCL/Setup.ini",
            b"UnknownSetting:example\n".as_slice(),
        ),
        ("overrides/PCL/Setup.ini", b"State:6\nState:6\n".as_slice()),
        ("overrides/pcl/Setup.ini", b"State:6\n".as_slice()),
    ] {
        fixture.manifest(vec![], &[(path, bytes)]);
        assert!(fixture.inspect(None).is_err(), "{path}");
    }
    fixture.manifest(vec![remote("PCL/Setup.ini", None, 12)], &[]);
    assert!(fixture.inspect(None).is_err());
    let oversized = vec![b'x'; 64 * 1024 + 1];
    fixture.manifest(vec![], &[("overrides/PCL/Setup.ini", &oversized)]);
    assert!(fixture.inspect(None).is_err());
}

#[test]
fn ignored_foreign_pcl_metadata_still_requires_the_original_crc() {
    let fixture = Fixture::new();
    let body = br#"{"InstanceMigratedJava":true}"#;
    fixture.manifest(vec![], &[("overrides/PCL/config.json", body)]);
    let mut bytes = fs::read(&fixture.source).unwrap();
    let at = bytes
        .windows(body.len())
        .position(|bytes| bytes == body)
        .unwrap();
    bytes[at + body.len() - 2] = b' ';
    fs::write(&fixture.source, bytes).unwrap();
    let error = match fixture.inspect(None) {
        Ok(_) => panic!("ignored metadata skipped CRC"),
        Err(error) => error,
    };
    assert!(error.contains("CRC"), "{error}");
}
