//! Account + health endpoints: GET /v2/health, GET /v2/accounts/me,
//! POST /v2/bootstrap (unauthenticated — provisions a free account by machine
//! fingerprint and returns the api_key on first creation).

use serde::Deserialize;

use super::{ApiClient, ApiResult};

/// Response from `POST /v2/bootstrap`. `api_key` is only returned the FIRST time
/// (account creation); for an already-known fingerprint only a hint comes back.
#[derive(Debug, Clone, Deserialize)]
pub struct BootstrapResponse {
    pub user_id: String,
    pub bootstrapped: bool,
    pub api_key: Option<String>,
    pub api_key_hint: Option<String>,
    #[serde(default)]
    pub claimed: bool,
    pub message: Option<String>,
}

/// Response from `POST /v2/accounts/set-email`.
#[derive(Debug, Clone, Deserialize)]
pub struct SetEmailResponse {
    pub message: Option<String>,
}

/// Response from `POST /v2/accounts/claim` and `.../claim/confirm`. The claim
/// request returns `verify_required` + a `pending_token` when the email already
/// has an account (a code is emailed); confirm returns `merged`.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimResponse {
    #[serde(default)]
    pub merged: bool,
    #[serde(default)]
    pub verify_required: bool,
    pub pending_token: Option<String>,
    pub email: Option<String>,
    pub merged_into_user_id: Option<String>,
    pub message: Option<String>,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Quota {
    pub used: i64,
    pub limit: i64,
    pub remaining: i64,
    pub month: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MeResponse {
    pub user_id: String,
    pub wallet_address: Option<String>,
    pub email: Option<String>,
    pub claimed: bool,
    pub quota: Quota,
    /// Account tier (e.g. "free" | "govern" | "audit" | "enterprise"). Absent on
    /// older API builds.
    #[serde(default)]
    pub plan: Option<String>,
    /// Server-authoritative admin flag. Absent until the backend adds it ⇒ treated
    /// as non-admin. Gates who may run outside production (with `enterprise` plan).
    #[serde(default)]
    pub admin: Option<bool>,
}

impl MeResponse {
    /// May this account run outside production? Admins (internal) and enterprise
    /// accounts may; self-serve customers may not.
    pub fn is_env_exempt(&self) -> bool {
        self.admin == Some(true)
            || self
                .plan
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case("enterprise"))
    }

    /// Server-authoritative admin. Stricter than `is_env_exempt` (enterprise is
    /// NOT admin): §5 admits a PRODUCTION URL override only for an admin account.
    pub fn is_admin(&self) -> bool {
        self.admin == Some(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthResponse {
    #[serde(default)]
    pub ok: bool,
}

impl ApiClient {
    pub async fn health(&self) -> ApiResult<HealthResponse> {
        self.get("/v2/health").await
    }

    pub async fn me(&self) -> ApiResult<MeResponse> {
        self.get("/v2/accounts/me").await
    }

    /// Provision (or recognize) a free account for this machine. Unauthenticated:
    /// call on an ApiClient built with `bearer = None`.
    pub async fn bootstrap(&self, fingerprint: &str, agent: &str) -> ApiResult<BootstrapResponse> {
        self.post(
            "/v2/bootstrap",
            Some(serde_json::json!({ "fingerprint": fingerprint, "agent": agent })),
        )
        .await
    }

    /// Send a magic link to claim/link an email to the account. Authenticated:
    /// build the ApiClient with the account's api_key as the bearer. 409 = the
    /// account is already linked.
    pub async fn set_email(&self, email: &str) -> ApiResult<SetEmailResponse> {
        self.post("/v2/accounts/set-email", Some(serde_json::json!({ "email": email })))
            .await
    }

    /// Step 1 of a verified claim. Links a fresh email, or — when the email
    /// already has an account — returns `verify_required` + a `pending_token`
    /// (a 6-digit code is emailed to that address).
    pub async fn claim(&self, email: &str) -> ApiResult<ClaimResponse> {
        self.post("/v2/accounts/claim", Some(serde_json::json!({ "email": email })))
            .await
    }

    /// Step 2: confirm the emailed code, merging this machine's account into the
    /// one that owns the email.
    pub async fn claim_confirm(&self, pending_token: &str, code: &str) -> ApiResult<ClaimResponse> {
        self.post(
            "/v2/accounts/claim/confirm",
            Some(serde_json::json!({ "pending_token": pending_token, "code": code })),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me(plan: Option<&str>, admin: Option<bool>) -> MeResponse {
        MeResponse {
            user_id: "u".into(),
            wallet_address: None,
            email: None,
            claimed: false,
            quota: Quota { used: 0, limit: 0, remaining: 0, month: "2026-06".into() },
            plan: plan.map(String::from),
            admin,
        }
    }

    #[test]
    fn admin_and_enterprise_are_env_exempt_others_are_not() {
        assert!(me(None, Some(true)).is_env_exempt()); // admin
        assert!(me(Some("enterprise"), None).is_env_exempt()); // enterprise tier
        assert!(me(Some("ENTERPRISE"), Some(false)).is_env_exempt()); // case-insensitive
        assert!(!me(Some("free"), Some(false)).is_env_exempt()); // self-serve
        assert!(!me(Some("govern"), None).is_env_exempt());
        assert!(!me(None, None).is_env_exempt()); // older API (no fields) ⇒ not exempt
    }

    #[test]
    fn is_admin_is_stricter_than_env_exempt() {
        // §5 prod URL override needs admin specifically — enterprise is NOT enough.
        assert!(me(None, Some(true)).is_admin());
        assert!(!me(Some("enterprise"), None).is_admin()); // exempt for env, but not admin
        assert!(!me(Some("enterprise"), Some(false)).is_admin());
        assert!(!me(None, None).is_admin());
    }

    #[test]
    fn me_parses_without_plan_or_admin_fields() {
        // Older API builds omit plan/admin — must still deserialize (serde default).
        let json = r#"{"user_id":"u","claimed":true,"quota":{"used":1,"limit":10,"remaining":9,"month":"2026-06"}}"#;
        let parsed: MeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.plan, None);
        assert_eq!(parsed.admin, None);
        assert!(!parsed.is_env_exempt());
    }
}
