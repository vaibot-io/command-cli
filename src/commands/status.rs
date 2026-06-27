//! `status [--json]` [REAL]. GET /v2/health + /v2/accounts/me (joined). The
//! --json model is canonical + never throws so the orchestrator can consume it.

use crate::api::{ApiClient, ApiResult};
use crate::broker::{get_broker, AuthSource};
use crate::config::creds::{api_base_for_env, load_store, resolve_credentials};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;

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

    println!("\n  VAIBot Status\n");

    let client = ApiClient::new(base.clone(), resolved.api_key.clone())?;
    // Health + /me concurrently.
    let (health, me) = tokio::join!(client.health(), async {
        if resolved.api_key.is_some() {
            Some(client.me().await)
        } else {
            None
        }
    });

    let health_label = if health.is_ok() {
        "reachable"
    } else {
        "unreachable"
    };
    println!("  Env         {}", resolved.env);
    println!("  API         {base}  {health_label}");

    match &resolved.api_key {
        Some(k) => println!("  API key     {}...", k.chars().take(14).collect::<String>()),
        None => println!("  API key     not set  — run `vaibot init`"),
    }

    if let Some(ApiResult::Ok { data, .. }) = me {
        if let Some(email) = &data.email {
            let tag = if data.claimed { "" } else { "  (unclaimed)" };
            println!("  User        {email}{tag}");
        }
        let pct = if data.quota.limit > 0 {
            (data.quota.used * 100 / data.quota.limit).clamp(0, 100)
        } else {
            0
        };
        println!(
            "  Quota       {} / {} decisions  {}",
            data.quota.used, data.quota.limit, data.quota.month
        );
        println!("              {pct}%  ({} remaining)", data.quota.remaining);
    } else if let Some(ApiResult::Err { error, .. }) = me {
        println!("\n  [warn] Could not fetch account details: {error}");
    }

    println!();
    Ok(())
}
