//! Reelix HTTP API: axum router, authentication, WebSocket event stream and OpenAPI docs.
//!
//! All routes live under `/api/...`; the server nests the router below `url_base`.
// The *arr compatibility JSON (`routes::arr`) nests deeply in `json!`.
#![recursion_limit = "256"]

pub mod auth;
pub mod error;
mod routes;
pub mod state;

use std::sync::OnceLock;

use axum::Router;
use axum::routing::{delete, get, post};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_scalar::{Scalar, Servable};

pub use auth::{AuthMethod, CurrentUser, IpNet, MaybeUser, SecureCookies, SecurityConfig};
pub use error::{ApiError, ApiResult};
pub use routes::integrations::{IMPORT_LIST_SYNC, Integrations, IntegrationsBackend};
pub use routes::maintenance::{LogsQuery, Maintenance, MaintenanceBackend, UploadStream, upload_too_large};
pub use routes::pipeline_activity::ActivityHealth;
pub use routes::setup::require_setup_token;
pub use routes::system::{HealthSource, HealthSources};
pub use routes::ws::visible_to;
pub use state::{ApiConfig, AppState};

/// Builds the API router (`/api/v1/...`, `/api/docs`) with `state` applied.
pub fn router(state: AppState) -> Router {
    use routes::{auth, profiles, setup, system, ws};

    let v1 = Router::new()
        .route("/setup", get(setup::status).post(setup::complete))
        .route("/setup/restore", post(setup::restore))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/apikeys", get(auth::list_keys).post(auth::create_key))
        .route("/auth/apikeys/{id}", delete(auth::delete_key))
        .route("/system/status", get(system::status))
        .route("/system/health", get(system::health))
        .route("/system/tasks", get(system::tasks))
        .route("/system/tasks/{kind}/run", post(system::run_task))
        .route("/system/jobs", get(system::jobs))
        .route("/ws", get(ws::ws))
        .route("/openapi.json", get(openapi_json))
        // Domain modules: one `.merge(<module>::routes())` line each.
        .merge(profiles::routes())
        .merge(routes::library::routes())
        .merge(routes::providers::routes())
        .merge(routes::pipeline_search::routes())
        .merge(routes::pipeline_activity::routes())
        .merge(routes::requests::routes())
        .merge(routes::discover::routes())
        .merge(routes::users::routes())
        .merge(routes::auth_external::routes())
        .merge(routes::integrations::routes())
        .merge(routes::books::routes())
        .merge(routes::music::routes())
        .merge(routes::radio::routes())
        .merge(routes::command::routes())
        .merge(routes::maintenance::routes())
        .merge(routes::migrate::routes())
        .merge(routes::direct::routes());

    // Compatibility endpoints for other apps (Torznab/Newznab aggregator), outside /api/v1.
    let compat = Router::new().merge(routes::torznab::routes());

    Router::new()
        .nest("/api/v1", v1)
        .nest("/api", compat)
        .merge(routes::arr::routes())
        .with_state(state)
        .merge(Scalar::with_url("/api/docs", openapi()))
}

async fn openapi_json() -> axum::Json<utoipa::openapi::OpenApi> {
    static DOC: OnceLock<utoipa::openapi::OpenApi> = OnceLock::new();
    axum::Json(DOC.get_or_init(openapi).clone())
}

/// The OpenAPI document for every route in [`router`].
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut doc = ApiDoc::openapi();
    // Domain modules: one `doc.merge(<module>::Api::openapi())` line each.
    doc.merge(routes::requests::Api::openapi());
    doc.merge(routes::discover::Api::openapi());
    doc.merge(routes::users::Api::openapi());
    doc.merge(routes::auth_external::Api::openapi());
    doc.merge(routes::profiles::Api::openapi());
    doc.merge(routes::library::Api::openapi());
    doc.merge(routes::providers::Api::openapi());
    doc.merge(routes::pipeline_search::Api::openapi());
    doc.merge(routes::pipeline_activity::Api::openapi());
    doc.merge(routes::integrations::Api::openapi());
    doc.merge(routes::books::Api::openapi());
    doc.merge(routes::music::Api::openapi());
    doc.merge(routes::radio::Api::openapi());
    doc.merge(routes::command::Api::openapi());
    doc.merge(routes::maintenance::Api::openapi());
    doc.merge(routes::migrate::Api::openapi());
    doc.merge(routes::direct::Api::openapi());
    doc.info.version = reelix_core::VERSION.to_owned();
    doc
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Reelix API", description = "Reelix media automation server API.", license(name = "GPL-3.0-or-later")),
    paths(
        routes::setup::status,
        routes::setup::complete,
        routes::setup::restore,
        routes::auth::login,
        routes::auth::logout,
        routes::auth::me,
        routes::auth::list_keys,
        routes::auth::create_key,
        routes::auth::delete_key,
        routes::system::status,
        routes::system::health,
        routes::system::tasks,
        routes::system::run_task,
        routes::system::jobs,
        routes::ws::ws,
    ),
    components(schemas(
        reelix_api_types::ApiErrorBody,
        reelix_api_types::SetupStatus,
        reelix_api_types::SetupRequest,
        reelix_api_types::LoginRequest,
        reelix_api_types::UserDto,
        reelix_api_types::ApiKeyDto,
        reelix_api_types::CreateApiKeyRequest,
        reelix_api_types::CreatedApiKey,
        reelix_api_types::SystemStatus,
        reelix_api_types::HealthLevel,
        reelix_api_types::HealthCheck,
        reelix_api_types::HealthReport,
        reelix_api_types::TaskDto,
        reelix_api_types::JobDto,
        reelix_api_types::EnqueuedJob,
    )),
    modifiers(&SecuritySchemes),
    tags(
        (name = "setup", description = "First-run setup"),
        (name = "auth", description = "Login, sessions and API keys"),
        (name = "system", description = "Status, health, tasks, jobs, backups, logs and updates"),
        (name = "events", description = "Live WebSocket event stream"),
    )
)]
struct ApiDoc;

struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new(reelix_api_types::API_KEY_HEADER))),
        );
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(reelix_api_types::SESSION_COOKIE))),
        );
    }
}
