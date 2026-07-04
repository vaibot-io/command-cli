//! Loopback PKCE login (default flow).
//!
//! 1. Generate S256 PKCE + random state.
//! 2. Bind a one-shot `tiny_http` server on 127.0.0.1:0 (IP literal, NEVER
//!    `localhost`); read the kernel-assigned port BEFORE building the redirect.
//! 3. Build the authorization URL; open the browser (or print on --no-browser).
//! 4. Receive exactly one callback on `/callback`; 404 anything else; reject a
//!    state mismatch (CSRF) and surfaced `?error=`.
//! 5. Exchange the code (PKCE verifier + expected state) at the token endpoint.
//! 6. 5-minute deadline; on timeout, fail cleanly. The server socket closes on
//!    drop.
//!
//! Network calls (discovery + token exchange) happen only when this fn runs.

use std::time::Duration;

use oauth2::{AuthorizationCode, ClientId, CsrfToken, RedirectUrl, Scope, TokenResponse};
use url::Url;

use super::discovery::{build_client, discover};
use super::pkce;
use super::tokens::{identity_from_tokens, TokenSet};
use super::{oauth_err, BrowserUnavailable, CLI_CLIENT_ID};
use crate::error::CliError;

const CALLBACK_DEADLINE: Duration = Duration::from_secs(300);

/// Result of a successful callback: the auth code + the state we got back.
struct Callback {
    code: String,
    state: String,
}

/// Run the loopback PKCE login. `print` receives user-facing status lines.
///
/// Returns `Err(CliError::Runtime)` carrying a `BrowserUnavailable` cause when
/// `open` fails — the broker downcasts on that to fall back to device flow. We
/// model that as a dedicated error string the broker recognizes; see
/// `commands::auth`.
pub async fn login(
    issuer: &str,
    scope: &str,
    no_browser: bool,
    print: &(dyn for<'a> Fn(&'a str) + Sync),
) -> Result<TokenSet, LoopbackError> {
    let meta = discover(issuer).await.map_err(LoopbackError::Cli)?;

    let pair = pkce::generate();
    let expected_state = pair.state.secret().clone();

    // Bind FIRST so we know the port before constructing the redirect URI.
    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| LoopbackError::Cli(oauth_err("loopback bind", e)))?;
    let port = match server.server_addr() {
        tiny_http::ListenAddr::IP(addr) => addr.port(),
        #[allow(unreachable_patterns)]
        _ => return Err(LoopbackError::Cli(CliError::Runtime("loopback: non-IP listen addr".into()))),
    };
    let redirect = format!("http://127.0.0.1:{port}/callback");

    let client = build_client(
        ClientId::new(CLI_CLIENT_ID.to_string()),
        &meta,
        Some(RedirectUrl::new(redirect.clone()).map_err(|e| LoopbackError::Cli(oauth_err("redirect url", e)))?),
    )
    .map_err(LoopbackError::Cli)?;

    let (auth_url, _csrf) = client
        .authorize_url(|| CsrfToken::new(expected_state.clone()))
        .add_scopes(scope.split_whitespace().map(|s| Scope::new(s.to_string())))
        .set_pkce_challenge(pair.challenge)
        .url();

    // Browser launch (or print on --no-browser / headless).
    if no_browser {
        print(&format!("Open this URL to authorize:\n\n  {auth_url}\n"));
    } else if let Err(_e) = open::that(auth_url.as_str()) {
        return Err(LoopbackError::BrowserUnavailable(BrowserUnavailable {
            auth_url: auth_url.to_string(),
        }));
    } else {
        print("Opened your browser to complete login...");
    }

    // Wait for the one-shot callback with a hard deadline. tiny_http is
    // blocking; run it on a blocking thread and race a timer.
    let cb = tokio::select! {
        res = tokio::task::spawn_blocking(move || recv_callback(server, &expected_state)) => {
            res.map_err(|e| LoopbackError::Cli(oauth_err("loopback join", e)))??
        }
        _ = tokio::time::sleep(CALLBACK_DEADLINE) => {
            return Err(LoopbackError::Cli(CliError::Runtime(
                "Login timed out — no callback received.".into(),
            )));
        }
    };

    // Exchange the code (PKCE verifier).
    let http = super::discovery::http_client().map_err(LoopbackError::Cli)?;
    let token = client
        .exchange_code(AuthorizationCode::new(cb.code))
        .set_pkce_verifier(pair.verifier)
        .request_async(&http)
        .await
        .map_err(|e| LoopbackError::Cli(oauth_err("token exchange", e)))?;

    let _ = cb.state; // already verified in recv_callback
    Ok(token_set_from(token))
}

/// Build the normalized `TokenSet` from an oauth2 token response.
pub(crate) fn token_set_from(token: super::discovery::CliTokenResponse) -> TokenSet {
    let access_token = token.access_token().secret().clone();
    let id_token = token.extra_fields().id_token.clone();
    // The OAuth server returns no id_token, so fall back to the access token (a Supabase
    // JWT carrying sub + email) — otherwise the CLI never learns the user's identity.
    let (subject, email) = identity_from_tokens(&access_token, id_token.as_deref());
    TokenSet {
        access_token,
        refresh_token: token.refresh_token().map(|r| r.secret().clone()),
        expires_in: token.expires_in().map(|d| d.as_secs()),
        scope: token
            .scopes()
            .map(|s| s.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ")),
        subject,
        email,
    }
}

/// Blocking: accept exactly one request, validate path + state, respond, return
/// the auth code. 404 for any non-`/callback` path; 400 for `?error=` or a
/// state mismatch.
fn recv_callback(server: tiny_http::Server, expected_state: &str) -> Result<Callback, LoopbackError> {
    let request = server
        .recv()
        .map_err(|e| LoopbackError::Cli(oauth_err("loopback recv", e)))?;

    let url = format!("http://127.0.0.1{}", request.url());
    let parsed = Url::parse(&url).map_err(|e| LoopbackError::Cli(oauth_err("callback url", e)))?;

    if parsed.path() != "/callback" {
        let _ = request.respond(tiny_http::Response::from_string("Not found").with_status_code(404));
        return Err(LoopbackError::Cli(CliError::Runtime(
            "loopback received an unexpected path".into(),
        )));
    }

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.to_string()),
            "state" => state = Some(v.to_string()),
            "error" => error = Some(v.to_string()),
            _ => {}
        }
    }

    if let Some(err) = error {
        let _ = request.respond(
            tiny_http::Response::from_string(format!("Authorization error: {err}"))
                .with_status_code(400),
        );
        return Err(LoopbackError::Cli(CliError::Runtime(format!(
            "OAuth provider returned error: {err}"
        ))));
    }

    if state.as_deref() != Some(expected_state) {
        let _ = request.respond(
            tiny_http::Response::from_string("OAuth state mismatch — possible CSRF, aborting.")
                .with_status_code(400),
        );
        return Err(LoopbackError::Cli(CliError::Runtime(
            "OAuth state mismatch — possible CSRF, aborting.".into(),
        )));
    }

    let Some(code) = code else {
        let _ = request
            .respond(tiny_http::Response::from_string("Missing code").with_status_code(400));
        return Err(LoopbackError::Cli(CliError::Runtime(
            "loopback callback missing authorization code".into(),
        )));
    };

    let html = "<!doctype html><html><body style=\"font-family:system-ui;text-align:center;margin-top:4rem\">\
                <h2>You are logged in to VAIBot.</h2><p>You can close this tab and return to your terminal.</p>\
                </body></html>";
    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
        .expect("static header");
    let _ = request.respond(
        tiny_http::Response::from_string(html)
            .with_status_code(200)
            .with_header(header),
    );

    Ok(Callback {
        code,
        state: expected_state.to_string(),
    })
}

/// Loopback-specific error so the broker can recognize the browser-unavailable
/// case for auto-fallback to the device flow.
#[derive(Debug)]
pub enum LoopbackError {
    BrowserUnavailable(BrowserUnavailable),
    Cli(CliError),
}

impl From<LoopbackError> for CliError {
    fn from(e: LoopbackError) -> Self {
        match e {
            LoopbackError::Cli(c) => c,
            LoopbackError::BrowserUnavailable(b) => CliError::Runtime(b.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_error_browser_unavailable_is_distinguishable() {
        let e = LoopbackError::BrowserUnavailable(BrowserUnavailable {
            auth_url: "https://x".into(),
        });
        assert!(matches!(e, LoopbackError::BrowserUnavailable(_)));
    }
}
