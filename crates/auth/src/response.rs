//! Bounded authentication responses and closed error messages. Never format a
//! reqwest error or provider body: either may contain a URL, token or identity.
use reqwest::{blocking::Response, StatusCode};
use serde_json::Value;
use std::io::Read;

const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy)]
pub(super) enum AuthStage {
    DeviceCode,
    MicrosoftToken,
    XboxLive,
    Xsts,
    MinecraftLogin,
    Entitlements,
    Profile,
}
impl AuthStage {
    fn label(self) -> &'static str {
        match self {
            Self::DeviceCode => "微软设备登录码",
            Self::MicrosoftToken => "微软登录凭据",
            Self::XboxLive => "Xbox Live 认证",
            Self::Xsts => "Xbox 安全认证（XSTS）",
            Self::MinecraftLogin => "Minecraft 登录",
            Self::Entitlements => "Minecraft 游戏资格",
            Self::Profile => "Minecraft 玩家档案",
        }
    }
    fn message(self, text: &str) -> String {
        format!("{}：{text}", self.label())
    }
    fn transport(self, error: &reqwest::Error) -> String {
        let reason = if error.is_timeout() {
            "请求超时，请检查网络及代理后重试"
        } else if error.is_connect() {
            // reqwest groups DNS, TLS and connection failures here. Do not
            // invent a more specific cause from unstable backend error text.
            "无法建立连接，请检查网络、代理及系统时间"
        } else {
            "请求未完成，请检查网络及代理后重试"
        };
        self.message(reason)
    }
    pub(super) fn http(self, status: StatusCode) -> String {
        let reason = match status.as_u16() {
            401 => "身份验证未通过，请重新登录",
            403 => "服务拒绝访问，持续出现时请联系项目维护者",
            429 => "请求过于频繁，请稍后重试",
            500..=599 => "认证服务暂时不可用，请稍后重试",
            300..=399 => "认证服务返回了未允许的跳转，请检查网络及代理",
            _ => "认证请求未成功，请稍后重试",
        };
        self.message(&format!("{reason}（HTTP {}）", status.as_u16()))
    }
}

pub(super) fn receive(
    stage: AuthStage,
    result: Result<Response, reqwest::Error>,
) -> Result<Response, String> {
    let response = result.map_err(|error| stage.transport(&error))?;
    let status = response.status();
    // Service outages and throttling commonly return HTML. Identify their
    // status directly rather than misreporting a malformed account response.
    if status.is_redirection()
        || status.is_server_error()
        || status == StatusCode::TOO_MANY_REQUESTS
    {
        return Err(stage.http(status));
    }
    Ok(response)
}
pub(super) fn read_json(stage: AuthStage, response: Response) -> Result<Value, String> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(stage.message("认证响应超过大小上限，请稍后重试"));
    }
    let mut bytes = Vec::new();
    response
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| stage.message("读取认证响应失败，请检查网络及代理后重试"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(stage.message("认证响应超过大小上限，请稍后重试"));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        if status.is_success() {
            stage.message("认证响应格式错误，请稍后重试")
        } else {
            stage.http(status)
        }
    })
}
pub(super) fn auth_error(stage: AuthStage, status: StatusCode, value: &Value) -> String {
    match value["XErr"].as_u64() {
        Some(2148916233) => return "请先在 Xbox 官网创建 Xbox 账户".into(),
        Some(2148916235) => return "此地区无法使用 Xbox Live".into(),
        Some(2148916236 | 2148916237) => return "Xbox 账户需要完成年龄验证".into(),
        Some(2148916238) => return "儿童账户需要加入由成人管理的 Microsoft 家庭".into(),
        _ => {}
    }
    match value["error"].as_str().unwrap_or("") {
        "invalid_client" | "unauthorized_client" => {
            "启动器的 Microsoft 登录配置不可用，请联系项目维护者".into()
        }
        "authorization_declined" | "access_denied" => "已拒绝微软登录授权".into(),
        "expired_token" => "设备登录码已过期，请重新登录".into(),
        "invalid_grant" => "微软登录凭据已失效，请重新登录".into(),
        _ => stage.http(status),
    }
}
pub(super) fn decode(
    stage: AuthStage,
    response: Result<Response, reqwest::Error>,
) -> Result<Value, String> {
    let response = receive(stage, response)?;
    let status = response.status();
    let value = read_json(stage, response)?;
    if status.is_success() {
        Ok(value)
    } else {
        Err(auth_error(stage, status, &value))
    }
}

#[cfg(test)]
mod tests;
