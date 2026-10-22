//! Credentials stay in the native transport. Never serialize them into URLs,
//! plans, receipts or WebUI data, or install them as shared default headers.
use reqwest::{header::HeaderValue, Client, RequestBuilder, Url};

#[cfg(not(test))]
mod bundled {
    include!(concat!(env!("OUT_DIR"), "/curseforge_credentials.rs"));
}

#[derive(Clone)]
pub(crate) struct Credential(HeaderValue);

impl Credential {
    pub(crate) fn load() -> Result<Self, String> {
        #[cfg(not(test))]
        {
            Self::select(std::env::var("PCL_RH_CURSEFORGE_API_KEY"), bundled::KEY)
        }
        // Fixture transports explicitly inject synthetic credentials. An
        // ordinary test must never use a real environment or bundled key.
        #[cfg(test)]
        {
            Self::select(Err(std::env::VarError::NotPresent), "")
        }
    }

    fn select(
        override_key: Result<String, std::env::VarError>,
        bundled: &str,
    ) -> Result<Self, String> {
        match override_key {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) if !bundled.is_empty() => Self::parse(bundled),
            Err(std::env::VarError::NotPresent) => Err("CurseForge 服务尚未配置，暂不可用".into()),
            Err(_) => Err("CurseForge 服务配置无效".into()),
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        if value.is_empty() || value.len() > 4096 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("CurseForge 服务配置无效".into());
        }
        let mut key = HeaderValue::from_str(value).map_err(|_| "CurseForge 服务配置无效")?;
        key.set_sensitive(true);
        Ok(Self(key))
    }

    pub(crate) fn header(&self) -> HeaderValue {
        self.0.clone()
    }

    pub(super) fn download_request(
        &self,
        client: &Client,
        url: &Url,
    ) -> Result<RequestBuilder, String> {
        super::download_url(url.as_str())?;
        Ok(client.get(url.clone()).header("x-api-key", self.header()))
    }

    #[cfg(test)]
    pub(crate) fn synthetic() -> Self {
        Self::parse("synthetic-test-key").unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_precedence_and_redaction() {
        let bundled = "synthetic-bundled-key";
        let get = |v| Credential::select(v, bundled).unwrap().header();
        assert_eq!(get(Err(std::env::VarError::NotPresent)), bundled);
        assert_eq!(get(Ok("synthetic-override".into())), "synthetic-override");
        for value in ["", "key with spaces", "key\r\nInjected: value", "密钥"] {
            let error = Credential::select(Ok(value.into()), bundled).err().unwrap();
            if !value.is_empty() {
                assert!(!error.contains(value));
            }
        }
        assert!(Credential::select(Err(std::env::VarError::NotPresent), "").is_err());
        let header = Credential::synthetic().header();
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains("synthetic-test-key"));
    }

    #[test]
    fn download_key_is_a_header_only_on_exact_official_https_file_urls() {
        let key = Credential::synthetic();
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for host in [
            "edge.forgecdn.net",
            "mediafilez.forgecdn.net",
            "media.forgecdn.net",
        ] {
            let url = Url::parse(&format!("https://{host}/files/1/2/mod.jar")).unwrap();
            let request = key
                .download_request(&client, &url)
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(request.url(), &url);
            assert_eq!(request.headers()["x-api-key"], "synthetic-test-key");
            assert!(request.headers()["x-api-key"].is_sensitive());
        }
        for raw in [
            "https://edge.forgecdn.net.evil.test/files/1.jar",
            "https://cdn.modrinth.com/files/1.jar",
            "http://edge.forgecdn.net/files/1.jar",
            "https://edge.forgecdn.net:8443/files/1.jar",
            "https://user@edge.forgecdn.net/files/1.jar",
            "https://edge.forgecdn.net/images/1.png",
        ] {
            assert!(key
                .download_request(&client, &Url::parse(raw).unwrap())
                .is_err());
        }
    }
}
