//! The credential belongs only to this fixed API client. Errors never include
//! response bodies, request/debug output or provider-controlled parse values.
use reqwest::{header, Client};
use serde::de::DeserializeOwned;
use std::{sync::Arc, time::Duration};

const API: &str = "https://api.curseforge.com/v1";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_OPERATION_BYTES: usize = 8 * 1024 * 1024;
const KEY_ENV: &str = "PCL_RH_CURSEFORGE_API_KEY";

pub(super) struct Transport {
    client: Client,
    key: header::HeaderValue,
    #[cfg(test)]
    loopback: Option<String>,
}

pub(super) struct Budget {
    requests: usize,
    bytes: usize,
}

impl Budget {
    pub fn new(requests: usize) -> Self {
        Self {
            requests,
            bytes: MAX_OPERATION_BYTES,
        }
    }
}

fn key_header(value: Result<String, std::env::VarError>) -> Result<header::HeaderValue, String> {
    let value = value.map_err(|_| "CurseForge 服务尚未配置，暂不可用".to_owned())?;
    if value.is_empty() || value.len() > 4096 || !value.bytes().all(|ch| ch.is_ascii_graphic()) {
        return Err("CurseForge 服务配置无效".to_owned());
    }
    let mut key =
        header::HeaderValue::from_str(&value).map_err(|_| "CurseForge 服务配置无效".to_owned())?;
    key.set_sensitive(true);
    Ok(key)
}

fn client(network: &pcl_network::ClientFactory, https_only: bool) -> Result<Client, String> {
    network
        .async_client()
        .user_agent("PCL-RH/0.2.0 (https://github.com/HyacineFengJin/PCL-RH)")
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .map_err(|_| "无法创建 CurseForge 网络客户端".into())
}

impl Transport {
    pub fn new(network: Arc<pcl_network::ClientFactory>) -> Result<Self, String> {
        let key = key_header(std::env::var(KEY_ENV))?;
        Ok(Self {
            client: client(&network, true)?,
            key,
            #[cfg(test)]
            loopback: None,
        })
    }

    #[cfg(test)]
    pub fn loopback(network: Arc<pcl_network::ClientFactory>, address: String) -> Self {
        let parsed = reqwest::Url::parse(&address).unwrap();
        assert_eq!(parsed.scheme(), "http");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.path(), "/v1");
        Self {
            client: client(&network, false).unwrap(),
            key: key_header(Ok("synthetic-test-key".into())).unwrap(),
            loopback: Some(address),
        }
    }

    pub async fn get<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        query: &[(&str, String)],
        budget: &mut Budget,
    ) -> Result<T, String> {
        if budget.requests == 0 {
            return Err("CurseForge 请求次数超过上限".into());
        }
        budget.requests -= 1;
        // All endpoints are assembled from parsed numeric IDs by the service.
        // The production base is never caller-controlled or provider-derived.
        let api = API;
        #[cfg(test)]
        let api = self.loopback.as_deref().unwrap_or(api);
        let mut response = self
            .client
            .get(format!("{api}/{endpoint}"))
            .query(query)
            .header(header::ACCEPT, "application/json")
            .header(header::ACCEPT_ENCODING, "identity")
            .header("x-api-key", self.key.clone())
            .send()
            .await
            .map_err(|_| "CurseForge 网络请求失败".to_owned())?;
        let status = response.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => "CurseForge API Key 无效、权限不足或请求被拒绝".into(),
                404 => "CurseForge 项目或文件不存在".into(),
                429 => "CurseForge 请求频率受限，请稍后再试".into(),
                code => format!("CurseForge 请求失败：HTTP {code}"),
            });
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|value| {
                !value
                    .to_str()
                    .is_ok_and(|value| value.trim().eq_ignore_ascii_case("identity"))
            })
        {
            return Err("CurseForge 返回了不支持的内容编码".into());
        }
        if !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
            })
        {
            return Err("CurseForge 返回的内容不是 JSON".into());
        }
        let max_bytes = MAX_RESPONSE_BYTES.min(budget.bytes);
        if response
            .content_length()
            .is_some_and(|size| size > max_bytes as u64)
        {
            return Err("CurseForge 响应超过大小上限".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "读取 CurseForge 响应失败".to_owned())?
        {
            if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
                return Err("CurseForge 响应超过大小上限".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        budget.bytes -= bytes.len();
        serde_json::from_slice(&bytes).map_err(|_| "CurseForge 返回的数据格式无效".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_malformed_credentials_have_closed_errors() {
        assert!(key_header(Err(std::env::VarError::NotPresent))
            .unwrap_err()
            .contains("CurseForge 服务"));
        for value in ["", "key with spaces", "key\r\nInjected: value", "密钥"] {
            let error = key_header(Ok(value.into())).unwrap_err();
            assert!(error.contains("CurseForge 服务"));
            if !value.is_empty() {
                assert!(!error.contains(value));
            }
        }
        let key = key_header(Ok("synthetic-test-key".into())).unwrap();
        assert!(key.is_sensitive());
        assert!(!format!("{key:?}").contains("synthetic-test-key"));
    }
}
