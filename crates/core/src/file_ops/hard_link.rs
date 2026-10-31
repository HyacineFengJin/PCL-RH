use crate::retained_fs::{self, Directory, Identity, Kind, Leaf, Links};
use std::{ffi::OsStr, fs::File, io};

pub(super) fn create(
    source_parent: &File,
    source: &OsStr,
    target_parent: &File,
    target: &OsStr,
) -> io::Result<()> {
    let source = Leaf::new(source)?;
    let target = Leaf::new(target)?;
    let from = Directory::from_file(source_parent.try_clone()?)?;
    let to = Directory::from_file(target_parent.try_clone()?)?;
    if to.entry(&target)?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Hard-link target exists",
        ));
    }
    let file = from.open_regular(&source, Links::Any)?;
    let original = retained_fs::metadata(&file)?;
    let same_volume = match (&original.identity, to.identity()?) {
        (Identity::Unix { device: a, .. }, Identity::Unix { device: b, .. }) => *a == b,
        (Identity::Windows { volume: a, .. }, Identity::Windows { volume: b, .. }) => *a == b,
        _ => false,
    };
    if !same_volume {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Hard links cannot cross filesystems",
        ));
    }
    from.recheck(&source, &original.identity)?;
    native(source_parent, &source, &file, target_parent, &target)?;
    let linked = to.open_regular(&target, Links::Any)?;
    let after = retained_fs::metadata(&file)?;
    let published = retained_fs::metadata(&linked)?;
    if after.kind != Kind::Regular
        || !original.same_object(&after)
        || !original.same_object(&published)
        || after.reparse_tag.is_some()
        || published.reparse_tag.is_some()
        || after.links < 2
    {
        return Err(io::Error::other(
            "Hard-link publication requires reconciliation",
        ));
    }
    from.recheck(&source, &original.identity)?;
    to.recheck(&target, &original.identity)
}

#[cfg(unix)]
fn native(from: &File, source: &Leaf, _: &File, to: &File, target: &Leaf) -> io::Result<()> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };
    let source = CString::new(source.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(io::Error::other)?;
    if unsafe {
        libc::linkat(
            from.as_raw_fd(),
            source.as_ptr(),
            to.as_raw_fd(),
            target.as_ptr(),
            0,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn native(_: &File, _: &Leaf, source: &File, parent: &File, target: &Leaf) -> io::Result<()> {
    use std::{
        mem::{offset_of, size_of},
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
        ptr,
    };
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FileLinkInformation, NtSetInformationFile, FILE_LINK_INFORMATION,
        },
        Win32::{Foundation::RtlNtStatusToDosError, System::IO::IO_STATUS_BLOCK},
    };
    let name: Vec<u16> = target.as_os_str().encode_wide().collect();
    let offset = offset_of!(FILE_LINK_INFORMATION, FileName);
    let bytes = (offset + name.len() * 2).max(size_of::<FILE_LINK_INFORMATION>());
    let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let info = storage.as_mut_ptr().cast::<FILE_LINK_INFORMATION>();
    unsafe {
        // All flags/ReplaceIfExists bytes are zero. Never request POSIX
        // replacement, readonly-attribute changes or storage-reserve overrides.
        info.write(FILE_LINK_INFORMATION::default());
        (*info).RootDirectory = parent.as_raw_handle();
        (*info).FileNameLength = (name.len() * 2) as u32;
        ptr::copy_nonoverlapping(
            name.as_ptr(),
            info.cast::<u8>().add(offset).cast::<u16>(),
            name.len(),
        );
    }
    let mut status_block = IO_STATUS_BLOCK::default();
    let status = unsafe {
        NtSetInformationFile(
            source.as_raw_handle(),
            &mut status_block,
            info.cast(),
            bytes as u32,
            FileLinkInformation,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ))
    }
}
