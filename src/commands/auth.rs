//! `login` [REAL] · `logout` [REAL] · `whoami` [REAL].

use crate::broker::{get_broker, mode_for, AuthSource, EnvOpt, LogoutOptions};
use crate::error::{CliError, ExitCode};
use crate::oauth::LoginOptions;

use super::stdout_print;

/// `vaibot login [--device] [--no-browser] [--api-url <url>]`.
pub async fn login(device: bool, no_browser: bool, api_url: Option<String>) -> Result<(), CliError> {
    let broker = get_broker();
    // Env resolves staging only when --api-url targets staging or VAIBOT_ENV is
    // set; the broker persists against the resolved env. We default to the
    // currently resolved env so a staging session lands in the staging slot.
    let env = super::current_env();
    let opts = LoginOptions {
        mode: mode_for(device),
        no_browser,
        issuer: api_url,
        env,
    };
    let cred = broker.login(opts, &stdout_print).await?;
    // Read back the session from the SAME env we just logged into (VaibotEnv is
    // Copy), so a --api-url/staging login reports the staging identity, not prod.
    let who = broker.whoami(Some(EnvOpt { env: Some(env) })).await.ok().flatten();
    let id = who
        .as_ref()
        .and_then(|w| w.email.clone().or_else(|| Some(w.subject.clone())))
        .unwrap_or_else(|| "your account".into());
    println!("\n✔ Logged in as {id}");
    if let Some(scope) = cred.scope {
        println!("  scope: {scope}");
    }
    Ok(())
}

/// `vaibot logout [--all-hosts]`.
pub async fn logout(all_hosts: bool) -> Result<(), CliError> {
    get_broker()
        .logout(Some(LogoutOptions {
            env: None,
            all_hosts,
        }))
        .await?;
    println!("✔ Logged out (local session cleared).");
    if all_hosts {
        println!(
            "  Note: --all-hosts key revocation is not yet wired (no scoped keys to revoke under the current god-key model)."
        );
    }
    println!("  The guard daemon keeps its own credentials and is unaffected.");
    Ok(())
}

/// `vaibot whoami [--json]`.
pub async fn whoami(json: bool) -> Result<(), CliError> {
    let who = get_broker().whoami(Some(EnvOpt { env: None })).await?;

    if json {
        let body = match &who {
            Some(w) => serde_json::json!({
                "subject": w.subject,
                "email": w.email,
                "scope": w.scope,
                "expiresInSec": finite_or_null(w.expires_in_sec),
                "source": source_str(w.source),
            }),
            None => serde_json::json!({ "loggedIn": false }),
        };
        println!("{}", serde_json::to_string_pretty(&body).unwrap());
        if who.is_none() {
            std::process::exit(ExitCode::Auth as i32);
        }
        return Ok(());
    }

    let Some(w) = who else {
        println!("Not logged in. Run `vaibot login`.");
        std::process::exit(ExitCode::Auth as i32);
    };

    println!("Identity: {}", w.email.as_deref().unwrap_or(&w.subject));
    println!("Source:   {}", source_str(w.source));
    if let Some(scope) = &w.scope {
        println!("Scope:    {scope}");
    }
    if w.expires_in_sec.is_finite() {
        println!("Expires:  in {}s", w.expires_in_sec as i64);
    }
    Ok(())
}

fn source_str(s: AuthSource) -> &'static str {
    match s {
        AuthSource::OAuth => "oauth",
        AuthSource::ApiKey => "api_key",
    }
}

fn finite_or_null(v: f64) -> serde_json::Value {
    if v.is_finite() {
        serde_json::json!(v)
    } else {
        serde_json::Value::Null
    }
}
