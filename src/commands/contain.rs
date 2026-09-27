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
            println!("\n         Lift it with `vaibot release` — your signed-in session plus an");
    println!("         emailed code, or `--recovery-code` if you cannot reach that inbox.\n");
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

/// `vaibot release` — lift containment: a signed-in session plus a second factor.
///
/// Deliberately NOT gated on the platform superuser flag. `admin` in this system
/// is a platform-wide flag, not an account role — requiring it here would have
/// guaranteed a lockout, because any account could arm containment and then never
/// lift it. The asymmetry that matters is carried by the two things that do: a
/// session rather than an api key, plus a second factor.
pub async fn release(recovery_code: Option<String>, api_url: Option<String>) -> Result<(), CliError> {
    // Validate before any I/O: a blank code should fail instantly rather than after
    // a config read and a token refresh, and it keeps this checkable without a server.
    let recovery_code = match recovery_code.as_deref().map(str::trim) {
        Some("") => return Err(CliError::Runtime(blank_recovery_code_message())),
        other => other,
    };

    let client = resolve_api_client(api_url.as_deref(), None).await?;

    // Break-glass first. A recovery code exists precisely because the emailed
    // factor is unreachable, so it must not route through any of that machinery.
    if let Some(code) = recovery_code {
        return match client.release_with_recovery_code(code).await {
            ApiResult::Ok { data, .. } => {
                report_released(&data);
                Ok(())
            }
            ApiResult::Err { status: 403, error } if error.contains("session") => Err(CliError::Runtime(
                "Lifting containment needs a signed-in session, not an API key.\n       Run `vaibot login` first, or lift it from the dashboard."
                    .into(),
            )),
            ApiResult::Err { status: 403, error } if error.contains("recovery") => Err(CliError::Runtime(
                "That recovery code is not valid, or has already been used.\n       Each code works once. Try another from the set you saved."
                    .into(),
            )),
            ApiResult::Err { error, status } => Err(CliError::Runtime(format!(
                "Could not release with that recovery code (HTTP {status}): {error}"
            ))),
        };
    }

    // Try first: if nothing is contained, say so without emailing anybody. If a
    // step-up window is already open, this also succeeds outright.
    match client.release().await {
        ApiResult::Ok { data, .. } => {
            report_released(&data);
            return Ok(());
        }
        // An api key cannot lift containment: that is the credential
        // agent-adjacent code holds, and containment exists to stop a
        // misbehaving agent. Arming with a key is fine; lifting is not.
        ApiResult::Err { status: 403, error } if error.contains("session") => {
            return Err(CliError::Runtime(
                "Lifting containment needs a signed-in session, not an API key.\n       Run `vaibot login` first, or lift it from the dashboard."
                    .into(),
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
            return Err(CliError::Runtime(
                "Incorrect or expired code — re-run to get a new one.\n       If you cannot reach that inbox, use `vaibot release --recovery-code <CODE>`."
                    .into(),
            ))
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
        ApiResult::Ok { data, .. } => {
                report_released(&data);
                Ok(())
            }
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

fn blank_recovery_code_message() -> String {
    "--recovery-code was empty. Pass one of the codes you saved, or drop the flag to use the emailed factor instead."
        .into()
}

fn describe_since(at: Option<&str>) -> String {
    match at {
        Some(t) => format!(" (since {t})"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `release` is mostly orchestration over HTTP, so what is worth pinning here is
    // the part that must hold with no server reachable at all: the argument check,
    // and the wording a stopped operator reads.

    #[tokio::test]
    async fn a_blank_recovery_code_fails_before_any_network_call() {
        // An unroutable api-url proves the point: if this touched the network it
        // would fail with a connection error, not the argument message.
        for blank in ["", "   ", "\t"] {
            let err = release(Some(blank.to_string()), Some("http://127.0.0.1:1".into()))
                .await
                .expect_err("a blank code must be refused");
            let msg = err.to_string();
            assert!(msg.contains("--recovery-code was empty"), "got: {msg}");
        }
    }

    #[test]
    fn the_blank_code_message_offers_the_way_out() {
        // Someone reading this has every agent stopped. Saying what is wrong is not
        // enough; it has to say what to do instead.
        let m = blank_recovery_code_message();
        assert!(m.contains("codes you saved"), "should point at the saved set: {m}");
        assert!(m.contains("emailed factor"), "should offer the other path: {m}");
    }

    #[test]
    fn describe_since_is_empty_when_the_server_sent_no_timestamp() {
        // Forward compatibility: an older or newer API may omit contained_at, and a
        // panic-button command must not render "(since )" at a person.
        assert_eq!(describe_since(None), "");
        assert_eq!(describe_since(Some("2026-09-27T00:00:00Z")), " (since 2026-09-27T00:00:00Z)");
    }
}
