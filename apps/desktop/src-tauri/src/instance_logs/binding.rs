//! FD-relative identity binding. Reuse the launcher's existing no-follow
//! directory/file opens and single-owner/single-link regular-file checks. Only
//! fixed log child directories and one validated basename can become inputs.
use super::{digest, reader, LogKind, CHANGED};
use crate::launcher_local::filesystem::{check_file, component, Dir, Stamp};
use pcl_core::content_scope::{automatic_scope, ContentScope, MarkerState};
use serde::Serialize;
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
const MAX_JSON_BYTES: u64 = 1024 * 1024;

pub(super) fn valid_name(kind: LogKind, name: &str) -> Result<(), String> {
    component(name)?;
    if name.contains('\\') || !kind.accepts(name) {
        return Err("请选择列表中的游戏日志或崩溃报告文件".into());
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq, Serialize)]
struct Marker {
    device: u64,
    inode: u64,
    mode: u32,
}
fn markers(instance: &Dir) -> Result<(ContentScope, Vec<Option<Marker>>), String> {
    let mut values = Vec::new();
    let scope = automatic_scope(|name| {
        let c = CString::new(name).unwrap();
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let marker = if unsafe {
            libc::fstatat(
                instance.0.as_raw_fd(),
                c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                None
            } else {
                return Err("无法检查实例隔离标记".into());
            }
        } else {
            let stat = unsafe { stat.assume_init() };
            Some(Marker {
                device: stat.st_dev,
                inode: stat.st_ino,
                mode: stat.st_mode,
            })
        };
        let state = match marker.as_ref().map(|m| m.mode & libc::S_IFMT) {
            None => MarkerState::Missing,
            Some(libc::S_IFREG | libc::S_IFDIR) => MarkerState::Present,
            Some(_) => MarkerState::Unsupported,
        };
        values.push(marker);
        Ok(state)
    })?;
    Ok((scope, values))
}

pub(super) struct Binding {
    root_path: PathBuf,
    id: String,
    root: Dir,
    versions: Dir,
    instance: Dir,
    json: File,
    json_stamp: Stamp,
    markers: Vec<Option<Marker>>,
    scope: ContentScope,
    game: Option<Dir>,
    crash: Option<Dir>,
}
impl Binding {
    pub fn capture(path: &Path, id: &str) -> Result<Self, String> {
        pcl_core::identifier(id)?;
        let root = Dir::absolute(path)?;
        let versions = root.child("versions")?;
        let instance = versions.child(id)?;
        let json = instance
            .file(&format!("{id}.json"))?
            .ok_or("未找到所选实例的版本元数据，请刷新实例列表")?;
        let json_stamp = Stamp::of(&json.metadata().map_err(|_| "无法检查实例元数据")?);
        if json_stamp.bytes > MAX_JSON_BYTES {
            return Err("实例元数据超过 1 MiB 上限".into());
        }
        let (scope, markers) = markers(&instance)?;
        let base = if scope.is_isolated() {
            &instance
        } else {
            &root
        };
        let game = base.optional(LogKind::Game.directory())?;
        let crash = base.optional(LogKind::Crash.directory())?;
        Ok(Self {
            root_path: path.into(),
            id: id.into(),
            root,
            versions,
            instance,
            json,
            json_stamp,
            markers,
            scope,
            game,
            crash,
        })
    }
    pub fn game_path(&self) -> PathBuf {
        if self.is_shared() {
            self.root_path.clone()
        } else {
            self.root_path
                .join(self.scope.relative_base(&self.id).expect("validated id"))
        }
    }
    pub fn is_shared(&self) -> bool {
        !self.scope.is_isolated()
    }
    pub fn root_identity(&self) -> Result<(u64, u64), String> {
        self.root.identity()
    }
    pub fn folder(&self, kind: LogKind) -> Option<&Dir> {
        match kind {
            LogKind::Game => self.game.as_ref(),
            LogKind::Crash => self.crash.as_ref(),
        }
    }
    pub fn signature(&self) -> Result<String, String> {
        // Directory mtime/ctime changes on unrelated file additions and is not
        // scope identity. Row file stamps supply the content revision instead.
        digest(&(
            &self.root_path,
            &self.id,
            self.root.identity()?,
            self.versions.identity()?,
            self.instance.identity()?,
            &self.json_stamp,
            &self.markers,
            self.is_shared(),
            self.game.as_ref().map(Dir::identity).transpose()?,
            self.crash.as_ref().map(Dir::identity).transpose()?,
        ))
    }
    pub fn validate_json(&mut self) -> Result<(), String> {
        self.json
            .seek(SeekFrom::Start(0))
            .map_err(|_| "无法读取实例元数据")?;
        let mut bytes = Vec::new();
        (&mut self.json)
            .take(MAX_JSON_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "无法读取实例元数据")?;
        if bytes.len() as u64 != self.json_stamp.bytes {
            return Err(CHANGED.into());
        }
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let json: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| "实例元数据无效，请检查所选实例")?;
        if !json.is_object() {
            return Err("实例元数据格式无效，请检查所选实例".into());
        }
        check_file(
            &self.json,
            &self.json_stamp,
            &self.instance,
            &format!("{}.json", self.id),
        )
    }
    pub fn verify(&self) -> Result<(), String> {
        let root = Dir::absolute(&self.root_path)?;
        let versions = root.child("versions")?;
        let instance = versions.child(&self.id)?;
        if root.identity()? != self.root.identity()?
            || versions.identity()? != self.versions.identity()?
            || instance.identity()? != self.instance.identity()?
        {
            return Err(CHANGED.into());
        }
        check_file(
            &self.json,
            &self.json_stamp,
            &instance,
            &format!("{}.json", self.id),
        )?;
        let (scope, markers) = markers(&instance)?;
        if scope != self.scope || markers != self.markers {
            return Err(CHANGED.into());
        }
        let base = if scope.is_isolated() {
            &instance
        } else {
            &root
        };
        for kind in [LogKind::Game, LogKind::Crash] {
            let fresh = base.optional(kind.directory())?;
            if fresh.as_ref().map(Dir::identity).transpose()?
                != self.folder(kind).map(Dir::identity).transpose()?
            {
                return Err(CHANGED.into());
            }
        }
        Ok(())
    }
}

pub(super) struct Selected {
    kind: LogKind,
    name: String,
    pub file: File,
    pub stamp: Stamp,
}
impl Selected {
    pub fn capture(binding: &Binding, kind: LogKind, name: &str) -> Result<Self, String> {
        valid_name(kind, name)?;
        let file = binding
            .folder(kind)
            .ok_or(CHANGED)?
            .file(name)?
            .ok_or(CHANGED)?;
        let stamp = Stamp::of(&file.metadata().map_err(|_| "无法检查所选游戏日志")?);
        Ok(Self {
            kind,
            name: name.into(),
            file,
            stamp,
        })
    }
    pub fn read(&mut self, closing: &AtomicBool) -> Result<reader::TextTail, String> {
        reader::read(
            &mut self.file,
            &self.stamp,
            self.name.ends_with(".gz"),
            closing,
        )
    }
    pub fn verify(&self, binding: &Binding) -> Result<(), String> {
        check_file(
            &self.file,
            &self.stamp,
            binding.folder(self.kind).ok_or(CHANGED)?,
            &self.name,
        )
    }
}
