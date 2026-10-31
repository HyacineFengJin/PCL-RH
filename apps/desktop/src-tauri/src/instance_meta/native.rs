//! The fixed launcher-owned document uses the existing native CAS backend.
//! Darwin exchanges actual directory entries; Windows reads immutable logical
//! revisions while retaining the original physical file as its permanent fence.
use super::{FileToken, FILE_NAME, LOCK_NAME, MAX_FILE_BYTES};
use crate::launcher_local::filesystem::{optional_snapshot, Dir, Snapshot};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) type Identity = crate::launcher_local::filesystem::DirectoryIdentity;
pub(super) type FileStamp = crate::launcher_local::filesystem::Stamp;
pub(super) type WriteLock = crate::launcher_local::filesystem::WriteLock;
pub(super) struct Directory(Dir);

fn token(snapshot: &Snapshot, bytes: &[u8]) -> FileToken {
    FileToken {
        stamp: snapshot.stamp.clone(),
        digest: Sha256::digest(bytes).into(),
    }
}
impl Directory {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        Dir::absolute(path).map(Self)
    }
    pub(super) fn identity(&self) -> Result<Identity, String> {
        self.0.identity()
    }
    pub(super) fn storage(&self) -> Result<Option<Self>, String> {
        self.0
            .optional(".pcl-rust")
            .map(|directory| directory.map(Self))
    }
    pub(super) fn create_storage(&self) -> Result<Self, String> {
        self.0.create(".pcl-rust").map(Self)
    }
    pub(super) fn write_lock(&self) -> Result<WriteLock, String> {
        self.0
            .lock(LOCK_NAME)
            .map_err(|error| format!("另一个启动器正在保存实例资料，或元数据写锁无效：{error}"))
    }
    pub(super) fn read_file(&self) -> Result<Option<(FileToken, Vec<u8>)>, String> {
        let Some(mut snapshot) = optional_snapshot(&self.0, FILE_NAME, MAX_FILE_BYTES as u64)?
        else {
            return Ok(None);
        };
        let bytes = snapshot.bytes(&self.0, FILE_NAME)?;
        Ok(Some((token(&snapshot, &bytes), bytes)))
    }
    pub(super) fn replace(&self, expected: Option<&FileToken>, bytes: &[u8]) -> Result<(), String> {
        let mut snapshot = optional_snapshot(&self.0, FILE_NAME, MAX_FILE_BYTES as u64)?;
        let fresh = snapshot
            .as_mut()
            .map(|snapshot| {
                let bytes = snapshot.bytes(&self.0, FILE_NAME)?;
                Ok::<_, String>(token(snapshot, &bytes))
            })
            .transpose()?;
        if fresh.as_ref() != expected {
            return Err("实例元数据文件已由外部修改，原件已保留".into());
        }
        self.0
            .replace(FILE_NAME, snapshot.as_ref(), bytes, MAX_FILE_BYTES as u64)
    }
}
