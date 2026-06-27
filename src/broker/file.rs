//! FileCredentialBroker — the only wired impl today.
//!
//! ⚠ GOD-KEY PATH: `get` ignores `req.audience` / `req.scopes` entirely. It
//! returns the user OAuth token if present (refreshing silently when near
//! expiry), else falls back to the env's full-access api_key as a bootstrap
//! bearer. Least-privilege is aspirational here until the scoped-keys API lands.

use async_trait::async_trait;

use super::{
    AuthSource, Credential, CredentialBroker, CredentialRequest, EnvOpt, LogoutOptions, TokenType,
    WhoAmI, REFRESH_SKEW_MS,
};
use crate::config::creds::{load_store, resolve_credentials, save_creds_for_env, CredRecord, Store, VaibotEnv};
use crate::config::token_store::{FileTokenStore, OAuthSession, TokenStore};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;
use crate::oauth::{self, discovery, session_from_tokens, AuthMode, LoginOptions};

pub struct FileCredentialBroker {
    store: Box<dyn TokenStore>,
}

impl FileCredentialBroker {
    pub fn new() -> Self {
        FileCredentialBroker {
            store: Box::new(FileTokenStore::new(&ProcessEnv)),
        }
    }

    /// Construct with an injected token store (tests).
    pub fn with_store(store: Box<dyn TokenStore>) -> Self {
        FileCredentialBroker { store }
    }

    fn env(&self) -> VaibotEnv {
        let store = creds_store();
        crate::config::creds::resolve_env(&ProcessEnv, &store)
    }

    /// Silent refresh against the SAME issuer the session was minted with. Returns
    /// the refreshed session, or `None` on failure (caller falls back / throws).
    async fn refresh(&self, session: &OAuthSession) -> Option<OAuthSession> {
        let refresh_token = session.refresh_token.clone()?;
        let issuer = session
            .issuer
            .clone()
            .unwrap_or_else(|| oauth::resolve_issuer(None));
        let meta = discovery::discover(&issuer).await.ok()?;
        let client = discovery::build_client(
            oauth2::ClientId::new(oauth::CLI_CLIENT_ID.to_string()),
            &meta,
            None,
        )
        .ok()?;
        let http = discovery::http_client().ok()?;
        let token = client
            .exchange_refresh_token(&oauth2::RefreshToken::new(refresh_token))
            .request_async(&http)
            .await
            .ok()?;
        let token_set = crate::oauth::loopback::token_set_from(token);
        let next = session_from_tokens(token_set, Some(session));
        // Persist against the resolved env.
        self.store.save(&next, self.env()).ok()?;
        Some(next)
    }
}

impl Default for FileCredentialBroker {
    fn default() -> Self {
        Self::new()
    }
}

fn creds_store() -> Store {
    load_store(&credentials_path(&ProcessEnv))
}

fn now_ms() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}

fn to_credential(s: &OAuthSession) -> Credential {
    Credential {
        access_token: s.access_token.clone(),
        refresh_token: s.refresh_token.clone(),
        expires_at: s.expires_at,
        scope: s.scope.clone(),
        token_type: TokenType::Bearer,
        is_api_key: false,
    }
}

#[async_trait]
impl CredentialBroker for FileCredentialBroker {
    async fn get(&self, _req: Option<CredentialRequest>) -> Result<Credential, CliError> {
        // GOD-KEY: req is intentionally ignored — see seam note in mod.rs.
        let env = self.env();
        if let Some(session) = self.store.load(env)? {
            if session.expires_at - now_ms() <= REFRESH_SKEW_MS && session.refresh_token.is_some() {
                if let Some(refreshed) = self.refresh(&session).await {
                    return Ok(to_credential(&refreshed));
                }
            }
            return Ok(to_credential(&session));
        }

        // No interactive session — fall back to the env api_key bootstrap bearer.
        let store = creds_store();
        let resolved = resolve_credentials(&ProcessEnv, &store);
        if let Some(api_key) = resolved.api_key {
            return Ok(Credential {
                access_token: api_key,
                refresh_token: None,
                expires_at: f64::INFINITY,
                scope: None,
                token_type: TokenType::Bearer,
                is_api_key: true,
            });
        }

        Err(CliError::Auth)
    }

    async fn login(
        &self,
        opts: LoginOptions,
        print: &(dyn for<'a> Fn(&'a str) + Sync),
    ) -> Result<Credential, CliError> {
        let issuer = oauth::resolve_issuer(opts.issuer.as_deref());

        let token_set = match opts.mode {
            AuthMode::Device => oauth::device::login(&issuer, oauth::LOGIN_SCOPE, print).await?,
            AuthMode::Loopback => {
                match oauth::loopback::login(&issuer, oauth::LOGIN_SCOPE, opts.no_browser, print).await {
                    Ok(t) => t,
                    Err(oauth::loopback::LoopbackError::BrowserUnavailable(_)) => {
                        // Headless / SSH — auto-fall through to device flow.
                        print("No browser available — falling back to device login.");
                        oauth::device::login(&issuer, oauth::LOGIN_SCOPE, print).await?
                    }
                    Err(oauth::loopback::LoopbackError::Cli(c)) => return Err(c),
                }
            }
        };

        let mut session = session_from_tokens(token_set, None);
        session.issuer = Some(issuer); // persist issuer so refresh hits same one
        self.store.save(&session, opts.env)?;
        Ok(to_credential(&session))
    }

    async fn logout(&self, opts: Option<LogoutOptions>) -> Result<(), CliError> {
        let env = opts.as_ref().and_then(|o| o.env).unwrap_or_else(|| self.env());
        self.store.clear(env)?;
        // --all-hosts is parsed-but-inert: no scoped keys to revoke (god-key model).
        Ok(())
    }

    async fn whoami(&self, opts: Option<EnvOpt>) -> Result<Option<WhoAmI>, CliError> {
        let env = opts.and_then(|o| o.env).unwrap_or_else(|| self.env());
        if let Some(session) = self.store.load(env)? {
            return Ok(Some(WhoAmI {
                subject: session.subject.clone().unwrap_or_else(|| "(unknown)".into()),
                email: session.email.clone(),
                scope: session.scope.clone(),
                expires_in_sec: ((session.expires_at - now_ms()) / 1000.0).round(),
                source: AuthSource::OAuth,
            }));
        }
        let store = creds_store();
        let resolved = resolve_credentials(&ProcessEnv, &store);
        if resolved.api_key.is_some() {
            return Ok(Some(WhoAmI {
                subject: resolved.wallet_address.unwrap_or_else(|| "(api-key account)".into()),
                email: None,
                scope: None,
                expires_in_sec: f64::INFINITY,
                source: AuthSource::ApiKey,
            }));
        }
        Ok(None)
    }
}

/// Persist an api_key for the resolved env (used by bootstrap/init paths).
pub fn persist_api_key(env: VaibotEnv, api_key: String) -> Result<(), CliError> {
    save_creds_for_env(
        &credentials_path(&ProcessEnv),
        env,
        CredRecord {
            api_key,
            wallet_address: None,
        },
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::token_store::FileTokenStore;

    #[tokio::test]
    async fn whoami_returns_oauth_identity_from_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::at(dir.path().join("oauth.json"));
        store
            .save(
                &OAuthSession {
                    access_token: "tok".into(),
                    refresh_token: None,
                    expires_at: now_ms() + 3_600_000.0,
                    token_type: "Bearer".into(),
                    scope: Some("openid govern".into()),
                    subject: Some("user-9".into()),
                    email: Some("a@b.c".into()),
                    issuer: Some("https://oauth.vaibot.io".into()),
                },
                VaibotEnv::Production,
            )
            .unwrap();
        // Force env resolution to production regardless of host env.
        std::env::set_var("VAIBOT_ENV", "production");
        let broker = FileCredentialBroker::with_store(Box::new(store));
        let who = broker.whoami(None).await.unwrap().unwrap();
        assert_eq!(who.email.as_deref(), Some("a@b.c"));
        assert_eq!(who.source, AuthSource::OAuth);
        std::env::remove_var("VAIBOT_ENV");
    }

    #[tokio::test]
    async fn get_without_session_or_key_is_auth_error() {
        let dir = tempfile::tempdir().unwrap();
        // Point creds + oauth at an empty temp dir, and clear any host api key.
        std::env::set_var("VAIBOT_CONFIG_DIR", dir.path());
        std::env::set_var("VAIBOT_ENV", "production");
        std::env::remove_var("VAIBOT_API_KEY");
        let store = FileTokenStore::at(dir.path().join("oauth.json"));
        let broker = FileCredentialBroker::with_store(Box::new(store));
        let err = broker.get(None).await.unwrap_err();
        assert!(matches!(err, CliError::Auth));
        std::env::remove_var("VAIBOT_CONFIG_DIR");
        std::env::remove_var("VAIBOT_ENV");
    }
}
