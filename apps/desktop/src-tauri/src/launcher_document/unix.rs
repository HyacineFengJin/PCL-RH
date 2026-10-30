//! Descriptor-relative CAS persistence shared by two fixed launcher documents.
//! Preferences and community favorites have independent names, locks and bounds;
//! callers cannot supply arbitrary storage paths. Read-only loads create nothing.
//! A process lock excludes cooperative writers; digest and inode checks detect
//! other editors. Atomic exchange retains their bytes if publication races.
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, Metadata},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

#[cfg(target_os = "linux")]
const RENAME_NEW: u32 = libc::RENAME_NOREPLACE;
#[cfg(target_os = "linux")]
const RENAME_SWAP: u32 = libc::RENAME_EXCHANGE;
#[cfg(target_os = "macos")]
const RENAME_NEW: u32 = libc::RENAME_EXCL;
#[cfg(target_os = "macos")]
const RENAME_SWAP: u32 = libc::RENAME_SWAP;

const DIRECTORY_NAME: &str = ".pcl-rust";
#[derive(Clone, Copy)]
enum DocumentKind {
    Preferences,
    Favorites,
}
impl DocumentKind {
    fn file(self) -> &'static str {
        match self {
            Self::Preferences => "launcher-preferences.json",
            Self::Favorites => "resource-favorites.json",
        }
    }
    fn lock(self) -> &'static str {
        match self {
            Self::Preferences => ".launcher-preferences.lock",
            Self::Favorites => ".resource-favorites.lock",
        }
    }
    fn max_bytes(self) -> usize {
        match self {
            Self::Preferences => 64 * 1024,
            Self::Favorites => 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(meta: &Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    identity: Identity,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self {
            identity: Identity::of(meta),
            bytes: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
    fn of_stat(stat: &libc::stat) -> Self {
        Self {
            identity: Identity {
                device: stat.st_dev as u64,
                inode: stat.st_ino,
            },
            bytes: stat.st_size.max(0) as u64,
            modified: (stat.st_mtime, stat.st_mtime_nsec),
            changed: (stat.st_ctime, stat.st_ctime_nsec),
        }
    }
    fn moved_matches(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.bytes == other.bytes
            && self.modified == other.modified
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    stamp: Stamp,
    digest: [u8; 32],
}
impl Token {
    fn moved_matches(&self, other: &Self) -> bool {
        self.stamp.moved_matches(&other.stamp) && self.digest == other.digest
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    directory: Option<Identity>,
    file: Option<Token>,
}
impl Snapshot {
    pub(crate) fn has_file(&self) -> bool {
        self.file.is_some()
    }
}

pub(crate) struct Directory(File, DocumentKind);
pub(crate) struct WriteLock(File);
impl Drop for WriteLock {
    fn drop(&mut self) {
        // Explicit unlock matters when child processes temporarily inherit a
        // duplicate of this open file description: close alone is insufficient.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(test)]
impl WriteLock {
    pub(crate) fn raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

fn c(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "启动器本地数据路径含有无效字符".into())
}
fn io_error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}

impl Directory {
    /// Walk every component without following directory symlinks. The retained
    /// descriptor is later checked against the visible project path.
    pub(crate) fn open_preferences(path: &Path) -> Result<Self, String> {
        Self::open_for(path, DocumentKind::Preferences)
    }
    pub(crate) fn open_favorites(path: &Path) -> Result<Self, String> {
        Self::open_for(path, DocumentKind::Favorites)
    }
    fn open_for(path: &Path, kind: DocumentKind) -> Result<Self, String> {
        if !path.is_absolute()
            || path
                .components()
                .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
        {
            return Err("启动器本地数据需要规范的绝对项目路径".into());
        }
        let fd = unsafe {
            libc::open(
                c("/")?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io_error("无法打开启动器项目目录"));
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) }, kind);
        for component in path.components() {
            if let Component::Normal(part) = component {
                let child =
                    CString::new(part.as_bytes()).map_err(|_| "启动器项目路径含有无效字符")?;
                dir = dir.child(&child)?;
            }
        }
        Ok(dir)
    }
    pub(crate) fn identity(&self) -> Result<Identity, String> {
        self.0
            .metadata()
            .map(|meta| Identity::of(&meta))
            .map_err(|error| error.to_string())
    }
    fn child(&self, child: &CString) -> Result<Self, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                child.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io_error("无法打开启动器本地数据目录（不允许符号链接）"));
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }, self.1))
    }
    fn stat(&self, name: &str) -> Result<Option<libc::stat>, String> {
        let mut stat = std::mem::MaybeUninit::uninit();
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(Some(unsafe { stat.assume_init() }));
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(io_error("无法检查启动器本地数据文件"))
        }
    }
    pub(crate) fn storage(&self) -> Result<Option<Self>, String> {
        let Some(before) = self.stat(DIRECTORY_NAME)? else {
            return Ok(None);
        };
        if before.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err("启动器本地数据目录不是普通目录，原内容已保留".into());
        }
        let dir = self.child(&c(DIRECTORY_NAME)?)?;
        if dir.identity()?
            != (Identity {
                device: before.st_dev as u64,
                inode: before.st_ino,
            })
        {
            return Err("启动器本地数据目录在读取期间已变化".into());
        }
        Ok(Some(dir))
    }
    pub(crate) fn ensure_storage(&self) -> Result<Self, String> {
        if let Some(dir) = self.storage()? {
            return Ok(dir);
        }
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), c(DIRECTORY_NAME)?.as_ptr(), 0o700) } != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(io_error("无法创建启动器本地数据目录"));
        }
        self.0
            .sync_all()
            .map_err(|error| format!("无法同步启动器本地数据目录：{error}"))?;
        self.storage()?.ok_or("启动器本地数据目录已消失".into())
    }
    fn read(&self, name: &str) -> Result<Option<(Token, Vec<u8>)>, String> {
        let Some(before) = self.stat(name)? else {
            return Ok(None);
        };
        if before.st_mode & libc::S_IFMT != libc::S_IFREG || before.st_nlink != 1 {
            return Err("启动器本地数据必须是独立的普通文件，不允许符号链接或硬链接".into());
        }
        if before.st_size < 0 || before.st_size as u64 > self.1.max_bytes() as u64 {
            return Err(format!(
                "启动器本地数据文件超过 {} KiB 上限",
                self.1.max_bytes() / 1024
            ));
        }
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io_error("无法读取启动器本地数据文件"));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        let stamp = Stamp::of(&metadata);
        if !metadata.is_file() || metadata.nlink() != 1 || stamp != Stamp::of_stat(&before) {
            return Err("启动器本地数据文件在打开期间已变化".into());
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(self.1.max_bytes() as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        let final_stat = self
            .stat(name)?
            .ok_or("启动器本地数据文件在读取期间已消失")?;
        if bytes.len() > self.1.max_bytes()
            || Stamp::of(&file.metadata().map_err(|error| error.to_string())?) != stamp
            || Stamp::of_stat(&final_stat) != stamp
            || final_stat.st_nlink != 1
        {
            return Err(format!(
                "启动器本地数据文件在读取期间已变化或超过 {} KiB 上限",
                self.1.max_bytes() / 1024
            ));
        }
        Ok(Some((
            Token {
                stamp,
                digest: Sha256::digest(&bytes).into(),
            },
            bytes,
        )))
    }
    pub(crate) fn read_bytes(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        self.read(name).map(|value| value.map(|(_, bytes)| bytes))
    }
    pub(crate) fn write_lock(&self) -> Result<WriteLock, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(self.1.lock())?.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io_error("无法打开启动器本地数据写锁"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file.metadata().map_err(|error| error.to_string())?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err("启动器本地数据写锁不是独立的普通文件".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("另一个启动器正在保存设置，请稍后重试".into());
        }
        let guard = WriteLock(file);
        let stat = self
            .stat(self.1.lock())?
            .ok_or("启动器本地数据写锁已消失")?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_nlink != 1
            || Stamp::of_stat(&stat).identity != Identity::of(&meta)
        {
            return Err("启动器本地数据写锁已被外部替换".into());
        }
        Ok(guard)
    }
    fn temporary(&self, name: &str) -> Result<File, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io_error("无法创建启动器本地数据临时文件"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(io_error("无法设置启动器文件权限"));
        }
        Ok(file)
    }
    fn rename(&self, from: &str, to: &str, flags: u32) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                self.0.as_raw_fd(),
                c(from)?.as_ptr(),
                self.0.as_raw_fd(),
                c(to)?.as_ptr(),
                flags,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                self.0.as_raw_fd(),
                c(from)?.as_ptr(),
                self.0.as_raw_fd(),
                c(to)?.as_ptr(),
                flags,
            )
        };
        if result != 0 {
            return Err(io_error("无法原子保存启动器本地数据"));
        }
        Ok(())
    }
    fn unlink(&self, name: &str) -> Result<(), String> {
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), c(name)?.as_ptr(), 0) } != 0 {
            return Err(io_error("无法清理启动器本地数据临时文件"));
        }
        Ok(())
    }
    fn cleanup_owned(&self, name: &str, file: &File, bytes: &[u8]) {
        // Never remove an exchange partner or a temporary file edited by an
        // external process; only our unchanged staged inode is disposable.
        if let Ok(Some((token, content))) = self.read(name) {
            if file
                .metadata()
                .is_ok_and(|meta| Identity::of(&meta) == token.stamp.identity)
                && content == bytes
            {
                let _ = self.unlink(name);
            }
        }
    }
    /// Anonymous staging has no pathname an outside process can substitute.
    /// linkat publishes without replacement, including when a chosen file
    /// appears after its save dialog returned. The visible parent is checked
    /// before and after linking; any failed cleanup only retains our own file.
    #[cfg(target_os = "linux")]
    pub(crate) fn publish_anonymous(
        &self,
        visible_parent: &Path,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        if bytes.len() > self.1.max_bytes() || name.is_empty() || name.contains(['/', '\\']) {
            return Err(format!(
                "本地数据备份名称无效或内容超过 {} KiB",
                self.1.max_bytes() / 1024
            ));
        }
        if self.stat(name)?.is_some() {
            return Err("目标文件已经存在，设置导出不会覆盖已有文件，请选择新文件名".into());
        }
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(".")?.as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io_error(
                "无法创建匿名本地数据备份，文件系统需要支持 O_TMPFILE",
            ));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(io_error("无法本地数据备份文件权限"));
        }
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("无法写入本地数据备份：{error}"))?;
        let result = (|| {
            if Directory::open_for(visible_parent, self.1)?.identity()? != self.identity()? {
                return Err("本地数据备份目标目录已被外部替换".into());
            }
            let direct = unsafe {
                libc::linkat(
                    file.as_raw_fd(),
                    c("")?.as_ptr(),
                    self.0.as_raw_fd(),
                    c(name)?.as_ptr(),
                    libc::AT_EMPTY_PATH,
                )
            };
            if direct != 0 {
                let error = std::io::Error::last_os_error();
                if !matches!(
                    error.raw_os_error(),
                    Some(libc::EPERM) | Some(libc::EINVAL) | Some(libc::ENOENT)
                ) {
                    return Err(format!("无法发布本地数据备份（不会覆盖已有文件）：{error}"));
                }
                // Unprivileged systems may reject AT_EMPTY_PATH. /proc follows
                // only our already-open anonymous inode, never a chosen source.
                let source = c(&format!("/proc/self/fd/{}", file.as_raw_fd()))?;
                if unsafe {
                    libc::linkat(
                        libc::AT_FDCWD,
                        source.as_ptr(),
                        self.0.as_raw_fd(),
                        c(name)?.as_ptr(),
                        libc::AT_SYMLINK_FOLLOW,
                    )
                } != 0
                {
                    return Err(io_error("无法发布本地数据备份（不会覆盖已有文件）"));
                }
            }
            let (token, content) = self.read(name)?.ok_or("本地数据备份在发布期间已消失")?;
            if token.stamp.identity
                != Identity::of(&file.metadata().map_err(|error| error.to_string())?)
                || content != bytes
                || Directory::open_for(visible_parent, self.1)?.identity()? != self.identity()?
            {
                return Err("本地数据备份或目标目录在发布期间已被外部修改".into());
            }
            self.0
                .sync_all()
                .map_err(|error| format!("本地数据备份目录同步失败：{error}"))
        })();
        if result.is_err() {
            self.cleanup_owned(name, &file, bytes);
            let _ = self.0.sync_all();
        }
        result
    }
    /// Darwin has no O_TMPFILE publication. A private workspace protects the
    /// named lease; the native no-replace Stage publishes into the held target.
    /// Interrupted leases remain evidence, never blind name-based cleanup.
    #[cfg(target_os = "macos")]
    pub(crate) fn publish_anonymous(
        &self,
        visible_parent: &Path,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        super::publish_named_export(
            pcl_core::retained_fs::Directory::from_file(
                self.0.try_clone().map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
            visible_parent,
            name,
            bytes,
            self.1.max_bytes(),
        )
    }
}

pub(crate) fn snapshot(project: &Directory) -> Result<(Snapshot, Option<Vec<u8>>), String> {
    let Some(dir) = project.storage()? else {
        return Ok((Snapshot::default(), None));
    };
    let identity = dir.identity()?;
    let read = dir.read(project.1.file())?;
    if project.storage()?.map(|dir| dir.identity()).transpose()? != Some(identity.clone()) {
        return Err("启动器本地数据目录在读取期间已被替换".into());
    }
    Ok((
        Snapshot {
            directory: Some(identity),
            file: read.as_ref().map(|(token, _)| token.clone()),
        },
        read.map(|(_, bytes)| bytes),
    ))
}

pub(crate) fn prepare_directory(
    project: &Directory,
    expected: &mut Snapshot,
) -> Result<Directory, String> {
    let dir = project.ensure_storage()?;
    let identity = dir.identity()?;
    if expected.directory.is_none() && expected.file.is_none() {
        // Other launcher stores may have created the shared directory after our
        // read. Adopt that directory only while our own document is absent.
        if dir.read(project.1.file())?.is_some() {
            return Err("启动器本地数据已被外部创建，请重新读取".into());
        }
        expected.directory = Some(identity.clone());
    }
    if expected.directory.as_ref() != Some(&identity) {
        return Err("启动器本地数据目录已被外部替换".into());
    }
    Ok(dir)
}

pub(crate) fn verify(
    project: &Directory,
    project_path: &Path,
    expected: &Snapshot,
) -> Result<(), String> {
    if Directory::open_for(project_path, project.1)?.identity()? != project.identity()? {
        return Err("启动器项目目录已被外部替换，请重新打开启动器".into());
    }
    let current = snapshot(project)?.0;
    // A previously absent shared directory is safe to adopt later, provided no
    // launcher preference file appeared there in the meantime.
    if expected.directory.is_none() && expected.file.is_none() && current.file.is_none() {
        return Ok(());
    }
    if current != *expected {
        return Err("启动器本地数据已被外部修改，原文件已保留，请重新读取后重试".into());
    }
    Ok(())
}

/// The caller owns the mutex and process lock. `before_replace` is an empty
/// closure in production and a deterministic race/failure gate in fixtures.
pub(crate) fn persist(
    project: &Directory,
    project_path: &Path,
    dir: &Directory,
    expected: &Snapshot,
    bytes: &[u8],
    temporary: &str,
    before_replace: impl FnOnce() -> Result<(), String>,
) -> Result<Snapshot, String> {
    if bytes.len() > project.1.max_bytes() {
        return Err(format!(
            "启动器本地数据文件超过 {} KiB 上限",
            project.1.max_bytes() / 1024
        ));
    }
    verify(project, project_path, expected)?;
    let mut staged_file = dir.temporary(temporary)?;
    let result = (|| {
        staged_file
            .write_all(bytes)
            .and_then(|_| staged_file.sync_all())
            .map_err(|error| format!("无法写入启动器本地数据：{error}"))?;
        let staged = dir
            .read(temporary)?
            .ok_or("启动器本地数据临时文件已消失")?
            .0;
        if staged.stamp != Stamp::of(&staged_file.metadata().map_err(|error| error.to_string())?)
            || staged.digest != Sha256::digest(bytes).as_slice()
        {
            return Err("启动器本地数据临时文件已被外部修改".into());
        }
        verify(project, project_path, expected)?;
        before_replace()?;
        match &expected.file {
            None => dir.rename(temporary, project.1.file(), RENAME_NEW)?,
            Some(before) => {
                dir.rename(temporary, project.1.file(), RENAME_SWAP)?;
                let published = dir
                    .read(project.1.file())?
                    .ok_or("新启动器本地数据在交换期间已消失")?
                    .0;
                if !staged.moved_matches(&published) {
                    return Err(format!(
                        "保存后的设置已被外部修改，双方内容已保留（{temporary}），请检查后重新读取"
                    ));
                }
                // An oversized file, hardlink or symlink introduced in the
                // final race window must also be restored at its original name.
                // We need not read its contents to know it differs from our
                // validated original snapshot; exchange preserves the object.
                let replaced = dir.read(temporary).ok().flatten().map(|(token, _)| token);
                if !replaced
                    .as_ref()
                    .is_some_and(|token| before.moved_matches(token))
                {
                    dir.rename(temporary, project.1.file(), RENAME_SWAP)?;
                    dir.0.sync_all().map_err(|error| {
                        format!("启动器本地数据冲突已回滚，但目录同步失败，请重新读取：{error}")
                    })?;
                    return Err("启动器本地数据在保存期间被外部修改，已恢复并保留原文件".into());
                }
                dir.unlink(temporary)?;
            }
        }
        let published = dir
            .read(project.1.file())?
            .ok_or("保存后的启动器本地数据已消失")?
            .0;
        if !staged.moved_matches(&published) {
            return Err("保存后的启动器本地数据已被外部修改，请重新读取".into());
        }
        dir.0.sync_all().map_err(|error| {
            format!("启动器本地数据已发布，但目录同步失败，请重新读取：{error}")
        })?;
        let after = snapshot(project)?.0;
        if Directory::open_for(project_path, project.1)?.identity()? != project.identity()?
            || after.directory != expected.directory
            || after.file.as_ref() != Some(&published)
        {
            return Err("启动器本地数据目录或文件在保存期间已被外部修改，请重新读取".into());
        }
        Ok(after)
    })();
    if result.is_err() {
        dir.cleanup_owned(temporary, &staged_file, bytes);
    }
    result
}
