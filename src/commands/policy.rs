//! `policy` group — the governed denylist on top of your preset floor.
//!
//! Final model (see docs/policy-lock-spec.md + the locked command list):
//! three lanes (allow/ask/deny), a two-tier system floor + a true user-floor
//! preset, and an append-only signed/anchored bundle.
//!
//!   show     [REAL] — active signed policy from the local guard.
//!   history  [REAL] — GET /v2/policy/history.
//!   deny     [REAL] — POST /v2/policy/request (add denial(s); signed server-side).
//!   allow    [REAL] — remove your additions via POST /v2/policy/apply.
//!   edit     [REAL] — bulk declarative edit ($EDITOR or -f file) via /v2/policy/apply.
//!   preset   [REAL] — show/set your floor via POST /v2/policy/preset.
//!   lock     [REAL] — freeze; email step-up (OTP) via POST /v2/policy/stepup/*.
//!   unlock   [REAL] — open a 30-min window / --permanent remove; email step-up.
//!
//! Hidden transitional helpers, kept so nothing regresses until `edit`/`allow`
//! land, then removed:
//!   pull / diff  (fold into `edit`)  ·  revoke  (coarse rollback; → `allow`).

use clap::Subcommand;

use crate::api::{ApiClient, ApiResult};
use crate::error::CliError;
use crate::policy::Policy;
use crate::services::guard_http;

use super::resolve_api_client;

#[derive(Subcommand, Debug)]
pub enum PolicyCmd {
    /// Show the active policy: floor, your additions, lock state.
    Show,
    /// Show the audited policy change log.
    History,
    /// Show or set your governance floor (permissive | balanced | strict).
    Preset {
        /// The floor to set. Omit to show the current floor + options.
        flavor: Option<String>,
    },
    /// Add denial(s) on top of your floor.
    Deny {
        /// One or more tool/command patterns to deny.
        #[arg(required = true, num_args = 1..)]
        patterns: Vec<String>,
    },
    /// Remove denial(s) you added (never below your floor).
    Allow {
        /// One or more patterns to remove from your additions.
        #[arg(required = true, num_args = 1..)]
        patterns: Vec<String>,
    },
    /// Bulk-edit your additions declaratively (your floor stays read-only).
    Edit {
        /// Apply from a YAML file instead of opening $EDITOR.
        #[arg(short = 'f', long = "file")]
        file: Option<String>,
        /// Preview the change without applying it.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Freeze the policy — changes then require email confirmation.
    Lock,
    /// Open a 30-minute change window (or --permanent to remove the lock).
    Unlock {
        /// Remove the lock entirely instead of opening a window.
        #[arg(long = "permanent")]
        permanent: bool,
    },

    /// (transitional) Write the active policy to a local YAML working copy.
    #[command(hide = true)]
    Pull {
        /// Output path.
        #[arg(short = 'o', long = "out", default_value = "vaibot-policy.yaml")]
        out: String,
    },
    /// (transitional) Diff a local YAML policy file against the active policy.
    #[command(hide = true)]
    Diff {
        /// Path to the policy YAML file.
        file: String,
    },
    /// (transitional) Coarse one-version rollback — superseded by `allow`.
    #[command(hide = true)]
    Revoke,

    /// (operator) Generate the VAIBot policy SIGNING keypair for a backend env.
    /// Self-host / control-plane provisioning only — NOT a customer step. Customers
    /// never hold a signing key; VAIBot signs bundles server-side.
    #[command(hide = true)]
    Keygen {
        /// Backend env to provision: staging | production.
        #[arg(long, default_value = "staging")]
        env: String,
    },
}

pub async fn dispatch(cmd: PolicyCmd, api_url: Option<String>) -> Result<(), CliError> {
    match cmd {
        PolicyCmd::Show => guard_http::run_policy_list().await,
        PolicyCmd::History => history(api_url).await,
        PolicyCmd::Deny { patterns } => deny(patterns, api_url).await,
        PolicyCmd::Allow { patterns } => allow(patterns, api_url).await,
        PolicyCmd::Edit { file, dry_run } => edit(file, dry_run, api_url).await,
        PolicyCmd::Preset { flavor } => preset(flavor, api_url).await,
        PolicyCmd::Lock => lock(api_url).await,
        PolicyCmd::Unlock { permanent } => unlock(permanent, api_url).await,
        PolicyCmd::Pull { out } => pull(out).await,
        PolicyCmd::Diff { file } => diff(file).await,
        PolicyCmd::Revoke => revoke(api_url).await,
        PolicyCmd::Keygen { env } => keygen(&env),
    }
}

/// (operator) Generate the policy signing keypair for a backend env. Hidden — this
/// provisions VAIBot's OWN control plane (prints `fly secrets set ...`), never a
/// customer flow. Kept here so the operator path survives outside `vaibot init`.
fn keygen(env: &str) -> Result<(), CliError> {
    let env = match env.to_ascii_lowercase().as_str() {
        "production" | "prod" => crate::config::creds::VaibotEnv::Production,
        "staging" => crate::config::creds::VaibotEnv::Staging,
        other => return Err(CliError::Runtime(format!("unknown env '{other}' (use staging | production)"))),
    };
    crate::services::signing_keys::ensure_signing_key(env, false)
}

/// Fetch the active denylist from the local guard (GET /v1/policy). No printing.
async fn fetch_active() -> Result<Vec<String>, CliError> {
    let base = guard_http::resolve_guard_base_url();
    guard_http::fetch_guard_policy(&base).await.map(|p| p.denylist)
}

fn load_policy_file(file: &str) -> Result<Policy, CliError> {
    let raw = std::fs::read_to_string(file).map_err(|e| CliError::Runtime(format!("read {file}: {e}")))?;
    Policy::load_yaml(&raw)
}

/// Diff two denylists (active vs desired); prints +added / -only-in-active.
fn print_denylist_diff(active: &[String], desired: &[String]) {
    let added: Vec<&String> = desired.iter().filter(|d| !active.contains(d)).collect();
    let removed: Vec<&String> = active.iter().filter(|a| !desired.contains(a)).collect();
    if added.is_empty() && removed.is_empty() {
        println!("  (no changes — local matches active: {} denial(s))", active.len());
        return;
    }
    for a in &added {
        println!("  + {a}   (would be added)");
    }
    for r in &removed {
        println!("  - {r}   (in active, not in your file)");
    }
    println!("\n  {} to add · {} only in active", added.len(), removed.len());
}

/// `vaibot policy deny <pattern>…` — add denial(s) on top of the floor.
async fn deny(patterns: Vec<String>, api_url: Option<String>) -> Result<(), CliError> {
    let patterns: Vec<String> = patterns
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if patterns.is_empty() {
        return Err(CliError::Runtime(
            "Usage: vaibot policy deny '<pattern>' [<pattern> ...]".into(),
        ));
    }

    let client = resolve_api_client(api_url.as_deref(), None).await?;
    println!("\nVAIBot Policy — deny\n");
    match client.request_policy(&patterns).await {
        ApiResult::Ok { data, .. } => {
            println!("[ok]   Added {} denial(s) — policy version {}", patterns.len(), data.version);
            println!("  added:  {}", patterns.join(", "));
            println!("  hash:   {}", data.hash);
            println!("  anchor: {}  (keccak leaf; verifiable on Base once anchored)", data.anchor_hash);
            println!("[info] Remove your additions with `vaibot policy allow <pattern>`.");
            println!("[info] Restart the guard (or wait for refresh) to pull the new signed policy.");
            Ok(())
        }
        ApiResult::Err { error, status } => Err(policy_error("Deny", status, &error)),
    }
}

/// Apply a full denylist declaratively and report (shared by allow/edit).
async fn apply_and_report(client: &ApiClient, denylist: &[String], verb: &str) -> Result<(), CliError> {
    match client.apply_policy(denylist).await {
        ApiResult::Ok { data, .. } => {
            println!("[ok]   {verb} — policy version {}", data.version);
            println!("  denials now: {}", denylist.len());
            println!("  hash:   {}", data.hash);
            println!("  anchor: {}  (keccak leaf; verifiable on Base once anchored)", data.anchor_hash);
            println!("[info] Restart the guard (or wait for refresh) to pull the new signed policy.");
            Ok(())
        }
        ApiResult::Err { error, status } => Err(policy_error(verb, status, &error)),
    }
}

/// Read the authoritative active denylist from the control plane (GET /v2/policy).
async fn control_plane_active(client: &ApiClient, verb: &str) -> Result<Vec<String>, CliError> {
    match client.active_denylist().await {
        ApiResult::Ok { data, .. } => Ok(data),
        ApiResult::Err { error, status } => Err(policy_error(verb, status, &error)),
    }
}

/// `vaibot policy allow <pattern>…` — remove your additions (never below your floor).
async fn allow(patterns: Vec<String>, api_url: Option<String>) -> Result<(), CliError> {
    let patterns: Vec<String> = patterns
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if patterns.is_empty() {
        return Err(CliError::Runtime(
            "Usage: vaibot policy allow '<pattern>' [<pattern> ...]".into(),
        ));
    }

    let client = resolve_api_client(api_url.as_deref(), None).await?;
    println!("\nVAIBot Policy — allow (remove your additions)\n");
    let active = control_plane_active(&client, "Allow").await?;

    let removed: Vec<&String> = patterns.iter().filter(|p| active.contains(p)).collect();
    let not_present: Vec<&String> = patterns.iter().filter(|p| !active.contains(p)).collect();

    if removed.is_empty() {
        println!("[info] None of those are in your additions — nothing to remove.");
        if !not_present.is_empty() {
            println!("  not present: {}", join_refs(&not_present));
        }
        println!("[info] System-floor verbs (curl, rm, sudo, …) are never on the denylist —");
        println!("       they're gated by the ask lane, not removable here.");
        return Ok(());
    }

    for r in &removed {
        println!("  - {r}");
    }
    if !not_present.is_empty() {
        println!("  (skipped, not present: {})", join_refs(&not_present));
    }
    println!();

    let new_denylist: Vec<String> = active.iter().filter(|d| !patterns.contains(d)).cloned().collect();
    apply_and_report(&client, &new_denylist, "Removed").await
}

/// `vaibot policy edit [-f file] [--dry-run]` — declarative bulk edit.
async fn edit(file: Option<String>, dry_run: bool, api_url: Option<String>) -> Result<(), CliError> {
    let client = resolve_api_client(api_url.as_deref(), None).await?;
    let active = control_plane_active(&client, "Edit").await?;

    let desired: Vec<String> = match &file {
        Some(path) => load_policy_file(path)?.denylist,
        None => edit_via_editor(&active)?,
    };

    println!("\nVAIBot Policy — edit\n");
    print_denylist_diff(&active, &desired);

    if dry_run {
        println!("\n[dry-run] No changes applied.");
        return Ok(());
    }

    let changed = desired.iter().any(|d| !active.contains(d)) || active.iter().any(|a| !desired.contains(a));
    if !changed {
        println!("\n[info] No changes — nothing to apply.");
        return Ok(());
    }
    println!();
    apply_and_report(&client, &desired, "Applied").await
}

/// Seed a temp YAML with the active denylist, open $EDITOR, and return the edited list.
fn edit_via_editor(seed: &[String]) -> Result<Vec<String>, CliError> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "vi".to_string());

    let policy = Policy { denylist: seed.to_vec(), ..Default::default() };
    let yaml = serde_yaml::to_string(&policy).map_err(|e| CliError::Runtime(format!("serialize: {e}")))?;
    let header = "# vaibot policy edit — your governed denials (edit the `denylist` items).\n\
                  # Save & quit to apply; quit without saving to abort.\n\
                  # The system floor (curl|sh, rm -rf /, …) is enforced separately, not shown here.\n";
    let path = std::env::temp_dir().join(format!("vaibot-policy-edit-{}.yaml", std::process::id()));
    std::fs::write(&path, format!("{header}{yaml}")).map_err(|e| CliError::Runtime(format!("write temp: {e}")))?;

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .map_err(|e| CliError::Runtime(format!("launch editor '{editor}': {e}")))?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(CliError::Runtime(format!("editor '{editor}' exited without saving — aborted")));
    }

    let edited = load_policy_file(path.to_str().unwrap_or_default());
    let _ = std::fs::remove_file(&path);
    Ok(edited?.denylist)
}

fn join_refs(items: &[&String]) -> String {
    items.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
}

const PRESET_FLAVORS: [&str; 3] = ["permissive", "balanced", "strict"];

/// `vaibot policy preset [<flavor>]` — show the floors, or set one.
async fn preset(flavor: Option<String>, api_url: Option<String>) -> Result<(), CliError> {
    let Some(raw) = flavor else {
        print_preset_help();
        return Ok(());
    };
    let f = raw.trim().to_lowercase();
    if !PRESET_FLAVORS.contains(&f.as_str()) {
        println!("\n[fail] Unknown preset '{f}'. Choose one of: {}.", PRESET_FLAVORS.join(", "));
        return Err(CliError::Runtime("unknown preset".into()));
    }

    let client = resolve_api_client(api_url.as_deref(), None).await?;
    println!("\nVAIBot Policy — preset {f}\n");
    match client.apply_preset(&f).await {
        ApiResult::Ok { data, .. } => {
            println!("[ok]   Floor set to {f} — policy version {}", data.version);
            println!("  hash:   {}", data.hash);
            println!("  anchor: {}  (keccak leaf; verifiable on Base once anchored)", data.anchor_hash);
            println!("[info] Tighten further with `vaibot policy deny`; freeze with `vaibot policy lock`.");
            println!("[info] Restart the guard (or wait for refresh) to pull the new signed policy.");
            Ok(())
        }
        ApiResult::Err { error, status } => Err(policy_error("Preset", status, &error)),
    }
}

/// `vaibot policy lock` — freeze the policy (email step-up).
async fn lock(api_url: Option<String>) -> Result<(), CliError> {
    let client = resolve_api_client(api_url.as_deref(), None).await?;
    lock_step_up(&client, "lock", "Policy frozen. Any change now needs `vaibot policy unlock` first.").await
}

/// `vaibot policy unlock [--permanent]` — open a 30-min window, or remove the lock.
async fn unlock(permanent: bool, api_url: Option<String>) -> Result<(), CliError> {
    let client = resolve_api_client(api_url.as_deref(), None).await?;
    let (action, done) = if permanent {
        ("unlock_permanent", "Lock removed. Policy changes are back to API-key only.")
    } else {
        ("unlock", "Unlocked for 30 min. Make your changes, then `vaibot policy lock` (or it re-locks itself).")
    };
    lock_step_up(&client, action, done).await
}

/// Shared email step-up: activate (emails an OTP) → paste the code → verify.
async fn lock_step_up(client: &ApiClient, action: &str, done_msg: &str) -> Result<(), CliError> {
    println!("\nVAIBot Policy — {} (email step-up)\n", action.replace('_', " "));
    let activate = match client.policy_stepup_activate(action).await {
        ApiResult::Ok { data, .. } => data,
        ApiResult::Err { status: 409, .. } => {
            println!("[fail] This needs a claimed email — we email you a code to confirm.");
            println!("       Claim one first (`vaibot login` or the dashboard), then retry.");
            return Err(CliError::Runtime("email unclaimed".into()));
        }
        ApiResult::Err { error, status } => return Err(policy_error("Step-up", status, &error)),
    };
    let Some(token) = activate.token else {
        return Err(CliError::Runtime("server did not return a step-up token".into()));
    };
    println!(
        "  ▸ Emailed a confirmation code to {} (expires in ~15 min).",
        activate.sent_to.as_deref().unwrap_or("your email")
    );
    let code = prompt_line("  Paste the code (or press Enter to use the email link instead): ")?;
    let code = code.trim();
    if code.is_empty() {
        println!("  No code entered — click the link in your email to confirm, then re-run to check state.");
        return Ok(());
    }
    match client.policy_stepup_verify(&token, code).await {
        ApiResult::Ok { .. } => {
            println!("  [ok]   {done_msg}");
            Ok(())
        }
        ApiResult::Err { status: 400, .. } => Err(CliError::Runtime("Incorrect or expired code — re-run to get a new one.".into())),
        ApiResult::Err { status: 429, .. } => Err(CliError::Runtime("Too many attempts — wait a moment, then re-run.".into())),
        ApiResult::Err { error, status } => Err(policy_error("Verify", status, &error)),
    }
}

/// Print a prompt and read one line from stdin.
fn prompt_line(msg: &str) -> Result<String, CliError> {
    use std::io::Write;
    print!("{msg}");
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .map_err(|e| CliError::Runtime(format!("read input: {e}")))?;
    Ok(buf)
}

fn print_preset_help() {
    println!("\nVAIBot Policy Presets — your governance floor\n");
    println!("  permissive   audit-first; only the safety floor asks, nothing extra denied");
    println!("  balanced     + package installs ask (recommended)");
    println!("  strict       + outbound network, secret reads, sudo, irreversible VCS hard-denied;");
    println!("               workspace writes ask");
    println!("\n  Set one:   vaibot policy preset <flavor>");
    println!("  Inspect:   vaibot policy show   (active denials)\n");
}

/// `vaibot policy pull` — write the active policy to a local YAML file. (transitional)
async fn pull(out: String) -> Result<(), CliError> {
    println!("\nVAIBot Policy Pull\n");
    let denylist = match fetch_active().await {
        Ok(d) => d,
        Err(e) => {
            let base = guard_http::resolve_guard_base_url();
            println!("  [fail] Guard not reachable at {base} ({e}).");
            println!("  Start it (a VAIBot plugin spawns it, or `vaibot guard install`).");
            return Err(CliError::Runtime("guard not reachable".into()));
        }
    };
    let policy = Policy { denylist, ..Default::default() };
    let yaml = serde_yaml::to_string(&policy).map_err(|e| CliError::Runtime(format!("serialize: {e}")))?;
    std::fs::write(&out, &yaml).map_err(|e| CliError::Runtime(format!("write {out}: {e}")))?;
    println!("  [ok]   Wrote {} denial(s) to {out}", policy.denylist.len());
    println!("  Edit it, then: vaibot policy diff {out}  /  vaibot policy edit -f {out}");
    Ok(())
}

/// `vaibot policy diff <file>` — diff a local YAML policy against the active. (transitional)
async fn diff(file: String) -> Result<(), CliError> {
    let local = load_policy_file(&file)?;
    println!("\nVAIBot Policy Diff — {file} vs active\n");
    let active = match fetch_active().await {
        Ok(d) => d,
        Err(e) => {
            let base = guard_http::resolve_guard_base_url();
            println!("  [fail] Guard not reachable at {base} ({e}).");
            return Err(CliError::Runtime("guard not reachable".into()));
        }
    };
    print_denylist_diff(&active, &local.denylist);
    Ok(())
}

/// `vaibot policy revoke` — coarse one-version rollback. (transitional → `allow`)
async fn revoke(api_url: Option<String>) -> Result<(), CliError> {
    let client = resolve_api_client(api_url.as_deref(), None).await?;
    println!("\nVAIBot Policy Revoke (coarse rollback — superseded by `policy allow`)\n");
    match client.revoke_policy().await {
        ApiResult::Ok { data, .. } => {
            match data.revoked {
                None => println!("[info] No active signed policy to revoke (already on built-in defaults)."),
                Some(r) => {
                    println!("[ok]   Revoked policy version {}", r.version);
                    match data.active {
                        Some(a) => println!("[info] Active is now version {} (this rolled back ONE version).", a.version),
                        None => println!("[info] No signed policy remains — clients revert to built-in defaults."),
                    }
                }
            }
            println!("[info] Guards honor this on their next refresh (or restart).");
            Ok(())
        }
        ApiResult::Err { error, status } => Err(policy_error("Revoke", status, &error)),
    }
}

async fn history(api_url: Option<String>) -> Result<(), CliError> {
    let client = resolve_api_client(api_url.as_deref(), None).await?;
    println!("\nVAIBot Policy History\n");
    match client.policy_history().await {
        ApiResult::Ok { data, .. } => {
            if data.history.is_empty() {
                println!("[info] No policy changes recorded.");
            } else {
                for h in &data.history {
                    let state = if let Some(rev) = &h.revoked_at {
                        format!("revoked {rev} by {}", h.revoked_by.as_deref().unwrap_or("?"))
                    } else if h.active {
                        "ACTIVE".into()
                    } else {
                        "superseded".into()
                    };
                    println!("  {}  [{state}]", h.version);
                    println!("    requested by {} at {}", h.created_by, h.created_at);
                    println!("    fingerprint {}  ·  anchor {}", h.hash, h.anchor_hash);
                }
            }
            Ok(())
        }
        ApiResult::Err { error, status } => Err(policy_error("History", status, &error)),
    }
}

fn policy_error(verb: &str, status: u16, error: &str) -> CliError {
    let msg = match status {
        400 => format!("Rejected: {error}"),
        401 => "Unauthorized — your API key was rejected.".to_string(),
        423 => "Policy is locked. Run `vaibot policy unlock` first (opens a 30-min window).".to_string(),
        503 => "The control plane has no signing key configured yet — policy signing is not provisioned.".to_string(),
        0 => format!("{verb} failed (network): {error}"),
        _ => format!("{verb} failed ({status}): {error}"),
    };
    CliError::Runtime(msg)
}
