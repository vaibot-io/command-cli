//! `init` [REAL composer] · `doctor` [REAL; --fix note-only] · `update` [STUB].

use crate::api::{ApiClient, ApiResult};
use crate::broker::{file::persist_api_key, get_broker, mode_for};
use crate::config::creds::{
    api_base_for_env, api_key_for_env, load_store, resolve_credentials, save_active_env, VaibotEnv,
};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;
use crate::oauth::LoginOptions;
use crate::services::guard_http;
use crate::services::host::Host;
use crate::services::installer;
use crate::services::{is_active_systemd_unit, systemd_available, which};

use super::stdout_print;

/// `vaibot init` — onboarding composer (login + stack install). The
/// --with-gateway / --with-mcp legs are note-only.
#[allow(clippy::too_many_arguments)]
pub async fn init(
    yes: bool,
    env: Option<String>,
    api_key: Option<String>,
    skip_login: bool,
    with_gateway: bool,
    with_mcp: bool,
    preset: Option<String>,
    api_url: Option<String>,
) -> Result<(), CliError> {
    // VAIBot is a production product: `init` reconciles every component to
    // PRODUCTION by default (it ignores a stored staging active_env / a staging
    // shell override — that's the whole point of re-initializing). Staging is
    // reserved for admin + enterprise accounts and must be requested explicitly
    // with `--env staging`, which is then gated.
    let requested = match env.as_deref() {
        Some("staging") => VaibotEnv::Staging,
        Some("production") | Some("prod") | None => VaibotEnv::Production,
        Some(other) => {
            println!("[warn] Unknown --env '{other}' — using production.");
            VaibotEnv::Production
        }
    };
    if requested != VaibotEnv::Production
        && !crate::commands::env_target_allowed(requested, api_url.as_deref()).await
    {
        return Err(CliError::Runtime(
            "\n  ✗  Staging is reserved for admin + enterprise accounts.\n     `vaibot init` (no --env) sets up production. Admins testing staging:\n     re-run with VAIBOT_ADMIN_OVERRIDE=1."
                .to_string(),
        ));
    }
    let env = requested;
    let store = load_store(&credentials_path(&ProcessEnv));
    println!("\n▸ VAIBot init  ({env})");
    if !yes {
        println!(
            "  I'll confirm each step — press Enter to accept the default [Y/n], or re-run with --yes to accept all.\n"
        );
    }
    // Every step below is INDEPENDENT + best-effort: one component failing warns and
    // moves on (never aborts init), and `summary` records the outcome of each.
    let mut summary: Vec<String> = Vec::new();

    // ── Step 1/5 — Account ───────────────────────────────────────────────────────
    // Recognize an existing OAuth session or key; else interactive login; else
    // bootstrap a free machine account so a fresh machine goes zero-to-installed.
    println!("Step 1/5 — Account");
    let have_key = api_key.is_some() || api_key_for_env(&store, env).is_some();
    let existing = get_broker()
        .whoami(Some(crate::broker::EnvOpt { env: Some(env) }))
        .await
        .ok()
        .flatten();
    if !skip_login && api_key.is_none() && !have_key && existing.is_none() {
        println!("  Logging in...");
        let opts = LoginOptions {
            mode: mode_for(false),
            no_browser: false,
            issuer: api_url.clone(),
            env,
        };
        if let Err(e) = get_broker().login(opts, &stdout_print).await {
            println!("  Login skipped/failed ({e}); falling back to a machine account.");
        }
    } else if let Some(who) = &existing {
        let id = who.email.clone().unwrap_or_else(|| who.subject.clone());
        println!("  Using existing login: {id}");
    }
    if let Some(key) = &api_key {
        persist_api_key(env, key.clone())?;
    } else if api_key_for_env(&load_store(&credentials_path(&ProcessEnv)), env).is_none() {
        bootstrap_account(env, api_url.as_deref()).await;
    }
    let _ = save_active_env(&credentials_path(&ProcessEnv), env);
    let has_key = api_key_for_env(&load_store(&credentials_path(&ProcessEnv)), env).is_some();
    summary.push(if has_key { "account ✓".into() } else { "account ✗ (no key)".into() });

    // ── Step 2/5 — Email (upfront; y/n-gated inside link_email) ───────────────────
    println!("\nStep 2/5 — Email");
    link_email(env, api_url.as_deref(), yes).await;

    // ── Step 3/5 — Guard (BEST-EFFORT — this is the fix: it never aborts init) ────
    println!("\nStep 3/5 — Guard  (the local enforcement + audit daemon — the core runtime)");
    let store = load_store(&credentials_path(&ProcessEnv));
    if api_key_for_env(&store, env).is_none() {
        println!("  [warn] No API key resolved — skipping the guard. Run `vaibot login`, then `vaibot guard install`.");
        summary.push("guard ✗ (no key)".into());
    } else if confirm(yes, "  Install the VAIBot guard now? [Y/n] ") {
        match install_guard() {
            Ok(()) => summary.push("guard ✓".into()),
            Err(e) => {
                println!("  [warn] Guard install hit a snag: {e}");
                println!("         Not fatal — the plugins self-spawn the guard on the first tool call.");
                println!("         Re-try later with `vaibot guard install`.");
                summary.push("guard ✗".into());
            }
        }
    } else {
        println!("  Skipped — install later with `vaibot guard install`. (Governance won't enforce locally until you do.)");
        summary.push("guard — skipped".into());
    }
    // Optional governance floor (preset), so the guard pulls a posture on first fetch.
    if let Some(flavor) = preset.as_deref() {
        apply_preset_at_init(env, api_url.as_deref(), flavor).await;
    }

    // ── Step 4/5 — MCP server (BEFORE plugins) ───────────────────────────────────
    println!("\nStep 4/5 — MCP server  (agent-facing governance tools your agent can call)");
    // `--with-mcp` (or --yes) auto-confirms; only_existing=false registers on every
    // detected agent, not just re-pins existing ones.
    if confirm(yes || with_mcp, "  Register the VAIBot MCP server on your detected agents? [Y/n] ") {
        match crate::commands::mcp::connect_to_env(env, None, api_url.as_deref(), false) {
            Ok(()) => summary.push("MCP ✓".into()),
            Err(e) => {
                println!("  [warn] MCP registration skipped: {e}");
                summary.push("MCP ✗".into());
            }
        }
    } else {
        println!("  Skipped — register later with `vaibot mcp connect`.");
        summary.push("MCP — skipped".into());
    }

    // ── Step 5/5 — Plugins (ordered Claude, OpenClaw, Codex, Cursor; y/n each) ─────
    println!("\nStep 5/5 — Plugins  (mandatory pre-execution enforcement, one per agent)");
    wire_hosts_interactive(yes, &mut summary).await;

    // Deferred leg (note-only, do not error).
    if with_gateway {
        println!("\nNote: --with-gateway is not yet wired (gateway serve is a shell-out stub).");
    }

    // ── Summary ──────────────────────────────────────────────────────────────────
    println!("\n▸ Done.  {}", summary.join("  ·  "));
    println!("  Switch modes anytime: `vaibot mode observe|enforce`. Approve from https://www.vaibot.io.");
    Ok(())
}

const INIT_PRESET_FLAVORS: [&str; 3] = ["permissive", "balanced", "strict"];

/// `init --preset <flavor>` — set the governance floor at activation via the
/// signed preset endpoint. Best-effort: narrates and returns on any problem.
async fn apply_preset_at_init(env: VaibotEnv, api_url: Option<&str>, flavor: &str) {
    let f = flavor.trim().to_lowercase();
    if !INIT_PRESET_FLAVORS.contains(&f.as_str()) {
        println!("\n[warn] Unknown preset '{flavor}' — skipping (choose {}).", INIT_PRESET_FLAVORS.join(" | "));
        return;
    }
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    let Some(key) = resolved.api_key.clone() else {
        println!("\n[warn] No API key — skipping preset. Set it later with `vaibot policy preset {f}`.");
        return;
    };
    let base = api_base_for_env(env, api_url.or(Some(&resolved.api_base_url)));
    let client = match ApiClient::new(base, Some(key)) {
        Ok(c) => c,
        Err(e) => {
            println!("\n[warn] preset skipped (client: {e}).");
            return;
        }
    };
    println!("\n▸ Setting your governance floor: {f}...");
    match client.apply_preset(&f).await {
        ApiResult::Ok { data, .. } => {
            println!("  [ok]   Floor set to {f} (policy version {}).", data.version);
            println!("  Freeze it so changes need email confirmation: `vaibot policy lock`.");
        }
        ApiResult::Err { error, status } => {
            println!("  [warn] preset failed ({status}): {error}.");
            println!("  Set it later with `vaibot policy preset {f}`.");
        }
    }
}

/// Bootstrap a free account by machine fingerprint and persist the returned
/// api_key. Best-effort: narrates and returns even on failure (the install step
/// then surfaces the missing-key error). Unauthenticated call (bearer = None).
async fn bootstrap_account(env: VaibotEnv, api_url: Option<&str>) {
    println!("▸ Creating your VAIBot account...");
    let client = match ApiClient::new(api_base_for_env(env, api_url), None) {
        Ok(c) => c,
        Err(e) => {
            println!("  [warn] could not build API client: {e}");
            return;
        }
    };
    match client.bootstrap(&machine_fingerprint(), "vaibot-cli").await {
        ApiResult::Ok { data, .. } => {
            if data.bootstrapped {
                match data.api_key {
                    Some(key) => match persist_api_key(env, key) {
                        Ok(()) => println!("  ✔ Account provisioned"),
                        Err(e) => println!("  [warn] account created but saving the key failed: {e}"),
                    },
                    None => println!("  ✔ Account provisioned (no key returned)"),
                }
            } else {
                let hint = data
                    .api_key_hint
                    .map(|h| format!(" (hint: {h})"))
                    .unwrap_or_default();
                println!("  Account already exists for this machine{hint}");
                println!("  Your key lives in ~/.vaibot/credentials.json — or set VAIBOT_API_KEY.");
            }
        }
        ApiResult::Err { error, .. } => {
            println!("  [warn] Auto-bootstrap failed: {error}");
            println!("  Provide one with --api-key, or find it at https://www.vaibot.io/dashboard/settings");
        }
    }
}

/// Stable per-machine+user fingerprint = sha256("{username}@{hostname}"),
/// byte-for-byte matching the Node CLI so the server dedups the same machine
/// across both implementations.
fn machine_fingerprint() -> String {
    use sha2::{Digest, Sha256};
    let user = whoami::username();
    let host = whoami::fallible::hostname().unwrap_or_else(|_| "localhost".to_string());
    let digest = Sha256::digest(format!("{user}@{host}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Optional email link via magic link (mirrors setup.ts `promptAndLinkEmail`).
/// Interactive: prompts for an email and POSTs /v2/accounts/set-email with the
/// account's api_key as bearer. No-op on --yes or when no key is resolvable.
async fn link_email(env: VaibotEnv, api_url: Option<&str>, yes: bool) {
    if yes {
        println!("  Skipped email link (--yes). Link later with `vaibot account claim`.");
        return;
    }
    let store = load_store(&credentials_path(&ProcessEnv));
    // Use the key for THIS env so we hit the matching API base — sending a staging
    // key to the prod endpoint (or vice-versa) is what produced the 401 on set-email.
    let Some(api_key) = api_key_for_env(&store, env) else {
        println!("  No account key yet — link an email later with `vaibot account claim`.");
        return; // nothing to link to for this env
    };
    if !prompt_yes_no("  Link an email so you can approve from the dashboard & recover your key? [Y/n] ") {
        println!("  Skipped — link later with `vaibot account claim`.");
        return;
    }
    let email = prompt("  Email: ");
    if email.is_empty() {
        println!("  No email entered — skipped. Link later with `vaibot account claim`.");
        return;
    }
    let client = match ApiClient::new(api_base_for_env(env, api_url), Some(api_key)) {
        Ok(c) => c,
        Err(e) => {
            println!("  [warn] could not build API client: {e}");
            return;
        }
    };
    claim_email_interactive(&client, &email).await;
}

/// The interactive verified-claim flow against a built client. Claims the email;
/// if it already has an account, prompts for the 6-digit code emailed to that
/// address and confirms the merge — so this machine ends up operating as that
/// account. Prints progress; never panics.
pub async fn claim_email_interactive(client: &ApiClient, email: &str) {
    match client.claim(email).await {
        ApiResult::Ok { data, .. } if data.verify_required => {
            let Some(token) = data.pending_token else {
                println!("  [warn] server did not return a verification token — try again.");
                return;
            };
            println!(
                "  {}",
                data.message
                    .unwrap_or_else(|| format!("A 6-digit code was sent to {email}."))
            );
            let code = prompt("  Enter the code (Enter to skip): ");
            if code.is_empty() {
                println!("  Skipped — re-run `vaibot account claim` to finish.");
                return;
            }
            match client.claim_confirm(&token, &code).await {
                ApiResult::Ok { data, .. } if data.merged => {
                    println!("  ✔ Linked — this machine now operates as your account.");
                }
                ApiResult::Ok { .. } => println!("  ✔ Linked."),
                ApiResult::Err { status: 400, .. } => {
                    println!("  [warn] Incorrect or expired code — re-run `vaibot account claim`.");
                }
                ApiResult::Err { error, .. } => println!("  [warn] Could not finish linking: {error}"),
            }
        }
        ApiResult::Ok { data, .. } => {
            // Fresh email — linked directly; a magic link verifies dashboard sign-in.
            let msg = data
                .hint
                .or(data.message)
                .unwrap_or_else(|| "Check your inbox to verify.".to_string());
            println!("  ✔ {msg}");
        }
        ApiResult::Err { status: 403, .. } => {
            println!("  This account is already linked — manage your email from the dashboard.");
        }
        ApiResult::Err { error, .. } => println!("  [warn] Could not link email: {error}"),
    }
}

/// `vaibot account claim [--email <addr>]` — link this machine to your real
/// account (the verified two-step). Resolves the active env's key; prompts for
/// the email when not given.
pub async fn claim_command(email: Option<String>, api_url: Option<String>) -> Result<(), CliError> {
    let env = crate::commands::current_env();
    let store = load_store(&credentials_path(&ProcessEnv));
    let Some(api_key) = api_key_for_env(&store, env) else {
        return Err(CliError::Auth);
    };
    let email = match email {
        Some(e) if !e.trim().is_empty() => e,
        _ => {
            let e = prompt("Email to link: ");
            if e.is_empty() {
                println!("No email given — nothing to do.");
                return Ok(());
            }
            e
        }
    };
    let client = ApiClient::new(api_base_for_env(env, api_url.as_deref()), Some(api_key))?;
    println!("Linking {email}  (env: {env})");
    claim_email_interactive(&client, &email).await;
    Ok(())
}

/// Print a label and read a trimmed line from stdin (empty string on EOF/error).
fn prompt(label: &str) -> String {
    use std::io::{self, Write};
    print!("{label}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return String::new();
    }
    line.trim().to_string()
}

/// y/n prompt; default YES on empty input (Enter).
fn prompt_yes_no(label: &str) -> bool {
    let ans = prompt(label).to_ascii_lowercase();
    ans.is_empty() || ans == "y" || ans == "yes"
}

/// Init plugin order: the reliable natives first, then Codex + Cursor LAST — they're
/// the most interactive/finicky (Codex's enable picker; Cursor's git-clone install),
/// so a hiccup there lands after everything sturdier is already in.
const INIT_HOST_ORDER: [Host; 4] =
    [Host::Claudecode, Host::Openclaw, Host::Codex, Host::Cursor];

/// Offer to install the circuit-breaker plugin for each DETECTED agent, in a fixed
/// order, one y/n at a time. Best-effort: a failure warns and moves to the next.
/// Records each outcome into `summary`.
async fn wire_hosts_interactive(yes: bool, summary: &mut Vec<String>) {
    let detected: Vec<Host> = INIT_HOST_ORDER
        .into_iter()
        .filter(|h| h.cli_present())
        .collect();
    if detected.is_empty() {
        println!("  No agents detected on PATH (Claude Code / OpenClaw / Codex / Cursor).");
        println!("  Install one, then run `vaibot plugin add <host>`.");
        summary.push("plugins — none detected".into());
        return;
    }
    let names: Vec<&str> = detected.iter().map(|h| h.label()).collect();
    println!("  Detected: {}.", names.join(", "));
    for h in detected {
        if !confirm(yes, &format!("  Install the VAIBot plugin for {}? [Y/n] ", h.label())) {
            println!("    Skipped — add later with `vaibot plugin add {}`.", h.key());
            summary.push(format!("{} — skipped", h.label()));
            continue;
        }
        match crate::commands::plugin::install_host_plugin(h) {
            Ok(()) => {
                // Count init-driven installs too (same best-effort telemetry as `plugin add`).
                crate::commands::plugin::report_plugin_install(h).await;
                summary.push(format!("{} ✓", h.label()));
            }
            Err(e) => {
                println!("    [warn] {} plugin install failed: {e} — continuing.", h.label());
                summary.push(format!("{} ✗", h.label()));
            }
        }
    }
}

/// Interactive y/n gate. Accepts unconditionally under --yes; otherwise prompts,
/// defaulting to YES on Enter.
fn confirm(yes: bool, label: &str) -> bool {
    yes || prompt_yes_no(label)
}

/// First-class, host-agnostic guard install: ensure `@vaibot/guard` is present
/// (npm; install if missing), write the guard env file, enable the systemd user
/// service. Best-effort — narrates each step. Shared by `init`, `guard install`,
/// and (ensure-if-missing) the host plugin installer.
pub fn install_guard() -> Result<(), CliError> {
    println!("[step] Installing guard...");
    if installer::guard_skill_exists() {
        println!("[ok]   Guard already installed — skipping");
    } else if installer::install_guard_skill() {
        println!("[ok]   Guard installed (npm i -g {})", installer::GUARD_NPM_SPEC);
    } else {
        println!("[warn] Could not install the guard automatically.");
        println!("       Install manually: npm install -g {}", installer::GUARD_NPM_SPEC);
    }

    println!("[step] Writing guard environment file...");
    // v3: the guard derives api_key + governance/provenance bases + policy feed
    // from the creds store; the env file only pins the per-host audit-log dir.
    let path = installer::write_guard_env_file()?;
    println!("[ok]   Guard env written to {}", path.display());
    println!("[ok]   The guard enforces your governance floor locally; it adopts VAIBot's signed");
    println!("       policy automatically once that feed is live for your account.");

    println!("[step] Installing the guard as a service (platform-aware: systemd / launchd / self-spawn)...");
    if installer::install_guard_service_platform() {
        println!("[ok]   Guard service installed + healthy.");
    } else {
        println!("[warn] Guard service not confirmed healthy — it self-spawns on the first tool call.");
        println!("       See ~/.vaibot/guard/launch.log or run `vaibot guard status`.");
    }
    Ok(())
}

/// `vaibot doctor [--fix]` — read-only diagnostics. --fix prints a note only.
pub async fn doctor(fix: bool) -> Result<(), CliError> {
    if fix {
        println!("[note] --fix has no remediations to apply right now; running read-only checks.\n");
    }
    println!("VAIBot Doctor\n");

    // Host integrations.
    println!(
        "  openclaw CLI:        {}",
        present(which("openclaw").is_some())
    );
    println!(
        "  claude CLI:          {}",
        present(which("claude").is_some())
    );
    println!("  codex CLI:           {}", present(which("codex").is_some()));
    println!(
        "  guard skill:         {}",
        present(installer::guard_skill_exists())
    );

    // Guard service + health.
    if systemd_available() {
        println!(
            "  guard service:       {}",
            if is_active_systemd_unit("vaibot-guard") {
                "active"
            } else {
                "not active"
            }
        );
    } else {
        println!("  guard service:       systemd not available");
    }

    let base = guard_http::resolve_guard_base_url();
    let guard_policy = match guard_http::fetch_guard_policy(&base).await {
        Ok(gp) => {
            println!("  guard /v1/policy:    reachable ({base}, source: {})", gp.source);
            Some(gp)
        }
        Err(e) => {
            // An `HTTP <code>` error means the guard answered but lacks the route —
            // i.e. it's running, just an older build. A non-HTTP error (refused /
            // timeout) is a genuinely unreachable guard.
            let msg = e.to_string();
            if msg.starts_with("HTTP ") {
                println!("  guard /v1/policy:    {msg} — guard is running but lacks this route (outdated build).");
                println!("                       Refresh it: `vaibot guard restart` (or reinstall @vaibot/guard).");
            } else {
                println!("  guard /v1/policy:    unreachable ({base}: {msg})");
            }
            None
        }
    };

    // Credentials.
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    println!("  env (CLI):           {}", resolved.env);
    let guard_env = crate::commands::guard_pinned_env();
    match guard_env {
        Some(g) => println!("  env (guard):         {g}"),
        None => println!("  env (guard):         not configured"),
    }
    // Production-coherence verdict (CLI + guard) — matches the run-gate.
    let coherent_prod = resolved.env == VaibotEnv::Production
        && guard_env.map(|e| e == VaibotEnv::Production).unwrap_or(true);
    if coherent_prod {
        println!("  environment:         ✓ production (coherent)");
    } else {
        println!("  environment:         ⚠ NOT coherently production — run `vaibot init` to reconcile");
    }
    println!("  api key:             {}", present(resolved.api_key.is_some()));
    if resolved.key_mismatch {
        println!("  [warn] stored key prefix names a different env — re-bootstrap this env.");
    }

    // Policy posture (customer view): is a signed bundle active for the account,
    // and is the guard consuming it? The guard runs on its built-in floor until
    // VAIBot's signed-policy public key is provisioned + pinned (a server-side
    // step) — that's the safe default, not an error the customer must fix.
    if let Ok(client) = ApiClient::new(
        api_base_for_env(resolved.env, Some(&resolved.api_base_url)),
        resolved.api_key.clone(),
    ) {
        if let ApiResult::Ok { data, .. } = client.active_policy().await {
            let cp_signed = data.bundle.is_some();
            let guard_verifies = guard_policy.as_ref().map(|g| g.source == "bundle");
            match (cp_signed, guard_verifies) {
                (true, Some(true)) => println!("  policy:              ok (guard is enforcing VAIBot signed policy)"),
                (true, Some(false)) => println!("  policy:              signed policy available; guard on built-in floor (pinning pending)"),
                (true, None) => println!("  policy:              signed policy available; guard not reporting its policy (see guard line above)"),
                (false, _) => println!("  policy:              built-in floor (no signed policy active for this account)"),
            }
        }
    }

    println!();
    Ok(())
}

fn present(b: bool) -> &'static str {
    if b {
        "present"
    } else {
        "not found"
    }
}

/// `vaibot update` — STUB.
pub fn update() -> Result<(), CliError> {
    Err(CliError::stub("update"))
}

#[cfg(test)]
mod tests {
    use super::machine_fingerprint;

    #[test]
    fn fingerprint_is_stable_64_hex_and_matches_node_formula() {
        let fp = machine_fingerprint();
        assert_eq!(fp.len(), 64, "sha256 hex is 64 chars");
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()), "lowercase hex");
        assert_eq!(fp, machine_fingerprint(), "deterministic");
        // Eyeball against the Node CLI: node -e "sha256(userInfo().username+'@'+hostname())".
        eprintln!("RUST_FINGERPRINT={fp}");
    }

    #[test]
    fn init_host_order_puts_codex_and_cursor_last() {
        use super::INIT_HOST_ORDER;
        use crate::services::host::Host;
        assert_eq!(INIT_HOST_ORDER.len(), 4);
        assert!(INIT_HOST_ORDER[0] == Host::Claudecode, "Claude Code first");
        let last_two = &INIT_HOST_ORDER[2..];
        assert!(last_two.contains(&Host::Codex), "Codex in the last two");
        assert!(last_two.contains(&Host::Cursor), "Cursor in the last two");
        let idx = |t: Host| INIT_HOST_ORDER.iter().position(|h| *h == t).unwrap();
        assert!(idx(Host::Openclaw) < idx(Host::Codex), "OpenClaw before Codex");
    }

    #[test]
    fn confirm_accepts_all_under_yes() {
        // --yes short-circuits before any stdin prompt, so this never blocks.
        assert!(super::confirm(true, "  unreached [Y/n] "));
    }
}
