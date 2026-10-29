//! A prepared execve image: no allocation, locks, environment reads or formatting
//! occur between fork and exec. Explicit shebang interpreters still work; plain
//! text cannot acquire an implicit /bin/sh interpreter from an exec search API.
use super::*;
use std::{ffi::CString, io, os::unix::ffi::OsStrExt, ptr};

pub(super) struct Image {
    program: CString,
    _arguments: Vec<CString>,
    _environment: Vec<CString>,
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
}
// SAFETY: every pointer addresses an immutable CString buffer owned by this
// image. Moving a CString/Vec does not move its heap allocation. No buffers are
// mutated after construction; execute only reads them until execve copies them.
unsafe impl Send for Image {}
unsafe impl Sync for Image {}

impl Image {
    pub fn new(path: &Path) -> Result<Self> {
        let program =
            CString::new(path.as_os_str().as_bytes()).map_err(|_| "Java 可执行文件路径无效")?;
        let mut arguments = vec![program.clone()];
        for argument in PROBE_ARGUMENTS {
            arguments.push(CString::new(argument).unwrap());
        }
        let mut environment = Vec::new();
        let mut total = 0usize;
        for (key, value) in std::env::vars_os() {
            if ["JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS"]
                .iter()
                .any(|name| key == std::ffi::OsStr::new(name))
            {
                continue;
            }
            let mut bytes = key.as_bytes().to_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_bytes());
            total = total
                .checked_add(bytes.len() + 1)
                .ok_or("Java 检测环境超过大小上限")?;
            if total > 2 * 1024 * 1024 {
                return Err("Java 检测环境超过大小上限".into());
            }
            environment.push(CString::new(bytes).map_err(|_| "Java 检测环境格式无效")?);
        }
        let mut argv: Vec<_> = arguments.iter().map(|value| value.as_ptr()).collect();
        let mut envp: Vec<_> = environment.iter().map(|value| value.as_ptr()).collect();
        argv.push(ptr::null());
        envp.push(ptr::null());
        Ok(Self {
            program,
            _arguments: arguments,
            _environment: environment,
            argv,
            envp,
        })
    }
    pub fn execute(&self) -> io::Result<()> {
        unsafe {
            libc::execve(
                self.program.as_ptr(),
                self.argv.as_ptr(),
                self.envp.as_ptr(),
            );
        }
        Err(io::Error::last_os_error())
    }
}
