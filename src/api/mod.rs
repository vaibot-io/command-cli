//! ApiClient — thin reqwest wrapper for the V2 governance API.
//!
//! Base URL = `api_base_for_env(env, override)`; Bearer = whatever the broker
//! returns. Built once per invocation and reused. Only set Content-Type when a
//! body is present (Fastify rejects bodyless requests advertising JSON).
//!
//! NETWORK GATE: construction is cheap and offline; requests fire only from the
//! command handlers that call these methods.

pub mod account;
pub mod policy;
pub mod provenance;

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::error::CliError;

/// A non-throwing API result mirroring the TS `ApiResult<T>` discriminated union.
pub enum ApiResult<T> {
    Ok { data: T, status: u16 },
    Err { error: String, status: u16 },
}

impl<T> ApiResult<T> {
    pub fn is_ok(&self) -> bool {
        matches!(self, ApiResult::Ok { .. })
    }
}

/// Reusable client carrying base URL + bearer.
pub struct ApiClient {
    base_url: String,
    bearer: Option<String>,
    http: reqwest::Client,
}

impl ApiClient {
    pub fn new(base_url: impl Into<String>, bearer: Option<String>) -> Result<Self, CliError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| CliError::Runtime(format!("http client: {e}")))?;
        Ok(ApiClient {
            base_url: base_url.into(),
            bearer,
            http,
        })
    }

    /// The configured base URL (for stream construction, etc.).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn bearer(&self) -> Option<&str> {
        self.bearer.as_deref()
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> ApiResult<T> {
        let url = format!("{}{path}", self.base_url);
        let mut req = self.http.request(method, &url).header("Accept", "application/json");
        if let Some(b) = &body {
            req = req.header("Content-Type", "application/json");
            req = req.body(serde_json::to_string(b).unwrap_or_default());
        }
        if let Some(token) = &self.bearer {
            req = req.bearer_auth(token);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                return ApiResult::Err {
                    error: e.to_string(),
                    status: 0,
                }
            }
        };
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let json: Option<serde_json::Value> = serde_json::from_str(&text).ok();

        if !(200..300).contains(&status) {
            // Surface both the short error code and the human message.
            let err_code = json
                .as_ref()
                .and_then(|j| j.get("error"))
                .and_then(|v| v.as_str());
            let err_msg = json
                .as_ref()
                .and_then(|j| j.get("message"))
                .and_then(|v| v.as_str());
            let error = match (err_code, err_msg) {
                (Some(c), Some(m)) => format!("{c}: {m}"),
                (Some(c), None) => c.to_string(),
                (None, Some(m)) => m.to_string(),
                (None, None) => format!("HTTP {status}"),
            };
            return ApiResult::Err { error, status };
        }

        match serde_json::from_str::<T>(&text) {
            Ok(data) => ApiResult::Ok { data, status },
            Err(e) => ApiResult::Err {
                error: format!("response parse: {e}"),
                status,
            },
        }
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        self.request(reqwest::Method::GET, path, None).await
    }

    pub(crate) async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> ApiResult<T> {
        self.request(reqwest::Method::POST, path, body).await
    }
}
