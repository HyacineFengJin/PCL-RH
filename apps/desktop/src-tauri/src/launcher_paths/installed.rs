//! Read-only distribution resources and a stable, per-user writable root.
//! Portable archives keep their historical co-located layout. Installed apps
//! never copy their resource tree into user data or derive data from cwd.
use super::portable_root;
#[cfg(unix)]
use pcl_core::retained_fs::Security;
use pcl_core::retained_fs::{Directory, Leaf, Traversal};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Component, Path, PathBuf},
};

pub(crate) struct Layout {
    pub(crate) project: PathBuf,
    pub(crate) resources: PathBuf,
    pub(crate) installed: bool,
    program_root: PathBuf,
}

fn absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path.as_os_str().is_empty()
    {
        return Err("安装资源与数据目录必须是无相对片段的绝对路径".into());
    }
    Ok(())
}

fn projected(path: &Path) -> Result<PathBuf, String> {
    absolute(path)?;
    let mut base = path;
    let mut tail = Vec::new();
    loop {
        match fs::symlink_metadata(base) {
            Ok(metadata) => {
                if !metadata.is_dir() && !metadata.file_type().is_symlink() {
                    return Err("数据目录的现存祖先不是文件夹".into());
                }
                let mut result = base.canonicalize().map_err(|error| error.to_string())?;
                for name in tail.iter().rev() {
                    result.push(name);
                }
                return Ok(result);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tail.push(
                    base.file_name()
                        .ok_or("数据目录没有有效的文件名")?
                        .to_os_string(),
                );
                base = base.parent().ok_or("数据目录没有可用的祖先")?;
            }
            Err(error) => return Err(format!("数据目录不可用：{error}")),
        }
    }
}

fn bundle_resources(executable: Option<&Path>) -> Option<(PathBuf, PathBuf)> {
    let executable = executable?.canonicalize().ok()?;
    if executable.file_name()? != "pcl-desktop" {
        return None;
    }
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| (contents.join("Resources/PCL-RH"), bundle.to_path_buf()))
}

fn default_data(
    system: &str,
    home: Option<&OsStr>,
    xdg: Option<&OsStr>,
    local: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let home = || {
        home.map(PathBuf::from)
            .ok_or("无法定位用户目录，请设置 PCL_RH_DATA_ROOT".to_string())
    };
    let result = match system {
        "macos" => home()?.join("Library/Application Support/PCL RH"),
        "linux" => match xdg.filter(|value| !value.is_empty()) {
            Some(value) => PathBuf::from(value).join("pcl-rh"),
            None => home()?.join(".local/share/pcl-rh"),
        },
        "windows" => PathBuf::from(
            local
                .filter(|value| !value.is_empty())
                .ok_or("无法定位 LocalAppData，请设置 PCL_RH_DATA_ROOT")?,
        )
        .join("PCL RH"),
        _ => return Err("此平台尚无安装版数据目录约定".into()),
    };
    absolute(&result)?;
    Ok(result)
}

pub(super) fn resolve(
    executable: Option<&Path>,
    portable: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<Layout, String> {
    let resource = std::env::var_os("PCL_RH_RESOURCE_ROOT");
    let data = std::env::var_os("PCL_RH_DATA_ROOT");
    let home = std::env::var_os("HOME");
    let xdg = std::env::var_os("XDG_DATA_HOME");
    let local = std::env::var_os("LOCALAPPDATA");
    let bundled = if cfg!(target_os = "macos") {
        bundle_resources(executable)
    } else {
        None
    };
    let installed = resource
        .map(|path| (PathBuf::from(path), None))
        .or_else(|| bundled.map(|(resources, program)| (resources, Some(program))));
    match installed {
        Some((resources, program)) => {
            let data = data.map(PathBuf::from).map(Ok).unwrap_or_else(|| {
                default_data(
                    std::env::consts::OS,
                    home.as_deref(),
                    xdg.as_deref(),
                    local.as_deref(),
                )
            })?;
            Layout::installed(&resources, &data, program.as_deref())
        }
        None if data.is_some() => {
            Err("PCL_RH_DATA_ROOT 需要安装版资源目录或完整 macOS .app".into())
        }
        None => {
            let project = portable()?;
            Ok(Layout {
                resources: project.clone(),
                program_root: project.clone(),
                project,
                installed: false,
            })
        }
    }
}

impl Layout {
    fn installed(resources: &Path, data: &Path, program: Option<&Path>) -> Result<Self, String> {
        absolute(resources)?;
        let resources = portable_root(resources)?;
        let program_root = program
            .unwrap_or(&resources)
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let project = projected(data)?;
        if project.starts_with(&program_root) || project.starts_with(&resources) {
            return Err("安装版数据目录不能位于程序或只读发行资源内".into());
        }
        Ok(Self {
            project,
            resources,
            program_root,
            installed: true,
        })
    }

    /// Create only missing directory components through retained parents. Do
    /// not chmod, replace or repair an existing user-selected directory.
    pub(crate) fn initialize(&self) -> Result<(), String> {
        if !self.installed {
            return Ok(());
        }
        let mut base = self.project.as_path();
        let mut tail: Vec<OsString> = Vec::new();
        while !base.try_exists().map_err(|error| error.to_string())? {
            tail.push(
                base.file_name()
                    .ok_or("数据目录缺少有效文件名")?
                    .to_os_string(),
            );
            base = base.parent().ok_or("数据目录缺少可用祖先")?;
        }
        let mut parent = Directory::open_absolute(base).map_err(|error| error.to_string())?;
        let mut created_final = false;
        for name in tail.iter().rev() {
            let name = Leaf::new(name).map_err(|error| error.to_string())?;
            created_final = false;
            let child = match parent.create_private(&name, Traversal::SameFilesystem) {
                Ok(child) => {
                    parent.sync().map_err(|error| error.to_string())?;
                    created_final = true;
                    child
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => parent
                    .child(&name, Traversal::SameFilesystem)
                    .map_err(|error| error.to_string())?,
                Err(error) => return Err(format!("无法创建用户数据目录：{error}")),
            };
            parent
                .recheck(&name, &child.identity().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
            parent = child;
        }
        let metadata = parent.metadata().map_err(|error| error.to_string())?;
        metadata
            .require_owned()
            .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        if !matches!(metadata.security, Security::Unix { mode, extended_acl: false, .. } if mode & 0o022 == 0)
        {
            return Err("用户数据目录允许其他用户写入或具有无法确认的 ACL，未修改原权限".into());
        }
        #[cfg(windows)]
        metadata
            .require_private()
            .map_err(|error| error.to_string())?;
        let actual = self
            .project
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if Directory::open_absolute(&actual)
            .map_err(|error| error.to_string())?
            .identity()
            .map_err(|error| error.to_string())?
            != parent.identity().map_err(|error| error.to_string())?
            || actual.starts_with(&self.resources)
            || actual.starts_with(&self.program_root)
        {
            return Err("用户数据目录在初始化期间发生变化".into());
        }
        if created_final {
            // A genuinely new installation starts with a usable empty game
            // root. Existing data roots and their selected folders are left
            // untouched, including user-controlled aliases and old layouts.
            let minecraft = parent
                .create_private(
                    &Leaf::new("Minecraft").map_err(|error| error.to_string())?,
                    Traversal::SameFilesystem,
                )
                .map_err(|error| error.to_string())?;
            minecraft
                .create_private(
                    &Leaf::new(".minecraft").map_err(|error| error.to_string())?,
                    Traversal::SameFilesystem,
                )
                .map_err(|error| error.to_string())?;
            minecraft.sync().map_err(|error| error.to_string())?;
            parent.sync().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_stable_and_never_relative_to_the_application_or_cwd() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap();
        assert_eq!(
            default_data("linux", Some(home.as_os_str()), None, None).unwrap(),
            home.join(".local/share/pcl-rh")
        );
        assert_eq!(
            default_data("macos", Some(home.as_os_str()), None, None).unwrap(),
            home.join("Library/Application Support/PCL RH")
        );
        assert!(default_data(
            "linux",
            Some(home.as_os_str()),
            Some(OsStr::new("relative")),
            None
        )
        .is_err());
        assert!(default_data("macos", None, None, None).is_err());
    }

    #[test]
    fn installed_resources_and_data_are_separate_and_user_bytes_survive_reinitialization() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let resources = base.join("app space & 资源");
        fs::create_dir(&resources).unwrap();
        fs::write(
            resources.join("pcl-rh.json"),
            br#"{"schemaVersion":1,"productId":"dev.pcl-rh.portable"}"#,
        )
        .unwrap();
        let data = base.join("new user data/个人/PCL RH");
        let layout = Layout::installed(&resources, &data, None).unwrap();
        assert!(!data.exists());
        layout.initialize().unwrap();
        assert!(data.join("Minecraft/.minecraft").is_dir());
        fs::write(data.join("keep.dat"), b"user data").unwrap();
        layout.initialize().unwrap();
        assert_eq!(fs::read(data.join("keep.dat")).unwrap(), b"user data");
        assert!(!resources.join(".pcl-rust").exists());
        assert!(!data.join("pcl-rh.json").exists());
        assert!(Layout::installed(&resources, &resources.join("state"), None).is_err());
        assert!(Layout::installed(&resources, &resources.join("../state"), None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_data_alias_back_into_program_resources_is_rejected_without_writing() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let resources = base.join("resources");
        fs::create_dir(&resources).unwrap();
        fs::write(
            resources.join("pcl-rh.json"),
            br#"{"schemaVersion":1,"productId":"dev.pcl-rh.portable"}"#,
        )
        .unwrap();
        let alias = base.join("data-alias");
        std::os::unix::fs::symlink(&resources, &alias).unwrap();
        assert!(Layout::installed(&resources, &alias.join("new-data"), None).is_err());
        assert!(!resources.join("new-data").exists());
    }
}
