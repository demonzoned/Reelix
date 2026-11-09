use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use reelix_api_types::system::PendingRestoreDto;
use reelix_api_types::{ApiErrorBody, SetupRequest, SetupStatus, UserDto};
use reelix_core::{AuthSource, DomainEvent, Permissions, VERSION};
use reelix_db::NewUser;

use crate::auth::{RequestMeta, hash_password_blocking, random_token, sha256_hex, start_session, user_dto};
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::state::AppState;

/// While no user exists, generates the one-time token `POST /setup` requires from remote
/// clients and announces it in the log and on stdout, so only someone with access to the
/// server can claim the admin account. Call before the state is cloned.
pub async fn require_setup_token(state: AppState) -> ApiResult<AppState> {
    if state.db().users().count().await? > 0 {
        return Ok(state);
    }
    let token = random_token();
    tracing::warn!(
        setup_token = %token,
        "no users yet: open the web UI and enter this setup token to create the admin account \
         (not needed when browsing from the server itself)"
    );
    println!(
        "\n  Reelix first-run setup token: {token}\n  \
         Enter it on the setup page to create the admin account.\n"
    );
    Ok(state.with_setup_token(token))
}

/// Whether `meta` must present the setup token.
///
/// A request from the server itself needs no code, but only when its `Host` is a name for this
/// computer. A page on another site can point its own name at 127.0.0.1 (DNS rebinding) and then
/// send any header it likes from the browser on the server; its `Host` is still its own name, so
/// such a request must give the code like any other. Requests from other computers (including
/// Docker's bridge, a reverse proxy and the host's own LAN address) always need the code, so any
/// `Host` is accepted from them.
fn token_required(state: &AppState, meta: &RequestMeta) -> bool {
    state.inner.setup_token.is_some() && !(meta.is_local() && host_is_this_computer(&meta.headers))
}

/// Whether the `Host` header names this computer: `localhost` or a name under `.localhost`, an IP
/// address (a rebinding page cannot use one), or this computer's own name, which Debian points at
/// 127.0.1.1. No `Host` at all is accepted too: every browser sends one.
fn host_is_this_computer(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST) else {
        return true;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    host_allowed(host, own_hostname().as_deref())
}

/// [`host_is_this_computer`] for one `Host` value, with this computer's name (if known).
fn host_allowed(host: &str, own_name: Option<&str>) -> bool {
    let host = host.trim().trim_end_matches('.');
    // Without the port: "[::1]:7700", "127.0.0.1:7700", "localhost:7700".
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inside, port)) if port.is_empty() || port.starts_with(':') => {
                return inside.parse::<std::net::Ipv6Addr>().is_ok();
            }
            _ => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
            _ => host,
        }
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    if name.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    if name == "localhost" || name.ends_with(".localhost") {
        return true;
    }
    own_name.is_some_and(|own| {
        let own = own.trim().trim_end_matches('.').to_ascii_lowercase();
        !own.is_empty() && name == own
    })
}

/// This computer's name, from the kernel (Linux) or `HOSTNAME`.
fn own_hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
}

/// Whether first-run setup is still required.
#[utoipa::path(get, path = "/api/v1/setup", tag = "setup",
    responses((status = 200, description = "Setup status", body = SetupStatus)))]
pub async fn status(State(state): State<AppState>, meta: RequestMeta) -> ApiResult<ApiJson<SetupStatus>> {
    let needs_setup = state.db().users().count().await? == 0;
    Ok(ApiJson(SetupStatus {
        needs_setup,
        setup_token_required: needs_setup && token_required(&state, &meta),
        version: VERSION.to_owned(),
    }))
}

/// Creates the first admin account and logs it in. Only allowed while no users exist.
#[utoipa::path(post, path = "/api/v1/setup", tag = "setup", request_body = SetupRequest,
    responses(
        (status = 201, description = "Admin created; session cookie set", body = UserDto),
        (status = 400, description = "Invalid input", body = ApiErrorBody),
        (status = 403, description = "Missing or wrong setup token", body = ApiErrorBody),
        (status = 409, description = "Setup already completed", body = ApiErrorBody),
    ))]
pub async fn complete(
    State(state): State<AppState>,
    meta: RequestMeta,
    ApiJson(req): ApiJson<SetupRequest>,
) -> ApiResult<Response> {
    let username = req.username.trim().to_owned();
    validate_username(&username)?;
    validate_password(&req.password)?;
    let email = req.email.map(|e| e.trim().to_owned()).filter(|e| !e.is_empty());
    if let Some(e) = &email {
        validate_email(e)?;
    }

    let _guard = state.inner.setup_lock.lock().await;
    if state.db().users().count().await? > 0 {
        return Err(ApiError::Conflict("setup has already been completed".into()));
    }
    if token_required(&state, &meta) && !token_matches(&state, req.setup_token.as_deref()) {
        return Err(wrong_token(&meta));
    }
    let password_hash = hash_password_blocking(req.password).await?;
    let user = state
        .db()
        .users()
        .create(NewUser {
            username,
            email,
            display_name: None,
            password_hash: Some(password_hash),
            auth_source: AuthSource::Local,
            permissions: Permissions::ROLE_ADMIN,
        })
        .await?;
    let cookie = start_session(&state, &meta, user.id, false).await?;
    tracing::info!(user = %user.username, "first-run setup completed");
    state.events().publish(DomainEvent::UserCreated {
        user_id: user.id,
        username: user.username.clone(),
    });
    state
        .events()
        .publish(DomainEvent::SetupCompleted { user_id: user.id });
    Ok((
        StatusCode::CREATED,
        [(header::SET_COOKIE, cookie)],
        ApiJson(user_dto(&user)),
    )
        .into_response())
}

/// Header carrying the set-up code on `POST /setup/restore`, whose body is the backup itself.
pub const SETUP_TOKEN_HEADER: &str = "x-reelix-setup-token";

/// Whether `given` is the set-up code this server printed.
fn token_matches(state: &AppState, given: Option<&str>) -> bool {
    let expected = state.inner.setup_token.as_deref().map(sha256_hex);
    let given = given.map(|t| sha256_hex(t.trim()));
    given.is_some() && given == expected
}

fn wrong_token(meta: &RequestMeta) -> ApiError {
    if meta.is_local() && !host_is_this_computer(&meta.headers) {
        tracing::warn!(
            ip = ?meta.ip,
            host = ?meta.headers.get(header::HOST),
            "first-run setup refused: the address is not a name for this computer"
        );
        return ApiError::Validation {
            status: StatusCode::FORBIDDEN,
            message: "open Reelix at http://localhost to set it up from this computer, or enter the \
                      setup token printed in the server log"
                .into(),
        };
    }
    tracing::warn!(ip = ?meta.ip, "first-run setup attempted without a valid setup token");
    ApiError::Validation {
        status: StatusCode::FORBIDDEN,
        message: "a valid setup token is required; it is printed in the server log".into(),
    }
}

/// Stages a Reelix backup (the raw zip as the body) during first-run setup, before any account
/// exists: the backup brings its own accounts. It is applied on the next restart. Remote clients
/// send the set-up code in the `x-reelix-setup-token` header, as `POST /setup` needs it.
#[utoipa::path(post, path = "/api/v1/setup/restore", tag = "setup",
    request_body(content = Vec<u8>, content_type = "application/zip"),
    params(("x-reelix-setup-token" = Option<String>, Header, description = "The set-up code; needed from other computers")),
    responses(
        (status = 202, description = "Restore staged; restart Reelix to apply it", body = PendingRestoreDto),
        (status = 400, description = "Not a valid Reelix backup", body = ApiErrorBody),
        (status = 403, description = "Missing or wrong setup token", body = ApiErrorBody),
        (status = 409, description = "Setup already completed", body = ApiErrorBody),
        (status = 413, description = "The archive is larger than 2 GiB", body = ApiErrorBody),
    ))]
pub async fn restore(
    State(state): State<AppState>,
    meta: RequestMeta,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Response> {
    // Reelix's own wizards send the CSRF header. Another site cannot add a custom header without
    // the browser asking first, so a page open in a browser on the server cannot post a backup
    // here, even though requests from the server itself need no code.
    if !headers.contains_key(reelix_api_types::CSRF_HEADER) {
        return Err(ApiError::Validation {
            status: StatusCode::FORBIDDEN,
            message: format!("the {} header is required", reelix_api_types::CSRF_HEADER),
        });
    }
    // These checks come before any of the body is read, so nobody can make the server take in
    // gigabytes without the code.
    if state.db().users().count().await? > 0 {
        return Err(ApiError::Conflict("setup has already been completed".into()));
    }
    if token_required(&state, &meta) {
        let given = headers.get(SETUP_TOKEN_HEADER).and_then(|v| v.to_str().ok());
        if !token_matches(&state, given) {
            return Err(wrong_token(&meta));
        }
    }
    let _guard = state.inner.setup_lock.lock().await;
    // An admin may have been created while this request waited for the lock.
    if state.db().users().count().await? > 0 {
        return Err(ApiError::Conflict("setup has already been completed".into()));
    }
    let pending = crate::routes::maintenance::stage_upload(&state, &headers, body).await?;
    tracing::warn!(ip = ?meta.ip, "restore staged from first-run setup; it is applied on the next restart");
    Ok((StatusCode::ACCEPTED, ApiJson(pending)).into_response())
}

pub(crate) fn validate_username(name: &str) -> ApiResult<()> {
    let ok_chars = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if name.is_empty() || name.len() > 64 || !ok_chars {
        return Err(ApiError::validation(
            "username must be 1-64 characters of A-Z, a-z, 0-9, '.', '_' or '-'",
        ));
    }
    Ok(())
}

pub(crate) fn validate_password(pw: &str) -> ApiResult<()> {
    if pw.chars().count() < 8 {
        return Err(ApiError::validation("password must be at least 8 characters"));
    }
    if pw.len() > 1024 {
        return Err(ApiError::validation("password must be at most 1024 bytes"));
    }
    Ok(())
}

fn validate_email(e: &str) -> ApiResult<()> {
    let valid = e.len() <= 254
        && e.split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        && !e.chars().any(char::is_whitespace);
    if valid {
        Ok(())
    } else {
        Err(ApiError::validation("email address is invalid"))
    }
}

#[cfg(test)]
mod tests {
    use super::host_allowed;

    #[test]
    fn only_names_for_this_computer_skip_the_setup_code() {
        for ok in [
            "localhost",
            "localhost:7700",
            "LOCALHOST.",
            "reelix.localhost:7700",
            "127.0.0.1:7700",
            "127.0.1.1",
            "[::1]:7700",
            "[::1]",
            "192.168.1.10:7700",
            "media-box:7700",
        ] {
            assert!(host_allowed(ok, Some("media-box")), "{ok}");
        }
        for bad in [
            "attacker.example:7700",
            "attacker.example",
            "localhost.attacker.example",
            "media-box.attacker.example",
            "[::1",
            "[::1]x",
            "",
        ] {
            assert!(!host_allowed(bad, Some("media-box")), "{bad}");
        }
        assert!(!host_allowed("media-box", None));
    }
}
