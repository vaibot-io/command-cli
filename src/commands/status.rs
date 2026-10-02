//! `status [--json]` [REAL]. GET /v2/health + /v2/accounts/me (joined). The
//! --json model is canonical + never throws so the orchestrator can consume it.

use crate::api::{ApiClient, ApiResult};
use crate::broker::{get_broker, AuthSource};
use crate::config::creds::{api_base_for_env, load_store, resolve_credentials};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;
use crate::ui;

/// `vaibot status [--json]`.
pub async fn run(json: bool, api_url: Option<String>) -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    let base = api_base_for_env(resolved.env, api_url.as_deref().or(Some(&resolved.api_base_url)));

    if json {
        // Best-effort, never throws.
        let who = get_broker().whoami(None).await.ok().flatten();
        let state = serde_json::json!({
            "env": resolved.env.to_string(),
            "apiBaseUrl": base,
            "hasApiKey": resolved.api_key.is_some(),
            "keyMismatch": resolved.key_mismatch,
            "loggedIn": who.is_some(),
            "identity": who.as_ref().map(|w| w.email.clone().unwrap_or_else(|| w.subject.clone())),
            "authSource": who.as_ref().map(|w| match w.source {
                AuthSource::OAuth => "oauth",
                AuthSource::ApiKey => "api_key",
            }),
        });
        println!("{}", serde_json::to_string_pretty(&state).unwrap());
        return Ok(());
    }

    let client = ApiClient::new(base.clone(), resolved.api_key.clone())?;
    // Health + /me concurrently.
    let (health, me) = tokio::join!(client.health(), async {
        if resolved.api_key.is_some() {
            Some(client.me().await)
        } else {
            None
        }
    });

    // Build every block first so they can share one value column — sections that
    // each align to their own longest label leave the page visibly ragged.

    // Connection — is the control plane there at all?
    let (health_state, health_label) = if health.is_ok() {
        (ui::State::Good, "reachable")
    } else {
        (ui::State::Bad, "unreachable")
    };
    let mut conn = ui::Rows::new();
    conn.push(
        "API",
        format!(
            "{base}    {} {}",
            ui::mark_state(health_state),
            health_state.label(health_label)
        ),
    );

    // Identity — who this machine is acting as.
    let mut ident = ui::Rows::new();
    if let Some(ApiResult::Ok { data, .. }) = &me {
        if let Some(email) = &data.email {
            let tag = if data.claimed {
                String::new()
            } else {
                format!("  {}", ui::State::Attention.label("(unclaimed)"))
            };
            ident.push("Account", format!("{email}{tag}"));
        }
    }
    match &resolved.api_key {
        Some(k) => ident.push(
            "API key",
            format!("{}{}", k.chars().take(14).collect::<String>(), ui::ellipsis()),
        ),
        None => ident.push("API key", ui::State::Attention.label("not set")),
    };

    // Usage — how much of the plan is left.
    let mut usage = ui::Rows::new();
    if let Some(ApiResult::Ok { data, .. }) = &me {
        let frac = if data.quota.limit > 0 {
            data.quota.used as f64 / data.quota.limit as f64
        } else {
            0.0
        };
        let pct = (frac * 100.0).round() as u64;
        usage.push(
            "Decisions",
            format!(
                "{} / {}  {}  {}",
                ui::num(data.quota.used as u64),
                ui::num(data.quota.limit as u64),
                ui::sep(),
                data.quota.month
            ),
        );
        usage.push_continuation(format!(
            "{}  {pct}%  {}  {} remaining",
            ui::bar(frac, 20),
            ui::sep(),
            ui::num(data.quota.remaining as u64)
        ));
    }

    // One column across every section, derived from the widest label anywhere.
    let width = ui::shared_width(&[&conn, &ident, &usage]);
    conn.set_min_width(width);
    ident.set_min_width(width);
    usage.set_min_width(width);

    ui::header("VAIBot", Some(&resolved.env.to_string()));

    ui::section("connection");
    conn.render();

    println!();
    ui::section("identity");
    ident.render();
    if resolved.api_key.is_none() {
        ui::step("vaibot init");
    }

    if !usage.is_empty() {
        println!();
        ui::section("usage");
        usage.render();
    } else if let Some(ApiResult::Err { error, .. }) = &me {
        println!();
        println!(
            "  {}  Could not fetch account details: {error}",
            ui::mark_warn()
        );
    }

    println!();
    Ok(())
}
