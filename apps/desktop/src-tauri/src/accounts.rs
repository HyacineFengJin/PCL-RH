//! Public profiles and live credentials have separate owners: JSON stores only
//! the former, the OS keyring stores the latter. Credential operations publish
//! a busy stage under admission, release state locks for any password prompt,
//! then commit profile metadata. Cancellation retires network generations;
//! admitted credential publication finishes before accepting edits or closing.
use pcl_auth::{AccountSummary, DeviceChallenge, LaunchIdentity, MicrosoftClient, Session};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedAccount {
    pub profile: AccountSummary,
    pub remembered: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../work/auth-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const CLIENT: &str = "01234567-89ab-cdef-0123-456789abcdef";
    #[test]
    fn missing_id_does_not_start_and_public_settings_survive_reload() {
        let f = Fixture::new();
        let a = Arc::new(Accounts::new(&f.0));
        assert_eq!(a.snapshot().client_id, DEFAULT_CLIENT_ID);
        a.configure(String::new()).unwrap();
        assert!(a.start(false).err().unwrap().contains("Client ID"));
        assert_eq!(a.snapshot().stage, "idle");
        assert!(a.identity().unwrap().is_none());
        assert!(a.configure("wrong-id".into()).is_err());
        a.configure(CLIENT.into()).unwrap();
        assert_eq!(Accounts::new(&f.0).snapshot().client_id, CLIENT);
    }
    #[test]
    fn broken_book_is_preserved() {
        let f = Fixture::new();
        let file = f.0.join(".pcl-rust/accounts.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"broken existing settings").unwrap();
        let a = Accounts::new(&f.0);
        assert!(a.snapshot().warning.is_some());
        assert!(a.configure(CLIENT.into()).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"broken existing settings");
    }
    #[test]
    fn restored_session_only_account_requires_login_and_can_be_removed() {
        let f = Fixture::new();
        let a = Accounts::new(&f.0);
        let id = "1234567890abcdef1234567890abcdef".to_string();
        a.persist(&Book {
            client_id: CLIENT.into(),
            selected: Some(id.clone()),
            accounts: vec![SavedAccount {
                profile: AccountSummary {
                    id: id.clone(),
                    uuid: id.clone(),
                    name: "OnlinePlayer".into(),
                    expires_at: 0,
                    textures: Default::default(),
                },
                remembered: false,
            }],
        })
        .unwrap();
        let a = Accounts::new(&f.0);
        assert!(a.identity().err().unwrap().contains("重新登录"));
        let remembered = a.begin_remove(&id).unwrap();
        a.remove_account(id, remembered).unwrap();
        let state = Accounts::new(&f.0).snapshot();
        assert!(state.accounts.is_empty());
        assert!(state.selected.is_none());
    }
    #[test]
    fn log_redaction_survives_account_removal_and_cancel_is_a_noop_when_idle() {
        let f = Fixture::new();
        let a = Accounts::new(&f.0);
        a.inner
            .lock()
            .unwrap()
            .redactions
            .push("minecraft-secret".into());
        assert_eq!(
            a.redact("token:minecraft-secret:uuid".into()),
            "token:[REDACTED]:uuid"
        );
        a.cancel();
        assert_eq!(a.snapshot().message, "选择离线账户或添加微软账户");
        let serialized = serde_json::to_string(&a.snapshot()).unwrap();
        assert!(!serialized.contains("minecraft-secret"));
    }
    fn profile() -> AccountSummary {
        AccountSummary {
            id: "1234567890abcdef1234567890abcdef".into(),
            uuid: "1234567890abcdef1234567890abcdef".into(),
            name: "OnlinePlayer".into(),
            expires_at: 0,
            textures: Default::default(),
        }
    }
    #[test]
    fn failed_store_or_book_publication_never_claims_remembered() {
        let f = Fixture::new();
        let a = Accounts::new(&f.0);
        let book = Book {
            accounts: vec![SavedAccount {
                profile: profile(),
                remembered: false,
            }],
            ..Default::default()
        };
        a.persist(&book).unwrap();
        {
            let mut i = a.inner.lock().unwrap();
            i.book = book;
            i.credential_busy = true;
        }
        assert!(a
            .finish_save(&profile().id, Err("keyring unavailable".into()))
            .is_err());
        assert!(!a.snapshot().accounts[0].remembered);
        assert!(!a.snapshot().saving);
        assert!(!Accounts::new(&f.0).snapshot().accounts[0].remembered);
        fs::write(&a.file, b"preserve malformed book").unwrap();
        assert!(a.finish_save(&profile().id, Ok(())).is_err());
        assert!(!a.snapshot().accounts[0].remembered);
        assert_eq!(fs::read(&a.file).unwrap(), b"preserve malformed book");
    }
    #[test]
    fn confirmed_store_success_persists_remembered_and_clears_warning() {
        let f = Fixture::new();
        let a = Accounts::new(&f.0);
        let book = Book {
            accounts: vec![SavedAccount {
                profile: profile(),
                remembered: false,
            }],
            ..Default::default()
        };
        a.persist(&book).unwrap();
        {
            let mut i = a.inner.lock().unwrap();
            i.book = book;
            i.credential_busy = true;
            i.warning = Some("previous failure".into());
        }
        a.finish_save(&profile().id, Ok(())).unwrap();
        assert!(Accounts::new(&f.0).snapshot().accounts[0].remembered);
        assert!(a.snapshot().warning.is_none());
        assert!(!a.snapshot().saving);
    }
    #[test]
    fn saving_requires_a_live_session_and_does_not_start_network_login() {
        let f = Fixture::new();
        let a = Accounts::new(&f.0);
        assert!(a.begin_save_login(&profile().id).is_err());
        assert!(!a.snapshot().saving);
        assert_eq!(a.snapshot().stage, "idle");
    }
    #[test]
    fn admission_remains_busy_while_a_keyring_prompt_has_released_state_locks() {
        let f = Fixture::new();
        let a = Accounts::new(&f.0);
        assert!(!a.busy());
        for stage in ["preparing", "waiting", "saving"] {
            a.inner.lock().unwrap().stage = stage.into();
            assert!(a.busy());
        }
        {
            let mut i = a.inner.lock().unwrap();
            i.stage = "idle".into();
            i.credential_busy = true;
        }
        assert!(a.busy());
        a.cancel();
        assert!(a.busy());
        a.finish_save(&profile().id, Err("fixture failure".into()))
            .unwrap_err();
        assert!(!a.busy());
    }
    #[test]
    fn window_close_drains_credential_publication_but_cancels_unpublished_login() {
        let f = crate::integration_tests::Fixture::new();
        let shared = f.shared();
        {
            let mut i = shared.accounts.inner.lock().unwrap();
            i.credential_busy = true;
            i.stage = "saving".into();
        }
        assert!(crate::begin_launcher_close(&shared).wait_for_workers);
        shared
            .accounts
            .finish_save(&profile().id, Err("cancelled keyring prompt".into()))
            .unwrap_err();
        assert!(!crate::begin_launcher_close(&shared).wait_for_workers);
        let f = crate::integration_tests::Fixture::new();
        let shared = f.shared();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut i = shared.accounts.inner.lock().unwrap();
            i.cancel = Some(cancel.clone());
            i.stage = "preparing".into();
        }
        assert!(!crate::begin_launcher_close(&shared).wait_for_workers);
        assert!(cancel.load(Ordering::SeqCst));
        assert!(!shared.accounts.busy());
    }
    #[test]
    fn failed_credential_removal_preserves_profile_and_releases_admission() {
        let f = Fixture::new();
        let a = Accounts::new(&f.0);
        let book = Book {
            accounts: vec![SavedAccount {
                profile: profile(),
                remembered: true,
            }],
            selected: Some(profile().id),
            ..Default::default()
        };
        a.persist(&book).unwrap();
        a.inner.lock().unwrap().book = book;
        assert!(a.begin_remove(&profile().id).unwrap());
        assert!(a.busy());
        assert!(a
            .finish_remove(&profile().id, Err("keyring locked".into()))
            .is_err());
        assert!(!a.busy());
        assert_eq!(
            a.snapshot().selected.as_deref(),
            Some(profile().id.as_str())
        );
        assert_eq!(a.snapshot().accounts.len(), 1);
        assert_eq!(Accounts::new(&f.0).snapshot().accounts.len(), 1);
    }
    #[test]
    fn automatic_device_assistance_is_once_only_and_cancelled_codes_retire() {
        let f = Fixture::new();
        let a = Accounts::new(&f.0);
        {
            let mut i = a.inner.lock().unwrap();
            i.challenge = Some(DeviceChallenge {
                user_code: "ABCD1234".into(),
                verification_uri: "https://www.microsoft.com/link".into(),
                expires_in: 900,
            });
            i.cancel = Some(Arc::new(AtomicBool::new(false)));
        }
        assert!(a.claim_device_assist("stale").is_err());
        assert!(a.claim_device_assist("ABCD1234").unwrap().is_some());
        assert!(a.claim_device_assist("ABCD1234").unwrap().is_none());
        a.cancel();
        assert!(a.claim_device_assist("ABCD1234").is_err());
    }
    #[test]
    fn old_empty_registration_uses_launcher_app_and_corrupt_book_cannot_login() {
        let f = Fixture::new();
        let a = Arc::new(Accounts::new(&f.0));
        a.configure(String::new()).unwrap();
        assert_eq!(Accounts::new(&f.0).snapshot().client_id, DEFAULT_CLIENT_ID);
        fs::write(&a.file, b"malformed").unwrap();
        assert!(a.start(false).is_err());
        assert_eq!(a.snapshot().stage, "idle");
    }
}
// The approved application registration belongs to the launcher, not each user.
pub const DEFAULT_CLIENT_ID: &str = "afab18dd-ed21-41c0-a2b5-82786db2d85b";
#[derive(Clone, Serialize, Deserialize)]
struct Book {
    client_id: String,
    selected: Option<String>,
    accounts: Vec<SavedAccount>,
}
impl Default for Book {
    fn default() -> Self {
        Self {
            client_id: DEFAULT_CLIENT_ID.into(),
            selected: None,
            accounts: Vec::new(),
        }
    }
}
#[derive(Clone, Serialize)]
pub struct AuthState {
    pub client_id: String,
    pub selected: Option<String>,
    pub accounts: Vec<SavedAccount>,
    pub stage: String,
    pub message: String,
    pub challenge: Option<DeviceChallenge>,
    pub warning: Option<String>,
    pub saveable: Vec<String>,
    pub saving: bool,
}
struct Inner {
    book: Book,
    sessions: BTreeMap<String, Session>,
    stage: String,
    message: String,
    challenge: Option<DeviceChallenge>,
    warning: Option<String>,
    generation: u64,
    cancel: Option<Arc<AtomicBool>>,
    // Includes credentials used by a running game even after account selection changes.
    redactions: Vec<String>,
    credential_busy: bool,
    device_assisted: bool,
}
pub struct Accounts {
    file: PathBuf,
    project_key: String,
    inner: Mutex<Inner>,
    appearance: crate::account_appearance::AppearanceCache,
}
impl Accounts {
    pub fn new(project: &Path) -> Self {
        let file = project.join(".pcl-rust/accounts.json");
        let (book, warning) = if file.exists() {
            match fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Book>(&bytes).ok())
            {
                Some(mut book) => {
                    if book.client_id.trim().is_empty() {
                        book.client_id = DEFAULT_CLIENT_ID.into();
                    }
                    (book, None)
                }
                None => (
                    Book::default(),
                    Some("账号列表读取失败，原文件已保留；请先修复 accounts.json".into()),
                ),
            }
        } else {
            (Book::default(), None)
        };
        Self {
            appearance: crate::account_appearance::AppearanceCache::new(project),
            file,
            project_key: project.display().to_string(),
            inner: Mutex::new(Inner {
                book,
                sessions: BTreeMap::new(),
                stage: "idle".into(),
                message: "选择离线账户或添加微软账户".into(),
                challenge: None,
                warning,
                generation: 0,
                cancel: None,
                redactions: Vec::new(),
                credential_busy: false,
                device_assisted: false,
            }),
        }
    }
    fn persist(&self, book: &Book) -> Result<(), String> {
        // Only public profile data and client ID are ever written to this file.
        if self.file.exists()
            && serde_json::from_slice::<Book>(
                &fs::read(&self.file).map_err(|_| "无法读取账号列表")?,
            )
            .is_err()
        {
            return Err("账号列表损坏，原文件保留，请先修复后再保存".into());
        }
        fs::create_dir_all(self.file.parent().unwrap()).map_err(|_| "无法创建账号设置目录")?;
        let temp = self.file.with_extension("tmp");
        fs::write(
            &temp,
            serde_json::to_vec_pretty(book).map_err(|_| "无法编码账号列表")?,
        )
        .map_err(|_| "无法保存账号列表")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
                .map_err(|_| "无法设置账号文件权限")?;
        }
        fs::rename(temp, &self.file).map_err(|_| "无法提交账号列表".into())
    }
    pub fn snapshot(&self) -> AuthState {
        let i = self.inner.lock().unwrap();
        AuthState {
            client_id: i.book.client_id.clone(),
            selected: i.book.selected.clone(),
            accounts: i.book.accounts.clone(),
            stage: i.stage.clone(),
            message: i.message.clone(),
            challenge: i.challenge.clone(),
            warning: i.warning.clone(),
            saveable: i.sessions.keys().cloned().collect(),
            saving: i.credential_busy,
        }
    }
    /// Shared admission checks include the keyring publication phase, even while
    /// its OS prompt waits outside the account and operation locks.
    pub fn busy(&self) -> bool {
        let i = self.inner.lock().unwrap();
        i.cancel.is_some()
            || i.credential_busy
            || matches!(i.stage.as_str(), "preparing" | "waiting" | "saving")
    }
    pub fn configure(&self, id: String) -> Result<(), String> {
        let id = id.trim().to_owned();
        if !id.is_empty() {
            MicrosoftClient::new(&id)?;
        }
        let mut i = self.inner.lock().unwrap();
        if i.cancel.is_some() || i.credential_busy {
            return Err("请先结束当前登录".into());
        }
        let mut book = i.book.clone();
        book.client_id = id;
        self.persist(&book)?;
        i.book = book;
        Ok(())
    }
    pub fn select(&self, id: Option<String>) -> Result<(), String> {
        let mut i = self.inner.lock().unwrap();
        if i.cancel.is_some() || i.credential_busy {
            return Err("请先结束当前登录".into());
        }
        if id
            .as_ref()
            .is_some_and(|id| !i.book.accounts.iter().any(|a| &a.profile.id == id))
        {
            return Err("账号不存在".into());
        }
        let mut book = i.book.clone();
        book.selected = id;
        self.persist(&book)?;
        i.book = book;
        Ok(())
    }
    pub fn cancel(&self) {
        let mut i = self.inner.lock().unwrap();
        // Once credential publication starts, finish it before accepting edits.
        // Otherwise cancellation could leave an unregistered saved credential.
        if i.credential_busy {
            return;
        }
        let Some(c) = i.cancel.take() else {
            return;
        };
        c.store(true, Ordering::SeqCst);
        i.generation += 1;
        i.stage = "idle".into();
        i.message = "已取消登录".into();
        i.challenge = None;
    }
    pub fn start(self: &Arc<Self>, remember: bool) -> Result<(), String> {
        let (client, cancel, generation) = {
            let mut i = self.inner.lock().unwrap();
            if i.cancel.is_some() || i.credential_busy {
                return Err("正在进行微软登录".into());
            }
            if self.file.exists()
                && serde_json::from_slice::<Book>(
                    &fs::read(&self.file).map_err(|_| "无法读取账号列表")?,
                )
                .is_err()
            {
                return Err("账号列表损坏，原文件已保留，请先修复".into());
            }
            let client = MicrosoftClient::new(&i.book.client_id)?;
            let cancel = Arc::new(AtomicBool::new(false));
            i.generation += 1;
            i.cancel = Some(cancel.clone());
            i.stage = "preparing".into();
            i.message = "正在获取微软登录码".into();
            i.challenge = None;
            i.warning = None;
            i.device_assisted = false;
            (client, cancel, i.generation)
        };
        let s = self.clone();
        std::thread::spawn(move || {
            let result = client.login_device(
                &cancel,
                |challenge| {
                    let mut i = s.inner.lock().unwrap();
                    if i.generation == generation && !cancel.load(Ordering::SeqCst) {
                        i.challenge = Some(challenge);
                        i.stage = "waiting".into();
                    }
                },
                |message| {
                    let mut i = s.inner.lock().unwrap();
                    if i.generation == generation && !cancel.load(Ordering::SeqCst) {
                        i.message = message.into();
                    }
                },
            );
            match result {
                Err(error) => {
                    let mut i = s.inner.lock().unwrap();
                    if i.generation == generation {
                        i.cancel = None;
                        i.stage = "error".into();
                        i.message = error;
                        i.challenge = None;
                    }
                }
                Ok(session) => {
                    // The short admission commits ownership. A keyring may open a
                    // password prompt, so never hold the snapshot lock across OS I/O.
                    let delete_previous = {
                        let mut i = s.inner.lock().unwrap();
                        if i.generation != generation || cancel.load(Ordering::SeqCst) {
                            return;
                        }
                        i.credential_busy = true;
                        i.stage = "saving".into();
                        i.message = "正在保存登录".into();
                        i.book
                            .accounts
                            .iter()
                            .any(|a| a.profile.id == session.public().id && a.remembered)
                    };
                    let saved = if remember {
                        pcl_auth::save_session(&s.project_key, &session)
                    } else {
                        if delete_previous {
                            pcl_auth::delete_session(&s.project_key, &session.public().id)
                        } else {
                            Ok(())
                        }
                    };
                    let mut i = s.inner.lock().unwrap();
                    i.credential_busy = false;
                    let profile = session.public();
                    let mut book = i.book.clone();
                    book.accounts.retain(|a| a.profile.id != profile.id);
                    book.accounts.push(SavedAccount {
                        profile: profile.clone(),
                        remembered: remember && saved.is_ok(),
                    });
                    book.selected = Some(profile.id.clone());
                    i.cancel = None;
                    i.challenge = None;
                    match s.persist(&book) {
                        Err(error) => {
                            i.stage = "error".into();
                            i.message = error;
                        }
                        Ok(()) => {
                            i.book = book;
                            i.redactions.push(session.launch_identity().access_token);
                            i.sessions.insert(profile.id, session);
                            i.stage = "success".into();
                            i.message = format!("已登录 {}", profile.name);
                            i.warning = saved.err();
                        }
                    }
                }
            }
        });
        Ok(())
    }
    pub fn begin_remove(&self, id: &str) -> Result<bool, String> {
        let mut i = self.inner.lock().unwrap();
        if i.cancel.is_some() || i.credential_busy {
            return Err("请先结束当前登录".into());
        }
        let remembered = i
            .book
            .accounts
            .iter()
            .find(|a| a.profile.id == id)
            .ok_or("账号不存在")?
            .remembered;
        i.credential_busy = true;
        i.stage = "saving".into();
        i.message = "正在移除账号".into();
        i.warning = None;
        Ok(remembered)
    }
    pub fn remove_account(&self, id: String, remembered: bool) -> Result<(), String> {
        let result = if remembered {
            pcl_auth::delete_session(&self.project_key, &id)
        } else {
            Ok(())
        };
        self.finish_remove(&id, result)
    }
    fn finish_remove(&self, id: &str, result: Result<(), String>) -> Result<(), String> {
        let mut i = self.inner.lock().unwrap();
        i.credential_busy = false;
        i.stage = "idle".into();
        if let Err(error) = result {
            i.warning = Some(error.clone());
            i.message = "账号尚未移除".into();
            return Err(error);
        }
        let mut book = i.book.clone();
        book.accounts.retain(|a| a.profile.id != id);
        if book.selected.as_deref() == Some(id) {
            book.selected = None;
        }
        self.persist(&book)?;
        i.book = book;
        i.sessions.remove(id);
        i.message = "账号已移除".into();
        Ok(())
    }
    /// Retry remembering a live login without repeating Microsoft's consent flow.
    pub fn begin_save_login(&self, id: &str) -> Result<Session, String> {
        {
            let mut i = self.inner.lock().unwrap();
            if i.cancel.is_some() || i.credential_busy {
                return Err("请先结束当前账号操作".into());
            }
            let session = i
                .sessions
                .get(id)
                .cloned()
                .ok_or("登录已不在本次会话中，请重新登录后保存")?;
            if session.launch_identity().client_id != i.book.client_id {
                return Err("应用编号已更改，请重新登录".into());
            }
            if !i.book.accounts.iter().any(|a| a.profile.id == id) {
                return Err("账号不存在".into());
            }
            i.credential_busy = true;
            i.warning = None;
            i.stage = "saving".into();
            i.message = "正在保存登录".into();
            Ok(session)
        }
    }
    pub fn save_login(&self, id: &str, session: Session) -> Result<(), String> {
        let result = pcl_auth::save_session(&self.project_key, &session);
        self.finish_save(id, result)
    }
    fn finish_save(&self, id: &str, result: Result<(), String>) -> Result<(), String> {
        let mut i = self.inner.lock().unwrap();
        i.credential_busy = false;
        i.stage = "success".into();
        let result = result.and_then(|_| {
            let mut book = i.book.clone();
            let account = book
                .accounts
                .iter_mut()
                .find(|a| a.profile.id == id)
                .ok_or("账号已移除")?;
            account.remembered = true;
            self.persist(&book)?;
            i.book = book;
            Ok(())
        });
        match &result {
            Ok(()) => {
                i.warning = None;
                i.message = "登录已保存".into();
            }
            Err(error) => {
                i.warning = Some(error.clone());
                i.message = "登录尚未保存".into();
            }
        }
        result
    }
    pub fn profile(&self, id: &str) -> Result<AccountSummary, String> {
        self.inner
            .lock()
            .unwrap()
            .book
            .accounts
            .iter()
            .find(|a| a.profile.id == id)
            .map(|a| a.profile.clone())
            .ok_or_else(|| "账号不存在".into())
    }
    pub fn appearance(
        &self,
        id: &str,
        refresh: bool,
    ) -> Result<crate::account_appearance::AccountAppearance, String> {
        let profile = self.profile(id)?;
        let appearance = self.appearance.load(&profile, refresh);
        // A removed/replaced profile must not publish a late image to its UI row.
        if self.profile(id)?.textures != profile.textures {
            return Err("账号皮肤已更新，请刷新".into());
        }
        Ok(appearance)
    }
    /// Claim one automatic clipboard/browser request for the active challenge.
    /// Repeated renderer effects and stale replies cannot reopen old login pages.
    pub fn claim_device_assist(&self, code: &str) -> Result<Option<DeviceChallenge>, String> {
        let mut i = self.inner.lock().unwrap();
        let challenge = i
            .challenge
            .clone()
            .filter(|c| c.user_code == code)
            .ok_or("设备代码已更新或登录已结束")?;
        if i.device_assisted {
            return Ok(None);
        }
        i.device_assisted = true;
        Ok(Some(challenge))
    }
    /// Refuses an unavailable/expired online identity instead of silently using offline mode.
    pub fn identity(&self) -> Result<Option<LaunchIdentity>, String> {
        let (id, client_id, cached, remembered) = {
            let i = self.inner.lock().unwrap();
            let Some(id) = i.book.selected.clone() else {
                return Ok(None);
            };
            let account = i
                .book
                .accounts
                .iter()
                .find(|a| a.profile.id == id)
                .ok_or("选择的账号不存在")?;
            (
                id.clone(),
                i.book.client_id.clone(),
                i.sessions.get(&id).cloned(),
                account.remembered,
            )
        };
        let client = MicrosoftClient::new(&client_id)?;
        let session = match cached {
            Some(s) => s,
            None if remembered => {
                pcl_auth::load_session(&self.project_key, &id)?.ok_or("此账号已失效，请重新登录")?
            }
            None => return Err("此账号只在上次会话有效，请重新登录".into()),
        };
        let refreshed = session.expires_soon();
        let session = if refreshed {
            client.refresh(&session)?
        } else {
            session
        };
        let identity = session.launch_identity();
        if identity.client_id != client_id {
            return Err("应用编号已更改，请重新登录".into());
        }
        let warning = if refreshed && remembered {
            pcl_auth::save_session(&self.project_key, &session).err()
        } else {
            None
        };
        let mut i = self.inner.lock().unwrap();
        if i.book.selected.as_ref() != Some(&id) {
            return Err("启动期间账号已更改，请重新启动".into());
        }
        if let Some(a) = i.book.accounts.iter_mut().find(|a| a.profile.id == id) {
            a.profile = session.public();
            if warning.is_some() {
                a.remembered = false;
            }
        }
        i.redactions.push(identity.access_token.clone());
        i.sessions.insert(id, session);
        if warning.is_some() {
            i.warning = warning;
        }
        self.persist(&i.book)?;
        Ok(Some(identity))
    }
    /// A detached game monitor receives this snapshot through an anonymous
    /// pipe. Never serialize it to account/settings files or diagnostic DTOs.
    pub fn redaction_snapshot(&self) -> Vec<String> {
        self.inner.lock().unwrap().redactions.clone()
    }
    pub fn redact(&self, text: String) -> String {
        let i = self.inner.lock().unwrap();
        i.redactions
            .iter()
            .filter(|s| !s.is_empty())
            .fold(text, |text, secret| text.replace(secret, "[REDACTED]"))
    }
}
