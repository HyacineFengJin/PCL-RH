//! The repair writer lock retains Unix directory-flock interoperability. The
//! Windows lock is a private literal file, owned until explicit UnlockFileEx.
use pcl_core::retained_fs;
#[cfg(any(windows, test))]
use pcl_core::retained_fs::{Directory, Leaf};
use std::{fs::File, io};

#[cfg(windows)]
pub(super) const NAME: &str = ".writer.lock";

pub(super) struct Lease {
    #[cfg(unix)]
    file: File,
    #[cfg(windows)]
    lock: retained_fs::Lock,
}
impl Lease {
    pub(super) fn acquire(directory: &File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let file = directory.try_clone()?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { file })
        }
        #[cfg(windows)]
        {
            let directory = Directory::from_file(directory.try_clone()?)?;
            let lock = directory.lock(&Leaf::new(NAME)?)?;
            Ok(Self { lock })
        }
    }
    pub(super) fn verify(&self, directory: &File) -> io::Result<()> {
        #[cfg(unix)]
        {
            let current = retained_fs::metadata(directory)?;
            let held = retained_fs::metadata(&self.file)?;
            if !current.same_object(&held) {
                return Err(io::Error::other("Repair lock directory identity changed"));
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            let directory = Directory::from_file(directory.try_clone()?)?;
            let held = retained_fs::metadata(self.lock.file())?;
            held.require_private()?;
            directory.recheck(&Leaf::new(NAME)?, &held.identity)
        }
    }
}
#[cfg(unix)]
impl Drop for Lease {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // Duplicated FDs must not keep the lock alive after its logical lease.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Recognize only the native lock file, never skip arbitrary unknown records.
pub(super) fn reserved_name(directory: &File, name: &str) -> io::Result<bool> {
    #[cfg(windows)]
    {
        if name == NAME {
            let file = Directory::from_file(directory.try_clone()?)?
                .open_regular(&Leaf::new(NAME)?, retained_fs::Links::Single)?;
            let metadata = retained_fs::metadata(&file)?;
            metadata.require_private()?;
            if metadata.len != 0 {
                return Err(io::Error::other("Repair lock has unexpected contents"));
            }
            return Ok(true);
        }
    }
    #[cfg(unix)]
    let _ = (directory, name);
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use retained_fs::Traversal;
    use std::{fs, path::Path};
    #[test]
    fn independent_writer_conflicts_and_explicit_release_allows_reopen() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest
            .ancestors()
            .find(|p| p.join("Cargo.lock").is_file() && p.join("crates/install").is_dir())
            .unwrap();
        let work = root.join("work/repair-native-lock-tests");
        fs::create_dir_all(&work).unwrap();
        let temp = tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap();
        let root = Directory::open_absolute(temp.path()).unwrap();
        let directory = root
            .create_private(&Leaf::new("store").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        let independent = directory.reopen(Traversal::SameFilesystem).unwrap();
        let lease = Lease::acquire(directory.file()).unwrap();
        lease.verify(directory.file()).unwrap();
        assert!(Lease::acquire(independent.file()).is_err());
        #[cfg(unix)]
        let duplicate = directory.try_clone_file().unwrap();
        #[cfg(windows)]
        assert!(reserved_name(directory.file(), NAME).unwrap());
        assert!(!reserved_name(directory.file(), "unknown-record").unwrap());
        drop(lease);
        let second = Lease::acquire(independent.file()).unwrap();
        second.verify(directory.file()).unwrap();
        drop(second);
        #[cfg(unix)]
        drop(duplicate);
    }
}
