//! Retained launcher-owned host resources and the real native single-host lock.
//! Existing Linux data directories retain their original permissions; no
//! permission repair or recursive pathname writer is used here.
use pcl_core::retained_fs::{self, Directory, Kind, Leaf, Links, Lock, Metadata, Traversal};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

pub(super) struct Lease {
    project_path: PathBuf,
    data_path: PathBuf,
    project: Directory,
    app: Directory,
    data: Directory,
    resources: Directory,
    script: File,
    script_stamp: Metadata,
    lock: Lock,
    lock_stamp: Metadata,
}
fn leaf(name: &str) -> io::Result<Leaf> {
    Leaf::new(name)
}
fn app_security(directory: &Directory) -> io::Result<()> {
    let metadata = directory.metadata()?;
    metadata.require_owned()?;
    #[cfg(unix)]
    {
        // The app container also holds installed binaries. Read/traverse
        // access is compatible with old 0755 archives, but foreign writes or
        // ACL grants that cannot be judged from POSIX bits are not admitted.
        match metadata.security {
            retained_fs::Security::Unix {
                mode,
                extended_acl: false,
                ..
            } if mode & 0o022 == 0 => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Experimental app container permits foreign writes or has an unsupported ACL",
            )),
        }
    }
    #[cfg(windows)]
    {
        // The native SD is authoritative. The shared metadata capability only
        // proves the stricter private policy, so retain it conservatively.
        metadata.require_private()
    }
}
fn data_security(directory: &Directory) -> io::Result<()> {
    let metadata = directory.metadata()?;
    #[cfg(target_os = "linux")]
    metadata.require_owned()?;
    #[cfg(not(target_os = "linux"))]
    metadata.require_private()?;
    Ok(())
}

fn lock_entry_matches(entry: &Metadata, held: &Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        // Darwin's nofollow entry snapshot deliberately has no ACL evidence.
        // Only the held descriptor supplies that field; all stat fields must
        // still agree. Private admission is checked on the held metadata.
        let mut entry = entry.clone();
        match (&mut entry.security, &held.security) {
            (
                retained_fs::Security::Unix { extended_acl, .. },
                retained_fs::Security::Unix {
                    extended_acl: actual,
                    ..
                },
            ) => *extended_acl = *actual,
            _ => return false,
        }
        entry == *held
    }
    #[cfg(not(target_os = "macos"))]
    {
        entry == held
    }
}

/// A read-only proof for the passive empty projection. Existing store entries
/// (including links, invalid kinds and invalid JSON) always belong to Node.
pub(super) fn extensions_absent(project_path: &Path) -> io::Result<bool> {
    match ExtensionAbsence::capture(project_path)? {
        Some(proof) => proof.recheck(),
        None => Ok(false),
    }
}
struct ExtensionAbsence {
    path: PathBuf,
    project: Directory,
    app: Option<Directory>,
    data: Option<Directory>,
}
impl ExtensionAbsence {
    fn capture(path: &Path) -> io::Result<Option<Self>> {
        let project = Directory::open_absolute(path)?;
        let mut proof = Self {
            path: path.to_owned(),
            project,
            app: None,
            data: None,
        };
        let name = leaf(".pcl-rust")?;
        if proof.project.entry(&name)?.is_none() {
            return Ok(Some(proof));
        }
        let app = proof.project.child(&name, Traversal::SameFilesystem)?;
        app_security(&app)?;
        proof.project.recheck(&name, &app.identity()?)?;
        proof.app = Some(app);
        let app = proof.app.as_ref().unwrap();
        let name = leaf("experimental")?;
        if app.entry(&name)?.is_none() {
            return Ok(Some(proof));
        }
        let data = app.child(&name, Traversal::SameFilesystem)?;
        data_security(&data)?;
        app.recheck(&name, &data.identity()?)?;
        proof.data = Some(data);
        if proof
            .data
            .as_ref()
            .unwrap()
            .entry(&leaf("extensions.json")?)?
            .is_some()
        {
            return Ok(None);
        }
        Ok(Some(proof))
    }
    fn carriers(&self) -> io::Result<()> {
        if Directory::open_absolute(&self.path)?.identity()? != self.project.identity()? {
            return Err(io::Error::other("Experimental project directory changed"));
        }
        if let Some(app) = &self.app {
            self.project
                .recheck(&leaf(".pcl-rust")?, &app.identity()?)?;
            app_security(app)?;
            if let Some(data) = &self.data {
                app.recheck(&leaf("experimental")?, &data.identity()?)?;
                data_security(data)?;
            }
        }
        Ok(())
    }
    fn missing(&self) -> io::Result<bool> {
        if let Some(data) = &self.data {
            return Ok(data.entry(&leaf("extensions.json")?)?.is_none());
        }
        if let Some(app) = &self.app {
            return Ok(app.entry(&leaf("experimental")?)?.is_none());
        }
        Ok(self.project.entry(&leaf(".pcl-rust")?)?.is_none())
    }
    fn recheck(&self) -> io::Result<bool> {
        self.carriers()?;
        if !self.missing()? {
            return Ok(false);
        }
        if let Some(data) = &self.data {
            let name = leaf("host.lock")?;
            if let Some(before) = data.entry(&name)? {
                // Match Lease::recheck explicitly, including legacy Linux data
                // directories whose own policy is owned rather than private.
                let file = data.open_regular(&name, Links::Single)?;
                let held_before = retained_fs::metadata(&file)?;
                held_before.require_private()?;
                if !lock_entry_matches(&before, &held_before) {
                    return Err(io::Error::other("Experimental host lock changed"));
                }
                let held = retained_fs::shared_lock_is_held(&file)?;
                data.recheck(&name, &before.identity)?;
                if retained_fs::metadata(&file)? != held_before {
                    return Err(io::Error::other("Experimental host lock changed"));
                }
                if held {
                    return Ok(false);
                }
            }
        }
        // Every missing level shares the same posterior graph/security check;
        // a nofollow ENOENT alone never proves a rebound project/app absent.
        self.carriers()?;
        let absent = self.missing()?;
        self.carriers()?;
        Ok(absent)
    }
}
fn ensure(
    parent: &Directory,
    name: &str,
    security: fn(&Directory) -> io::Result<()>,
) -> io::Result<Directory> {
    let name = leaf(name)?;
    let directory = match parent.child(&name, Traversal::SameFilesystem) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match parent.create_private(&name, Traversal::SameFilesystem) {
                Ok(directory) => {
                    parent.sync()?;
                    directory
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    parent.child(&name, Traversal::SameFilesystem)?
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    security(&directory)?;
    parent.recheck(&name, &directory.identity()?)?;
    Ok(directory)
}
impl Lease {
    pub(super) fn open(project_path: &Path) -> io::Result<Self> {
        let project = Directory::open_absolute(project_path)?;
        let app = ensure(&project, ".pcl-rust", app_security)?;
        let data = ensure(&app, "experimental", data_security)?;
        let lock = data.lock(&leaf("host.lock")?)?;
        let lock_stamp = retained_fs::metadata(lock.file())?;
        let resources = project.child(&leaf("experimental")?, Traversal::SameFilesystem)?;
        let script = resources.open_regular(&leaf("host.mjs")?, Links::Single)?;
        let script_stamp = retained_fs::metadata(&script)?;
        let lease = Self {
            project_path: project_path.to_owned(),
            data_path: project_path.join(".pcl-rust/experimental"),
            project,
            app,
            data,
            resources,
            script,
            script_stamp,
            lock,
            lock_stamp,
        };
        lease.recheck()?;
        Ok(lease)
    }
    pub(super) fn project_path(&self) -> &Path {
        &self.project_path
    }
    pub(super) fn data_path(&self) -> &Path {
        &self.data_path
    }
    pub(super) fn script_path(&self) -> PathBuf {
        self.project_path.join("experimental/host.mjs")
    }
    pub(super) fn recheck(&self) -> io::Result<()> {
        if Directory::open_absolute(&self.project_path)?.identity()? != self.project.identity()? {
            return Err(io::Error::other(
                "Experimental host project directory changed",
            ));
        }
        self.project
            .recheck(&leaf(".pcl-rust")?, &self.app.identity()?)?;
        self.app
            .recheck(&leaf("experimental")?, &self.data.identity()?)?;
        self.project
            .recheck(&leaf("experimental")?, &self.resources.identity()?)?;
        app_security(&self.app)?;
        data_security(&self.data)?;
        let lock = retained_fs::metadata(self.lock.file())?;
        lock.require_private()?;
        if lock != self.lock_stamp || lock.kind != Kind::Regular || lock.links != 1 {
            return Err(io::Error::other("Experimental host lock changed"));
        }
        self.data.recheck(&leaf("host.lock")?, &lock.identity)?;
        if retained_fs::metadata(&self.script)? != self.script_stamp {
            return Err(io::Error::other("Experimental host script changed"));
        }
        self.resources
            .recheck(&leaf("host.mjs")?, &self.script_stamp.identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("project");
        fs::create_dir(&path).unwrap();
        (temporary, path.canonicalize().unwrap())
    }
    fn data(path: &Path) -> (Directory, Directory, Directory) {
        let project = Directory::open_absolute(path).unwrap();
        let app = project
            .create_private(&leaf(".pcl-rust").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let data = app
            .create_private(&leaf("experimental").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        (project, app, data)
    }

    #[test]
    fn passive_absence_is_readonly_at_each_missing_level_and_existing_bytes_are_not_parsed() {
        let (_temporary, path) = fixture();
        assert!(extensions_absent(&path).unwrap());
        assert!(!path.join(".pcl-rust").exists());
        let project = Directory::open_absolute(&path).unwrap();
        let app = project
            .create_private(&leaf(".pcl-rust").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        assert!(extensions_absent(&path).unwrap());
        assert!(!path.join(".pcl-rust/experimental").exists());
        let data = app
            .create_private(&leaf("experimental").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        assert!(extensions_absent(&path).unwrap());
        assert!(data.names(1).unwrap().is_empty());
        let mut file = data.create_file(&leaf("extensions.json").unwrap()).unwrap();
        assert!(
            !extensions_absent(&path).unwrap(),
            "an existing empty file still belongs to Node"
        );
        file.write_all(b"not valid JSON or a permission schema")
            .unwrap();
        assert!(!extensions_absent(&path).unwrap());
        assert_eq!(
            fs::read(path.join(".pcl-rust/experimental/extensions.json")).unwrap(),
            b"not valid JSON or a permission schema"
        );
        assert!(!path.join(".pcl-rust/experimental/host.lock").exists());
    }

    #[test]
    fn absent_proofs_reject_project_app_and_data_path_substitution() {
        for missing in ["app", "data", "store"] {
            let (_temporary, path) = fixture();
            let project = Directory::open_absolute(&path).unwrap();
            if missing != "app" {
                let app = project
                    .create_private(&leaf(".pcl-rust").unwrap(), Traversal::SameFilesystem)
                    .unwrap();
                if missing == "store" {
                    app.create_private(&leaf("experimental").unwrap(), Traversal::SameFilesystem)
                        .unwrap();
                }
            }
            let proof = ExtensionAbsence::capture(&path).unwrap().unwrap();
            let target = match missing {
                "app" => path.clone(),
                "data" => path.join(".pcl-rust"),
                _ => path.join(".pcl-rust/experimental"),
            };
            let retained = target.with_file_name(format!("retained-{missing}"));
            fs::rename(&target, &retained).unwrap();
            fs::create_dir(&target).unwrap();
            assert!(
                proof.recheck().is_err(),
                "rebound {missing} was treated as empty"
            );
            assert!(!target.join("host.lock").exists());
        }
    }

    #[test]
    fn appearing_store_or_a_live_existing_lease_cannot_be_reported_empty() {
        let (_temporary, path) = fixture();
        let project = Directory::open_absolute(&path).unwrap();
        let resources = project
            .create_private(&leaf("experimental").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        resources
            .create_file(&leaf("host.mjs").unwrap())
            .unwrap()
            .write_all(b"// never executed")
            .unwrap();
        let lease = Lease::open(&path).unwrap();
        assert!(
            !extensions_absent(&path).unwrap(),
            "a different live native lease was hidden"
        );
        drop(lease);
        assert!(
            extensions_absent(&path).unwrap(),
            "an unlocked existing lock is not a live session"
        );
        let proof = ExtensionAbsence::capture(&path).unwrap().unwrap();
        let app = project
            .child(&leaf(".pcl-rust").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let data = app
            .child(&leaf("experimental").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        data.create_file(&leaf("extensions.json").unwrap()).unwrap();
        assert!(!proof.recheck().unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn lock_entry_unknown_acl_uses_private_held_descriptor_evidence() {
        let (_temporary, path) = fixture();
        let (_project, _app, data) = data(&path);
        let name = leaf("host.lock").unwrap();
        let file = data.create_file(&name).unwrap();
        let entry = data.entry(&name).unwrap().unwrap();
        let held = retained_fs::metadata(&file).unwrap();
        assert!(matches!(
            &entry.security,
            retained_fs::Security::Unix {
                extended_acl: true,
                ..
            }
        ));
        assert_eq!(
            entry.require_private().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        held.require_private().unwrap();
        assert!(lock_entry_matches(&entry, &held));
        assert!(extensions_absent(&path).unwrap());
    }

    #[test]
    fn lock_entry_comparison_keeps_all_other_metadata_evidence() {
        let (_temporary, path) = fixture();
        let (_project, _app, data) = data(&path);
        let file = data.create_file(&leaf("host.lock").unwrap()).unwrap();
        let held = retained_fs::metadata(&file).unwrap();
        let mut changed = vec![held.clone(); 9];
        match &mut changed[0].identity {
            retained_fs::Identity::Unix { inode, .. } => *inode ^= 1,
            retained_fs::Identity::Windows { file, .. } => file[0] ^= 1,
        }
        changed[1].kind = Kind::Directory;
        changed[2].len ^= 1;
        changed[3].links ^= 1;
        changed[4].modified.nanos = (held.modified.nanos + 1) % 1_000_000_000;
        changed[5].changed.nanos = (held.changed.nanos + 1) % 1_000_000_000;
        match &mut changed[6].security {
            retained_fs::Security::Unix { owner, .. } => *owner ^= 1,
            retained_fs::Security::Windows { owner, .. } => owner.push(0),
        }
        match &mut changed[7].security {
            retained_fs::Security::Unix { mode, .. } => *mode ^= 1,
            retained_fs::Security::Windows { descriptor, .. } => descriptor.push(0),
        }
        changed[8].reparse_tag = Some(1);
        for stamp in changed {
            assert!(!lock_entry_matches(&stamp, &held), "{stamp:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_carriers_and_foreign_writable_app_are_not_absence() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let (_temporary, path) = fixture();
        symlink(path.join("missing-target"), path.join(".pcl-rust")).unwrap();
        assert!(extensions_absent(&path).is_err());
        fs::remove_file(path.join(".pcl-rust")).unwrap();
        let (_project, _app, data) = data(&path);
        symlink(
            path.join("missing-store"),
            path.join(".pcl-rust/experimental/extensions.json"),
        )
        .unwrap();
        assert!(!extensions_absent(&path).unwrap());
        fs::remove_file(path.join(".pcl-rust/experimental/extensions.json")).unwrap();
        fs::set_permissions(path.join(".pcl-rust"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(extensions_absent(&path).is_err());
        assert!(data.names(1).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_weak_host_lock_cannot_be_hidden_by_an_absent_store() {
        use std::os::unix::fs::PermissionsExt;
        let (_temporary, path) = fixture();
        let (_project, _app, data) = data(&path);
        data.create_file(&leaf("host.lock").unwrap()).unwrap();
        let lock = path.join(".pcl-rust/experimental/host.lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        let error = extensions_absent(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read(&lock).unwrap(), b"");
        assert!(!path.join(".pcl-rust/experimental/extensions.json").exists());
    }
}
