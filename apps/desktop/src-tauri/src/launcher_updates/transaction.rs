//! One pending portable installation transaction, with an immutable journal.
//!
//! `journal -> publish next -> exchange -> fsync bin -> committed marker` is
//! the forward order. Startup rolls back any exchange without a durable commit
//! marker. Rollback intent is persisted before its exchange. The two executable
//! inodes are never overwritten or unlinked by this service. Phase markers are
//! append-only, avoiding a journal rewrite that could lose an external edit.
#[cfg(not(target_os = "linux"))]
use super::files;
use super::{
    files::{c, Directory, FileToken, Identity, Stage},
    model::*,
    provider::{hex, MAX_BINARY_BYTES},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::FromRawFd,
    os::unix::fs::MetadataExt,
};
const JOURNAL: &str = "launcher-update-journal.json";
const LOCK: &str = ".launcher-updates.lock";
const BINARY: &str = "pcl-desktop";
const SCHEMA: u32 = if cfg!(target_os = "linux") { 1 } else { 2 };
const MAX_JOURNAL: u64 = 8 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    nonce: String,
    project: Identity,
    storage: Identity,
    bin: Identity,
    original: FileToken,
    replacement: FileToken,
    original_build: DiskBuild,
    replacement_build: DiskBuild,
}
impl Journal {
    fn validate(&self) -> Result<(), String> {
        let valid_file = |file: &FileToken| file.valid();
        let valid_build = |build: &DiskBuild| {
            build.version.len() <= 128
                && semver::Version::parse(&build.version).is_ok()
                && build.commit.as_ref().is_none_or(|commit| hex(commit, 40))
                && hex(&build.sha256, 64)
        };
        if self.schema_version != SCHEMA
            || !hex(&self.nonce, 32)
            || !valid_file(&self.original)
            || !valid_file(&self.replacement)
            || !valid_build(&self.original_build)
            || !valid_build(&self.replacement_build)
            || self.original.sha256 != self.original_build.sha256
            || self.replacement.sha256 != self.replacement_build.sha256
            || self.original.same_identity(&self.replacement)
        {
            return Err("启动器更新日志格式、身份或版本无效；所有文件已保留".into());
        }
        Ok(())
    }
    fn swap_name(&self) -> String {
        format!(".pcl-desktop-update-{}.swap", self.nonce)
    }
    fn marker_name(&self, phase: &str) -> String {
        format!(".launcher-update-{}-{phase}.json", self.nonce)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema_version: u32,
    nonce: String,
    phase: String,
}

#[cfg(target_os = "linux")]
struct Lock(File);
#[cfg(not(target_os = "linux"))]
struct Lock(#[allow(dead_code)] pcl_core::retained_fs::Lock);
#[cfg(target_os = "linux")]
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
struct Fs {
    path: PathBuf,
    project: Directory,
    storage: Directory,
    bin: Directory,
}
impl Fs {
    fn open(path: &Path) -> Result<Self, String> {
        let project = Directory::open(path)?;
        let storage = project.child(&c(".pcl-rust")?)?;
        let bin = storage.child(&c("bin")?)?;
        let fs = Self {
            path: path.into(),
            project,
            storage,
            bin,
        };
        fs.check_visible()?;
        Ok(fs)
    }
    fn check_visible(&self) -> Result<(), String> {
        let project = Directory::open(&self.path)?;
        let storage = project.child(&c(".pcl-rust")?)?;
        let bin = storage.child(&c("bin")?)?;
        if project.identity()? != self.project.identity()?
            || storage.identity()? != self.storage.identity()?
            || bin.identity()? != self.bin.identity()?
        {
            return Err("启动器更新目录被外部替换；所有文件已保留".into());
        }
        Ok(())
    }
    fn bind_journal(&self, journal: &Journal) -> Result<(), String> {
        if self.project.identity()? != journal.project
            || self.storage.identity()? != journal.storage
            || self.bin.identity()? != journal.bin
        {
            return Err("启动器更新日志与当前安装目录的身份不匹配；所有文件已保留".into());
        }
        self.check_visible()
    }
    fn lock(&self) -> Result<Lock, String> {
        #[cfg(target_os = "linux")]
        {
            self.check_visible()?;
            let fd = unsafe {
                libc::openat(
                    self.storage.0.as_raw_fd(),
                    c(LOCK)?.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                return Err("无法打开启动器更新锁".into());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            let meta = file.metadata().map_err(|_| "无法检查启动器更新锁")?;
            if !meta.is_file() || meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
                return Err("启动器更新锁不是独立的当前用户普通文件".into());
            }
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err("另一个进程正在处理启动器更新".into());
            }
            let lock = Lock(file);
            let visible = entry_stat(&self.storage, LOCK)?.ok_or("启动器更新锁已消失")?;
            if visible.st_dev != meta.dev()
                || visible.st_ino != meta.ino()
                || visible.st_nlink != 1
                || visible.st_mode & libc::S_IFMT != libc::S_IFREG
            {
                return Err("启动器更新锁被外部替换".into());
            }
            Ok(lock)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.check_visible()?;
            self.storage
                .held()?
                .lock(&pcl_core::retained_fs::Leaf::new(LOCK).map_err(|e| e.to_string())?)
                .map(Lock)
                .map_err(|e| e.to_string())
        }
    }
    fn journal(&self) -> Result<Option<Journal>, String> {
        let Some((_, bytes)) = read_named(&self.storage, JOURNAL, MAX_JOURNAL, None)? else {
            return Ok(None);
        };
        let journal: Journal =
            serde_json::from_slice(&bytes).map_err(|_| "启动器更新日志不可读取；原文件已保留")?;
        journal.validate()?;
        self.bind_journal(&journal)?;
        Ok(Some(journal))
    }
    fn marker(&self, journal: &Journal, phase: &str) -> Result<bool, String> {
        let Some((_, bytes)) = read_named(
            &self.storage,
            &journal.marker_name(phase),
            MAX_JOURNAL,
            None,
        )?
        else {
            return Ok(false);
        };
        let marker: Marker =
            serde_json::from_slice(&bytes).map_err(|_| "启动器更新阶段标记无效；所有文件已保留")?;
        if marker.schema_version != 1 || marker.nonce != journal.nonce || marker.phase != phase {
            return Err("启动器更新阶段标记不匹配；所有文件已保留".into());
        }
        Ok(true)
    }
    fn mark(&self, journal: &Journal, phase: &str) -> Result<(), String> {
        self.assert_journal(journal)?;
        if self.marker(journal, phase)? {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&Marker {
            schema_version: 1,
            nonce: journal.nonce.clone(),
            phase: phase.into(),
        })
        .map_err(|_| "无法编码更新阶段标记")?;
        self.publish(&self.storage, &journal.marker_name(phase), &bytes)?;
        if !self.marker(journal, phase)? {
            return Err("更新阶段标记保存后消失".into());
        }
        Ok(())
    }
    fn assert_journal(&self, expected: &Journal) -> Result<(), String> {
        self.check_visible()?;
        let actual = self
            .journal()?
            .ok_or("启动器更新日志被外部移除；所有文件已保留")?;
        // Typed equality still requires an immutable byte-for-byte encoding;
        // a manual reformat of a journal is an external edit, not a new phase.
        let expected_bytes = serde_json::to_vec(expected).map_err(|_| "无法编码更新日志")?;
        let (_, actual_bytes) =
            read_named(&self.storage, JOURNAL, MAX_JOURNAL, None)?.ok_or("启动器更新日志已消失")?;
        if actual.nonce != expected.nonce || actual_bytes != expected_bytes {
            return Err("启动器更新日志被外部修改；所有文件已保留".into());
        }
        Ok(())
    }
    fn publish(&self, dir: &Directory, name: &str, bytes: &[u8]) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            self.check_visible()?;
            let fd = unsafe {
                libc::openat(
                    dir.0.as_raw_fd(),
                    c(".")?.as_ptr(),
                    libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err("文件系统不支持安全匿名更新日志".into());
            }
            let mut file = unsafe { File::from_raw_fd(fd) };
            file.write_all(bytes)
                .map_err(|_| "无法写入启动器更新日志")?;
            file.sync_all().map_err(|_| "无法同步启动器更新日志")?;
            link_anonymous(&file, dir, name)?;
            dir.0.sync_all().map_err(|_| "无法同步启动器更新日志目录")?;
            self.check_visible()?;
            let (_, actual) =
                read_named(dir, name, MAX_JOURNAL, None)?.ok_or("更新日志保存后消失")?;
            if actual != bytes {
                return Err("更新日志保存期间被外部修改".into());
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.check_visible()?;
            files::publish_bytes(&self.path, dir, name, bytes)?;
            let (_, actual) =
                read_named(dir, name, MAX_JOURNAL, None)?.ok_or("更新记录发布后消失")?;
            if actual != bytes {
                return Err("MayHavePublished：更新记录发布后改变，证据保留".into());
            }
            self.check_visible()
        }
    }
    fn tokens(&self, journal: &Journal) -> Result<(Option<FileToken>, Option<FileToken>), String> {
        self.assert_journal(journal)?;
        let current = read_named(&self.bin, BINARY, MAX_BINARY_BYTES, None)?.map(|r| r.0);
        let swap =
            read_named(&self.bin, &journal.swap_name(), MAX_BINARY_BYTES, None)?.map(|r| r.0);
        self.check_visible()?;
        Ok((current, swap))
    }
    fn exchange(&self, journal: &Journal) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            self.assert_journal(journal)?;
            if unsafe {
                libc::renameat2(
                    self.bin.0.as_raw_fd(),
                    c(BINARY)?.as_ptr(),
                    self.bin.0.as_raw_fd(),
                    c(&journal.swap_name())?.as_ptr(),
                    libc::RENAME_EXCHANGE,
                )
            } != 0
            {
                return Err("无法原子交换启动器文件；原文件和暂存均已保留".into());
            }
            self.bin
                .sync()
                .map_err(|_| "无法同步启动器交换结果；需要启动恢复")?;
            self.check_visible()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.assert_journal(journal)?;
            #[cfg(target_os = "macos")]
            {
                if unsafe {
                    libc::renameatx_np(
                        self.bin.0.as_raw_fd(),
                        c(BINARY)?.as_ptr(),
                        self.bin.0.as_raw_fd(),
                        c(&journal.swap_name())?.as_ptr(),
                        libc::RENAME_SWAP,
                    )
                } != 0
                {
                    return Err("无法原子交换启动器，原件保留".into());
                }
                self.bin
                    .held()?
                    .sync()
                    .map_err(|e| format!("MayHavePublished：启动器已交换但目录同步未确认：{e}"))?;
                self.check_visible()
                    .map_err(|e| format!("MayHavePublished：{e}"))
            }
            #[cfg(windows)]
            {
                Err("Windows运行中EXE没有受支持的原子交换能力；未开始替换，需要独立应用器".into())
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn entry_stat(dir: &Directory, name: &str) -> Result<Option<libc::stat>, String> {
    let mut stat = std::mem::MaybeUninit::uninit();
    if unsafe {
        libc::fstatat(
            dir.0.as_raw_fd(),
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
        Err("无法检查启动器更新文件".into())
    }
}
fn read_named(
    dir: &Directory,
    name: &str,
    max: u64,
    cancel: Option<&AtomicBool>,
) -> Result<Option<(FileToken, Vec<u8>)>, String> {
    #[cfg(target_os = "linux")]
    {
        let Some(visible) = entry_stat(dir, name)? else {
            return Ok(None);
        };
        if visible.st_mode & libc::S_IFMT != libc::S_IFREG
            || visible.st_nlink != 1
            || visible.st_size < 0
            || visible.st_size as u64 > max
            || visible.st_uid != unsafe { libc::geteuid() }
        {
            return Err("启动器更新文件不是受支持的独立当前用户普通文件；所有内容已保留".into());
        }
        let fd = unsafe {
            libc::openat(
                dir.0.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err("无法读取启动器更新文件".into());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let before = file.metadata().map_err(|_| "无法检查启动器更新文件")?;
        if before.dev() != visible.st_dev
            || before.ino() != visible.st_ino
            || before.nlink() != 1
            || !before.is_file()
        {
            return Err("启动器更新文件在打开期间被外部替换".into());
        }
        let mut digest = Sha256::new();
        let mut bytes = Vec::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
                return Err("启动器更新应用已取消".into());
            }
            let count = file
                .read(&mut buffer)
                .map_err(|_| "无法读取启动器更新文件")?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > max {
                return Err("启动器更新文件在读取期间超过大小限制".into());
            }
            digest.update(&buffer[..count]);
            if max <= MAX_JOURNAL {
                bytes.extend_from_slice(&buffer[..count]);
            }
        }
        let after = file.metadata().map_err(|_| "无法重新检查启动器更新文件")?;
        let now = entry_stat(dir, name)?.ok_or("启动器更新文件在读取期间消失")?;
        let token = FileToken::from_meta(&after, format!("{:x}", digest.finalize()));
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || total != after.len()
            || after.nlink() != 1
            || before.mode() != after.mode()
            || now.st_dev != after.dev()
            || now.st_ino != after.ino()
            || now.st_size < 0
            || now.st_size as u64 != after.len()
            || now.st_mtime != after.mtime()
            || now.st_mtime_nsec != after.mtime_nsec()
            || now.st_ctime != after.ctime()
            || now.st_ctime_nsec != after.ctime_nsec()
            || now.st_nlink != 1
            || now.st_mode & libc::S_IFMT != libc::S_IFREG
        {
            return Err("启动器更新文件在读取期间被外部修改；所有内容已保留".into());
        }
        Ok(Some((token, bytes)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        files::read_named(dir, name, max, cancel)
    }
}
#[cfg(target_os = "linux")]
fn link_anonymous(file: &File, dir: &Directory, name: &str) -> Result<(), String> {
    let name = c(name)?;
    let result = unsafe {
        libc::linkat(
            file.as_raw_fd(),
            c("")?.as_ptr(),
            dir.0.as_raw_fd(),
            name.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    };
    if result == 0 {
        return Ok(());
    }
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
        return Err("更新目标名称已存在，未覆盖任何文件".into());
    }
    let source = c(&format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    if unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            dir.0.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    } != 0
    {
        return Err("无法安全发布更新文件，未覆盖任何文件".into());
    }
    Ok(())
}

fn owned_portable(project: &Path, current: &CurrentBuild) -> bool {
    current.install_channel == InstallChannel::Portable
        && Path::new(&current.executable) == project.join(".pcl-rust/bin/pcl-desktop")
}
fn running_matches(current: &CurrentBuild, disk: &DiskBuild) -> bool {
    let versions = semver::Version::parse(&current.version)
        .ok()
        .zip(semver::Version::parse(&disk.version).ok());
    versions.is_some_and(|(a, b)| a.cmp_precedence(&b).is_eq())
        && match (&current.commit, &disk.commit) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (None, None) => true,
            _ => false,
        }
}
fn applied(journal: &Journal, current: &CurrentBuild) -> InstallationView {
    InstallationView {
        state: InstallationState::Applied,
        disk_build: Some(journal.replacement_build.clone()),
        restart_required: !running_matches(current, &journal.replacement_build),
        rollback: Some(RollbackView {
            token: journal.nonce.clone(),
            original_version: journal.original_build.version.clone(),
            applied_version: journal.replacement_build.version.clone(),
            can_rollback: true,
        }),
        warning: None,
    }
}
fn rolled_back(journal: &Journal, current: &CurrentBuild) -> InstallationView {
    InstallationView {
        state: InstallationState::RolledBack,
        disk_build: Some(journal.original_build.clone()),
        restart_required: !running_matches(current, &journal.original_build),
        rollback: Some(RollbackView {
            token: journal.nonce.clone(),
            original_version: journal.original_build.version.clone(),
            applied_version: journal.replacement_build.version.clone(),
            can_rollback: false,
        }),
        warning: None,
    }
}
pub(super) fn blocked(error: String) -> InstallationView {
    InstallationView {
        state: InstallationState::RecoveryRequired,
        warning: Some(error),
        ..Default::default()
    }
}
fn recover_locked(
    fs: &Fs,
    journal: &Journal,
    current: &CurrentBuild,
) -> Result<InstallationView, String> {
    let committed = fs.marker(journal, "committed")?;
    let rollback_requested = fs.marker(journal, "rollback-requested")?;
    let rolled_back_marker = fs.marker(journal, "rolled-back")?;
    let (visible, swap) = fs.tokens(journal)?;
    let original_current = visible.as_ref() == Some(&journal.original);
    let replacement_current = visible.as_ref() == Some(&journal.replacement);
    let original_swap = swap.as_ref() == Some(&journal.original);
    let replacement_swap = swap.as_ref() == Some(&journal.replacement);
    if committed && !rollback_requested && !rolled_back_marker {
        if replacement_current && original_swap {
            return Ok(applied(journal, current));
        }
        return Err("已提交更新的文件布局被外部修改；所有文件和日志已保留".into());
    }
    // A crash after exchange but before `committed` is always rolled back.
    if replacement_current && original_swap && !rolled_back_marker {
        fs.exchange(journal)?;
        let (visible, swap) = fs.tokens(journal)?;
        if visible.as_ref() != Some(&journal.original)
            || swap.as_ref() != Some(&journal.replacement)
        {
            return Err("恢复交换期间出现外部修改；所有文件已保留".into());
        }
    } else if !(original_current && (replacement_swap || swap.is_none())) {
        return Err("更新恢复遇到外部文件或内容变化；未覆盖任何文件，日志已保留".into());
    }
    fs.mark(journal, "rolled-back")?;
    Ok(rolled_back(journal, current))
}

pub(super) fn has_pending(project: &Path) -> Result<bool, String> {
    #[cfg(target_os = "linux")]
    {
        let root = Directory::open(project)?;
        let Some(stat) = entry_stat(&root, ".pcl-rust")? else {
            return Ok(false);
        };
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err("启动器数据目录不是普通目录".into());
        }
        let storage = root.child(&c(".pcl-rust")?)?;
        Ok(entry_stat(&storage, JOURNAL)?.is_some())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let root = Directory::open(project)?;
        let Some(entry) = files::entry(&root, ".pcl-rust")? else {
            return Ok(false);
        };
        if entry.kind != pcl_core::retained_fs::Kind::Directory || entry.reparse_tag.is_some() {
            return Err("启动器数据目录不是literal目录".into());
        }
        let storage = root.child(&c(".pcl-rust")?)?;
        Ok(files::entry(&storage, JOURNAL)?.is_some())
    }
}

pub(super) fn recover(project: &Path, current: &CurrentBuild) -> InstallationView {
    let run = || -> Result<InstallationView, String> {
        if !has_pending(project)? {
            return Ok(InstallationView::default());
        }
        if !owned_portable(project, current) {
            return Err(
                "发现便携更新恢复日志，但当前运行位置不是该项目管理的启动器；未改动任何文件".into(),
            );
        }
        let fs = Fs::open(project)?;
        let _lock = fs.lock()?;
        let Some(journal) = fs.journal()? else {
            return Ok(InstallationView::default());
        };
        recover_locked(&fs, &journal, current)
    };
    run().unwrap_or_else(blocked)
}

pub(super) fn apply(
    project: &Path,
    current: &CurrentBuild,
    release: &ReleaseView,
    mut stage: Stage,
    cancel: &AtomicBool,
    close_cancellation: &dyn Fn() -> bool,
) -> InstallationView {
    let mut run = || -> Result<InstallationView, String> {
        if cfg!(windows) {
            return Err(
                "Windows运行中EXE当前需要独立应用器，尚未开始替换；校验来源将安全退役".into(),
            );
        }
        if !owned_portable(project, current) {
            return Err("当前启动器不是本项目管理的便携安装，不能应用更新".into());
        }
        let fs = Fs::open(project)?;
        let _lock = fs.lock()?;
        if fs.journal()?.is_some() {
            return Err("仍有上次启动器更新日志，请先恢复或确认已重启版本".into());
        }
        let original = stage
            .original
            .clone()
            .ok_or("更新计划缺少原文件身份，请重新下载并确认")?;
        let current_token = read_named(&fs.bin, BINARY, MAX_BINARY_BYTES, Some(cancel))?
            .ok_or("原启动器文件已不存在")?
            .0;
        if current_token != original {
            return Err("原启动器在生成计划后已被外部修改，未应用更新".into());
        }
        stage.finish(release.bytes, &release.sha256, &release.architecture)?;
        let nonce = super::opaque_token()?;
        #[cfg(not(target_os = "linux"))]
        stage.bind_target(&fs.bin, &format!(".pcl-desktop-update-{nonce}.swap"))?;
        #[cfg(unix)]
        let mode = if cfg!(target_os = "macos") {
            0o700
        } else {
            (original.mode & 0o777) | 0o100
        };
        #[cfg(unix)]
        if unsafe { libc::fchmod(stage.file().as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err("无法设置更新 ELF 的可执行权限".into());
        }
        stage.file().sync_all().map_err(|_| "无法同步更新 ELF")?;
        let replacement = FileToken::from_file(stage.file(), release.sha256.clone())?;
        #[cfg(target_os = "linux")]
        let new_links_ok = stage.file().metadata().map_err(|e| e.to_string())?.nlink() == 0;
        #[cfg(not(target_os = "linux"))]
        let new_links_ok = pcl_core::retained_fs::metadata(stage.file())
            .map_err(|e| e.to_string())?
            .links
            == 1;
        if !replacement.same_filesystem(&fs.bin)? || !new_links_ok {
            return Err("更新暂存与便携安装不在同一文件系统，无法安全交换".into());
        }
        #[cfg(unix)]
        if original.mode & !0o777 != 0 {
            return Err("原启动器含特殊权限，不能应用内替换".into());
        }
        let journal = Journal {
            schema_version: SCHEMA,
            nonce,
            project: fs.project.identity()?,
            storage: fs.storage.identity()?,
            bin: fs.bin.identity()?,
            original_build: DiskBuild {
                version: current.version.clone(),
                commit: current.commit.clone(),
                sha256: original.sha256.clone(),
            },
            replacement_build: DiskBuild {
                version: release.version.clone(),
                commit: Some(release.commit.clone()),
                sha256: replacement.sha256.clone(),
            },
            original,
            replacement,
        };
        journal.validate()?;
        let bytes = serde_json::to_vec(&journal).map_err(|_| "无法编码启动器更新日志")?;
        fs.publish(&fs.storage, JOURNAL, &bytes)?;
        #[cfg_attr(target_os = "linux", allow(unused_mut))]
        // Native publication consumes the retained Ticket.
        let mut forward = || -> Result<InstallationView, String> {
            if cancel.load(Ordering::Acquire) {
                return Err("更新应用已取消，原启动器保留".into());
            }
            fs.assert_journal(&journal)?;
            #[cfg(target_os = "linux")]
            link_anonymous(stage.file(), &fs.bin, &journal.swap_name())?;
            #[cfg(not(target_os = "linux"))]
            stage.publish(&fs.bin, &journal.swap_name())?;
            fs.bin.sync().map_err(|_| "无法同步更新发布目录")?;
            let (visible, swap) = fs.tokens(&journal)?;
            if visible.as_ref() != Some(&journal.original)
                || swap.as_ref() != Some(&journal.replacement)
            {
                return Err("更新交换前出现外部修改，所有文件已保留".into());
            }
            if !close_cancellation() {
                return Err("更新应用已取消，原启动器保留".into());
            }
            // Cancellation ends here. Once exchange begins, complete durability
            // or recovery before releasing the operation's admission.
            fs.exchange(&journal)?;
            let (visible, swap) = fs.tokens(&journal)?;
            if visible.as_ref() != Some(&journal.replacement)
                || swap.as_ref() != Some(&journal.original)
            {
                return Err("更新交换后出现外部修改，所有文件已保留".into());
            }
            fs.mark(&journal, "committed")?;
            Ok(applied(&journal, current))
        };
        match forward() {
            Ok(view) => Ok(view),
            Err(error) => {
                let mut recovered = recover_locked(&fs, &journal, current).unwrap_or_else(blocked);
                recovered.warning = Some(match recovered.warning.take() {
                    Some(warning) => format!("{error}；{warning}"),
                    None => error,
                });
                Ok(recovered)
            }
        }
    };
    run().unwrap_or_else(|error| {
        // Before a journal exists there is no filesystem transaction to
        // recover. Preserve that distinction for ordinary plan/permission
        // failures; uncertain journal state still requires startup recovery.
        if matches!(has_pending(project), Ok(false)) {
            InstallationView {
                state: InstallationState::Error,
                warning: Some(error),
                ..Default::default()
            }
        } else {
            blocked(error)
        }
    })
}

pub(super) fn rollback(project: &Path, current: &CurrentBuild, token: &str) -> InstallationView {
    let run = || -> Result<InstallationView, String> {
        if !owned_portable(project, current) {
            return Err("当前安装位置不支持便携更新回滚".into());
        }
        let fs = Fs::open(project)?;
        let _lock = fs.lock()?;
        let journal = fs.journal()?.ok_or("没有待回滚的启动器更新")?;
        if token != journal.nonce {
            return Err("回滚选择已过期，请刷新更新状态".into());
        }
        let view = recover_locked(&fs, &journal, current)?;
        if view.state != InstallationState::Applied {
            return Ok(view);
        }
        fs.mark(&journal, "rollback-requested")?;
        recover_locked(&fs, &journal, current)
    };
    run().unwrap_or_else(blocked)
}

/// Acknowledgement releases the one pending transaction without deleting its
/// original inode. The immutable journal is archived with NOREPLACE; its small
/// phase markers and original backup remain available for manual recovery.
pub(super) fn acknowledge(project: &Path, current: &CurrentBuild, token: &str) -> InstallationView {
    let run = || -> Result<InstallationView, String> {
        if !owned_portable(project, current) {
            return Err("当前安装位置不支持确认便携更新".into());
        }
        let fs = Fs::open(project)?;
        let _lock = fs.lock()?;
        let journal = fs.journal()?.ok_or("没有待确认的启动器更新")?;
        if token != journal.nonce {
            return Err("更新确认选择已过期".into());
        }
        let mut view = recover_locked(&fs, &journal, current)?;
        let disk = view.disk_build.as_ref().ok_or("无法确认磁盘版本")?;
        if !running_matches(current, disk) {
            return Err("当前进程尚未运行磁盘上的版本，请重启启动器后确认".into());
        }
        fs.assert_journal(&journal)?;
        let history = format!("launcher-update-journal-{}.json", journal.nonce);
        #[cfg(target_os = "linux")]
        if unsafe {
            libc::renameat2(
                fs.storage.0.as_raw_fd(),
                c(JOURNAL)?.as_ptr(),
                fs.storage.0.as_raw_fd(),
                c(&history)?.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err("无法归档更新日志，未覆盖任何文件".into());
        }
        #[cfg(not(target_os = "linux"))]
        pcl_core::file_ops::rename_noreplace_at(
            &fs.storage.0,
            std::ffi::OsStr::new(JOURNAL),
            &fs.storage.0,
            std::ffi::OsStr::new(&history),
        )
        .map_err(|e| format!("MayHavePublished：更新日志归档需要复核：{e}"))?;
        fs.storage.sync().map_err(|_| "无法同步更新日志归档")?;
        let (_, bytes) =
            read_named(&fs.storage, &history, MAX_JOURNAL, None)?.ok_or("归档更新日志已消失")?;
        if bytes != serde_json::to_vec(&journal).map_err(|_| "无法验证归档更新日志")? {
            return Err("更新日志归档期间被外部修改；内容已保留".into());
        }
        fs.check_visible()?;
        view.rollback = None;
        view.warning = Some("更新已确认；原可执行文件的 inode 与恢复日志仍保留在项目目录".into());
        Ok(view)
    };
    run().unwrap_or_else(blocked)
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;
