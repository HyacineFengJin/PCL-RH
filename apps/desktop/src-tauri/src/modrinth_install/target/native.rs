//! Native literal resource graph. No pathname fallback or fabricated Unix IDs.
use super::*;
use pcl_core::retained_fs::{self, Directory, Kind, Leaf, Links, Metadata, Traversal};
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Key {
    #[cfg(unix)]
    pub dev: u64,
    #[cfg(unix)]
    pub ino: u64,
    #[cfg(windows)]
    format: &'static str,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
}
impl Key {
    pub fn from_file(file: &File) -> Result<Self> {
        let m = retained_fs::metadata(file).map_err(|e| e.to_string())?;
        match m.identity {
            #[cfg(unix)]
            retained_fs::Identity::Unix { device, inode } => Ok(Self {
                dev: device,
                ino: inode,
            }),
            #[cfg(windows)]
            retained_fs::Identity::Windows { volume, file } => Ok(Self {
                format: "windows-file-id-128-v1",
                volume,
                file_id: file,
            }),
            _ => Err("资源目标身份不属于当前平台".into()),
        }
    }
}
#[cfg(unix)]
pub(crate) type Other = (u64, u64, u32);
#[cfg(windows)]
pub(crate) type Other = (
    retained_fs::Identity,
    Kind,
    retained_fs::Security,
    Option<u32>,
);
pub(crate) fn is_regular(m: &Metadata) -> bool {
    m.kind == Kind::Regular && m.reparse_tag.is_none()
}
pub(crate) fn other(m: &Metadata) -> Other {
    #[cfg(unix)]
    {
        let mode = match m.security {
            retained_fs::Security::Unix { mode, .. } => mode,
            _ => unreachable!("native Unix security"),
        };
        let retained_fs::Identity::Unix { device, inode } = m.identity else {
            unreachable!("native Unix identity")
        };
        (device, inode, mode)
    }
    #[cfg(windows)]
    {
        (
            m.identity.clone(),
            m.kind,
            m.security.clone(),
            m.reparse_tag,
        )
    }
}
pub(crate) struct Dir(pub File, Option<std::sync::Arc<super::super::source::Area>>);
fn native(file: &File) -> Result<Directory> {
    Directory::from_file(file.try_clone().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn leaf(name: &str) -> Result<Leaf> {
    provider::file_name(name)?;
    Leaf::new(name).map_err(|e| e.to_string())
}
impl Dir {
    pub fn open(path: &Path) -> Result<Self> {
        retained_fs::validate_absolute_path(path).map_err(|e| e.to_string())?;
        let d = Directory::open_absolute(path).map_err(|e| e.to_string())?;
        Ok(Self(d.try_clone_file().map_err(|e| e.to_string())?, None))
    }
    pub fn key(&self) -> Result<Key> {
        Key::from_file(&self.0)
    }
    pub fn child(&self, name: &str) -> Result<Self> {
        let d = native(&self.0)?
            .child(&leaf(name)?, Traversal::SameFilesystem)
            .map_err(|e| format!("资源路径含链接、挂载点或已变化：{e}"))?;
        Ok(Self(
            d.try_clone_file().map_err(|e| e.to_string())?,
            self.1.clone(),
        ))
    }
    pub fn regular(&self, name: &str) -> Result<File> {
        native(&self.0)?
            .open_regular(&leaf(name)?, Links::Single)
            .map_err(|e| format!("资源必须是独立普通文件，原内容已保留：{e}"))
    }
    pub fn stat(&self, name: &str) -> Result<Option<Metadata>> {
        native(&self.0)?
            .entry(&leaf(name)?)
            .map_err(|e| e.to_string())
    }
    pub fn optional(&self, name: &str) -> Result<Option<Self>> {
        if self.stat(name)?.is_some() {
            self.child(name).map(Some)
        } else {
            Ok(None)
        }
    }
    pub fn ensure(&self, name: &str) -> Result<Self> {
        match self.optional(name)? {
            Some(d) => Ok(d),
            None => self.create_shared(name),
        }
    }
    fn create_shared(&self, name: &str) -> Result<Self> {
        let parent = native(&self.0)?;
        let d = match parent.create_private(&leaf(name)?, Traversal::SameFilesystem) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => parent
                .child(&leaf(name)?, Traversal::SameFilesystem)
                .map_err(|e| e.to_string())?,
            Err(e) => return Err(e.to_string()),
        };
        parent.sync().map_err(|e| e.to_string())?;
        Ok(Self(
            d.try_clone_file().map_err(|e| e.to_string())?,
            self.1.clone(),
        ))
    }
    pub(crate) fn names(&self) -> Result<Vec<String>> {
        let mut names = native(&self.0)?
            .names(4096)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|n| {
                n.into_string()
                    .map_err(|_| "资源目录含非UTF8名称".to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        for name in &names {
            provider::file_name(name)?;
        }
        names.sort();
        Ok(names)
    }
    pub fn with_input_sources(&self) -> Result<Self> {
        let area = super::super::source::open_area(&self.0, "registered-sources", MAX_BATCH_BYTES)?;
        Ok(Self(
            self.0.try_clone().map_err(|e| e.to_string())?,
            Some(area),
        ))
    }
    pub fn allocate(&self, max: u64) -> Result<super::super::source::Input> {
        if max == 0 || max > MAX_FILE_BYTES {
            return Err("资源下载声明超过大小限制".into());
        }
        let area = self.1.as_ref().ok_or("资源下载缺少来源登记能力")?;
        super::super::source::allocate(area, &self.0, "verified-download-input", max)
    }
    pub fn check_cleanup(&self) -> Result<()> {
        if let Some(area) = &self.1 {
            area.check_cleanup()?;
        }
        Ok(())
    }
}
