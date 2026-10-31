//! Read-only native coordination observations. No claim grants filesystem IO
//! authority, creates a missing runtime directory, or pins an admitted path.
use super::{Ancestor, Key, Location, Result, MAX_ANCESTORS, MAX_PATH_BYTES};
use pcl_core::retained_fs::{self, Directory, Kind, Leaf, Links, Traversal};
use std::{
    ffi::OsStr,
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
};

pub(super) fn validate_path(path: &Path) -> Result<()> {
    #[cfg(windows)]
    let size = {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().count().saturating_mul(2)
    };
    #[cfg(unix)]
    let size = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    };
    #[cfg(windows)]
    let mut prefix = false;
    let mut root = false;
    let mut names = 0usize;
    for part in path.components() {
        match part {
            #[cfg(windows)]
            Component::Prefix(value) if !prefix && !root && names == 0 => {
                use std::path::Prefix;
                if !matches!(
                    value.kind(),
                    Prefix::Disk(_)
                        | Prefix::VerbatimDisk(_)
                        | Prefix::UNC(_, _)
                        | Prefix::VerbatimUNC(_, _)
                ) {
                    return Err("任务目标不支持此 Windows 命名空间".into());
                }
                prefix = true;
            }
            Component::RootDir if !root && names == 0 => root = true,
            Component::Normal(name) if root => {
                Leaf::new(name).map_err(|_| "任务路径含有无效的单叶名称")?;
                names += 1;
            }
            _ => return Err("任务目标必须是有界规范绝对路径".into()),
        }
    }
    #[cfg(windows)]
    if !prefix {
        return Err("任务 Windows 路径必须包含显式磁盘或共享前缀".into());
    }
    if !path.is_absolute()
        || !root
        || size > MAX_PATH_BYTES
        || path.components().count() > MAX_ANCESTORS
    {
        return Err("任务目标必须是有界规范绝对路径".into());
    }
    Ok(())
}
pub(super) fn directory_key(file: &File) -> io::Result<Key> {
    let stamp = retained_fs::metadata(file)?;
    if stamp.kind != Kind::Directory || stamp.reparse_tag.is_some() {
        return Err(io::Error::other(
            "Task coordination directory is a reparse point or non-directory",
        ));
    }
    Ok(stamp.identity)
}
pub(super) fn open_directory(path: &Path) -> Result<File> {
    validate_path(path)?;
    Directory::open_absolute(path)
        .and_then(|directory| directory.try_clone_file())
        .map_err(|_| "任务目录身份已变化或包含符号链接、重解析点".into())
}
pub(super) fn directory_child(parent: &File, name: &OsStr) -> io::Result<File> {
    Directory::from_file(parent.try_clone()?)?
        // Existing physical mounts are observed, as on Linux. Only a suffix
        // that was missing at admission must remain on its anchor filesystem.
        .child(&Leaf::new(name)?, Traversal::AllowMounts)?
        .try_clone_file()
}
fn file_key(parent: &Directory, name: &Leaf) -> Result<Option<Key>> {
    let Some(stamp) = parent.entry(name).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if stamp.kind != Kind::Regular || stamp.reparse_tag.is_some() {
        return Err("任务目标不是独立普通文件路径".into());
    }
    let held = parent
        .open_regular(name, Links::Any)
        .map_err(|e| e.to_string())?;
    let actual = retained_fs::metadata(&held).map_err(|e| e.to_string())?;
    if actual.kind != Kind::Regular
        || actual.reparse_tag.is_some()
        || actual.identity != stamp.identity
    {
        return Err("任务文件目标身份已变化".into());
    }
    parent
        .recheck(name, &actual.identity)
        .map_err(|e| e.to_string())?;
    Ok(Some(actual.identity))
}
pub(super) fn capture(path: &Path, directory: bool) -> Result<Location> {
    validate_path(path)?;
    let parent_path = if directory {
        path
    } else {
        path.parent().ok_or("任务文件缺少父目录")?
    };
    // Registered aliases are ordinary coordination inputs. Resolve those first
    // as before, then observe their literal physical graph through native IO.
    // Managed runtime admission separately opens its original path nofollow.
    let canonical_parent = fs::canonicalize(parent_path).map_err(|_| "任务目标目录不可用")?;
    let parent = Directory::open_absolute(&canonical_parent)
        .map_err(|_| "任务目标目录不可用或身份已变化")?;
    let parent_key = parent.identity().map_err(|e| e.to_string())?;
    let name = if directory {
        None
    } else {
        Some(Leaf::new(path.file_name().ok_or("任务文件缺少文件名")?).map_err(|e| e.to_string())?)
    };
    let final_key = name
        .as_ref()
        .map(|name| file_key(&parent, name))
        .transpose()?
        .flatten();
    let canonical = name.as_ref().map_or_else(
        || canonical_parent.clone(),
        |name| canonical_parent.join(name.as_os_str()),
    );
    let mut current = parent.clone();
    #[cfg(windows)]
    let mut visible = canonical_parent.clone();
    let mut relative = name
        .as_ref()
        .map_or_else(PathBuf::new, |name| PathBuf::from(name.as_os_str()));
    let mut ancestors = Vec::new();
    loop {
        if ancestors.len() >= MAX_ANCESTORS {
            return Err("任务目标目录层级超过限制".into());
        }
        let key = current.identity().map_err(|e| e.to_string())?;
        ancestors.push(Ancestor {
            key: key.clone(),
            relative: relative.clone(),
        });
        let upper = current
            .parent_snapshot()
            .map_err(|_| "任务目标目录祖先身份已变化")?;
        let upper_key = upper.identity().map_err(|e| e.to_string())?;
        if key == upper_key {
            break;
        }
        #[cfg(windows)]
        let leaf = visible
            .file_name()
            .ok_or("任务目录物理祖先与原生路径不一致")?
            .to_owned();
        #[cfg(target_os = "macos")]
        let leaf = physical_leaf(&current)?;
        let edge = upper
            .child(
                &Leaf::new(&leaf).map_err(|e| e.to_string())?,
                Traversal::AllowMounts,
            )
            .map_err(|_| "任务目录物理祖先绑定已变化")?;
        if edge.identity().map_err(|e| e.to_string())? != key {
            return Err("任务目录物理祖先身份已变化".into());
        }
        relative = PathBuf::from(leaf).join(relative);
        #[cfg(windows)]
        if !visible.pop() {
            return Err("任务目录物理祖先层级无效".into());
        }
        current = upper;
    }
    if fs::canonicalize(parent_path).map_err(|_| "任务目标目录身份已变化")? != canonical_parent
        || Directory::open_absolute(&canonical_parent)
            .and_then(|directory| directory.identity())
            .map_err(|_| "任务目标目录身份已变化")?
            != parent_key
        || name
            .as_ref()
            .map(|name| file_key(&parent, name))
            .transpose()?
            .flatten()
            != final_key
    {
        return Err("任务目标目录或文件身份已变化".into());
    }
    Ok(Location {
        submitted: path.into(),
        directory,
        canonical,
        ancestors,
        file_key: final_key,
        nofollow: false,
        future: None,
    })
}

#[cfg(target_os = "macos")]
fn physical_leaf(directory: &Directory) -> Result<std::ffi::OsString> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};
    // The physical parent observed through '..' can cross a firmlink/mount
    // boundary absent from realpath's spelling. Query this held object's name,
    // not a guessed lexical pop, and bind it back through the observed parent.
    // Apple's public fcntl.h/man fcntl define this non-mutating query:
    // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/fcntl.h
    let mut bytes = [0u8; libc::PATH_MAX as usize];
    if unsafe {
        libc::fcntl(
            directory.file().as_raw_fd(),
            libc::F_GETPATH_NOFIRMLINK,
            bytes.as_mut_ptr(),
        )
    } != 0
    {
        return Err("无法观测任务目录的原生物理名称".into());
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("任务目录原生名称无终止符")?;
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes[..end].to_vec()));
    path.file_name()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "任务目录原生物理父关系无效".into())
}
pub(super) fn check_future_directory(anchor: &File, relative: &Path) -> Result<()> {
    let mut current = Directory::from_file(anchor.try_clone().map_err(|e| e.to_string())?)
        .map_err(|_| "排队任务的托管 Java 祖先不是字面目录")?;
    let mut count = 0;
    for part in relative.components() {
        let Component::Normal(name) = part else {
            return Err("托管 Java 路径无效".into());
        };
        count += 1;
        if count > MAX_ANCESTORS {
            return Err("托管 Java 路径层级无效".into());
        }
        let name = Leaf::new(name).map_err(|_| "托管 Java 路径无效")?;
        match current.child(&name, Traversal::SameFilesystem) {
            Ok(child) => current = child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                return Err(
                    "排队任务的托管 Java 目录已变化或出现链接、跨文件系统，无法安全运行".into(),
                )
            }
        }
    }
    if count == 0 {
        return Err("托管 Java 路径为空".into());
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn path_starts_with(path: &Path, prefix: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Globalization::CompareStringOrdinal;
    let mut path = path.components();
    for component in prefix.components() {
        let Some(actual) = path.next() else {
            return false;
        };
        let left: Vec<u16> = actual.as_os_str().encode_wide().collect();
        let right: Vec<u16> = component.as_os_str().encode_wide().collect();
        // Conservatively reserve case aliases even in a case-sensitive NTFS
        // directory. Failure blocks overlap rather than inventing distinct IO
        // authority; neither this comparison nor any footprint grants IO.
        let result = unsafe {
            CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            )
        };
        if result != 2 && result != 0 {
            return false;
        }
    }
    true
}
