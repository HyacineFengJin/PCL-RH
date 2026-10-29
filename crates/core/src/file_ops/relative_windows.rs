use super::{directory, invalid_name};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    ptr,
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FileRenameInformation, NtCreateFile, NtSetInformationFile, FILE_OPEN,
            FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{RtlNtStatusToDosError, OBJ_CASE_INSENSITIVE, UNICODE_STRING},
        Storage::FileSystem::{
            DELETE, FILE_ACCESS_RIGHTS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

fn leaf(name: &OsStr) -> io::Result<Vec<u16>> {
    let units: Vec<u16> = name.encode_wide().collect();
    if units.is_empty()
        || units.len() > u16::MAX as usize / 2
        || units
            .iter()
            .any(|c| *c < 32 || [47, 92, 58, 34, 60, 62, 63, 124, 42].contains(c))
        || matches!(units.last(), Some(32 | 46))
    {
        return Err(invalid_name());
    }
    // RootDirectory avoids the DOS device namespace, but do not publish names
    // that Win32 consumers would subsequently interpret as DOS devices.
    let stem = units.split(|c| *c == 46).next().unwrap();
    let stem = String::from_utf16_lossy(stem).to_ascii_uppercase();
    let stem = stem.trim_end_matches(' ');
    if matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(
                stem.get(3..),
                Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            ))
    {
        return Err(invalid_name());
    }
    Ok(units)
}

fn native_error(status: i32) -> io::Error {
    // NT calls return their own status, not GetLastError.
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}

fn entry(parent: &File, units: &[u16], access: FILE_ACCESS_RIGHTS) -> io::Result<File> {
    let name = UNICODE_STRING {
        Length: (units.len() * 2) as u16,
        MaximumLength: (units.len() * 2) as u16,
        // NtCreateFile treats the counted ObjectName buffer as input only.
        Buffer: units.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut handle = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status != 0 {
        return Err(native_error(status));
    }
    // Only one validated component is opened with OPEN_REPARSE_POINT: a final
    // link/junction is the entry to rename, never a path to its referent. No
    // intermediate components or OBJ_DONT_REPARSE assumptions are needed.
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn exists(parent: &File, units: &[u16]) -> io::Result<bool> {
    // Ask only for metadata access, not permission to read file contents.
    match entry(parent, units, FILE_READ_ATTRIBUTES) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(in crate::file_ops) fn rename(
    source_parent: &File,
    source: &OsStr,
    target_parent: &File,
    target: &OsStr,
) -> io::Result<()> {
    let source = leaf(source)?;
    let target = leaf(target)?;
    directory(source_parent)?;
    directory(target_parent)?;
    if exists(target_parent, &target)? {
        // NT can accept a rename onto another link to the same file even
        // with ReplaceIfExists=false. Let the filesystem resolve aliases;
        // ordinal string casing need not match a volume's name semantics.
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Rename destination already exists",
        ));
    }
    let source_file = entry(source_parent, &source, DELETE | FILE_READ_ATTRIBUTES)?;
    let offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
    let byte_len = (offset + target.len() * 2).max(size_of::<FILE_RENAME_INFORMATION>());
    // usize allocation provides native alignment for the flexible NT struct.
    let mut storage = vec![0usize; byte_len.div_ceil(size_of::<usize>())];
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    unsafe {
        information.write(FILE_RENAME_INFORMATION::default());
        (*information).RootDirectory = target_parent.as_raw_handle();
        (*information).FileNameLength = (target.len() * 2) as u32;
        ptr::copy_nonoverlapping(
            target.as_ptr(),
            (information.cast::<u8>().add(offset)).cast::<u16>(),
            target.len(),
        );
    }
    // All replacement/POSIX/copy flags remain zero. The target is relative to
    // the held directory object, including if its former pathname was moved.
    let mut status_block = IO_STATUS_BLOCK::default();
    let status = unsafe {
        NtSetInformationFile(
            source_file.as_raw_handle(),
            &mut status_block,
            information.cast(),
            byte_len as u32,
            FileRenameInformation,
        )
    };
    if status == 0 {
        // Detect native no-ops from late aliases. A concurrently recreated
        // source is also an explicit conflict after a possible publication,
        // requiring the caller to reconcile its durable transaction state.
        if exists(source_parent, &source)? {
            Err(io::Error::other(
                "Rename needs reconciliation: source entry remains",
            ))
        } else {
            Ok(())
        }
    } else {
        Err(native_error(status))
    }
}
