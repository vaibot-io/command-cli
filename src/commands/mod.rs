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
use crate::config::creds::{api_base_for_env, env_for_api_url, load_store, resolve_env, VaibotEnv};
use crate::config::{credentials_path, guard_env_path, ProcessEnv};
use crate::error::CliError;

/// Resolve the env the CLI is operating in.
pub fn current_env() -> VaibotEnv {
    let store = load_store(&credentials_path(&ProcessEnv));
    resolve_env(&ProcessEnv, &store)
}

/// Resolve the API base URL for the current env (honoring VAIBOT_API_URL +
/// --api-url override).
pub fn api_base(override_url: Option<&str>) -> String {
    let env = current_env();
    let from_env = std::env::var("VAIBOT_API_URL").ok();
    let chosen = override_url.map(String::from).or(from_env);
    api_base_for_env(env, chosen.as_deref())
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

/// The env the guard daemon is pinned to, read from its env file's
/// `VAIBOT_POLICY_URL`. `None` when the guard isn't configured yet.
pub fn guard_pinned_env() -> Option<VaibotEnv> {
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
}
