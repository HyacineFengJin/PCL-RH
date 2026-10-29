//! Native, unprivileged junction fixture. No shell, symlink privilege, external
//! tools or system settings; both junction and referent belong to a TempDir.
use std::{
    fs, io,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT},
    System::IO::DeviceIoControl,
};

pub(super) fn create(target: &Path, link: &Path) {
    let target = fs::canonicalize(target).unwrap();
    let units: Vec<u16> = target.as_os_str().encode_wide().collect();
    assert!(units.starts_with(&[92, 92, 63, 92])); // canonical local Win32 extended path
    assert_eq!(units.get(5), Some(&58)); // drive letter, never a remote share
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
    // REPARSE_DATA_BUFFER, mount-point variant. All offsets are byte offsets
    // within PathBuffer; the two UTF-16 names are individually NUL terminated.
    bytes[0..4].copy_from_slice(&0xA0000003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    bytes[4..6].copy_from_slice(&((size - 8) as u16).to_le_bytes());
    bytes[10..12].copy_from_slice(&(substitute_bytes as u16).to_le_bytes());
    bytes[12..14].copy_from_slice(&((substitute_bytes + 2) as u16).to_le_bytes());
    bytes[14..16].copy_from_slice(&(print_bytes as u16).to_le_bytes());
    for (i, unit) in substitute.iter().enumerate() {
        bytes[16 + i * 2..18 + i * 2].copy_from_slice(&unit.to_le_bytes());
    }
    let print_offset = 18 + substitute_bytes;
    for (i, unit) in print.iter().enumerate() {
        bytes[print_offset + i * 2..print_offset + i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    fs::create_dir(link).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)
        .unwrap();
    let mut returned = 0;
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            0x000900a4, // FSCTL_SET_REPARSE_POINT
            aligned.as_ptr().cast(),
            size as u32,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    };
    assert_ne!(ok, 0, "junction fixture: {}", io::Error::last_os_error());
}
