//! `mode` group.
//!   show    [REAL]    — local VAIBOT_MODE override + plan/email (from /v2/accounts/me),
//!                       and a pointer to the dashboard for the account/per-key mode.
//!   enforce [FORWARD] — open the dashboard settings page; the email step-up
//!   observe [FORWARD]   (paid-plan + claimed-email confirmation) runs there.
//!
//! The authoritative account/per-key enforcement mode is a sensitive, email-gated
//! server setting and its read endpoint (`/v2/enforcement/state`) is dashboard-
//! session only — not API-key readable. So changes forward to the settings page
//! (where the web2 EnforcementCard flow lives) and `show` surfaces what the API
//! key CAN see + points at the dashboard for the rest.

use clap::Subcommand;
use serde::Deserialize;

use crate::api::{ApiClient, ApiResult};
use crate::config::creds::{api_base_for_env, load_store, resolve_credentials, VaibotEnv};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;

#[derive(Subcommand, Debug)]
pub enum ModeCmd {
    /// Show the governance mode (local override + plan; dashboard for the rest).
    Show,
    /// Switch to enforce mode (opens the dashboard — email-confirmed there).
    Enforce,
    /// Switch to observe mode (opens the dashboard — email-confirmed there).
    Observe,
}

/// Subset of `GET /v2/accounts/me` (API-key accessible).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AccountState {
    claimed: Option<bool>,
    plan: Option<String>,
}

pub async fn dispatch(cmd: ModeCmd, api_url: Option<String>) -> Result<(), CliError> {
    match cmd {
        ModeCmd::Show => show(api_url).await,
        ModeCmd::Enforce => forward_to_settings("enforce"),
        ModeCmd::Observe => forward_to_settings("observe"),
    }
}

/// `vaibot mode show` — what the API key can see + a pointer to the dashboard.
async fn show(api_url: Option<String>) -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    let base = api_base_for_env(resolved.env, api_url.as_deref().or(Some(&resolved.api_base_url)));
    let client = ApiClient::new(base, resolved.api_key.clone())?;
    let settings = dashboard_settings_url(resolved.env);

    println!("\nVAIBot Governance Mode\n");

    // The local VAIBOT_MODE env override wins wherever it's set (guard + plugins).
    match std::env::var("VAIBOT_MODE").ok().filter(|s| !s.is_empty()) {
        Some(m) => println!("  VAIBOT_MODE (local override): {m}  — wins wherever this env is set"),
        None => println!("  VAIBOT_MODE (local override): not set"),
    }

    // Plan + claimed-email — enforce requires a paid plan AND a claimed email.
    match client.get::<AccountState>("/v2/accounts/me").await {
        ApiResult::Ok { data, .. } => {
            println!("  plan:                         {}", data.plan.as_deref().unwrap_or("(unknown)"));
            println!("  email claimed:                {}", yes_no(data.claimed));
        }
        ApiResult::Err { status, error } => {
            println!("  [warn] couldn't read account (HTTP {status}): {error}");
            println!("         log in with `vaibot login` for plan + entitlement.");
        }
    }

    println!("\n  Your account / per-key enforcement mode is managed in the dashboard");
    println!("  (reading + changing it is email-confirmed): {settings}");
    println!();
    Ok(())
}

/// `vaibot mode enforce|observe` — forward to the dashboard settings page, where
/// the email step-up confirmation already lives. We never write the mode locally.
fn forward_to_settings(target: &str) -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    let url = dashboard_settings_url(resolved.env);

    println!("\nSwitching to {target} mode requires email confirmation (step-up).");
    println!("Opening the settings page where you can confirm it:\n  {url}\n");
    if open::that(&url).is_err() {
        println!("(Could not open a browser automatically — visit the URL above.)");
    }
    Ok(())
}

/// The dashboard settings URL: `$VAIBOT_DASHBOARD_URL` override → per-env default.
fn dashboard_settings_url(env: VaibotEnv) -> String {
    if let Ok(o) = std::env::var("VAIBOT_DASHBOARD_URL") {
        if !o.is_empty() {
            return format!("{}/settings", o.trim_end_matches('/'));
        }
    }
    let base = match env {
        VaibotEnv::Production => "https://www.vaibot.io",
        VaibotEnv::Staging => "https://staging.vaibot.io",
    };
    format!("{base}/settings")
}

fn yes_no(b: Option<bool>) -> &'static str {
    match b {
        Some(true) => "yes",
        Some(false) => "no",
        None => "(unknown)",
    }
}
