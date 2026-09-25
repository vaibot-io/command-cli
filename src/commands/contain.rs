//! `vaibot contain` / `vaibot release` — the Tier-0 panic switch.
//!
//! Containment stops every agent on this account, on every machine, before
//! policy or classifier runs — and it holds even in observe mode. Guards learn
//! about it by being pushed to, so it lands in about a second rather than at
//! the next poll.
//!
//! The two directions are not symmetric, and the UX says so. `contain` is one
//! command with no confirmation: hesitating is the expensive mistake, and a
//! false arm is undone in a minute. `release` walks an emailed step-up, because
//! it re-enables everything.

use crate::api::ApiResult;
use crate::error::CliError;

use super::policy::prompt_line;
use super::resolve_api_client;

/// `vaibot contain [--reason "..."]` — stop every agent on this account.
pub async fn contain(reason: Option<String>, api_url: Option<String>) -> Result<(), CliError> {
    let client = resolve_api_client(api_url.as_deref(), None).await?;

    match client.contain(reason.as_deref()).await {
        ApiResult::Ok { data, .. } => {
            if data.already {
                println!("\n  [ok]   Already contained{}.", describe_since(data.contained_at.as_deref()));
                if let Some(r) = data.reason.as_deref() {
                    println!("         Reason on record: {r}");
                }
            } else {
                println!("\n  [ok]   CONTAINED — every agent on this account is stopped.");
                println!("         Guards pick this up in about a second; ones that are offline");
                println!("         pick it up the moment they reconnect.");
                if let Some(r) = data.reason.as_deref() {
                    println!("         Reason: {r}");
                }
            }
            println!("\n         Lift it with `vaibot release` (admin + emailed code).\n");
            Ok(())
        }
        ApiResult::Err { status: 401, .. } => Err(CliError::Runtime(
            "Not signed in. Run `vaibot login` first — containment needs your account.".into(),
        )),
        ApiResult::Err { error, status } => Err(CliError::Runtime(format!(
            "Could not contain (HTTP {status}): {error}"
        ))),
    }
}

/// `vaibot release` — lift containment. Admin, plus an emailed code.
pub async fn release(api_url: Option<String>) -> Result<(), CliError> {
    let client = resolve_api_client(api_url.as_deref(), None).await?;

    // Try first: if nothing is contained, say so without emailing anybody. If a
    // step-up window is already open, this also succeeds outright.
    match client.release().await {
        ApiResult::Ok { data, .. } => return Ok(report_released(&data)),
        // An api key cannot lift containment: that is the credential
        // agent-adjacent code holds, and containment exists to stop a
        // misbehaving agent. Arming with a key is fine; lifting is not.
        ApiResult::Err { status: 403, error } if error.contains("session") => {
            return Err(CliError::Runtime(
                "Lifting containment needs a signed-in session, not an API key.\n       Run `vaibot login` first, or lift it from the dashboard."
                    .into(),
            ))
        }
        ApiResult::Err { status: 403, error } if error.contains("admin") => {
            return Err(CliError::Runtime(
                "Lifting containment re-enables every agent on this account, so it takes an administrator.".into(),
            ))
        }
        // stepup_required — expected on the first call; fall through and walk it.
        ApiResult::Err { status: 403, .. } => {}
        ApiResult::Err { status: 501, .. } => {
            return Err(CliError::Runtime(
                "Mobile device-key release is not enabled yet. Use the emailed code.".into(),
            ))
        }
        ApiResult::Err { error, status } => {
            return Err(CliError::Runtime(format!("Could not release (HTTP {status}): {error}")))
        }
    }

    println!("\nVAIBot — release containment (email step-up)\n");
    let activate = match client.policy_stepup_activate("containment_release").await {
        ApiResult::Ok { data, .. } => data,
        ApiResult::Err { status: 409, .. } => {
            println!("  [fail] This needs a claimed email — we email you a code to confirm.");
            println!("         Claim one first (`vaibot login` or the dashboard), then retry.");
            return Err(CliError::Runtime("email unclaimed".into()));
        }
        ApiResult::Err { error, status } => {
            return Err(CliError::Runtime(format!("Step-up failed (HTTP {status}): {error}")))
        }
    };
    let Some(token) = activate.token else {
        return Err(CliError::Runtime("server did not return a step-up token".into()));
    };
    println!(
        "  ▸ Emailed a confirmation code to {} (expires in ~15 min).",
        activate.sent_to.as_deref().unwrap_or("your email")
    );

    let code = prompt_line("  Paste the code: ")?;
    let code = code.trim();
    if code.is_empty() {
        println!("  No code entered — still contained. Re-run when you have it.");
        return Ok(());
    }

    match client.policy_stepup_verify(&token, code).await {
        ApiResult::Ok { .. } => {}
        ApiResult::Err { status: 400, .. } => {
            return Err(CliError::Runtime("Incorrect or expired code — re-run to get a new one.".into()))
        }
        ApiResult::Err { status: 429, .. } => {
            return Err(CliError::Runtime("Too many attempts — wait a moment, then re-run.".into()))
        }
        ApiResult::Err { error, status } => {
            return Err(CliError::Runtime(format!("Verify failed (HTTP {status}): {error}")))
        }
    }

    // The window is open; spend it.
    match client.release().await {
        ApiResult::Ok { data, .. } => Ok(report_released(&data)),
        ApiResult::Err { error, status } => Err(CliError::Runtime(format!(
            "Code accepted but the release did not apply (HTTP {status}): {error}"
        ))),
    }
}

fn report_released(data: &crate::api::enforcement::ReleaseResponse) {
    if data.already {
        println!("\n  [ok]   Nothing to lift — this account is not contained.\n");
        return;
    }
    println!("\n  [ok]   RELEASED — agents return to your policy.");
    if let Some(f) = data.release_factor.as_deref() {
        println!("         Confirmed by: {f}");
    }
    println!("         Guards resume in about a second.\n");
}

fn describe_since(at: Option<&str>) -> String {
    match at {
        Some(t) => format!(" (since {t})"),
        None => String::new(),
    }
}
