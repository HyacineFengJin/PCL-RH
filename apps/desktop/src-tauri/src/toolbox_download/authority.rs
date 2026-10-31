//! Only a real chooser can supply directory authority. A pending download keeps
//! its own descriptor snapshot, so later selections cannot retarget an admitted
//! worker. Session tokens never carry arbitrary frontend filesystem paths.
use super::*;
use std::{
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};
const PREVIEW_LIFETIME: Duration = Duration::from_secs(600);
#[derive(Default)]
pub struct DownloadSession(Mutex<Session>);
#[derive(Default)]
struct Session {
    generation: u64,
    directory: Option<Arc<ChosenDirectory>>,
    pending: Option<PreparedDownload>,
}
pub struct ChosenDirectory {
    token: String,
    pub(crate) directory: files::Directory,
}
pub struct PreparedDownload {
    pub(crate) preview: DownloadPreview,
    // Preserve the exact supplied URL only in native memory. Never Debug/log or
    // serialize it; the display version removes every query value.
    pub(crate) url: reqwest::Url,
    pub(crate) directory: Arc<ChosenDirectory>,
    prepared: Instant,
}
impl ChosenDirectory {
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    pub fn recheck(&self) -> Result<()> {
        self.directory.recheck()
    }
}

fn token(prefix: &str) -> Result<String> {
    let mut bytes = [0u8; 24];
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut bytes))
            .map_err(|_| "无法生成目录下载确认标识")?;
    }
    #[cfg(target_os = "macos")]
    if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return Err("无法生成目录下载确认标识".into());
    }
    #[cfg(windows)]
    {
        use std::{ffi::c_void, ptr};
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(
                provider: *mut c_void,
                bytes: *mut u8,
                length: u32,
                flags: u32,
            ) -> i32;
        }
        const USE_SYSTEM_PREFERRED_RNG: u32 = 2;
        if unsafe {
            BCryptGenRandom(
                ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                USE_SYSTEM_PREFERRED_RNG,
            )
        } != 0
        {
            return Err("无法生成目录下载确认标识".into());
        }
    }
    Ok(format!(
        "{prefix}:{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}
pub struct StartGuard<'a> {
    session: MutexGuard<'a, Session>,
}
impl StartGuard<'_> {
    /// Scheduling uses the confirmed final basename, allowing independent files
    /// in one directory to run together while matching root scopes still block.
    pub fn target(&self) -> PathBuf {
        let pending = self.session.pending.as_ref().unwrap();
        pending
            .directory
            .directory
            .path()
            .join(&pending.preview.file_name)
    }
    pub fn directory(&self) -> &Path {
        self.session
            .pending
            .as_ref()
            .unwrap()
            .directory
            .directory
            .path()
    }
    pub fn take(mut self) -> PreparedDownload {
        self.session.pending.take().unwrap()
    }
}
impl DownloadSession {
    /// A cancelled chooser retains the previous directory; every newly opened
    /// chooser invalidates old previews. A late chooser reply cannot replace a
    /// newer selection or recreate its discarded confirmation.
    pub fn begin_choose(&self) -> (u64, Option<PathBuf>) {
        let mut session = self.0.lock().unwrap();
        session.generation = session.generation.wrapping_add(1);
        session.pending = None;
        let initial = session
            .directory
            .as_ref()
            .map(|directory| directory.directory.path().to_path_buf());
        (session.generation, initial)
    }
    pub fn adopt_directory(&self, generation: u64, path: PathBuf) -> Result<DirectoryView> {
        let directory = files::Directory::open(&path)?;
        directory.recheck()?;
        let chosen = Arc::new(ChosenDirectory {
            token: token("toolbox-directory")?,
            directory,
        });
        let mut session = self.0.lock().unwrap();
        if session.generation != generation {
            return Err("目录选择已被较新的请求替代，请重新选择".into());
        }
        let view = DirectoryView {
            token: chosen.token.clone(),
            directory: chosen.directory.path().display().to_string(),
        };
        session.directory = Some(chosen);
        session.pending = None;
        Ok(view)
    }
    pub fn held_directory(&self, directory_token: &str) -> Result<Arc<ChosenDirectory>> {
        let session = self.0.lock().unwrap();
        let directory = session
            .directory
            .as_ref()
            .filter(|directory| directory.token == directory_token)
            .ok_or("目录选择已失效，请重新选择保存目录")?
            .clone();
        directory.directory.recheck()?;
        Ok(directory)
    }
    pub fn prepare(
        &self,
        request: PrepareRequest,
        preferences_revision: String,
    ) -> Result<DownloadPreview> {
        let mut session = self.0.lock().unwrap();
        // Invalid attempts also invalidate an earlier preview, preventing a
        // changed form from silently starting its previous payload later.
        session.pending = None;
        let url = validate_url(&request.url)?;
        files::validate_name(&request.file_name)?;
        if preferences_revision.is_empty() || preferences_revision.len() > 256 {
            return Err("启动器设置版本无效，请重新确认下载".into());
        }
        let directory = session
            .directory
            .as_ref()
            .filter(|directory| directory.token == request.directory_token)
            .ok_or("目录选择已失效，请重新选择保存目录")?
            .clone();
        directory.directory.recheck()?;
        directory.directory.absent(&request.file_name)?;
        let target = directory
            .directory
            .path()
            .join(&request.file_name)
            .display()
            .to_string();
        let preview = DownloadPreview {
            token: token("toolbox-download")?,
            url_display: display_url(&url),
            file_name: request.file_name,
            directory: directory.directory.path().display().to_string(),
            target,
            preferences_revision,
        };
        session.pending = Some(PreparedDownload {
            preview: preview.clone(),
            url,
            directory,
            prepared: Instant::now(),
        });
        Ok(preview)
    }
    /// Hold the session lock across task admission and consumption. A chooser
    /// reply cannot invalidate the pending object between validation and take;
    /// rejecting task admission simply drops this guard and retains the preview.
    pub fn start_guard<'a>(
        &'a self,
        token: &str,
        preferences_revision: &str,
    ) -> Result<StartGuard<'a>> {
        let session = self.0.lock().unwrap();
        check_pending(&session, token, preferences_revision)?;
        Ok(StartGuard { session })
    }
    #[cfg(test)]
    pub(crate) fn expire_pending(&self) {
        self.0.lock().unwrap().pending.as_mut().unwrap().prepared =
            Instant::now() - PREVIEW_LIFETIME - Duration::from_secs(1);
    }
}
fn check_pending(session: &Session, token: &str, preferences_revision: &str) -> Result<()> {
    let pending = session.pending.as_ref().ok_or("请先确认工具箱下载内容")?;
    if pending.preview.token != token
        || pending.prepared.elapsed() > PREVIEW_LIFETIME
        || pending.preview.preferences_revision != preferences_revision
        || session.directory.as_ref().map(|directory| &directory.token)
            != Some(&pending.directory.token)
    {
        return Err("下载确认已失效，请重新确认地址、文件名和保存目录".into());
    }
    pending.directory.directory.recheck()?;
    pending
        .directory
        .directory
        .absent(&pending.preview.file_name)
}
pub(crate) fn validate_url(value: &str) -> Result<reqwest::Url> {
    if value.is_empty()
        || value.len() > 8192
        || value.trim() != value
        || value.contains('\\')
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err("下载地址为空、过长或含无效字符".into());
    }
    let url = reqwest::Url::parse(value).map_err(|_| "下载地址格式无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("下载地址须为HTTP或HTTPS，且不能含用户名、密码或片段".into());
    }
    Ok(url)
}
pub(crate) fn display_url(url: &reqwest::Url) -> String {
    let mut display = url.clone();
    if display.query().is_some() {
        display.set_query(None);
        format!("{display}?[已隐藏]")
    } else {
        display.to_string()
    }
}
