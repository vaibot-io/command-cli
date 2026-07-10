//! vaibot-cli — the VAIBot front-door CLI library surface.
//!
//! INSTALLER-NOT-RUNTIME: this crate orchestrates the separate guard and gateway
//! daemons; it never embeds them. `guard serve` / `gateway serve` locate a
//! standalone binary and exec it (signal-forwarded), or print the
//! separate-binary model and exit non-zero. No daemon ever runs in-process.
//!
//! The composition root for credentials is `broker::get_broker()` — swapping the
//! god-key `FileCredentialBroker` for the `ScopedCredentialBroker` there is the
//! entire least-privilege migration (zero call-site churn).

pub mod api;
pub mod broker;
pub mod cli;
pub mod commands;
pub mod component;
pub mod config;
pub mod error;
pub mod oauth;
pub mod policy;
pub mod services;

use cli::{Cli, Command};
use error::CliError;

/// Commands exempt from the production-environment gate — the ones you need to
/// diagnose or FIX a non-production / split-brain host (and `init`, the blessed
/// reconcile path). Everything else refuses to run outside production.
fn is_env_gate_exempt(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Login { .. }
            | Command::Logout { .. }
            | Command::Whoami { .. }
            | Command::Account { .. }
            | Command::Init { .. }
            | Command::Status { .. }
            | Command::Doctor { .. }
            | Command::Update
    )
}

/// Dispatch a parsed `Cli` to its handler. Returns the command's `Result` so the
/// binary can map it to the documented exit code.
pub async fn dispatch(cli: Cli) -> Result<(), CliError> {
    let api_url = cli.api_url.clone();
    // §5: vet any PRODUCTION url override first — runs for every command (even the
    // env-gate-exempt ones) so an established prod key can't be diverted by an env var.
    commands::enforce_url_override_policy(api_url.as_deref()).await?;
    // Production-only gate (admin/enterprise + VAIBOT_ADMIN_OVERRIDE exempt).
    commands::enforce_production_env(is_env_gate_exempt(&cli.command), api_url.as_deref()).await?;

    // Auto-update check (non-blocking, skip if VAIBOT_NO_UPDATE_CHECK is set or update command).
    // check_and_notify_update owns its own time budget (and cache fallback), so no
    // outer timeout is layered here.
    if !matches!(cli.command, Command::Update) && std::env::var("VAIBOT_NO_UPDATE_CHECK").is_err() {
        if let Some(latest) = services::updater::check_and_notify_update().await {
            services::updater::show_update_notification(&latest);
        }
    }

    match cli.command {
        // ── auth ──
        Command::Login { device, no_browser } => commands::auth::login(device, no_browser, api_url).await,
        Command::Logout { all_hosts } => commands::auth::logout(all_hosts).await,
        Command::Whoami { json } => commands::auth::whoami(json).await,
        Command::Account { cmd } => commands::account::dispatch(cmd, api_url).await,

        // ── lifecycle ──
        Command::Init {
            yes,
            env,
            api_key,
            skip_login,
            with_gateway,
            with_mcp,
            preset,
        } => {
            commands::setup::init(
                yes,
                env,
                api_key,
                skip_login,
                with_gateway,
                with_mcp,
                preset,
                api_url,
            )
            .await
        }
        Command::Status { json } => commands::status::run(json, api_url).await,
        Command::Doctor { fix } => commands::setup::doctor(fix).await,
        Command::Update => commands::setup::update().await,

        // ── component groups ──
        Command::Guard { cmd } => commands::guard::dispatch(cmd).await,
        Command::Gateway { cmd } => commands::gateway::dispatch(cmd).await,
        Command::Plugin { cmd } => commands::plugin::dispatch(cmd).await,
        Command::Policy { cmd } => commands::policy::dispatch(cmd, api_url).await,
        Command::Mode { cmd } => commands::mode::dispatch(cmd, api_url).await,
        Command::Mcp { cmd } => commands::mcp::dispatch(cmd, api_url),
        Command::Provenance { cmd } => commands::provenance::dispatch(cmd, api_url).await,
        // alias: `receipts` → the SAME ProvenanceCmd dispatch.
        Command::Receipts { cmd } => commands::provenance::dispatch(cmd, api_url).await,
    }
}
