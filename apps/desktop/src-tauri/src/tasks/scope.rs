//! Coordination footprints, not filesystem authorization. Commands still own
//! their validated descriptors and must recheck their captured target before
//! publication. Physical directory ancestry catches nested roots, symlink or
//! bind aliases; a final-file claim reserves its basename, not the whole folder.
//! Managed Java additionally reserves a fixed future subtree without creating
//! it. Every claim is final at admission; workers cannot widen their footprint.

use std::{
    ffi::{CStr, CString},
    fs::{self, File},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

type Result<T> = std::result::Result<T, String>;
const MAX_PATH_BYTES: usize = 4096;
const MAX_ANCESTORS: usize = 128;
const MAX_FILES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    device: u64,
    inode: u64,
}
impl Key {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Ancestor {
    key: Key,
    relative: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Location {
    submitted: PathBuf,
    directory: bool,
    canonical: PathBuf,
    ancestors: Vec<Ancestor>,
    file_key: Option<Key>,
    nofollow: bool,
    future: Option<FutureDirectory>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FutureDirectory {
    anchor: Box<Location>,
    relative: PathBuf,
}

#[derive(Clone, Debug)]
enum Claims {
    Global,
    Paths(Vec<Location>),
}

#[derive(Clone, Debug)]
pub struct TaskScope {
    claims: Claims,
}

impl TaskScope {
    pub(super) fn is_global(&self) -> bool {
        matches!(self.claims, Claims::Global)
    }
    /// Conservative scope for existing synchronous/project-wide transactions.
    pub fn global() -> Self {
        Self {
            claims: Claims::Global,
        }
    }

    /// The command supplies a registered, currently available game directory.
    pub fn root(path: &Path) -> Result<Self> {
        Ok(Self {
            claims: Claims::Paths(vec![Location::capture(path, true)?]),
        })
    }

    /// Native selection already validated the existing parent descriptors. A
    /// missing final file is normal; an existing symlink/non-file is refused.
    pub fn files(paths: &[PathBuf]) -> Result<Self> {
        if paths.is_empty() || paths.len() > MAX_FILES {
            return Err("任务文件目标数量无效".into());
        }
        let mut locations = Vec::new();
        for path in paths {
            let location = Location::capture(path, false)?;
            if !locations.contains(&location) {
                locations.push(location);
            }
        }
        Ok(Self {
            claims: Claims::Paths(locations),
        })
    }

    /// Combine already captured footprints before admission. A bounded union
    /// neither acquires a task turn nor authorizes any additional filesystem IO.
    pub fn union(self, other: Self) -> Result<Self> {
        let (Claims::Paths(mut left), Claims::Paths(right)) = (self.claims, other.claims) else {
            return Ok(Self::global());
        };
        for location in right {
            if !left.contains(&location) {
                left.push(location);
            }
        }
        if left.len() > MAX_FILES {
            return Err("任务目标数量超过限制".into());
        }
        Ok(Self {
            claims: Claims::Paths(left),
        })
    }

    /// Reserve the launcher's one managed-runtime store without creating it.
    /// Existing directories are identity-bound and no path component may be a
    /// symlink. A missing suffix can appear while an earlier Java task runs;
    /// its original ancestor remains fixed and the whole future subtree was
    /// already reserved. Call only when this task may actually install Java.
    pub fn managed_runtime_store(project: &Path) -> Result<Self> {
        let path = project.join(".pcl-rust/java");
        validate_path(&path)?;
        let mut directory = open_directory(project)?;
        let mut anchor_path = project.to_path_buf();
        for name in [c".pcl-rust", c"java"] {
            match directory_child(&directory, name) {
                Ok(child) => {
                    directory = child;
                    anchor_path.push(name.to_str().expect("fixed Java store component"));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => return Err("托管 Java 目录不可用或包含符号链接".into()),
            }
        }
        let mut anchor = Location::capture(&anchor_path, true)?;
        anchor.nofollow = true;
        if anchor.ancestors.first().map(|a| &a.key)
            != Some(&Key::of(&directory.metadata().map_err(|e| e.to_string())?))
        {
            return Err("托管 Java 目录身份已变化".into());
        }
        anchor.recheck()?;
        let location = if anchor_path == path {
            anchor
        } else {
            let relative = path
                .strip_prefix(&anchor_path)
                .map_err(|_| "托管 Java 目录路径无效")?
                .to_path_buf();
            let mut location = anchor.clone();
            location.submitted = path;
            location.canonical = anchor.canonical.join(&relative);
            for ancestor in &mut location.ancestors {
                ancestor.relative.push(&relative);
            }
            location.future = Some(FutureDirectory {
                anchor: Box::new(anchor),
                relative,
            });
            location
        };
        Ok(Self {
            claims: Claims::Paths(vec![location]),
        })
    }

    pub(crate) fn conflicts(&self, other: &Self) -> bool {
        match (&self.claims, &other.claims) {
            (Claims::Global, _) | (_, Claims::Global) => true,
            (Claims::Paths(left), Claims::Paths(right)) => {
                left.iter().any(|a| right.iter().any(|b| a.conflicts(b)))
            }
        }
    }

    /// Waiting never adopts a replacement for an existing binding. Only the
    /// already reserved managed-store suffix may be created by an earlier task.
    /// This is outside the scheduler mutex; its turn remains held until cleanup
    /// publishes terminal state, including when this check refuses the worker.
    pub(super) fn recheck(&self) -> Result<()> {
        if let Claims::Paths(locations) = &self.claims {
            for location in locations {
                location.recheck()?;
            }
        }
        Ok(())
    }
}

impl Location {
    fn capture(path: &Path, directory: bool) -> Result<Self> {
        validate_path(path)?;
        let parent = if directory {
            path
        } else {
            path.parent().ok_or("任务文件缺少父目录")?
        };
        let canonical_parent = fs::canonicalize(parent).map_err(|_| "任务目标目录不可用")?;
        let parent_metadata = fs::metadata(&canonical_parent).map_err(|_| "任务目标目录不可用")?;
        if !parent_metadata.is_dir() {
            return Err("任务目标父路径不是目录".into());
        }
        let canonical = if directory {
            canonical_parent.clone()
        } else {
            canonical_parent.join(path.file_name().ok_or("任务文件缺少文件名")?)
        };
        let file_key = if directory {
            None
        } else {
            match fs::symlink_metadata(&canonical) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    Some(Key::of(&metadata))
                }
                Ok(_) => return Err("任务目标不是独立普通文件路径".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err("无法检查任务文件目标".into()),
            }
        };
        let mut ancestors = Vec::new();
        for ancestor in canonical_parent.ancestors() {
            if ancestors.len() >= MAX_ANCESTORS {
                return Err("任务目标目录层级超过限制".into());
            }
            let metadata = fs::metadata(ancestor).map_err(|_| "任务目标目录身份已变化")?;
            if !metadata.is_dir() {
                return Err("任务目标目录身份已变化".into());
            }
            ancestors.push(Ancestor {
                key: Key::of(&metadata),
                relative: canonical
                    .strip_prefix(ancestor)
                    .map_err(|_| "任务目标目录身份已变化")?
                    .to_path_buf(),
            });
        }
        // Capture is observational. Detect a namespace replacement while the
        // ancestry was read; validated source/output FDs remain command-owned.
        if fs::canonicalize(parent).map_err(|_| "任务目标目录身份已变化")? != canonical_parent
            || Key::of(&fs::metadata(&canonical_parent).map_err(|_| "任务目标目录身份已变化")?)
                != Key::of(&parent_metadata)
        {
            return Err("任务目标目录身份已变化".into());
        }
        Ok(Self {
            submitted: path.into(),
            directory,
            canonical,
            ancestors,
            file_key,
            nofollow: false,
            future: None,
        })
    }

    fn recheck(&self) -> Result<()> {
        if let Some(future) = &self.future {
            future.anchor.recheck()?;
            let directory = open_directory(&future.anchor.submitted)?;
            if future.anchor.ancestors.first().map(|a| &a.key)
                != Some(&Key::of(&directory.metadata().map_err(|e| e.to_string())?))
            {
                return Err("排队任务的托管 Java 祖先目录身份已变化".into());
            }
            return check_future_directory(&directory, &future.relative);
        }
        let mut current = Self::capture(&self.submitted, self.directory)?;
        if self.nofollow {
            let directory = open_directory(&self.submitted)?;
            if current.ancestors.first().map(|a| &a.key)
                != Some(&Key::of(&directory.metadata().map_err(|e| e.to_string())?))
            {
                return Err("排队任务的托管 Java 目录身份已变化".into());
            }
            current.nofollow = true;
        }
        if current != *self {
            return Err("排队任务的目标文件或目录身份已变化，请重新提交".into());
        }
        Ok(())
    }

    fn conflicts(&self, other: &Self) -> bool {
        if !self.directory
            && !other.directory
            && self.file_key.is_some()
            && self.file_key == other.file_key
        {
            return true;
        }
        self.ancestors.iter().any(|a| {
            other.ancestors.iter().any(|b| {
                a.key == b.key
                    && match (self.directory, other.directory) {
                        (true, true) => {
                            a.relative.starts_with(&b.relative)
                                || b.relative.starts_with(&a.relative)
                        }
                        (true, false) => b.relative.starts_with(&a.relative),
                        (false, true) => a.relative.starts_with(&b.relative),
                        (false, false) => a.relative == b.relative,
                    }
            })
        })
    }
}

fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().len() > MAX_PATH_BYTES
        || path.components().count() > MAX_ANCESTORS
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        Err("任务目标必须是有界规范绝对路径".into())
    } else {
        Ok(())
    }
}

fn directory_child(parent: &File, name: &CStr) -> std::io::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn open_directory(path: &Path) -> Result<File> {
    validate_path(path)?;
    let mut directory = File::open("/").map_err(|e| e.to_string())?;
    for component in path.components() {
        if let Component::Normal(name) = component {
            let name = CString::new(name.as_bytes()).map_err(|_| "任务目录含有空字符")?;
            directory = directory_child(&directory, &name)
                .map_err(|_| "任务目录身份已变化或包含符号链接")?;
        }
    }
    Ok(directory)
}

/// A newly appeared bind mount could alias another running task even though
/// that inode did not exist in our captured ancestry. Refuse mounts only below
/// the originally missing suffix; existing project mounts remain supported.
fn check_future_directory(anchor: &File, relative: &Path) -> Result<()> {
    // An unchanged absence needs no openat2 capability. Once any reserved
    // suffix appeared, inspect the full descent with mount rejection below.
    let first = relative
        .components()
        .next()
        .and_then(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .ok_or("托管 Java 路径无效")?;
    let first = CString::new(first.as_bytes()).map_err(|_| "托管 Java 路径无效")?;
    match directory_child(anchor, &first) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("排队任务的托管 Java 目录已变化或出现符号链接".into()),
        Ok(_) => {}
    }
    #[repr(C)]
    struct How {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = How {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: 0,
        resolve: 0x01 | 0x02 | 0x04 | 0x08,
    };
    let path = CString::new(relative.as_os_str().as_bytes()).map_err(|_| "托管 Java 路径无效")?;
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            anchor.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<How>(),
        )
    };
    if fd >= 0 {
        drop(unsafe { File::from_raw_fd(fd as i32) });
        Ok(())
    } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err("排队任务的托管 Java 目录已变化或出现链接、挂载，无法安全运行".into())
    }
}

#[cfg(test)]
#[path = "runtime_scope_tests.rs"]
mod runtime_scope_tests;
