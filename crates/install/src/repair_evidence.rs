//! Native identity evidence for dependency repair journals. Unix records retain
//! their original JSON shape; Windows records use the complete volume/FileId128
//! pair rather than invented device/inode values. Only held handles produce
//! evidence. These stamps do not grant authority or replace content hashes.
use pcl_core::retained_fs::{self, Identity, Kind, Metadata};
use serde::{Deserialize, Serialize};
use std::{fs::File, io};

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum WindowsFormat {
    #[serde(rename = "windows-file-id-128-v1")]
    FileId128V1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Key {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    format: WindowsFormat,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
}
impl Key {
    pub(super) fn of(file: &File) -> io::Result<Self> {
        Self::from_metadata(&retained_fs::metadata(file)?)
    }
    pub(super) fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        if metadata.kind != Kind::Directory || metadata.reparse_tag.is_some() {
            return Err(io::Error::other(
                "Repair evidence requires a literal directory",
            ));
        }
        Self::from_identity(&metadata.identity)
    }
    fn from_identity(identity: &Identity) -> io::Result<Self> {
        match identity {
            #[cfg(unix)]
            Identity::Unix { device, inode } => Ok(Self {
                dev: *device,
                ino: *inode,
            }),
            #[cfg(windows)]
            Identity::Windows { volume, file } => Ok(Self {
                format: WindowsFormat::FileId128V1,
                volume: *volume,
                file_id: *file,
            }),
            _ => Err(io::Error::other(
                "Repair identity belongs to another platform",
            )),
        }
    }
    pub(super) fn matches(&self, metadata: &Metadata) -> bool {
        Self::from_metadata(metadata).is_ok_and(|current| current == *self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stamp {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    format: WindowsFormat,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
    pub(super) len: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}
impl Stamp {
    pub(super) fn of(file: &File) -> io::Result<Self> {
        Self::from_metadata(&retained_fs::metadata(file)?)
    }
    pub(super) fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        if metadata.kind != Kind::Regular || metadata.reparse_tag.is_some() {
            return Err(io::Error::other(
                "Repair evidence requires a literal regular file",
            ));
        }
        let key = Key::from_identity(&metadata.identity)?;
        Ok(Self {
            #[cfg(unix)]
            dev: key.dev,
            #[cfg(unix)]
            ino: key.ino,
            #[cfg(windows)]
            format: key.format,
            #[cfg(windows)]
            volume: key.volume,
            #[cfg(windows)]
            file_id: key.file_id,
            len: metadata.len,
            mtime: metadata.modified.seconds,
            mtime_nsec: i64::from(metadata.modified.nanos),
            ctime: metadata.changed.seconds,
            ctime_nsec: i64::from(metadata.changed.nanos),
        })
    }
    pub(super) fn same_object(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.dev == other.dev && self.ino == other.ino
        }
        #[cfg(windows)]
        {
            self.format == other.format
                && self.volume == other.volume
                && self.file_id == other.file_id
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcl_core::retained_fs::{Directory, Leaf, Traversal};
    use std::{fs, io::Write, path::Path};
    fn fixture() -> (tempfile::TempDir, Directory) {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repository = manifest
            .ancestors()
            .find(|p| p.join("Cargo.lock").is_file() && p.join("crates/install").is_dir())
            .unwrap();
        let work = repository.join("work/repair-native-evidence-tests");
        fs::create_dir_all(&work).unwrap();
        let temp = tempfile::tempdir_in(fs::canonicalize(work).unwrap()).unwrap();
        let root = Directory::open_absolute(temp.path()).unwrap();
        let directory = root
            .create_private(&Leaf::new("private").unwrap(), Traversal::SameFilesystem)
            .unwrap();
        (temp, directory)
    }
    #[test]
    fn handle_evidence_survives_parent_rename_and_rejects_replacement() {
        let (temp, directory) = fixture();
        let before = Key::of(directory.file()).unwrap();
        fs::rename(temp.path().join("private"), temp.path().join("held")).unwrap();
        fs::create_dir(temp.path().join("private")).unwrap();
        let replacement = Directory::open_absolute(&temp.path().join("private")).unwrap();
        assert_eq!(before, Key::of(directory.file()).unwrap());
        assert!(!before.matches(&replacement.metadata().unwrap()));
        let mut file = directory
            .create_file(&Leaf::new("content").unwrap())
            .unwrap();
        let empty = Stamp::of(&file).unwrap();
        file.write_all(b"bytes").unwrap();
        let written = Stamp::of(&file).unwrap();
        assert!(empty.same_object(&written));
        assert_ne!(empty, written);
        assert_eq!(written.len, 5);
        assert_eq!(
            written,
            serde_json::from_slice(&serde_json::to_vec(&written).unwrap()).unwrap()
        );
        assert!(Key::of(&file).is_err());
        assert!(Stamp::of(directory.file()).is_err());
    }
    #[test]
    fn wire_identity_is_native_strict_and_backward_compatible() {
        let (_temp, directory) = fixture();
        let mut file = directory
            .create_file(&Leaf::new("content").unwrap())
            .unwrap();
        file.write_all(b"hello").unwrap();
        let stamp = Stamp::of(&file).unwrap();
        let key = Key::of(directory.file()).unwrap();
        let mut value = serde_json::to_value(stamp).unwrap();
        #[cfg(unix)]
        {
            assert_eq!(
                serde_json::to_value(key).unwrap(),
                serde_json::json!({"dev": key.dev, "ino": key.ino})
            );
            assert_eq!(
                value,
                serde_json::json!({"dev": stamp.dev, "ino": stamp.ino, "len": 5, "mtime": stamp.mtime, "mtime_nsec": stamp.mtime_nsec, "ctime": stamp.ctime, "ctime_nsec": stamp.ctime_nsec})
            );
            let old =
                r#"{"dev":42,"ino":7,"len":5,"mtime":1,"mtime_nsec":2,"ctime":3,"ctime_nsec":4}"#;
            assert_eq!(
                serde_json::to_string(&serde_json::from_str::<Stamp>(old).unwrap()).unwrap(),
                old
            );
            assert!(serde_json::from_str::<Key>(r#"{"format":"windows-file-id-128-v1","volume":42,"file_id":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#).is_err());
        }
        #[cfg(windows)]
        {
            let identity = retained_fs::metadata(&file).unwrap().identity;
            let Identity::Windows { volume, file } = identity else {
                panic!("Windows native identity missing")
            };
            assert_eq!(value["format"], "windows-file-id-128-v1");
            assert_eq!(value["volume"], volume);
            assert_eq!(value["file_id"], serde_json::json!(file));
            assert!(value.get("dev").is_none() && value.get("ino").is_none());
            let high = Key::from_identity(&Identity::Windows {
                volume,
                file: [255; 16],
            })
            .unwrap();
            let mut changed = [255; 16];
            changed[15] = 254;
            assert_ne!(
                high,
                Key::from_identity(&Identity::Windows {
                    volume,
                    file: changed
                })
                .unwrap()
            );
            assert_eq!(
                high,
                serde_json::from_slice(&serde_json::to_vec(&high).unwrap()).unwrap()
            );
            assert!(serde_json::from_str::<Key>(r#"{"dev":42,"ino":7}"#).is_err());
            assert_eq!(key.format, WindowsFormat::FileId128V1);
        }
        value["unexpected"] = true.into();
        assert!(serde_json::from_value::<Stamp>(value).is_err());
    }
}
