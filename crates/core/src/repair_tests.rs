use super::*;
use crate::platform::{Architecture, OperatingSystem, Platform};
use serde_json::json;

fn descriptor(path: &str) -> Value {
    json!({"url": format!("https://libraries.minecraft.net/{path}"), "sha1": "1234567890123456789012345678901234567890", "size": 10, "path": path})
}
fn metadata(value: Value) -> RepairMetadata {
    resolve_metadata("fixture", |_| Ok(value.clone()), |_| Ok(false)).unwrap()
}
fn linux() -> RuleContext {
    RuleContext {
        platform: Platform {
            os: OperatingSystem::Linux,
            arch: Architecture::X86_64,
        },
        release: "6.12".into(),
        custom_resolution: false,
    }
}

#[test]
fn implicit_forge_files_are_reported_without_inventing_download_authority() {
    let mut value = json!({"id":"fixture","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
        "libraries":[],"arguments":{"game":["--launchTarget","forgeclient",
        "--fml.forgeGroup","net.minecraftforge","--fml.mcVersion","1.20.1",
        "--fml.forgeVersion","47.4.0","--fml.mcpVersion","20230612.114412"]}});
    let paths = crate::loader_runtime::required_paths(&value).unwrap();
    let files = required_for(&metadata(value.clone()), &linux())
        .unwrap()
        .files;
    assert!(paths.iter().all(
        |path| files.iter().any(|file| file.relative.as_ref() == Some(path)
            && file.download.is_none()
            && file.unsupported.is_some())
    ));
    let path = paths[0].strip_prefix("libraries/").unwrap();
    value["libraries"] = json!([{"name":"net.minecraftforge:fmlcore:1.20.1-47.4.0","downloads":{"artifact":descriptor(path)}}]);
    let files = required_for(&metadata(value), &linux()).unwrap().files;
    let matching: Vec<_> = files
        .iter()
        .filter(|file| file.relative.as_ref() == Some(&paths[0]))
        .collect();
    assert_eq!(matching.len(), 1);
    assert!(matching[0].download.is_some());
}

#[test]
fn runtime_download_records_cannot_add_unrelated_paths_or_ambiguous_authority() {
    let mut value = json!({"id":"fixture","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
        "libraries":[],"arguments":{"game":["--launchTarget","forgeclient",
        "--fml.forgeGroup","net.minecraftforge","--fml.mcVersion","1.20.1",
        "--fml.forgeVersion","47.4.0","--fml.mcpVersion","20230612.114412"]}});
    let path = implicit_runtime_paths(&value).unwrap().remove(0);
    let mut record = descriptor(&path);
    record["path"] = json!(path);
    value["pclRuntimeFiles"] = json!([record.clone()]);
    let files = required_for(&metadata(value.clone()), &linux())
        .unwrap()
        .files;
    assert!(files
        .iter()
        .any(|file| file.relative.as_ref() == Some(&path) && file.download.is_some()));
    for rows in [
        json!([record.clone(), record.clone()]),
        json!([{"path":"mods/unrelated.jar","url":"https://libraries.minecraft.net/a","sha1":"a".repeat(40),"size":1}]),
        json!([{"path":path,"shell":"forbidden"}]),
        json!("invalid record"),
    ] {
        value["pclRuntimeFiles"] = rows;
        assert!(required_for(&metadata(value.clone()), &linux()).is_err());
    }
    record["sha1"] = json!("unverified");
    value["pclRuntimeFiles"] = json!([record]);
    let files = required_for(&metadata(value), &linux()).unwrap().files;
    assert!(files
        .iter()
        .any(|file| file.relative.as_ref() == Some(&path)
            && file.download.is_none()
            && file.unsupported.is_some()));
}

#[test]
fn inherited_jar_uses_owner_descriptor_and_launch_library_overrides() {
    let base = json!({"id":"base","downloads":{"client":descriptor("parent-client")},"libraries":[{"name":"org.example:shared:1","downloads":{"artifact":descriptor("old.jar")}}]});
    let child = json!({"id":"child","inheritsFrom":"base","downloads":{"client":descriptor("child-client")},"libraries":[{"name":"org.example:shared:2","downloads":{"artifact":descriptor("new.jar")}}]});
    let resolved = resolve_metadata(
        "child",
        |id| {
            Ok(if id == "base" {
                base.clone()
            } else {
                child.clone()
            })
        },
        |_| Ok(false),
    )
    .unwrap();
    let files = required_for(&resolved, &linux()).unwrap().files;
    assert!(files
        .iter()
        .any(|file| file.relative.as_deref() == Some("libraries/new.jar")));
    assert!(!files
        .iter()
        .any(|file| file.relative.as_deref() == Some("libraries/old.jar")));
    let jar = files
        .iter()
        .find(|file| file.relative.as_deref() == Some("versions/base/base.jar"))
        .unwrap();
    assert!(jar
        .download
        .as_ref()
        .unwrap()
        .url
        .ends_with("parent-client"));
    let own = resolve_metadata(
        "child",
        |id| {
            Ok(if id == "base" {
                base.clone()
            } else {
                child.clone()
            })
        },
        |_| Ok(true),
    )
    .unwrap();
    assert!(required_for(&own, &linux())
        .unwrap()
        .files
        .iter()
        .any(|file| file.relative.as_deref() == Some("versions/child/child.jar")));
}

#[test]
fn platform_rules_select_matching_artifacts_and_legacy_native_classifier() {
    let resolved = metadata(json!({"id":"fixture","libraries":[
        {"name":"org.lwjgl:lwjgl:3:natives-windows","downloads":{"artifact":descriptor("windows.jar")}},
        {"name":"org.lwjgl:lwjgl:3:natives-linux","downloads":{"artifact":descriptor("linux.jar")}},
        {"name":"org.example:conditional:1","rules":[{"action":"allow","os":{"name":"windows"}}],"downloads":{"artifact":descriptor("conditional.jar")}},
        {"name":"org.example:old:1","natives":{"linux":"natives-linux-${arch}"},"downloads":{"artifact":descriptor("normal.jar"),"classifiers":{"natives-linux-64":descriptor("native.jar"),"natives-linux-32":descriptor("wrong.jar")}}}
    ]}));
    let files = required_for(&resolved, &linux()).unwrap().files;
    let paths: Vec<_> = files
        .iter()
        .filter_map(|file| file.relative.as_deref())
        .collect();
    assert!(paths.contains(&"libraries/linux.jar"));
    assert!(paths.contains(&"libraries/normal.jar"));
    assert!(paths.contains(&"libraries/native.jar"));
    assert!(!paths.contains(&"libraries/windows.jar"));
    assert!(!paths.contains(&"libraries/conditional.jar"));
    assert!(!paths.contains(&"libraries/wrong.jar"));
}

#[test]
fn unsafe_library_destinations_and_incomplete_descriptors_are_explicit() {
    let resolved = metadata(json!({"id":"fixture","libraries":[
        {"name":"org.example:traversal:1","downloads":{"artifact":descriptor("../mods/target.jar")}},
        {"name":"org.example:absolute:1","downloads":{"artifact":descriptor("/tmp/target.jar")}},
        {"name":"org.example:custom:1"},
        {"name":"org.example:unknown-size:1","downloads":{"artifact":{"path":"no-size.jar","url":"https://libraries.minecraft.net/a","sha1":"1234567890123456789012345678901234567890"}}}
    ]}));
    let files = required_for(&resolved, &linux()).unwrap().files;
    assert_eq!(
        files.iter().filter(|file| file.relative.is_none()).count(),
        2
    );
    assert!(files.iter().any(|file| file.label.ends_with("no-size.jar")
        && file.unsupported.as_deref() == Some("缺少明确文件大小")));
    assert!(files
        .iter()
        .any(|file| file.label.contains("custom") && file.download.is_none()));
    for unsafe_path in [
        "../mods/a",
        "/root/a",
        "libraries/a\\b",
        "libraries/a\0b",
        "libraries/a/../../world/a",
    ] {
        assert!(relative_path(unsafe_path).is_err());
    }
}

#[test]
fn asset_names_never_authorize_destinations_and_sizes_remain_required() {
    let hash = "1234567890123456789012345678901234567890";
    let files = asset_objects(&json!({"objects":{
        "../../world/user.dat":{"hash":hash,"size":10},
        "invalid":{"hash":"../../world","size":10},
        "unknown-size":{"hash":"abcdefabcdefabcdefabcdefabcdefabcdefabcd"}
    }}))
    .unwrap();
    assert!(files
        .iter()
        .any(|file| file.relative.as_deref() == Some(&format!("assets/objects/12/{hash}"))));
    assert!(files
        .iter()
        .any(|file| file.relative.is_none() && file.unsupported.is_some()));
    assert!(files.iter().any(|file| file
        .relative
        .as_deref()
        .is_some_and(|s| s.ends_with("abcdefabcdefabcdefabcdefabcdefabcdefabcd"))
        && file.download.is_none()));
    assert!(asset_objects(&json!({"objects":[]})).is_err());
}

#[test]
fn contradictory_duplicate_destinations_and_byte_budgets_fail_closed() {
    let a = required(
        "libraries/shared.jar".into(),
        &descriptor("a"),
        MAX_REPAIR_FILE_BYTES,
    );
    let mut b = a.clone();
    b.download.as_mut().unwrap().sha1 = "abcdefabcdefabcdefabcdefabcdefabcdefabcd".into();
    let files = deduplicate(vec![a, b]).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0].unsupported.as_deref(),
        Some("同一路径存在相互冲突的下载声明")
    );
    assert!(files[0].download.is_none());
    let files: Vec<_> = (0..33)
        .map(|i| {
            let mut descriptor = descriptor("large");
            descriptor["size"] = MAX_REPAIR_FILE_BYTES.into();
            required(
                format!("libraries/{i}.jar"),
                &descriptor,
                MAX_REPAIR_FILE_BYTES,
            )
        })
        .collect();
    assert!(deduplicate(files).unwrap_err().contains("总大小"));
    let oversized = required(
        "libraries/oversized.jar".into(),
        &json!({"url":"https://libraries.minecraft.net/a","sha1":"1234567890123456789012345678901234567890","size":MAX_REPAIR_FILE_BYTES+1}),
        MAX_REPAIR_FILE_BYTES,
    );
    assert!(oversized.download.is_none());
}

#[test]
fn inherited_cycle_is_rejected_without_reads_beyond_the_cycle() {
    assert!(resolve_metadata(
        "fixture",
        |_| Ok(json!({"id":"fixture","inheritsFrom":"fixture"})),
        |_| Ok(false)
    )
    .err()
    .unwrap()
    .contains("cycle"));
}
