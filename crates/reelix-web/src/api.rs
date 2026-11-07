//! Typed HTTP client for the Reelix `/api/v1` REST API.

use std::cell::RefCell;
use std::fmt;
use std::sync::OnceLock;

use gloo_net::http::{Request, RequestBuilder, Response};
use reelix_api_types::{ApiErrorBody, CSRF_HEADER};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Error returned by every API call.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    /// HTTP status, or 0 when the request never reached the server.
    pub status: u16,
    /// Machine-readable code from [`ApiErrorBody::error`] (or `network` / `decode`).
    pub code: String,
    /// Message safe to show to the user.
    pub message: String,
}

impl ApiError {
    fn network(err: impl fmt::Display) -> Self {
        Self {
            status: 0,
            code: "network".into(),
            message: format!("Could not reach the Reelix server ({err})."),
        }
    }

    fn decode(err: impl fmt::Display) -> Self {
        Self {
            status: 0,
            code: "decode".into(),
            message: format!("Unexpected response from the server ({err})."),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// The URL base the UI is served under (`""` or e.g. `"/reelix"`), read from `<base href>`.
pub fn base_path() -> &'static str {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| {
        let path = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.base_uri().ok().flatten())
            .and_then(|uri| web_sys::Url::new(&uri).ok())
            .map(|url| url.pathname())
            .unwrap_or_default();
        path.trim_end_matches('/').to_string()
    })
}

/// Absolute path of an API endpoint, e.g. `api_url("/auth/me")` → `/reelix/api/v1/auth/me`.
pub fn api_url(path: &str) -> String {
    format!("{}/api/v1{}", base_path(), path)
}

/// `scheme://host[:port]` of the page.
pub fn origin() -> String {
    web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default()
}

/// WebSocket URL of the live event stream.
pub fn ws_url() -> String {
    let location = web_sys::window().map(|w| w.location());
    let (protocol, host) = location
        .map(|l| (l.protocol().unwrap_or_default(), l.host().unwrap_or_default()))
        .unwrap_or_default();
    let scheme = if protocol == "https:" { "wss" } else { "ws" };
    format!("{scheme}://{host}{}/api/v1/ws", base_path())
}

thread_local! {
    static ON_UNAUTHORIZED: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Registers the callback run when any request (other than a login attempt) returns 401.
pub fn set_unauthorized_handler(f: impl Fn() + 'static) {
    ON_UNAUTHORIZED.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

fn with_headers(builder: RequestBuilder) -> RequestBuilder {
    builder
        .header(CSRF_HEADER, "1")
        .header("accept", "application/json")
}

async fn finish<T: DeserializeOwned>(path: &str, result: Result<Response, gloo_net::Error>) -> ApiResult<T> {
    let resp = result.map_err(ApiError::network)?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if resp.ok() {
        // 204 / empty bodies decode as JSON `null`, which is what `()` expects.
        let body = if text.trim().is_empty() {
            "null"
        } else {
            text.as_str()
        };
        return serde_json::from_str(body).map_err(ApiError::decode);
    }

    let mut err = match serde_json::from_str::<ApiErrorBody>(&text) {
        Ok(body) => ApiError {
            status,
            code: body.error,
            message: body.message,
        },
        Err(_) => ApiError {
            status,
            code: format!("http_{status}"),
            message: format!("Request failed ({status} {}).", resp.status_text()),
        },
    };
    if status == 429
        && let Some(secs) = resp.headers().get("retry-after")
        && !err.message.contains(&secs)
    {
        err.message = format!("{} Try again in {secs}s.", err.message.trim_end());
    }
    if status == 401 && path != "/auth/login" {
        ON_UNAUTHORIZED.with(|h| {
            if let Some(cb) = h.borrow().as_ref() {
                cb();
            }
        });
    }
    Err(err)
}

/// `GET {path}` and decode the JSON body.
pub async fn get<T: DeserializeOwned>(path: &str) -> ApiResult<T> {
    let result = with_headers(Request::get(&api_url(path))).send().await;
    finish(path, result).await
}

/// `POST {path}` with a JSON body.
pub async fn post<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    let req = with_headers(Request::post(&api_url(path)))
        .json(body)
        .map_err(ApiError::decode)?;
    finish(path, req.send().await).await
}

/// `PUT {path}` with a JSON body.
pub async fn put<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    let req = with_headers(Request::put(&api_url(path)))
        .json(body)
        .map_err(ApiError::decode)?;
    finish(path, req.send().await).await
}

/// `POST {path}` with a file as the raw request body (e.g. a backup archive).
pub async fn post_file<T: DeserializeOwned>(path: &str, file: &web_sys::File) -> ApiResult<T> {
    post_file_with(path, file, &[]).await
}

/// [`post_file`] with extra request headers (the set-up code on `/setup/restore`).
pub async fn post_file_with<T: DeserializeOwned>(
    path: &str,
    file: &web_sys::File,
    headers: &[(&str, &str)],
) -> ApiResult<T> {
    let content_type = Some(file.type_()).filter(|t| !t.is_empty());
    let mut builder = with_headers(Request::post(&api_url(path)));
    for (k, v) in headers {
        builder = builder.header(k, v);
    }
    let req = builder
        .header(
            "content-type",
            content_type.as_deref().unwrap_or("application/octet-stream"),
        )
        .body(wasm_bindgen::JsValue::from(file.clone()))
        .map_err(ApiError::decode)?;
    finish(path, req.send().await).await
}

/// `POST {path}` without a body.
pub async fn post_empty<T: DeserializeOwned>(path: &str) -> ApiResult<T> {
    let result = with_headers(Request::post(&api_url(path))).send().await;
    finish(path, result).await
}

/// `DELETE {path}` with a JSON body, decoding the JSON response (bulk removals).
pub async fn delete_with_body<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    let req = with_headers(Request::delete(&api_url(path)))
        .json(body)
        .map_err(ApiError::decode)?;
    finish(path, req.send().await).await
}

/// `DELETE {path}`.
pub async fn delete(path: &str) -> ApiResult<()> {
    let result = with_headers(Request::delete(&api_url(path))).send().await;
    finish(path, result).await
}
