//! Windows relative-path cwd lifetime guard, owned only by the game monitor.
//!
//! Each literal ancestor is held with READ/WRITE sharing and no DELETE sharing.
//! A new protected, random, NoDelete file keeps the leaf nonempty. Every other
//! directory stays nonempty because its next child in the held graph cannot be
//! removed. This addresses namespace rename/reparse mutation; it is not an
//! executable, file-content, ACL or hostile-kernel sandbox. The native volume
//! GUID namespace avoids a mutable per-session DOS drive-letter binding.
//!
//! MS-FSA FSCTL_SET_REPARSE_POINT permits WRITE_DATA or WRITE_ATTRIBUTES and
//! rejects nonempty directories. Ordinary sharing does not block attributes:
//! https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/4aeefef8-92c3-4abc-af7a-a610caf8a165
//! https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew
use crate::retained_fs::{self, Identity, Kind, Leaf};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Write},
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr,
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{
            RtlNtStatusToDosError, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
        },
        Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
        Storage::FileSystem::{
            CreateFileW, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
            FILE_NAME_NORMALIZED, FILE_PERSISTENT_ACLS, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_SUPPORTS_REPARSE_POINTS, FILE_TRAVERSE,
            FILE_WRITE_DATA, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE, VOLUME_NAME_GUID,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

const SHARING: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const DIRECTORY_ACCESS: u32 =
    FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
const MAX_DEPTH: usize = 64;
const MAX_PATH_UNITS: usize = 32768;

fn invalid(message: &'static str) -> io::Error {
    io::Error::other(message)
}
fn final_path(file: &File) -> io::Result<PathBuf> {
    // The required size includes NUL only on insufficient-buffer results.
    // Bound allocation and churn; an unstable name is a launch rejection.
    let mut buffer = vec![0u16; 512];
    for _ in 0..3 {
        let count = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_GUID,
            )
        } as usize;
        if count == 0 {
            return Err(io::Error::last_os_error());
        }
        if count < buffer.len() {
            if buffer[..count].contains(&0) {
                return Err(invalid("目录原生路径含有 NUL"));
            }
            return Ok(OsString::from_wide(&buffer[..count]).into());
        }
        if count > MAX_PATH_UNITS {
            return Err(invalid("目录原生路径过长"));
        }
        buffer.resize(count, 0);
    }
    Err(invalid("目录原生路径在读取期间变化"))
}
fn identity(file: &File) -> io::Result<Identity> {
    let stamp = retained_fs::metadata(file)?;
    if stamp.kind != Kind::Directory || stamp.reparse_tag.is_some() {
        return Err(invalid("工作目录图不是字面目录或含有重解析点"));
    }
    Ok(stamp.identity)
}
fn volume_guid(prefix: &OsStr) -> bool {
    let Some(text) = prefix.to_str() else {
        return false;
    };
    let Some(guid) = text
        .strip_prefix("Volume{")
        .and_then(|text| text.strip_suffix('}'))
    else {
        return false;
    };
    guid.len() == 36
        && guid.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
fn require_ntfs(file: &File) -> io::Result<()> {
    let mut flags = 0;
    let mut filesystem = [0u16; 64];
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut flags,
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let length = filesystem
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| invalid("卷文件系统名称无终止符"))?;
    let ntfs: Vec<u16> = "NTFS".encode_utf16().collect();
    if filesystem[..length] != ntfs
        || flags & (FILE_PERSISTENT_ACLS | FILE_SUPPORTS_REPARSE_POINTS)
            != (FILE_PERSISTENT_ACLS | FILE_SUPPORTS_REPARSE_POINTS)
    {
        return Err(invalid(
            "固定工作目录要求具有持久 ACL 和重解析能力的本地 NTFS 卷",
        ));
    }
    Ok(())
}
fn open_root(path: &Path) -> io::Result<File> {
    let text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let raw = unsafe {
        CreateFileW(
            text.as_ptr(),
            DIRECTORY_ACCESS,
            SHARING,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(raw) })
}
fn open_at(parent: &File, leaf: &Leaf, create_anchor: bool) -> io::Result<File> {
    let mut text: Vec<u16> = leaf.as_os_str().encode_wide().collect();
    let name = UNICODE_STRING {
        Length: (text.len() * 2) as u16,
        MaximumLength: (text.len() * 2) as u16,
        Buffer: text.as_mut_ptr(),
    };
    let descriptor = if create_anchor {
        Some(crate::retained_fs::windows_security::private_descriptor(
            false,
        )?)
    } else {
        None
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: descriptor
            .as_ref()
            .map_or(ptr::null(), |value| value.as_ptr()),
        ..Default::default()
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut raw = ptr::null_mut();
    let access = if create_anchor {
        FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE
    } else {
        DIRECTORY_ACCESS
    };
    let options = if create_anchor {
        FILE_NON_DIRECTORY_FILE
    } else {
        FILE_DIRECTORY_FILE
    };
    let status = unsafe {
        NtCreateFile(
            &mut raw,
            access,
            &attributes,
            &mut status_block,
            ptr::null(),
            0,
            SHARING,
            if create_anchor {
                FILE_CREATE
            } else {
                FILE_OPEN
            },
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    Ok(unsafe { File::from_raw_handle(raw) })
}
fn anchor(parent: &File) -> io::Result<(Leaf, File, Identity)> {
    let mut random = [0u8; 16];
    let status = unsafe {
        BCryptGenRandom(
            ptr::null_mut(),
            random.as_mut_ptr(),
            random.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(invalid("无法分配工作目录 anchor 身份"));
    }
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let name = Leaf::new(format!(".pcl-rh-cwd-anchor-v1-{suffix}"))?;
    // FILE_CREATE is exclusive. This very first handle has no DELETE access,
    // no DELETE sharing and an atomically supplied protected owner/DACL. There
    // is no close/reopen interval in which another process can steal the name.
    let mut file = open_at(parent, &name, true)?;
    file.write_all(b"PCL-RH cwd anchor v1\n")?;
    let stamp = retained_fs::metadata(&file)?;
    stamp.require_private()?;
    if stamp.kind != Kind::Regular || stamp.reparse_tag.is_some() || stamp.links != 1 {
        return Err(invalid("工作目录 anchor 不是私有单链接普通文件"));
    }
    Ok((name, file, stamp.identity))
}

pub(super) struct CwdLease {
    path: PathBuf,
    input: File,
    graph: Vec<(File, Identity)>,
    anchor: File,
    anchor_identity: Identity,
    anchor_name: Leaf,
}
impl CwdLease {
    pub(super) fn new(input: &File) -> io::Result<Self> {
        Self::prepare(input, || {})
    }
    fn prepare(input: &File, after_path: impl FnOnce()) -> io::Result<Self> {
        let expected = identity(input)?;
        let path = final_path(input)?;
        after_path();
        let mut root = PathBuf::new();
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) if root.as_os_str().is_empty() => {
                    if !matches!(prefix.kind(), Prefix::Verbatim(volume) if volume_guid(volume)) {
                        return Err(invalid("固定工作目录仅支持原生 volume GUID 绝对路径"));
                    }
                    root.push(prefix.as_os_str());
                }
                Component::RootDir if names.is_empty() => root.push(component.as_os_str()),
                Component::Normal(name) => names.push(Leaf::new(name)?),
                _ => return Err(invalid("工作目录原生路径分量无效")),
            }
        }
        // A volume root can be supplied: its anchor still keeps it nonempty.
        if !root.is_absolute() || names.len() > MAX_DEPTH {
            return Err(invalid("工作目录图不是有界绝对目录"));
        }
        let file = open_root(&root)?;
        require_ntfs(&file)?;
        let root_identity = identity(&file)?;
        let mut graph = vec![(file, root_identity.clone())];
        for name in names {
            let file = open_at(&graph.last().unwrap().0, &name, false)?;
            let stamp = identity(&file)?;
            if !matches!((&root_identity, &stamp),
                (Identity::Windows { volume: a, .. }, Identity::Windows { volume: b, .. }) if a == b)
            {
                return Err(invalid("工作目录图跨越文件系统"));
            }
            graph.push((file, stamp));
        }
        if graph.last().unwrap().1 != expected
            || identity(input)? != expected
            || final_path(input)? != path
        {
            return Err(invalid("输入目录与固定路径图身份不一致或路径变化"));
        }
        let (anchor_name, anchor, anchor_identity) = anchor(&graph.last().unwrap().0)?;
        let lease = Self {
            path,
            input: input.try_clone()?,
            graph,
            anchor,
            anchor_identity,
            anchor_name,
        };
        lease.recheck()?;
        Ok(lease)
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn recheck(&self) -> io::Result<()> {
        for (file, expected) in &self.graph {
            if identity(file)? != *expected {
                return Err(invalid("固定工作目录图身份变化"));
            }
        }
        if identity(&self.input)? != self.graph.last().unwrap().1
            || final_path(&self.input)? != self.path
            || final_path(&self.graph.last().unwrap().0)? != self.path
        {
            return Err(invalid("固定工作目录的输入身份或可见路径变化"));
        }
        let stamp = retained_fs::metadata(&self.anchor)?;
        stamp.require_private()?;
        if stamp.identity != self.anchor_identity
            || stamp.kind != Kind::Regular
            || stamp.reparse_tag.is_some()
            || stamp.links != 1
        {
            return Err(invalid("工作目录 anchor 身份变化"));
        }
        let leaf = retained_fs::Directory::from_file(self.graph.last().unwrap().0.try_clone()?)?;
        leaf.recheck(&self.anchor_name, &self.anchor_identity)
    }
}
// Deliberately no path-based Drop cleanup. This small, explicitly versioned
// app-owned anchor may remain after launch failure, exit, or monitor loss. A
// future GC must establish its own exact identity/ownership authorization.
#[cfg(test)]
#[path = "cwd_tests.rs"]
mod tests;
