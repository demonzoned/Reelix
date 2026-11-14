//! Metadata providers: TMDB (primary), TVDB (optional, anime/absolute numbering),
//! TheXEM scene-numbering mappings, Open Library (books), MusicBrainz (music) and ListenBrainz
//! (music charts and new releases).
//!
//! ```no_run
//! # async fn demo() -> Result<(), reelix_metadata::MetaError> {
//! use reelix_metadata::{MetadataConfig, MetadataProvider};
//! let cfg = MetadataConfig { tmdb_api_key: Some("key".into()), ..Default::default() };
//! let tmdb = cfg.tmdb(reqwest::Client::new())?;
//! let movie = tmdb.movie(603).await?;
//! println!("{} ({:?})", movie.title, movie.year);
//! # Ok(()) }
//! ```

pub mod books;
pub mod http;
pub mod listenbrainz;
pub mod model;
pub mod music;
pub mod musicbrainz;
pub mod open;
pub mod openlibrary;
pub mod tmdb;
pub mod tvdb;
pub mod xem;

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::{Deserialize, Serialize};

pub use books::*;
pub use listenbrainz::{ChartAlbum, ListenBrainz, ListenBrainzOptions, StatsRange};
pub use model::*;
pub use music::*;
pub use musicbrainz::{MusicBrainz, MusicBrainzOptions};
pub use open::{OpenData, OpenDataOptions};
pub use openlibrary::{OpenLibrary, OpenLibraryOptions};
pub use tmdb::{ExternalSource, FindResults, Tmdb, TmdbAuth, TmdbOptions, image_url};
pub use tvdb::{Tvdb, TvdbOptions, TvdbOrdering};
pub use xem::{SceneName, SceneNumber, Xem, XemMapping, XemMappings, XemOptions};

/// Errors from metadata providers.
#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    /// No API key / token was configured for the named provider.
    #[error("{0} is not configured: set an API key")]
    NotConfigured(&'static str),
    #[error("not found")]
    NotFound,
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("rate limited by provider")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("provider returned HTTP {status}: {message}")]
    Status { status: u16, message: String },
    /// Transport error; the URL is stripped so API keys and tokens in it never leak into
    /// messages, health checks or logs.
    #[error("http error: {0}")]
    Http(reqwest::Error),
    #[error("unexpected response: {0}")]
    Decode(String),
    #[error("{0} is not supported by this provider")]
    Unsupported(&'static str),
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
}

impl From<serde_json::Error> for MetaError {
    fn from(e: serde_json::Error) -> Self {
        Self::Decode(e.to_string())
    }
}

/// Metadata source used by library management, requests and discovery.
#[async_trait]
pub trait MetadataProvider: Send + Sync {
    /// Stable implementation id (`tmdb`, `tvdb`).
    fn name(&self) -> &'static str;
    async fn movie(&self, id: TmdbId) -> Result<MovieMetadata, MetaError>;
    /// Series with every season and its episodes.
    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError>;
    async fn search(&self, q: &str, kind: MediaKind) -> Result<Vec<SearchHit>, MetaError>;
    async fn discover(&self, feed: DiscoverFeed, page: u32) -> Result<Page<SearchHit>, MetaError>;
}

/// Global metadata settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetadataConfig {
    /// TMDB v3 API key (query parameter auth).
    pub tmdb_api_key: Option<String>,
    /// TMDB v4 read access token (bearer auth); preferred over the API key.
    pub tmdb_read_token: Option<String>,
    pub tvdb_api_key: Option<String>,
    /// BCP-47-ish language, e.g. `en-US`.
    pub language: String,
    /// ISO 3166-1 region for release dates/certifications, e.g. `US`.
    pub region: Option<String>,
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            tmdb_api_key: None,
            tmdb_read_token: None,
            tvdb_api_key: None,
            language: "en-US".into(),
            region: None,
        }
    }
}

fn blank(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

impl MetadataConfig {
    /// Builds the TMDB client, or [`MetaError::NotConfigured`] without credentials.
    pub fn tmdb(&self, http: reqwest::Client) -> Result<Tmdb, MetaError> {
        let auth = match (blank(&self.tmdb_read_token), blank(&self.tmdb_api_key)) {
            (Some(t), _) => TmdbAuth::Bearer(t),
            (None, Some(k)) => TmdbAuth::ApiKey(k),
            (None, None) => return Err(MetaError::NotConfigured("TMDB")),
        };
        Ok(Tmdb::new(
            http,
            auth,
            TmdbOptions {
                language: self.language.clone(),
                region: self.region.clone(),
                ..Default::default()
            },
        ))
    }

    /// Builds the TVDB client, or [`MetaError::NotConfigured`] without an API key.
    pub fn tvdb(&self, http: reqwest::Client) -> Result<Tvdb, MetaError> {
        let key = blank(&self.tvdb_api_key).ok_or(MetaError::NotConfigured("TVDB"))?;
        Ok(Tvdb::new(
            http,
            key,
            None,
            TvdbOptions {
                language: self.language.clone(),
                ..Default::default()
            },
        ))
    }

    /// TheXEM needs no credentials.
    pub fn xem(&self, http: reqwest::Client) -> Xem {
        Xem::new(http, XemOptions::default())
    }
}

/// Dependencies handed to [`build`].
#[derive(Debug, Clone, Default)]
pub struct BuildContext {
    pub http: reqwest::Client,
}

/// One schema per provider implementation (`tmdb`, `tvdb`).
pub fn schemas() -> Vec<ProviderSchema> {
    let language = || {
        FieldSchema::new("language", "Language", FieldKind::Text)
            .default_value("en-US")
            .help("Language for titles and overviews, e.g. en-US, de-DE")
    };
    vec![
        ProviderSchema {
            implementation: "tmdb".into(),
            name: "TMDB".into(),
            description: Some(
                "The Movie Database. This product uses the TMDB API but is not endorsed or certified by TMDB."
                    .into(),
            ),
            info_url: Some("https://www.themoviedb.org/settings/api".into()),
            fields: vec![
                FieldSchema::new("api_key", "API key (v3)", FieldKind::Secret),
                FieldSchema::new("read_token", "Read access token (v4)", FieldKind::Secret)
                    .help("Used instead of the API key when set"),
                language(),
                FieldSchema::new("region", "Region", FieldKind::Text)
                    .placeholder("US")
                    .help("Country for release dates and certifications"),
                FieldSchema::new("base_url", "API URL", FieldKind::Url)
                    .default_value(tmdb::DEFAULT_BASE_URL)
                    .advanced(),
                FieldSchema::new("image_base_url", "Image URL", FieldKind::Url)
                    .default_value(tmdb::DEFAULT_IMAGE_BASE_URL)
                    .advanced(),
            ],
        },
        ProviderSchema {
            implementation: "tvdb".into(),
            name: "TheTVDB".into(),
            description: Some("Optional; improves anime and absolute/scene episode numbering".into()),
            info_url: Some("https://thetvdb.com/api-information".into()),
            fields: vec![
                FieldSchema::new("api_key", "API key", FieldKind::Secret).required(),
                FieldSchema::new("pin", "Subscriber PIN", FieldKind::Secret)
                    .help("Only for user-supported keys"),
                language(),
                FieldSchema::new("base_url", "API URL", FieldKind::Url)
                    .default_value(tvdb::DEFAULT_BASE_URL)
                    .advanced(),
            ],
        },
    ]
}

/// Builds a provider from a validated settings object.
pub fn build(
    implementation: &str,
    settings: &serde_json::Value,
    ctx: &BuildContext,
) -> Result<Box<dyn MetadataProvider>, MetaError> {
    let schema = schemas()
        .into_iter()
        .find(|s| s.implementation == implementation)
        .ok_or_else(|| MetaError::InvalidSettings(format!("unknown metadata provider `{implementation}`")))?;
    let s = schema.validate(settings).map_err(MetaError::InvalidSettings)?;
    let get = |k: &str| {
        s.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let language = get("language").unwrap_or("en-US").to_string();
    match implementation {
        "tmdb" => {
            let auth = match (get("read_token"), get("api_key")) {
                (Some(t), _) => TmdbAuth::Bearer(t.into()),
                (None, Some(k)) => TmdbAuth::ApiKey(k.into()),
                (None, None) => return Err(MetaError::NotConfigured("TMDB")),
            };
            let mut opts = TmdbOptions {
                language,
                region: get("region").map(String::from),
                ..Default::default()
            };
            if let Some(u) = get("base_url") {
                opts.base_url = u.into();
            }
            if let Some(u) = get("image_base_url") {
                opts.image_base_url = u.into();
            }
            Ok(Box::new(Tmdb::new(ctx.http.clone(), auth, opts)))
        }
        "tvdb" => {
            let key = get("api_key").ok_or(MetaError::NotConfigured("TVDB"))?;
            let mut opts = TvdbOptions {
                language,
                ..Default::default()
            };
            if let Some(u) = get("base_url") {
                opts.base_url = u.into();
            }
            Ok(Box::new(Tvdb::new(
                ctx.http.clone(),
                key.into(),
                get("pin").map(String::from),
                opts,
            )))
        }
        _ => unreachable!("schema lookup covers every implementation"),
    }
}

impl From<reqwest::Error> for MetaError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.without_url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn not_configured_without_keys() {
        let cfg = MetadataConfig::default();
        assert!(matches!(
            cfg.tmdb(reqwest::Client::new()),
            Err(MetaError::NotConfigured("TMDB"))
        ));
        assert!(matches!(
            cfg.tvdb(reqwest::Client::new()),
            Err(MetaError::NotConfigured("TVDB"))
        ));
        let blank = MetadataConfig {
            tmdb_api_key: Some("  ".into()),
            ..Default::default()
        };
        assert!(matches!(
            blank.tmdb(reqwest::Client::new()),
            Err(MetaError::NotConfigured(_))
        ));
        let ctx = BuildContext::default();
        assert!(matches!(
            build("tmdb", &json!({}), &ctx),
            Err(MetaError::NotConfigured("TMDB"))
        ));
        assert!(matches!(
            build("tvdb", &json!({}), &ctx),
            Err(MetaError::InvalidSettings(_))
        ));
        assert!(matches!(
            build("nope", &json!({}), &ctx),
            Err(MetaError::InvalidSettings(_))
        ));
    }

    #[test]
    fn schemas_and_build() {
        let all = schemas();
        assert_eq!(
            all.iter().map(|s| s.implementation.as_str()).collect::<Vec<_>>(),
            ["tmdb", "tvdb"]
        );
        assert_eq!(
            all[0].secret_fields().collect::<Vec<_>>(),
            ["api_key", "read_token"]
        );
        let ctx = BuildContext::default();
        let p = build("tmdb", &json!({"api_key": "abc", "region": "GB"}), &ctx).unwrap();
        assert_eq!(p.name(), "tmdb");
        let p = build("tvdb", &json!({"api_key": "abc"}), &ctx).unwrap();
        assert_eq!(p.name(), "tvdb");
    }

    #[test]
    fn config_defaults_deserialize() {
        let c: MetadataConfig = serde_json::from_value(json!({"tmdb_api_key": "k"})).unwrap();
        assert_eq!(c.language, "en-US");
        assert!(c.tmdb(reqwest::Client::new()).is_ok());
    }
}
