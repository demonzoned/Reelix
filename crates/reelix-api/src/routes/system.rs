use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use reelix_api_types::{
    ApiErrorBody, BackgroundWorkDto, EnqueuedJob, HealthCheck, HealthLevel, HealthReport, JobDto,
    SystemStatus, TaskDto,
};
use reelix_core::{Permissions, VERSION};
use reelix_db::DbError;
use serde::Deserialize;

use crate::auth::{CurrentUser, MaybeUser};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Version, platform and runtime information.
#[utoipa::path(get, path = "/api/v1/system/status", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "System status", body = SystemStatus),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
    ))]
pub async fn status(State(state): State<AppState>, user: CurrentUser) -> ApiJson<SystemStatus> {
    let cfg = state.config();
    let uptime = (chrono::Utc::now() - cfg.start_time).num_seconds().max(0) as u64;
    ApiJson(SystemStatus {
        version: VERSION.to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        start_time: cfg.start_time,
        uptime_secs: uptime,
        database: cfg.database.clone(),
        // The server path is only for admins.
        data_dir: user.is_admin().then(|| cfg.data_dir.display().to_string()),
        url_base: cfg.url_base.clone(),
    })
}

/// Health checks. Callers without Manage library (anonymous and request-only users) only get
/// the overall status, since check messages name indexers, clients and paths; HTTP 503 when it
/// is `error`.
#[utoipa::path(get, path = "/api/v1/system/health", tag = "system",
    responses(
        (status = 200, description = "Healthy or degraded", body = HealthReport),
        (status = 503, description = "At least one check failed", body = HealthReport),
    ))]
pub async fn health(State(state): State<AppState>, MaybeUser(user): MaybeUser) -> Response {
    let checks = run_health_checks(&state).await;
    let status = checks.iter().map(|c| c.level).max().unwrap_or(HealthLevel::Ok);
    let code = if status == HealthLevel::Error {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    let detailed = user
        .as_ref()
        .is_some_and(|u| u.require(Permissions::MANAGE_LIBRARY).is_ok());
    let checks = if detailed { checks } else { Vec::new() };
    (code, ApiJson(HealthReport { status, checks })).into_response()
}

async fn run_health_checks(state: &AppState) -> Vec<HealthCheck> {
    let mut checks = Vec::new();
    checks.push(match state.db().ping().await {
        Ok(()) => check("database", HealthLevel::Ok, "Database is reachable".into()),
        Err(e) => {
            tracing::error!(error = %e, "health: database ping failed");
            check(
                "database",
                HealthLevel::Error,
                "Database is unreachable; see the server logs".into(),
            )
        }
    });
    let dir = state.config().data_dir.clone();
    let probe = dir.join(format!(".reelix-write-test-{:016x}", rand::random::<u64>()));
    let writable = tokio::task::spawn_blocking(move || {
        std::fs::write(&probe, b"ok")?;
        std::fs::remove_file(&probe)
    })
    .await;
    checks.push(match writable {
        Ok(Ok(())) => check(
            "data_dir",
            HealthLevel::Ok,
            format!("Data directory {} is writable", dir.display()),
        ),
        Ok(Err(e)) => check(
            "data_dir",
            HealthLevel::Error,
            format!("Data directory {} is not writable: {e}", dir.display()),
        ),
        Err(e) => check(
            "data_dir",
            HealthLevel::Error,
            format!("Data directory check failed: {e}"),
        ),
    });
    if let Some(sources) = state.try_ext::<HealthSources>() {
        for s in &sources.0 {
            checks.extend(s.checks());
        }
    }
    checks
}

/// Extra health checks contributed by domain services (providers, …).
pub trait HealthSource: Send + Sync {
    /// Current findings; should be cheap (use cached state, no network calls).
    fn checks(&self) -> Vec<HealthCheck>;
}

/// Every registered [`HealthSource`]; register once with `AppState::with_extension`.
#[derive(Default)]
pub struct HealthSources(pub Vec<std::sync::Arc<dyn HealthSource>>);

fn check(source: &str, level: HealthLevel, message: String) -> HealthCheck {
    HealthCheck {
        source: source.to_owned(),
        level,
        message,
    }
}

/// Scheduled tasks (admin).
#[utoipa::path(get, path = "/api/v1/system/tasks", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Scheduled tasks", body = Vec<TaskDto>),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn tasks(State(state): State<AppState>, user: CurrentUser) -> ApiResult<ApiJson<Vec<TaskDto>>> {
    user.require(Permissions::ADMIN)?;
    let db = state.db();
    let pace = state.jobs().pace();
    let mut out = Vec::new();
    for t in db.tasks().list().await? {
        let running = db.jobs().is_active(&t.kind).await?;
        out.push(TaskDto {
            kind: t.kind,
            name: t.name,
            interval_secs: t.interval_secs,
            enabled: t.enabled,
            last_run: t.last_run,
            next_run: t.next_run,
            last_duration_ms: t.last_duration_ms,
            last_error: t.last_error,
            running,
            paced_interval_secs: pace.apply(t.interval_secs),
        });
    }
    Ok(ApiJson(out))
}

/// The background work pace (admin).
#[utoipa::path(get, path = "/api/v1/system/background-work", tag = "system",
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Current pace", body = BackgroundWorkDto),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn background_work(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<BackgroundWorkDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(BackgroundWorkDto {
        pace: state.jobs().pace(),
    }))
}

/// Sets the background work pace (admin). Applies without a restart.
#[utoipa::path(put, path = "/api/v1/system/background-work", tag = "system",
    request_body = BackgroundWorkDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Saved", body = BackgroundWorkDto),
        (status = 400, description = "Unknown pace", body = ApiErrorBody),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn set_background_work(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<BackgroundWorkDto>,
) -> ApiResult<ApiJson<BackgroundWorkDto>> {
    user.require(Permissions::ADMIN)?;
    state.jobs().set_pace(req.pace).await?;
    tracing::info!(pace = ?req.pace, user = %user.user.username, "background work pace changed");
    Ok(ApiJson(req))
}

/// Runs a scheduled task now (admin). `job_id` is null if one is already queued or running.
#[utoipa::path(post, path = "/api/v1/system/tasks/{kind}/run", tag = "system",
    params(("kind" = String, Path, description = "Task kind, e.g. `housekeeping`")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Enqueued", body = EnqueuedJob),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
        (status = 404, description = "Unknown task", body = ApiErrorBody),
    ))]
pub async fn run_task(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(kind): ApiPath<String>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    if state.db().tasks().get(&kind).await?.is_none() {
        return Err(ApiError::not_found(format!("unknown task '{kind}'")));
    }
    let job_id = match state.jobs().run_task_now(&kind).await {
        Ok(id) => id,
        Err(DbError::Conflict(_)) => return Err(ApiError::not_found(format!("unknown task '{kind}'"))),
        Err(e) => return Err(e.into()),
    };
    tracing::info!(task = %kind, by = %user.user.username, ?job_id, "task run requested");
    Ok((StatusCode::ACCEPTED, ApiJson(EnqueuedJob { job_id })).into_response())
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JobsQuery {
    /// Maximum number of jobs (1-500, default 50).
    pub limit: Option<i64>,
}

/// Recent jobs, newest first (admin).
#[utoipa::path(get, path = "/api/v1/system/jobs", tag = "system",
    params(JobsQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Recent jobs", body = Vec<JobDto>),
        (status = 401, description = "Not authenticated", body = ApiErrorBody),
        (status = 403, description = "Admin only", body = ApiErrorBody),
    ))]
pub async fn jobs(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<JobsQuery>,
) -> ApiResult<ApiJson<Vec<JobDto>>> {
    user.require(Permissions::ADMIN)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let jobs = state.db().jobs().recent(limit).await?;
    Ok(ApiJson(
        jobs.into_iter()
            .map(|j| JobDto {
                id: j.id,
                kind: j.kind,
                status: j.status,
                attempts: j.attempts,
                created_at: j.created_at,
                started_at: j.started_at,
                finished_at: j.finished_at,
                error: j.error,
            })
            .collect(),
    ))
}
