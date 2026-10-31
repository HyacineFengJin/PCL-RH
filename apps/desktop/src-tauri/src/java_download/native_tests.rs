use super::{
    catalog::{Download, Entry, Manifest, Package},
    files, Store,
};
use pcl_core::platform::{Architecture, OperatingSystem, Platform};
use sha1::{Digest, Sha1};
use std::{collections::BTreeMap, fs, io::Write};

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha1::digest(bytes))
}
fn entry(bytes: &[u8], executable: bool) -> Entry {
    let sha1 = hash(bytes);
    Entry::File {
        executable,
        downloads: BTreeMap::from([(
            "raw".into(),
            Download {
                url: format!("https://piston-data.mojang.com/v1/objects/{sha1}/java"),
                sha1,
                size: bytes.len() as u64,
            },
        )]),
    }
}
fn plan() -> Manifest {
    let platform = Platform::current().unwrap();
    let path = if platform.os == OperatingSystem::MacOs {
        "jre.bundle/Contents/Home/bin/java".into()
    } else {
        format!("bin/{}", platform.java_executable())
    };
    let mut files = BTreeMap::new();
    let parts: Vec<&str> = path.split('/').collect();
    for i in 1..parts.len() {
        files.insert(parts[..i].join("/"), Entry::Directory);
    }
    files.insert(
        path,
        entry(b"fixture runtime", platform.os != OperatingSystem::Windows),
    );
    Manifest { files }
}
fn package(manifest: &Manifest) -> Package {
    let raw = serde_json::to_vec(manifest).unwrap();
    let sha1 = hash(&raw);
    Package {
        component: "java-runtime-delta".into(),
        version: "21".into(),
        major: 21,
        platform: super::catalog::platform().unwrap().into(),
        sha1: sha1.clone(),
        released: "2026-01-01".into(),
        manifest: Download {
            url: format!("https://piston-meta.mojang.com/v1/packages/{sha1}/manifest.json"),
            sha1,
            size: raw.len() as u64,
        },
    }
}
fn fixture() -> (tempfile::TempDir, Store) {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().canonicalize().unwrap();
    let store = Store::capture(&path).unwrap();
    (fixture, store)
}
fn populate(stage: &files::Stage<'_>, manifest: &Manifest) {
    stage.prepare_directories().unwrap();
    for (path, entry) in &manifest.files {
        if let Entry::File { executable, .. } = entry {
            let mut file = stage.download(path).unwrap();
            files::download_handle(&mut file)
                .write_all(b"fixture runtime")
                .unwrap();
            stage.finish_download(path, file, *executable).unwrap();
        }
    }
}
#[test]
fn catalog_uses_native_executable_and_bundle_candidates() {
    for (os, path, executable) in [
        (OperatingSystem::Windows, "bin/java.exe", false),
        (OperatingSystem::Linux, "bin/java", true),
        (
            OperatingSystem::MacOs,
            "jre.bundle/Contents/Home/bin/java",
            true,
        ),
    ] {
        let manifest = Manifest {
            files: BTreeMap::from([(path.into(), entry(b"fixture runtime", executable))]),
        };
        assert_eq!(
            manifest
                .java_entry_for(Platform {
                    os,
                    arch: Architecture::X86_64
                })
                .unwrap(),
            path
        );
        let wrong = Manifest {
            files: BTreeMap::from([(
                "bin/javaw.exe".into(),
                entry(b"fixture runtime", executable),
            )]),
        };
        assert!(wrong
            .java_entry_for(Platform {
                os,
                arch: Architecture::X86_64
            })
            .is_err());
    }
}
#[test]
fn checked_native_payload_publishes_complete_runtime_without_replacement() {
    let (_fixture, store) = fixture();
    let manifest = plan();
    manifest.validate().unwrap();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let mut stage = store.create(&package, &manifest, "publish", &lock).unwrap();
    populate(&stage, &manifest);
    stage.verify_complete().unwrap();
    let path = stage.publish().unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"fixture runtime");
    let root = store
        .path
        .join(format!("{}-{}", package.component, package.sha1));
    assert!(!root.join(".pcl-java-sources").exists());
    assert!(!store.path.join(".stage-publish").exists());
    assert!(store
        .create(&package, &manifest, "collision", &lock)
        .is_err());
    assert_eq!(fs::read(path).unwrap(), b"fixture runtime");
}
#[test]
fn dropping_partial_download_seals_actual_bytes_before_cancel_cleanup() {
    let (_fixture, store) = fixture();
    let manifest = plan();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let stage = store.create(&package, &manifest, "cancel", &lock).unwrap();
    stage.prepare_directories().unwrap();
    let path = manifest.java_entry().unwrap();
    let mut file = stage.download(&path).unwrap();
    files::download_handle(&mut file)
        .write_all(b"partial")
        .unwrap();
    drop(file);
    stage.cleanup().unwrap();
    assert!(!store.path.join(".stage-cancel").exists());
}
#[test]
fn reopening_store_recovers_sealed_partial_sources_and_original_owner_schema() {
    let (fixture, store) = fixture();
    let manifest = plan();
    let package = package(&manifest);
    {
        let lock = store.dir.lock(".java-install.lock").unwrap();
        let stage = store.create(&package, &manifest, "recover", &lock).unwrap();
        stage.prepare_directories().unwrap();
        let mut file = stage.download(&manifest.java_entry().unwrap()).unwrap();
        files::download_handle(&mut file)
            .write_all(b"partial")
            .unwrap();
        drop(file);
    }
    drop(store);
    let store = Store::capture(&fixture.path().canonicalize().unwrap()).unwrap();
    let lock = store.dir.lock(".java-install.lock").unwrap();
    store.recover(&lock).unwrap();
    assert!(!store.path.join(".stage-recover").exists());
    #[cfg(unix)]
    {
        // Linux's original schema 1 owner has the exact same tuple identity;
        // no named source area is silently granted to this old schema.
        let stage = store.create(&package, &manifest, "legacy", &lock).unwrap();
        populate(&stage, &manifest);
        stage.check_sources_for_fixture();
        let marker = store.path.join(".stage-legacy/.pcl-java-owner.json");
        let mut owner: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        owner["schema"] = 1.into();
        fs::write(&marker, serde_json::to_vec(&owner).unwrap()).unwrap();
        drop(stage);
        store.recover(&lock).unwrap();
        assert!(!store.path.join(".stage-legacy").exists());
    }
}
#[test]
fn modified_published_payload_is_retained_before_any_tree_cleanup() {
    let (_fixture, store) = fixture();
    let manifest = plan();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let stage = store
        .create(&package, &manifest, "modified", &lock)
        .unwrap();
    populate(&stage, &manifest);
    let path = store
        .path
        .join(".stage-modified")
        .join(manifest.java_entry().unwrap());
    fs::write(&path, b"modified by fixture").unwrap();
    assert!(stage.cleanup().is_err());
    assert_eq!(fs::read(path).unwrap(), b"modified by fixture");
    assert!(store
        .path
        .join(".stage-modified/.pcl-java-owner.json")
        .exists());
}
#[test]
fn final_target_collision_keeps_both_runtime_trees() {
    let (_fixture, store) = fixture();
    let manifest = plan();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let mut stage = store
        .create(&package, &manifest, "collision", &lock)
        .unwrap();
    populate(&stage, &manifest);
    let target = store
        .path
        .join(format!("{}-{}", package.component, package.sha1));
    fs::create_dir(&target).unwrap();
    fs::write(target.join("original"), b"original").unwrap();
    assert!(stage.publish().is_err());
    assert_eq!(fs::read(target.join("original")).unwrap(), b"original");
    assert!(store.path.join(".stage-collision").exists());
}
#[cfg(not(target_os = "linux"))]
#[test]
fn unsealed_or_unknown_source_is_retained_after_interrupted_download() {
    let (_fixture, store) = fixture();
    let manifest = plan();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let stage = store
        .create(&package, &manifest, "unsealed", &lock)
        .unwrap();
    stage.prepare_directories().unwrap();
    let mut file = stage.download(&manifest.java_entry().unwrap()).unwrap();
    files::download_handle(&mut file)
        .write_all(b"longer than the manifest's byte limit")
        .unwrap();
    drop(file); // sealing fails; an unknown named source remains, never deleted
    assert!(stage.cleanup().is_err());
    assert!(store
        .path
        .join(".stage-unsealed/.pcl-java-sources")
        .exists());
}

#[cfg(unix)]
#[test]
fn native_links_keep_manifest_targets_and_modified_links_block_cleanup() {
    let (_fixture, store) = fixture();
    let mut manifest = plan();
    let executable = manifest.java_entry().unwrap();
    let parent = executable.rsplit_once('/').unwrap().0;
    let alias = format!("{parent}/java-alias");
    manifest.files.insert(
        alias.clone(),
        Entry::Link {
            target: "java".into(),
        },
    );
    manifest.validate().unwrap();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let stage = store.create(&package, &manifest, "links", &lock).unwrap();
    populate(&stage, &manifest);
    stage.links().unwrap();
    stage.verify_complete().unwrap();
    let root = store.path.join(".stage-links");
    let link = root.join(&alias);
    assert_eq!(fs::read_link(&link).unwrap(), std::path::Path::new("java"));
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("../../outside", &link).unwrap();
    assert!(stage.cleanup().is_err());
    assert!(root.join(executable).exists());
    assert_eq!(
        fs::read_link(link).unwrap(),
        std::path::Path::new("../../outside")
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn modified_sealed_partial_source_is_retained_with_all_owner_records() {
    let (_fixture, store) = fixture();
    let manifest = plan();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let stage = store
        .create(&package, &manifest, "partial-modified", &lock)
        .unwrap();
    stage.prepare_directories().unwrap();
    let mut file = stage.download(&manifest.java_entry().unwrap()).unwrap();
    files::download_handle(&mut file)
        .write_all(b"partial")
        .unwrap();
    drop(file);
    let sources = store.path.join(".stage-partial-modified/.pcl-java-sources");
    let source = fs::read_dir(&sources)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_str().unwrap().ends_with(".lease"))
        .unwrap();
    fs::write(&source, b"changed").unwrap();
    assert!(stage.cleanup().is_err());
    assert_eq!(fs::read(source).unwrap(), b"changed");
    assert_eq!(fs::read_dir(sources).unwrap().count(), 2);
}

#[test]
fn unknown_stage_entry_blocks_cleanup_before_deleting_verified_payload() {
    let (_fixture, store) = fixture();
    let manifest = plan();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let stage = store.create(&package, &manifest, "unknown", &lock).unwrap();
    populate(&stage, &manifest);
    let root = store.path.join(".stage-unknown");
    fs::write(root.join("unknown"), b"keep").unwrap();
    assert!(stage.cleanup().is_err());
    assert_eq!(
        fs::read(root.join(manifest.java_entry().unwrap())).unwrap(),
        b"fixture runtime"
    );
    assert_eq!(fs::read(root.join("unknown")).unwrap(), b"keep");
}

#[test]
fn native_binding_observes_without_creating_then_preserves_the_captured_anchor() {
    let fixture = tempfile::tempdir().unwrap();
    let project = fixture.path().canonicalize().unwrap();
    let first = super::StoreBinding::capture(&project).unwrap();
    let waiting = super::StoreBinding::capture(&project).unwrap();
    assert!(!project.join(".pcl-rust").exists());
    first.verify().unwrap();
    let store = first.open().unwrap();
    assert!(store.path.is_dir());
    assert_eq!(fs::read_dir(&store.path).unwrap().count(), 0);
    assert!(first.verify().is_err());
    let next = waiting.open().unwrap();
    assert_eq!(store.dir.identity().unwrap(), next.dir.identity().unwrap());
    assert!(!store.path.join(".java-install.lock").exists());
}

#[cfg(windows)]
#[test]
fn windows_manifest_rejects_aliases_stream_names_and_links_before_staging() {
    for name in ["bin/java.exe:stream", "bin/NUL", "bin/java.exe."] {
        let mut manifest = plan();
        manifest
            .files
            .insert(name.into(), entry(b"fixture runtime", false));
        assert!(manifest.validate().is_err());
    }
    let mut manifest = plan();
    manifest
        .files
        .insert("bin/JAVA.EXE".into(), entry(b"fixture runtime", false));
    assert!(manifest.validate().is_err());
    let mut manifest = plan();
    manifest.files.insert(
        "bin/alias".into(),
        Entry::Link {
            target: "java.exe".into(),
        },
    );
    assert!(manifest.validate().is_err());
}

#[test]
fn maximum_manifest_with_root_owner_and_source_area_verifies_and_cleans_up() {
    use pcl_core::retained_fs::{Directory, Leaf};
    let (_fixture, store) = fixture();
    let executable = format!("bin/{}", Platform::current().unwrap().java_executable());
    let mut manifest = Manifest {
        files: BTreeMap::from([
            ("bin".into(), Entry::Directory),
            (
                executable.clone(),
                entry(b"fixture runtime", !cfg!(windows)),
            ),
        ]),
    };
    for index in 0..4094 {
        manifest
            .files
            .insert(format!("file-{index:04}"), entry(b"", false));
    }
    assert_eq!(manifest.files.len(), 4096);
    manifest.validate().unwrap();
    let package = package(&manifest);
    let lock = store.dir.lock(".java-install.lock").unwrap();
    let stage = store.create(&package, &manifest, "maximum", &lock).unwrap();
    stage.prepare_directories().unwrap();
    let root = Directory::from_file(stage.dir.0.try_clone().unwrap()).unwrap();
    for index in 0..4094 {
        // Native creation matters on elevated Windows runners: ordinary stdfs
        // fixtures can choose BuiltinAdministrators rather than TokenUser owner.
        root.create_file(&Leaf::new(format!("file-{index:04}")).unwrap())
            .unwrap();
    }
    let mut file = stage.download(&executable).unwrap();
    files::download_handle(&mut file)
        .write_all(b"fixture runtime")
        .unwrap();
    stage
        .finish_download(&executable, file, !cfg!(windows))
        .unwrap();
    assert_eq!(root.names(4098).unwrap().len(), 4097);
    stage.verify_complete().unwrap();
    // The larger root scan is only an internal-entry budget. A real extra
    // business file must still fail before the cleanup deletes any payload.
    let unknown = Leaf::new("unknown-extra").unwrap();
    root.create_file(&unknown).unwrap();
    assert!(stage.verify_complete().is_err());
    assert!(stage.cleanup().is_err());
    assert!(store.path.join(".stage-maximum/file-0000").exists());
    assert!(store.path.join(".stage-maximum/.pcl-java-sources").exists());
    let stamp = root.entry(&unknown).unwrap().unwrap();
    assert!(matches!(
        root.remove_workspace_entry(&unknown, &stamp).unwrap(),
        pcl_core::retained_fs::WorkspaceRemoval::Removed
    ));
    stage.verify_complete().unwrap();
    stage.cleanup().unwrap();
    assert!(!store.path.join(".stage-maximum").exists());
}

#[test]
fn cleanup_never_reauthorizes_replacement_payload_owner_or_directory() {
    use pcl_core::retained_fs::{Directory, Leaf, Traversal};
    for case in 0..3 {
        let (_fixture, store) = fixture();
        let manifest = plan();
        let package = package(&manifest);
        let lock = store.dir.lock(".java-install.lock").unwrap();
        let stage = store
            .create(&package, &manifest, "replace-leaf", &lock)
            .unwrap();
        populate(&stage, &manifest);
        let java = manifest.java_entry().unwrap();
        let victim = match case {
            0 => java,
            1 => ".pcl-java-owner.json".into(),
            _ => "bin".into(),
        };
        // macOS's Java lives in a bundle; use its deepest manifest directory.
        let victim = if case == 2 {
            manifest
                .files
                .iter()
                .filter_map(|(path, entry)| matches!(entry, Entry::Directory).then_some(path))
                .max_by_key(|path| path.matches('/').count())
                .unwrap()
                .clone()
        } else {
            victim
        };
        let path = store.path.join(".stage-replace-leaf").join(&victim);
        let parked = path.with_file_name("original-parked-by-fixture");
        let mut replaced = false;
        let error = stage
            .cleanup_before_remove_for_fixture(|name| {
                if name != victim {
                    return;
                }
                replaced = true;
                fs::rename(&path, &parked).unwrap();
                let parent = Directory::open_absolute(path.parent().unwrap()).unwrap();
                let name = Leaf::new(path.file_name().unwrap()).unwrap();
                if case == 2 {
                    parent
                        .create_private(&name, Traversal::SameFilesystem)
                        .unwrap();
                } else {
                    parent
                        .create_file(&name)
                        .unwrap()
                        .write_all(b"external replacement")
                        .unwrap();
                }
            })
            .unwrap_err();
        assert!(replaced, "fixture did not reach cleanup boundary: {error}");
        assert!(parked.exists());
        if case == 2 {
            assert!(path.is_dir());
        } else {
            assert_eq!(fs::read(&path).unwrap(), b"external replacement");
        }
    }
}
