//! [`ProviderService`]: CRUD, live instance cache, indexer manager, notifications fan-out,
//! metadata clients, tags, remote path mappings and health.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Utc;
use reelix_api_types::providers::{
    DefinitionLoadErrorDto, DefinitionSource, DefinitionsSettingsDto, DefinitionsStatusDto,
    IndexerSettingsDto, IndexerStatusDto, MetadataSettingsDto, PlexPinDto, PlexPinStatusDto, PlexServerDto,
    ProviderDto, ProviderInput, ProviderKind, ProviderSchemaDto, ProviderTestRequest, ProxyDto, TestCheckDto,
    TestResultDto,
};
use reelix_api_types::{HealthCheck, HealthLevel};
use reelix_core::bus::EventBus;
use reelix_core::provider::{ProviderSchema, SECRET_MASK};
use reelix_core::{DomainEvent, MediaKind, Protocol};
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_downloaders::{ClientStatus, DownloadClient};
use reelix_indexers::{
    AddressGuard, DefinitionStore, Indexer, IndexerManager, IndexerSearchResult, IndexerStatus, ProxyConfig,
    ProxyKind, RateLimit, SearchQuery,
};
use reelix_lists::{BaseUrls, ImportList};
use reelix_mediaserver::MediaServer;
use reelix_mediaserver::plextv::PlexTv;
use reelix_metadata::{MetadataConfig, Tmdb, TmdbAuth, TmdbOptions, Tvdb, TvdbOptions, Xem, XemOptions};
use reelix_notify::{NotificationEvent, Notifier};
use serde_json::{Map, Value};

use crate::crypto;
use crate::error::{ProviderError, Result};
use crate::model::{ProviderRow, RemotePathMapping, Tag, apply_mappings};
use crate::repo::{self, WriteProvider};
use crate::schemas;

/// Settings key for [`IndexerSettingsDto`] (proxy password encrypted).
pub const INDEXER_SETTINGS_KEY: &str = "indexers.settings";
/// Settings key for [`MetadataSettingsDto`] (keys/tokens encrypted).
pub const METADATA_SETTINGS_KEY: &str = "metadata.settings";
/// Settings key for [`DefinitionsSettingsDto`] (definition catalog sync).
pub const DEFINITIONS_SETTINGS_KEY: &str = "definitions.settings";
const CLIENT_ID_KEY: &str = "providers.client_id";

/// Tunables and test overrides.
#[derive(Debug, Clone)]
pub struct ProviderOptions {
    /// YAML indexer definitions (`{data_dir}/definitions`, with `upstream/` managed by the
    /// definitions sync and `custom/` for user files); created if missing.
    pub definitions_dir: PathBuf,
    /// Per-indexer search timeout (also used for RSS).
    pub search_timeout: Duration,
    /// Timeout for connection tests and notification delivery.
    pub test_timeout: Duration,
    pub plex_tv_url: Option<String>,
    pub plex_discover_url: Option<String>,
    pub list_base_urls: Option<BaseUrls>,
    pub tmdb_base_url: Option<String>,
    pub tvdb_base_url: Option<String>,
    pub xem_base_url: Option<String>,
}

impl ProviderOptions {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            definitions_dir: data_dir.join("definitions"),
            search_timeout: Duration::from_secs(30),
            test_timeout: Duration::from_secs(30),
            plex_tv_url: None,
            plex_discover_url: None,
            list_base_urls: None,
            tmdb_base_url: None,
            tvdb_base_url: None,
            xem_base_url: None,
        }
    }
}

type Built<T> = Vec<(ProviderRow, Arc<T>)>;

#[derive(Default)]
struct Cache {
    indexers: Built<dyn Indexer>,
    download_clients: Built<dyn DownloadClient>,
    notifiers: Built<dyn Notifier>,
    media_servers: Built<dyn MediaServer>,
    import_lists: Built<dyn ImportList>,
    /// Build / decrypt errors of enabled rows, per kind: id → (name, error).
    errors: HashMap<ProviderKind, HashMap<i64, (String, String)>>,
    /// Number of enabled rows per kind.
    enabled: HashMap<ProviderKind, usize>,
    tags: Vec<Tag>,
    mappings: Vec<RemotePathMapping>,
    /// Plain (decrypted) settings.
    indexer_settings: IndexerSettingsDto,
    metadata_settings: MetadataSettingsDto,
    tmdb: Option<Arc<Tmdb>>,
    tvdb: Option<Arc<Tvdb>>,
    /// Last `client_health` poll per download client.
    client_status: HashMap<i64, std::result::Result<ClientStatus, String>>,
    /// Definition files that failed to load.
    definition_failed: Vec<DefinitionLoadErrorDto>,
    /// (upstream files, custom files, overridden upstream ids, loaded definitions).
    definition_counts: (usize, usize, usize, usize),
    definitions_settings: DefinitionsSettingsDto,
}

/// Owns every configured provider and its live instance.
pub struct ProviderService {
    db: Db,
    secrets: SecretBox,
    events: EventBus,
    http: reqwest::Client,
    /// Client for notifications and import lists while private addresses are blocked: the
    /// SSRF guard is enforced at connect time and on every redirect hop, not just on save.
    guarded_http: reqwest::Client,
    opts: ProviderOptions,
    client_id: String,
    definitions: RwLock<Arc<DefinitionStore>>,
    manager: IndexerManager,
    xem: Arc<Xem>,
    cache: RwLock<Cache>,
    reload_lock: tokio::sync::Mutex<()>,
    sync_lock: tokio::sync::Mutex<()>,
    syncing: std::sync::atomic::AtomicBool,
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|p| p.into_inner())
}

fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|p| p.into_inner())
}

fn validation(msg: impl Into<String>) -> ProviderError {
    ProviderError::Validation(msg.into())
}

fn ok_result(message: Option<String>, checks: Vec<TestCheckDto>) -> TestResultDto {
    let ok = checks.iter().all(|c| c.ok);
    let message = if ok {
        message
    } else {
        checks
            .iter()
            .find(|c| !c.ok)
            .map(|c| format!("{}: {}", c.name, c.message.as_deref().unwrap_or("check failed")))
    };
    TestResultDto { ok, message, checks }
}

fn check(name: &str, r: std::result::Result<(), String>) -> TestCheckDto {
    TestCheckDto {
        name: name.into(),
        ok: r.is_ok(),
        message: r.err(),
    }
}

fn failed(name: &str, msg: impl Into<String>) -> TestResultDto {
    ok_result(None, vec![check(name, Err(msg.into()))])
}

/// Where a provider's settings point: `host:port` of every URL value plus the plain
/// host/port fields. Masked secrets are only reused while this is unchanged.
fn endpoints(settings: &Map<String, Value>) -> BTreeSet<String> {
    settings
        .iter()
        .filter_map(|(k, v)| {
            let raw = match v {
                Value::String(s) => s.trim().to_ascii_lowercase(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            if raw.contains("://")
                && let Ok(u) = url::Url::parse(&raw)
                && let Some(h) = u.host_str()
            {
                return Some(format!("{k}={h}:{}", u.port_or_known_default().unwrap_or(0)));
            }
            matches!(k.as_str(), "host" | "hostname" | "server" | "port").then(|| format!("{k}={raw}"))
        })
        .collect()
}

/// True when `input` keeps a masked (stored) secret while the saved row's address changes.
fn secret_follows_new_host(input: &Value, previous: &ProviderRow, row: &ProviderRow) -> bool {
    let reuses_secret = input
        .as_object()
        .is_some_and(|o| o.values().any(|v| v.as_str() == Some(SECRET_MASK)));
    reuses_secret && endpoints(&previous.settings) != endpoints(&row.settings)
}

/// The definitions catalog is code-like input: HTTPS only (plain HTTP just for loopback mirrors).
fn definitions_source_ok(url: &url::Url) -> bool {
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => match url.host() {
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

/// A client that can only reach public addresses: host names resolving to private, loopback or
/// link-local addresses are refused when connecting (so DNS rebinding can't get round it), and
/// redirects to such literals are not followed. For URLs that come from users.
pub fn public_http_client() -> reqwest::Result<reqwest::Client> {
    let guard = AddressGuard { allow_private: false };
    guard
        .secure(reqwest::Client::builder())
        .user_agent(concat!("Reelix/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .redirect(guard.redirect_policy())
        .build()
}

impl ProviderService {
    /// Loads definitions, settings and every provider; builds live instances.
    pub async fn new(
        db: Db,
        secrets: SecretBox,
        events: EventBus,
        http: reqwest::Client,
        opts: ProviderOptions,
    ) -> Result<Arc<Self>> {
        let client_id = match db.settings().get::<String>(CLIENT_ID_KEY).await? {
            Some(id) => id,
            None => {
                let id = format!("reelix-{:032x}", rand::random::<u128>());
                db.settings().set(CLIENT_ID_KEY, &id).await?;
                id
            }
        };
        let guarded_http =
            public_http_client().map_err(|e| ProviderError::Upstream(format!("http client: {e}")))?;
        let mut xem_opts = XemOptions::default();
        if let Some(u) = &opts.xem_base_url {
            xem_opts.base_url = u.clone();
        }
        let svc = Arc::new(Self {
            xem: Arc::new(Xem::new(http.clone(), xem_opts)),
            db,
            secrets,
            events,
            http,
            guarded_http,
            opts,
            client_id,
            definitions: RwLock::new(Arc::new(DefinitionStore::default())),
            manager: IndexerManager::new(),
            cache: RwLock::new(Cache::default()),
            reload_lock: tokio::sync::Mutex::new(()),
            sync_lock: tokio::sync::Mutex::new(()),
            syncing: std::sync::atomic::AtomicBool::new(false),
        });
        svc.load_definitions().await;
        svc.seal_plaintext_secrets().await?;
        svc.reload_settings().await?;
        svc.reload_tags_and_mappings().await?;
        for kind in ProviderKind::ALL {
            svc.reload_kind(kind, true).await?;
        }
        for (id, status) in repo::load_indexer_status(&svc.db).await? {
            svc.manager.restore_status(id, status);
        }
        Ok(svc)
    }

    /// Loads `upstream/` + `custom/` (custom ids win) off the async runtime and swaps the store.
    async fn load_definitions(&self) {
        let root = self.opts.definitions_dir.clone();
        let loaded = match tokio::task::spawn_blocking(move || crate::definitions::load(&root)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "loading indexer definitions panicked");
                return;
            }
        };
        let count = loaded.store.list().len();
        *write(&self.definitions) = Arc::new(loaded.store);
        let mut cache = write(&self.cache);
        cache.definition_failed = loaded.failed;
        cache.definition_counts = (
            loaded.upstream_count,
            loaded.custom_count,
            loaded.overridden,
            count,
        );
    }

    /// Re-reads the definition folders and rebuilds definition-based indexers (Torznab /
    /// Newznab instances are kept untouched).
    pub async fn reload_definitions(&self) -> Result<()> {
        self.load_definitions().await;
        self.seal_plaintext_secrets().await?;
        self.reload_kind_with(ProviderKind::Indexer, &|row| {
            row.implementation
                .starts_with(reelix_indexers::definition::IMPLEMENTATION_PREFIX)
        })
        .await?;
        self.publish("indexer_definitions", "reloaded", 0);
        Ok(())
    }

    /// Encrypts secret fields still stored in plain text, e.g. definition credentials that
    /// were declared `type: text` before they were treated as secrets. Idempotent.
    async fn seal_plaintext_secrets(&self) -> Result<()> {
        let defs = self.defs();
        for row in repo::list(&self.db, None).await? {
            let Some(found) = schemas::find(row.kind, &row.implementation, &defs) else {
                continue;
            };
            let plain: Vec<&str> = found
                .schema
                .secret_fields()
                .filter(|f| {
                    row.settings
                        .get(*f)
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty() && !s.starts_with(crypto::ENC_PREFIX))
                })
                .collect();
            if plain.is_empty() {
                continue;
            }
            let sealed = crypto::encrypt_fields(&self.secrets, &row.settings, plain.iter().copied())?;
            repo::update_settings(&self.db, row.id, &sealed).await?;
            tracing::info!(
                kind = row.kind.as_str(),
                id = row.id,
                fields = ?plain,
                "encrypted provider secrets stored in plain text"
            );
        }
        Ok(())
    }

    fn defs(&self) -> Arc<DefinitionStore> {
        read(&self.definitions).clone()
    }

    fn publish(&self, entity: &str, action: &str, id: i64) {
        self.events.publish(DomainEvent::EntityChanged {
            entity: entity.into(),
            action: action.into(),
            id,
            user_id: None,
        });
    }

    // ------------------------------------------------------------------
    // Schemas
    // ------------------------------------------------------------------

    /// Every implementation that can be added for `kind` (incl. YAML definitions for indexers).
    pub fn schemas(&self, kind: ProviderKind) -> Vec<ProviderSchemaDto> {
        schemas::catalog(kind, &self.defs())
    }

    pub fn schema(&self, kind: ProviderKind, implementation: &str) -> Option<ProviderSchema> {
        schemas::find(kind, implementation, &self.defs()).map(|s| s.schema)
    }

    // ------------------------------------------------------------------
    // Rows
    // ------------------------------------------------------------------

    /// Decrypts a stored row in place; returns the error message if a secret can't be read.
    fn decrypt(&self, row: &mut ProviderRow) -> Option<String> {
        crypto::decrypt_all(&self.secrets, &mut row.settings)
            .err()
            .map(|e| e.to_string())
    }

    async fn load_rows(&self, kind: ProviderKind) -> Result<Vec<(ProviderRow, Option<String>)>> {
        let mut out = Vec::new();
        for mut row in repo::list(&self.db, Some(kind)).await? {
            let err = self.decrypt(&mut row);
            out.push((row, err));
        }
        Ok(out)
    }

    /// All rows of a kind (plain settings), ordered by priority then name.
    pub async fn list(&self, kind: ProviderKind) -> Result<Vec<ProviderRow>> {
        Ok(self.load_rows(kind).await?.into_iter().map(|(r, _)| r).collect())
    }

    pub async fn get(&self, kind: ProviderKind, id: i64) -> Result<Option<ProviderRow>> {
        let Some(mut row) = repo::get(&self.db, kind, id).await? else {
            return Ok(None);
        };
        self.decrypt(&mut row);
        Ok(Some(row))
    }

    async fn require(&self, kind: ProviderKind, id: i64) -> Result<ProviderRow> {
        self.get(kind, id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("{} {id} not found", kind.label())))
    }

    /// API view of a row: secrets masked, plus protocol, indexer status and build error.
    pub fn to_dto(&self, row: &ProviderRow) -> ProviderDto {
        let defs = self.defs();
        let schema = schemas::find(row.kind, &row.implementation, &defs);
        let settings = match &schema {
            Some(s) => crypto::mask(&row.settings, s.schema.secret_fields()),
            None => {
                // Unknown implementation (e.g. removed definition): mask every string but URLs.
                let keys: Vec<String> = row
                    .settings
                    .iter()
                    .filter(|(k, v)| v.is_string() && !matches!(k.as_str(), "base_url" | "url" | "host"))
                    .map(|(k, _)| k.clone())
                    .collect();
                crypto::mask(&row.settings, keys.iter().map(String::as_str))
            }
        };
        let protocol = match row.kind {
            ProviderKind::Indexer => schemas::indexer_protocol(&row.implementation, &defs),
            ProviderKind::DownloadClient => reelix_downloaders::protocol_of(&row.implementation),
            _ => None,
        };
        let status = (row.kind == ProviderKind::Indexer)
            .then(|| self.manager.status(row.id))
            .flatten()
            .map(|s| IndexerStatusDto {
                failure_count: s.failure_count,
                disabled_until: s.disabled_until.filter(|d| *d > Utc::now()),
                last_failure_reason: s.last_failure_reason,
            });
        let error = read(&self.cache)
            .errors
            .get(&row.kind)
            .and_then(|m| m.get(&row.id))
            .map(|(_, e)| e.clone());
        ProviderDto {
            id: row.id,
            kind: row.kind,
            implementation: row.implementation.clone(),
            implementation_name: schema
                .map(|s| s.schema.name)
                .unwrap_or_else(|| row.implementation.clone()),
            name: row.name.clone(),
            enabled: row.enabled,
            priority: row.priority,
            tags: row.tags.clone(),
            settings: Value::Object(settings),
            protocol,
            status,
            error: if row.enabled { error } else { None },
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }

    /// Validates input against the schema, restores masked secrets from `previous` and
    /// returns the row that would be stored (plain settings).
    async fn prepare(
        &self,
        kind: ProviderKind,
        input: &ProviderInput,
        previous: Option<&ProviderRow>,
    ) -> Result<(ProviderRow, ProviderSchema)> {
        let schema = self.schema(kind, &input.implementation).ok_or_else(|| {
            validation(format!(
                "unknown {} implementation `{}`",
                kind.label().to_lowercase(),
                input.implementation
            ))
        })?;
        let name = input.name.trim();
        if name.is_empty() {
            return Err(validation("name is required"));
        }
        if name.chars().count() > 100 {
            return Err(validation("name is too long (max 100 characters)"));
        }
        if !(0..=100).contains(&input.priority) {
            return Err(validation("priority must be between 0 and 100"));
        }
        let known: Vec<i64> = read(&self.cache).tags.iter().map(|t| t.id).collect();
        if let Some(bad) = input.tags.iter().find(|t| !known.contains(t)) {
            return Err(validation(format!("unknown tag id {bad}")));
        }
        let mut obj = match &input.settings {
            Value::Object(m) => m.clone(),
            Value::Null => Map::new(),
            _ => return Err(validation("settings must be a JSON object")),
        };
        // Only fall back to stored secrets for the same implementation.
        let prev_settings = previous
            .filter(|p| p.implementation == input.implementation)
            .map(|p| &p.settings);
        crypto::unmask(&mut obj, prev_settings);
        if kind == ProviderKind::DownloadClient {
            for f in schemas::ROW_FIELDS {
                obj.remove(f);
            }
        }
        let settings = schema.validate(&Value::Object(obj)).map_err(validation)?;
        let mut tags = input.tags.clone();
        tags.sort_unstable();
        tags.dedup();
        let now = Utc::now();
        let row = ProviderRow {
            id: previous.map_or(0, |p| p.id),
            kind,
            implementation: input.implementation.clone(),
            name: name.to_owned(),
            enabled: input.enabled,
            priority: input.priority,
            tags,
            settings,
            created_at: previous.map_or(now, |p| p.created_at),
            updated_at: now,
        };
        Ok((row, schema))
    }

    /// SSRF pre-flight for notifications and import lists: every http(s) URL in the settings
    /// must resolve to a public address unless `allow_private_addresses` is on.
    async fn check_addresses(&self, row: &ProviderRow) -> Result<()> {
        if !matches!(row.kind, ProviderKind::Notification | ProviderKind::ImportList) {
            return Ok(());
        }
        let guard = AddressGuard {
            allow_private: read(&self.cache).indexer_settings.allow_private_addresses,
        };
        if guard.allow_private {
            return Ok(());
        }
        let hint = |e: reelix_indexers::IndexerError| {
            validation(format!(
                "{e} (enable \"allow private addresses\" in Settings → Indexers to allow LAN targets)"
            ))
        };
        if row.kind == ProviderKind::Notification
            && row.implementation == reelix_notify::email::ID
            && let Some(server) = row.settings.get("server").and_then(Value::as_str)
        {
            let server = server.trim();
            let host = if server.contains(':') && !server.starts_with('[') {
                format!("[{server}]")
            } else {
                server.to_owned()
            };
            let url = url::Url::parse(&format!("http://{host}/"))
                .map_err(|e| validation(format!("SMTP server: {e}")))?;
            guard.check(&url).await.map_err(hint)?;
        }
        for v in row.settings.values() {
            let Some(s) = v.as_str() else { continue };
            let s = s.trim();
            if !(s.starts_with("http://") || s.starts_with("https://")) {
                continue;
            }
            let Ok(url) = url::Url::parse(s) else { continue };
            guard.check(&url).await.map_err(hint)?;
        }
        Ok(())
    }

    async fn write_row(&self, row: &ProviderRow, schema: &ProviderSchema, id: Option<i64>) -> Result<i64> {
        let enc = crypto::encrypt_fields(&self.secrets, &row.settings, schema.secret_fields())?;
        let w = WriteProvider {
            kind: row.kind,
            implementation: &row.implementation,
            name: &row.name,
            enabled: row.enabled,
            priority: row.priority,
            tags: &row.tags,
            settings: &enc,
        };
        match id {
            None => repo::insert(&self.db, &w).await,
            Some(id) => {
                if repo::update(&self.db, id, &w).await? {
                    Ok(id)
                } else {
                    Err(ProviderError::NotFound(format!(
                        "{} {id} not found",
                        row.kind.label()
                    )))
                }
            }
        }
    }

    pub async fn create(&self, kind: ProviderKind, input: &ProviderInput) -> Result<ProviderRow> {
        let (row, schema) = self.prepare(kind, input, None).await?;
        self.check_addresses(&row).await?;
        self.try_build(&row).map_err(validation)?;
        let id = self.write_row(&row, &schema, None).await?;
        self.reload_kind(kind, false).await?;
        self.publish(kind.as_str(), "created", id);
        tracing::info!(kind = kind.as_str(), id, name = %row.name, "provider created");
        if kind == ProviderKind::DownloadClient
            && row.implementation == reelix_downloaders::slskd::IMPLEMENTATION
            && row.setting_bool("add_indexer", true)
            && let Err(e) = self.add_slskd_indexer(&row).await
        {
            tracing::warn!(client = %row.name, error = %e, "could not add the matching slskd indexer");
        }
        self.require(kind, id).await
    }

    /// slskd is both a download client and an indexer: a new slskd client also gets an slskd
    /// indexer with the same address and API key, unless one for that address exists.
    async fn add_slskd_indexer(&self, client: &ProviderRow) -> Result<Option<i64>> {
        let http: reelix_downloaders::common::HttpSettings =
            serde_json::from_value(Value::Object(client.settings.clone()))
                .map_err(|e| validation(e.to_string()))?;
        let base_url = http.base_url().map_err(|e| validation(e.to_string()))?;
        let same = |u: &str| {
            u.trim()
                .trim_end_matches('/')
                .eq_ignore_ascii_case(base_url.trim_end_matches('/'))
        };
        let exists = self.list(ProviderKind::Indexer).await?.into_iter().any(|r| {
            r.implementation == reelix_indexers::slskd::IMPLEMENTATION
                && r.setting_str("base_url").is_some_and(same)
        });
        if exists {
            return Ok(None);
        }
        let input = ProviderInput {
            implementation: reelix_indexers::slskd::IMPLEMENTATION.into(),
            name: client.name.clone(),
            enabled: client.enabled,
            priority: 25,
            tags: client.tags.clone(),
            settings: serde_json::json!({
                "base_url": base_url,
                "api_key": client.settings.get("api_key").cloned().unwrap_or(Value::Null),
            }),
        };
        let kind = ProviderKind::Indexer;
        let (row, schema) = self.prepare(kind, &input, None).await?;
        self.try_build(&row).map_err(validation)?;
        let id = self.write_row(&row, &schema, None).await?;
        self.reload_kind(kind, false).await?;
        self.publish(kind.as_str(), "created", id);
        tracing::info!(id, name = %row.name, "slskd indexer added with its download client");
        Ok(Some(id))
    }

    pub async fn update(&self, kind: ProviderKind, id: i64, input: &ProviderInput) -> Result<ProviderRow> {
        let previous = self.require(kind, id).await?;
        let (row, schema) = self.prepare(kind, input, Some(&previous)).await?;
        // As in `test_input`: a masked secret is only kept for the host it was saved for, so
        // changing the address cannot send the stored password or API key somewhere else.
        if secret_follows_new_host(&input.settings, &previous, &row) {
            return Err(validation(
                "the address changed; re-enter the password / API key for the new host",
            ));
        }
        self.check_addresses(&row).await?;
        self.try_build(&row).map_err(validation)?;
        self.write_row(&row, &schema, Some(id)).await?;
        self.reload_kind(kind, false).await?;
        self.publish(kind.as_str(), "updated", id);
        self.require(kind, id).await
    }

    pub async fn delete(&self, kind: ProviderKind, id: i64) -> Result<bool> {
        let found = repo::delete(&self.db, kind, id).await?;
        if found {
            write(&self.cache).client_status.remove(&id);
            self.reload_kind(kind, false).await?;
            self.publish(kind.as_str(), "deleted", id);
        }
        Ok(found)
    }

    // ------------------------------------------------------------------
    // Building
    // ------------------------------------------------------------------

    fn indexer_ctx(&self, row: &ProviderRow, settings: &IndexerSettingsDto) -> reelix_indexers::BuildContext {
        let mut ctx = reelix_indexers::BuildContext::new(row.id, &row.name);
        ctx.http = self.http.clone();
        ctx.allow_private_addresses = settings.allow_private_addresses;
        ctx.timeout = self.opts.search_timeout;
        ctx.definitions = Some(self.defs());
        ctx.flaresolverr = settings
            .flaresolverr_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .and_then(|s| url::Url::parse(s.trim()).ok());
        ctx.proxy = settings
            .proxy
            .as_ref()
            .filter(|p| !p.host.trim().is_empty())
            .map(|p| ProxyConfig {
                kind: if p.kind.eq_ignore_ascii_case("socks5") {
                    ProxyKind::Socks5
                } else {
                    ProxyKind::Http
                },
                host: p.host.trim().to_owned(),
                port: p.port,
                username: p.username.clone().filter(|u| !u.is_empty()),
                password: p.password.clone().filter(|u| !u.is_empty()),
            });
        ctx
    }

    fn mediaserver_ctx(&self) -> reelix_mediaserver::BuildContext {
        reelix_mediaserver::BuildContext::new(self.http.clone(), self.client_id.clone())
    }

    fn client_settings(&self, row: &ProviderRow) -> Value {
        let mut s = row.settings.clone();
        let cache = read(&self.cache);
        let labels: Vec<Value> = row
            .tags
            .iter()
            .filter_map(|id| cache.tags.iter().find(|t| t.id == *id))
            .map(|t| Value::String(t.label.clone()))
            .collect();
        s.insert("name".into(), Value::String(row.name.clone()));
        s.insert("priority".into(), Value::from(row.priority.clamp(0, 255)));
        s.insert("tags".into(), Value::Array(labels));
        Value::Object(s)
    }

    fn build_indexer(&self, row: &ProviderRow) -> std::result::Result<Arc<dyn Indexer>, String> {
        let settings = read(&self.cache).indexer_settings.clone();
        let ctx = self.indexer_ctx(row, &settings);
        reelix_indexers::build(&row.implementation, &Value::Object(row.settings.clone()), &ctx)
            .map(Arc::from)
            .map_err(|e| e.to_string())
    }

    fn build_client(&self, row: &ProviderRow) -> std::result::Result<Arc<dyn DownloadClient>, String> {
        let ctx = reelix_downloaders::BuildContext {
            http: self.http.clone(),
            instance_id: Some(row.id),
        };
        reelix_downloaders::build(&row.implementation, &self.client_settings(row), &ctx)
            .map(Arc::from)
            .map_err(|e| e.to_string())
    }

    /// HTTP client for notifications and import lists, guarded unless private addresses are
    /// allowed (instances are rebuilt when that setting changes).
    fn outbound_http(&self) -> reqwest::Client {
        if read(&self.cache).indexer_settings.allow_private_addresses {
            self.http.clone()
        } else {
            self.guarded_http.clone()
        }
    }

    fn build_notifier(&self, row: &ProviderRow) -> std::result::Result<Arc<dyn Notifier>, String> {
        let mut ctx = reelix_notify::BuildContext::new(self.outbound_http());
        ctx.timeout = self.opts.test_timeout;
        reelix_notify::build(&row.implementation, &Value::Object(row.settings.clone()), &ctx)
            .map(Arc::from)
            .map_err(|e| e.to_string())
    }

    fn build_media_server(&self, row: &ProviderRow) -> std::result::Result<Arc<dyn MediaServer>, String> {
        reelix_mediaserver::build(
            &row.implementation,
            &Value::Object(row.settings.clone()),
            &self.mediaserver_ctx(),
        )
        .map(Arc::from)
        .map_err(|e| e.to_string())
    }

    fn build_list(&self, row: &ProviderRow) -> std::result::Result<Arc<dyn ImportList>, String> {
        let mut ctx = reelix_lists::BuildContext::new(self.outbound_http());
        if let Some(b) = &self.opts.list_base_urls {
            ctx.base_urls = b.clone();
        }
        reelix_lists::build(&row.implementation, &Value::Object(row.settings.clone()), &ctx)
            .map(Arc::from)
            .map_err(|e| e.to_string())
    }

    /// Builds (and discards) an instance to validate settings.
    fn try_build(&self, row: &ProviderRow) -> std::result::Result<(), String> {
        match row.kind {
            ProviderKind::Indexer => self.build_indexer(row).map(drop),
            ProviderKind::DownloadClient => self.build_client(row).map(drop),
            ProviderKind::Notification => self.build_notifier(row).map(drop),
            ProviderKind::MediaServer => self.build_media_server(row).map(drop),
            ProviderKind::ImportList => self.build_list(row).map(drop),
        }
    }

    fn rate_limit(row: &ProviderRow) -> RateLimit {
        let hours = row
            .setting_u64("query_limit_hours")
            .filter(|h| *h > 0)
            .unwrap_or(24);
        RateLimit {
            query_limit: row
                .setting_u64("query_limit")
                .filter(|n| *n > 0)
                .map(|n| n.min(u64::from(u32::MAX)) as u32),
            period: Duration::from_secs(hours * 3600),
            min_interval: row
                .setting_u64("min_interval_secs")
                .filter(|s| *s > 0)
                .map(Duration::from_secs),
        }
    }

    /// Rebuilds the live instances of one kind from the database. Unchanged indexers keep
    /// their instance (and counters) unless `force`.
    pub async fn reload_kind(&self, kind: ProviderKind, force: bool) -> Result<()> {
        self.reload_kind_with(kind, &|_| force).await
    }

    /// [`Self::reload_kind`] where `force` decides per row whether an unchanged indexer is rebuilt.
    async fn reload_kind_with(
        &self,
        kind: ProviderKind,
        force: &(dyn Fn(&ProviderRow) -> bool + Sync),
    ) -> Result<()> {
        let _guard = self.reload_lock.lock().await;
        let rows = self.load_rows(kind).await?;
        let mut errors = HashMap::new();
        let enabled = rows.iter().filter(|(r, _)| r.enabled).count();
        macro_rules! build_all {
            ($build:ident) => {{
                let mut v = Vec::new();
                for (row, err) in rows.iter().filter(|(r, _)| r.enabled) {
                    if let Some(e) = err {
                        errors.insert(row.id, (row.name.clone(), e.clone()));
                        continue;
                    }
                    match self.$build(row) {
                        Ok(inst) => v.push((row.clone(), inst)),
                        Err(e) => {
                            tracing::warn!(kind = kind.as_str(), id = row.id, error = %e, "cannot build provider");
                            errors.insert(row.id, (row.name.clone(), e));
                        }
                    }
                }
                v
            }};
        }
        match kind {
            ProviderKind::Indexer => {
                let old: HashMap<i64, (ProviderRow, Arc<dyn Indexer>)> = read(&self.cache)
                    .indexers
                    .iter()
                    .map(|(r, i)| (r.id, (r.clone(), i.clone())))
                    .collect();
                let mut built = Vec::new();
                for (row, err) in rows.iter().filter(|(r, _)| r.enabled) {
                    if let Some(e) = err {
                        errors.insert(row.id, (row.name.clone(), e.clone()));
                        continue;
                    }
                    if !force(row)
                        && let Some((old_row, inst)) = old.get(&row.id).filter(|(r, _)| r == row)
                    {
                        built.push((old_row.clone(), inst.clone()));
                        continue;
                    }
                    match self.build_indexer(row) {
                        Ok(inst) => {
                            self.manager
                                .register(row.id, &row.name, inst.clone(), Self::rate_limit(row));
                            built.push((row.clone(), inst));
                        }
                        Err(e) => {
                            tracing::warn!(id = row.id, error = %e, "cannot build indexer");
                            errors.insert(row.id, (row.name.clone(), e));
                        }
                    }
                }
                for id in self.manager.ids() {
                    if !built.iter().any(|(r, _)| r.id == id) {
                        self.manager.unregister(id);
                    }
                }
                write(&self.cache).indexers = built;
            }
            ProviderKind::DownloadClient => {
                let v = build_all!(build_client);
                write(&self.cache).download_clients = v;
            }
            ProviderKind::Notification => {
                let v = build_all!(build_notifier);
                write(&self.cache).notifiers = v;
            }
            ProviderKind::MediaServer => {
                let v = build_all!(build_media_server);
                write(&self.cache).media_servers = v;
            }
            ProviderKind::ImportList => {
                let v = build_all!(build_list);
                write(&self.cache).import_lists = v;
            }
        }
        let mut cache = write(&self.cache);
        cache.errors.insert(kind, errors);
        cache.enabled.insert(kind, enabled);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Live instances
    // ------------------------------------------------------------------

    /// The indexer registry (searches, RSS, grabs, stats). Only enabled, buildable indexers
    /// are registered. Call [`Self::persist_indexer_status`] after using it directly.
    pub fn indexer_manager(&self) -> &IndexerManager {
        &self.manager
    }

    /// Enabled indexers, by priority.
    pub fn indexers(&self) -> Vec<(ProviderRow, Arc<dyn Indexer>)> {
        read(&self.cache).indexers.clone()
    }

    /// Enabled download clients, ordered by priority (lower first) then name.
    pub fn download_clients(&self) -> Vec<(ProviderRow, Arc<dyn DownloadClient>)> {
        read(&self.cache).download_clients.clone()
    }

    /// Enabled download clients of one protocol, by priority.
    pub fn download_clients_for(&self, protocol: Protocol) -> Vec<(ProviderRow, Arc<dyn DownloadClient>)> {
        self.download_clients()
            .into_iter()
            .filter(|(_, c)| c.protocol() == protocol)
            .collect()
    }

    pub fn download_client(&self, id: i64) -> Option<(ProviderRow, Arc<dyn DownloadClient>)> {
        read(&self.cache)
            .download_clients
            .iter()
            .find(|(r, _)| r.id == id)
            .cloned()
    }

    pub fn notifiers(&self) -> Vec<(ProviderRow, Arc<dyn Notifier>)> {
        read(&self.cache).notifiers.clone()
    }

    pub fn media_servers(&self) -> Vec<(ProviderRow, Arc<dyn MediaServer>)> {
        read(&self.cache).media_servers.clone()
    }

    pub fn import_lists(&self) -> Vec<(ProviderRow, Arc<dyn ImportList>)> {
        read(&self.cache).import_lists.clone()
    }

    /// Sends `event` to every enabled notifier subscribed to it. Failures are logged only.
    pub async fn notify(&self, event: &NotificationEvent) {
        let targets: Vec<_> = self
            .notifiers()
            .into_iter()
            .filter(|(_, n)| n.wants(event))
            .collect();
        let timeout = self.opts.test_timeout;
        futures::future::join_all(targets.iter().map(|(row, n)| async move {
            match tokio::time::timeout(timeout, n.notify(event)).await {
                Ok(Ok(())) => tracing::debug!(notifier = %row.name, "notification sent"),
                Ok(Err(e)) => tracing::warn!(notifier = %row.name, error = %e, "notification failed"),
                Err(_) => tracing::warn!(notifier = %row.name, "notification timed out"),
            }
        }))
        .await;
    }

    /// Searches indexers concurrently. `ids = None` uses every enabled indexer with
    /// interactive (or automatic) search enabled. Persists indexer health afterwards.
    pub async fn search(
        &self,
        query: &SearchQuery,
        ids: Option<&[i64]>,
        interactive: bool,
    ) -> Vec<IndexerSearchResult> {
        let flag = if interactive {
            "enable_interactive_search"
        } else {
            "enable_automatic_search"
        };
        let ids: Vec<i64> = self
            .indexers()
            .iter()
            .filter(|(r, _)| match ids {
                Some(ids) => ids.contains(&r.id),
                None => r.setting_bool(flag, true),
            })
            .map(|(r, _)| r.id)
            .collect();
        let results = self.manager.search(&ids, query, self.opts.search_timeout).await;
        self.persist_indexer_status().await;
        results
    }

    /// Writes the manager's back-off state to `indexer_status`.
    pub async fn persist_indexer_status(&self) {
        for s in self.manager.stats() {
            if let Err(e) = repo::save_indexer_status(&self.db, s.indexer_id, &s.status).await {
                tracing::warn!(id = s.indexer_id, error = %e, "cannot persist indexer status");
            }
        }
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    async fn run_test(&self, row: &ProviderRow) -> TestResultDto {
        let timeout = self.opts.test_timeout;
        let timed =
            |name: &'static str| move || failed(name, format!("timed out after {}s", timeout.as_secs()));
        macro_rules! simple {
            ($build:ident, $name:literal) => {
                match self.$build(row) {
                    Err(e) => failed("settings", e),
                    Ok(inst) => match tokio::time::timeout(timeout, inst.test()).await {
                        Ok(Ok(_)) => ok_result(None, vec![check($name, Ok(()))]),
                        Ok(Err(e)) => failed($name, e.to_string()),
                        Err(_) => timed($name)(),
                    },
                }
            };
        }
        match row.kind {
            ProviderKind::Indexer => simple!(build_indexer, "connection"),
            ProviderKind::Notification => simple!(build_notifier, "notification"),
            ProviderKind::ImportList => simple!(build_list, "list"),
            ProviderKind::DownloadClient => match self.build_client(row) {
                Err(e) => failed("settings", e),
                Ok(c) => match tokio::time::timeout(timeout, c.test_report()).await {
                    Ok(report) => ok_result(
                        None,
                        report
                            .checks
                            .into_iter()
                            .map(|c| TestCheckDto {
                                name: c.name,
                                ok: c.ok,
                                message: c.message,
                            })
                            .collect(),
                    ),
                    Err(_) => timed("connection")(),
                },
            },
            ProviderKind::MediaServer => match self.build_media_server(row) {
                Err(e) => failed("settings", e),
                Ok(s) => match tokio::time::timeout(timeout, s.test()).await {
                    Ok(Ok(info)) => ok_result(
                        Some(format!("Connected to {} ({})", info.name, info.version)),
                        vec![check("connection", Ok(()))],
                    ),
                    Ok(Err(e)) => failed("connection", e.to_string()),
                    Err(_) => timed("connection")(),
                },
            },
        }
    }

    /// Tests unsaved settings. With `req.id`, masked secrets come from that saved instance.
    pub async fn test_input(&self, kind: ProviderKind, req: &ProviderTestRequest) -> Result<TestResultDto> {
        let previous = match req.id {
            Some(id) => Some(self.require(kind, id).await?),
            None => None,
        };
        let row = match self.prepare(kind, &req.provider, previous.as_ref()).await {
            Ok((row, _)) => row,
            Err(ProviderError::Validation(m)) => return Ok(failed("settings", m)),
            Err(e) => return Err(e),
        };
        // Stored secrets only go back to the host they were saved for.
        if let Some(prev) = &previous
            && secret_follows_new_host(&req.provider.settings, prev, &row)
        {
            return Ok(failed(
                "settings",
                "the address changed; re-enter the password / API key to test against the new host",
            ));
        }
        if let Err(ProviderError::Validation(m)) = self.check_addresses(&row).await {
            return Ok(failed("address", m));
        }
        Ok(self.run_test(&row).await)
    }

    /// Tests a saved instance. For enabled indexers this also clears/records back-off.
    pub async fn test_saved(&self, kind: ProviderKind, id: i64) -> Result<TestResultDto> {
        let row = self.require(kind, id).await?;
        if kind == ProviderKind::Indexer && self.manager.get(id).is_some() {
            let res = self.manager.test(id, self.opts.test_timeout).await;
            self.persist_indexer_status().await;
            self.publish(kind.as_str(), "updated", id);
            return Ok(match res {
                Ok(()) => ok_result(None, vec![check("connection", Ok(()))]),
                Err(e) => failed("connection", e.to_string()),
            });
        }
        Ok(self.run_test(&row).await)
    }

    /// Categories/labels a download client knows (for category dropdowns).
    pub async fn download_client_categories(&self, id: i64) -> Result<Vec<String>> {
        let row = self.require(ProviderKind::DownloadClient, id).await?;
        let client = match self.download_client(id) {
            Some((_, c)) => c,
            None => self.build_client(&row).map_err(validation)?,
        };
        client
            .categories()
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))
    }

    // ------------------------------------------------------------------
    // Global settings: indexers + metadata
    // ------------------------------------------------------------------

    async fn reload_settings(&self) -> Result<()> {
        let mut idx: IndexerSettingsDto = self
            .db
            .settings()
            .get(INDEXER_SETTINGS_KEY)
            .await?
            .unwrap_or_default();
        if let Some(p) = idx.proxy.as_mut()
            && let Some(pw) = &p.password
        {
            p.password = Some(crypto::open(&self.secrets, pw).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot decrypt indexer proxy password");
                String::new()
            }));
        }
        let mut meta: MetadataSettingsDto = self
            .db
            .settings()
            .get(METADATA_SETTINGS_KEY)
            .await?
            .unwrap_or_default();
        for v in [
            &mut meta.tmdb_api_key,
            &mut meta.tmdb_read_token,
            &mut meta.tvdb_api_key,
            &mut meta.tvdb_pin,
        ] {
            if let Some(s) = v.as_ref() {
                *v = match crypto::open(&self.secrets, s) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot decrypt metadata key");
                        None
                    }
                };
            }
        }
        let defs: DefinitionsSettingsDto = self
            .db
            .settings()
            .get(DEFINITIONS_SETTINGS_KEY)
            .await?
            .unwrap_or_default();
        let (tmdb, tvdb) = self.build_metadata(&meta);
        let mut cache = write(&self.cache);
        cache.definitions_settings = defs;
        cache.indexer_settings = idx;
        cache.metadata_settings = meta;
        cache.tmdb = tmdb.ok().map(Arc::new);
        cache.tvdb = tvdb.ok().flatten().map(Arc::new);
        Ok(())
    }

    /// Indexer-wide settings, proxy password masked.
    pub fn indexer_settings(&self) -> IndexerSettingsDto {
        let mut s = read(&self.cache).indexer_settings.clone();
        if let Some(p) = s.proxy.as_mut() {
            p.password = crypto::mask_opt(&p.password);
        }
        s
    }

    /// Whether loopback/private targets are allowed for indexers, notifications and lists.
    pub fn allow_private_addresses(&self) -> bool {
        read(&self.cache).indexer_settings.allow_private_addresses
    }

    pub async fn set_indexer_settings(&self, mut s: IndexerSettingsDto) -> Result<IndexerSettingsDto> {
        if s.rss_interval_minutes != 0 && !(10..=120).contains(&s.rss_interval_minutes) {
            return Err(validation(
                "RSS interval must be 0 (disabled) or between 10 and 120 minutes",
            ));
        }
        s.flaresolverr_url = s
            .flaresolverr_url
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty());
        if let Some(u) = &s.flaresolverr_url {
            let parsed = url::Url::parse(u).map_err(|e| validation(format!("FlareSolverr URL: {e}")))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(validation("FlareSolverr URL must be http(s)"));
            }
        }
        let previous = read(&self.cache).indexer_settings.clone();
        if let Some(p) = s.proxy.as_mut() {
            if p.host.trim().is_empty() {
                s.proxy = None;
            } else {
                if !matches!(p.kind.to_ascii_lowercase().as_str(), "http" | "socks5") {
                    return Err(validation("proxy kind must be `http` or `socks5`"));
                }
                if p.port == 0 {
                    return Err(validation("proxy port is required"));
                }
                p.kind = p.kind.to_ascii_lowercase();
                p.host = p.host.trim().to_owned();
                p.password = crypto::unmask_opt(
                    p.password.take(),
                    previous.proxy.as_ref().and_then(|o| o.password.as_ref()),
                );
            }
        }
        let mut stored = s.clone();
        if let Some(ProxyDto {
            password: Some(pw), ..
        }) = stored.proxy.as_mut()
            && !pw.is_empty()
        {
            *pw = crypto::seal(&self.secrets, pw)?;
        }
        self.db.settings().set(INDEXER_SETTINGS_KEY, &stored).await?;
        let guard_changed = previous.allow_private_addresses != s.allow_private_addresses;
        write(&self.cache).indexer_settings = s;
        self.reload_kind(ProviderKind::Indexer, true).await?;
        if guard_changed {
            self.reload_kind(ProviderKind::Notification, false).await?;
            self.reload_kind(ProviderKind::ImportList, false).await?;
        }
        self.publish("indexer_settings", "updated", 0);
        Ok(self.indexer_settings())
    }

    #[allow(clippy::type_complexity)]
    fn build_metadata(
        &self,
        m: &MetadataSettingsDto,
    ) -> (
        std::result::Result<Tmdb, reelix_metadata::MetaError>,
        std::result::Result<Option<Tvdb>, reelix_metadata::MetaError>,
    ) {
        let blank = |s: &Option<String>| {
            s.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let tmdb = match (blank(&m.tmdb_read_token), blank(&m.tmdb_api_key)) {
            (Some(t), _) => Ok(TmdbAuth::Bearer(t)),
            (None, Some(k)) => Ok(TmdbAuth::ApiKey(k)),
            (None, None) => Err(reelix_metadata::MetaError::NotConfigured("TMDB")),
        }
        .map(|auth| {
            let mut o = TmdbOptions {
                language: m.language.clone(),
                region: blank(&m.region),
                ..Default::default()
            };
            if let Some(u) = &self.opts.tmdb_base_url {
                o.base_url = u.clone();
            }
            Tmdb::new(self.http.clone(), auth, o)
        });
        let tvdb = Ok(blank(&m.tvdb_api_key).map(|key| {
            let mut o = TvdbOptions {
                language: m.language.clone(),
                ..Default::default()
            };
            if let Some(u) = &self.opts.tvdb_base_url {
                o.base_url = u.clone();
            }
            Tvdb::new(self.http.clone(), key, blank(&m.tvdb_pin), o)
        }));
        (tmdb, tvdb)
    }

    /// Metadata settings with keys masked.
    pub fn metadata_settings(&self) -> MetadataSettingsDto {
        let mut m = read(&self.cache).metadata_settings.clone();
        for v in [
            &mut m.tmdb_api_key,
            &mut m.tmdb_read_token,
            &mut m.tvdb_api_key,
            &mut m.tvdb_pin,
        ] {
            *v = crypto::mask_opt(v).filter(|s| !s.is_empty());
        }
        m
    }

    /// The settings as a [`MetadataConfig`] (plain keys) for code that builds its own clients.
    pub fn metadata_config(&self) -> MetadataConfig {
        let m = read(&self.cache).metadata_settings.clone();
        MetadataConfig {
            tmdb_api_key: m.tmdb_api_key,
            tmdb_read_token: m.tmdb_read_token,
            tvdb_api_key: m.tvdb_api_key,
            language: m.language,
            region: m.region,
        }
    }

    fn merge_metadata(&self, mut m: MetadataSettingsDto) -> Result<MetadataSettingsDto> {
        let prev = read(&self.cache).metadata_settings.clone();
        m.tmdb_api_key = crypto::unmask_opt(m.tmdb_api_key, prev.tmdb_api_key.as_ref());
        m.tmdb_read_token = crypto::unmask_opt(m.tmdb_read_token, prev.tmdb_read_token.as_ref());
        m.tvdb_api_key = crypto::unmask_opt(m.tvdb_api_key, prev.tvdb_api_key.as_ref());
        m.tvdb_pin = crypto::unmask_opt(m.tvdb_pin, prev.tvdb_pin.as_ref());
        for v in [
            &mut m.tmdb_api_key,
            &mut m.tmdb_read_token,
            &mut m.tvdb_api_key,
            &mut m.tvdb_pin,
            &mut m.region,
        ] {
            *v = v.take().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        }
        m.language = m.language.trim().to_owned();
        if m.language.is_empty() {
            m.language = "en-US".into();
        }
        if m.language.len() > 16
            || !m
                .language
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(validation("language must look like `en-US`"));
        }
        if let Some(r) = &m.region
            && (r.len() != 2 || !r.chars().all(|c| c.is_ascii_alphabetic()))
        {
            return Err(validation("region must be a two-letter country code, e.g. US"));
        }
        m.region = m.region.map(|r| r.to_ascii_uppercase());
        Ok(m)
    }

    pub async fn set_metadata_settings(&self, m: MetadataSettingsDto) -> Result<MetadataSettingsDto> {
        let m = self.merge_metadata(m)?;
        let mut stored = m.clone();
        for v in [
            &mut stored.tmdb_api_key,
            &mut stored.tmdb_read_token,
            &mut stored.tvdb_api_key,
            &mut stored.tvdb_pin,
        ] {
            if let Some(s) = v.as_mut() {
                *s = crypto::seal(&self.secrets, s)?;
            }
        }
        self.db.settings().set(METADATA_SETTINGS_KEY, &stored).await?;
        self.reload_settings().await?;
        self.publish("metadata_settings", "updated", 0);
        Ok(self.metadata_settings())
    }

    /// Checks (unsaved) metadata settings against TMDB and, if a key is set, TVDB.
    pub async fn test_metadata(&self, m: MetadataSettingsDto) -> Result<TestResultDto> {
        let m = match self.merge_metadata(m) {
            Ok(m) => m,
            Err(ProviderError::Validation(e)) => return Ok(failed("settings", e)),
            Err(e) => return Err(e),
        };
        let (tmdb, tvdb) = self.build_metadata(&m);
        let timeout = self.opts.test_timeout;
        let mut checks = Vec::new();
        checks.push(match tmdb {
            Err(e) => check("tmdb", Err(e.to_string())),
            Ok(t) => match tokio::time::timeout(timeout, t.genres(MediaKind::Movie)).await {
                Ok(Ok(_)) => check("tmdb", Ok(())),
                Ok(Err(e)) => check("tmdb", Err(e.to_string())),
                Err(_) => check("tmdb", Err("timed out".into())),
            },
        });
        if let Ok(Some(t)) = tvdb {
            checks.push(match tokio::time::timeout(timeout, t.token()).await {
                Ok(Ok(_)) => check("tvdb", Ok(())),
                Ok(Err(e)) => check("tvdb", Err(e.to_string())),
                Err(_) => check("tvdb", Err("timed out".into())),
            });
        }
        Ok(ok_result(None, checks))
    }

    /// The configured TMDB client (shared rate limiter + cache).
    pub fn metadata(&self) -> Result<Arc<Tmdb>> {
        read(&self.cache).tmdb.clone().ok_or_else(|| {
            ProviderError::NotConfigured(
                "TMDB is not configured: add an API key or read access token in Settings → Metadata".into(),
            )
        })
    }

    /// The TVDB client, when an API key is configured.
    pub fn tvdb(&self) -> Option<Arc<Tvdb>> {
        read(&self.cache).tvdb.clone()
    }

    /// TheXEM client (no credentials needed).
    pub fn xem(&self) -> Arc<Xem> {
        self.xem.clone()
    }

    // ------------------------------------------------------------------
    // Tags + remote path mappings
    // ------------------------------------------------------------------

    async fn reload_tags_and_mappings(&self) -> Result<()> {
        let tags = repo::list_tags(&self.db).await?;
        let mappings = repo::list_mappings(&self.db).await?;
        let mut cache = write(&self.cache);
        cache.tags = tags;
        cache.mappings = mappings;
        Ok(())
    }

    pub fn tags(&self) -> Vec<Tag> {
        read(&self.cache).tags.clone()
    }

    pub async fn create_tag(&self, label: &str) -> Result<Tag> {
        let label = label.trim();
        if label.is_empty() {
            return Err(validation("tag label is required"));
        }
        if label.chars().count() > 64 || label.contains(',') {
            return Err(validation(
                "tag labels are at most 64 characters and cannot contain commas",
            ));
        }
        let tag = repo::create_tag(&self.db, label).await?;
        self.reload_tags_and_mappings().await?;
        self.publish("tag", "created", tag.id);
        Ok(tag)
    }

    pub async fn delete_tag(&self, id: i64) -> Result<bool> {
        let found = repo::delete_tag(&self.db, id).await?;
        if found {
            self.reload_tags_and_mappings().await?;
            for kind in ProviderKind::ALL {
                self.reload_kind(kind, false).await?;
            }
            self.publish("tag", "deleted", id);
        }
        Ok(found)
    }

    pub fn remote_path_mappings(&self) -> Vec<RemotePathMapping> {
        read(&self.cache).mappings.clone()
    }

    fn validate_mapping(host: &str, remote: &str, local: &str) -> Result<(String, String, String)> {
        let (host, remote, local) = (host.trim(), remote.trim(), local.trim());
        if host.is_empty() || remote.is_empty() || local.is_empty() {
            return Err(validation("host, remote path and local path are required"));
        }
        if !Path::new(local).is_absolute() {
            return Err(validation("local path must be absolute"));
        }
        Ok((host.to_owned(), remote.to_owned(), local.to_owned()))
    }

    pub async fn create_mapping(&self, host: &str, remote: &str, local: &str) -> Result<RemotePathMapping> {
        let (h, r, l) = Self::validate_mapping(host, remote, local)?;
        let id = repo::create_mapping(&self.db, &h, &r, &l).await?;
        self.reload_tags_and_mappings().await?;
        self.publish("remote_path_mapping", "created", id);
        Ok(RemotePathMapping {
            id,
            host: h,
            remote_path: r,
            local_path: l,
        })
    }

    pub async fn update_mapping(
        &self,
        id: i64,
        host: &str,
        remote: &str,
        local: &str,
    ) -> Result<RemotePathMapping> {
        let (h, r, l) = Self::validate_mapping(host, remote, local)?;
        if !repo::update_mapping(&self.db, id, &h, &r, &l).await? {
            return Err(ProviderError::NotFound(format!(
                "remote path mapping {id} not found"
            )));
        }
        self.reload_tags_and_mappings().await?;
        self.publish("remote_path_mapping", "updated", id);
        Ok(RemotePathMapping {
            id,
            host: h,
            remote_path: r,
            local_path: l,
        })
    }

    pub async fn delete_mapping(&self, id: i64) -> Result<bool> {
        let found = repo::delete_mapping(&self.db, id).await?;
        if found {
            self.reload_tags_and_mappings().await?;
            self.publish("remote_path_mapping", "deleted", id);
        }
        Ok(found)
    }

    /// Translates a download client's path into a local one (unchanged if no mapping applies).
    pub fn map_remote_path(&self, host: &str, remote: &str) -> PathBuf {
        apply_mappings(&read(&self.cache).mappings, host, remote).unwrap_or_else(|| PathBuf::from(remote))
    }

    // ------------------------------------------------------------------
    // Plex PIN flow
    // ------------------------------------------------------------------

    fn plex_tv(&self) -> PlexTv {
        let p = PlexTv::new(&self.mediaserver_ctx());
        match (&self.opts.plex_tv_url, &self.opts.plex_discover_url) {
            (Some(tv), d) => {
                let d = d
                    .clone()
                    .unwrap_or_else(|| reelix_mediaserver::plextv::DISCOVER_URL.into());
                p.with_base_urls(tv, &d)
            }
            _ => p,
        }
    }

    pub async fn plex_create_pin(&self) -> Result<PlexPinDto> {
        let pin = self
            .plex_tv()
            .create_pin(None)
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;
        Ok(PlexPinDto {
            id: pin.id,
            code: pin.code,
            auth_url: pin.auth_url,
        })
    }

    pub async fn plex_check_pin(&self, id: i64) -> Result<PlexPinStatusDto> {
        let plex = self.plex_tv();
        let token = plex
            .check_pin(id)
            .await
            .map_err(|e| ProviderError::Upstream(e.to_string()))?;
        let Some(token) = token else {
            return Ok(PlexPinStatusDto {
                authorized: false,
                token: None,
                servers: Vec::new(),
            });
        };
        let servers = match plex.servers(&token).await {
            Ok(list) => list
                .into_iter()
                .map(|s| {
                    let mut conns = s.connections;
                    conns.sort_by_key(|c| (c.relay, !c.local));
                    PlexServerDto {
                        name: s.name,
                        owned: s.owned,
                        uris: conns.into_iter().map(|c| c.uri).collect(),
                    }
                })
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "plex: cannot list servers");
                Vec::new()
            }
        };
        Ok(PlexPinStatusDto {
            authorized: true,
            token: Some(token),
            servers,
        })
    }

    // ------------------------------------------------------------------
    // Health + scheduled checks
    // ------------------------------------------------------------------

    /// Re-tests every indexer that has failures (ignoring back-off), then persists status.
    pub async fn check_indexers(&self) {
        let failing: Vec<i64> = self
            .manager
            .stats()
            .into_iter()
            .filter(|s| s.status.failure_count > 0 || s.status.disabled_until.is_some())
            .map(|s| s.indexer_id)
            .collect();
        for id in &failing {
            match self.manager.test(*id, self.opts.test_timeout).await {
                Ok(()) => tracing::info!(id, "indexer recovered"),
                Err(e) => tracing::info!(id, error = %e, "indexer still failing"),
            }
            self.publish(ProviderKind::Indexer.as_str(), "updated", *id);
        }
        self.persist_indexer_status().await;
    }

    /// Polls every enabled download client's status (incl. output folders vs. remote path
    /// mappings) and caches the result for [`Self::health`].
    pub async fn check_clients(&self) {
        let clients = self.download_clients();
        let timeout = self.opts.test_timeout;
        let results = futures::future::join_all(clients.iter().map(|(row, c)| async move {
            let r = match tokio::time::timeout(timeout, c.status()).await {
                Ok(Ok(mut status)) => {
                    let host = row.setting_str("host").unwrap_or_default().to_owned();
                    status.check_output_dirs(|remote| Some(self.map_remote_path(&host, remote)));
                    Ok(status)
                }
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("timed out".to_owned()),
            };
            (row.id, r)
        }))
        .await;
        let mut cache = write(&self.cache);
        cache.client_status = results.into_iter().collect();
    }

    /// Provider-related health checks for `/system/health`.
    pub fn health(&self) -> Vec<HealthCheck> {
        let cache = read(&self.cache);
        let mut out = Vec::new();
        let mut push = |source: &str, level: HealthLevel, message: String| {
            out.push(HealthCheck {
                source: source.into(),
                level,
                message,
            })
        };
        let errors = |k: ProviderKind| cache.errors.get(&k).cloned().unwrap_or_default();
        let enabled = |k: ProviderKind| cache.enabled.get(&k).copied().unwrap_or(0);

        // Indexers
        if enabled(ProviderKind::Indexer) == 0 {
            push(
                "indexers",
                HealthLevel::Warning,
                "No indexers are enabled; Reelix cannot find releases".into(),
            );
        } else {
            let now = Utc::now();
            let mut unavailable = 0;
            for (row, _) in &cache.indexers {
                let Some(status) = self.manager.status(row.id) else {
                    continue;
                };
                if let Some(until) = status.disabled_until.filter(|u| *u > now) {
                    unavailable += 1;
                    push(
                        "indexers",
                        HealthLevel::Warning,
                        format!(
                            "Indexer {} is unavailable due to failures until {} ({})",
                            row.name,
                            until.format("%Y-%m-%d %H:%M UTC"),
                            status.last_failure_reason.as_deref().unwrap_or("unknown error")
                        ),
                    );
                }
            }
            let errs = errors(ProviderKind::Indexer);
            for (name, e) in errs.values() {
                push(
                    "indexers",
                    HealthLevel::Error,
                    format!("Indexer {name} is misconfigured: {e}"),
                );
            }
            if !cache.indexers.is_empty() && unavailable == cache.indexers.len() {
                push(
                    "indexers",
                    HealthLevel::Error,
                    "All indexers are unavailable due to failures".into(),
                );
            }
        }
        let upstream_failed = cache
            .definition_failed
            .iter()
            .filter(|f| f.source == DefinitionSource::Upstream)
            .count();
        if upstream_failed > 0 {
            push(
                "indexers",
                HealthLevel::Warning,
                format!(
                    "{upstream_failed} downloaded indexer definitions failed to load (see Settings → Indexers)"
                ),
            );
        }
        for f in cache
            .definition_failed
            .iter()
            .filter(|f| f.source == DefinitionSource::Custom)
        {
            push(
                "indexers",
                HealthLevel::Warning,
                format!("Custom indexer definition {} failed to load: {}", f.file, f.error),
            );
        }
        if let Some(e) = &cache.definitions_settings.last_error {
            push(
                "indexers",
                HealthLevel::Warning,
                format!("Indexer definitions could not be updated: {e}"),
            );
        }

        // Download clients
        if enabled(ProviderKind::DownloadClient) == 0 {
            push(
                "download_clients",
                HealthLevel::Warning,
                "No download clients are enabled; releases cannot be grabbed".into(),
            );
        }
        for (name, e) in errors(ProviderKind::DownloadClient).into_values() {
            push(
                "download_clients",
                HealthLevel::Error,
                format!("Download client {name} is misconfigured: {e}"),
            );
        }
        for (row, _) in &cache.download_clients {
            match cache.client_status.get(&row.id) {
                Some(Err(e)) => push(
                    "download_clients",
                    HealthLevel::Error,
                    format!("Unable to communicate with {}: {e}", row.name),
                ),
                Some(Ok(status)) => {
                    for w in &status.warnings {
                        push(
                            "download_clients",
                            HealthLevel::Warning,
                            format!("{}: {}", row.name, w.message),
                        );
                    }
                }
                None => {}
            }
        }

        // Metadata
        if cache.tmdb.is_none() {
            push(
                "metadata",
                HealthLevel::Warning,
                "TMDB is not configured, so films and series use open data (Wikidata and TVmaze) \
                 without film posters, collections or Discover; add an API key in Settings → Metadata"
                    .into(),
            );
        }

        for (kind, source) in [
            (ProviderKind::Notification, "notifications"),
            (ProviderKind::MediaServer, "media_servers"),
            (ProviderKind::ImportList, "import_lists"),
        ] {
            for (name, e) in errors(kind).into_values() {
                push(
                    source,
                    HealthLevel::Warning,
                    format!("{} {name} is misconfigured: {e}", kind.label()),
                );
            }
        }
        out
    }

    /// Restores a status manually (used by tests and migrations).
    pub fn restore_indexer_status(&self, id: i64, status: IndexerStatus) {
        self.manager.restore_status(id, status);
    }
}

/// Outcome of [`ProviderService::sync_definitions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionsSyncOutcome {
    /// Not due yet (or automatic updates disabled).
    Skipped,
    /// The source answered "not modified".
    UpToDate,
    /// New definitions were installed (number of files).
    Updated(usize),
}

impl ProviderService {
    // ------------------------------------------------------------------
    // Indexer definition catalog sync
    // ------------------------------------------------------------------

    /// Definition sync settings (incl. last sync state).
    pub fn definitions_settings(&self) -> DefinitionsSettingsDto {
        read(&self.cache).definitions_settings.clone()
    }

    async fn store_definitions_settings(&self, s: DefinitionsSettingsDto) -> Result<()> {
        self.db.settings().set(DEFINITIONS_SETTINGS_KEY, &s).await?;
        write(&self.cache).definitions_settings = s;
        Ok(())
    }

    /// Saves the user-editable sync settings; sync state fields are kept from the server.
    pub async fn set_definitions_settings(
        &self,
        s: DefinitionsSettingsDto,
    ) -> Result<DefinitionsSettingsDto> {
        let source_url = s.source_url.trim().to_owned();
        let parsed = url::Url::parse(&source_url).map_err(|e| validation(format!("source URL: {e}")))?;
        if !definitions_source_ok(&parsed) {
            return Err(validation("source URL must be an https URL"));
        }
        let version = s.schema_version.trim().to_owned();
        let valid_version = version.len() >= 2
            && version.len() <= 8
            && version.starts_with('v')
            && version[1..].chars().all(|c| c.is_ascii_digit());
        if !valid_version {
            return Err(validation("schema version must look like `v11`"));
        }
        if !(1..=720).contains(&s.auto_update_hours) {
            return Err(validation("update interval must be between 1 and 720 hours"));
        }
        let _sync = self.sync_lock.lock().await;
        let mut next = self.definitions_settings();
        if next.source_url != source_url || next.schema_version != version {
            // A different catalog: download it fully on the next check.
            next.etag = None;
            next.last_check = None;
            next.commit = None;
        }
        next.enabled = s.enabled;
        next.source_url = source_url;
        next.schema_version = version;
        next.auto_update_hours = s.auto_update_hours;
        self.store_definitions_settings(next.clone()).await?;
        self.publish("definitions_settings", "updated", 0);
        Ok(next)
    }

    /// Catalog status: sync state, counts and load failures.
    pub fn definitions_status(&self) -> DefinitionsStatusDto {
        let cache = read(&self.cache);
        let s = &cache.definitions_settings;
        let (upstream_count, custom_count, overridden, loaded) = cache.definition_counts;
        DefinitionsStatusDto {
            enabled: s.enabled,
            source_url: s.source_url.clone(),
            schema_version: s.schema_version.clone(),
            auto_update_hours: s.auto_update_hours,
            last_sync: s.last_sync,
            last_check: s.last_check,
            last_error: s.last_error.clone(),
            commit: s.commit.clone(),
            upstream_count,
            custom_count,
            overridden,
            loaded,
            failed: cache.definition_failed.clone(),
            custom_dir: self
                .opts
                .definitions_dir
                .join(crate::definitions::CUSTOM_DIR)
                .display()
                .to_string(),
            syncing: self.syncing.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Whether the scheduled task should sync now: automatic updates on and the interval
    /// elapsed, or no upstream definitions installed yet.
    pub fn definitions_sync_due(&self) -> bool {
        let s = self.definitions_settings();
        if !s.enabled {
            return false;
        }
        if crate::definitions::upstream_count(&self.opts.definitions_dir) == 0 {
            return true;
        }
        match s.last_check {
            None => true,
            Some(t) => Utc::now() - t >= chrono::Duration::hours(i64::from(s.auto_update_hours)),
        }
    }

    /// Downloads the catalog (conditional on the stored ETag), installs it atomically and
    /// reloads definition-based indexers. Without `force` it only runs when
    /// [`Self::definitions_sync_due`]. Failures are recorded in `last_error`.
    pub async fn sync_definitions(&self, force: bool) -> Result<DefinitionsSyncOutcome> {
        let _sync = self.sync_lock.lock().await;
        if !force && !self.definitions_sync_due() {
            return Ok(DefinitionsSyncOutcome::Skipped);
        }
        self.syncing.store(true, std::sync::atomic::Ordering::Relaxed);
        self.publish("indexer_definitions", "syncing", 0);
        let res = self.sync_definitions_inner().await;
        self.syncing.store(false, std::sync::atomic::Ordering::Relaxed);
        let mut s = self.definitions_settings();
        match &res {
            Ok(outcome) => {
                s.last_error = None;
                s.last_check = Some(Utc::now());
                if let DefinitionsSyncOutcome::Updated(n) = outcome {
                    s.last_sync = s.last_check;
                    s.upstream_count = *n;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "indexer definitions sync failed");
                s.last_error = Some(e.to_string());
            }
        }
        // `sync_definitions_inner` already stored etag/commit into the cache.
        self.store_definitions_settings(s).await?;
        match res {
            Ok(outcome) => {
                if let DefinitionsSyncOutcome::Updated(n) = &outcome {
                    tracing::info!(files = n, "indexer definitions updated");
                }
                self.publish("indexer_definitions", "synced", 0);
                Ok(outcome)
            }
            Err(e) => {
                self.publish("indexer_definitions", "failed", 0);
                Err(e)
            }
        }
    }

    async fn sync_definitions_inner(&self) -> Result<DefinitionsSyncOutcome> {
        use crate::definitions::{MAX_ARCHIVE_BYTES, install, upstream_count};
        let settings = self.definitions_settings();
        let url = url::Url::parse(&settings.source_url)
            .map_err(|e| validation(format!("definitions source URL: {e}")))?;
        if !definitions_source_ok(&url) {
            return Err(validation("definitions source URL must be https"));
        }
        let root = self.opts.definitions_dir.clone();
        let have_upstream = upstream_count(&root) > 0;
        // A fixed, admin-chosen public URL: intentionally not subject to the indexer SSRF guard.
        let mut req = self.http.get(url).timeout(Duration::from_secs(300)).header(
            reqwest::header::ACCEPT,
            "application/gzip, application/x-gzip, */*",
        );
        if have_upstream && let Some(etag) = &settings.etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let upstream = |e: reqwest::Error| ProviderError::Upstream(format!("download failed: {e}"));
        let mut resp = req.send().await.map_err(upstream)?;
        if !definitions_source_ok(resp.url()) {
            return Err(ProviderError::Upstream(
                "download failed: redirected to a non-https URL".into(),
            ));
        }
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(DefinitionsSyncOutcome::UpToDate);
        }
        if !resp.status().is_success() {
            return Err(ProviderError::Upstream(format!(
                "download failed: HTTP {}",
                resp.status()
            )));
        }
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_ARCHIVE_BYTES as u64)
        {
            return Err(ProviderError::Upstream(format!(
                "archive is larger than {} MB",
                MAX_ARCHIVE_BYTES / (1024 * 1024)
            )));
        }
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(upstream)? {
            if body.len() + chunk.len() > MAX_ARCHIVE_BYTES {
                return Err(ProviderError::Upstream(format!(
                    "archive is larger than {} MB",
                    MAX_ARCHIVE_BYTES / (1024 * 1024)
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let version = settings.schema_version.clone();
        let extracted = tokio::task::spawn_blocking(move || install(&root, &body, &version))
            .await
            .map_err(|e| ProviderError::Upstream(format!("extraction panicked: {e}")))?
            .map_err(ProviderError::Upstream)?;
        {
            let mut cache = write(&self.cache);
            cache.definitions_settings.etag = etag;
            cache.definitions_settings.commit = extracted.commit.clone();
        }
        self.reload_definitions().await?;
        Ok(DefinitionsSyncOutcome::Updated(extracted.files))
    }
}

impl std::fmt::Debug for ProviderService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderService").finish_non_exhaustive()
    }
}
