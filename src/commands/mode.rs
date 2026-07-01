//! `mode` group.
//!   show    [REAL]    — DISPLAY only: the live control-plane mode (/v2/accounts/me)
//!                       vs what the guard enforces now, side by side. No ↵ refresh.
//!   enforce [FORWARD] — open the dashboard for the email-confirmed switch, THEN wait
//!   observe [FORWARD]   on ↵ to force the guard to re-poll + apply it now (Ctrl-C skips).
//!
//! CHANGING the mode stays email-gated on the dashboard. But the RESOLVED
//! `enforcement.effective_mode` is API-key readable via `/v2/accounts/me` (it's the
//! field the guard itself polls), so `show` reads it live — that's the authoritative
//! value the moment you set it. The guard's published value (what agents honor) lags by
//! up to its poll interval; `show` displays both honestly. The ↵-apply lives on
//! `enforce|observe` — right where you initiate a change — not on the passive `show`.

use std::io::{self, IsTerminal, Write};

use clap::Subcommand;
use serde::Deserialize;

use crate::api::{ApiClient, ApiResult};
use crate::config::creds::{api_base_for_env, load_store, resolve_credentials, VaibotEnv};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;

#[derive(Subcommand, Debug)]
pub enum ModeCmd {
    /// Show the governance mode (live control plane + guard-enforced; ↵ to refresh).
    Show,
    /// Switch to enforce mode (opens the dashboard — email-confirmed there).
    Enforce,
    /// Switch to observe mode (opens the dashboard — email-confirmed there).
    Observe,
}

/// The resolved enforcement block of `GET /v2/accounts/me` (API-key accessible).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
struct Enforcement {
    effective_mode: Option<String>,
}

/// Subset of `GET /v2/accounts/me` (API-key accessible).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
struct AccountState {
    claimed: Option<bool>,
    plan: Option<String>,
    enforcement: Option<Enforcement>,
}

pub async fn dispatch(cmd: ModeCmd, api_url: Option<String>) -> Result<(), CliError> {
    match cmd {
        ModeCmd::Show => show(api_url).await,
        ModeCmd::Enforce => forward_to_settings("enforce", api_url).await,
        ModeCmd::Observe => forward_to_settings("observe", api_url).await,
    }
}

/// `vaibot mode show` — DISPLAY only: the live control-plane mode (/v2/accounts/me)
/// vs what the guard is enforcing now, side by side. No refresh trigger — to APPLY a
/// just-made change immediately, use `vaibot mode enforce|observe` (which waits on ↵).
async fn show(api_url: Option<String>) -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    let base = api_base_for_env(resolved.env, api_url.as_deref().or(Some(&resolved.api_base_url)));
    let settings = dashboard_settings_url(resolved.env);

    let account = fetch_account(&base, resolved.api_key.clone()).await;
    let control_plane = control_mode(&account);
    let guard = crate::services::guard_http::read_guard_mode();

    render(&control_plane, &guard, account.as_ref(), &settings);

    // If the guard hasn't reconciled a just-changed account mode, point at the apply path.
    if let (Some(cp), Some(g)) = (control_plane.as_deref(), guard.as_deref()) {
        if cp != g {
            println!("\n  → guard reconciles on its next poll, or run `vaibot mode {cp}` to apply it now.");
        }
    }
    Ok(())
}

/// GET /v2/accounts/me (None on any error — `show` degrades gracefully).
async fn fetch_account(base: &str, api_key: Option<String>) -> Option<AccountState> {
    let client = ApiClient::new(base.to_string(), api_key).ok()?;
    match client.get::<AccountState>("/v2/accounts/me").await {
        ApiResult::Ok { data, .. } => Some(data),
        ApiResult::Err { .. } => None,
    }
}

fn control_mode(account: &Option<AccountState>) -> Option<String> {
    account.as_ref()?.enforcement.as_ref()?.effective_mode.clone()
}

/// Render the mode block: the live control-plane value vs what the guard enforces.
fn render(control_plane: &Option<String>, guard: &Option<String>, account: Option<&AccountState>, settings: &str) {
    println!("\nVAIBot Governance Mode\n");
    match (control_plane.as_deref(), guard.as_deref()) {
        (Some(cp), Some(g)) if cp == g => {
            println!("  effective mode:                 {g}   <- control plane + guard in sync ✓");
        }
        (Some(cp), Some(g)) => {
            println!("  effective mode (guard now):     {g}   <- what agents honor");
            println!("  effective mode (control plane): {cp}   <- the mode you've set (guard not yet reconciled)");
        }
        (None, Some(g)) => {
            println!("  effective mode (guard now):     {g}   <- what agents honor");
            println!("  effective mode (control plane): (couldn't read /v2/accounts/me)");
        }
        (Some(cp), None) => {
            println!("  effective mode (control plane): {cp}   <- authoritative");
            println!("  effective mode (guard now):     unknown (guard not running, or an older guard)");
        }
        (None, None) => println!("  effective mode: unknown (guard down and /v2/accounts/me unreadable)"),
    }
    match std::env::var("VAIBOT_MODE").ok().filter(|s| !s.is_empty()) {
        Some(m) => println!("  VAIBOT_MODE (local fallback):   {m}   (plugins use this only when the guard is unreachable)"),
        None => println!("  VAIBOT_MODE (local fallback):   not set"),
    }
    match account {
        Some(a) => println!(
            "  plan: {}   ·   email claimed: {}",
            a.plan.as_deref().unwrap_or("(unknown)"),
            yes_no(a.claimed)
        ),
        None => println!("  [warn] couldn't read account — `vaibot login` for plan + entitlement."),
    }
    println!("\n  Account / per-key mode is changed (email-confirmed) in the dashboard: {settings}");
}

/// `vaibot mode enforce|observe` — open the dashboard settings page (the email step-up
/// confirmation lives there; we never write the mode locally), then wait on ↵ to force
/// the guard to re-poll and apply the change NOW. Ctrl-C skips the wait (the guard also
/// picks it up on its next poll). Non-interactive: opens the page and returns.
async fn forward_to_settings(target: &str, api_url: Option<String>) -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    let base = api_base_for_env(resolved.env, api_url.as_deref().or(Some(&resolved.api_base_url)));
    let url = dashboard_settings_url(resolved.env);

    println!("\nSwitching to {target} mode requires email confirmation (step-up).");
    println!("Opening the settings page where you confirm it:\n  {url}\n");
    if open::that(&url).is_err() {
        println!("(Could not open a browser automatically — visit the URL above.)");
    }

    if !io::stdin().is_terminal() {
        return Ok(()); // non-interactive: forwarded, nothing to wait on
    }
    println!("Once you've confirmed {target} in the browser, press ↵ here to apply it to the");
    println!("guard now — or Ctrl-C to skip (the guard also picks it up on its next poll).");

    let stdin = io::stdin();
    let mut line = String::new();
    loop {
        print!("  ↵ apply now  ·  Ctrl-C to exit ");
        let _ = io::stdout().flush();
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) => {
                println!();
                break;
            } // EOF (Ctrl-D)
            Ok(_) => {}
            Err(_) => break,
        }
        // Force the guard to re-poll the control plane now.
        let guard = match crate::services::guard_http::refresh_guard_mode().await {
            Ok(m) => Some(m),
            Err(e) => {
                println!("\n  [warn] couldn't reach the guard ({e}).");
                crate::services::guard_http::read_guard_mode()
            }
        };
        let account = fetch_account(&base, resolved.api_key.clone()).await;
        let control_plane = control_mode(&account);
        println!();
        render(&control_plane, &guard, account.as_ref(), &url);
        if guard.as_deref() == Some(target) {
            println!("\n  ✓ {target} is now enforced by the guard.");
            break;
        }
        println!(
            "\n  guard still on {} — confirm {target} in the browser, then ↵ again.",
            guard.as_deref().unwrap_or("unknown")
        );
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
