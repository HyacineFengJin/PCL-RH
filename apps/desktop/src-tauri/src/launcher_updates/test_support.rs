//! Native-owned fixture files only; no real installation or downloaded program runs.
use super::*;
use pcl_core::retained_fs::{Directory, Identity, Leaf, Traversal};
use std::{
    fs::{self, File},
    io::Write,
};
pub(super) fn tree() -> (PathBuf, tempfile::TempDir) {
    let temporary = tempfile::tempdir().unwrap();
    let parent = Directory::open_absolute(&temporary.path().canonicalize().unwrap()).unwrap();
    let root = parent
        .create_private(&Leaf::new("fixture").unwrap(), Traversal::SameFilesystem)
        .unwrap();
    let storage = root
        .create_private(&Leaf::new(".pcl-rust").unwrap(), Traversal::SameFilesystem)
        .unwrap();
    storage
        .create_private(&Leaf::new("bin").unwrap(), Traversal::SameFilesystem)
        .unwrap();
    (
        temporary.path().join("fixture").canonicalize().unwrap(),
        temporary,
    )
}
pub(super) fn write_new(path: &Path, bytes: &[u8]) {
    let directory = Directory::open_absolute(path.parent().unwrap()).unwrap();
    let mut file = directory
        .create_file(&Leaf::new(path.file_name().unwrap()).unwrap())
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
pub(super) fn identity(path: impl AsRef<Path>) -> Identity {
    pcl_core::retained_fs::metadata(&File::open(path).unwrap())
        .unwrap()
        .identity
}
pub(super) fn links(path: impl AsRef<Path>) -> u64 {
    pcl_core::retained_fs::metadata(&File::open(path).unwrap())
        .unwrap()
        .links
}
pub(super) fn no_partial(staging: &Path) {
    if staging.exists() {
        #[cfg(target_os = "linux")]
        assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(files::pending(staging).unwrap(), 0);
    }
}
#[cfg(unix)]
pub(super) use std::os::unix::fs::symlink as symlink_file;
#[cfg(unix)]
pub(super) use std::os::unix::fs::symlink as symlink_dir;
#[cfg(windows)]
pub(super) use std::os::windows::fs::{symlink_dir, symlink_file};
pub(super) fn unlink_dir_alias(path: &Path) {
    #[cfg(unix)]
    fs::remove_file(path).unwrap();
    #[cfg(windows)]
    fs::remove_dir(path).unwrap();
}
#[cfg(windows)]
pub(super) fn native_binary(a: Architecture) -> Vec<u8> {
    let mut b = vec![0u8; 640];
    b[..2].copy_from_slice(b"MZ");
    b[60..64].copy_from_slice(&64u32.to_le_bytes());
    b[64..68].copy_from_slice(b"PE\0\0");
    let machine: u16 = if a == Architecture::Aarch64 {
        0xaa64
    } else {
        0x8664
    };
    b[68..70].copy_from_slice(&machine.to_le_bytes());
    b[70..72].copy_from_slice(&1u16.to_le_bytes());
    b[84..86].copy_from_slice(&112u16.to_le_bytes());
    b[86..88].copy_from_slice(&2u16.to_le_bytes());
    b[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
    b[104..108].copy_from_slice(&0x1000u32.to_le_bytes());
    b[144..148].copy_from_slice(&0x2000u32.to_le_bytes());
    b[148..152].copy_from_slice(&512u32.to_le_bytes());
    b[208..212].copy_from_slice(&128u32.to_le_bytes());
    b[212..216].copy_from_slice(&0x1000u32.to_le_bytes());
    b[216..220].copy_from_slice(&128u32.to_le_bytes());
    b[220..224].copy_from_slice(&512u32.to_le_bytes());
    b[236..240].copy_from_slice(&0x20000000u32.to_le_bytes());
    b
}
#[cfg(target_os = "macos")]
pub(super) fn native_binary(a: Architecture) -> Vec<u8> {
    let mut b = vec![0u8; 256];
    b[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    let cpu: u32 = if a == Architecture::Aarch64 {
        0x0100000c
    } else {
        0x01000007
    };
    b[4..8].copy_from_slice(&cpu.to_le_bytes());
    b[12..16].copy_from_slice(&2u32.to_le_bytes());
    b[16..20].copy_from_slice(&2u32.to_le_bytes());
    b[20..24].copy_from_slice(&96u32.to_le_bytes());
    b[32..36].copy_from_slice(&0x19u32.to_le_bytes());
    b[36..40].copy_from_slice(&72u32.to_le_bytes());
    b[64..72].copy_from_slice(&256u64.to_le_bytes());
    b[80..88].copy_from_slice(&256u64.to_le_bytes());
    b[92..96].copy_from_slice(&5u32.to_le_bytes());
    b[104..108].copy_from_slice(&0x80000028u32.to_le_bytes());
    b[108..112].copy_from_slice(&24u32.to_le_bytes());
    b[112..120].copy_from_slice(&200u64.to_le_bytes());
    b
}
