//! ScopedCredentialBroker — the least-privilege SEAM (STUB today).
//!
//! When the scoped-keys server build lands, `get` will honor `req.audience` /
//! `req.scopes` via RFC 8693 token-exchange OR the server's mint-key-by-role
//! endpoint (role → fixed narrow scope; no-escalation enforced server-side).
//!
//! Swapping the binding in `get_broker()` (gated on `VAIBOT_SCOPED_KEYS == "1"`)
//! is the ENTIRE migration — zero call-site churn, because every command already
//! calls `get_broker().get(...)`.
//!
//! Until then: `login` / `logout` / `whoami` delegate to the inner
//! `FileCredentialBroker` (the user JWT path is unchanged), and `get` returns a
//! clean StubError (exit 2).

use async_trait::async_trait;

use super::file::FileCredentialBroker;
use super::{
    Credential, CredentialBroker, CredentialRequest, EnvOpt, LogoutOptions, WhoAmI,
};
use crate::error::CliError;
use crate::oauth::LoginOptions;

pub struct ScopedCredentialBroker {
    inner: FileCredentialBroker,
}

impl ScopedCredentialBroker {
    pub fn new(inner: FileCredentialBroker) -> Self {
        ScopedCredentialBroker { inner }
    }
}

#[async_trait]
impl CredentialBroker for ScopedCredentialBroker {
    async fn get(&self, _req: Option<CredentialRequest>) -> Result<Credential, CliError> {
        // SEAM: mint/cache a narrow key for req.scopes against req.audience here.
        // The shape is already scoped (CredentialRequest carries audience+scopes),
        // so wiring this is purely additive — no command code changes.
        Err(CliError::stub("scoped credentials"))
    }

    async fn login(
        &self,
        opts: LoginOptions,
        print: &(dyn for<'a> Fn(&'a str) + Sync),
    ) -> Result<Credential, CliError> {
        self.inner.login(opts, print).await
    }

    async fn logout(&self, opts: Option<LogoutOptions>) -> Result<(), CliError> {
        self.inner.logout(opts).await
    }

    async fn whoami(&self, opts: Option<EnvOpt>) -> Result<Option<WhoAmI>, CliError> {
        self.inner.whoami(opts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scoped_get_is_stub_exit2() {
        let b = ScopedCredentialBroker::new(FileCredentialBroker::new());
        let err = b.get(None).await.unwrap_err();
        assert!(matches!(err, CliError::Stub { .. }));
        assert_eq!(err.exit_code() as i32, 2);
    }
}
