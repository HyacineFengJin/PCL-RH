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
