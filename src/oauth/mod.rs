//! OAuth 2.1 surface for the interactive `vaibot login`.
//!
//! - Issuer resolution: `--api-url || $VAIBOT_OAUTH_URL || https://oauth.vaibot.io`
//!   (trailing slashes stripped).
//! - Loopback PKCE (default) on a 127.0.0.1:0 one-shot callback server.
//! - RFC 8628 device flow (`--device`, or auto-fallback when no browser).
//!
//! NETWORK GATE: nothing here performs a network call at construction. Discovery
//! and token exchange only run inside `loopback::login` / `device::login` /
//! `refresh`, which are reached only when the `login` command (or a refresh of a
//! near-expired token) is actually invoked.

pub mod device;
pub mod discovery;
pub mod loopback;
pub mod pkce;
pub mod tokens;

use crate::config::creds::VaibotEnv;
use crate::config::token_store::OAuthSession;
use crate::error::CliError;

pub const DEFAULT_OAUTH_ISSUER: &str = "https://oauth.vaibot.io";

/// Public, non-secret client id for the installed CLI app (PKCE + device).
pub const CLI_CLIENT_ID: &str = "vaibot-cli";

/// Scopes requested by the interactive user login. `govern` is the VAIBot
/// custom scope gating Governance-plane actions; openid/profile/email standard.
pub const LOGIN_SCOPE: &str = "openid profile email govern";

/// Which interactive flow to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Loopback,
    Device,
}

/// Options for an interactive login, assembled by `commands::auth::login`.
pub struct LoginOptions {
    pub mode: AuthMode,
    pub no_browser: bool,
    /// Override issuer (--api-url).
    pub issuer: Option<String>,
    pub env: VaibotEnv,
}

/// Resolve the issuer base URL with the documented precedence (slashes stripped).
pub fn resolve_issuer(explicit: Option<&str>) -> String {
    let raw = explicit
        .map(|s| s.to_string())
        .or_else(|| std::env::var("VAIBOT_OAUTH_URL").ok())
        .unwrap_or_else(|| DEFAULT_OAUTH_ISSUER.to_string());
    raw.trim_end_matches('/').to_string()
}

/// Typed signal that the loopback flow could not open a browser — the broker
/// catches this to auto-fall through to the device flow.
#[derive(Debug)]
pub struct BrowserUnavailable {
    pub auth_url: String,
}

impl std::fmt::Display for BrowserUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no browser available; open this URL to authorize:\n\n  {}", self.auth_url)
    }
}

impl std::error::Error for BrowserUnavailable {}

/// Build an `OAuthSession` from a fresh token response, carrying forward prior
/// fields the server may have omitted (refresh_token, scope, subject, email).
pub fn session_from_tokens(tok: tokens::TokenSet, prev: Option<&OAuthSession>) -> OAuthSession {
    let now_ms = chrono::Utc::now().timestamp_millis() as f64;
    let expires_in_ms = (tok.expires_in.unwrap_or(3600) as f64) * 1000.0;
    OAuthSession {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token.or_else(|| prev.and_then(|p| p.refresh_token.clone())),
        expires_at: now_ms + expires_in_ms,
        token_type: "Bearer".into(),
        scope: tok.scope.or_else(|| prev.and_then(|p| p.scope.clone())),
        subject: tok.subject.or_else(|| prev.and_then(|p| p.subject.clone())),
        email: tok.email.or_else(|| prev.and_then(|p| p.email.clone())),
        issuer: prev.and_then(|p| p.issuer.clone()),
    }
}

/// Internal helper used by login flows to map any oauth2/discovery failure into
/// a CliError carrying a human message.
pub(crate) fn oauth_err(ctx: &str, e: impl std::fmt::Display) -> CliError {
    CliError::Runtime(format!("{ctx}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_issuer_strips_trailing_slashes() {
        assert_eq!(resolve_issuer(Some("https://x.io///")), "https://x.io");
    }

    #[test]
    fn resolve_issuer_defaults() {
        std::env::remove_var("VAIBOT_OAUTH_URL");
        assert_eq!(resolve_issuer(None), DEFAULT_OAUTH_ISSUER);
    }

    #[test]
    fn session_carries_forward_prev_fields() {
        let prev = OAuthSession {
            access_token: "old".into(),
            refresh_token: Some("R".into()),
            expires_at: 0.0,
            token_type: "Bearer".into(),
            scope: Some("openid".into()),
            subject: Some("sub".into()),
            email: Some("a@b.c".into()),
            issuer: Some("https://iss".into()),
        };
        let tok = tokens::TokenSet {
            access_token: "new".into(),
            refresh_token: None,
            expires_in: Some(60),
            scope: None,
            subject: None,
            email: None,
        };
        let s = session_from_tokens(tok, Some(&prev));
        assert_eq!(s.access_token, "new");
        assert_eq!(s.refresh_token.as_deref(), Some("R"));
        assert_eq!(s.scope.as_deref(), Some("openid"));
        assert_eq!(s.issuer.as_deref(), Some("https://iss"));
    }
}
