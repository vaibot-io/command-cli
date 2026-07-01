//! Command handlers + shared dispatch helpers.
//!
//! `resolve_api_client` builds the V2 `ApiClient` once per invocation: it
//! resolves the env + base URL from the credential store, then asks the broker
//! for a bearer. Every networked command funnels its credential through the
//! broker — no command reads tokens directly (the governance choke point).

pub mod account;
pub mod auth;
pub mod gateway;
pub mod guard;
pub mod mcp;
pub mod mode;
pub mod plugin;
pub mod policy;
pub mod provenance;
pub mod setup;
pub mod status;

use crate::api::{ApiClient, ApiResult};
use crate::broker::{get_broker, CredentialRequest};
use crate::config::creds::{
    env_for_api_url, gate_url_override, governance_base_for_env, load_store, resolve_credentials,
    resolve_env, url_override_allowed, VaibotEnv,
};
use crate::config::{credentials_path, guard_env_path, ProcessEnv};
use crate::error::CliError;

/// Resolve the env the CLI is operating in.
pub fn current_env() -> VaibotEnv {
    let store = load_store(&credentials_path(&ProcessEnv));
    resolve_env(&ProcessEnv, &store)
}

/// Resolve the V2 **governance** base URL for the current env.
///
/// v3 (creds split): store-aware — honors a stored `governance.url` slot, and the
/// override precedence mirrors `resolve_credentials`: `--api-url` flag →
/// `VAIBOT_GOVERNANCE_URL` → stored slot → canonical default. (Deprecated
/// `VAIBOT_API_URL` overrides NO base — env inference only. The §5 admin/flag gate
/// on *production* overrides is enforced by the production-env preflight.)
pub fn api_base(override_url: Option<&str>) -> String {
    let store = load_store(&credentials_path(&ProcessEnv));
    let env = resolve_env(&ProcessEnv, &store);
    let gov_override = override_url
        .map(String::from)
        .or_else(|| std::env::var("VAIBOT_GOVERNANCE_URL").ok().filter(|s| !s.is_empty()));
    // §5 flag-gate: a production override lacking VAIBOT_ALLOW_URL_OVERRIDE is
    // dropped here (admin half enforced by enforce_url_override_policy preflight).
    let gated = gate_url_override(env, gov_override.as_deref(), url_override_allowed(&ProcessEnv));
    governance_base_for_env(&store, env, gated)
}

/// Build an `ApiClient` for the current env, with a bearer minted by the broker.
/// `req` carries scopes for the future scoped broker (ignored by the god-key
/// impl today). Returns `Err(CliError::Auth)` when there's no usable credential.
pub async fn resolve_api_client(
    override_url: Option<&str>,
    req: Option<CredentialRequest>,
) -> Result<ApiClient, CliError> {
    let base = api_base(override_url);
    let cred = get_broker().get(req).await?;
    ApiClient::new(base, Some(cred.access_token))
}

/// Print helper that the broker login flows can pass through to stdout.
pub fn stdout_print(line: &str) {
    println!("{line}");
}

// ── Production-environment enforcement ──────────────────────────────────────
//
// VAIBot is a production product. Self-serve customers must run with EVERY
// component (CLI, guard, plugins, MCP) on production; staging is reserved for
// admin + enterprise accounts. The gate below refuses non-exempt commands when
// the host isn't coherently on production, and points at `vaibot init` to fix
// it. Transitional escape: `VAIBOT_ADMIN_OVERRIDE=1` (until the backend returns
// `admin` on /v2/accounts/me).

/// The env the guard daemon resolved to. Prefers the value a v3 guard publishes in
/// the rendezvous (`guard.json` → `env`); falls back to inferring it from a pre-v3
/// guard's env file (`VAIBOT_POLICY_URL`). `None` when the guard isn't configured /
/// not running and the env file carries no inferable signal.
pub fn guard_pinned_env() -> Option<VaibotEnv> {
    if let Some(e) = crate::services::guard_http::read_guard_env().and_then(|s| VaibotEnv::parse(&s)) {
        return Some(e);
    }
    let contents = std::fs::read_to_string(guard_env_path()).ok()?;
    env_from_guard_env_contents(&contents)
}

/// Pure: derive the guard's env from its env-file contents (`VAIBOT_POLICY_URL`).
fn env_from_guard_env_contents(contents: &str) -> Option<VaibotEnv> {
    contents.lines().find_map(|l| {
        l.trim()
            .strip_prefix("VAIBOT_POLICY_URL=")
            .and_then(|v| env_for_api_url(v.trim().trim_matches('"')))
    })
}

/// Is the transitional admin override set in the environment?
fn admin_override() -> bool {
    std::env::var("VAIBOT_ADMIN_OVERRIDE")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "TRUE"))
        .unwrap_or(false)
}

/// Authoritative exemption check: the transitional override, or an admin /
/// enterprise account (per `/v2/accounts/me`). Network only when no override.
async fn account_is_env_exempt(api_url: Option<&str>) -> bool {
    if admin_override() {
        return true;
    }
    match resolve_api_client(api_url, None).await {
        Ok(client) => matches!(client.me().await, ApiResult::Ok { data, .. } if data.is_env_exempt()),
        Err(_) => false,
    }
}

/// Build an `ApiClient` against the CANONICAL governance base for the current env —
/// the stored slot or canonical default, NEVER an env / `--api-url` override, and
/// not subject to the flag-gate. The §5 admin check uses this so a URL override can
/// never spoof the admin verdict, and there is no `api_base` circularity.
async fn canonical_api_client() -> Result<ApiClient, CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let env = resolve_env(&ProcessEnv, &store);
    let base = governance_base_for_env(&store, env, None);
    let cred = get_broker().get(None).await?;
    ApiClient::new(base, Some(cred.access_token))
}

/// Is the account a server-authoritative admin, verified against the canonical
/// governance host? Honors the transitional `VAIBOT_ADMIN_OVERRIDE` escape.
async fn account_is_admin_canonical() -> bool {
    if admin_override() {
        return true;
    }
    match canonical_api_client().await {
        Ok(client) => matches!(client.me().await, ApiResult::Ok { data, .. } if data.is_admin()),
        Err(_) => false,
    }
}

/// §5 preflight: vet a PRODUCTION URL override before any command sends the bearer.
///
/// Runs for EVERY command (even env-gate-exempt ones like `status`/`account`), so an
/// established prod key can never be diverted by an env var. Fast no-op unless a prod
/// override is actually requested — only then does it make the canonical `/me` call.
///
///   prod override + no flag        → suppressed by the resolver; warn, allow (canonical)
///   prod override + flag + admin   → honored; note, allow
///   prod override + flag + !admin  → REFUSED (a customer can't redirect their prod key)
///   non-production override         → honored (no gate)
pub async fn enforce_url_override_policy(api_url: Option<&str>) -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let env = resolve_env(&ProcessEnv, &store);
    // Deprecated VAIBOT_API_URL is intentionally NOT here — it overrides no base, so
    // it must not trip the override gate (it only drives env inference).
    let requested = api_url
        .map(String::from)
        .or_else(|| std::env::var("VAIBOT_GOVERNANCE_URL").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("VAIBOT_PROVENANCE_URL").ok().filter(|s| !s.is_empty()));
    // Nothing to protect until a prod key exists (init / bootstrap runs override-free).
    let has_key = resolve_credentials(&ProcessEnv, &store).api_key.is_some();
    let allow = url_override_allowed(&ProcessEnv);

    match classify_url_override(env == VaibotEnv::Production, requested.is_some(), has_key, allow) {
        UrlOverridePreflight::Proceed => Ok(()),
        UrlOverridePreflight::SuppressNoFlag => {
            let requested = requested.unwrap_or_default();
            eprintln!(
                "[vaibot] Ignoring production URL override ({requested}); your prod key stays on its\n         canonical host. Set VAIBOT_ALLOW_URL_OVERRIDE=1 (admin accounts only) to redirect it."
            );
            Ok(())
        }
        UrlOverridePreflight::RequireAdmin => {
            let requested = requested.unwrap_or_default();
            // Only here do we make the canonical /me call.
            if account_is_admin_canonical().await {
                eprintln!("[vaibot] Note: production URL override honored ({requested}) — admin account.");
                Ok(())
            } else {
                Err(url_override_refused_error(&requested))
            }
        }
    }
}

/// Pure §5 pre-decision (no network): given the sync facts, does a prod URL override
/// need an admin check, get suppressed by the flag-gate, or is there nothing to do?
/// The network `/me` admin check is made ONLY for `RequireAdmin`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UrlOverridePreflight {
    /// not prod, or no override requested, or no key to protect → nothing to do.
    Proceed,
    /// prod override but no VAIBOT_ALLOW_URL_OVERRIDE → resolver already suppressed it; warn + proceed.
    SuppressNoFlag,
    /// prod override + flag → admit iff the canonical /me says admin.
    RequireAdmin,
}

pub(crate) fn classify_url_override(
    is_prod: bool,
    has_override: bool,
    has_key: bool,
    allow: bool,
) -> UrlOverridePreflight {
    if !is_prod || !has_override || !has_key {
        UrlOverridePreflight::Proceed
    } else if !allow {
        UrlOverridePreflight::SuppressNoFlag
    } else {
        UrlOverridePreflight::RequireAdmin
    }
}

fn url_override_refused_error(requested: &str) -> CliError {
    CliError::Runtime(
        [
            String::new(),
            "  ✗  Production URL override refused.".to_string(),
            String::new(),
            format!("     An override is requested ({requested}) with VAIBOT_ALLOW_URL_OVERRIDE set,"),
            "     but your account is not an admin. A customer can never redirect where their".to_string(),
            "     production key is sent — the override is ignored and the command refused.".to_string(),
            "     Unset the override (and the flag) to proceed on the canonical host.".to_string(),
            String::new(),
        ]
        .join("\n"),
    )
}

/// May a command/setup target this env? Production is always allowed; a
/// non-production env requires an exempt (admin/enterprise/override) account.
pub async fn env_target_allowed(env: VaibotEnv, api_url: Option<&str>) -> bool {
    env == VaibotEnv::Production || account_is_env_exempt(api_url).await
}

/// A human reason for why the CLI resolves to a non-production env (best-effort,
/// for the refusal message). Mirrors `resolve_env`'s precedence.
fn non_prod_reason() -> String {
    if std::env::var("VAIBOT_ENV").map(|v| !v.is_empty()).unwrap_or(false) {
        return "VAIBOT_ENV is set in your shell".to_string();
    }
    match std::env::var("VAIBOT_API_URL") {
        Ok(u) if !u.is_empty() => format!("VAIBOT_API_URL={u} is set in your shell"),
        _ => "your stored active environment is not production".to_string(),
    }
}

/// Preflight gate. Refuses a non-exempt command unless the host is coherently on
/// production — or the account is admin/enterprise (or the override is set).
///
/// Fast path: when the CLI and the guard both resolve to production, returns
/// immediately with NO network call. Only a non-production signal triggers the
/// authoritative `/v2/accounts/me` exemption check.
pub async fn enforce_production_env(
    exempt_command: bool,
    api_url: Option<&str>,
) -> Result<(), CliError> {
    if exempt_command || admin_override() {
        return Ok(());
    }

    let effective = current_env();
    let guard_env = guard_pinned_env();
    let cli_prod = effective == VaibotEnv::Production;
    // An absent guard env file means the guard isn't installed yet — don't block
    // on it (that's `init`'s job, and `init` is exempt anyway).
    let guard_prod = guard_env.map(|e| e == VaibotEnv::Production).unwrap_or(true);
    if cli_prod && guard_prod {
        return Ok(());
    }

    // Something is non-production → only admin/enterprise accounts may proceed.
    let exempt = account_is_env_exempt(api_url).await;
    if exempt {
        eprintln!(
            "[vaibot] Note: operating on the {effective} environment (allowed for admin/enterprise accounts)."
        );
        return Ok(());
    }

    // Refuse — loudly, with the exact drift and the one-command fix.
    let mut lines = vec![
        String::new(),
        "  ✗  VAIBot runs only in the production environment.".to_string(),
        String::new(),
        "     Non-production configuration detected:".to_string(),
    ];
    if !cli_prod {
        lines.push(format!("       • CLI resolves to {effective} — {}", non_prod_reason()));
    }
    if let Some(g) = guard_env {
        if g != VaibotEnv::Production {
            lines.push(format!("       • the guard is pinned to {g}"));
        }
    }
    lines.push(String::new());
    lines.push("     Reconcile every component to production:".to_string());
    lines.push("       vaibot init".to_string());
    lines.push(String::new());
    lines.push("     Staging is reserved for admin + enterprise accounts.".to_string());
    Err(CliError::Runtime(lines.join("\n")))
}

#[cfg(test)]
mod env_gate_tests {
    use super::*;

    #[test]
    fn guard_env_contents_maps_policy_url_to_env() {
        let prod = "VAIBOT_API_KEY=vb_live_x\nVAIBOT_POLICY_URL=https://api.vaibot.io/v2/policy\n";
        let stg = "VAIBOT_POLICY_URL=\"https://staging-api.vaibot.io/v2/policy\"\n";
        assert_eq!(env_from_guard_env_contents(prod), Some(VaibotEnv::Production));
        assert_eq!(env_from_guard_env_contents(stg), Some(VaibotEnv::Staging));
    }

    #[test]
    fn guard_env_contents_none_when_url_absent_or_unknown() {
        assert_eq!(env_from_guard_env_contents("VAIBOT_API_KEY=vb_live_x\n"), None);
        assert_eq!(
            env_from_guard_env_contents("VAIBOT_POLICY_URL=https://example.com/v2/policy\n"),
            None
        );
    }

    // §5 preflight decision — the network-free half, exhaustively. The RequireAdmin
    // arm is the sole protection once the flag is set, so every branch is pinned here.
    #[test]
    fn classify_url_override_covers_every_branch() {
        use UrlOverridePreflight::*;
        // not production → never gated, regardless of override/flag.
        assert_eq!(classify_url_override(false, true, true, true), Proceed);
        assert_eq!(classify_url_override(false, true, true, false), Proceed);
        // production, but no override requested → nothing to do.
        assert_eq!(classify_url_override(true, false, true, true), Proceed);
        // production + override but no key to protect (init/bootstrap) → proceed.
        assert_eq!(classify_url_override(true, true, false, true), Proceed);
        assert_eq!(classify_url_override(true, true, false, false), Proceed);
        // production + override + key, NO flag → suppressed by the resolver; warn.
        assert_eq!(classify_url_override(true, true, true, false), SuppressNoFlag);
        // production + override + key + flag → must check admin (the refuse path).
        assert_eq!(classify_url_override(true, true, true, true), RequireAdmin);
    }

    #[test]
    fn url_override_refused_error_names_the_target_and_is_runtime() {
        let e = url_override_refused_error("https://attacker.example");
        match e {
            CliError::Runtime(msg) => {
                assert!(msg.contains("refused"));
                assert!(msg.contains("https://attacker.example"));
                assert!(msg.contains("not an admin"));
            }
            _ => panic!("expected a Runtime refusal error"),
        }
    }
}
