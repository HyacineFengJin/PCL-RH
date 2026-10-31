//! Cross-provider desktop integration cases, retained one-for-one outside the
//! native resource-only wrapper. These exercise actual rename/delete/reset.
use crate::resource_ops::update_batch::{tests::Fixture, HISTORY};
use crate::resource_ops::{restore_update, updates_history};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    sync::atomic::AtomicBool,
};

#[test]
fn actual_rename_delete_and_restore_services_carry_update_history() {
    let f = Fixture::new(true, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let update_id = f.run(&mut files, &[r]).undo_id.unwrap();
    fs::write(f.root.join("versions/test/test.json"),br#"{"id":"test","jar":"test","mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","libraries":[]}"#).unwrap();
    fs::write(f.root.join("versions/test/test.jar"), b"client jar").unwrap();
    let project = f.source.join("project");
    fs::create_dir_all(project.join(".pcl-rust")).unwrap();
    fs::write(project.join(".pcl-rust/settings.json"),serde_json::to_vec(&serde_json::json!({"schema_version":3,"active_root_id":"root-fixture","player":"Player","memory_gib":6,"java":{"mode":"auto"},"java_paths":[],"roots":[{"id":"root-fixture","name":"Game","path":f.root,"selected":"test","overrides":{},"java_overrides":{}}]})).unwrap()).unwrap();
    crate::resource_ops::ensure_ready(&f.root).unwrap();
    let rename = crate::instance_rename::prepare(&f.root, "test", "renamed").unwrap();
    let refs =
        crate::instance_rename_refs::prepare(&project, "root-fixture", &f.root, "test", "renamed")
            .unwrap();
    crate::instance_rename::execute(
        &f.root,
        &project,
        rename,
        refs,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        updates_history(&f.root, "renamed").unwrap()[0].id,
        update_id
    );
    let deletion =
        crate::instance_delete::prepare(&f.root, &project, "root-fixture", "renamed").unwrap();
    crate::instance_delete::execute(&f.root, &project, deletion, &AtomicBool::new(false), |_| {})
        .unwrap();
    assert!(!f.root.join("versions/renamed").exists());
    let entry = crate::instance_delete::history(&f.root, &project, "root-fixture")
        .unwrap()
        .remove(0);
    crate::instance_delete::undo(
        &f.root,
        &project,
        "root-fixture",
        &entry.operation_id,
        entry.revision.as_ref().unwrap(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        updates_history(&f.root, "renamed").unwrap()[0].id,
        update_id
    );
    restore_update(
        &f.root,
        "renamed",
        &update_id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    assert!(f.root.join("versions/renamed/mods/old.jar").exists());
    f.clean();
}
#[test]
fn actual_export_omits_private_history_and_reset_prepare_keeps_it() {
    let f = Fixture::new(true, true);
    let r = f.old("mods", "old.jar", b"old");
    let mut files = vec![f.file("mods", &r.new_file_name, b"new")];
    let id = f.run(&mut files, &[r]).undo_id.unwrap();
    fs::write(f.root.join("versions/test/test.json"),br#"{"id":"test","jar":"test","mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","libraries":[]}"#).unwrap();
    fs::write(f.root.join("versions/test/test.jar"), b"client jar").unwrap();
    let history_path = f
        .root
        .join("versions/test")
        .join(HISTORY)
        .join(&id)
        .join("journal.json");
    let before = fs::read(&history_path).unwrap();
    let request = crate::instance_export::ExportRequest {
        name: "Fixture".into(),
        version: "1".into(),
        checks: crate::instance_export::ExportChecks {
            other: true,
            ..Default::default()
        },
        excluded: BTreeMap::new(),
    };
    let plan = crate::instance_export::prepare(&f.root, "test", request).unwrap();
    let destination = f.source.join("export.zip");
    crate::instance_export::execute(plan, &destination, &AtomicBool::new(false), |_| {}).unwrap();
    let mut zip = zip::ZipArchive::new(File::open(destination).unwrap()).unwrap();
    let mut has_mod = false;
    for index in 0..zip.len() {
        let item = zip.by_index(index).unwrap();
        assert!(!item.name().contains(HISTORY));
        if item.name().ends_with("mods/next-old.jar") {
            has_mod = true;
        }
    }
    assert!(has_mod);
    crate::instance_reset::prepare(&f.root, "test", vec![]).unwrap();
    assert_eq!(fs::read(history_path).unwrap(), before);
    restore_update(
        &f.root,
        "test",
        &id,
        &AtomicBool::new(false),
        &mut || Ok(()),
    )
    .unwrap();
    f.clean();
}
