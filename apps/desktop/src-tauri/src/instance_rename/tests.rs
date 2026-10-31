use super::*;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::sync::Mutex;
fn identity(path: impl AsRef<Path>) -> pcl_core::retained_fs::Identity {
    let path = path.as_ref();
    if path.is_dir() {
        pcl_core::retained_fs::Directory::open_absolute(path)
            .unwrap()
            .identity()
            .unwrap()
    } else {
        pcl_core::retained_fs::metadata(&File::open(path).unwrap())
            .unwrap()
            .identity
    }
}
#[cfg(windows)]
fn directory_alias(source: &Path, target: &Path) {
    #[cfg(unix)]
    symlink(source, target).unwrap();
    #[cfg(windows)]
    {
        fs::create_dir(target).unwrap();
        super::junction_fixture::set_existing(source, target).unwrap();
    }
}
fn remove_alias(path: &Path) {
    #[cfg(unix)]
    fs::remove_file(path).unwrap();
    #[cfg(windows)]
    fs::remove_dir(path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn rename_lock_owner_drop_releases_lock_even_with_a_duplicate_descriptor() {
    let fixture = Fixture::new();
    let root = Dir::open(&fixture.root).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let owner = lock(&store).unwrap();
    // dup shares the same lock description as an inherited pre-exec fd, without
    // relying on fork scheduling to reproduce the completion/recovery race.
    let inherited = owner.0.try_clone().unwrap();
    assert!(lock(&store).is_err());
    drop(owner);
    let next_owner = lock(&store).unwrap();
    assert!(lock(&store).is_err());
    drop(inherited);
    assert!(lock(&store).is_err());
    drop(next_owner);
    assert!(lock(&store).is_ok());
}

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    project: PathBuf,
    content: Vec<(String, Vec<u8>, pcl_core::retained_fs::Identity)>,
    _temporary: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap();
        fs::create_dir_all(path.join("game/versions/old")).unwrap();
        fs::create_dir(path.join("project")).unwrap();
        pcl_core::retained_fs::Directory::open_absolute(&path.join("project"))
            .unwrap()
            .create_private(
                &pcl_core::retained_fs::Leaf::new(".pcl-rust").unwrap(),
                pcl_core::retained_fs::Traversal::AllowMounts,
            )
            .unwrap();
        let root = path.join("game");
        let project = path.join("project");
        let mut fixture = Self {
            path,
            root,
            project,
            content: Vec::new(),
            _temporary: temporary,
        };
        fixture.profile("old",json!({"id":"old","mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","jar":"old","libraries":[]}));
        fs::write(fixture.instance("old").join("old.jar"), b"client jar").unwrap();
        fixture.profile(
            "child",
            json!({"id":"child","inheritsFrom":"old","jar":"old","libraries":[]}),
        );
        fixture.profile("jar-user",json!({"id":"jar-user","mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","jar":"old","libraries":[]}));
        for name in [
            "mods/user.jar",
            "config/user.toml",
            "saves/world/level.dat",
            "options.txt",
            "PCL/Setup.ini",
        ] {
            let file = fixture.instance("old").join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, name.as_bytes()).unwrap();
            fixture
                .content
                .push((name.into(), name.as_bytes().to_vec(), identity(file)));
        }
        fixture.settings();
        fixture
    }
    fn instance(&self, id: &str) -> PathBuf {
        self.root.join("versions").join(id)
    }
    fn profile(&self, id: &str, data: Value) {
        fs::create_dir_all(self.instance(id)).unwrap();
        fs::write(
            self.instance(id).join(format!("{id}.json")),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
    }
    fn settings(&self) {
        let directory =
            pcl_core::retained_fs::Directory::open_absolute(&self.project.join(".pcl-rust"))
                .unwrap();
        let mut file = directory
            .create_file(&pcl_core::retained_fs::Leaf::new("settings.json").unwrap())
            .unwrap();
        file.write_all(&serde_json::to_vec(&json!({
            "schema_version":2,"active_root_id":"root-fixture","player":"Player","memory_gib":6,
            "roots":[{"id":"root-fixture","name":"Game","path":self.root,"selected":"old","overrides":{"old":8}}]
        })).unwrap()).unwrap();
    }
    fn settings_bytes(&self) -> Vec<u8> {
        crate::instance_rename_refs::store_snapshot(&self.project, "settings.json", 2 * 1024 * 1024)
            .unwrap()
            .1
            .unwrap()
    }
    fn plan(&self) -> RenamePlan {
        prepare(&self.root, "old", "renamed").unwrap()
    }
    fn refs(&self) -> RenameReferences {
        crate::instance_rename_refs::prepare(
            &self.project,
            "root-fixture",
            &self.root,
            "old",
            "renamed",
        )
        .unwrap()
    }
    fn prepared(&self) -> (Dir, Dir, Journal) {
        let plan = self.plan();
        let root = Dir::open(&self.root).unwrap();
        let store = storage(&root, true).unwrap().unwrap();
        let (operation, mut j) = setup(&root, &store, &self.project, &plan, self.refs()).unwrap();
        for (i, file) in plan.files.iter().enumerate() {
            stage_file(&operation, &mut j, i, &file.bytes, &AtomicBool::new(false)).unwrap();
        }
        j.state = State::Prepared;
        write_journal(&operation, &j).unwrap();
        j.refs
            .mark_pending(&self.project, &self.root, &j.operation_id)
            .unwrap();
        (root, operation, j)
    }
    fn assert_content(&self, id: &str) {
        for (name, bytes, ino) in &self.content {
            let path = self.instance(id).join(name);
            assert_eq!(fs::read(&path).unwrap(), *bytes);
            assert_eq!(identity(path), *ino);
        }
    }
    fn assert_original(&self, j: &Journal) {
        let versions = Dir::open(&self.root.join("versions")).unwrap();
        for file in &j.files {
            let dir = versions.child(&file.instance).unwrap();
            assert!(matches(
                &dir,
                &format!("{}.json", file.instance),
                &file.original,
                MAX_JSON
            )
            .unwrap());
        }
        self.assert_content("old");
        assert!(!self.instance("renamed").exists());
    }
    fn crash_after(&self, root: &Dir, operation: &Dir, j: &Journal, moves: usize) {
        let versions = bound_versions(root, j).unwrap();
        let instance = versions.child(&j.id).unwrap();
        let backup = operation.child("backup").unwrap();
        let incoming = operation.child("incoming").unwrap();
        let mut step = 0;
        for (i, file) in j.files.iter().enumerate() {
            let dir = profile_dir(&versions, j, file).unwrap();
            if step == moves {
                return;
            }
            rename_new(&dir, &format!("{}.json", file.instance), &backup, &slot(i)).unwrap();
            step += 1;
            if step == moves {
                return;
            }
            rename_new(&incoming, &slot(i), &dir, &target_name(j, file)).unwrap();
            step += 1;
        }
        if j.jar.is_some() {
            if step == moves {
                return;
            }
            rename_new(
                &instance,
                &format!("{}.jar", j.id),
                &instance,
                &format!("{}.jar", j.new_name),
            )
            .unwrap();
            step += 1;
        }
        if step == moves {
            return;
        }
        rename_new(&versions, &j.id, &versions, &j.new_name).unwrap();
    }
}

#[test]
fn rename_preserves_minecraft_inheritance_resource_bytes_and_inodes() {
    let f = Fixture::new();
    let plan = f.plan();
    assert_eq!(plan.dependent_instances, vec!["child", "jar-user"]);
    let dir_inode = identity(f.instance("old"));
    let jar_inode = identity(f.instance("old").join("old.jar"));
    let before = pcl_core::scan_instances(&f.root).unwrap();
    let progress = Mutex::new(Vec::new());
    let result = execute(
        &f.root,
        &f.project,
        plan,
        f.refs(),
        &AtomicBool::new(false),
        |p| progress.lock().unwrap().push(p),
    )
    .unwrap();
    assert_eq!(result["id"], "renamed");
    assert_eq!(identity(f.instance("renamed")), dir_inode);
    assert_eq!(
        identity(f.instance("renamed").join("renamed.jar")),
        jar_inode
    );
    f.assert_content("renamed");
    let selected: Value =
        serde_json::from_slice(&fs::read(f.instance("renamed").join("renamed.json")).unwrap())
            .unwrap();
    assert_eq!(selected["id"], "renamed");
    assert_eq!(selected["clientVersion"], "1.20.1");
    assert_eq!(selected["jar"], "renamed");
    let after = pcl_core::scan_instances(&f.root).unwrap();
    for old in before {
        let id = if old.id == "old" { "renamed" } else { &old.id };
        let renamed = after.iter().find(|item| item.id == id).unwrap();
        assert_eq!(renamed.minecraft_version, old.minecraft_version);
        assert_eq!(renamed.isolated, old.isolated);
    }
    let settings: Value = serde_json::from_slice(&f.settings_bytes()).unwrap();
    assert_eq!(settings["roots"][0]["selected"], "renamed");
    assert_eq!(settings["roots"][0]["overrides"]["renamed"], 8);
    let events = progress.into_inner().unwrap();
    let total = events.last().unwrap().total;
    assert_eq!(events.last().unwrap().completed, total);
    assert!(events.iter().all(|p| p.total == total));
    assert!(events.windows(2).all(|p| p[0].completed <= p[1].completed));
    ensure_ready(&f.root).unwrap();
    crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
}
#[test]
fn unnamed_vanilla_minecraft_identity_survives_physical_rename() {
    let f = Fixture::new();
    f.profile(
        "1.20.1",
        json!({"id":"1.20.1","mainClass":"net.minecraft.client.main.Main","libraries":[]}),
    );
    fs::write(f.instance("1.20.1").join("1.20.1.jar"), b"vanilla client").unwrap();
    let plan = prepare(&f.root, "1.20.1", "renamed").unwrap();
    assert_eq!(plan.minecraft, "1.20.1");
    let refs = crate::instance_rename_refs::prepare(
        &f.project,
        "root-fixture",
        &f.root,
        "1.20.1",
        "renamed",
    )
    .unwrap();
    execute(
        &f.root,
        &f.project,
        plan,
        refs,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let metadata: Value =
        serde_json::from_slice(&fs::read(f.instance("renamed").join("renamed.json")).unwrap())
            .unwrap();
    assert_eq!(metadata["id"], "renamed");
    assert_eq!(metadata["clientVersion"], "1.20.1");
    assert_eq!(
        pcl_core::scan_instances(&f.root)
            .unwrap()
            .iter()
            .find(|i| i.id == "renamed")
            .unwrap()
            .minecraft_version,
        "1.20.1"
    );
    f.assert_content("old");
}
#[test]
fn vanilla_without_client_version_keeps_original_version_and_external_jar_is_retained() {
    let f = Fixture::new();
    f.profile("base",json!({"id":"base","mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","libraries":[]}));
    fs::write(f.instance("base").join("base.jar"), b"external base jar").unwrap();
    f.profile("old",json!({"id":"old","mainClass":"net.minecraft.client.main.Main","inheritsFrom":"base","jar":"base","libraries":[]}));
    fs::remove_file(f.instance("old").join("old.jar")).unwrap();
    let base_bytes = fs::read(f.instance("base").join("base.json")).unwrap();
    let base_inode = identity(f.instance("base").join("base.jar"));
    execute(
        &f.root,
        &f.project,
        f.plan(),
        f.refs(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let renamed: Value =
        serde_json::from_slice(&fs::read(f.instance("renamed").join("renamed.json")).unwrap())
            .unwrap();
    assert_eq!(renamed["clientVersion"], "1.20.1");
    assert_eq!(renamed["jar"], "base");
    assert_eq!(renamed["inheritsFrom"], "base");
    assert_eq!(
        fs::read(f.instance("base").join("base.json")).unwrap(),
        base_bytes
    );
    assert_eq!(identity(f.instance("base").join("base.jar")), base_inode);
    assert!(!f.instance("renamed").join("renamed.jar").exists());
    f.assert_content("renamed");
}
#[test]
fn every_midcommit_fault_rolls_back_all_json_jar_directory_and_refs() {
    for stop in 1..=8 {
        let f = Fixture::new();
        let settings = fs::read(f.project.join(".pcl-rust/settings.json")).unwrap();
        let (root, operation, mut j) = f.prepared();
        let error = commit_files(&root, &operation, &mut j, &|moved| {
            if moved == stop {
                Err("fixture fault".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error, "fixture fault");
        assert_eq!(j.state, State::RolledBack);
        f.assert_original(&j);
        assert_eq!(
            fs::read(f.project.join(".pcl-rust/settings.json")).unwrap(),
            settings
        );
        ensure_ready(&f.root).unwrap();
        crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
    }
}
#[test]
fn every_prepared_crash_position_recovers_originals_with_no_overwrite() {
    for moves in 0..=8 {
        let f = Fixture::new();
        let (root, operation, j) = f.prepared();
        f.crash_after(&root, &operation, &j, moves);
        assert_eq!(ensure_ready(&f.root).unwrap_err(), READY);
        assert!(crate::instance_rename_refs::ensure_project_ready(&f.project).is_err());
        assert_eq!(
            recover_pending(&f.root, &f.project).unwrap()["recovered"],
            1
        );
        f.assert_original(&j);
        assert_eq!(
            recover_pending(&f.root, &f.project).unwrap()["recovered"],
            0
        );
        crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
    }
}
#[test]
fn files_committed_crash_finishes_refs_and_finished_crash_clears_marker() {
    let f = Fixture::new();
    let (root, operation, mut j) = f.prepared();
    commit_files(&root, &operation, &mut j, &|_| Ok(())).unwrap();
    assert_eq!(j.state, State::FilesCommitted);
    assert!(ensure_ready(&f.root).is_err());
    assert_eq!(
        recover_pending(&f.root, &f.project).unwrap()["recovered"],
        1
    );
    f.assert_content("renamed");
    assert!(
        read_journal(&operation, &j.operation_id)
            .unwrap()
            .marker_cleared
    );
    let f = Fixture::new();
    let (root, operation, mut j) = f.prepared();
    commit_files(&root, &operation, &mut j, &|_| Ok(())).unwrap();
    j.refs.apply(&f.project, &f.root).unwrap();
    j.state = State::Finished;
    write_journal(&operation, &j).unwrap();
    assert!(crate::instance_rename_refs::ensure_project_ready(&f.project).is_err());
    assert_eq!(
        recover_pending(&f.root, &f.project).unwrap()["recovered"],
        1
    );
    crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
}
#[test]
fn cancellation_before_commit_cleans_and_late_cancel_commits_successfully() {
    for late in [false, true] {
        let f = Fixture::new();
        let cancel = AtomicBool::new(false);
        let result = execute(&f.root, &f.project, f.plan(), f.refs(), &cancel, |p| {
            if (!late && p.phase == "staging")
                || (late && p.phase == "committing" && p.completed > 3)
            {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        if late {
            result.unwrap();
            f.assert_content("renamed");
        } else {
            let error = result.unwrap_err();
            assert!(error.contains("已取消"));
            assert!(!error.starts_with("取消清理失败："));
            f.assert_content("old");
        }
        ensure_ready(&f.root).unwrap();
        crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
    }
}
#[test]
fn post_files_external_reference_edit_is_retained_and_guarded() {
    let f = Fixture::new();
    let settings = fs::read(f.project.join(".pcl-rust/settings.json")).unwrap();
    let result = execute(
        &f.root,
        &f.project,
        f.plan(),
        f.refs(),
        &AtomicBool::new(false),
        |p| {
            if p.phase == "references" {
                fs::write(
                    f.project.join(".pcl-rust/settings.json"),
                    b"external edit retained",
                )
                .unwrap();
            }
        },
    );
    assert!(result
        .unwrap_err()
        .starts_with("取消清理失败：实例文件已改名"));
    assert_eq!(ensure_ready(&f.root).unwrap_err(), READY);
    f.assert_content("renamed");
    assert_eq!(
        fs::read(f.project.join(".pcl-rust/settings.json")).unwrap(),
        b"external edit retained"
    );
    assert!(recover_pending(&f.root, &f.project).is_err());
    fs::write(f.project.join(".pcl-rust/settings.json"), settings).unwrap();
    // The original identity/ctime is part of the refs snapshot. A content-only
    // restoration is deliberately insufficient and must remain guarded.
    assert!(recover_pending(&f.root, &f.project).is_err());
}
#[test]
fn post_files_reference_store_failure_recovers_after_store_is_restored() {
    let f = Fixture::new();
    let app = f.project.join(".pcl-rust");
    let detached = f.project.join("detached-app-data");
    let result = execute(
        &f.root,
        &f.project,
        f.plan(),
        f.refs(),
        &AtomicBool::new(false),
        |p| {
            if p.phase == "references" {
                fs::rename(&app, &detached).unwrap();
                #[cfg(unix)]
                symlink(&f.root, &app).unwrap();
                #[cfg(windows)]
                directory_alias(&f.root, &app);
            }
        },
    );
    assert!(result
        .unwrap_err()
        .starts_with("取消清理失败：实例文件已改名"));
    f.assert_content("renamed");
    assert_eq!(ensure_ready(&f.root).unwrap_err(), READY);
    assert!(recover_pending(&f.root, &f.project).is_err());
    remove_alias(&app);
    fs::rename(&detached, &app).unwrap();
    assert_eq!(
        recover_pending(&f.root, &f.project).unwrap()["recovered"],
        1
    );
    ensure_ready(&f.root).unwrap();
    crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
    let settings: Value = serde_json::from_slice(&f.settings_bytes()).unwrap();
    assert_eq!(settings["roots"][0]["selected"], "renamed");
}
#[test]
fn cancellation_accepted_at_commit_gate_is_still_cleaned_before_first_mutation() {
    let f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let result = execute(&f.root, &f.project, f.plan(), f.refs(), &cancel, |p| {
        if p.phase == "committing" {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    let error = result.unwrap_err();
    assert!(error.contains("已取消"));
    assert!(!error.starts_with("取消清理失败："));
    f.assert_content("old");
    assert!(!f.instance("renamed").exists());
    ensure_ready(&f.root).unwrap();
    crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
}
#[test]
fn occupied_targets_stale_plans_links_and_unknown_paths_never_change_instances() {
    let f = Fixture::new();
    let plan = f.plan();
    let refs = f.refs();
    fs::write(f.instance("old").join("old.jar"), b"changed jar").unwrap();
    assert!(execute(
        &f.root,
        &f.project,
        plan,
        refs,
        &AtomicBool::new(false),
        |_| {}
    )
    .unwrap_err()
    .contains("已变化"));
    f.assert_content("old");
    fs::create_dir(f.instance("renamed")).unwrap();
    assert!(f.plan_error().contains("占用"));
    fs::remove_dir(f.instance("renamed")).unwrap();
    #[cfg(unix)]
    {
        symlink(&f.project, f.instance("renamed")).unwrap();
        assert!(f.plan_error().contains("占用"));
        fs::remove_file(f.instance("renamed")).unwrap();
        fs::remove_file(f.instance("old").join("old.jar")).unwrap();
        symlink(
            f.project.join(".pcl-rust/settings.json"),
            f.instance("old").join("old.jar"),
        )
        .unwrap();
        assert!(prepare(&f.root, "old", "renamed").is_err());
        fs::remove_file(f.instance("old").join("old.jar")).unwrap();
        fs::write(f.instance("old").join("old.jar"), b"client jar").unwrap();
    }
    f.profile("old",json!({"id":"old","mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","jar":"old","libraries":[],"customPath":f.instance("old").join("options.txt")}));
    assert!(f.plan_error().contains("路径引用"));
    f.profile("old",json!({"id":"old","mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","jar":"old","libraries":[],"customPath":"C:\\game\\versions\\old\\options.txt"}));
    assert!(f.plan_error().contains("路径引用"));
    f.assert_content("old");
}
impl Fixture {
    fn plan_error(&self) -> String {
        prepare(&self.root, "old", "renamed").unwrap_err()
    }
}
#[test]
fn target_appearing_at_commit_is_preserved_and_originals_remain_ready() {
    let f = Fixture::new();
    let once = AtomicBool::new(false);
    let error = execute(
        &f.root,
        &f.project,
        f.plan(),
        f.refs(),
        &AtomicBool::new(false),
        |p| {
            if p.phase == "committing" && !once.swap(true, Ordering::Relaxed) {
                fs::create_dir(f.instance("renamed")).unwrap();
                fs::write(f.instance("renamed").join("user.txt"), b"external target").unwrap();
            }
        },
    )
    .unwrap_err();
    assert!(error.contains("占用"));
    assert_eq!(
        fs::read(f.instance("renamed").join("user.txt")).unwrap(),
        b"external target"
    );
    f.assert_content("old");
    ensure_ready(&f.root).unwrap();
    crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
}
#[test]
fn occupied_final_directory_after_core_moves_rolls_back_without_touching_occupant() {
    let f = Fixture::new();
    let (root, operation, mut j) = f.prepared();
    let error = commit_files(&root, &operation, &mut j, &|moved| {
        if moved == 7 {
            fs::create_dir(f.instance("renamed")).unwrap();
            fs::write(f.instance("renamed").join("user.txt"), b"occupied target").unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert!(error.contains("无覆盖重命名"));
    assert_eq!(j.state, State::RolledBack);
    assert_eq!(
        fs::read(f.instance("renamed").join("user.txt")).unwrap(),
        b"occupied target"
    );
    let versions = Dir::open(&f.root.join("versions")).unwrap();
    for file in &j.files {
        assert!(matches(
            &versions.child(&file.instance).unwrap(),
            &format!("{}.json", file.instance),
            &file.original,
            MAX_JSON
        )
        .unwrap());
    }
    assert!(matches(
        &versions.child("old").unwrap(),
        "old.jar",
        j.jar.as_ref().unwrap(),
        MAX_JAR
    )
    .unwrap());
    f.assert_content("old");
    ensure_ready(&f.root).unwrap();
    crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
}
#[test]
fn externally_edited_midcommit_core_retains_both_content_and_original_backup() {
    let f = Fixture::new();
    let (root, operation, mut j) = f.prepared();
    let error = commit_files(&root, &operation, &mut j, &|moved| {
        if moved == 2 {
            fs::write(
                f.instance("old").join("renamed.json"),
                b"external replacement edit",
            )
            .unwrap();
            Err("fixture fault".into())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(error.starts_with("取消清理失败："));
    assert_eq!(
        fs::read(f.instance("old").join("renamed.json")).unwrap(),
        b"external replacement edit"
    );
    assert!(matches(
        &operation.child("backup").unwrap(),
        &slot(0),
        &j.files[0].original,
        MAX_JSON
    )
    .unwrap());
    assert!(recover_pending(&f.root, &f.project).is_err());
    assert!(ensure_ready(&f.root).is_err());
    f.assert_content("old");
}
#[test]
fn original_long_name_can_be_shortened_and_selected_cycle_is_rejected() {
    let f = Fixture::new();
    let long = "x".repeat(121);
    fs::rename(f.instance("old"), f.instance(&long)).unwrap();
    fs::rename(
        f.instance(&long).join("old.json"),
        f.instance(&long).join(format!("{long}.json")),
    )
    .unwrap();
    fs::rename(
        f.instance(&long).join("old.jar"),
        f.instance(&long).join(format!("{long}.jar")),
    )
    .unwrap();
    f.profile(&long,json!({"id":long,"mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","jar":long,"libraries":[]}));
    assert!(prepare(&f.root, &long, "short").is_ok());
    f.profile(&long,json!({"id":long,"mainClass":"net.minecraft.client.main.Main","clientVersion":"1.20.1","inheritsFrom":long,"jar":long,"libraries":[]}));
    assert!(prepare(&f.root, &long, "short")
        .unwrap_err()
        .contains("继承循环"));
}
#[cfg(target_os = "linux")]
#[test]
fn anonymous_json_registered_without_name_recovers_and_does_not_touch_user_data() {
    let f = Fixture::new();
    let plan = f.plan();
    let root = Dir::open(&f.root).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let (operation, mut j) = setup(&root, &store, &f.project, &plan, f.refs()).unwrap();
    let incoming = operation.child("incoming").unwrap();
    let mut file = incoming.anonymous_file().unwrap();
    file.write_all(&plan.files[0].bytes).unwrap();
    file.sync_all().unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    j.files[0].replacement = Some(snapshot(file.try_clone().unwrap(), MAX_JSON, None).unwrap());
    write_journal(&operation, &j).unwrap();
    drop(file);
    assert!(incoming.names().unwrap().is_empty());
    recover_pending(&f.root, &f.project).unwrap();
    f.assert_original(&j);
}
#[cfg(unix)]
#[test]
fn absolute_relative_and_nested_content_symlinks_are_retained_and_rejected() {
    for relative in [false, true] {
        let f = Fixture::new();
        let mods = f.instance("old").join("mods");
        let shared = f.instance("old").join("sharedmods");
        fs::rename(&mods, &shared).unwrap();
        let target = if relative {
            PathBuf::from("sharedmods")
        } else {
            shared
        };
        symlink(&target, &mods).unwrap();
        // Automatic content scope now reports a linked marker as a scan issue.
        // Refusing rename must preserve that evidence and every other instance;
        // treating the unsafe instance as a normal scan result would hide it.
        let before = pcl_core::scan_instances_report(&f.root).unwrap();
        assert!(!before.instances.iter().any(|i| i.id == "old"));
        assert!(before
            .issues
            .iter()
            .any(|issue| issue.id == "old" && issue.message.contains("符号链接")));
        assert!(f.plan_error().contains("符号链接"));
        assert_eq!(fs::read_link(&mods).unwrap(), target);
        let after = pcl_core::scan_instances_report(&f.root).unwrap();
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap()
        );
        f.assert_content("old");
    }
    let f = Fixture::new();
    let nested = f.instance("old").join("saves/world/deep");
    fs::create_dir(&nested).unwrap();
    symlink(&f.project, nested.join("external")).unwrap();
    let error = f.plan_error();
    assert!(error.contains("saves/world/deep/external") && error.contains("符号链接"));
    assert_eq!(fs::read_link(nested.join("external")).unwrap(), f.project);
    f.assert_content("old");
}
#[cfg(unix)]
#[test]
fn late_inserted_content_links_before_commit_and_publish_cancel_the_transaction_safely() {
    for after_move in [0, 1, 8] {
        let f = Fixture::new();
        let inserted = AtomicBool::new(false);
        let link = Mutex::new(None::<PathBuf>);
        let error = execute(
            &f.root,
            &f.project,
            f.plan(),
            f.refs(),
            &AtomicBool::new(false),
            |p| {
                if p.phase == "committing"
                    && p.completed == 3 + after_move
                    && !inserted.swap(true, Ordering::Relaxed)
                {
                    let instance = if after_move == 8 {
                        f.instance("renamed")
                    } else {
                        f.instance("old")
                    };
                    let path = instance.join("saves/world/late-link");
                    symlink(f.instance("old").join("mods"), &path).unwrap();
                    *link.lock().unwrap() = Some(path);
                }
            },
        )
        .unwrap_err();
        assert!(error.contains("符号链接"), "{error}");
        assert!(f.instance("old").exists());
        assert!(!f.instance("renamed").exists());
        assert_eq!(
            fs::read_link(f.instance("old").join("saves/world/late-link")).unwrap(),
            f.instance("old").join("mods")
        );
        f.assert_content("old");
        ensure_ready(&f.root).unwrap();
        crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
    }
}
#[cfg(target_os = "linux")]
#[test]
fn interrupted_initial_anonymous_journal_leaves_an_empty_recoverable_orphan() {
    let f = Fixture::new();
    let root = Dir::open(&f.root).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let name = operation_id();
    let operation = store.create_dir(&name).unwrap();
    let mut partial = operation.anonymous_file().unwrap();
    partial.write_all(b"{").unwrap();
    partial.sync_all().unwrap();
    drop(partial);
    assert!(operation.names().unwrap().is_empty());
    assert_eq!(ensure_ready(&f.root).unwrap_err(), READY);
    assert_eq!(
        recover_pending(&f.root, &f.project).unwrap()["recovered"],
        1
    );
    assert!(store.optional(&name).unwrap().is_none());
    f.assert_content("old");
}
#[test]
fn fully_registered_initial_staging_recovers_and_unknown_orphan_content_is_preserved() {
    let f = Fixture::new();
    let plan = f.plan();
    let root = Dir::open(&f.root).unwrap();
    let store = storage(&root, true).unwrap().unwrap();
    let (operation, j) = setup(&root, &store, &f.project, &plan, f.refs()).unwrap();
    assert_eq!(
        read_journal(&operation, &j.operation_id).unwrap().state,
        State::Staging
    );
    assert_eq!(
        recover_pending(&f.root, &f.project).unwrap()["recovered"],
        1
    );
    f.assert_original(&j);
    let name = operation_id();
    let orphan = store.create_dir(&name).unwrap();
    let mut file = orphan.create_file("user.txt").unwrap();
    file.write_all(b"unknown user content").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let before = snapshot(orphan.regular("user.txt").unwrap(), MAX_JSON, None).unwrap();
    assert!(recover_pending(&f.root, &f.project)
        .unwrap_err()
        .contains("未登记"));
    assert_eq!(
        snapshot(orphan.regular("user.txt").unwrap(), MAX_JSON, None).unwrap(),
        before
    );
    f.assert_content("old");
}
#[cfg(target_os = "linux")]
#[test]
fn completed_journal_update_leftover_is_verified_and_cleaned_during_recovery() {
    let f = Fixture::new();
    let (_, operation, j) = f.prepared();
    let name = format!("journal-{}.next", operation_id());
    let mut file = operation.create_file(&name).unwrap();
    file.write_all(&serde_json::to_vec(&j).unwrap()).unwrap();
    file.sync_all().unwrap();
    recover_pending(&f.root, &f.project).unwrap();
    assert!(operation.stat(&name).unwrap().is_none());
    f.assert_original(&j);
}
#[test]
fn detached_record_store_is_retained_and_recovery_requires_restoring_its_binding() {
    let f = Fixture::new();
    let (_, _, j) = f.prepared();
    let store = f.root.join(".pcl-linux/instance-renames");
    let detached = f.root.join(".pcl-linux/detached-renames");
    fs::rename(&store, &detached).unwrap();
    assert!(recover_pending(&f.root, &f.project)
        .unwrap_err()
        .contains("记录目录缺失"));
    assert!(crate::instance_rename_refs::ensure_project_ready(&f.project).is_err());
    fs::rename(&detached, &store).unwrap();
    assert_eq!(
        recover_pending(&f.root, &f.project).unwrap()["recovered"],
        1
    );
    f.assert_original(&j);
    ensure_ready(&f.root).unwrap();
    crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
}

#[test]
fn uncertain_commit_barrier_never_rewrites_prepared_and_recovers_from_actual_record() {
    for after_publication in [false, true] {
        let f = Fixture::new();
        let (root, operation, mut journal) = f.prepared();
        COMMIT_JOURNAL_FAULT.with(|fault| fault.set(if after_publication { 2 } else { 1 }));
        let error = commit_files(&root, &operation, &mut journal, &|_| Ok(())).unwrap_err();
        COMMIT_JOURNAL_FAULT.with(|fault| fault.set(0));
        assert!(error.contains("无法确认提交状态"), "{error}");
        assert_eq!(journal.state, State::FilesCommitted);
        f.assert_content("renamed");
        assert!(!f.instance("old").exists());
        assert_eq!(
            read_journal(&operation, &journal.operation_id)
                .unwrap()
                .state,
            if after_publication {
                State::FilesCommitted
            } else {
                State::Prepared
            }
        );
        assert!(crate::instance_rename_refs::ensure_project_ready(&f.project).is_err());
        recover_pending(&f.root, &f.project).unwrap();
        if after_publication {
            f.assert_content("renamed");
            assert_eq!(
                serde_json::from_slice::<Value>(&f.settings_bytes()).unwrap()["roots"][0]
                    ["selected"],
                "renamed"
            );
        } else {
            f.assert_original(&journal);
        }
        ensure_ready(&f.root).unwrap();
        crate::instance_rename_refs::ensure_project_ready(&f.project).unwrap();
    }
}
#[cfg(not(target_os = "linux"))]
#[test]
fn equivalent_destination_alias_is_rejected_before_any_control_or_game_mutation() {
    let f = Fixture::new();
    let root = Dir::open(&f.root).unwrap();
    let versions = root.child("versions").unwrap();
    if versions.stat("OLD").unwrap().is_some() {
        let before = fs::read(f.instance("old").join("old.json")).unwrap();
        assert!(prepare(&f.root, "old", "OLD")
            .unwrap_err()
            .contains("等价别名"));
        assert!(storage(&root, false).unwrap().is_none());
        assert_eq!(
            fs::read(f.instance("old").join("old.json")).unwrap(),
            before
        );
        f.assert_content("old");
    } else {
        // Case-sensitive native filesystems have genuinely distinct slots.
        assert!(prepare(&f.root, "old", "OLD").is_ok());
    }
}
#[cfg(windows)]
#[test]
fn held_native_descendant_refuses_directory_move_without_stopping_owner_and_retry_succeeds() {
    let f = Fixture::new();
    let held =
        pcl_core::retained_fs::Directory::open_absolute(&f.instance("old").join("saves/world"))
            .unwrap();
    let (root, operation, mut journal) = f.prepared();
    let error = commit_files(&root, &operation, &mut journal, &|_| Ok(())).unwrap_err();
    assert!(error.contains("无覆盖重命名"), "{error}");
    assert_eq!(journal.state, State::RolledBack);
    f.assert_original(&journal);
    assert!(held.metadata().is_ok());
    drop(held);
    execute(
        &f.root,
        &f.project,
        f.plan(),
        f.refs(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    f.assert_content("renamed");
}
#[cfg(windows)]
#[test]
fn native_junction_content_is_preserved_and_rejected_before_staging() {
    let f = Fixture::new();
    let outside = f.path.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"unchanged").unwrap();
    let alias = f.instance("old").join("saves/world/external");
    directory_alias(&outside, &alias);
    let error = prepare(&f.root, "old", "renamed").unwrap_err();
    assert!(
        error.contains("saves/world/external") && error.contains("重解析"),
        "{error}"
    );
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"unchanged");
    assert!(!f.root.join(".pcl-linux").exists());
    f.assert_content("old");
    remove_alias(&alias);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_snapshot_and_directory_key_keep_schema_one_field_order_and_integer_values() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    let path = f.instance("old").join("old.json");
    let m = fs::metadata(&path).unwrap();
    let snap = snapshot(File::open(&path).unwrap(), MAX_JSON, None).unwrap();
    let expected=format!("{{\"size\":{},\"hash\":\"{}\",\"dev\":{},\"ino\":{},\"mtime\":{},\"mtime_nsec\":{},\"ctime\":{},\"ctime_nsec\":{}}}",m.len(),snap.hash,m.dev(),m.ino(),m.mtime(),m.mtime_nsec(),m.ctime(),m.ctime_nsec());
    assert_eq!(serde_json::to_string(&snap).unwrap(), expected);
    let root = Dir::open(&f.root).unwrap();
    let m = fs::metadata(&f.root).unwrap();
    assert_eq!(
        serde_json::to_string(&root.key().unwrap()).unwrap(),
        format!("{{\"dev\":{},\"ino\":{}}}", m.dev(), m.ino())
    );
    assert_eq!(SCHEMA, 1);
    assert_eq!(REVISION_PREFIX, "rename-v1-");
}

#[cfg(windows)]
#[test]
fn windows_reference_case_alias_of_victim_is_rejected_before_any_mutation() {
    for field in ["inheritsFrom", "jar"] {
        let fixture = Fixture::new();
        let mut profile = json!({"id":"case-user","libraries":[]});
        profile[field] = json!("OLD");
        fixture.profile("case-user", profile);
        let versions = Dir::open(&fixture.root).unwrap().child("versions").unwrap();
        assert_eq!(
            versions.child("OLD").unwrap().key().unwrap(),
            versions.child("old").unwrap().key().unwrap(),
            "fixture must observe a real Windows directory alias"
        );
        drop(versions);
        let files: Vec<_> = ["old", "child", "jar-user", "case-user"]
            .into_iter()
            .map(|id| {
                let path = fixture.instance(id).join(format!("{id}.json"));
                (path.clone(), fs::read(&path).unwrap(), identity(&path))
            })
            .collect();
        let settings = fixture.settings_bytes();
        let error = match prepare(&fixture.root, "old", "renamed") {
            Ok(_) => panic!("directory alias reference must be refused"),
            Err(error) => error,
        };
        assert!(error.contains("待改名实例的目录别名"), "{error}");
        assert!(error.contains(field), "{error}");
        assert!(!fixture.root.join(".pcl-linux/instance-renames").exists());
        assert!(!fixture.instance("renamed").exists());
        for (path, bytes, key) in files {
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert_eq!(identity(path), key);
        }
        assert_eq!(fixture.settings_bytes(), settings);
        fixture.assert_content("old");
    }
}
