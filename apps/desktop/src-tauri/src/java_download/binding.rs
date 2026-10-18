//! Admission keeps the existing store, or its nearest existing ancestor, as
//! filesystem authority. Queue scopes only coordinate; opening this binding
//! never substitutes a newly reopened pathname for the captured descriptor.
use super::{Result, Store};
use crate::launcher_local::filesystem::Dir;
use std::{
    ffi::CString,
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path, PathBuf},
};

const SUFFIX: [&str; 2] = [".pcl-rust", "java"];

pub struct StoreBinding {
    path: PathBuf,
    anchor_path: PathBuf,
    anchor: Dir,
    identity: (u64, u64),
    missing: Vec<&'static str>,
}

impl StoreBinding {
    /// Pure observation: no mkdir, lock file, permissions change or network.
    /// Existing project mounts are supported; every component refuses links.
    pub fn capture(project: &Path) -> Result<Self> {
        let path = project.join(".pcl-rust/java");
        if !path.is_absolute()
            || path.as_os_str().as_bytes().len() > 4096
            || path.components().count() > 128
            || path
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        {
            return Err("托管 Java 目录必须是有界规范绝对路径".into());
        }
        let mut anchor = Dir::absolute(project)?;
        let mut anchor_path = project.to_path_buf();
        let mut missing = Vec::new();
        for (index, name) in SUFFIX.iter().enumerate() {
            match anchor.optional(name)? {
                Some(child) => {
                    anchor = child;
                    anchor_path.push(name);
                }
                None => {
                    missing.extend_from_slice(&SUFFIX[index..]);
                    break;
                }
            }
        }
        let identity = anchor.identity()?;
        let binding = Self {
            path,
            anchor_path,
            anchor,
            identity,
            missing,
        };
        binding.verify()?;
        Ok(binding)
    }

    /// Confirm the same existing/missing boundary after capturing the task
    /// scope, still before admission. Appearance in this short window requires
    /// resubmission; appearance after admission is allowed by `open` below.
    pub fn verify(&self) -> Result<()> {
        self.recheck_anchor()?;
        if let Some(name) = self.missing.first() {
            if self.anchor.optional(name)?.is_some() {
                return Err("托管 Java 目录在准入期间已变化，请重新提交".into());
            }
        }
        Ok(())
    }

    fn recheck_anchor(&self) -> Result<()> {
        if self.anchor.identity()? != self.identity
            || Dir::absolute(&self.anchor_path)?.identity()? != self.identity
        {
            return Err("托管 Java 目录或其现存祖先已被替换，请重新提交".into());
        }
        Ok(())
    }

    /// Called only after the task acquires its complete submitted scope. Any
    /// mkdir is relative to the original FD, after checking its visible name.
    /// A preceding worker may have created the fixed suffix; new links/mounts
    /// cannot convert that suffix into authority over another directory.
    pub fn open(&self) -> Result<Store> {
        self.recheck_anchor()?;
        let mut directory = Dir(self.anchor.0.try_clone().map_err(|e| e.to_string())?);
        for name in &self.missing {
            directory = match child(&directory, name)? {
                Some(child) => child,
                None => {
                    self.recheck_anchor()?;
                    let created = directory.create(name)?;
                    let opened = child(&directory, name)?.ok_or("新建托管 Java 目录已消失")?;
                    if created.identity()? != opened.identity()? {
                        return Err("新建托管 Java 目录已被替换".into());
                    }
                    opened
                }
            };
        }
        directory.require_private()?;
        self.recheck_anchor()?;
        if Dir::absolute(&self.path)?.identity()? != directory.identity()? {
            return Err("托管 Java 目录发布路径与固定目录不一致".into());
        }
        Ok(Store {
            path: self.path.clone(),
            dir: directory,
        })
    }
}

fn child(parent: &Dir, name: &str) -> Result<Option<Dir>> {
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
    let name = CString::new(name).map_err(|_| "托管 Java 路径无效")?;
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.0.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<How>(),
        )
    };
    if fd >= 0 {
        Ok(Some(Dir(unsafe { File::from_raw_fd(fd as i32) })))
    } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(format!(
            "无法打开新托管 Java 目录（不允许链接或新挂载）：{}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
