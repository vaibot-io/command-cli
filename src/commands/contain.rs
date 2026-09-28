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
                println!("\n  [ok]   CONTAINED — every agent on this account is stopped, on every machine.");
                println!("         Guards apply this within about a second. Any that are offline");
                println!("         apply it the moment they reconnect.");
                if let Some(r) = data.reason.as_deref() {
                    println!("         Reason: {r}");
                }
            }
            println!("\n         To lift it, run `vaibot release`. You'll confirm with a code we");
    println!("         email you — or use `--recovery-code` if you can't reach that inbox.\n");
            Ok(())
        }
        ApiResult::Err { status: 401, .. } => Err(CliError::Runtime(
            "You need to be signed in. Run `vaibot login`, then try again.".into(),
        )),
        ApiResult::Err { error, status } => Err(CliError::Runtime(format!(
            "Couldn't apply containment (HTTP {status}): {error}"
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
                "Releasing needs you signed in, not an API key.\n       Run `vaibot login`, or release it from the dashboard."
                    .into(),
            )),
            ApiResult::Err { status: 403, error } if error.contains("recovery") => Err(CliError::Runtime(
                "That recovery code isn't valid, or it's already been used.\n       Each code works once — try another from the set you saved."
                    .into(),
            )),
            ApiResult::Err { error, status } => Err(CliError::Runtime(format!(
                "Couldn't release with that recovery code (HTTP {status}): {error}"
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
                "Releasing needs you signed in, not an API key.\n       Run `vaibot login`, or release it from the dashboard."
                    .into(),
            ))
        }
        // stepup_required — expected on the first call; fall through and walk it.
        ApiResult::Err { status: 403, .. } => {}
        ApiResult::Err { status: 501, .. } => {
            return Err(CliError::Runtime(
                "Confirm with the emailed code instead — run `vaibot release` on its own.".into(),
            ))
        }
        ApiResult::Err { error, status } => {
            return Err(CliError::Runtime(format!("Couldn't release (HTTP {status}): {error}")))
        }
    }

    println!("\nVAIBot — release containment\n");
    let activate = match client.policy_stepup_activate("containment_release").await {
        ApiResult::Ok { data, .. } => data,
        ApiResult::Err { status: 409, .. } => {
            println!("  [warn] We need an email address on file to send your confirmation code.");
            println!("         Add one with `vaibot login` or in the dashboard, then run this again.");
            return Err(CliError::Runtime(
                "no email on file to send the confirmation code to".into(),
            ));
        }
        ApiResult::Err { error, status } => {
            return Err(CliError::Runtime(format!("Couldn't send the confirmation code (HTTP {status}): {error}")))
        }
    };
    let Some(token) = activate.token else {
        return Err(CliError::Runtime(
            "Couldn't start email confirmation. Try `vaibot release` again in a moment.".into(),
        ));
    };
    println!(
        "  ▸ We emailed a confirmation code to {}. It expires in about 15 minutes.",
        activate.sent_to.as_deref().unwrap_or("your email")
    );

    let code = prompt_line("  Paste the code: ")?;
    let code = code.trim();
    if code.is_empty() {
        println!("  No code entered. Still contained — run `vaibot release` again when you have it.");
        return Ok(());
    }

    match client.policy_stepup_verify(&token, code).await {
        ApiResult::Ok { .. } => {}
        ApiResult::Err { status: 400, .. } => {
            return Err(CliError::Runtime(
                "That code didn't match, or it expired. Run `vaibot release` again for a new one.\n       Can't reach that inbox? Use `vaibot release --recovery-code <CODE>`."
                    .into(),
            ))
        }
        ApiResult::Err { status: 429, .. } => {
            return Err(CliError::Runtime("Too many attempts. Wait a minute, then run `vaibot release` again.".into()))
        }
        ApiResult::Err { error, status } => {
            return Err(CliError::Runtime(format!("Couldn't confirm that code (HTTP {status}): {error}")))
        }
    }

    // The window is open; spend it.
    match client.release().await {
        ApiResult::Ok { data, .. } => {
                report_released(&data);
                Ok(())
            }
        ApiResult::Err { error, status } => Err(CliError::Runtime(format!(
            "Your code was accepted, but the release didn't go through (HTTP {status}): {error}.\n       Run `vaibot release` again for a fresh code."
        ))),
    }
}

fn report_released(data: &crate::api::enforcement::ReleaseResponse) {
    if data.already {
        println!("\n  [ok]   Nothing to lift — this account isn't contained.\n");
        return;
    }
    println!("\n  [ok]   RELEASED — your agents are governed by your policy again.");
    if let Some(f) = data.release_factor.as_deref() {
        println!("         Confirmed by: {f}");
    }
    println!("         Guards pick this up within about a second.\n");
}

fn blank_recovery_code_message() -> String {
    "--recovery-code needs a value. Pass one of the codes you saved, or leave the flag off to confirm by email instead."
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
            assert!(msg.contains("--recovery-code needs a value"), "got: {msg}");
        }
    }

    #[test]
    fn the_blank_code_message_offers_the_way_out() {
        // Someone reading this has every agent stopped. Saying what is wrong is not
        // enough; it has to say what to do instead.
        let m = blank_recovery_code_message();
        assert!(m.contains("codes you saved"), "should point at the saved set: {m}");
        assert!(m.contains("confirm by email"), "should offer the other path: {m}");
    }

    #[test]
    fn describe_since_is_empty_when_the_server_sent_no_timestamp() {
        // Forward compatibility: an older or newer API may omit contained_at, and a
        // panic-button command must not render "(since )" at a person.
        assert_eq!(describe_since(None), "");
        assert_eq!(describe_since(Some("2026-09-27T00:00:00Z")), " (since 2026-09-27T00:00:00Z)");
    }
}
