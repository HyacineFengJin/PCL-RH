//! The test batch traverses the real planner, HTTP body verification and
//! anonymous descriptor handoff. Only metadata authority is replaced by a
//! fixture provider. The same batch also exercises application command gates.
use super::super::{
    provider::{ApiFile, Hashes},
    tests::{dependency, run, server, sha, FakeProvider},
};
use super::*;
use std::{fs, io::Write, time::Duration};

fn archive(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(name, zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap().into_inner()
}

/// Creates only generic fixture resources. Call before admission so the plan
/// snapshots both old inodes; staging must not mutate them before commit.
pub(crate) fn test_update_batch(
    root: &Path,
    project: &Path,
    root_id: &str,
    id: &str,
    isolation: pcl_core::content_scope::IsolationPolicy,
    settings_revision: &str,
) -> VerifiedUpdateBatch {
    let instance = root.join("versions").join(id);
    fs::create_dir_all(instance.join("mods")).unwrap();
    fs::create_dir_all(project.join(".pcl-rust")).unwrap();
    fs::write(
        instance.join(format!("{id}.json")),
        serde_json::to_vec(&serde_json::json!({
            "id": id, "clientVersion": "1.20.1",
            "libraries": [{"name": "net.fabricmc:fabric-loader:0.16.0"}],
        }))
        .unwrap(),
    )
    .unwrap();
    let old = archive(
        "fabric.mod.json",
        br#"{"schemaVersion":1,"id":"sample","version":"1"}"#,
    );
    let new = archive(
        "fabric.mod.json",
        br#"{"schemaVersion":1,"id":"sample","version":"2"}"#,
    );
    let old_disabled = archive(
        "fabric.mod.json",
        br#"{"schemaVersion":1,"id":"optional","version":"1"}"#,
    );
    let new_disabled = archive(
        "fabric.mod.json",
        br#"{"schemaVersion":1,"id":"optional","version":"2"}"#,
    );
    let dep = archive(
        "fabric.mod.json",
        br#"{"schemaVersion":1,"id":"dependency","version":"1"}"#,
    );
    let pack = archive(
        "pack.mcmeta",
        br#"{"pack":{"pack_format":15,"description":"sample"}}"#,
    );
    fs::write(instance.join("mods/old.jar"), &old).unwrap();
    fs::write(
        instance.join("mods/old-disabled.jar.disabled"),
        &old_disabled,
    )
    .unwrap();
    fs::write(instance.join("mods/custom-dep.jar"), &dep).unwrap();
    let mut fake = FakeProvider::default();
    for (project_id, old_id, new_id, old_name, new_name, before, after) in [
        (
            "Proj0001", "Vers0001", "Vers0002", "old.jar", "new.jar", &old, &new,
        ),
        (
            "Proj0002",
            "Vers0003",
            "Vers0004",
            "old-disabled.jar",
            "new-disabled.jar",
            &old_disabled,
            &new_disabled,
        ),
    ] {
        let version = fake.add(project_id, old_id, "mod", old_name, before);
        version.date_published = "2026-01-01T00:00:00Z".into();
        let version = version.clone();
        fake.hashes.insert(sha(before), version);
        fake.add(project_id, new_id, "mod", new_name, after)
            .date_published = "2026-02-01T00:00:00Z".into();
    }
    let version = fake
        .add("Proj0003", "Vers0005", "mod", "dependency.jar", &dep)
        .clone();
    fake.hashes.insert(sha(&dep), version);
    let version = fake.versions.get_mut("Vers0002").unwrap();
    version
        .dependencies
        .push(dependency(Some("Proj0003"), None, "required"));
    version.files.push(ApiFile {
        filename: "required.zip".into(),
        size: pack.len() as u64,
        url: "https://cdn.modrinth.com/required.zip".into(),
        primary: false,
        hashes: Hashes {
            sha512: sha(&pack),
            sha1: None,
        },
        file_type: Some("required-resource-pack".into()),
    });
    let selected = ["old.jar", "old-disabled.jar.disabled"]
        .into_iter()
        .map(|name| ResourceFile {
            file_name: name.into(),
            fingerprint: crate::resource_ops::fingerprint(&instance.join("mods").join(name))
                .unwrap(),
        })
        .collect();
    let cancel = AtomicBool::new(false);
    let target = target::capture_with_isolation(
        root,
        project,
        root_id,
        id,
        &cancel,
        isolation,
        settings_revision,
    )
    .unwrap();
    let plan = run(prepare_with(&fake, target, selected, &cancel)).unwrap();
    let http = provider::HttpProvider::new(&cancel).unwrap();
    let staging = target::Dir::open(project)
        .unwrap()
        .ensure(".pcl-rust")
        .unwrap()
        .ensure("resource-downloads")
        .unwrap()
        .with_input_sources()
        .unwrap();
    let mut files = Vec::new();
    for item in plan.transfer.files.iter().filter(|item| !item.reused) {
        let body = match item.file_name.as_str() {
            "new.jar" => &new,
            "new-disabled.jar.disabled" => &new_disabled,
            "required.zip" => &pack,
            _ => panic!("unexpected fixture resource"),
        };
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let (url, worker) = server(&response, Duration::ZERO);
        let mut file = staging.allocate(item.size).unwrap();
        run(transfer::response_into_file(
            &http,
            http.client.get(url),
            item.size,
            &item.sha512,
            &mut file,
            &cancel,
            |_| {},
        ))
        .unwrap();
        worker.join().unwrap();
        super::super::test_support::assert_source(&file);
        files.push(transfer::VerifiedFile {
            kind: item.kind.clone(),
            file_name: item.file_name.clone(),
            size: item.size,
            sha512: item.sha512.clone(),
            file,
        });
    }
    let network_bytes = http.network_bytes.load(Ordering::Relaxed);
    assert_eq!(network_bytes, plan.download_bytes);
    recheck_update_target(&plan, &cancel).unwrap();
    VerifiedUpdateBatch {
        plan,
        files,
        network_bytes,
    }
}

#[test]
fn anonymous_mixed_kind_updates_and_undo_preserve_disabled_inodes_and_unrelated_content() {
    use crate::resource_ops::{self, Replacement, VerifiedImport};
    let fixture = super::tests::Fixture::new();
    let instance = fixture.root.join("versions/sample");
    fs::write(instance.join("options.txt"), b"preserved options").unwrap();
    fs::create_dir_all(fixture.root.join("libraries/shared")).unwrap();
    fs::write(
        fixture.root.join("libraries/shared/cache.jar"),
        b"preserved cache",
    )
    .unwrap();
    let batch = test_update_batch(
        &fixture.root,
        &fixture.project,
        "root-fixture",
        "sample",
        pcl_core::content_scope::IsolationPolicy::Automatic,
        "provider-fixture-revision",
    );
    assert_eq!(batch.plan.replacements.len(), 2);
    assert_eq!(batch.plan.adds.len(), 1);
    assert_eq!(batch.plan.reuse.len(), 1);
    let originals: Vec<_> = ["old.jar", "old-disabled.jar.disabled", "custom-dep.jar"]
        .into_iter()
        .map(|name| {
            let path = instance.join("mods").join(name);
            (
                name,
                fs::read(&path).unwrap(),
                super::super::test_support::identity(&path),
            )
        })
        .collect();
    let replacements: Vec<_> = batch
        .plan
        .replacements
        .iter()
        .map(|item| Replacement {
            kind: item.kind.clone(),
            old_file_name: item.old_file_name.clone(),
            old_fingerprint: item.old_fingerprint.clone(),
            old_sha512: item.old_sha512.clone(),
            new_file_name: item.new_file_name.clone(),
        })
        .collect();
    let mut files: Vec<_> = batch
        .files
        .into_iter()
        .map(|item| VerifiedImport {
            kind: item.kind,
            file_name: item.file_name,
            size: item.size,
            sha512: item.sha512,
            file: item.file,
        })
        .collect();
    let cancel = AtomicBool::new(false);
    let mut commits = 0;
    let result = resource_ops::update_verified_batch(
        &fixture.root,
        "sample",
        &mut files,
        &replacements,
        &cancel,
        &mut || {
            recheck_update_target(&batch.plan, &cancel)?;
            assert!(!instance.join("resourcepacks").exists());
            commits += 1;
            Ok(())
        },
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(commits, 1);
    assert_eq!(result.changed, 3);
    assert!(!instance.join("mods/old.jar").exists());
    assert!(!instance.join("mods/old-disabled.jar.disabled").exists());
    assert!(instance.join("mods/new.jar").exists());
    assert!(instance.join("mods/new-disabled.jar.disabled").exists());
    assert!(!instance.join("mods/new-disabled.jar").exists());
    for item in &batch.plan.transfer.files {
        let name = item
            .existing_file_name
            .as_deref()
            .unwrap_or(&item.file_name);
        assert_eq!(
            sha(&fs::read(instance.join(&item.kind).join(name)).unwrap()),
            item.sha512
        );
    }
    resource_ops::ensure_updates_ready(&fixture.root).unwrap();
    let history = resource_ops::updates_history(&fixture.root, "sample").unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, result.undo_id.unwrap());
    let restored = resource_ops::restore_update(
        &fixture.root,
        "sample",
        &history[0].id,
        &cancel,
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(restored.changed, 3);
    for (name, bytes, inode) in originals {
        let path = instance.join("mods").join(name);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(super::super::test_support::identity(path), inode);
    }
    assert!(!instance.join("mods/new.jar").exists());
    assert!(!instance.join("mods/new-disabled.jar.disabled").exists());
    assert!(!instance.join("resourcepacks").exists());
    assert_eq!(
        fs::read(instance.join("options.txt")).unwrap(),
        b"preserved options"
    );
    assert_eq!(
        fs::read(fixture.root.join("libraries/shared/cache.jar")).unwrap(),
        b"preserved cache"
    );
    resource_ops::retire_sources(&mut files).unwrap();
    super::super::test_support::assert_staging_path(&fixture.project, 0);
    assert!(resource_ops::updates_history(&fixture.root, "sample")
        .unwrap()
        .is_empty());
    resource_ops::ensure_updates_ready(&fixture.root).unwrap();
}
