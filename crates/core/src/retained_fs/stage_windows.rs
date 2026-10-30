use super::*;
use std::{
    mem::{offset_of, size_of},
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    ptr,
};
use windows_sys::{
    Wdk::Storage::FileSystem::{
        FileRenameInformation, NtSetInformationFile, FILE_RENAME_INFORMATION,
    },
    Win32::{
        Foundation::RtlNtStatusToDosError,
        Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
        System::IO::IO_STATUS_BLOCK,
    },
};

pub(super) const SOURCE_KIND: SourceKind = SourceKind::ExclusiveHandleLease;
pub(super) const INITIAL_LINKS: u64 = 1;

fn native_error(status: i32) -> io::Error {
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}

pub(super) fn new(parent: &Directory) -> io::Result<(File, Option<Leaf>)> {
    for _ in 0..32 {
        let mut entropy = [0u8; 16];
        let status = unsafe {
            BCryptGenRandom(
                ptr::null_mut(),
                entropy.as_mut_ptr(),
                entropy.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(native_error(status));
        }
        let name = format!(
            ".pcl-stage-{}.lease",
            entropy
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let leaf = Leaf::new(name)?;
        // Directory::create_file atomically supplies a protected private SD and
        // requests DELETE/write access while denying write and delete sharing.
        match parent.create_file(&leaf) {
            Ok(file) => return Ok((file, Some(leaf))),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(changed("Could not allocate a unique private stage lease"))
}

pub(super) fn publish(stage: &Stage, target: &Directory, leaf: &Leaf) -> io::Result<()> {
    let units: Vec<u16> = leaf.as_os_str().encode_wide().collect();
    let offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
    let byte_len = (offset + units.len() * 2).max(size_of::<FILE_RENAME_INFORMATION>());
    let mut storage = vec![0usize; byte_len.div_ceil(size_of::<usize>())];
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    unsafe {
        information.write(FILE_RENAME_INFORMATION::default());
        (*information).RootDirectory = target.file().as_raw_handle();
        (*information).FileNameLength = (units.len() * 2) as u32;
        ptr::copy_nonoverlapping(
            units.as_ptr(),
            information.cast::<u8>().add(offset).cast::<u16>(),
            units.len(),
        );
    }
    let mut status_block = IO_STATUS_BLOCK::default();
    // Rename the original stage handle. Never reopen its source name, and never
    // request replacement/POSIX/copy flags or a cross-volume copy fallback.
    let status = unsafe {
        NtSetInformationFile(
            stage.file.as_raw_handle(),
            &mut status_block,
            information.cast(),
            byte_len as u32,
            FileRenameInformation,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(native_error(status))
    }
}

pub(super) fn sync_parent(parent: &Directory) -> io::Result<()> {
    parent.sync()
}
