//! Trusted system Node discovery and asynchronous ownership retirement. These
//! processes are launcher hosts, never authorization to execute author builds.
use super::host_files::Lease;
use pcl_core::retained_fs::{self, Directory, Kind, Leaf, Links, Metadata};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

pub(super) struct Node {
    path: PathBuf,
    parent: Directory,
    name: Leaf,
    file: File,
    stamp: Metadata,
    fixed_path: std::ffi::OsString,
}
pub(super) const NODE_MISSING: &str = "PCL_NODE_MISSING:";
pub(super) const NODE_UNSUPPORTED: &str = "PCL_NODE_UNSUPPORTED:";

fn definitely_missing(candidate: &Path) -> io::Result<bool> {
    let mut ancestor = candidate;
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                // A dangling final link or ancestor is an existing, broken
                // installation, not proof that Node was never installed.
                if metadata.file_type().is_symlink() || cfg!(windows) && reparse(&metadata) {
                    ancestor.canonicalize().map_err(|error| {
                        io::Error::other(format!(
                            "Existing Node installation link cannot be resolved: {error}"
                        ))
                    })?;
                }
                if ancestor != candidate {
                    let resolved = ancestor.canonicalize().map_err(|error| {
                        io::Error::other(format!(
                            "Existing Node installation path changed: {error}"
                        ))
                    })?;
                    let projected =
                        resolved.join(candidate.strip_prefix(ancestor).map_err(io::Error::other)?);
                    if !permitted_resolution(candidate, &projected) {
                        return Err(io::Error::other(
                            "System Node installation path resolves outside its prefix",
                        ));
                    }
                }
                return Ok(ancestor != candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}
fn reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}
#[cfg(windows)]
fn known_folder(id: &windows_sys::core::GUID) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{System::Com::CoTaskMemFree, UI::Shell::SHGetKnownFolderPath};
    let mut pointer = std::ptr::null_mut();
    let result = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut pointer) };
    if result < 0 || pointer.is_null() {
        if !pointer.is_null() {
            unsafe { CoTaskMemFree(pointer.cast()) };
        }
        return Err(io::Error::other(
            "Cannot locate the system Node installation directory",
        ));
    }
    let mut length = 0;
    while length < 32768 && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    let value = if length == 32768 {
        Err(io::Error::other(
            "System installation directory exceeds its bound",
        ))
    } else {
        Ok(PathBuf::from(std::ffi::OsString::from_wide(unsafe {
            std::slice::from_raw_parts(pointer, length)
        })))
    };
    unsafe { CoTaskMemFree(pointer.cast()) };
    value
}
pub(super) fn candidates() -> io::Result<(Vec<PathBuf>, std::ffi::OsString)> {
    #[cfg(target_os = "linux")]
    return Ok((vec![PathBuf::from("/usr/bin/node")], "/usr/bin:/bin".into()));
    #[cfg(target_os = "macos")]
    return Ok((
        vec![
            PathBuf::from("/opt/homebrew/bin/node"),
            PathBuf::from("/usr/local/bin/node"),
        ],
        "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".into(),
    ));
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, FOLDERID_Windows};
        let node = known_folder(&FOLDERID_ProgramFiles)?.join("nodejs");
        let windows = known_folder(&FOLDERID_Windows)?;
        let path = std::env::join_paths([node.clone(), windows.join("System32"), windows])
            .map_err(io::Error::other)?;
        Ok((vec![node.join("node.exe")], path))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "System Node discovery is unsupported",
    ))
}
fn permitted_resolution(candidate: &Path, resolved: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        resolved == candidate
            || resolved.starts_with("/opt/homebrew/Cellar/node")
            || resolved.starts_with("/usr/local/Cellar/node")
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows canonicalization adds a verbatim prefix. Both paths are
        // compared as retained objects below, without accepting a PATH entry.
        let _ = (candidate, resolved);
        true
    }
}
impl Node {
    pub(super) fn discover() -> io::Result<Self> {
        let (candidates, fixed_path) = candidates()?;
        Self::discover_from(candidates, fixed_path)
    }
    fn discover_from(candidates: Vec<PathBuf>, fixed_path: std::ffi::OsString) -> io::Result<Self> {
        let mut first_error = None;
        let mut missing = 0;
        for candidate in candidates {
            match definitely_missing(&candidate) {
                Ok(true) => {
                    missing += 1;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            }
            let result = (|| {
                #[cfg(windows)]
                let path = candidate.clone();
                #[cfg(not(windows))]
                let path = candidate.canonicalize()?;
                if !permitted_resolution(&candidate, &path) {
                    return Err(io::Error::other(
                        "System Node resolves outside its installation prefix",
                    ));
                }
                let parent = Directory::open_absolute(
                    path.parent()
                        .ok_or_else(|| io::Error::other("Node path has no parent"))?,
                )?;
                let name = Leaf::new(
                    path.file_name()
                        .ok_or_else(|| io::Error::other("Node path has no filename"))?,
                )?;
                let file = parent.open_regular(&name, Links::Single)?;
                let stamp = retained_fs::metadata(&file)?;
                if stamp.kind != Kind::Regular || stamp.reparse_tag.is_some() {
                    return Err(io::Error::other("System Node is not a regular executable"));
                }
                #[cfg(unix)]
                if matches!(&stamp.security, retained_fs::Security::Unix { mode, .. } if mode & 0o111 == 0)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "System Node is not executable",
                    ));
                }
                parent.recheck(&name, &stamp.identity)?;
                Ok(Self {
                    path,
                    parent,
                    name,
                    file,
                    stamp,
                    fixed_path: fixed_path.clone(),
                })
            })();
            match result {
                Ok(node) => return Ok(node),
                Err(error) => {
                    // This candidate was observed to exist. A subsequent
                    // NotFound is a discovery race/broken installation, never
                    // an all-candidates-missing result.
                    first_error.get_or_insert_with(|| {
                        if error.kind() == io::ErrorKind::NotFound {
                            io::Error::other(format!(
                                "Existing Node candidate changed during discovery: {error}"
                            ))
                        } else {
                            error
                        }
                    });
                }
            }
        }
        Err(first_error.unwrap_or_else(|| {
            if missing == 0 {
                return io::Error::new(
                    io::ErrorKind::Unsupported,
                    "No supported system Node locations are configured",
                );
            }
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{NODE_MISSING} System Node is not installed at a supported location"),
            )
        }))
    }
    pub(super) fn recheck(&self) -> io::Result<()> {
        if retained_fs::metadata(&self.file)? != self.stamp {
            return Err(io::Error::other("System Node changed before launch"));
        }
        self.parent.recheck(&self.name, &self.stamp.identity)
    }
    fn command(&self) -> io::Result<Command> {
        self.recheck()?;
        let mut command = Command::new(&self.path);
        command
            .env_clear()
            .env("PATH", &self.fixed_path)
            .env("LANG", "C.UTF-8");
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::Shell::FOLDERID_Windows;
            command.env("SystemRoot", known_folder(&FOLDERID_Windows)?);
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        Ok(command)
    }
    pub(super) fn version_command(&self) -> io::Result<Command> {
        let mut command = self.command()?;
        command
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(command)
    }
    pub(super) fn host_command(&self, lease: &Lease) -> io::Result<Command> {
        lease.recheck()?;
        let mut command = self.command()?;
        command
            .arg(lease.script_path())
            .arg(lease.data_path())
            .current_dir(lease.project_path())
            .env("PCL_RH_BUILD_RUNNER_EXE", std::env::current_exe()?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(command)
    }
}

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Slot;
impl Slot {
    fn acquire() -> io::Result<Self> {
        let mut observed = ACTIVE.load(Ordering::Acquire);
        loop {
            if observed >= 16 {
                return Err(io::Error::other(
                    "Experimental host retirement capacity is exhausted",
                ));
            }
            match ACTIVE.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
        Ok(Self)
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone)]
pub(super) struct Lifecycle {
    pub(super) retiring: Arc<AtomicUsize>,
    pub(super) stopped: Arc<AtomicBool>,
}
struct Retiring {
    child: Child,
    lease: Option<Arc<Lease>>,
    pipes: Vec<Arc<AtomicBool>>,
    exited: bool,
    lifecycle: Lifecycle,
    completed: Arc<AtomicBool>,
    _slot: Slot,
}
impl Retiring {
    fn poll(&mut self) -> bool {
        if !self.exited {
            let _ = self.child.kill();
            self.exited = matches!(self.child.try_wait(), Ok(Some(_)));
        }
        self.exited
            && self
                .pipes
                .iter()
                .all(|closed| closed.load(Ordering::Acquire))
    }
}
impl Retiring {
    fn complete(self) {
        let Self {
            child,
            lease,
            pipes,
            lifecycle,
            completed,
            _slot,
            ..
        } = self;
        // Publish completion only after all exact OS owners and the host lock
        // have actually been released. This path is reached only by poll=true.
        drop(child);
        drop(pipes);
        drop(lease);
        drop(_slot);
        if lifecycle.retiring.fetch_sub(1, Ordering::AcqRel) == 1 {
            lifecycle.stopped.store(true, Ordering::Release);
        }
        completed.store(true, Ordering::Release);
    }
}
struct Retirement {
    waiting: Mutex<Vec<Retiring>>,
}
fn retirement() -> io::Result<Arc<Retirement>> {
    static RETIREMENT: OnceLock<Mutex<Option<Arc<Retirement>>>> = OnceLock::new();
    let mut shared = RETIREMENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(queue) = shared.as_ref() {
        return Ok(queue.clone());
    }
    let queue = Arc::new(Retirement {
        waiting: Mutex::new(Vec::new()),
    });
    let worker = queue.clone();
    std::thread::Builder::new()
        .name("pcl-node-retire".into())
        .spawn(move || {
            let mut pending: Vec<Retiring> = Vec::new();
            loop {
                // The tiny queue lock never spans process IO, a sleep, or a join.
                // All queued/in-flight owners together are bounded by ACTIVE=16.
                pending.extend(std::mem::take(
                    &mut *worker
                        .waiting
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                ));
                let mut index = 0;
                while index < pending.len() {
                    if pending[index].poll() {
                        pending.swap_remove(index).complete();
                    } else {
                        index += 1;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })?;
    *shared = Some(queue.clone());
    Ok(queue)
}
pub(super) struct Process {
    child: Option<Child>,
    lease: Option<Arc<Lease>>,
    pipes: Vec<Arc<AtomicBool>>,
    lifecycle: Lifecycle,
    completed: Arc<AtomicBool>,
    slot: Option<Slot>,
    retirement: Arc<Retirement>,
}
impl Process {
    pub(super) fn spawn(
        mut command: Command,
        lease: Arc<Lease>,
        lifecycle: Lifecycle,
    ) -> io::Result<Self> {
        let slot = Slot::acquire()?;
        let retirement = retirement()?;
        lease.recheck()?;
        let child = command.spawn()?;
        Ok(Self {
            child: Some(child),
            lease: Some(lease),
            pipes: Vec::new(),
            lifecycle,
            completed: Arc::new(AtomicBool::new(false)),
            slot: Some(slot),
            retirement,
        })
    }
    pub(super) fn child(&mut self) -> &mut Child {
        self.child.as_mut().expect("live owned Node")
    }
    pub(super) fn wait_exit(&mut self, deadline: Instant) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.child().try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "System Node probe did not exit",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[cfg(test)]
    pub(super) fn id(&self) -> u32 {
        self.child.as_ref().expect("live owned Node").id()
    }
    pub(super) fn track_pipe(&mut self, closed: Arc<AtomicBool>) {
        self.pipes.push(closed);
    }
    pub(super) fn completion(&self) -> Arc<AtomicBool> {
        self.completed.clone()
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            self.lifecycle.retiring.fetch_add(1, Ordering::AcqRel);
            let owner = Retiring {
                child,
                lease: Some(self.lease.take().expect("owned host lease")),
                pipes: std::mem::take(&mut self.pipes),
                exited: false,
                lifecycle: self.lifecycle.clone(),
                completed: self.completed.clone(),
                _slot: self.slot.take().expect("owned retirement slot"),
            };
            self.retirement
                .waiting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(owner);
        }
    }
}
pub(super) fn wait_retired(completed: &AtomicBool, deadline: Instant) -> bool {
    while !completed.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    true
}
pub(super) fn parse_version(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    let version = std::str::from_utf8(bytes).ok()?.trim().strip_prefix('v')?;
    let mut numbers = version.split('.');
    let mut number = || {
        let text = numbers.next()?;
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u32>().ok())
            .flatten()
    };
    let result = (number()?, number()?, number()?);
    if numbers.next().is_some() {
        return None;
    }
    Some(result)
}
pub(super) fn supported_version(bytes: &[u8]) -> bool {
    matches!(parse_version(bytes), Some((major, minor, _)) if major > 22 || (major == 22 && minor >= 19))
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    #[test]
    fn only_all_confirmed_missing_candidates_have_the_missing_prefix() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let missing = root.join("missing/node");
        let empty = Node::discover_from(vec![], "fixed".into()).err().unwrap();
        assert_eq!(empty.kind(), io::ErrorKind::Unsupported);
        assert!(!empty.to_string().starts_with(NODE_MISSING));
        let error = Node::discover_from(vec![missing.clone()], "synthetic fixed path".into())
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().starts_with(NODE_MISSING));
        let unsafe_path = root.join("not-a-file");
        std::fs::create_dir(&unsafe_path).unwrap();
        let error = Node::discover_from(vec![unsafe_path, missing], "synthetic fixed path".into())
            .err()
            .unwrap();
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert!(!error.to_string().starts_with(NODE_MISSING));
    }
    #[cfg(unix)]
    #[test]
    fn broken_links_and_nonexecutables_are_not_overwritten_by_a_later_missing_candidate() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let missing = root.join("missing");
        let broken = root.join("broken");
        symlink(&missing, &broken).unwrap();
        let broken_parent = root.join("broken-parent");
        symlink(root.join("missing-directory"), &broken_parent).unwrap();
        for candidate in [broken, broken_parent.join("node")] {
            let error = Node::discover_from(vec![candidate, missing.clone()], "fixed".into())
                .err()
                .unwrap();
            assert_ne!(error.kind(), io::ErrorKind::NotFound);
            assert!(!error.to_string().starts_with(NODE_MISSING));
        }
        let noexec = root.join("node-noexec");
        std::fs::write(&noexec, b"synthetic bytes, never executed").unwrap();
        std::fs::set_permissions(&noexec, std::fs::Permissions::from_mode(0o600)).unwrap();
        let error = Node::discover_from(vec![noexec, missing], "fixed".into())
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!error.to_string().starts_with(NODE_MISSING));
    }
    #[test]
    fn retained_candidate_rejects_actual_file_change_without_execution() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let path = root.join("node");
        std::fs::write(&path, b"synthetic bytes, never executed").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let node = Node::discover_from(vec![path.clone()], "fixed".into()).unwrap();
        node.recheck().unwrap();
        // In-place metadata/content changes are valid on Windows even when the
        // held file prevents deleting/replacing its directory entry.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(1)
            .unwrap();
        assert!(node.recheck().is_err());
    }
}
