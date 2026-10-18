//! Immutable network policy snapshots shared by authentication, catalogs and
//! installers. Factory construction captures Linux proxy environment once;
//! replacing the process policy affects new clients, never an existing job.
//!
//! Callers retain their user agent, redirect, timeout and authority/hash rules.
//! The desktop must exclude active jobs/authentication before swapping policy
//! and recreate any caller-owned long-lived client after a successful change.
mod doh;
mod download;
mod parallel;
mod policy;
pub use download::{
    download_snapshot, install_download_policy, install_download_snapshot, DownloadPermit,
    DownloadPolicy, DownloadScheduler, DownloadStatus, DOWNLOAD_SLICE_BYTES,
};
pub use parallel::for_each_bounded;
pub use policy::{DnsFallback, DnsPolicy, Policy, ProxyPolicy};
use serde::Serialize;
use std::{
    net::IpAddr,
    sync::{Arc, OnceLock, RwLock},
};

#[derive(Clone, Debug, Serialize)]
pub struct NetworkStatus {
    pub policy: Policy,
    pub system_proxy_configured: bool,
    pub system_proxy_warning: Option<String>,
    pub doh_queries: u64,
    pub doh_cache_hits: u64,
    pub doh_failures: u64,
    pub system_dns_fallbacks: u64,
    pub last_dns_failure: Option<String>,
    /// Proxied HTTP destinations are resolved by the proxy. DoH only replaces
    /// lookups the local client actually performs. Bootstrap reaches that proxy
    /// using system DNS to avoid recursion; direct resolver HTTPS uses fixed IPs.
    pub dns_scope: &'static str,
}

pub struct ClientFactory {
    policy: Policy,
    proxies: policy::ProxySnapshot,
    resolver: Option<Arc<doh::DohResolver>>,
    system_slots: Arc<tokio::sync::Semaphore>,
}
impl ClientFactory {
    pub fn new(policy: Policy) -> Result<Self, String> {
        policy.validate()?;
        let proxies = policy::ProxySnapshot::capture(&policy.proxy)?;
        Self::with_snapshot(policy, proxies)
    }
    fn with_snapshot(policy: Policy, proxies: policy::ProxySnapshot) -> Result<Self, String> {
        let resolver = match policy.dns {
            DnsPolicy::System => None,
            DnsPolicy::Doh { fallback } => {
                Some(Arc::new(doh::DohResolver::new(&proxies, fallback)?))
            }
        };
        Ok(Self {
            policy,
            proxies,
            resolver,
            system_slots: Arc::new(tokio::sync::Semaphore::new(8)),
        })
    }
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
    pub fn uses_proxy(&self) -> bool {
        self.proxies.configured
    }
    /// Returns the exact policy snapshot's A/AAAA addresses for a caller-owned
    /// public-address check. The caller must then pin those addresses on this
    /// same factory's builder and keep normal hostname/TLS validation intact.
    pub async fn resolve_host(&self, hostname: &str) -> Result<Vec<IpAddr>, String> {
        match &self.resolver {
            Some(resolver) => resolver.resolve_host(hostname).await,
            None => doh::system_addresses(hostname, self.system_slots.clone()).await,
        }
    }
    pub fn async_client(&self) -> reqwest::ClientBuilder {
        let builder = self.proxies.async_builder(reqwest::Client::builder());
        match &self.resolver {
            Some(resolver) => builder.dns_resolver(resolver.clone()),
            None => builder,
        }
    }
    pub fn blocking_client(&self) -> reqwest::blocking::ClientBuilder {
        let builder = self
            .proxies
            .blocking_builder(reqwest::blocking::Client::builder());
        match &self.resolver {
            Some(resolver) => builder.dns_resolver(resolver.clone()),
            None => builder,
        }
    }
    pub fn status(&self) -> NetworkStatus {
        let stats = self
            .resolver
            .as_ref()
            .map(|resolver| resolver.status())
            .unwrap_or_default();
        NetworkStatus { policy: self.policy.clone(), system_proxy_configured: matches!(self.policy.proxy, ProxyPolicy::System) && self.proxies.configured,
            system_proxy_warning: self.proxies.warning.clone(), doh_queries: stats.queries,
            doh_cache_hits: stats.cache_hits, doh_failures: stats.failures,
            system_dns_fallbacks: stats.fallbacks, last_dns_failure: stats.last_failure,
            dns_scope: "local lookups; HTTP(S) proxies resolve destination names; proxy bootstrap uses system DNS" }
    }
}

fn global() -> &'static RwLock<Arc<ClientFactory>> {
    static FACTORY: OnceLock<RwLock<Arc<ClientFactory>>> = OnceLock::new();
    FACTORY.get_or_init(|| {
        // Default system policy has no custom URL or TLS construction to fail.
        // Malformed environment entries are retained as redacted status warnings.
        let proxies = policy::ProxySnapshot::capture(&ProxyPolicy::System)
            .unwrap_or_else(policy::ProxySnapshot::with_warning);
        RwLock::new(Arc::new(ClientFactory {
            policy: Policy::default(),
            proxies,
            resolver: None,
            system_slots: Arc::new(tokio::sync::Semaphore::new(8)),
        }))
    })
}
pub fn snapshot() -> Arc<ClientFactory> {
    global()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}
/// Preparation is fallible; the atomic replacement occurs only after the whole
/// new factory is valid. Callers can prepare a factory before persisting prefs.
pub fn install_policy(policy: Policy) -> Result<(), String> {
    install_snapshot(Arc::new(ClientFactory::new(policy)?));
    Ok(())
}
/// Returns the previous immutable factory for coordinated rollback if recreating
/// a long-lived service fails. The caller owns admission around that transition.
pub fn install_snapshot(factory: Arc<ClientFactory>) -> Arc<ClientFactory> {
    std::mem::replace(
        &mut *global()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        factory,
    )
}
pub fn async_client() -> reqwest::ClientBuilder {
    snapshot().async_client()
}
pub fn blocking_client() -> reqwest::blocking::ClientBuilder {
    snapshot().blocking_client()
}
pub fn status() -> NetworkStatus {
    snapshot().status()
}

#[cfg(test)]
mod tests;
