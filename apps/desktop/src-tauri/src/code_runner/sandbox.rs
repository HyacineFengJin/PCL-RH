//! Build input validation and the entire executable/argument allowlist. A
//! basename beginning with @ or - is still a javac source, never an option or
//! argument file, because every project argument has an explicit ./ prefix.
use super::{process, Failure, Recipe, Request};
use std::{
    ffi::CString,
    fs::{self, File},
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

pub(super) const TMP_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 12_000;
const MAX_FILES: usize = 2000;
const MAX_INPUT: u64 = 64 * 1024 * 1024;
const MAX_FILE: u64 = 16 * 1024 * 1024;

struct Dir(File);
impl Dir {
    fn absolute(path: &Path) -> Result<Self, Failure> {
        if !path.is_absolute()
            || fs::canonicalize(path).map_err(|e| Failure::io("Cannot resolve build attempt", e))?
                != path
        {
            return Err(Failure::new(
                "UNSAFE_BUILD_INPUT",
                "Build attempt must be canonical without directory links",
            ));
        }
        let fd = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(Failure::io(
                "Cannot open build root",
                std::io::Error::last_os_error(),
            ));
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(part) => {
                    dir = dir.child(part.to_str().ok_or_else(|| {
                        Failure::new("UNSAFE_BUILD_INPUT", "Build input paths must be UTF-8")
                    })?)?
                }
                _ => {
                    return Err(Failure::new(
                        "UNSAFE_BUILD_INPUT",
                        "Build input paths cannot contain relative jumps",
                    ))
                }
            }
        }
        let metadata = dir
            .0
            .metadata()
            .map_err(|e| Failure::io("Cannot inspect build attempt", e))?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(Failure::new(
                "UNSAFE_BUILD_INPUT",
                "Build attempt must be private to the current user",
            ));
        }
        Ok(dir)
    }
    fn name(name: &str) -> Result<CString, Failure> {
        if name.is_empty()
            || matches!(name, "." | "..")
            || name.len() > 255
            || name.contains(['/', '\\'])
            || name.chars().any(char::is_control)
        {
            return Err(Failure::new(
                "UNSAFE_BUILD_INPUT",
                "Invalid build input component",
            ));
        }
        CString::new(name)
            .map_err(|_| Failure::new("UNSAFE_BUILD_INPUT", "Invalid build input component"))
    }
    fn child(&self, name: &str) -> Result<Self, Failure> {
        let name = Self::name(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(Failure::io(
                "Cannot open link-free build directory",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }
    fn create(&self, name: &str) -> Result<Self, Failure> {
        let component = Self::name(name)?;
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), component.as_ptr(), 0o700) } != 0 {
            return Err(Failure::io(
                "Cannot create private compiler output",
                std::io::Error::last_os_error(),
            ));
        }
        self.child(name)
    }
    fn names(&self) -> Result<Vec<String>, Failure> {
        // Enumeration follows the owned descriptor, never the caller's path.
        let mut names = Vec::new();
        for entry in fs::read_dir(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
            .map_err(|e| Failure::io("Cannot list build source", e))?
        {
            let name = entry
                .map_err(|e| Failure::io("Cannot list build source", e))?
                .file_name()
                .into_string()
                .map_err(|_| {
                    Failure::new("UNSAFE_BUILD_INPUT", "Build source names must be UTF-8")
                })?;
            Self::name(&name)?;
            names.push(name);
            if names.len() > MAX_ENTRIES {
                return Err(Failure::new(
                    "BUILD_INPUT_LIMIT",
                    "Build directory exceeds its entry limit",
                ));
            }
        }
        names.sort();
        Ok(names)
    }
    fn regular(&self, name: &str) -> Result<File, Failure> {
        let name = Self::name(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(Failure::io(
                "Cannot open regular build input",
                std::io::Error::last_os_error(),
            ));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file
            .metadata()
            .map_err(|e| Failure::io("Cannot inspect build input", e))?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_FILE {
            return Err(Failure::new(
                "UNSAFE_BUILD_INPUT",
                "Build input must be a bounded regular file with one link",
            ));
        }
        Ok(file)
    }
}

fn scan_source(root: &Dir) -> Result<Vec<String>, Failure> {
    let mut pending = vec![(String::new(), root.child_or_clone()?, 0usize)];
    let mut entries = 0;
    let mut files = 0;
    let mut bytes = 0;
    let mut sources = Vec::new();
    while let Some((relative, directory, depth)) = pending.pop() {
        for name in directory.names()? {
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err(Failure::new(
                    "BUILD_INPUT_LIMIT",
                    "Build source exceeds its inventory limit",
                ));
            }
            if relative.is_empty() && name == ".rh-build" {
                return Err(Failure::new(
                    "UNSAFE_BUILD_INPUT",
                    "Compiler output must not already exist in captured source",
                ));
            }
            let path = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            if path.len() > 512 {
                return Err(Failure::new(
                    "BUILD_INPUT_LIMIT",
                    "Build source path exceeds its byte limit",
                ));
            }
            let component = Dir::name(&name)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
            if unsafe {
                libc::fstatat(
                    directory.0.as_raw_fd(),
                    component.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(Failure::io(
                    "Cannot inspect build source",
                    std::io::Error::last_os_error(),
                ));
            }
            let stat = unsafe { stat.assume_init() };
            match stat.st_mode & libc::S_IFMT {
                libc::S_IFDIR => {
                    if depth >= 12 {
                        return Err(Failure::new(
                            "BUILD_INPUT_LIMIT",
                            "Build source exceeds its directory depth limit",
                        ));
                    }
                    pending.push((path, directory.child(&name)?, depth + 1));
                }
                libc::S_IFREG => {
                    let file = directory.regular(&name)?;
                    let metadata = file
                        .metadata()
                        .map_err(|e| Failure::io("Cannot inspect build input", e))?;
                    if metadata.dev() != stat.st_dev || metadata.ino() != stat.st_ino {
                        return Err(Failure::new(
                            "UNSAFE_BUILD_INPUT",
                            "Build input identity changed during inspection",
                        ));
                    }
                    files += 1;
                    bytes += metadata.len();
                    if files > MAX_FILES || bytes > MAX_INPUT {
                        return Err(Failure::new(
                            "BUILD_INPUT_LIMIT",
                            "Build source exceeds its file or byte limit",
                        ));
                    }
                    if path.ends_with(".java") {
                        sources.push(format!("./{path}"));
                    }
                }
                _ => {
                    return Err(Failure::new(
                        "UNSAFE_BUILD_INPUT",
                        "Links and special files are unavailable in build input",
                    ))
                }
            }
        }
    }
    sources.sort();
    Ok(sources)
}
impl Dir {
    fn child_or_clone(&self) -> Result<Self, Failure> {
        Ok(Self(self.0.try_clone().map_err(|e| {
            Failure::io("Cannot retain build input", e)
        })?))
    }
}

fn resolved_executable(path: &Path, system_root: &Path) -> Result<PathBuf, Failure> {
    let real = fs::canonicalize(path).map_err(|_| {
        Failure::new(
            "BUILD_TOOL_UNAVAILABLE",
            format!("Required build tool is unavailable: {}", path.display()),
        )
    })?;
    let metadata = real
        .metadata()
        .map_err(|e| Failure::io("Cannot inspect build tool", e))?;
    if !real.starts_with(system_root) || !metadata.is_file() || metadata.mode() & 0o111 == 0 {
        return Err(Failure::new(
            "BUILD_TOOL_UNAVAILABLE",
            "Build tools must be executable system files under /usr",
        ));
    }
    Ok(real)
}
fn executable(path: &str) -> Result<String, Failure> {
    // Only these fixed system-tool names enter this boundary. Resolving a
    // distro's /etc/alternatives link before confinement keeps the executable
    // inside the already-admitted /usr mount, without exposing alternatives.
    resolved_executable(Path::new(path), Path::new("/usr"))?
        .into_os_string()
        .into_string()
        .map_err(|_| {
            Failure::new(
                "BUILD_TOOL_UNAVAILABLE",
                "System build tool path is not UTF-8",
            )
        })
}
fn memory_file(name: &str, bytes: &[u8]) -> Result<File, Failure> {
    let name = CString::new(name).unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(Failure::io(
            "Cannot create private sandbox configuration",
            std::io::Error::last_os_error(),
        ));
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)
        .map_err(|e| Failure::io("Cannot prepare sandbox configuration", e))?;
    // bwrap --file reads from the descriptor's current position.
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))
        .map_err(|e| Failure::io("Cannot rewind sandbox configuration", e))?;
    Ok(file)
}

pub(super) struct Prepared {
    _root: Dir,
    source: Dir,
    home: Dir,
    configuration: Vec<(&'static str, File)>,
    recipe: Vec<String>,
}
impl Prepared {
    pub fn new(request: &Request) -> Result<Self, Failure> {
        executable("/usr/bin/bwrap")?;
        let root = Dir::absolute(&request.root)?;
        let source = root.child("source")?;
        let home = root.child("home")?;
        let sources = scan_source(&source)?;
        let recipe = match request.recipe {
            Recipe::Javac => {
                let javac = executable("/usr/bin/javac")?;
                if sources.is_empty() {
                    return Err(Failure::new(
                        "BUILD_SOURCE_UNAVAILABLE",
                        "No captured Java sources are available",
                    ));
                }
                source.create(".rh-build")?.create("classes")?;
                let mut arguments: Vec<String> = std::iter::once(javac)
                    .chain(
                        [
                            "-J-Xmx512m",
                            "-J-XX:ActiveProcessorCount=4",
                            "-encoding",
                            "UTF-8",
                            "-proc:none",
                            "-classpath",
                            "/workspace/.rh-build/classes",
                            "-d",
                            "/workspace/.rh-build/classes",
                        ]
                        .into_iter()
                        .map(str::to_owned),
                    )
                    .collect();
                arguments.extend(sources);
                arguments
            }
            Recipe::Gradle => {
                let wrapper = source
                    .0
                    .metadata()
                    .and_then(|_| {
                        fs::symlink_metadata(format!(
                            "/proc/self/fd/{}/gradle/wrapper/gradle-wrapper.jar",
                            source.0.as_raw_fd()
                        ))
                    })
                    .is_ok();
                let mut arguments = if wrapper {
                    source
                        .child("gradle")?
                        .child("wrapper")?
                        .regular("gradle-wrapper.jar")?;
                    let java = executable("/usr/bin/java")?;
                    std::iter::once(java)
                        .chain(
                            [
                                "-Xmx512m",
                                "-XX:ActiveProcessorCount=4",
                                "-Duser.home=/home/build",
                                "-cp",
                                "/workspace/gradle/wrapper/gradle-wrapper.jar",
                                "org.gradle.wrapper.GradleWrapperMain",
                            ]
                            .into_iter()
                            .map(str::to_owned),
                        )
                        .collect::<Vec<_>>()
                } else {
                    executable("/usr/bin/gradle")?;
                    vec!["/usr/bin/gradle".to_owned()]
                };
                arguments.extend(
                    ["--no-daemon", "--console=plain"]
                        .into_iter()
                        .map(str::to_owned),
                );
                if !request.network {
                    arguments.push("--offline".into());
                }
                arguments.push(request.target.argument().into());
                arguments
            }
        };
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        let passwd = format!("build:x:{uid}:{gid}:Build:/home/build:/usr/bin/false\n");
        let group = format!("build:x:{gid}:\n");
        let configuration = vec![
            (
                "/etc/passwd",
                memory_file("build-passwd", passwd.as_bytes())?,
            ),
            ("/etc/group", memory_file("build-group", group.as_bytes())?),
            (
                "/etc/nsswitch.conf",
                memory_file(
                    "build-nsswitch",
                    b"passwd: files\ngroup: files\nhosts: files dns\n",
                )?,
            ),
        ];
        Ok(Self {
            _root: root,
            source,
            home,
            configuration,
            recipe,
        })
    }
    pub fn command(&self, request: &Request, probe: bool) -> Result<Command, Failure> {
        let mut command = Command::new("/usr/bin/bwrap");
        command.args([
            "--unshare-all",
            "--unshare-user",
            "--disable-userns",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--clearenv",
        ]);
        if request.network {
            command.arg("--share-net");
        }
        for (name, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("LANG", "C.UTF-8"),
            ("HOME", "/home/build"),
            ("GRADLE_USER_HOME", "/home/build/.gradle"),
            ("TMPDIR", "/tmp"),
            ("TZ", "UTC"),
        ] {
            command.args(["--setenv", name, value]);
        }
        command.args([
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/bin",
            "/bin",
            "--symlink",
            "usr/lib",
            "/lib",
        ]);
        if Path::new("/usr/lib64").exists() {
            command.args(["--symlink", "usr/lib64", "/lib64"]);
        } else {
            command.args(["--symlink", "usr/lib", "/lib64"]);
        }
        command.args([
            "--dir",
            "/etc",
            "--dir",
            "/etc/ssl",
            "--dir",
            "/etc/ca-certificates",
        ]);
        // Mount only Java configuration and public trust data. Never expose all
        // of /etc, SSL private keys, host passwd/HOME, keyrings, or /run sockets.
        let mut java_configuration: Vec<String> = fs::read_dir("/etc")
            .map_err(|e| Failure::io("Cannot inspect Java configuration availability", e))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name == "java" || name.starts_with("java-"))
            .map(|name| format!("/etc/{name}"))
            .collect();
        java_configuration.sort();
        for path in java_configuration.iter().map(String::as_str).chain([
            "/etc/ssl/certs",
            "/etc/ca-certificates/extracted",
            "/etc/ld.so.cache",
        ]) {
            if Path::new(path).exists() {
                command.args(["--ro-bind", path, path]);
            }
        }
        if request.network {
            for path in ["/etc/resolv.conf", "/etc/hosts"] {
                if Path::new(path).exists() {
                    command.args(["--ro-bind", path, path]);
                }
            }
        }
        for (destination, file) in &self.configuration {
            command
                .arg("--file")
                .arg(file.as_raw_fd().to_string())
                .arg(destination);
        }
        command
            .args(["--proc", "/proc", "--dev", "/dev", "--size"])
            .arg(TMP_BYTES.to_string())
            .args(["--tmpfs", "/tmp", "--dir", "/home", "--bind-fd"])
            .arg(self.home.0.as_raw_fd().to_string())
            .arg("/home/build")
            .arg("--bind-fd")
            .arg(self.source.0.as_raw_fd().to_string())
            .arg("/workspace")
            .args(["--chdir", "/workspace", "--"]);
        if probe {
            command.arg("/usr/bin/true");
        } else {
            command.args(&self.recipe);
        }
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let inherited: Vec<_> = [&self.source.0, &self.home.0]
            .into_iter()
            .chain(self.configuration.iter().map(|(_, file)| file))
            .map(AsRawFd::as_raw_fd)
            .collect();
        let configuration_fds: Vec<_> = self
            .configuration
            .iter()
            .map(|(_, file)| file.as_raw_fd())
            .collect();
        let parent = std::process::id() as i32;
        let timeout = request.timeout_ms;
        unsafe {
            command.pre_exec(move || {
                // Only sandbox setup FDs cross exec. Bubblewrap consumes and
                // closes them before running code; the attempt root stays CLOEXEC.
                for fd in &inherited {
                    let flags = libc::fcntl(*fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                // The preflight consumed these shared open-file offsets. Each
                // subsequent sandbox receives the same complete static files.
                for fd in &configuration_fds {
                    if libc::lseek(*fd, 0, libc::SEEK_SET) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::from_raw_os_error(libc::ECANCELED));
                }
                process::set_limits(timeout)
            });
        }
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn resolver_follows_real_links_inside_system_tree_and_rejects_external_targets() {
        // A real temporary filesystem mirror tests only resolution. These
        // synthetic bytes are never launched or passed off as a system JDK.
        let temporary = tempfile::tempdir().unwrap();
        let system = temporary.path().join("system");
        fs::create_dir_all(system.join("bin")).unwrap();
        fs::create_dir_all(system.join("runtime/bin")).unwrap();
        let system = system.canonicalize().unwrap();
        let tool = system.join("runtime/bin/tool");
        fs::write(&tool, b"synthetic resolver fixture; never executable code").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let entry = system.join("bin/tool");
        symlink("../runtime/bin/tool", &entry).unwrap();
        assert_eq!(
            resolved_executable(&entry, &system).unwrap(),
            tool.canonicalize().unwrap()
        );
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(resolved_executable(&entry, &system).is_err());
        fs::remove_file(&entry).unwrap();
        let outside = temporary.path().join("system-other");
        fs::create_dir(&outside).unwrap();
        let external = outside.join("tool");
        fs::write(&external, b"outside the admitted system tree").unwrap();
        fs::set_permissions(&external, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&external, &entry).unwrap();
        assert!(resolved_executable(&entry, &system).is_err());
        assert!(resolved_executable(&system, &system).is_err());
    }
}
