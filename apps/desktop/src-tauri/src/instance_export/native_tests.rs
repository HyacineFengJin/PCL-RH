use super::*;
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use std::{fs, sync::Mutex};

struct Fixture {
    _temp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new(isolated: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let parent = Directory::open_absolute(&base).unwrap();
        drop(
            parent
                .create_private(&Leaf::new("game").unwrap(), Traversal::AllowMounts)
                .unwrap(),
        );
        let f = Self {
            _temp: temp,
            root: base.join("game"),
            base,
        };
        f.write(
            "versions/base/base.json",
            br#"{"id":"base","clientVersion":"1.20.1","libraries":[]}"#,
        );
        f.write("versions/base/base.jar", b"native client fixture");
        f.write(
            "versions/example/example.json",
            br#"{"id":"example","inheritsFrom":"base","libraries":[]}"#,
        );
        if isolated {
            f.game("options.txt", b"music:0.5", true);
        }
        f
    }
    fn directory(&self, path: &str) -> Directory {
        let mut dir = Directory::open_absolute(&self.root).unwrap();
        for name in path.split('/').filter(|name| !name.is_empty()) {
            let leaf = Leaf::new(name).unwrap();
            dir = match dir.child(&leaf, Traversal::AllowMounts) {
                Ok(child) => child,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    dir.create_private(&leaf, Traversal::AllowMounts).unwrap()
                }
                Err(e) => panic!("{e}"),
            };
        }
        dir
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let dir = self.directory(parent);
        let leaf = Leaf::new(name).unwrap();
        if dir.entry(&leaf).unwrap().is_some() {
            fs::write(self.root.join(path), bytes).unwrap();
        } else {
            let mut file = dir.create_file(&leaf).unwrap();
            file.write_all(bytes).unwrap();
        }
    }
    fn game(&self, path: &str, bytes: &[u8], isolated: bool) {
        self.write(
            &if isolated {
                format!("versions/example/{path}")
            } else {
                path.into()
            },
            bytes,
        );
    }
    fn destination(&self) -> PathBuf {
        self.base.join("export.zip")
    }
    fn plan(&self, request: ExportRequest) -> ExportPlan {
        prepare(&self.root, "example", request).unwrap()
    }
    fn assert_no_output(&self) {
        assert!(!self.destination().exists());
        let names = fs::read_dir(&self.base)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from("game")]);
    }
}
fn request() -> ExportRequest {
    ExportRequest {
        name: "Native fixture".into(),
        version: "1.0".into(),
        checks: ExportChecks::default(),
        excluded: BTreeMap::new(),
    }
}
fn unzip(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut zip = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
    let mut result = BTreeMap::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(result.insert(name, bytes).is_none());
    }
    result
}

#[test]
fn native_export_defaults_isolation_manifest_selected_files_and_archive_hash_are_real() {
    for isolated in [false, true] {
        let f = Fixture::new(isolated);
        for name in [
            "mods/example.jar",
            "config/settings.toml",
            "resourcepacks/pack.zip",
            "saves/world/level.dat",
            "screenshots/personal.png",
            "logs/latest.log",
            "other/accounts.json",
        ] {
            f.game(name, name.as_bytes(), isolated);
        }
        let plan = f.plan(request());
        let selected = plan.selected_files();
        assert!(selected
            .iter()
            .any(|entry| entry.path.ends_with("mods/example.jar")
                && !entry.directory
                && entry.hash.len() == 64));
        assert!(!selected
            .iter()
            .any(|entry| entry.path.contains("screenshots/")
                || entry.path.contains("saves/")
                || entry.path.contains("accounts.json")));
        plan.validate_copy_source(&AtomicBool::new(false), true)
            .unwrap();
        let manifest: Value = serde_json::from_slice(plan.manifest_bytes()).unwrap();
        assert_eq!(manifest["game"]["isolated"], isolated);
        let original = fs::read(f.root.join("versions/base/base.jar")).unwrap();
        let events = Mutex::new(Vec::new());
        let result = execute(
            plan,
            &f.destination(),
            &AtomicBool::new(false),
            |progress| events.lock().unwrap().push(progress),
        )
        .unwrap();
        let entries = unzip(&f.destination());
        let prefix = if isolated {
            ".minecraft/versions/example/"
        } else {
            ".minecraft/"
        };
        assert_eq!(
            entries[&format!("{prefix}mods/example.jar")],
            b"mods/example.jar"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&entries["pcl-export.json"]).unwrap(),
            manifest
        );
        assert_eq!(
            result["sha256"],
            format!("{:x}", Sha256::digest(fs::read(f.destination()).unwrap()))
        );
        assert_eq!(
            fs::read(f.root.join("versions/base/base.jar")).unwrap(),
            original
        );
        assert!(events
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .steps
            .iter()
            .all(|step| step.state == "complete"));
        assert_eq!(fs::read_dir(&f.base).unwrap().count(), 2);
    }
}

#[test]
fn native_export_personal_flags_exclusions_and_mod_parent_gating_are_enforced() {
    let f = Fixture::new(true);
    for path in [
        "hotbar.nbt",
        "config/jei/bookmarks.ini",
        "patchouli_data.json",
        "XaeroWaypoints/server/points.txt",
        "screenshots/keep.png",
        "screenshots/skip.png",
        "mods/keep.jar",
        "mods/skip.jar",
        "saves/keep/level.dat",
        "saves/skip/level.dat",
        "servers.dat",
        "scripts/example.zs",
        "other/note.txt",
        "other/accounts.json",
    ] {
        f.game(path, b"fixture", true);
    }
    f.directory("versions/example/saves/empty");
    let mut req = request();
    req.checks.game_personal = true;
    req.checks.maps = true;
    req.checks.jei_personal = true;
    req.checks.guide_personal = true;
    req.checks.screenshots = true;
    req.checks.saves = true;
    req.checks.server = true;
    req.checks.other = true;
    req.excluded = BTreeMap::from([
        ("mods".into(), vec!["skip.jar".into()]),
        ("saves".into(), vec!["skip".into()]),
        ("screenshots".into(), vec!["skip.png".into()]),
    ]);
    let plan = f.plan(req.clone());
    assert!(!plan
        .entries
        .keys()
        .any(|path| path.contains("skip") || path.contains("accounts")));
    for suffix in [
        "hotbar.nbt",
        "config/jei/bookmarks.ini",
        "patchouli_data.json",
        "XaeroWaypoints/server/points.txt",
        "screenshots/keep.png",
        "saves/keep/level.dat",
        "saves/empty",
        "servers.dat",
        "scripts/example.zs",
        "other/note.txt",
    ] {
        assert!(
            plan.entries
                .contains_key(&format!("versions/example/{suffix}")),
            "{suffix}"
        );
    }
    execute(plan, &f.destination(), &AtomicBool::new(false), |_| {}).unwrap();
    assert!(unzip(&f.destination()).contains_key(".minecraft/versions/example/saves/empty/"));
    req.checks.mods = false;
    let no_mods = f.plan(req);
    assert!(!no_mods.entries.keys().any(|path| path.contains("/mods/")
        || path.contains("/config/")
        || path.contains("/scripts/")
        || path.contains("Xaero")));
}

#[test]
fn native_export_current_host_classifiers_assets_and_library_rules_are_used() {
    let f = Fixture::new(true);
    let host = pcl_core::platform::Platform::current()
        .unwrap()
        .os
        .minecraft_name();
    let version = json!({"id":"base","clientVersion":"1.20.1","assetIndex":{"id":"fixture"},"libraries":[{"name":"example:native:1","downloads":{"classifiers":{"natives-linux":{"path":"example/native/1/linux.jar"},"natives-windows":{"path":"example/native/1/windows.jar"},"natives-osx":{"path":"example/native/1/osx.jar"}}},"natives":{"linux":"natives-linux","windows":"natives-windows","osx":"natives-osx"}}]});
    f.write(
        "versions/base/base.json",
        &serde_json::to_vec(&version).unwrap(),
    );
    f.write(
        &format!("libraries/example/native/1/{host}.jar"),
        b"host native",
    );
    f.write("assets/indexes/fixture.json", br#"{"objects":{"sound/example.ogg":{"hash":"0123456789012345678901234567890123456789","size":5}}}"#);
    f.write(
        "assets/objects/01/0123456789012345678901234567890123456789",
        b"sound",
    );
    assert!(library_applies(&json!({"rules":[{"action":"allow","os":{"name":host}}]})).unwrap());
    assert!(!library_applies(
        &json!({"rules":[{"action":"allow","os":{"name":"nonexistent-host"}}]})
    )
    .unwrap());
    let mut req = request();
    req.checks.bundle_assets = true;
    let plan = f.plan(req);
    assert!(plan
        .entries
        .contains_key(&format!("libraries/example/native/1/{host}.jar")));
    execute(plan, &f.destination(), &AtomicBool::new(false), |_| {}).unwrap();
    let entries = unzip(&f.destination());
    assert_eq!(
        entries[&format!(".minecraft/libraries/example/native/1/{host}.jar")],
        b"host native"
    );
    assert_eq!(
        entries[".minecraft/assets/objects/01/0123456789012345678901234567890123456789"],
        b"sound"
    );
}

#[test]
fn native_export_source_edits_and_cancellation_clean_only_owned_staging() {
    for cancel_task in [false, true] {
        let f = Fixture::new(true);
        f.game("mods/example.jar", &[b'x'; 300_000], true);
        let cancel = AtomicBool::new(false);
        let fired = AtomicBool::new(false);
        let result = execute(f.plan(request()), &f.destination(), &cancel, |progress| {
            if progress.stage == "export-finalize" && !fired.swap(true, Ordering::SeqCst) {
                if cancel_task {
                    cancel.store(true, Ordering::SeqCst);
                } else {
                    f.game("options.txt", b"external source edit", true);
                }
            }
        });
        assert!(result.is_err());
        assert!(fired.load(Ordering::SeqCst));
        f.assert_no_output();
        if !cancel_task {
            assert_eq!(
                fs::read(f.root.join("versions/example/options.txt")).unwrap(),
                b"external source edit"
            );
        }
    }
}

#[test]
fn native_export_existing_and_late_collision_preserve_target_and_source() {
    for late in [false, true] {
        let f = Fixture::new(true);
        if !late {
            fs::write(f.destination(), b"external target").unwrap();
        }
        let result = execute(
            f.plan(request()),
            &f.destination(),
            &AtomicBool::new(false),
            |progress| {
                if late && progress.stage == "export-finalize" {
                    fs::write(f.destination(), b"external target").unwrap();
                }
            },
        );
        assert!(result.err().unwrap().contains("已经存在"));
        assert_eq!(fs::read(f.destination()).unwrap(), b"external target");
        assert_eq!(fs::read_dir(&f.base).unwrap().count(), 2);
        assert_eq!(
            fs::read(f.root.join("versions/base/base.jar")).unwrap(),
            b"native client fixture"
        );
    }
}

#[test]
fn native_export_flags_path_checks_and_final_authority_are_not_bypassed() {
    let f = Fixture::new(true);
    for flag in ["game", "launcher", "modrinth"] {
        let mut req = request();
        match flag {
            "game" => req.checks.game = false,
            "launcher" => req.checks.launcher = true,
            _ => req.checks.modrinth = true,
        }
        assert!(prepare(&f.root, "example", req).is_err());
    }
    let mut invalid = request();
    invalid
        .excluded
        .insert("saves".into(), vec!["../outside".into()]);
    assert!(prepare(&f.root, "example", invalid).is_err());
    assert!(execute(
        f.plan(request()),
        &f.root.join("inside.zip"),
        &AtomicBool::new(false),
        |_| {}
    )
    .is_err());
    let result = execute_with_guard(
        f.plan(request()),
        &f.destination(),
        &AtomicBool::new(false),
        |_| {},
        || Err::<(), _>("retired authority".into()),
    );
    assert!(result.err().unwrap().contains("retired authority"));
    f.assert_no_output();
    let mut forged = f.plan(request());
    forged.request.checks.launcher = true;
    assert!(execute(forged, &f.destination(), &AtomicBool::new(false), |_| {}).is_err());
    f.assert_no_output();
}

#[test]
fn native_export_completed_zip_crc_and_member_hash_are_rechecked() {
    let f = Fixture::new(true);
    let plan = f.plan(request());
    // A syntactically valid ZIP with the selected member name and size but
    // different data must fail the real verifier; it cannot authorize output.
    let path = f.base.join("wrong.zip");
    let mut zip = ZipWriter::new(
        fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap(),
    );
    let options = SimpleFileOptions::default();
    for (path, entry) in &plan.entries {
        match entry {
            Entry::Directory => zip
                .add_directory(format!(".minecraft/{path}/"), options)
                .unwrap(),
            Entry::File => {
                zip.start_file(format!(".minecraft/{path}"), options)
                    .unwrap();
                zip.write_all(&vec![b'!'; plan.nodes[path].stamp.bytes as usize])
                    .unwrap();
            }
        }
    }
    zip.start_file("pcl-export.json", options).unwrap();
    zip.write_all(&plan.manifest).unwrap();
    let mut wrong = zip.finish().unwrap();
    wrong.sync_all().unwrap();
    assert!(verify_archive(&mut wrong, &plan, &AtomicBool::new(false)).is_err());
    assert!(!f.destination().exists());
    drop(wrong);
    let crc_path = f.base.join("crc.zip");
    let mut zip = ZipWriter::new(
        fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&crc_path)
            .unwrap(),
    );
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (path, entry) in &plan.entries {
        match entry {
            Entry::Directory => zip
                .add_directory(format!(".minecraft/{path}/"), stored)
                .unwrap(),
            Entry::File => {
                zip.start_file(format!(".minecraft/{path}"), stored)
                    .unwrap();
                zip.write_all(&fs::read(f.root.join(path)).unwrap())
                    .unwrap();
            }
        }
    }
    zip.start_file("pcl-export.json", stored).unwrap();
    zip.write_all(&plan.manifest).unwrap();
    let mut file = zip.finish().unwrap();
    let position = {
        let mut archive = zip::ZipArchive::new(file.try_clone().unwrap()).unwrap();
        let entry = archive
            .by_name(".minecraft/versions/base/base.jar")
            .unwrap();
        entry.data_start()
    };
    file.seek(SeekFrom::Start(position)).unwrap();
    file.write_all(b"!").unwrap();
    file.flush().unwrap();
    let error = verify_archive(&mut file, &plan, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(error.contains("CRC"), "{error}");
}

#[cfg(unix)]
#[test]
fn native_export_source_and_destination_symlink_ancestors_are_refused() {
    let f = Fixture::new(true);
    f.directory("versions/example/mods");
    let link = f.root.join("versions/example/mods/link.jar");
    std::os::unix::fs::symlink(f.root.join("versions/base/base.jar"), &link).unwrap();
    assert!(prepare(&f.root, "example", request()).is_err());
    fs::remove_file(link).unwrap();
    let alias = f.base.join("alias");
    std::os::unix::fs::symlink(&f.root, &alias).unwrap();
    assert!(prepare(&alias, "example", request()).is_err());
    let output = f.base.join("output");
    fs::create_dir(&output).unwrap();
    let alias_out = f.base.join("alias-out");
    std::os::unix::fs::symlink(&output, &alias_out).unwrap();
    assert!(execute(
        f.plan(request()),
        &alias_out.join("output.zip"),
        &AtomicBool::new(false),
        |_| {}
    )
    .is_err());
    assert_eq!(fs::read_dir(output).unwrap().count(), 0);
}

#[cfg(windows)]
#[test]
fn native_export_source_and_destination_junction_ancestors_are_refused() {
    let f = Fixture::new(true);
    f.directory("versions/example/mods");
    let target = f.base.join("outside-mods");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("kept.jar"), b"outside original").unwrap();
    let source_link = f.root.join("versions/example/mods/junction");
    junction(&target, &source_link);
    assert!(prepare(&f.root, "example", request()).is_err());
    assert_eq!(
        fs::read(target.join("kept.jar")).unwrap(),
        b"outside original"
    );
    fs::remove_dir(source_link).unwrap();
    let root_alias = f.base.join("root-alias");
    junction(&f.root, &root_alias);
    assert!(prepare(&root_alias, "example", request()).is_err());
    let output_alias = f.base.join("output-alias");
    junction(&target, &output_alias);
    assert!(execute(
        f.plan(request()),
        &output_alias.join("export.zip"),
        &AtomicBool::new(false),
        |_| {}
    )
    .is_err());
    assert!(!target.join("export.zip").exists());
    fs::remove_dir(root_alias).unwrap();
    fs::remove_dir(output_alias).unwrap();
}

/// Private unprivileged mount-point reparse fixture, using the same documented
/// REPARSE_DATA_BUFFER wire layout as the core native boundary tests. No shell,
/// developer mode, administrator grant, or user path is involved.
#[cfg(windows)]
fn junction(target: &Path, link: &Path) {
    use std::{
        ffi::c_void,
        os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
        ptr,
    };
    #[link(name = "kernel32")]
    extern "system" {
        fn DeviceIoControl(
            handle: *mut c_void,
            code: u32,
            input: *const c_void,
            input_size: u32,
            output: *mut c_void,
            output_size: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }
    fs::create_dir(link).unwrap();
    let target = fs::canonicalize(target).unwrap();
    let units: Vec<u16> = target.as_os_str().encode_wide().collect();
    assert!(units.starts_with(&[92, 92, 63, 92]));
    assert_eq!(units.get(5), Some(&58));
    let print = &units[4..];
    let substitute: Vec<u16> = [92, 63, 63, 92]
        .into_iter()
        .chain(print.iter().copied())
        .collect();
    let substitute_bytes = substitute.len() * 2;
    let print_bytes = print.len() * 2;
    let size = 16 + substitute_bytes + 2 + print_bytes + 2;
    assert!(size < 16 * 1024);
    let mut aligned = vec![0u32; size.div_ceil(4)];
    let bytes = unsafe { std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast::<u8>(), size) };
    bytes[0..4].copy_from_slice(&0xA0000003u32.to_le_bytes());
    bytes[4..6].copy_from_slice(&((size - 8) as u16).to_le_bytes());
    bytes[10..12].copy_from_slice(&(substitute_bytes as u16).to_le_bytes());
    bytes[12..14].copy_from_slice(&((substitute_bytes + 2) as u16).to_le_bytes());
    bytes[14..16].copy_from_slice(&(print_bytes as u16).to_le_bytes());
    for (index, unit) in substitute.iter().enumerate() {
        bytes[16 + index * 2..18 + index * 2].copy_from_slice(&unit.to_le_bytes());
    }
    let offset = 18 + substitute_bytes;
    for (index, unit) in print.iter().enumerate() {
        bytes[offset + index * 2..offset + index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(0x02200000)
        .open(link)
        .unwrap();
    let mut returned = 0;
    assert_ne!(
        unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                0x000900a4,
                aligned.as_ptr().cast(),
                size as u32,
                ptr::null_mut(),
                0,
                &mut returned,
                ptr::null_mut(),
            )
        },
        0,
        "junction fixture: {}",
        std::io::Error::last_os_error()
    );
}

#[test]
fn native_export_json_and_file_count_budgets_admit_before_the_actual_hash_reader() {
    let f = Fixture::new(true);
    let path = f.root.join("versions/base/base.json");
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_JSON_BYTES + 1)
        .unwrap();
    let mut scanner = Scanner::new(Dir::open_absolute(&f.root).unwrap()).unwrap();
    HASH_OBSERVATION.with(|observed| observed.set((0, 0)));
    let error = scanner.json("versions/base/base.json").err().unwrap();
    assert!(error.contains("JSON 文件过大"), "{error}");
    assert_eq!(
        HASH_OBSERVATION.with(|observed| observed.get()),
        (0, 0),
        "oversized JSON reached the production hash reader"
    );

    // The scanner's actual cumulative counter is already exhausted. A sparse
    // ordinary file keeps the regression disposable and avoids creating two
    // hundred thousand unrelated files just to exercise the admission point.
    scanner.files = MAX_FILES;
    HASH_OBSERVATION.with(|observed| observed.set((0, 0)));
    let error = scanner.add_file("versions/base/base.json").err().unwrap();
    assert!(error.contains("文件数量过多"), "{error}");
    assert_eq!(
        HASH_OBSERVATION.with(|observed| observed.get()),
        (0, 0),
        "the 200001st file reached the production hash reader"
    );
}

#[test]
fn native_export_maps_alone_traverses_config_without_selecting_other_personal_or_mod_settings() {
    let f = Fixture::new(true);
    for path in [
        "config/journeymap/data/server/tile.png",
        "config/journeymap/settings.json",
        "config/other/settings.toml",
        "config/jei/bookmarks.ini",
        "config/patchouli_data.json",
    ] {
        f.game(path, path.as_bytes(), true);
    }
    let default = f.plan(request());
    assert!(!default
        .entries
        .contains_key("versions/example/config/journeymap/data/server/tile.png"));
    let mut req = request();
    req.checks.maps = true;
    req.checks.mod_settings = false;
    req.checks.jei_personal = false;
    req.checks.guide_personal = false;
    let plan = f.plan(req);
    execute(plan, &f.destination(), &AtomicBool::new(false), |_| {}).unwrap();
    let entries = unzip(&f.destination());
    assert_eq!(
        entries[".minecraft/versions/example/config/journeymap/data/server/tile.png"],
        b"config/journeymap/data/server/tile.png"
    );
    for suffix in [
        "config/journeymap/settings.json",
        "config/other/settings.toml",
        "config/jei/bookmarks.ini",
        "config/patchouli_data.json",
    ] {
        assert!(
            !entries.contains_key(&format!(".minecraft/versions/example/{suffix}")),
            "unselected file leaked: {suffix}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_export_final_authority_cannot_mutate_the_same_staging_inode_then_publish_stale_sha() {
    let f = Fixture::new(true);
    let called = AtomicBool::new(false);
    let result = execute_with_guard(
        f.plan(request()),
        &f.destination(),
        &AtomicBool::new(false),
        |_| {},
        || {
            let temporary = fs::read_dir(&f.base)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".pcl-export-")
                })
                .unwrap();
            let mut file = fs::OpenOptions::new().write(true).open(temporary).unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(b"!").unwrap();
            file.sync_all().unwrap();
            called.store(true, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(called.load(Ordering::SeqCst));
    let error = result
        .err()
        .expect("mutated ZIP must never report the previous successful SHA");
    assert!(
        error.contains("SHA-256") || error.starts_with("MayHavePublished"),
        "{error}"
    );
    f.assert_no_output();
}

#[cfg(target_os = "linux")]
#[test]
fn linux_export_after_rename_integrity_failure_reports_may_and_preserves_the_target() {
    let f = Fixture::new(true);
    let destination = f.destination();
    let _hook = filesystem::after_rename_for_test(move || {
        fs::write(destination, b"post-rename external edit").unwrap()
    });
    let error = execute(
        f.plan(request()),
        &f.destination(),
        &AtomicBool::new(false),
        |_| {},
    )
    .err()
    .expect("post-rename conflict must not report successful old SHA");
    assert!(error.starts_with("MayHavePublished"), "{error}");
    assert_eq!(
        fs::read(f.destination()).unwrap(),
        b"post-rename external edit"
    );
    assert_eq!(
        fs::read(f.root.join("versions/base/base.jar")).unwrap(),
        b"native client fixture"
    );
}
