//! Governed-policy endpoints: POST /v2/policy/request (tighten),
//! POST /v2/policy/revoke, GET /v2/policy/history.
//!
//! Policy bundles are signed SERVER-SIDE (Ed25519); the CLI never holds the key.

use serde::Deserialize;

use super::{ApiClient, ApiResult};

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyRequestResponse {
    pub version: String,
    pub hash: String,
    pub anchor_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyRef {
    pub version: String,
    #[allow(dead_code)]
    pub hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyRevokeResponse {
    pub revoked: Option<PolicyRef>,
    pub active: Option<PolicyRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyHistoryEntry {
    pub version: String,
    pub hash: String,
    pub anchor_hash: String,
    pub created_by: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub revoked_by: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyHistoryResponse {
    pub history: Vec<PolicyHistoryEntry>,
}

/// GET /v2/policy — the active signed bundle (or `{ active: null }`).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ActivePolicyResponse {
    pub bundle: Option<ActiveBundle>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveBundle {
    pub policy: ActivePolicyBody,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ActivePolicyBody {
    pub denylist: Vec<String>,
}

/// POST /v2/policy/stepup/activate response (200).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct StepUpActivateResponse {
    pub pending: bool,
    pub token: Option<String>,
    pub sent_to: Option<String>,
    pub expires_at: Option<String>,
}

/// POST /v2/policy/stepup/verify response (200).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct StepUpVerifyResponse {
    pub action: Option<String>,
    pub locked: bool,
    pub unlock_until: Option<String>,
}

impl ApiClient {
    /// POST /v2/policy/request — tighten the governed policy (add denials).
    pub async fn request_policy(&self, denylist: &[String]) -> ApiResult<PolicyRequestResponse> {
        let body = serde_json::json!({ "denylist": denylist });
        self.post("/v2/policy/request", Some(body)).await
    }

    /// POST /v2/policy/apply — declaratively replace the governed denylist with
    /// exactly `denylist` (empty = floor only). Signed server-side.
    pub async fn apply_policy(&self, denylist: &[String]) -> ApiResult<PolicyRequestResponse> {
        let body = serde_json::json!({ "denylist": denylist });
        self.post("/v2/policy/apply", Some(body)).await
    }

    /// POST /v2/policy/preset — apply a named floor (permissive|balanced|strict).
    /// The server expands the flavor into a full signed PolicyBody.
    pub async fn apply_preset(&self, flavor: &str) -> ApiResult<PolicyRequestResponse> {
        let body = serde_json::json!({ "flavor": flavor });
        self.post("/v2/policy/preset", Some(body)).await
    }

    /// POST /v2/policy/stepup/activate — begin a lock-state change; emails an OTP.
    pub async fn policy_stepup_activate(&self, action: &str) -> ApiResult<StepUpActivateResponse> {
        self.post("/v2/policy/stepup/activate", Some(serde_json::json!({ "action": action }))).await
    }

    /// POST /v2/policy/stepup/verify — complete the change with the emailed code.
    pub async fn policy_stepup_verify(&self, token: &str, code: &str) -> ApiResult<StepUpVerifyResponse> {
        self.post("/v2/policy/stepup/verify", Some(serde_json::json!({ "token": token, "code": code }))).await
    }

    /// GET /v2/policy — the authoritative active denylist from the control plane
    /// (not the possibly-stale guard). Empty when no signed bundle is active.
    pub async fn active_denylist(&self) -> ApiResult<Vec<String>> {
        match self.get::<ActivePolicyResponse>("/v2/policy").await {
            ApiResult::Ok { data, status } => ApiResult::Ok {
                data: data.bundle.map(|b| b.policy.denylist).unwrap_or_default(),
                status,
            },
            ApiResult::Err { error, status } => ApiResult::Err { error, status },
        }
    }

    /// GET /v2/policy — the active signed bundle (or `{ active: null }`). Used by
    /// `doctor` to detect a half-finished key rotation (control plane signed but
    /// the guard can't verify it).
    pub async fn active_policy(&self) -> ApiResult<ActivePolicyResponse> {
        self.get::<ActivePolicyResponse>("/v2/policy").await
    }

    /// POST /v2/policy/revoke — revoke the active bundle (rollback).
    pub async fn revoke_policy(&self) -> ApiResult<PolicyRevokeResponse> {
        self.post("/v2/policy/revoke", None).await
    }

    /// GET /v2/policy/history — audited policy change log.
    pub async fn policy_history(&self) -> ApiResult<PolicyHistoryResponse> {
        self.get("/v2/policy/history").await
    }
}
