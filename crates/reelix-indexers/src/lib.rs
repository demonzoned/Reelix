//! Indexer adapters: Torznab, Newznab, slskd (Soulseek) and YAML definitions, with rate limiting
//! and health.
//!
//! Adapters are built generically from a provider implementation id plus a validated JSON
//! settings object ([`build`]); [`IndexerManager`] fans searches out across instances with
//! per-indexer rate limits, failure back-off and statistics.

pub mod categories;
pub mod definition;
mod error;
#[doc(hidden)]
pub mod fuzz;
pub mod http;
pub mod manager;
pub mod model;
pub mod newznab;
pub mod slskd;
pub mod util;
mod xml;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reelix_core::Protocol;
use reelix_core::provider::ProviderSchema;
use url::Url;

pub use definition::{DefinitionIndexer, DefinitionStore};
pub use error::IndexerError;
pub use http::{AddressGuard, FlareSolverr, HttpOptions, ProxyConfig, ProxyKind};
pub use manager::{IndexerManager, IndexerSearchResult, IndexerStats, IndexerStatus, RateLimit};
pub use model::{
    Category, IndexerCapabilities, IndexerFlag, ReleaseInfo, SearchKind, SearchMode, SearchParam, SearchQuery,
};
pub use newznab::{Flavor, NewznabIndexer};
pub use slskd::SlskdIndexer;

/// A configured indexer instance.
#[async_trait]
pub trait Indexer: Send + Sync {
    fn protocol(&self) -> Protocol;
    /// Whether `url` is on this indexer's own host (or a subdomain of it). User-supplied
    /// download links are only accepted there; unknown implementations own nothing.
    fn owns_url(&self, _url: &Url) -> bool {
        false
    }
    /// Search modes and categories (fetched from the indexer once, then cached).
    async fn capabilities(&self) -> Result<IndexerCapabilities, IndexerError>;
    async fn search(&self, query: &SearchQuery) -> Result<Vec<ReleaseInfo>, IndexerError>;
    /// Latest releases, optionally restricted to Newznab categories.
    async fn rss(&self, categories: &[u32]) -> Result<Vec<ReleaseInfo>, IndexerError>;
    /// Connectivity + credentials check.
    async fn test(&self) -> Result<(), IndexerError>;
    /// Fetches a `.torrent`/`.nzb` with the indexer's auth (cookies, API key).
    /// A redirect to a magnet link surfaces as [`IndexerError::MagnetRedirect`].
    async fn download(&self, url: &str) -> Result<Vec<u8>, IndexerError>;
}

/// Everything adapters need from the host besides their own settings.
#[derive(Clone)]
pub struct BuildContext {
    pub indexer_id: i64,
    pub name: String,
    /// Shared client (used for FlareSolverr); indexers build their own client per instance
    /// so proxies, redirect policy and cookies stay isolated.
    pub http: reqwest::Client,
    pub proxy: Option<ProxyConfig>,
    /// FlareSolverr root URL for Cloudflare-protected definitions.
    pub flaresolverr: Option<Url>,
    /// Allow loopback/private targets (SSRF guard opt-in; most homelab indexers are on the LAN).
    pub allow_private_addresses: bool,
    pub timeout: Duration,
    pub user_agent: String,
    /// Required to build `definition:<id>` implementations.
    pub definitions: Option<Arc<DefinitionStore>>,
}

impl BuildContext {
    /// Defaults: no proxy, private addresses blocked, 30s timeout.
    pub fn new(indexer_id: i64, name: &str) -> Self {
        Self {
            indexer_id,
            name: name.into(),
            http: reqwest::Client::new(),
            proxy: None,
            flaresolverr: None,
            allow_private_addresses: false,
            timeout: Duration::from_secs(30),
            user_agent: format!("Reelix/{}", reelix_core::VERSION),
            definitions: None,
        }
    }

    pub fn guard(&self) -> AddressGuard {
        AddressGuard {
            allow_private: self.allow_private_addresses,
            // The proxy looks the site up, not the local DNS.
            remote_dns: self.proxy.is_some(),
        }
    }

    pub fn http_options(&self) -> HttpOptions {
        HttpOptions {
            proxy: self.proxy.clone(),
            timeout: self.timeout,
            user_agent: self.user_agent.clone(),
            guard: self.guard(),
        }
    }
}

/// Schemas for the built-in protocol implementations (`torznab`, `newznab`, `slskd`).
pub fn schemas() -> Vec<ProviderSchema> {
    vec![
        newznab::schema(Flavor::Torznab),
        newznab::schema(Flavor::Newznab),
        slskd::schema(),
    ]
}

/// Built-in schemas plus one per loaded definition (`definition:<id>`).
pub fn all_schemas(definitions: &DefinitionStore) -> Vec<ProviderSchema> {
    let mut v = schemas();
    v.extend(definitions.schemas());
    v
}

/// Builds an indexer from its implementation id and settings object.
pub fn build(
    implementation: &str,
    settings: &serde_json::Value,
    ctx: &BuildContext,
) -> Result<Box<dyn Indexer>, IndexerError> {
    match implementation {
        "torznab" => Ok(Box::new(NewznabIndexer::from_settings(
            Flavor::Torznab,
            settings,
            ctx,
        )?)),
        "newznab" => Ok(Box::new(NewznabIndexer::from_settings(
            Flavor::Newznab,
            settings,
            ctx,
        )?)),
        slskd::IMPLEMENTATION => Ok(Box::new(SlskdIndexer::from_settings(settings, ctx)?)),
        other => {
            let id = other
                .strip_prefix(definition::IMPLEMENTATION_PREFIX)
                .ok_or_else(|| IndexerError::UnknownImplementation(other.into()))?;
            let def = ctx
                .definitions
                .as_ref()
                .and_then(|d| d.get(id))
                .ok_or_else(|| IndexerError::UnknownImplementation(other.into()))?;
            Ok(Box::new(DefinitionIndexer::from_settings(def, settings, ctx)?))
        }
    }
}
