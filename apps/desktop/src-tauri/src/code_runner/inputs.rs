//! Read-only, bounded inventory of an owned attempt. This is input evidence,
//! never permission to execute a build or mutate its original project.
use super::Failure;
use pcl_core::retained_fs::{self, Directory, Identity, Kind, Leaf, Links, Metadata, Traversal};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

const MAX_ENTRIES: usize = 12_000;
const MAX_FILES: usize = 2000;
const MAX_INPUT: u64 = 64 * 1024 * 1024;
const MAX_FILE: u64 = 16 * 1024 * 1024;

fn io<T>(value: std::io::Result<T>) -> Result<T, Failure> {
    value.map_err(|error| Failure::io("Cannot inspect captured build input", error))
}
fn unsafe_input(message: &str) -> Failure {
    Failure::new("UNSAFE_BUILD_INPUT", message)
}
fn limit(message: &str) -> Failure {
    Failure::new("BUILD_INPUT_LIMIT", message)
}
fn input_security(metadata: &Metadata) -> Result<(), Failure> {
    #[cfg(target_os = "linux")]
    {
        io(metadata.require_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        io(metadata.require_private())
    }
}
fn leaf(name: &str) -> Result<Leaf, Failure> {
    if name.len() > 255 || name.chars().any(char::is_control) || name.contains('\\') {
        return Err(unsafe_input("Invalid build input component"));
    }
    Leaf::new(name).map_err(|_| unsafe_input("Invalid build input component"))
}
struct CapturedFile {
    parent: Directory,
    name: Leaf,
    file: File,
    metadata: Metadata,
    _sha256: [u8; 32],
}
struct CapturedDirectory {
    directory: Directory,
    metadata: Metadata,
    names: Vec<std::ffi::OsString>,
}
pub(super) struct Inventory {
    root: Directory,
    root_metadata: Metadata,
    bindings: Vec<(Directory, Leaf, Identity)>,
    directories: Vec<CapturedDirectory>,
    files: Vec<CapturedFile>,
    pub(super) java_sources: Vec<String>,
    pub(super) total_bytes: u64,
    entries: usize,
}
impl Inventory {
    pub(super) fn capture(path: &Path) -> Result<Self, Failure> {
        let root = io(Directory::open_absolute(path))?;
        let root_metadata = io(root.metadata())?;
        io(root_metadata.require_private())?;
        let mut result = Self {
            root,
            root_metadata,
            bindings: Vec::new(),
            directories: Vec::new(),
            files: Vec::new(),
            java_sources: Vec::new(),
            total_bytes: 0,
            entries: 0,
        };
        for name in ["source", "home"] {
            let child = io(result.root.child(&leaf(name)?, Traversal::SameFilesystem))?;
            result
                .bindings
                .push((result.root.clone(), leaf(name)?, io(child.identity())?));
            result.scan(child, name == "source")?;
        }
        // Inventory is checked against the retained graph and current root,
        // including native ACL evidence; no canonicalized pathname substitute.
        let visible = io(Directory::open_absolute(path))?;
        if io(visible.metadata())? != result.root_metadata
            || io(result.root.metadata())? != result.root_metadata
        {
            return Err(unsafe_input("Build attempt changed during inventory"));
        }
        result.recheck()?;
        result.java_sources.sort();
        Ok(result)
    }
    fn scan(&mut self, root: Directory, source: bool) -> Result<(), Failure> {
        let mut pending = vec![(String::new(), root, 0usize)];
        while let Some((relative, directory, depth)) = pending.pop() {
            let before = io(directory.metadata())?;
            input_security(&before)?;
            let mut names = io(directory.names(MAX_ENTRIES + 1))?;
            names.sort();
            for name in &names {
                self.entries += 1;
                if self.entries > MAX_ENTRIES {
                    return Err(limit("Build input exceeds its inventory limit"));
                }
                let name = name
                    .to_str()
                    .ok_or_else(|| unsafe_input("Build input names must be UTF-8"))?;
                let component = leaf(name)?;
                let output_name = if cfg!(windows) {
                    name.eq_ignore_ascii_case(".rh-build")
                } else {
                    name == ".rh-build"
                };
                if source && relative.is_empty() && output_name {
                    return Err(unsafe_input(
                        "Compiler output must not already exist in captured source",
                    ));
                }
                let path = if relative.is_empty() {
                    name.to_owned()
                } else {
                    format!("{relative}/{name}")
                };
                if path.len() > 512 {
                    return Err(limit("Build input path exceeds its byte limit"));
                }
                let entry = io(directory.entry(&component))?
                    .ok_or_else(|| unsafe_input("Build input disappeared during inventory"))?;
                if entry.reparse_tag.is_some() {
                    return Err(unsafe_input("Build inputs cannot contain reparse points"));
                }
                match entry.kind {
                    Kind::Directory => {
                        if depth >= 12 {
                            return Err(limit("Build input exceeds its directory depth limit"));
                        }
                        let child = io(directory.child(&component, Traversal::SameFilesystem))?;
                        if !entry.same_object(&io(child.metadata())?) {
                            return Err(unsafe_input(
                                "Build directory identity changed during inventory",
                            ));
                        }
                        self.bindings
                            .push((directory.clone(), component, io(child.identity())?));
                        pending.push((path, child, depth + 1));
                    }
                    Kind::Regular => {
                        if entry.links != 1 || entry.len > MAX_FILE {
                            return Err(unsafe_input(
                                "Build input must be a bounded single-link regular file",
                            ));
                        }
                        if self.files.len() >= MAX_FILES
                            || self
                                .total_bytes
                                .checked_add(entry.len)
                                .is_none_or(|n| n > MAX_INPUT)
                        {
                            return Err(limit("Build input exceeds its file or byte limit"));
                        }
                        let mut file = io(directory.open_regular(&component, Links::Single))?;
                        let metadata = io(retained_fs::metadata(&file))?;
                        input_security(&metadata)?;
                        if !entry.same_object(&metadata) || metadata.len != entry.len {
                            return Err(unsafe_input(
                                "Build input identity changed during inventory",
                            ));
                        }
                        let mut digest = Sha256::new();
                        let mut bytes = [0; 64 * 1024];
                        let mut length = 0u64;
                        loop {
                            let count = io(file.read(&mut bytes))?;
                            if count == 0 {
                                break;
                            }
                            length += count as u64;
                            if length > metadata.len || length > MAX_FILE {
                                return Err(unsafe_input("Build input grew during inventory"));
                            }
                            digest.update(&bytes[..count]);
                        }
                        if length != metadata.len || io(retained_fs::metadata(&file))? != metadata {
                            return Err(unsafe_input("Build input changed during inventory"));
                        }
                        io(directory.recheck(&component, &metadata.identity))?;
                        self.total_bytes += length;
                        self.files.push(CapturedFile {
                            parent: directory.clone(),
                            name: component,
                            file,
                            metadata,
                            _sha256: digest.finalize().into(),
                        });
                        if source && path.ends_with(".java") {
                            self.java_sources.push(format!("./{path}"));
                        }
                    }
                    _ => {
                        return Err(unsafe_input(
                            "Links and special files are unavailable in build input",
                        ))
                    }
                }
            }
            self.directories.push(CapturedDirectory {
                directory,
                metadata: before,
                names,
            });
        }
        Ok(())
    }
    pub(super) fn recheck(&self) -> Result<(), Failure> {
        for (parent, name, identity) in &self.bindings {
            io(parent.recheck(name, identity))?;
        }
        for captured in &self.directories {
            let mut names = io(captured.directory.names(MAX_ENTRIES + 1))?;
            names.sort();
            if names != captured.names || io(captured.directory.metadata())? != captured.metadata {
                return Err(unsafe_input("Build directory changed during inventory"));
            }
        }
        for captured in &self.files {
            io(captured
                .parent
                .recheck(&captured.name, &captured.metadata.identity))?;
            if io(retained_fs::metadata(&captured.file))? != captured.metadata {
                return Err(unsafe_input("Build file changed during inventory"));
            }
        }
        Ok(())
    }
}
