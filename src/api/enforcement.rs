//! Tier-0 containment: POST /v2/enforcement/contain, POST /v2/enforcement/release.
//!
//! Arming and clearing are deliberately asymmetric. A false arm costs a stalled
//! agent and is recoverable in seconds, so any authenticated principal on the
//! account may arm. Clearing re-enables every agent on every machine, so it
//! takes an admin plus an emailed second factor.

use serde::Deserialize;

use super::{ApiClient, ApiResult};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ContainResponse {
    pub contained: bool,
    /// True when the account was already contained — arming is idempotent.
    pub already: bool,
    pub contained_at: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ReleaseResponse {
    pub contained: bool,
    pub already: bool,
    /// Which second factor was accepted: `email` | `mobile_device_key`.
    pub release_factor: Option<String>,
}

impl ApiClient {
    /// POST /v2/enforcement/contain — arm the panic switch for this account.
    pub async fn contain(&self, reason: Option<&str>) -> ApiResult<ContainResponse> {
        let mut body = serde_json::json!({ "source": "cli" });
        if let Some(r) = reason {
            body["reason"] = serde_json::Value::String(r.to_string());
        }
        self.post("/v2/enforcement/contain", Some(body)).await
    }

    /// POST /v2/enforcement/release — lift containment (admin + second factor).
    pub async fn release(&self) -> ApiResult<ReleaseResponse> {
        self.post("/v2/enforcement/release", None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Forward compatibility: the CLI ships ahead of and behind the API, so a
    // response with fields it has never heard of, or missing ones it expects,
    // must still parse rather than fail a panic-button command.

    #[test]
    fn contain_parses_a_full_response() {
        let json = r#"{"ok":true,"contained":true,"contained_at":"2026-09-24T10:00:00Z","reason":"laptop compromised"}"#;
        let parsed: ContainResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.contained);
        assert!(!parsed.already);
        assert_eq!(parsed.reason.as_deref(), Some("laptop compromised"));
    }

    #[test]
    fn contain_parses_the_idempotent_repeat() {
        let json = r#"{"ok":true,"contained":true,"already":true,"contained_at":"2026-09-24T10:00:00Z","reason":"first"}"#;
        let parsed: ContainResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.already, "a repeat arm is reported, not treated as new");
        assert_eq!(parsed.reason.as_deref(), Some("first"));
    }

    #[test]
    fn contain_tolerates_a_sparse_or_extended_response() {
        let sparse: ContainResponse = serde_json::from_str(r#"{"contained":true}"#).unwrap();
        assert!(sparse.contained);
        assert!(sparse.contained_at.is_none());

        let extended: ContainResponse =
            serde_json::from_str(r#"{"contained":true,"reason":"x","future_field":42}"#).unwrap();
        assert!(extended.contained);
    }

    #[test]
    fn release_reports_which_factor_was_accepted() {
        let email: ReleaseResponse =
            serde_json::from_str(r#"{"ok":true,"contained":false,"release_factor":"email"}"#).unwrap();
        assert!(!email.contained);
        assert_eq!(email.release_factor.as_deref(), Some("email"));

        // The mobile path lands later; the shape already carries it.
        let device: ReleaseResponse =
            serde_json::from_str(r#"{"contained":false,"release_factor":"mobile_device_key"}"#).unwrap();
        assert_eq!(device.release_factor.as_deref(), Some("mobile_device_key"));
    }

    #[test]
    fn release_parses_the_nothing_to_do_case() {
        let parsed: ReleaseResponse = serde_json::from_str(r#"{"ok":true,"contained":false,"already":true}"#).unwrap();
        assert!(parsed.already);
        assert!(parsed.release_factor.is_none());
    }
}
