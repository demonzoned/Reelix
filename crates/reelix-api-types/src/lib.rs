//! DTOs for the `/api/v1` REST API and WebSocket.
//!
//! Compiled by both the server (with the `openapi` feature) and the wasm frontend,
//! so the API and UI cannot drift apart.

pub mod books;
pub mod content;
pub mod install;
pub mod integrations;
pub mod library;
pub mod migrate;
pub mod music;
pub mod pipeline;
pub mod profiles;
pub mod providers;
pub mod radio;
pub mod requests;
pub mod system;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use reelix_core::jobs::JobStatus;
pub use reelix_core::{DomainEvent, Permissions};

/// Header that cookie-authenticated, state-changing requests must carry (CSRF guard).
pub const CSRF_HEADER: &str = "x-reelix-request";
/// Header carrying an API key.
pub const API_KEY_HEADER: &str = "x-api-key";
/// Session cookie name.
pub const SESSION_COOKIE: &str = "reelix_session";

/// Error body returned with every non-2xx response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApiErrorBody {
    /// Machine-readable code, e.g. `unauthorized`, `validation`, `conflict`.
    pub error: String,
    /// Human-readable message, safe to show in the UI.
    pub message: String,
}

// ---------- Setup ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SetupStatus {
    /// True until the first admin account exists.
    pub needs_setup: bool,
    /// True when `POST /setup` must carry the one-time `setup_token` printed in the server log
    /// (requests from the server machine itself don't need it).
    #[serde(default)]
    pub setup_token_required: bool,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub email: Option<String>,
    /// One-time token from the server log; see [`SetupStatus::setup_token_required`].
    #[serde(default)]
    pub setup_token: Option<String>,
}

// ---------- Auth ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// Persist the session cookie for 30 days instead of the browser session.
    #[serde(default)]
    pub remember_me: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UserDto {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    /// `local`, `jellyfin`, `emby`, `plex` or `oidc`.
    pub auth_source: String,
    /// Permission bitflags (see `Permissions` in reelix-core).
    #[cfg_attr(feature = "openapi", schema(value_type = u64))]
    pub permissions: Permissions,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApiKeyDto {
    pub id: i64,
    pub name: String,
    /// First characters of the key, for recognising it in the UI.
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateApiKeyRequest {
    pub name: String,
}

/// Returned once on creation; the full key is never retrievable again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreatedApiKey {
    pub api_key: ApiKeyDto,
    pub key: String,
}

// ---------- System ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SystemStatus {
    pub version: String,
    pub os: String,
    pub arch: String,
    pub start_time: DateTime<Utc>,
    pub uptime_secs: u64,
    /// Database engine, e.g. `sqlite`.
    pub database: String,
    /// Only returned to admins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
    pub url_base: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum HealthLevel {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HealthCheck {
    /// Component that produced the check, e.g. `database`.
    pub source: String,
    pub level: HealthLevel,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HealthReport {
    /// Worst level across all checks.
    pub status: HealthLevel,
    /// Individual checks; empty for unauthenticated callers.
    pub checks: Vec<HealthCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskDto {
    pub kind: String,
    pub name: String,
    pub interval_secs: i64,
    pub enabled: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub last_duration_ms: Option<i64>,
    pub last_error: Option<String>,
    /// A job for this task is currently queued or running.
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct JobDto {
    pub id: i64,
    pub kind: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub status: JobStatus,
    pub attempts: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EnqueuedJob {
    /// `None` when an identical job was already queued or running.
    pub job_id: Option<i64>,
}

pub mod direct;

// ---------- Commands ----------

/// `POST /command`: a named command that queues a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommandRequest {
    /// `RefreshRadioShow` (`show_ids`, empty = every show), `RescanRadioLibrary` or
    /// `RadioMissingSearch` (indexer search for every wanted radio episode).
    pub name: String,
    #[serde(default)]
    pub show_ids: Vec<i64>,
}
