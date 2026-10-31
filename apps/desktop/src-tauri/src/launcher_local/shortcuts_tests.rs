use super::*;
use std::{fs, os::unix::fs::symlink};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = super::super::filesystem::fixture_path(&format!(
            "entry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(p.join("project")).unwrap();
        fs::write(p.join("project/start-native.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            p.join("project/start-native.sh"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        Self(p)
    }
    fn store(&self) -> ShortcutStore {
        ShortcutStore::new(
            self.0.join("project"),
            XdgPaths {
                applications: self.0.join("xdg/applications"),
                desktop: Some(self.0.join("Desktop")),
            },
        )
    }
    fn create(&self, target: ShortcutTarget) -> ShortcutStore {
        let store = self.store();
        let plan = store.prepare_create(target).unwrap();
        store.create(target, &plan.revision).unwrap();
        store
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Frozen pre-brand-fix bytes, independent of the current generation template.
fn historical_entry(store: &ShortcutStore, names: &str, owner: bool) -> Vec<u8> {
    let project = store.project.to_str().unwrap();
    let ownership = if owner {
        format!(
            "X-PCL-Linux-Owner=launcher-local-v1\nX-PCL-Linux-Project={}\n",
            store.project_key()
        )
    } else {
        String::new()
    };
    format!("[Desktop Entry]\nType=Application\nVersion=1.0\n{names}Comment=Launch existing Minecraft versions on Linux\nComment[zh_CN]=在 Linux 上启动已有的 Minecraft 版本\nExec=\"{project}/start-native.sh\"\nPath={project}\nIcon={project}/assets/pcl-linux.png\nTerminal=false\nCategories=Game;\nKeywords=Minecraft;PCL;Forge;\nStartupNotify=true\nStartupWMClass=pcl-desktop\n{ownership}").into_bytes()
}

#[test]
fn new_application_and_desktop_entries_use_the_shipped_brand_icon() {
    let f = Fixture::new();
    let icon = f.0.join("project/assets/pcl-rh.png");
    fs::create_dir(icon.parent().unwrap()).unwrap();
    let image = include_bytes!("../../../../../assets/pcl-rh.png");
    fs::write(&icon, image).unwrap();
    assert!(!f.0.join("project/assets/pcl-linux.png").exists());
    for target in [ShortcutTarget::Applications, ShortcutTarget::Desktop] {
        let store = f.create(target);
        let path = store.path(target).unwrap().join(NAME);
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("Name=PCL RH\nName[zh_CN]=PCL RH\n"));
        assert!(text.contains("Comment=Minecraft launcher\nComment[zh_CN]=Minecraft 启动器\n"));
        assert!(
            text.lines()
                .any(|line| line
                    == format!("Icon={}", desktop_value(icon.to_str().unwrap()).unwrap()))
        );
        assert!(!text.contains("pcl-linux.png"));
        assert!(text.contains("X-PCL-Linux-Owner=launcher-local-v1\n"));
        assert!(text.contains(&format!("X-PCL-Linux-Project={}\n", store.project_key())));
        assert_eq!(fs::read(&icon).unwrap(), image);
    }
}

#[test]
fn former_icon_entries_keep_exact_registration_and_persistent_undo() {
    for names in [CURRENT_NAMES, FORMER_NAMES] {
        for owner in [false, true] {
            let f = Fixture::new();
            let store = f.store();
            fs::create_dir_all(&store.paths.applications).unwrap();
            let path = store.paths.applications.join(NAME);
            let bytes = historical_entry(&store, names, owner);
            fs::write(&path, &bytes).unwrap();
            let plan = store.prepare_create(ShortcutTarget::Applications).unwrap();
            assert!(plan.entries[0].already_registered);
            store
                .create(ShortcutTarget::Applications, &plan.revision)
                .unwrap();
            assert_eq!(fs::read(&path).unwrap(), bytes);
            let plan = store.prepare_withdraw().unwrap();
            assert_eq!(plan.entries.len(), 1);
            store.withdraw(&plan.revision).unwrap();
            assert!(!path.exists());
            drop(store);
            let reopened = f.store();
            let rows = reopened.recovery().unwrap();
            assert_eq!(rows.len(), 1);
            assert!(rows[0].warnings.is_empty());
            reopened
                .restore(&rows[0].operation_id, &rows[0].revision)
                .unwrap();
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert!(reopened.recovery().unwrap().is_empty());
        }
    }
}

#[test]
fn brand_migration_never_claims_edited_templates_or_a_foreign_project() {
    let f = Fixture::new();
    let store = f.store();
    fs::create_dir_all(&store.paths.applications).unwrap();
    let path = store.paths.applications.join(NAME);
    let old = String::from_utf8(historical_entry(&store, CURRENT_NAMES, true)).unwrap();
    let foreign = ShortcutStore::new(f.0.join("foreign-project"), store.paths.clone());
    let mut variants = vec![
        old.replace("assets/pcl-linux.png", "assets/user-icon.png")
            .into_bytes(),
        old.replace(
            "Comment=Launch existing Minecraft versions on Linux",
            "Comment=My launcher",
        )
        .into_bytes(),
        (old.clone() + "X-Edited=true\n").into_bytes(),
        historical_entry(&foreign, CURRENT_NAMES, true),
        historical_entry(&foreign, FORMER_NAMES, false),
    ];
    let mut current = store.contents().unwrap();
    current.extend_from_slice(b"X-Edited=true\n");
    variants.push(current);
    for bytes in variants {
        fs::write(&path, &bytes).unwrap();
        assert!(store.prepare_create(ShortcutTarget::Applications).is_err());
        let plan = store.prepare_withdraw().unwrap();
        assert!(plan.entries.is_empty());
        assert_eq!(plan.warnings.len(), 1);
        assert!(store.withdraw(&plan.revision).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn edited_former_icon_recovery_copies_are_retained_without_reclaiming_the_entry() {
    let f = Fixture::new();
    let store = f.store();
    fs::create_dir_all(&store.paths.applications).unwrap();
    let path = store.paths.applications.join(NAME);
    fs::write(&path, historical_entry(&store, CURRENT_NAMES, true)).unwrap();
    let plan = store.prepare_withdraw().unwrap();
    let outcome = store.withdraw(&plan.revision).unwrap();
    let id = outcome.operation_ids[0].split_once(':').unwrap().1;
    let archive = store.paths.applications.join(TRASH).join(id).join(NAME);
    let mut edited = fs::read(&archive).unwrap();
    edited.extend_from_slice(b"X-Edited=true\n");
    fs::write(&archive, &edited).unwrap();
    fs::write(&path, b"keep user's replacement").unwrap();
    drop(store);
    let reopened = f.store();
    let row = reopened.recovery().unwrap().remove(0);
    assert!(!row.warnings.is_empty());
    assert!(reopened.restore(&row.operation_id, &row.revision).is_err());
    assert_eq!(fs::read(&archive).unwrap(), edited);
    assert_eq!(fs::read(path).unwrap(), b"keep user's replacement");
}

#[test]
fn plans_are_read_only_and_entries_withdraw_and_restore_persistently() {
    let f = Fixture::new();
    let store = f.store();
    let plan = store.prepare_create(ShortcutTarget::Applications).unwrap();
    assert_eq!(plan.entries.len(), 1);
    assert!(!f.0.join("xdg").exists());
    assert!(!f.0.join("project/.pcl-rust").exists());
    store
        .create(ShortcutTarget::Applications, &plan.revision)
        .unwrap();
    let plan = store.prepare_create(ShortcutTarget::Desktop).unwrap();
    store
        .create(ShortcutTarget::Desktop, &plan.revision)
        .unwrap();
    let plan = store.prepare_withdraw().unwrap();
    assert_eq!(plan.entries.len(), 2);
    let outcome = store.withdraw(&plan.revision).unwrap();
    assert_eq!(outcome.operation_ids.len(), 2);
    assert!(!f.0.join("xdg/applications").join(NAME).exists());
    assert!(!f.0.join("Desktop").join(NAME).exists());
    drop(store);
    let store = f.store();
    let rows = store.recovery().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert!(row.warnings.is_empty());
        store.restore(&row.operation_id, &row.revision).unwrap();
    }
    assert!(store.recovery().unwrap().is_empty());
    assert!(f.0.join("xdg/applications").join(NAME).is_file());
    assert!(f.0.join("Desktop").join(NAME).is_file());
    assert!(!f.0.join("project/.pcl-rust/accounts.json").exists());
}
#[test]
fn legacy_script_entry_is_owned_but_edited_or_foreign_entries_are_retained() {
    let f = Fixture::new();
    let store = f.store();
    fs::create_dir_all(&store.paths.applications).unwrap();
    let path = store.paths.applications.join(NAME);
    fs::write(&path, store.legacy().unwrap()).unwrap();
    let plan = store.prepare_withdraw().unwrap();
    assert_eq!(plan.entries.len(), 1);
    let outcome = store.withdraw(&plan.revision).unwrap();
    let row = store.recovery().unwrap().remove(0);
    store.restore(&row.operation_id, &row.revision).unwrap();
    assert_eq!(fs::read(&path).unwrap(), store.legacy().unwrap());
    assert_eq!(outcome.paths, vec![path.display().to_string()]);
    fs::write(&path, b"[Desktop Entry]\nExec=other-application\n").unwrap();
    assert!(store.prepare_create(ShortcutTarget::Applications).is_err());
    let plan = store.prepare_withdraw().unwrap();
    assert!(plan.entries.is_empty());
    assert_eq!(plan.warnings.len(), 1);
    assert!(store.withdraw(&plan.revision).is_err());
    assert_eq!(
        fs::read(path).unwrap(),
        b"[Desktop Entry]\nExec=other-application\n"
    );
}
#[test]
fn stale_withdraw_and_restore_conflict_keep_both_files() {
    let f = Fixture::new();
    let store = f.create(ShortcutTarget::Applications);
    let path = store.paths.applications.join(NAME);
    let plan = store.prepare_withdraw().unwrap();
    fs::write(&path, b"external replacement").unwrap();
    assert!(store.withdraw(&plan.revision).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"external replacement");
    fs::write(&path, store.contents().unwrap()).unwrap();
    let plan = store.prepare_withdraw().unwrap();
    store.withdraw(&plan.revision).unwrap();
    let row = store.recovery().unwrap().remove(0);
    fs::write(&path, b"keep new entry").unwrap();
    assert!(store.restore(&row.operation_id, &row.revision).is_err());
    let changed = store.recovery().unwrap().remove(0);
    assert!(!changed.warnings.is_empty());
    assert!(store
        .restore(&changed.operation_id, &changed.revision)
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"keep new entry");
    let id = row.operation_id.split_once(':').unwrap().1;
    assert!(store
        .paths
        .applications
        .join(TRASH)
        .join(id)
        .join(NAME)
        .is_file());
}
#[test]
fn directory_and_entry_symlinks_are_never_followed() {
    let f = Fixture::new();
    let store = f.store();
    fs::create_dir_all(f.0.join("actual-apps")).unwrap();
    fs::create_dir_all(f.0.join("xdg")).unwrap();
    symlink(f.0.join("actual-apps"), &store.paths.applications).unwrap();
    assert!(store.prepare_create(ShortcutTarget::Applications).is_err());
    assert_eq!(fs::read_dir(f.0.join("actual-apps")).unwrap().count(), 0);
    fs::remove_file(&store.paths.applications).unwrap();
    fs::create_dir(&store.paths.applications).unwrap();
    fs::write(f.0.join("foreign.desktop"), b"keep foreign").unwrap();
    symlink(
        f.0.join("foreign.desktop"),
        store.paths.applications.join(NAME),
    )
    .unwrap();
    assert!(store.prepare_withdraw().is_err());
    assert_eq!(
        fs::read(f.0.join("foreign.desktop")).unwrap(),
        b"keep foreign"
    );
}
#[test]
fn desktop_exec_escapes_both_parser_layers_and_percent_field_codes() {
    let text = desktop_exec(Path::new(
        "/example path/$value`tick\"quote\\slash%f/start-native.sh",
    ))
    .unwrap();
    assert_eq!(
        text,
        "\"/example path/\\\\$value\\\\`tick\\\\\"quote\\\\\\\\slash%%f/start-native.sh\""
    );
    assert!(desktop_exec(Path::new("/project=unsupported/start.sh")).is_err());
    assert!(desktop_exec(Path::new("/project\n/start.sh")).is_err());
}
#[test]
fn unknown_recovery_files_are_retained() {
    let f = Fixture::new();
    let store = f.create(ShortcutTarget::Applications);
    let plan = store.prepare_withdraw().unwrap();
    let outcome = store.withdraw(&plan.revision).unwrap();
    let id = outcome.operation_ids[0].split_once(':').unwrap().1;
    let unknown = store
        .paths
        .applications
        .join(TRASH)
        .join(id)
        .join("unknown-data");
    fs::write(&unknown, b"retain").unwrap();
    let row = store.recovery().unwrap().remove(0);
    assert!(!row.warnings.is_empty());
    assert!(store.restore(&row.operation_id, &row.revision).is_err());
    assert_eq!(fs::read(unknown).unwrap(), b"retain");
}

#[test]
fn interrupted_restore_and_later_original_replacement_do_not_reclaim_filename() {
    let f = Fixture::new();
    let store = f.create(ShortcutTarget::Applications);
    let plan = store.prepare_withdraw().unwrap();
    let outcome = store.withdraw(&plan.revision).unwrap();
    let id = outcome.operation_ids[0].split_once(':').unwrap().1;
    let parent = Dir::absolute(&store.paths.applications).unwrap();
    let operation = parent.child(TRASH).unwrap().child(id).unwrap();
    // A crash after rename, or intent persisted before an unperformed rename,
    // leaves a journal and the exact original inode in its original position.
    operation.move_new(NAME, &parent).unwrap();
    assert!(store.recovery().unwrap().is_empty());
    store
        .restore_one(ShortcutTarget::Applications, id, None)
        .unwrap();
    let original = store.paths.applications.join(NAME);
    fs::remove_file(&original).unwrap();
    let other = f.0.join("new-application.desktop");
    fs::write(&other, b"unrelated replacement").unwrap();
    symlink(&other, &original).unwrap();
    assert!(store.recovery().unwrap().is_empty());
    assert!(store
        .restore_one(ShortcutTarget::Applications, id, None)
        .is_err());
    assert_eq!(fs::read(other).unwrap(), b"unrelated replacement");
}

#[test]
fn same_basename_from_another_project_is_never_owned_by_current_project() {
    let f = Fixture::new();
    let store = f.store();
    fs::create_dir_all(&store.paths.applications).unwrap();
    let foreign_project = f.0.join("other-project");
    let foreign = ShortcutStore::new(foreign_project, store.paths.clone());
    let bytes = foreign.legacy().unwrap();
    fs::write(store.paths.applications.join(NAME), &bytes).unwrap();
    let plan = store.prepare_withdraw().unwrap();
    assert!(plan.entries.is_empty());
    assert!(!plan.warnings.is_empty());
    assert!(store.withdraw(&plan.revision).is_err());
    assert_eq!(
        fs::read(store.paths.applications.join(NAME)).unwrap(),
        bytes
    );
    assert!(!f.0.join("project/.pcl-rust").exists());
}

#[test]
fn unsafe_legacy_exec_is_withdrawable_but_cannot_be_silently_reused() {
    for name in ["project%f", "project=legacy"] {
        let f = Fixture::new();
        let project = f.0.join(name);
        fs::rename(f.0.join("project"), &project).unwrap();
        let original = f.store();
        let store = ShortcutStore::new(project, original.paths);
        fs::create_dir_all(&store.paths.applications).unwrap();
        let bytes = store.legacy().unwrap();
        fs::write(store.paths.applications.join(NAME), &bytes).unwrap();
        assert!(store.prepare_create(ShortcutTarget::Applications).is_err());
        let plan = store.prepare_withdraw().unwrap();
        assert_eq!(plan.entries.len(), 1);
        store.withdraw(&plan.revision).unwrap();
        let row = store.recovery().unwrap().remove(0);
        store.restore(&row.operation_id, &row.revision).unwrap();
        assert_eq!(
            fs::read(store.paths.applications.join(NAME)).unwrap(),
            bytes
        );
    }
    assert_eq!(desktop_value("/project ").unwrap(), "/project\\s");
}

#[test]
fn product_rename_keeps_exact_former_entries_and_rejects_edited_entries() {
    let f = Fixture::new();
    let store = f.store();
    fs::create_dir_all(&store.paths.applications).unwrap();
    let path = store.paths.applications.join(NAME);
    let current = String::from_utf8(historical_entry(&store, CURRENT_NAMES, true)).unwrap();
    let former = current.replace(CURRENT_NAMES, FORMER_NAMES);
    fs::write(&path, &former).unwrap();
    let plan = store.prepare_withdraw().unwrap();
    assert_eq!(plan.entries.len(), 1);
    store.withdraw(&plan.revision).unwrap();
    let row = store.recovery().unwrap().remove(0);
    store.restore(&row.operation_id, &row.revision).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), former);
    fs::write(&path, former + "X-Edited=true\n").unwrap();
    assert!(store.prepare_create(ShortcutTarget::Applications).is_err());
    assert!(store.prepare_withdraw().unwrap().entries.is_empty());
}
#[test]
fn product_rename_recognizes_current_install_script_entry() {
    let f = Fixture::new();
    let store = f.store();
    fs::create_dir_all(&store.paths.applications).unwrap();
    let current = String::from_utf8(store.legacy().unwrap())
        .unwrap()
        .replace(FORMER_NAMES, CURRENT_NAMES);
    fs::write(store.paths.applications.join(NAME), current).unwrap();
    assert_eq!(store.prepare_withdraw().unwrap().entries.len(), 1);
    assert!(
        store
            .prepare_create(ShortcutTarget::Applications)
            .unwrap()
            .entries[0]
            .already_registered
    );
}
