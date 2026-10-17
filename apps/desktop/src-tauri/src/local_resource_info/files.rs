//! Read-only source binding. All directories/files are descriptor-relative and
//! no-follow. A batch has a total hashing budget and cancellation deadline; no
//! chooser or writer admission is held during bulk reads. Fingerprints match the
//! resource row format, while SHA256 detects content edits between confirmation.
use crate::launcher_local::filesystem::{check_file, component, Dir, Stamp};
use pcl_core::content_scope::{resolve_scope, ContentScope, IsolationPolicy, MarkerState};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
pub(super) const FILE_LIMIT: u64 = 256 * 1024 * 1024;
pub(super) struct Budget {
    remaining: u64,
    deadline: Instant,
}
impl Budget {
    pub fn new() -> Self {
        Self {
            remaining: 512 * 1024 * 1024,
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }
    pub fn check(&self, closing: &AtomicBool) -> Result<(), String> {
        if closing.load(Ordering::SeqCst) {
            return Err("启动器正在关闭，信息导出已取消".into());
        }
        if Instant::now() >= self.deadline {
            return Err("资源信息读取达到 30 秒上限，请减少选择".into());
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn limited(bytes: u64, duration: Duration) -> Self {
        Self {
            remaining: bytes,
            deadline: Instant::now() + duration,
        }
    }
}
pub(super) struct Pinned {
    pub file: File,
    pub stamp: Stamp,
    pub digest: String,
    pub token: String,
}
fn token(file: &File) -> Result<String, String> {
    let m = file.metadata().map_err(|_| "无法检查资源文件")?;
    Ok(format!(
        "v2:{}:{}:{}:{}:{}:{}:{}",
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.dev(),
        m.ino(),
        m.ctime(),
        m.ctime_nsec()
    ))
}
impl Pinned {
    pub fn read(
        folder: &Dir,
        name: &str,
        max: u64,
        budget: &mut Budget,
        closing: &AtomicBool,
    ) -> Result<Self, String> {
        budget.check(closing)?;
        component(name)?;
        let mut file = folder.file(name)?.ok_or("所选资源文件不存在，请刷新列表")?;
        let stamp = Stamp::of(&file.metadata().map_err(|_| "无法检查资源文件")?);
        if stamp.bytes > max {
            return Err(if max == FILE_LIMIT {
                "所选资源超过单文件 256 MiB 上限"
            } else {
                "实例元数据超过 1 MiB 上限"
            }
            .into());
        }
        if stamp.bytes > budget.remaining {
            return Err("资源信息读取超过总计 512 MiB 上限，请减少选择".into());
        }
        let before = token(&file)?;
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            budget.check(closing)?;
            let read = file.read(&mut buffer).map_err(|_| "无法读取资源信息")?;
            if read == 0 {
                break;
            }
            bytes = bytes.checked_add(read as u64).ok_or("资源文件长度无效")?;
            if bytes > max || read as u64 > budget.remaining {
                return Err("资源文件在读取期间超过大小上限".into());
            }
            budget.remaining -= read as u64;
            hash.update(&buffer[..read]);
        }
        check_file(&file, &stamp, folder, name)?;
        if bytes != stamp.bytes || before != token(&file)? {
            return Err("资源文件在读取期间已变化，请刷新列表".into());
        }
        Ok(Self {
            file,
            stamp,
            digest: format!("{:x}", hash.finalize()),
            token: before,
        })
    }
    pub fn check(&self, folder: &Dir, name: &str) -> Result<(), String> {
        check_file(&self.file, &self.stamp, folder, name)
    }
    pub fn bytes(&mut self, max: usize) -> Result<Vec<u8>, String> {
        if self.stamp.bytes > max as u64 {
            return Err("实例元数据超过大小上限".into());
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "无法读取实例元数据")?;
        let mut bytes = Vec::with_capacity(self.stamp.bytes as usize);
        (&mut self.file)
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "无法读取实例元数据")?;
        if bytes.len() as u64 != self.stamp.bytes {
            return Err("实例元数据已变化".into());
        }
        Ok(bytes)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Marker {
    device: u64,
    inode: u64,
    mode: u32,
}
fn marker(folder: &Dir, name: &str) -> Result<Option<Marker>, String> {
    let name = CString::new(name).unwrap();
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            folder.0.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err("无法核对实例隔离配置".into())
        };
    }
    let stat = unsafe { stat.assume_init() };
    Ok(Some(Marker {
        device: stat.st_dev,
        inode: stat.st_ino,
        mode: stat.st_mode,
    }))
}
fn isolation_snapshot(
    folder: &Dir,
    policy: IsolationPolicy,
) -> Result<(ContentScope, Vec<Option<Marker>>), String> {
    let mut markers = Vec::new();
    let scope = resolve_scope(policy, |name| {
        let found = marker(folder, name)?;
        let state = match found.as_ref().map(|m| m.mode & libc::S_IFMT) {
            None => MarkerState::Missing,
            Some(libc::S_IFREG | libc::S_IFDIR) => MarkerState::Present,
            Some(_) => MarkerState::Unsupported,
        };
        markers.push(found);
        Ok(state)
    })?;
    Ok((scope, markers))
}
pub(super) struct Binding {
    pub folder: Dir,
    root: Dir,
    instance: Dir,
    root_path: PathBuf,
    id: String,
    kind: String,
    json: Pinned,
    markers: Vec<Option<Marker>>,
    policy: IsolationPolicy,
}
impl Binding {
    pub fn capture(
        path: &Path,
        id: &str,
        kind: &str,
        budget: &mut Budget,
        closing: &AtomicBool,
        policy: IsolationPolicy,
    ) -> Result<Self, String> {
        pcl_core::identifier(id)?;
        if !matches!(kind, "mods" | "resourcepacks" | "shaderpacks") {
            return Err("当前仅支持有文件指纹的模组、资源包和光影信息".into());
        }
        let root = Dir::absolute(path)?;
        let instance = root.child("versions")?.child(id)?;
        let mut json = Pinned::read(
            &instance,
            &format!("{id}.json"),
            1024 * 1024,
            budget,
            closing,
        )?;
        let bytes = json.bytes(1024 * 1024)?;
        let content = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let data: serde_json::Value =
            serde_json::from_slice(content).map_err(|_| "实例元数据无效")?;
        if !data.is_object() {
            return Err("实例元数据格式无效".into());
        }
        let (scope, markers) = isolation_snapshot(&instance, policy)?;
        let folder = if scope.is_isolated() {
            instance.child(kind)?
        } else {
            root.child(kind)?
        };
        let bound = Self {
            folder,
            root,
            instance,
            root_path: path.into(),
            id: id.into(),
            kind: kind.into(),
            json,
            markers,
            policy,
        };
        bound.check()?;
        Ok(bound)
    }
    pub fn check(&self) -> Result<(), String> {
        let root = Dir::absolute(&self.root_path)?;
        let instance = root.child("versions")?.child(&self.id)?;
        if root.identity()? != self.root.identity()?
            || instance.identity()? != self.instance.identity()?
        {
            return Err("游戏或实例目录已变化，请刷新后重新导出".into());
        }
        self.json.check(&instance, &format!("{}.json", self.id))?;
        let (scope, markers) = isolation_snapshot(&instance, self.policy)?;
        if markers != self.markers {
            return Err("实例隔离配置已变化，请刷新后重新导出".into());
        }
        let folder = if scope.is_isolated() {
            instance.child(&self.kind)?
        } else {
            root.child(&self.kind)?
        };
        if folder.identity()? != self.folder.identity()? {
            return Err("资源目录已变化，请刷新后重新导出".into());
        }
        Ok(())
    }
}
/// Inspect the ordinary ZIP footer and central directory before constructing
/// ZipArchive. ZIP64/multi-disk/oversized indexes are metadata-unavailable, never
/// permission to decode an unbounded index or extract archive contents.
pub(super) fn bounded_zip(file: &mut File) -> Result<(), String> {
    let size = file.metadata().map_err(|_| "无法检查资源文件")?.len();
    let tail_size = size.min(65557) as usize;
    file.seek(SeekFrom::End(-(tail_size as i64)))
        .map_err(|_| "无法读取 ZIP 索引")?;
    let mut tail = vec![0; tail_size];
    file.read_exact(&mut tail)
        .map_err(|_| "无法读取 ZIP 索引")?;
    let offset = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|i| {
            tail[*i..].starts_with(b"PK\x05\x06")
                && *i + 22 + u16::from_le_bytes(tail[*i + 20..*i + 22].try_into().unwrap()) as usize
                    == tail.len()
        })
        .ok_or("资源不是受支持的普通 ZIP/JAR")?;
    let end = &tail[offset..offset + 22];
    let u16_at = |i| u16::from_le_bytes(end[i..i + 2].try_into().unwrap());
    let u32_at = |i| u32::from_le_bytes(end[i..i + 4].try_into().unwrap());
    let count = u16_at(10);
    let central_size = u32_at(12) as u64;
    let start = u32_at(16) as u64;
    let footer = size - tail_size as u64 + offset as u64;
    if u16_at(4) != 0
        || u16_at(6) != 0
        || u16_at(8) != count
        || count > 32768
        || central_size > 8 * 1024 * 1024
        || start.checked_add(central_size).is_none_or(|v| v > footer)
    {
        return Err("ZIP 索引超过上限或格式不受支持".into());
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|_| "无法读取 ZIP 索引")?;
    let mut consumed = 0u64;
    for _ in 0..count {
        let mut header = [0; 46];
        file.read_exact(&mut header).map_err(|_| "ZIP 索引不完整")?;
        if !header.starts_with(b"PK\x01\x02") {
            return Err("ZIP 索引无效".into());
        }
        let extra = [28, 30, 32]
            .iter()
            .map(|i| u16::from_le_bytes(header[*i..*i + 2].try_into().unwrap()) as u64)
            .sum::<u64>();
        consumed = consumed.checked_add(46 + extra).ok_or("ZIP 索引无效")?;
        if consumed > central_size {
            return Err("ZIP 索引无效".into());
        }
        file.seek(SeekFrom::Current(extra as i64))
            .map_err(|_| "ZIP 索引无效")?;
    }
    if consumed != central_size {
        return Err("ZIP 索引含有额外内容".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "无法读取资源元数据")?;
    Ok(())
}
