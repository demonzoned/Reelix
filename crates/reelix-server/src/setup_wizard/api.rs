//! A small client for Reelix's own API on this server, for the terminal wizard.

use reelix_api_types::{ApiErrorBody, CSRF_HEADER};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

pub struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    /// `base` is Reelix's address, such as `http://127.0.0.1:7700`.
    pub fn new(base: &str) -> Result<Self, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        // Writes signed in with the session cookie must carry this header (CSRF guard).
        headers.insert(CSRF_HEADER, reqwest::header::HeaderValue::from_static("1"));
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .user_agent(concat!("reelix-setup/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("could not start the HTTP client: {e}"))?;
        Ok(Self {
            base: format!("{}/api/v1", base.trim_end_matches('/')),
            http,
        })
    }

    async fn finish<T: DeserializeOwned>(res: reqwest::Result<reqwest::Response>) -> Result<T, ApiError> {
        let res = res.map_err(|e| ApiError {
            status: 0,
            message: e.to_string(),
        })?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if status.is_success() {
            let body = if text.trim().is_empty() { "null" } else { &text };
            return serde_json::from_str(body).map_err(|e| ApiError {
                status: status.as_u16(),
                message: format!("an answer Reelix could not read: {e}"),
            });
        }
        let message = serde_json::from_str::<ApiErrorBody>(&text)
            .map(|b| super::term::clean(&b.message))
            .unwrap_or_else(|_| format!("HTTP {status}"));
        Err(ApiError {
            status: status.as_u16(),
            message,
        })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        Self::finish(self.http.get(format!("{}{path}", self.base)).send().await).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, ApiError> {
        Self::finish(
            self.http
                .post(format!("{}{path}", self.base))
                .json(body)
                .send()
                .await,
        )
        .await
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, ApiError> {
        Self::finish(
            self.http
                .put(format!("{}{path}", self.base))
                .json(body)
                .send()
                .await,
        )
        .await
    }

    /// Sends a file as the raw body, read as it goes rather than loaded into memory first, with the
    /// set-up code when there is one.
    pub async fn post_file<T: DeserializeOwned>(
        &self,
        path: &str,
        file: tokio::fs::File,
        len: u64,
        setup_code: &str,
    ) -> Result<T, ApiError> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .header(reqwest::header::CONTENT_TYPE, "application/zip")
            .header(reqwest::header::CONTENT_LENGTH, len)
            .header(reelix_api_types::CSRF_HEADER, "1")
            .body(reqwest::Body::from(file));
        if !setup_code.is_empty() {
            req = req.header("x-reelix-setup-token", setup_code);
        }
        Self::finish(req.send().await).await
    }
}
