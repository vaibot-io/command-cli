//! CredentialBroker — the single auth choke point for the whole CLI.
//!
//! Governance posture: NO command reads tokens directly. guard / gateway / mcp /
//! policy / provenance all call `broker.get(req)` and hand the result to their
//! service. That makes the broker the one place to enforce least-privilege once
//! scoped keys exist.
//!
//! ┌─ god-keys-now / scoped-keys-later seam ────────────────────────────────────┐
//! │ CredentialRequest carries `audience` + `scopes` TODAY, but                  │
//! │ FileCredentialBroker IGNORES them and returns the single full-access        │
//! │ user/account credential (the "god key"). The signature is already          │
//! │ scoped-shaped, so the future upgrade is a BINDING SWAP in get_broker() —    │
//! │ not a refactor of any call site.                                            │
//! └────────────────────────────────────────────────────────────────────────────┘

pub mod file;
pub mod scoped;

use std::sync::OnceLock;

use async_trait::async_trait;

use crate::config::creds::VaibotEnv;
use crate::error::CliError;
use crate::oauth::{AuthMode, LoginOptions};

/// Refresh once we're within this window (ms) of expiry.
pub const REFRESH_SKEW_MS: f64 = 60_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Bearer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSource {
    OAuth,
    ApiKey,
}

/// Caller declares the capability it needs. TODAY these fields are CARRIED BUT
/// IGNORED — every component gets the one full-access user/account key.
#[derive(Debug, Clone, Default)]
pub struct CredentialRequest {
    /// Target resource server. IGNORED until scoped keys ship.
    pub audience: Option<String>,
    /// e.g. ["guard:read","receipts:write"]. IGNORED until scoped keys ship.
    pub scopes: Vec<String>,
}

/// The bearer a service should send.
#[derive(Debug, Clone)]
pub struct Credential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// epoch ms; `f64::INFINITY` for api_key.
    pub expires_at: f64,
    pub scope: Option<String>,
    pub token_type: TokenType,
    /// True when this is the env api_key bootstrap bearer, not a user JWT.
    pub is_api_key: bool,
}

/// Identity snapshot for `whoami` / `status`.
#[derive(Debug, Clone)]
pub struct WhoAmI {
    pub subject: String,
    pub email: Option<String>,
    pub scope: Option<String>,
    /// Seconds until the access token expires (negative if expired; +inf for api_key).
    pub expires_in_sec: f64,
    pub source: AuthSource,
}

/// Logout options. `all_hosts` is parsed-but-inert under the god-key model.
#[derive(Debug, Clone, Default)]
pub struct LogoutOptions {
    pub env: Option<VaibotEnv>,
    pub all_hosts: bool,
}

/// Env scoping for whoami.
#[derive(Debug, Clone, Default)]
pub struct EnvOpt {
    pub env: Option<VaibotEnv>,
}

#[async_trait]
pub trait CredentialBroker: Send + Sync {
    /// Refresh/mint as needed; returns the bearer a service should send.
    async fn get(&self, req: Option<CredentialRequest>) -> Result<Credential, CliError>;
    /// Interactive login (loopback PKCE or device); persists the session.
    async fn login(&self, opts: LoginOptions, print: &(dyn for<'a> Fn(&'a str) + Sync))
        -> Result<Credential, CliError>;
    /// Clear the local session.
    async fn logout(&self, opts: Option<LogoutOptions>) -> Result<(), CliError>;
    /// Identity snapshot, or `None` when not logged in.
    async fn whoami(&self, opts: Option<EnvOpt>) -> Result<Option<WhoAmI>, CliError>;
}

// ── composition root ────────────────────────────────────────────────────────

static BROKER: OnceLock<Box<dyn CredentialBroker>> = OnceLock::new();

/// The ONLY binding swap point. Swapping god-key → scoped is one line here:
/// gate on `VAIBOT_SCOPED_KEYS == "1"` to construct `ScopedCredentialBroker`.
/// Zero call-site churn, because every command already calls `get_broker().get()`.
pub fn get_broker() -> &'static dyn CredentialBroker {
    BROKER
        .get_or_init(|| {
            if std::env::var("VAIBOT_SCOPED_KEYS").as_deref() == Ok("1") {
                Box::new(scoped::ScopedCredentialBroker::new(file::FileCredentialBroker::new()))
            } else {
                Box::new(file::FileCredentialBroker::new())
            }
        })
        .as_ref()
}

/// Test seam: install a broker before any `get_broker()` call. Returns `false`
/// if a broker was already initialized (OnceLock can't be re-set).
pub fn set_broker_for_test(b: Box<dyn CredentialBroker>) -> bool {
    BROKER.set(b).is_ok()
}

/// Re-export so commands can build `LoginOptions` with the right mode without a
/// second import path.
pub use crate::oauth::AuthMode as Mode;

/// Convenience: choose mode from a `--device` flag.
pub fn mode_for(device: bool) -> AuthMode {
    if device {
        AuthMode::Device
    } else {
        AuthMode::Loopback
    }
}
