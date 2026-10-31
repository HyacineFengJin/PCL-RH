//! Original Linux FD operations and exchange/rollback sequence.
use super::*;
use std::{
    ffi::{CStr, CString},
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
};
pub(crate) struct Dir(File);
fn c(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "引用路径含有无效字符".into())
}
fn error(context: &str) -> String {
    format!("{context}：{}", std::io::Error::last_os_error())
}
impl Dir {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        absolute(path)?;
        let fd = unsafe {
            libc::open(
                c("/")?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(error("无法打开引用目录"));
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            if let Component::Normal(part) = part {
                dir = dir.child(part.to_str().ok_or("引用目录不是 UTF-8")?)?;
            }
        }
        Ok(dir)
    }
    pub(crate) fn identity(&self) -> Result<Identity, String> {
        self.0
            .metadata()
            .map(|metadata| Identity::of(&metadata))
            .map_err(|e| e.to_string())
    }
    pub(crate) fn child(&self, name: &str) -> Result<Self, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(error("无法打开引用目录（不允许符号链接）"));
        }
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }
    pub(crate) fn optional(&self, name: &str) -> Result<Option<Self>, String> {
        if self.stat(name)?.is_none() {
            Ok(None)
        } else {
            self.child(name).map(Some)
        }
    }
    pub(crate) fn ensure(&self, name: &str) -> Result<Self, String> {
        if let Some(dir) = self.optional(name)? {
            return Ok(dir);
        }
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), c(name)?.as_ptr(), 0o700) } != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(error("无法创建引用目录"));
        }
        self.sync()?;
        self.child(name)
    }
    pub(crate) fn stat(&self, name: &str) -> Result<Option<libc::stat>, String> {
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
            Err(error("无法读取引用文件信息"))
        }
    }
    pub(crate) fn read(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Option<(Token, Vec<u8>)>, String> {
        let Some(expected) = self.stat(name)? else {
            return Ok(None);
        };
        if expected.st_mode & libc::S_IFMT != libc::S_IFREG || expected.st_nlink != 1 {
            return Err(
                "引用文件不是独立的普通文件（不允许符号链接或硬链接），原文件已保留".into(),
            );
        }
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(error("无法读取引用文件"));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > limit as u64
            || metadata.dev() != expected.st_dev
            || metadata.ino() != expected.st_ino
        {
            return Err("引用文件过大或读取期间已变化，原文件已保留".into());
        }
        let stamp = Stamp::of(&metadata);
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let final_stat = self.stat(name)?.ok_or("引用文件在读取期间消失")?;
        if bytes.len() > limit
            || Stamp::of(&file.metadata().map_err(|e| e.to_string())?) != stamp
            || final_stat.st_dev != stamp.identity.device
            || final_stat.st_ino != stamp.identity.inode
            || final_stat.st_size.max(0) as u64 != stamp.bytes
            || (final_stat.st_mtime, final_stat.st_mtime_nsec) != stamp.modified
            || (final_stat.st_ctime, final_stat.st_ctime_nsec) != stamp.changed
        {
            return Err("引用文件在读取期间已变化，原文件已保留".into());
        }
        Ok(Some((
            Token {
                stamp,
                hash: hash(&bytes),
            },
            bytes,
        )))
    }
    pub(crate) fn create(&self, name: &str) -> Result<File, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(error("无法创建引用暂存文件"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(crate) fn sync(&self) -> Result<(), String> {
        self.0
            .sync_all()
            .map_err(|e| format!("引用目录同步失败：{e}"))
    }
    pub(crate) fn unlink(&self, name: &str) -> Result<(), String> {
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), c(name)?.as_ptr(), 0) } != 0 {
            return Err(error("无法删除引用文件"));
        }
        self.sync()
    }
    pub(crate) fn rename(&self, from: &str, to: &str, flags: u32) -> Result<(), String> {
        if unsafe {
            libc::renameat2(
                self.0.as_raw_fd(),
                c(from)?.as_ptr(),
                self.0.as_raw_fd(),
                c(to)?.as_ptr(),
                flags,
            )
        } != 0
        {
            return Err(error("无法原子迁移引用文件"));
        }
        Ok(())
    }
    pub(crate) fn names(&self) -> Result<Vec<String>, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(".")?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(error("无法扫描资源历史"));
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = error("无法扫描资源历史");
            unsafe {
                libc::close(fd);
            }
            return Err(error);
        }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        let mut names = Vec::new();
        loop {
            unsafe {
                *libc::__errno_location() = 0;
            }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if std::io::Error::last_os_error().raw_os_error() != Some(0) {
                    return Err(error("资源历史读取失败"));
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            names.push(
                std::str::from_utf8(bytes)
                    .map_err(|_| "资源历史名称无效")?
                    .into(),
            );
            if names.len() > MAX_HISTORY {
                return Err("历史资源记录过多，请先整理后再重命名".into());
            }
        }
        names.sort();
        Ok(names)
    }
    pub(crate) fn lock(&self, name: &str) -> Result<ReferenceLock, String> {
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(error("无法打开引用写锁"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err("引用写锁不是独立的普通文件".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("另一个启动器正在修改引用资料，请稍后重试".into());
        }
        let guard = ReferenceLock(file);
        let stat = self.stat(name)?.ok_or("引用写锁已消失")?;
        if stat.st_dev != metadata.dev() || stat.st_ino != metadata.ino() {
            return Err("引用写锁已被外部替换".into());
        }
        Ok(guard)
    }
    pub(crate) fn update(
        &self,
        name: &str,
        before: Option<&Token>,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<(), String> {
        let now = self.read(name, limit)?.map(|(token, _)| token);
        if now.as_ref() != before {
            return Err("引用文件已被外部修改，未覆盖原文件；重命名仍等待恢复".into());
        }
        let temporary = format!(".rename-reference-{}.tmp", nonce());
        if let Some(bytes) = after {
            if bytes.len() > limit {
                return Err("迁移后的引用文件过大".into());
            }
            let mut file = self.create(&temporary)?;
            let result = (|| {
                file.write_all(bytes)
                    .and_then(|_| file.sync_all())
                    .map_err(|e| e.to_string())?;
                let staged = self.read(&temporary, limit)?.ok_or("引用暂存文件已消失")?.0;
                if staged.stamp != Stamp::of(&file.metadata().map_err(|e| e.to_string())?)
                    || staged.hash != hash(bytes)
                {
                    return Err("引用暂存文件已被外部修改，原文件已保留".into());
                }
                if self.read(name, limit)?.map(|(token, _)| token).as_ref() != before {
                    return Err("引用文件已被外部修改，未覆盖原文件".into());
                }
                if let Some(before) = before {
                    self.rename(&temporary, name, libc::RENAME_EXCHANGE)?;
                    let replaced = self
                        .read(&temporary, limit)?
                        .ok_or("被替换的引用文件已消失")?
                        .0;
                    let published = self.read(name, limit)?.ok_or("迁移后的引用文件已消失")?.0;
                    if !staged.stamp.moved_matches(&published.stamp)
                        || staged.hash != published.hash
                    {
                        return Err(format!("发布后的引用文件已被外部修改，旧资料已保留（{temporary}），请检查后恢复"));
                    }
                    if !before.stamp.moved_matches(&replaced.stamp) || before.hash != replaced.hash
                    {
                        if staged.stamp.moved_matches(&published.stamp)
                            && staged.hash == published.hash
                        {
                            self.rename(&temporary, name, libc::RENAME_EXCHANGE)?;
                            let _ = self.unlink(&temporary);
                            return Err("引用文件在替换期间被外部修改，已还原并保留原文件".into());
                        }
                        return Err(format!(
                            "引用文件并发冲突，已保留双方内容（{temporary}），请检查后恢复"
                        ));
                    }
                    self.unlink(&temporary)?;
                } else {
                    self.rename(&temporary, name, libc::RENAME_NOREPLACE)?;
                    let published = self.read(name, limit)?.ok_or("迁移后的引用文件已消失")?.0;
                    if !staged.stamp.moved_matches(&published.stamp)
                        || staged.hash != published.hash
                    {
                        return Err("发布后的引用文件已被外部修改，原文件已保留".into());
                    }
                }
                self.sync()
            })();
            if result.is_err() {
                // Only clean an owned staged replacement; a swapped-out file
                // may contain external user edits and must remain preserved.
                if self
                    .read(&temporary, limit)
                    .ok()
                    .flatten()
                    .is_some_and(|(token, data)| {
                        file.metadata()
                            .is_ok_and(|metadata| token.stamp.identity == Identity::of(&metadata))
                            && data == bytes
                    })
                {
                    let _ = self.unlink(&temporary);
                }
            }
            result
        } else if let Some(before) = before {
            self.rename(name, &temporary, libc::RENAME_NOREPLACE)?;
            let moved = self
                .read(&temporary, limit)?
                .ok_or("待删除引用文件已消失")?
                .0;
            if !before.stamp.moved_matches(&moved.stamp) || before.hash != moved.hash {
                self.rename(&temporary, name, libc::RENAME_NOREPLACE)?;
                return Err("引用文件在删除期间被外部修改，已保留原文件".into());
            }
            self.unlink(&temporary)
        } else {
            Ok(())
        }
    }
}

pub(crate) struct ReferenceLock(File);
impl Drop for ReferenceLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Dir {
    pub(crate) fn snapshot(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<(DiskSnapshot, Option<Vec<u8>>), String> {
        let value = self.read(name, limit)?;
        Ok((
            DiskSnapshot {
                directory: Some(self.identity()?),
                file: value.as_ref().map(|(token, _)| token.clone()),
            },
            value.map(|(_, bytes)| bytes),
        ))
    }
    pub(crate) fn update_checked(
        &self,
        name: &str,
        before: &DiskSnapshot,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<(), String> {
        if self.snapshot(name, limit)?.0 != *before {
            return Err("引用文档 CAS 冲突".into());
        }
        self.update(name, before.file.as_ref(), after, limit)
    }
    pub(crate) fn publish_marker(
        &self,
        build: impl FnOnce(Identity) -> Result<Vec<u8>, String>,
    ) -> Result<(), String> {
        let temporary = format!(".rename-reference-{}.tmp", nonce());
        let mut file = self.create(&temporary)?;
        let identity = Identity::of(&file.metadata().map_err(|e| e.to_string())?);
        let result = (|| {
            let bytes = build(identity.clone())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            let (token, staged) = self
                .read(&temporary, PRESET_LIMIT)?
                .ok_or("重命名标记暂存文件已消失")?;
            if token.stamp.identity != identity || staged != bytes {
                return Err("重命名标记暂存文件已被外部修改，原文件已保留".into());
            }
            self.rename(&temporary, MARKER, libc::RENAME_NOREPLACE)?;
            self.sync()
        })();
        if result.is_err()
            && self
                .stat(&temporary)
                .ok()
                .flatten()
                .is_some_and(|stat| stat.st_dev == identity.device && stat.st_ino == identity.inode)
        {
            let _ = self.unlink(&temporary);
        }
        result
    }
    pub(crate) fn clear_marker(&self, token: &Token, _bytes: &[u8]) -> Result<(), String> {
        self.update(MARKER, Some(token), None, PRESET_LIMIT)
    }
    pub(crate) fn write_backup(
        &self,
        name: &str,
        bytes: &[u8],
        limit: usize,
    ) -> Result<(), String> {
        let mut file = self.create(name)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        let (token, saved) = self.read(name, limit)?.ok_or("旧设置备份已消失")?;
        if token.stamp != Stamp::of(&file.metadata().map_err(|e| e.to_string())?) || saved != bytes
        {
            return Err("旧设置备份已被外部修改，原设置已保留".into());
        }
        self.sync()
    }
}
