//! Microsoft consumer device authorization. Secrets never implement Debug or Serialize.
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
mod response;
mod textures;
use response::{auth_error, decode, read_json, receive, AuthStage};
pub use textures::{official_texture_url, ProfileTextures, TextureReference};
const SCOPE: &str = "XboxLive.signin offline_access";
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Serialize, Deserialize)]
pub struct AccountSummary {
    pub id: String,
    pub uuid: String,
    pub name: String,
    pub expires_at: u64,
    #[serde(default)]
    pub textures: ProfileTextures,
}
#[derive(Clone, Serialize)]
pub struct DeviceChallenge {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
}
#[derive(Clone)]
pub struct Session {
    account: AccountSummary,
    access_token: String,
    refresh_token: String,
    xuid: String,
    client_id: String,
}
pub struct LaunchIdentity {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub xuid: String,
    pub client_id: String,
}
impl Session {
    pub fn public(&self) -> AccountSummary {
        self.account.clone()
    }
    pub fn expires_soon(&self) -> bool {
        self.account.expires_at <= now().saturating_add(300)
    }
    pub fn launch_identity(&self) -> LaunchIdentity {
        LaunchIdentity {
            name: self.account.name.clone(),
            uuid: self.account.uuid.clone(),
            access_token: self.access_token.clone(),
            xuid: self.xuid.clone(),
            client_id: self.client_id.clone(),
        }
    }
}
struct Endpoints {
    device: String,
    token: String,
    xbox: String,
    xsts: String,
    minecraft: String,
    entitlements: String,
    profile: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            device: "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode".into(),
            token: "https://login.microsoftonline.com/consumers/oauth2/v2.0/token".into(),
            xbox: "https://user.auth.xboxlive.com/user/authenticate".into(),
            xsts: "https://xsts.auth.xboxlive.com/xsts/authorize".into(),
            minecraft: "https://api.minecraftservices.com/authentication/login_with_xbox".into(),
            entitlements: "https://api.minecraftservices.com/entitlements/mcstore".into(),
            profile: "https://api.minecraftservices.com/minecraft/profile".into(),
        }
    }
}
pub struct MicrosoftClient {
    http: Client,
    client_id: String,
    endpoints: Endpoints,
}
fn field(v: &Value, k: &str) -> Result<String, String> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "认证服务器响应不完整，请重试".into())
}
fn numeric_xuid(claims: &Value) -> Option<String> {
    claims["xid"]
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_owned)
}
fn cancelled(c: &AtomicBool) -> Result<(), String> {
    if c.load(Ordering::Relaxed) {
        Err("已取消微软登录".into())
    } else {
        Ok(())
    }
}
fn wait(c: &AtomicBool, until: Instant, deadline: Instant) -> Result<(), String> {
    loop {
        cancelled(c)?;
        let t = Instant::now();
        if t >= deadline {
            return Err("设备登录码已过期，请重新登录".into());
        }
        if t >= until {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100).min(until - t));
    }
}
impl MicrosoftClient {
    pub fn new(client_id: &str) -> Result<Self, String> {
        let id = client_id.trim();
        if id.is_empty() {
            return Err("启动器尚未配置 Microsoft 登录，请联系项目维护者".into());
        }
        if id.len() != 36
            || !id.chars().enumerate().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c == '-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
        {
            return Err("Microsoft Client ID 必须是应用注册的 UUID".into());
        }
        Ok(Self {
            http: pcl_network::blocking_client()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "无法初始化认证网络客户端".to_string())?,
            client_id: id.into(),
            endpoints: Endpoints::default(),
        })
    }
    pub fn login_device(
        &self,
        c: &AtomicBool,
        on_challenge: impl Fn(DeviceChallenge),
        on_stage: impl Fn(&str),
    ) -> Result<Session, String> {
        cancelled(c)?;
        on_stage("正在获取微软设备登录码");
        let started = Instant::now();
        let v = decode(
            AuthStage::DeviceCode,
            self.http
                .post(&self.endpoints.device)
                .form(&[("client_id", self.client_id.as_str()), ("scope", SCOPE)])
                .send(),
        )?;
        let code = field(&v, "device_code")?;
        let expiry = v["expires_in"]
            .as_u64()
            .filter(|x| *x > 0 && *x <= 3600)
            .ok_or("认证服务器返回无效登录码有效期")?;
        // Always use a fixed Microsoft URL; never expose a server-supplied navigation target.
        cancelled(c)?;
        let user_code = field(&v, "user_code")?;
        if user_code.len() > 32
            || !user_code
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err("认证服务器返回无效设备代码".into());
        }
        on_challenge(DeviceChallenge {
            user_code,
            verification_uri: "https://www.microsoft.com/link".into(),
            expires_in: expiry,
        });
        let deadline = started + Duration::from_secs(expiry);
        let mut interval = v["interval"].as_u64().unwrap_or(5).clamp(1, expiry);
        on_stage("等待微软账户授权");
        let token = loop {
            wait(c, Instant::now() + Duration::from_secs(interval), deadline)?;
            let r = receive(
                AuthStage::MicrosoftToken,
                self.http
                    .post(&self.endpoints.token)
                    .form(&[
                        ("client_id", self.client_id.as_str()),
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("device_code", code.as_str()),
                    ])
                    .send(),
            )?;
            let status = r.status();
            let ok = status.is_success();
            let v = read_json(AuthStage::MicrosoftToken, r)?;
            cancelled(c)?;
            if ok {
                break v;
            }
            match v["error"].as_str() {
                Some("authorization_pending") => {}
                Some("slow_down") => interval = interval.saturating_add(5).min(expiry),
                _ => return Err(auth_error(AuthStage::MicrosoftToken, status, &v)),
            }
        };
        self.exchange(token, None, c, &on_stage)
    }
    pub fn refresh(&self, s: &Session) -> Result<Session, String> {
        if s.client_id != self.client_id {
            return Err("账户使用的 Client ID 已更改，请重新登录".into());
        }
        let v = decode(
            AuthStage::MicrosoftToken,
            self.http
                .post(&self.endpoints.token)
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", s.refresh_token.as_str()),
                    ("scope", SCOPE),
                ])
                .send(),
        )?;
        let refreshed =
            self.exchange(v, Some(&s.refresh_token), &AtomicBool::new(false), &|_| {})?;
        if refreshed.account.id != s.account.id {
            return Err("刷新后的账户身份不匹配，请重新登录".into());
        }
        Ok(refreshed)
    }
    fn exchange(
        &self,
        v: Value,
        old: Option<&str>,
        c: &AtomicBool,
        stage: &impl Fn(&str),
    ) -> Result<Session, String> {
        let ms = field(&v, "access_token")?;
        let refresh = v["refresh_token"]
            .as_str()
            .or(old)
            .filter(|s| !s.is_empty())
            .ok_or("微软未返回刷新凭据，请重新授权")?
            .to_owned();
        cancelled(c)?;
        stage("正在验证 Xbox Live 账户");
        let x=decode(AuthStage::XboxLive, self.http.post(&self.endpoints.xbox).json(&json!({"Properties":{"AuthMethod":"RPS","SiteName":"user.auth.xboxlive.com","RpsTicket":format!("d={ms}")},"RelyingParty":"http://auth.xboxlive.com","TokenType":"JWT"})).send())?;
        let xbox = field(&x, "Token")?;
        let xbox_claims = &x["DisplayClaims"]["xui"][0];
        let xbox_uhs = field(xbox_claims, "uhs")?;
        let xbox_xuid = numeric_xuid(xbox_claims);
        cancelled(c)?;
        stage("正在获取 Xbox 安全令牌");
        let x=decode(AuthStage::Xsts, self.http.post(&self.endpoints.xsts).json(&json!({"Properties":{"SandboxId":"RETAIL","UserTokens":[xbox]},"RelyingParty":"rp://api.minecraftservices.com/","TokenType":"JWT"})).send())?;
        let token = field(&x, "Token")?;
        let claims = &x["DisplayClaims"]["xui"][0];
        let uhs = field(claims, "uhs")?;
        if uhs != xbox_uhs {
            return Err("Xbox 身份验证结果不一致，请重新登录".into());
        }
        let xuid = numeric_xuid(claims).or(xbox_xuid).unwrap_or_default();
        cancelled(c)?;
        stage("正在登录 Minecraft 服务");
        let response = receive(
            AuthStage::MinecraftLogin,
            self.http
                .post(&self.endpoints.minecraft)
                .json(&json!({"identityToken":format!("XBL3.0 x={uhs};{token}")}))
                .send(),
        )?;
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(AuthStage::MinecraftLogin.http(reqwest::StatusCode::FORBIDDEN));
        }
        let mc = decode(AuthStage::MinecraftLogin, Ok(response))?;
        let access = field(&mc, "access_token")?;
        let expiry = mc["expires_in"]
            .as_u64()
            .filter(|x| *x > 0)
            .ok_or("Minecraft 令牌有效期无效")?;
        cancelled(c)?;
        stage("正在检查 Minecraft Java 资格");
        let entitlement = decode(
            AuthStage::Entitlements,
            self.http
                .get(&self.endpoints.entitlements)
                .bearer_auth(&access)
                .send(),
        )?;
        if !entitlement["items"].is_array() {
            return Err("Minecraft 资格响应格式错误".into());
        }
        cancelled(c)?;
        stage("正在获取 Minecraft 玩家档案");
        let r = receive(
            AuthStage::Profile,
            self.http
                .get(&self.endpoints.profile)
                .bearer_auth(&access)
                .send(),
        )?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Err("该微软账户没有可用的 Minecraft Java 玩家档案，请确认已购买 Java 版或启用 Game Pass 并创建档案".into());
        }
        let p = decode(AuthStage::Profile, Ok(r))?;
        let uuid = field(&p, "id")?;
        if uuid.len() != 32 || !uuid.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("Minecraft 玩家 UUID 格式错误".into());
        }
        cancelled(c)?;
        Ok(Session {
            account: AccountSummary {
                id: uuid.clone(),
                uuid,
                name: field(&p, "name")?,
                expires_at: now().saturating_add(expiry),
                textures: textures::from_profile(&p),
            },
            access_token: access,
            refresh_token: refresh,
            xuid,
            client_id: self.client_id.clone(),
        })
    }
}
// Serialization is confined to the Secret Service boundary, never a public DTO.
#[derive(Serialize, Deserialize)]
struct Stored {
    account: AccountSummary,
    access_token: String,
    refresh_token: String,
    xuid: String,
    client_id: String,
}
fn entry(project: &str, id: &str) -> Result<keyring::Entry, String> {
    if project.is_empty() || id.is_empty() {
        return Err("安全存储账户标识为空".into());
    }
    keyring::Entry::new(&format!("PCL-Linux:{project}"), id)
        .map_err(|_| "系统 Secret Service 不可用；账户只能保留在本次会话".into())
}
pub fn save_session(project: &str, s: &Session) -> Result<(), String> {
    let data = Stored {
        account: s.account.clone(),
        access_token: s.access_token.clone(),
        refresh_token: s.refresh_token.clone(),
        xuid: s.xuid.clone(),
        client_id: s.client_id.clone(),
    };
    let bytes = serde_json::to_vec(&data).map_err(|_| "无法编码安全账户凭据")?;
    let entry = entry(project, &s.account.id)?;
    store_and_verify(&entry, &bytes)
}
fn store_and_verify(entry: &keyring::Entry, bytes: &[u8]) -> Result<(), String> {
    entry
        .set_secret(&bytes)
        .map_err(|_| "无法保存登录，请解锁系统密钥环后重试保存".to_string())?;
    // Mark an account remembered only after the OS store confirms the same data.
    // No token is ever returned through IPC or copied to launcher JSON files.
    let stored = entry
        .get_secret()
        .map_err(|_| "登录已写入密钥环，但读取验证失败，请重试保存".to_string())?;
    if stored.as_slice() != bytes {
        return Err("系统密钥环未保留完整登录凭据，请重试保存".into());
    }
    Ok(())
}
pub fn load_session(project: &str, id: &str) -> Result<Option<Session>, String> {
    let bytes = match entry(project, id)?.get_secret() {
        Ok(v) => v,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(_) => return Err("无法读取系统 Secret Service，请解锁系统密钥环".into()),
    };
    let s: Stored =
        serde_json::from_slice(&bytes).map_err(|_| "系统密钥环中的账户凭据损坏，请重新登录")?;
    if s.account.id != id || s.account.uuid != id {
        return Err("安全账户身份不匹配".into());
    }
    Ok(Some(Session {
        account: s.account,
        access_token: s.access_token,
        refresh_token: s.refresh_token,
        xuid: s.xuid,
        client_id: s.client_id,
    }))
}
pub fn delete_session(project: &str, id: &str) -> Result<(), String> {
    match entry(project, id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("无法从系统 Secret Service 删除账户凭据，请解锁系统密钥环".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    #[test]
    fn credential_readback_is_verified_without_using_the_system_keyring() {
        let credential = keyring::mock::MockCredential::default();
        let entry = keyring::Entry::new_with_credential(Box::new(credential));
        store_and_verify(&entry, b"synthetic-credential").unwrap();
        assert_eq!(entry.get_secret().unwrap(), b"synthetic-credential");
    }
    #[test]
    fn credential_store_failure_is_actionable_and_does_not_echo_backend_details() {
        let credential = keyring::mock::MockCredential::default();
        credential.set_error(keyring::Error::PlatformFailure(Box::new(
            std::io::Error::other("private-backend-detail"),
        )));
        let entry = keyring::Entry::new_with_credential(Box::new(credential));
        let error = store_and_verify(&entry, b"synthetic-credential").unwrap_err();
        assert!(error.contains("解锁"));
        assert!(!error.contains("private-backend-detail"));
    }
    fn server(replies: Vec<(&'static str, u16, Value)>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            for (path, status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    request.extend_from_slice(&buf[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&request[..end]);
                        let size = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + size {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.lines().next().unwrap().contains(path));
                if path == "/xbox" {
                    assert!(request.contains("d=ms-secret"));
                }
                if path == "/xsts" {
                    assert!(request.contains("rp://api.minecraftservices.com/"));
                }
                if path == "/entitlements" || path == "/profile" {
                    assert!(request
                        .to_lowercase()
                        .contains("authorization: bearer mc-secret"));
                }
                let b = body.to_string();
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",b.len()).unwrap();
            }
        });
        (url, thread)
    }
    fn client(base: &str) -> MicrosoftClient {
        let mut c = MicrosoftClient::new(ID).unwrap();
        c.http = Client::builder().no_proxy().build().unwrap();
        c.endpoints = Endpoints {
            device: format!("{base}/device"),
            token: format!("{base}/token"),
            xbox: format!("{base}/xbox"),
            xsts: format!("{base}/xsts"),
            minecraft: format!("{base}/mc"),
            entitlements: format!("{base}/entitlements"),
            profile: format!("{base}/profile"),
        };
        c
    }
    fn chain() -> Vec<(&'static str, u16, Value)> {
        vec![
            (
                "/xbox",
                200,
                json!({"Token":"xbox-secret","DisplayClaims":{"xui":[{"uhs":"hash"}]}}),
            ),
            (
                "/xsts",
                200,
                json!({"Token":"xsts-secret","DisplayClaims":{"xui":[{"uhs":"hash","xid":"123"}]}}),
            ),
            (
                "/mc",
                200,
                json!({"access_token":"mc-secret","expires_in":3600}),
            ),
            ("/entitlements", 200, json!({"items":[]})),
            (
                "/profile",
                200,
                json!({"id":"1234567890abcdef1234567890abcdef","name":"Player"}),
            ),
        ]
    }
    #[test]
    fn full_chain_accepts_gamepass_profile_and_redacts_dto() {
        let (base, t) = server(chain());
        let s = client(&base)
            .exchange(
                json!({"access_token":"ms-secret","refresh_token":"refresh-secret"}),
                None,
                &AtomicBool::new(false),
                &|_| {},
            )
            .unwrap();
        let public = serde_json::to_string(&s.public()).unwrap();
        assert!(!public.contains("secret"));
        assert_eq!(s.launch_identity().xuid, "123");
        t.join().unwrap();
    }
    #[test]
    fn refresh_preserves_rotated_or_previous_token() {
        let mut replies = vec![("/token", 200, json!({"access_token":"ms-secret"}))];
        replies.extend(chain());
        let (base, t) = server(replies);
        let old = Session {
            account: AccountSummary {
                id: "1234567890abcdef1234567890abcdef".into(),
                uuid: "1234567890abcdef1234567890abcdef".into(),
                name: "Old".into(),
                expires_at: 0,
                textures: ProfileTextures::default(),
            },
            access_token: "old".into(),
            refresh_token: "refresh-secret".into(),
            xuid: "123".into(),
            client_id: ID.into(),
        };
        let s = client(&base).refresh(&old).unwrap();
        assert_eq!(s.refresh_token, "refresh-secret");
        t.join().unwrap();
    }
    #[test]
    fn device_pending_then_denial_never_leaks_server_body() {
        let (base, t) = server(vec![
            (
                "/device",
                200,
                json!({"device_code":"device-secret","user_code":"ABCD","verification_uri":"https://evil.invalid","expires_in":30,"interval":1}),
            ),
            ("/token", 400, json!({"error":"authorization_pending"})),
            (
                "/token",
                400,
                json!({"error":"authorization_declined","error_description":"refresh-secret"}),
            ),
        ]);
        let e = client(&base)
            .login_device(
                &AtomicBool::new(false),
                |d| {
                    let dto = serde_json::to_string(&d).unwrap();
                    assert!(!dto.contains("device-secret"));
                    assert!(!dto.contains("evil"));
                },
                |_| {},
            )
            .err()
            .unwrap();
        assert_eq!(e, "已拒绝微软登录授权");
        t.join().unwrap();
    }
    #[test]
    fn cancellation_and_errors_are_safe() {
        assert!(MicrosoftClient::new("").is_err());
        assert!(cancelled(&AtomicBool::new(true)).is_err());
        assert!(auth_error(
            AuthStage::Xsts,
            reqwest::StatusCode::BAD_REQUEST,
            &json!({"XErr":2148916238u64,"Message":"secret"})
        )
        .contains("家庭"));
        assert!(!auth_error(
            AuthStage::Xsts,
            reqwest::StatusCode::BAD_REQUEST,
            &json!({"error":"invalid_client","error_description":"secret"})
        )
        .contains("secret"));
    }
    #[test]
    fn slow_down_obeys_device_deadline() {
        let (base, thread) = server(vec![
            (
                "/device",
                200,
                json!({"device_code":"device-secret","user_code":"ABCD","expires_in":2,"interval":1}),
            ),
            ("/token", 400, json!({"error":"slow_down"})),
        ]);
        let error = client(&base)
            .login_device(&AtomicBool::new(false), |_| {}, |_| {})
            .err()
            .unwrap();
        assert!(error.contains("过期"));
        thread.join().unwrap();
    }
    #[test]
    fn missing_java_profile_is_not_an_offline_identity() {
        let mut replies = chain();
        *replies.last_mut().unwrap() = ("/profile", 404, json!({"errorMessage":"secret"}));
        let (base, thread) = server(replies);
        let error = client(&base)
            .exchange(
                json!({"access_token":"ms-secret","refresh_token":"refresh-secret"}),
                None,
                &AtomicBool::new(false),
                &|_| {},
            )
            .err()
            .unwrap();
        assert!(error.contains("Java"));
        assert!(!error.contains("secret"));
        thread.join().unwrap();
    }
    #[test]
    fn xsts_uhs_only_is_valid_and_xuid_is_optional() {
        let mut replies = chain();
        replies[1].2 = json!({"Token":"xsts-secret","DisplayClaims":{"xui":[{"uhs":"hash"}]}});
        let (base, thread) = server(replies);
        let session = client(&base)
            .exchange(
                json!({"access_token":"ms-secret","refresh_token":"refresh-secret"}),
                None,
                &AtomicBool::new(false),
                &|_| {},
            )
            .unwrap();
        assert_eq!(session.launch_identity().xuid, "");
        thread.join().unwrap();
    }
    #[test]
    fn different_xbox_hash_is_rejected_before_minecraft_login() {
        let mut replies = chain();
        replies.truncate(2);
        replies[1].2 = json!({"Token":"xsts-secret","DisplayClaims":{"xui":[{"uhs":"different"}]}});
        let (base, thread) = server(replies);
        assert!(client(&base)
            .exchange(
                json!({"access_token":"ms-secret","refresh_token":"refresh-secret"}),
                None,
                &AtomicBool::new(false),
                &|_| {}
            )
            .err()
            .unwrap()
            .contains("不一致"));
        thread.join().unwrap();
    }
    #[test]
    fn minecraft_forbidden_reports_status_without_guessing_permission_or_body() {
        let mut replies = chain();
        replies.truncate(3);
        replies[2] = ("/mc", 403, json!("secret invalid server body"));
        let (base, thread) = server(replies);
        let error = client(&base)
            .exchange(
                json!({"access_token":"ms-secret","refresh_token":"refresh-secret"}),
                None,
                &AtomicBool::new(false),
                &|_| {},
            )
            .err()
            .unwrap();
        assert!(error.contains("HTTP 403"));
        assert!(!error.contains("尚未获得"));
        assert!(!error.contains("secret"));
        thread.join().unwrap();
    }
}
