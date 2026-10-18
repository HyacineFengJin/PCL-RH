//! Exact mrpack mirror authority shared by preview and manual redirects.
//!
//! A valid declaration may still be unsupported. Only the four explicit
//! domains are standard request authority. Local manifest inputs additionally
//! admit two exact official Forge CDN hosts under /files/. Every redirect uses
//! the captured input policy; no CDN URL is reconstructed.
use super::Result;
use reqwest::Url;
use std::collections::BTreeSet;

pub(super) const MAX_MIRRORS: usize = 16;
const MAX_URL_BYTES: usize = 4096;
const MAX_REDIRECTS: usize = 5;

fn encoded_reference(value: &str) -> Result<()> {
    if value.len() > MAX_URL_BYTES
        || !value.is_ascii()
        || value.bytes().any(|b| b.is_ascii_control() || b == b' ')
    {
        return Err("mrpack 下载 URI 必须是有界且正确编码的 HTTPS 链接".into());
    }
    let raw = value.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            if i + 2 >= raw.len() || !raw[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit) {
                return Err("mrpack 下载 URI 含无效百分号编码".into());
            }
            i += 3;
        } else {
            if !raw[i].is_ascii_alphanumeric() && !b"-._~:/?#[]@!$&'()*+,;=".contains(&raw[i]) {
                return Err("mrpack 下载 URI 含未编码字符".into());
            }
            i += 1;
        }
    }
    Ok(())
}

fn absolute(value: &str) -> Result<Url> {
    encoded_reference(value)?;
    // Reject even empty userinfo and missing // before URL normalization can
    // turn browser-style input into a seemingly ordinary HTTPS authority.
    let authority = value
        .split_once("://")
        .map(|(_, tail)| tail.split(['/', '?', '#']).next().unwrap_or_default())
        .ok_or("mrpack 下载 URI 缺少绝对 HTTPS authority")?;
    let url = Url::parse(value).map_err(|_| "mrpack 下载 URI 无效")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || authority.is_empty()
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("mrpack 下载只允许无凭据、无片段的 HTTPS 链接".into());
    }
    Ok(url)
}

fn allowed(url: &Url) -> bool {
    url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some("cdn.modrinth.com" | "github.com" | "raw.githubusercontent.com" | "gitlab.com")
        )
}

/// Retains the preview distinction between malformed and valid-but-unsupported
/// declarations. No error contains the declaration or its sensitive query.
pub(crate) fn declaration(value: &str) -> Result<Option<Url>> {
    let url = absolute(value)?;
    Ok(allowed(&url).then_some(url))
}

/// Local manifests are explicit file authority, with mandatory SHA1/SHA512.
/// This does not enable CurseForge API ID resolution or its service gate.
pub(super) fn local_declaration(value: &str) -> Result<Option<Url>> {
    let url = absolute(value)?;
    let local = url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some("mediafilez.forgecdn.net" | "edge.forgecdn.net")
        )
        && url.path().starts_with("/files/");
    Ok((allowed(&url) || local).then_some(url))
}

pub(super) struct Chain {
    current: Url,
    visited: BTreeSet<String>,
    redirects: usize,
    local: bool,
}
impl Chain {
    pub(super) fn new(current: Url) -> Self {
        Self {
            visited: BTreeSet::from([current.as_str().to_owned()]),
            current,
            redirects: 0,
            local: false,
        }
    }
    pub(super) fn local(current: Url) -> Self {
        Self {
            local: true,
            ..Self::new(current)
        }
    }
    pub(super) fn current(&self) -> &Url {
        &self.current
    }
    /// The caller drops the old response before requesting this new authority.
    /// Relative Location is resolved against the logical HTTPS URL, never a
    /// fixture transport URL or an implicitly followed reqwest response URL.
    pub(super) fn follow(&mut self, location: &str) -> Result<()> {
        if self.redirects >= MAX_REDIRECTS {
            return Err("mrpack 镜像重定向超过 5 次限制".into());
        }
        encoded_reference(location)?;
        // Validate raw authority before join can remove an empty @ userinfo.
        // A colon before any path/query/fragment delimiter denotes an absolute
        // scheme reference, which must retain an explicit HTTPS authority.
        if location
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|head| head.contains(':'))
        {
            absolute(location)?;
        } else if location.starts_with("//") {
            absolute(&format!("https:{location}"))?;
        }
        let next = self
            .current
            .join(location)
            .map_err(|_| "mrpack 镜像重定向 URI 无效")?;
        let next = if self.local {
            local_declaration(next.as_str())
        } else {
            declaration(next.as_str())
        }?
        .ok_or("mrpack 镜像重定向目标域名、路径或端口不受支持")?;
        if !self.visited.insert(next.as_str().to_owned()) {
            return Err("mrpack 镜像包含循环重定向".into());
        }
        self.current = next;
        self.redirects += 1;
        Ok(())
    }
}

#[cfg(test)]
#[path = "mirror/tests.rs"]
mod tests;
