//! Optional, public appearance metadata from the authenticated Minecraft profile.
//! A malformed texture never rejects an otherwise valid login. Navigation and
//! downloads accept only Mojang's content-addressed texture URLs, never a URL
//! supplied by the renderer or an arbitrary profile field.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileTextures {
    pub skin: Option<TextureReference>,
    pub cape: Option<TextureReference>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextureReference {
    pub url: String,
    #[serde(default)]
    pub name: String,
}

/// Legacy profiles used HTTP; upgrade the exact Mojang host to HTTPS.
pub fn official_texture_url(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str() != Some("textures.minecraft.net")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let hash = url.path().strip_prefix("/texture/")?;
    if !(32..=64).contains(&hash.len()) || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("https://textures.minecraft.net/texture/{hash}"))
}

fn active(profile: &Value, key: &str) -> Option<TextureReference> {
    profile[key].as_array()?.iter().take(32).find_map(|item| {
        if item["state"].as_str() != Some("ACTIVE") {
            return None;
        }
        Some(TextureReference {
            url: official_texture_url(item["url"].as_str()?)?,
            name: item["alias"]
                .as_str()
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect(),
        })
    })
}

pub(super) fn from_profile(profile: &Value) -> ProfileTextures {
    ProfileTextures {
        skin: active(profile, "skins"),
        cape: active(profile, "capes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    #[test]
    fn active_official_skin_and_cape_are_public_and_legacy_urls_upgrade() {
        let url = format!("http://textures.minecraft.net/texture/{HASH}");
        let textures = from_profile(&json!({
            "skins": [{"state":"INACTIVE","url":url}, {"state":"ACTIVE","url":url}],
            "capes": [{"state":"ACTIVE","url":url,"alias":"Migration"}],
            "access_token":"private-token"
        }));
        assert!(textures.skin.unwrap().url.starts_with("https://"));
        assert_eq!(textures.cape.unwrap().name, "Migration");
    }
    #[test]
    fn malformed_and_foreign_texture_urls_never_become_download_targets() {
        for url in [
            format!("https://textures.minecraft.net.evil.test/texture/{HASH}"),
            format!("https://user@textures.minecraft.net/texture/{HASH}"),
            format!("https://textures.minecraft.net:8443/texture/{HASH}"),
            format!("https://textures.minecraft.net/texture/{HASH}?token=private"),
            format!("https://textures.minecraft.net/texture/{HASH}#fragment"),
            "file:///etc/passwd".into(),
            "https://textures.minecraft.net/texture/../other".into(),
        ] {
            assert!(official_texture_url(&url).is_none());
        }
        assert!(
            from_profile(&json!({"skins": [{"state":"ACTIVE", "url":"https://evil.test"}]}))
                .skin
                .is_none()
        );
    }
    #[test]
    fn old_public_profile_still_deserializes_without_appearance() {
        let p: crate::AccountSummary =
            serde_json::from_value(json!({"id":"id","uuid":"id","name":"Player","expires_at":0}))
                .unwrap();
        assert!(p.textures.skin.is_none());
    }
}
