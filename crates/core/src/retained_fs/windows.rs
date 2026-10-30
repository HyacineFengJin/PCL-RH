use super::*;
use std::{
    mem::{offset_of, size_of},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle},
    },
    ptr,
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtCreateFile, NtFlushBuffersFile, FILE_CREATE, FILE_DIRECTORY_FILE,
            FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{
            RtlNtStatusToDosError, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE,
            UNICODE_STRING,
        },
        Storage::FileSystem::*,
        System::IO::IO_STATUS_BLOCK,
    },
};

const SHARING: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const DIRECTORY_ACCESS: u32 =
    FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;

pub(super) fn validate_leaf(name: &OsStr) -> io::Result<()> {
    let units: Vec<u16> = name.encode_wide().collect();
    if units.is_empty()
        || units.len() > u16::MAX as usize / 2
        || units
            .iter()
            .any(|c| *c < 32 || [47, 92, 58, 34, 60, 62, 63, 124, 42].contains(c))
        || matches!(units.last(), Some(32 | 46))
    {
        return Err(invalid("Invalid literal Windows file name"));
    }
    let stem =
        String::from_utf16_lossy(units.split(|c| *c == 46).next().unwrap()).to_ascii_uppercase();
    let stem = stem.trim_end_matches(' ');
    if matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(
                stem.get(3..),
                Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            ))
    {
        return Err(invalid("Reserved Windows file name"));
    }
    Ok(())
}
pub(super) fn absolute_parts(path: &Path) -> io::Result<(std::path::PathBuf, Vec<OsString>)> {
    use std::path::Prefix;
    let mut root = std::path::PathBuf::new();
    let mut names = Vec::new();
    for part in path.components() {
        match part {
            Component::Prefix(prefix) if root.as_os_str().is_empty() => {
                if !matches!(
                    prefix.kind(),
                    Prefix::Disk(_)
                        | Prefix::VerbatimDisk(_)
                        | Prefix::UNC(_, _)
                        | Prefix::VerbatimUNC(_, _)
                ) {
                    return Err(invalid("Unsupported Windows filesystem namespace"));
                }
                root.push(prefix.as_os_str());
            }
            Component::RootDir if names.is_empty() => root.push(part.as_os_str()),
            Component::Normal(name) => {
                validate_leaf(name)?;
                names.push(name.to_owned());
            }
            _ => return Err(invalid("Invalid absolute Windows directory path")),
        }
    }
    if !root.is_absolute() {
        return Err(invalid("Expected an absolute Windows filesystem root"));
    }
    Ok((root, names))
}
fn native_error(status: i32) -> io::Error {
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}
fn owned(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<File> {
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_handle(handle) })
    }
}
pub(super) fn open_root(root: &Path) -> io::Result<File> {
    let mut units: Vec<u16> = root.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(invalid("Invalid filesystem root"));
    }
    units.push(0);
    owned(unsafe {
        CreateFileW(
            units.as_ptr(),
            DIRECTORY_ACCESS,
            SHARING,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    })
}
fn open(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    sharing: u32,
    private: Option<bool>,
) -> io::Result<File> {
    let mut units: Vec<u16> = name.encode_wide().collect();
    if units.len() > u16::MAX as usize / 2 {
        return Err(invalid("Windows file name is too long"));
    }
    let name = UNICODE_STRING {
        Length: (units.len() * 2) as u16,
        MaximumLength: (units.len() * 2) as u16,
        Buffer: if units.is_empty() {
            ptr::null_mut()
        } else {
            units.as_mut_ptr()
        },
    };
    let descriptor = private
        .map(super::windows_security::private_descriptor)
        .transpose()?;
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: descriptor
            .as_ref()
            .map(|d| d.as_ptr())
            .unwrap_or(ptr::null()),
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
            sharing,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status == 0 {
        owned(handle)
    } else {
        Err(native_error(status))
    }
}
pub(super) fn open_directory(parent: &File, name: &OsStr, policy: Traversal) -> io::Result<File> {
    let file = open(
        parent,
        name,
        DIRECTORY_ACCESS,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        SHARING,
        None,
    )?;
    let child = metadata(&file)?;
    if child.kind != Kind::Directory || child.reparse_tag.is_some() {
        return Err(invalid(
            "Directory is a reparse point or has the wrong type",
        ));
    }
    if policy == Traversal::SameFilesystem {
        let root = metadata(parent)?;
        match (&root.identity, &child.identity) {
            (Identity::Windows { volume: left, .. }, Identity::Windows { volume: right, .. })
                if left == right => {}
            _ => return Err(conflict("Opened child crosses the admitted volume")),
        }
    }
    Ok(file)
}
pub(super) fn reopen_directory(parent: &File, _: Traversal) -> io::Result<File> {
    // An internal empty NT name addresses the RootDirectory object itself;
    // unlike Win32 paths, NT file names do not accept the DOS dot component.
    // Public Leaf rejects empty names. No parent pathname is reconstructed.
    let file = open(
        parent,
        OsStr::new(""),
        DIRECTORY_ACCESS,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        SHARING,
        None,
    )?;
    if metadata(&file)?.identity != metadata(parent)?.identity {
        return Err(conflict("Reopened directory identity differs"));
    }
    Ok(file)
}
pub(super) fn create_directory(parent: &File, name: &OsStr, policy: Traversal) -> io::Result<File> {
    let file = open(
        parent,
        name,
        DIRECTORY_ACCESS | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | DELETE,
        FILE_CREATE,
        FILE_DIRECTORY_FILE,
        SHARING,
        Some(true),
    )?;
    if policy == Traversal::SameFilesystem {
        match (&metadata(parent)?.identity, &metadata(&file)?.identity) {
            (Identity::Windows { volume: left, .. }, Identity::Windows { volume: right, .. })
                if left == right => {}
            _ => return Err(conflict("Created directory crosses the admitted volume")),
        }
    }
    Ok(file)
}
pub(super) fn open_regular(parent: &File, name: &OsStr) -> io::Result<File> {
    open(
        parent,
        name,
        FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        SHARING,
        None,
    )
}
pub(super) fn create_file(parent: &File, name: &OsStr) -> io::Result<File> {
    // A newly created writable object is protected by its held sharing lease.
    // Later stage publication must rename this handle, not reopen its name.
    open(
        parent,
        name,
        FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        FILE_SHARE_READ,
        Some(false),
    )
}
pub(super) fn entry(parent: &File, name: &OsStr) -> io::Result<Option<Metadata>> {
    match open(
        parent,
        name,
        FILE_READ_ATTRIBUTES | READ_CONTROL,
        FILE_OPEN,
        0,
        SHARING,
        None,
    ) {
        Ok(file) => metadata(&file).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn info<T: Default>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut value = T::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            (&mut value as *mut T).cast(),
            size_of::<T>() as u32,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}
fn time(ticks: i64) -> io::Result<Time> {
    if ticks < 0 {
        return Err(invalid("Invalid Windows timestamp"));
    }
    Ok(Time {
        seconds: ticks / 10_000_000 - 11_644_473_600,
        nanos: ((ticks % 10_000_000) * 100) as u32,
    })
}
pub(super) fn metadata(file: &File) -> io::Result<Metadata> {
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(invalid("Expected a disk filesystem object"));
    }
    let id: FILE_ID_INFO = info(file, FileIdInfo)?;
    let basic: FILE_BASIC_INFO = info(file, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = info(file, FileStandardInfo)?;
    let reparse_tag = if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let tag: FILE_ATTRIBUTE_TAG_INFO = info(file, FileAttributeTagInfo)?;
        Some(tag.ReparseTag)
    } else {
        None
    };
    let kind = if reparse_tag.is_some() {
        Kind::Link
    } else if standard.Directory {
        Kind::Directory
    } else {
        Kind::Regular
    };
    let security = super::windows_security::describe(file)?;
    Ok(Metadata {
        identity: Identity::Windows {
            volume: id.VolumeSerialNumber,
            file: id.FileId.Identifier,
        },
        kind,
        len: u64::try_from(standard.EndOfFile).map_err(|_| invalid("Invalid Windows file size"))?,
        links: standard.NumberOfLinks as u64,
        modified: time(basic.LastWriteTime)?,
        changed: time(basic.ChangeTime)?,
        security: Security::Windows {
            owner: security.owner,
            descriptor: security.descriptor,
            owner_is_current: security.owner_is_current,
            private: security.private,
        },
        reparse_tag,
    })
}
pub(super) fn require_owned(metadata: &Metadata) -> io::Result<()> {
    if matches!(
        metadata.security,
        Security::Windows {
            owner_is_current: true,
            ..
        }
    ) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Object is not owned by the current Windows user",
        ))
    }
}
pub(super) fn require_private(metadata: &Metadata) -> io::Result<()> {
    require_owned(metadata)?;
    if matches!(metadata.security, Security::Windows { private: true, .. }) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Object DACL does not establish private access",
        ))
    }
}
pub(super) fn names(parent: &File, limit: usize) -> io::Result<Vec<OsString>> {
    let directory = reopen_directory(parent, Traversal::AllowMounts)?;
    let mut words = vec![0u64; 8192];
    let capacity = words.len() * size_of::<u64>();
    let offset = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    let mut first = true;
    let mut names = Vec::new();
    loop {
        words.fill(0);
        let class = if first {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        first = false;
        if unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                class,
                words.as_mut_ptr().cast(),
                capacity as u32,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            } else {
                return Err(error);
            }
        }
        let mut position = 0usize;
        loop {
            if position + offset > capacity {
                return Err(invalid("Truncated native directory entry"));
            }
            let record = unsafe {
                words
                    .as_ptr()
                    .cast::<u8>()
                    .add(position)
                    .cast::<FILE_ID_BOTH_DIR_INFO>()
            };
            let next =
                unsafe { ptr::addr_of!((*record).NextEntryOffset).read_unaligned() } as usize;
            let length =
                unsafe { ptr::addr_of!((*record).FileNameLength).read_unaligned() } as usize;
            let name_end = position
                .checked_add(offset)
                .and_then(|value| value.checked_add(length));
            if length == 0
                || length % 2 != 0
                || name_end.is_none_or(|end| end > capacity)
                || (next != 0
                    && (next % 8 != 0 || next < offset + length || next > capacity - position))
            {
                return Err(invalid("Malformed native directory entry"));
            }
            let units = unsafe {
                std::slice::from_raw_parts(
                    (record.cast::<u8>().add(offset)).cast::<u16>(),
                    length / 2,
                )
            };
            if units != [46] && units != [46, 46] {
                if names.len() >= limit {
                    return Err(invalid("Directory entry count exceeds its limit"));
                }
                names.push(OsString::from_wide(units));
            }
            if next == 0 {
                break;
            }
            position += next;
        }
    }
    names.sort();
    Ok(names)
}
pub(super) fn open_lock(parent: &File, name: &OsStr) -> io::Result<File> {
    open(
        parent,
        name,
        FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL,
        FILE_OPEN_IF,
        FILE_NON_DIRECTORY_FILE,
        SHARING,
        Some(false),
    )
}
pub(super) fn lock(file: &File) -> io::Result<()> {
    let mut overlapped = windows_sys::Win32::System::IO::OVERLAPPED::default();
    if unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
pub(super) fn unlock(file: &File) {
    let mut overlapped = windows_sys::Win32::System::IO::OVERLAPPED::default();
    unsafe {
        UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped);
    }
}
pub(super) fn sync_directory(file: &File) -> io::Result<()> {
    // A directory opened only for lookup lacks write access required by flush.
    // Reopen the same held object with write-data access; do not rebuild its
    // pathname or hide a denied/unsupported durability operation.
    let writable = open(
        file,
        OsStr::new(""),
        FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        SHARING,
        None,
    )?;
    if metadata(&writable)?.identity != metadata(file)?.identity {
        return Err(conflict(
            "Directory flush capability refers to another object",
        ));
    }
    let mut status = IO_STATUS_BLOCK::default();
    let result = unsafe { NtFlushBuffersFile(writable.as_raw_handle(), &mut status) };
    if result == 0 {
        Ok(())
    } else {
        Err(native_error(result))
    }
}
