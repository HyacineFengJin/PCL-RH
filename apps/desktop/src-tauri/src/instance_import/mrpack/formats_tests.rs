use super::*;
use serde_json::json;
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use zip::{write::SimpleFileOptions, ZipWriter};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/modpack-install-2026-10-06/mirrors/formats-fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        Self {
            path: base.join("pack.data"),
            base,
        }
    }
    fn zip(&self, entries: &[(&str, &[u8])]) {
        let mut writer = ZipWriter::new(File::create(&self.path).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, body) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(body).unwrap();
            }
        }
        writer.finish().unwrap();
    }
    fn prepared(&self, format: Format) -> Result<PreparedArchive> {
        prepare(
            super::super::super::open_source(&self.path)?,
            "Imported",
            format,
            None,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}
fn curse(files: Value) -> Value {
    json!({"manifestType":"minecraftModpack","manifestVersion":1,"name":"Fixture","version":"1.0","overrides":"overrides","minecraft":{"version":"1.20.1","modLoaders":[{"id":"forge-47.2.0","primary":true}]},"files":files})
}
fn mcbbs(body: &[u8]) -> Value {
    json!({"manifestType":"minecraftModpack","manifestVersion":1,"name":"Fixture","version":"1.0","description":"","addons":[{"id":"game","version":"1.20.1"},{"id":"forge","version":"47.2.0"}],"files":[{"type":"addon","path":"mods/local.jar","hash":format!("{:x}",Sha1::digest(body))}]})
}
fn failure(result: Result<PreparedArchive>) -> String {
    match result {
        Ok(_) => panic!("unexpected success"),
        Err(error) => error,
    }
}

#[test]
fn curseforge_root_and_single_wrapper_authorize_only_the_declared_override_layer() {
    for prefix in ["", "Wrapped/"] {
        let fixture = Fixture::new();
        let manifest = curse(json!([])).to_string();
        let marker = format!("{prefix}manifest.json");
        let config = format!("{prefix}overrides/config/file.txt");
        let ignored = format!("{prefix}launcher.exe");
        fixture.zip(&[
            (&marker, manifest.as_bytes()),
            (&config, b"configuration"),
            (&ignored, b"ignored"),
        ]);
        assert_eq!(classify(&fixture.path).unwrap(), Format::CurseForge);
        let prepared = fixture.prepared(Format::CurseForge).unwrap();
        assert_eq!(prepared.dependencies["minecraft"], "1.20.1");
        assert_eq!(prepared.dependencies["forge"], "47.2.0");
        assert!(prepared.blockers.is_empty());
        assert_eq!(prepared.outputs.len(), 1);
        let Output::Override(file) = &prepared.outputs["config/file.txt"] else {
            panic!("wrong source")
        };
        assert_eq!(file.archive_path, config);
        assert_eq!(file.hash, format!("{:x}", Sha256::digest(b"configuration")));
        assert_eq!(fs::read_dir(&fixture.base).unwrap().count(), 1);
    }
}

#[test]
fn curseforge_remote_ids_are_strict_and_missing_authority_is_a_blocker() {
    let fixture = Fixture::new();
    let data = curse(json!([{"projectID":123,"fileID":456,"required":true}]));
    fixture.zip(&[
        ("manifest.json", data.to_string().as_bytes()),
        ("overrides/config/local.txt", b"local"),
    ]);
    let prepared = fixture.prepared(Format::CurseForge).unwrap();
    assert_eq!(prepared.outputs.len(), 1);
    assert!(prepared
        .blockers
        .iter()
        .any(|blocker| blocker.contains("官方文件元数据")));
    for files in [
        json!([{"projectID":0,"fileID":456}]),
        json!([{"projectID":123,"fileID":"456"}]),
        json!([{"projectID":123,"fileID":456,"required":"yes"}]),
        json!([{"projectID":123,"fileID":456},{"projectID":123,"fileID":456}]),
    ] {
        fixture.zip(&[("manifest.json", curse(files).to_string().as_bytes())]);
        assert!(fixture.prepared(Format::CurseForge).is_err());
    }
}

#[test]
fn curseforge_root_override_supports_dot_and_excludes_its_manifest() {
    let fixture = Fixture::new();
    let mut data = curse(json!([]));
    data["overrides"] = ".".into();
    fixture.zip(&[
        ("manifest.json", data.to_string().as_bytes()),
        ("config/local.txt", b"local"),
    ]);
    let prepared = fixture.prepared(Format::CurseForge).unwrap();
    assert_eq!(prepared.outputs.len(), 1);
    assert!(prepared.outputs.contains_key("config/local.txt"));
}

#[test]
fn mcbbs_addon_hash_and_compatibility_core_are_both_checked() {
    let fixture = Fixture::new();
    let body = b"local jar";
    let mut data = mcbbs(body);
    data["manifestVersion"] = 2.into();
    let compatible = curse(json!([]));
    fixture.zip(&[
        ("mcbbs.packmeta", data.to_string().as_bytes()),
        ("manifest.json", compatible.to_string().as_bytes()),
        ("overrides/mods/local.jar", body),
    ]);
    assert_eq!(classify(&fixture.path).unwrap(), Format::Mcbbs);
    let prepared = fixture.prepared(Format::Mcbbs).unwrap();
    assert!(prepared.blockers.is_empty());
    assert_eq!(prepared.outputs.len(), 1);
    let mut mismatch = compatible.clone();
    mismatch["minecraft"]["version"] = "1.20.2".into();
    fixture.zip(&[
        ("mcbbs.packmeta", data.to_string().as_bytes()),
        ("manifest.json", mismatch.to_string().as_bytes()),
        ("overrides/mods/local.jar", body),
    ]);
    assert!(failure(fixture.prepared(Format::Mcbbs)).contains("声明不一致"));
    fixture.zip(&[
        ("mcbbs.packmeta", data.to_string().as_bytes()),
        ("overrides/mods/local.jar", b"tampered"),
    ]);
    assert!(failure(fixture.prepared(Format::Mcbbs)).contains("SHA1"));
}

#[test]
fn mcbbs_manifest_addons_variant_and_remote_curse_are_not_ignored() {
    let fixture = Fixture::new();
    let body = b"addon";
    let mut data = mcbbs(body);
    data["files"].as_array_mut().unwrap().push(
        json!({"type":"curse","projectID":1,"fileID":2,"url":"https://cdn.modrinth.com/file"}),
    );
    fixture.zip(&[
        ("Wrapped/manifest.json", data.to_string().as_bytes()),
        ("Wrapped/overrides/mods/local.jar", body),
    ]);
    assert_eq!(classify(&fixture.path).unwrap(), Format::Mcbbs);
    let prepared = fixture.prepared(Format::Mcbbs).unwrap();
    assert!(prepared
        .blockers
        .iter()
        .any(|blocker| blocker.contains("未解析")));
    assert!(prepared.outputs.contains_key("mods/local.jar"));
}

#[test]
fn wrapper_modrinth_preserves_optional_and_client_overlay_authority() {
    let fixture = Fixture::new();
    let remote = |path: &str, client: &str| json!({"path":path,"hashes":{"sha1":"0".repeat(40),"sha512":"0".repeat(128)},"downloads":["https://cdn.modrinth.com/file"],"fileSize":4,"env":{"client":client,"server":"required"}});
    let data = json!({"formatVersion":1,"game":"minecraft","name":"Fixture","versionId":"1","files":[remote("mods/required.jar","required"),remote("mods/optional.jar","optional"),remote("mods/server.jar","unsupported")],"dependencies":{"minecraft":"1.20.1"}});
    fixture.zip(&[
        ("Wrapped/modrinth.index.json", data.to_string().as_bytes()),
        ("Wrapped/overrides/mods/required.jar", b"common"),
        ("Wrapped/client-overrides/mods/required.jar", b"client"),
        ("Wrapped/server-overrides/config/server.txt", b"server"),
    ]);
    assert_eq!(classify(&fixture.path).unwrap(), Format::Modrinth);
    let prepared = fixture.prepared(Format::Modrinth).unwrap();
    assert_eq!(prepared.outputs.len(), 1);
    let Output::Override(file) = &prepared.outputs["mods/required.jar"] else {
        panic!("wrong overlay")
    };
    assert!(file.client);
    assert_eq!(
        file.archive_path,
        "Wrapped/client-overrides/mods/required.jar"
    );
    assert!(prepared
        .preview_files
        .iter()
        .any(|file| file.path == "mods/optional.jar" && !file.selected));
    let selected = ["mods/optional.jar".into()];
    let prepared = prepare(
        File::open(&fixture.path).unwrap(),
        "Imported",
        Format::Modrinth,
        Some(&selected),
    )
    .unwrap();
    assert_eq!(prepared.outputs.len(), 2);
    assert!(matches!(
        prepared.outputs["mods/optional.jar"],
        Output::Remote { .. }
    ));
    assert!(prepared
        .warnings
        .iter()
        .any(|warning| warning.contains("server-overrides")));
}

#[test]
fn hmcl_game_version_and_real_loader_profile_are_mapped_without_core_override() {
    let fixture = Fixture::new();
    let data = json!({"name":"Fixture","version":"1","gameVersion":"1.20.1"});
    let profile = json!({"id":"pack","jar":"1.20.1","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]});
    fixture.zip(&[
        ("modpack.json", data.to_string().as_bytes()),
        ("minecraft/pack.json", profile.to_string().as_bytes()),
        ("minecraft/config/file.txt", b"config"),
    ]);
    assert_eq!(classify(&fixture.path).unwrap(), Format::Hmcl);
    let prepared = fixture.prepared(Format::Hmcl).unwrap();
    assert_eq!(prepared.dependencies["fabric-loader"], "0.16.0");
    assert_eq!(prepared.outputs.len(), 1);
    assert!(!prepared.outputs.contains_key("pack.json"));
    assert_eq!(
        json(prepared.rebuild_profile.as_deref().unwrap()).unwrap(),
        profile
    );
    let mut wrong = profile.clone();
    wrong["jar"] = "1.20.2".into();
    fixture.zip(&[
        ("modpack.json", data.to_string().as_bytes()),
        ("minecraft/version.json", wrong.to_string().as_bytes()),
    ]);
    assert!(failure(fixture.prepared(Format::Hmcl)).contains("不一致"));
}

fn hmcl_runtime() -> Value {
    // Captured-provider-shaped data only: no real provider HTTP is performed.
    json!({
        "id":"1.20.1-fabric", "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
        "javaVersion":{"majorVersion":17,"component":"java-runtime-gamma"},
        "arguments":{"game":["--username","${auth_player_name}","--version","${version_name}"],
            "jvm":["-Djava.library.path=${natives_directory}","-cp","${classpath}","-Dloader.standard=true"]},
        "libraries":[
            {"name":"net.fabricmc:fabric-loader:0.16.0","url":"https://maven.fabricmc.net/",
                "downloads":{"artifact":{"path":"net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar",
                    "url":"https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.16.0/fabric-loader-0.16.0.jar",
                    "size":9,"sha1":"a".repeat(40)}}},
            {"name":"org.lwjgl:lwjgl:3.3.1","rules":[{"action":"allow","os":{"name":"linux"}}],
                "natives":{"linux":"natives-linux"},"extract":{"exclude":["META-INF/"]}}
        ]
    })
}
fn runtime_blockers(declared: &Value, expected: &Value) -> Vec<String> {
    rebuild_blockers(&serde_json::to_vec(declared).unwrap(), expected).unwrap()
}

#[test]
fn hmcl_sparse_standard_runtime_and_defaults_match_captured_profile() {
    let expected = hmcl_runtime();
    let declared = json!({
        "id":"friendly-pack", "jar":"1.20.1", "mainClass":expected["mainClass"],
        "description":"unrelated pack metadata", "customDisplay":{"theme":"blue"},
        "arguments":{"game":["--version","${version_name}"],"jvm":[]},
        "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0@jar","url":"https://maven.fabricmc.net/",
            "downloads":{},"checksums":[],"rules":[],"natives":{},"extract":{},"hint":""}]
    });
    assert!(runtime_blockers(&declared, &expected).is_empty());
    assert!(runtime_blockers(
        &json!({"libraries":null,"patches":null,"arguments":null}),
        &expected
    )
    .is_empty());
    assert!(runtime_blockers(&expected, &expected).is_empty());
    let legacy =
        json!({"minecraftArguments":"--username ${auth_player_name} --version ${version_name}"});
    assert!(runtime_blockers(&json!({"minecraftArguments":"--username   ${auth_player_name}\n --version ${version_name}"}), &legacy).is_empty());
}

#[test]
fn hmcl_extra_library_versions_rules_and_local_hints_block_rebuild() {
    let expected = hmcl_runtime();
    for library in [
        json!({"name":"example:extra-runtime:1"}),
        json!({"name":"net.fabricmc:fabric-loader:0.17.0"}),
        json!({"name":"org.lwjgl:lwjgl:3.3.1","rules":[]}),
        json!({"name":"org.lwjgl:lwjgl:3.3.1","natives":{}}),
        json!({"name":"org.lwjgl:lwjgl:3.3.1","extract":{}}),
        json!({"name":"net.fabricmc:fabric-loader:0.16.0","hint":"local"}),
        json!({"name":"net.fabricmc:fabric-loader:0.16.0","unknownRuntimeFlag":true}),
        json!({"name":"net.fabricmc:fabric-loader:0.16.0","sha1":"b".repeat(40)}),
        json!({"name":"net.fabricmc:fabric-loader:0.16.0","url":"https://example.invalid/"}),
    ] {
        assert!(runtime_blockers(&json!({"libraries":[library]}), &expected)
            .iter()
            .any(|b| b.contains("运行库或库规则")));
    }
    assert!(runtime_blockers(
        &json!({"libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0",
        "sha1":"A".repeat(40),"size":9}]}),
        &expected
    )
    .is_empty());
}

#[test]
fn hmcl_custom_args_cannot_recombine_official_flag_values_or_rules() {
    let expected = hmcl_runtime();
    for arguments in [
        json!({"jvm":["-Dcustom.runtime=true"]}),
        json!({"game":["--username","${version_name}"]}),
        json!({"game":["--username"]}),
        json!({"game":["${version_name}"]}),
        json!({"jvm":["-cp","${natives_directory}"]}),
        json!({"unsupported":["--version","${version_name}"]}),
        json!({"game":[{"rules":[{"action":"allow"}],"value":["--version","${version_name}"]}]}),
    ] {
        assert!(runtime_blockers(&json!({"arguments":arguments}), &expected)
            .iter()
            .any(|b| b.contains("自定义启动参数")));
    }
    assert!(
        runtime_blockers(&json!({"mainClass":"example.CustomMain"}), &expected)
            .iter()
            .any(|b| b.contains("mainClass"))
    );
    assert!(
        runtime_blockers(&json!({"javaVersion":{"majorVersion":21}}), &expected)
            .iter()
            .any(|b| b.contains("javaVersion"))
    );
    assert!(
        runtime_blockers(&json!({"extraLibraries":["example:extra:1"]}), &expected)
            .iter()
            .any(|b| b.contains("extraLibraries"))
    );
}

#[test]
fn hmcl_patch_runtime_is_checked_after_bounded_priority_fold() {
    let expected = hmcl_runtime();
    let patches = json!({"patches":[
        {"id":"fabric","version":"0.16.0","priority":30000,"mainClass":expected["mainClass"],
            "libraries":[expected["libraries"][0]],"arguments":{"jvm":["-Dloader.standard=true"]}},
        {"id":"game","version":"1.20.1","priority":0,"mainClass":"net.minecraft.client.main.Main",
            "arguments":{"game":expected["arguments"]["game"],"jvm":["-Djava.library.path=${natives_directory}","-cp","${classpath}"]}}
    ]});
    assert!(runtime_blockers(&patches, &expected).is_empty());
    let mut custom = patches.clone();
    custom["patches"][0]["arguments"]["jvm"] = json!(["-Dcustom.runtime=true"]);
    assert!(runtime_blockers(&custom, &expected)
        .iter()
        .any(|b| b.contains("自定义启动参数")));
    let mut bad = patches;
    bad["patches"][0]["libraries"] = json!([{"name":"example:patch-runtime:1"}]);
    assert!(runtime_blockers(&bad, &expected)
        .iter()
        .any(|b| b.contains("运行库或库规则")));
    assert!(rebuild_blockers(br#"{"arguments":{},"arguments":{}}"#, &expected).is_err());
    assert!(rebuild_blockers(b"[]", &expected).is_err());
    let many = json!({"patches":(0..65).map(|i|json!({"id":"game","version":"1","priority":i})).collect::<Vec<_>>()});
    assert!(rebuild_blockers(&serde_json::to_vec(&many).unwrap(), &expected).is_err());
}

#[test]
fn multimc_components_are_mapped_and_foreign_commands_are_blocked() {
    let fixture = Fixture::new();
    let data = json!({"formatVersion":1,"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.fabricmc.fabric-loader","version":"0.16.0"},{"uid":"org.lwjgl3","version":"3.3.1"}]});
    fixture.zip(&[
        ("Wrapped/mmc-pack.json", data.to_string().as_bytes()),
        (
            "Wrapped/instance.cfg",
            b"[General]\nname=Fixture\nPreLaunchCommand=echo unsafe\n",
        ),
        ("Wrapped/.minecraft/mods/local.jar", b"jar"),
    ]);
    assert_eq!(classify(&fixture.path).unwrap(), Format::Multimc);
    let prepared = fixture.prepared(Format::Multimc).unwrap();
    assert_eq!(prepared.pack_name, "Fixture");
    assert_eq!(prepared.dependencies["fabric-loader"], "0.16.0");
    assert!(prepared
        .blockers
        .iter()
        .any(|blocker| blocker.contains("自定义命令")));
    assert_eq!(prepared.outputs.len(), 1);
}

#[test]
fn complete_game_folds_inheritance_renames_core_and_preserves_all_shared_files() {
    let fixture = Fixture::new();
    let base = json!({"id":"1.20.1","mainClass":"net.minecraft.client.main.Main","libraries":[{"name":"org.example:module:1"}],"arguments":{"game":["base"],"jvm":["base-jvm"]}});
    let child = json!({"id":"custom","inheritsFrom":"1.20.1","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher","libraries":[{"name":"org.example:module:2"},{"name":"net.minecraftforge:forge:1.20.1-47.2.0"}],"arguments":{"game":["child"]}});
    fixture.zip(&[
        (
            "Wrapped/.minecraft/versions/1.20.1/1.20.1.json",
            base.to_string().as_bytes(),
        ),
        ("Wrapped/.minecraft/versions/1.20.1/1.20.1.jar", b"core"),
        (
            "Wrapped/.minecraft/versions/custom/custom.json",
            child.to_string().as_bytes(),
        ),
        (
            "Wrapped/.minecraft/versions/custom/config/local.txt",
            b"config",
        ),
        ("Wrapped/.minecraft/libraries/", b""),
        ("Wrapped/.minecraft/libraries/generated.jar", b"generated"),
        ("Wrapped/.minecraft/assets/", b""),
        ("Wrapped/.minecraft/assets/objects/cache", b"asset"),
        ("Wrapped/.minecraft/launcher_accounts.json", b"private"),
        ("Wrapped/launcher.exe", b"ignored"),
    ]);
    assert_eq!(classify(&fixture.path).unwrap(), Format::ReadyGame);
    let prepared = fixture.prepared(Format::ReadyGame).unwrap();
    assert_eq!(prepared.dependencies["minecraft"], "1.20.1");
    assert_eq!(prepared.dependencies["forge"], "47.2.0");
    assert_eq!(prepared.outputs.len(), 1);
    assert!(prepared.outputs.contains_key("config/local.txt"));
    let bundled = prepared.bundled.unwrap();
    let metadata: Value = serde_json::from_slice(&bundled.metadata).unwrap();
    assert_eq!(metadata["id"], "Imported");
    assert_eq!(metadata["jar"], "Imported");
    assert_eq!(metadata["clientVersion"], "1.20.1");
    assert!(metadata.get("inheritsFrom").is_none());
    assert_eq!(metadata["arguments"]["game"], json!(["base", "child"]));
    assert_eq!(metadata["libraries"][0]["name"], "org.example:module:2");
    assert_eq!(
        bundled.jar.archive_path,
        "Wrapped/.minecraft/versions/1.20.1/1.20.1.jar"
    );
    assert_eq!(bundled.shared.len(), 2);
    assert!(bundled.shared.contains_key("libraries/generated.jar"));
}

#[test]
fn complete_game_rejects_multiple_leaves_cycles_and_private_metadata() {
    let fixture = Fixture::new();
    let one = json!({"id":"one"});
    let two = json!({"id":"two"});
    fixture.zip(&[
        ("versions/one/one.json", one.to_string().as_bytes()),
        ("versions/one/one.jar", b"core"),
        ("versions/two/two.json", two.to_string().as_bytes()),
        ("versions/two/two.jar", b"core"),
    ]);
    assert!(fixture.prepared(Format::ReadyGame).is_err());
    let private = json!({"id":"1.20.1","minecraftArguments":"--accessToken secret"});
    fixture.zip(&[
        (
            "versions/1.20.1/1.20.1.json",
            private.to_string().as_bytes(),
        ),
        ("versions/1.20.1/1.20.1.jar", b"core"),
    ]);
    assert!(failure(fixture.prepared(Format::ReadyGame)).contains("账户"));
    let cyclic = json!({"id":"one","inheritsFrom":"one"});
    fixture.zip(&[
        ("versions/one/one.json", cyclic.to_string().as_bytes()),
        ("versions/one/one.jar", b"core"),
    ]);
    assert!(failure(fixture.prepared(Format::ReadyGame)).contains("循环"));
}

#[test]
fn independent_markers_or_multiple_wrappers_are_ambiguous() {
    let fixture = Fixture::new();
    let data = curse(json!([])).to_string();
    fixture.zip(&[
        ("manifest.json", data.as_bytes()),
        ("modrinth.index.json", b"{}"),
    ]);
    assert!(classify(&fixture.path).unwrap_err().contains("歧义"));
    fixture.zip(&[
        ("One/manifest.json", data.as_bytes()),
        ("Two/manifest.json", data.as_bytes()),
    ]);
    assert!(classify(&fixture.path).unwrap_err().contains("歧义"));
    fixture.zip(&[("Too/Deep/manifest.json", data.as_bytes())]);
    assert!(classify(&fixture.path).is_err());
}

#[test]
fn all_archive_bodies_including_ignored_executables_receive_crc_validation() {
    let fixture = Fixture::new();
    let data = curse(json!([])).to_string();
    fixture.zip(&[
        ("manifest.json", data.as_bytes()),
        ("launcher.exe", b"BADCRC_FIXTURE"),
    ]);
    let mut bytes = fs::read(&fixture.path).unwrap();
    let at = bytes
        .windows(b"BADCRC_FIXTURE".len())
        .position(|value| value == b"BADCRC_FIXTURE")
        .unwrap();
    bytes[at] ^= 1;
    fs::write(&fixture.path, bytes).unwrap();
    assert_eq!(classify(&fixture.path).unwrap(), Format::CurseForge);
    assert!(failure(fixture.prepared(Format::CurseForge)).contains("CRC"));
}

#[test]
fn duplicate_json_keys_nodes_output_parents_and_launcher_paths_are_rejected() {
    let fixture = Fixture::new();
    let data = curse(json!([])).to_string();
    let duplicate = data.replace(
        "\"version\":\"1.20.1\"",
        "\"version\":\"1.20.1\",\"vers\\u0069on\":\"1.20.2\"",
    );
    fixture.zip(&[("manifest.json", duplicate.as_bytes())]);
    assert!(classify(&fixture.path).is_err());
    fixture.zip(&[
        ("manifest.json", data.as_bytes()),
        ("overrides/config/", b""),
        ("overrides/config", b"file"),
    ]);
    assert!(classify(&fixture.path).is_err());
    fixture.zip(&[
        ("manifest.json", data.as_bytes()),
        ("overrides/config", b"file"),
        ("overrides/config/local.txt", b"child"),
    ]);
    assert!(fixture.prepared(Format::CurseForge).is_err());
    for path in [
        "overrides/../escape",
        "overrides/accounts.json",
        "overrides/launcher.exe",
        "overrides/Imported.json",
    ] {
        fixture.zip(&[("manifest.json", data.as_bytes()), (path, b"bad")]);
        assert!(fixture.prepared(Format::CurseForge).is_err(), "{path}");
    }
}

#[test]
fn pcl_marker_dispatch_does_not_inherit_common_archive_compressed_budget() {
    let fixture = Fixture::new();
    fixture.zip(&[
        ("pcl-export.json", b"{}"),
        (".minecraft/config/file.txt", b"config"),
    ]);
    assert_eq!(classify(&fixture.path).unwrap(), Format::Pcl);
    assert!(failure(fixture.prepared(Format::Pcl)).contains("原生导入服务"));
    // A sparse prefix exercises ZIP64 central offsets beyond the common pack
    // budget without writing or decompressing gigabytes of fixture data.
    let mut file = File::create(&fixture.path).unwrap();
    file.seek(SeekFrom::Start(ARCHIVE_BYTES + 1)).unwrap();
    let mut writer = ZipWriter::new(file);
    writer
        .start_file(
            "pcl-export.json",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"{}").unwrap();
    drop(writer.finish().unwrap());
    assert!(fs::metadata(&fixture.path).unwrap().len() > ARCHIVE_BYTES);
    assert_eq!(classify(&fixture.path).unwrap(), Format::Pcl);
    assert_eq!(Format::ReadyGame.as_str(), "ready_game");
}

#[test]
fn wrapped_mrpack_ignores_only_bounded_known_foreign_launcher_metadata() {
    let fixture = Fixture::new();
    let index = json!({"formatVersion":1,"game":"minecraft","name":"Synthetic pack","versionId":"1","dependencies":{"minecraft":"1.20.1"},"files":[]}).to_string();
    for prefix in ["", "Wrapped/"] {
        let marker = format!("{prefix}modrinth.index.json");
        let config = format!("{prefix}overrides/PCL/config.json");
        let setup = format!("{prefix}overrides/PCL/Setup.ini");
        let directory = format!("{prefix}overrides/PCL/");
        fixture.zip(&[
            (&marker, index.as_bytes()),
            (&directory, b""),
            (&config, br#"{"InstanceMigratedJava":true}"#),
            (&setup, b"State:6\r\nInfo:Synthetic pack\r\n"),
        ]);
        let prepared = fixture.prepared(Format::Modrinth).unwrap();
        assert!(prepared.outputs.is_empty());
        assert!(!prepared.directories.contains("PCL"));
        assert!(prepared
            .warnings
            .iter()
            .any(|value| value.contains("2 个已识别的 PCL")));
        fixture.zip(&[
            (&marker, index.as_bytes()),
            (
                &config,
                br#"{"InstanceMigratedJava":true,"UnknownSetting":true}"#,
            ),
        ]);
        assert!(fixture.prepared(Format::Modrinth).is_err());
    }
}
