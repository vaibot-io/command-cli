//! `plugin` group.
//!   add   [REAL] — ensure the shared guard, then install the host plugin via its native CLI + verify.
//!   list  [REAL,--json].
//!   remove [REAL] — uninstall the host plugin (+ --with-guard for the shared guard).
//!   update [REAL] — re-pull the guard + host plugin to latest.

use std::time::Duration;

use clap::Subcommand;

use crate::api::ApiClient;
use crate::config::creds::{api_base_for_env, api_key_for_env, load_store, resolve_credentials};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;
use crate::services::host::Host;
use crate::services::installer;
use crate::services::{is_active_systemd_unit, which};

use super::{current_env, setup};

#[derive(Subcommand, Debug)]
pub enum PluginCmd {
    /// Install a host's circuit-breaker plugin (and ensure the shared guard).
    Add {
        /// Target host: claudecode | codex | openclaw | cursor | hermes.
        #[arg(default_value = "openclaw")]
        host: String,
        /// Skip ensuring the shared guard.
        #[arg(long = "skip-guard")]
        skip_guard: bool,
        /// Skip the circuit-breaker plugin install.
        #[arg(long = "skip-plugin")]
        skip_plugin: bool,
    },
    /// List installed VAIBot host integrations.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Uninstall a VAIBot host integration (the circuit-breaker plugin).
    Remove {
        /// Target host: claudecode | codex | openclaw | cursor | hermes.
        #[arg(default_value = "openclaw")]
        host: String,
        /// Also uninstall the SHARED guard (npm + systemd). Off by default —
        /// the guard is shared across hosts.
        #[arg(long = "with-guard")]
        with_guard: bool,
    },
    /// Upgrade a VAIBot host integration (guard + circuit-breaker plugin).
    Update {
        /// Target host: claudecode | codex | openclaw | cursor | hermes.
        #[arg(default_value = "openclaw")]
        host: String,
        /// Skip updating the shared guard.
        #[arg(long = "skip-guard")]
        skip_guard: bool,
    },
}

pub async fn dispatch(cmd: PluginCmd) -> Result<(), CliError> {
    match cmd {
        PluginCmd::Add {
            host,
            skip_guard,
            skip_plugin,
        } => add(host, skip_guard, skip_plugin).await,
        PluginCmd::List { json } => list(json),
        PluginCmd::Remove { host, with_guard } => remove(host, with_guard),
        PluginCmd::Update { host, skip_guard } => update(host, skip_guard),
    }
}

async fn add(host: String, skip_guard: bool, skip_plugin: bool) -> Result<(), CliError> {
    let h = parse_host(&host)?;

    if !skip_guard {
        ensure_guard()?;
    }
    if skip_plugin {
        println!("\nSkipped the {} plugin (--skip-plugin).", h.label());
        return Ok(());
    }
    install_host_plugin(h)?;
    // Best-effort adoption telemetry — bounded + swallowed, never blocks/fails the install.
    report_plugin_install(h).await;

    println!("\n[ok]   {} plugin add complete.", h.label());
    Ok(())
}

/// Report a successful plugin install to the API's install counter — from BOTH
/// `plugin add` and `init` (so init-driven installs are counted too). Especially
/// useful for Cursor, whose git-clone install produces no npm download signal.
/// Best-effort: opt-out-able, needs a key, short-timeout, every failure swallowed.
pub(crate) async fn report_plugin_install(h: Host) {
    // Privacy opt-out (either flag disables it).
    if env_opt_out("VAIBOT_NO_TELEMETRY") || env_opt_out("DO_NOT_TRACK") {
        return;
    }
    let env = current_env();
    let store = load_store(&credentials_path(&ProcessEnv));
    let key = match api_key_for_env(&store, env) {
        Some(k) => k,
        None => return, // no key → can't attribute; skip (the common add() path has one)
    };
    let client = match ApiClient::new(api_base_for_env(env, None), Some(key)) {
        Ok(c) => c,
        Err(_) => return,
    };
    let body = serde_json::json!({
        "host": h.key(),
        "action": "add",
        "cli_version": env!("CARGO_PKG_VERSION"),
        "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    });
    // Bounded so a slow/unreachable API can't hang the CLI; result ignored.
    let _ = tokio::time::timeout(
        Duration::from_secs(4),
        client.post::<serde_json::Value>("/v2/telemetry/plugin-install", Some(body)),
    )
    .await;
}

/// A telemetry opt-out env var is "on" when set to a non-empty value other than 0/false.
fn env_opt_out(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

/// Install + verify a single host's circuit-breaker plugin via its native CLI.
/// Assumes the caller already ensured the shared guard. Reused by `plugin add`
/// and `init`'s auto-detect.
pub fn install_host_plugin(h: Host) -> Result<(), CliError> {
    if matches!(h, Host::Cursor) {
        return install_cursor();
    }
    if matches!(h, Host::Hermes) {
        return install_hermes();
    }
    require_cli(h)?;
    for &(label, cmd) in h.install_steps() {
        run_narrated(label, cmd);
    }
    verify_after(h, true)?;
    if let Some(step) = h.manual_enable() {
        println!("\nFinish enabling in {}:\n  {}", h.label(), step);
    }
    Ok(())
}

// ── Hermes: place the published wheel, then enable ───────────────────────────
//
// Hermes has a plugin CLI, but only for enabling — the files have to arrive first, and
// the plugin is Python. Rather than shell out to pip (which fails outright on
// PEP 668 systems and picks whichever interpreter is on PATH), or reimplement
// fetch-and-verify here, this delegates to the published npm installer:
//
//   npx @vaibot/hermes-circuitbreaker-plugin install
//
// That installer fetches the wheel from PyPI and verifies it against a SHA-256 pinned
// at its own publish time. Delegating keeps ONE implementation of that check, the same
// reason the breakers call the guard's `classify` instead of growing a second
// classifier.
//
// Unlike the other hosts this does NOT use the best-effort `install_steps` loop: a
// digest mismatch has to be able to fail the whole operation, and that loop only warns.

fn install_hermes() -> Result<(), CliError> {
    require_cli(Host::Hermes)?;
    require_npx()?;

    let dir = installer::hermes_plugin_dir();
    println!("[step] Installing the Hermes plugin → {}", dir.display());
    println!(
        "       via npx @vaibot/hermes-circuitbreaker-plugin (fetches the published\n\
         \x20      wheel from PyPI and verifies its digest)"
    );

    // --force so a re-run replaces an existing install rather than refusing; the
    // installer keeps the previous copy as a .bak either way.
    if !installer::run_step("npx --yes @vaibot/hermes-circuitbreaker-plugin install --force") {
        println!(
            "[fail] Could not install the Hermes plugin.\n\
             \x20      The installer verifies the wheel against a pinned digest and refuses on a\n\
             \x20      mismatch, so this is either a network problem or something to look at. Run it\n\
             \x20      directly to see why:\n\
             \x20        npx @vaibot/hermes-circuitbreaker-plugin install --dry-run"
        );
        return Err(CliError::Runtime("hermes plugin install failed".into()));
    }
    println!("[ok]   Plugin files installed.");

    run_narrated("Enabling the plugin", "hermes plugins enable vaibot");
    verify_after(Host::Hermes, true)?;

    println!(
        "\nThe guard ships inside the plugin, so there is nothing else to install — it needs\n\
         Node on PATH to run. Inside Hermes, `/vaibot status` shows which guard answered.\n\
         `vaibot plugin update hermes` re-fetches and re-verifies the wheel."
    );
    Ok(())
}

fn remove_hermes(with_guard: bool) -> Result<(), CliError> {
    // Disable first so Hermes stops loading it, then remove the files — `plugins
    // disable` leaves the directory in place, so only doing one of the two would leave
    // a plugin that comes back on the next enable.
    run_narrated("Disabling the plugin", Host::Hermes.remove_cmd());

    let dir = installer::hermes_plugin_dir();
    println!("[step] Removing {}...", dir.display());
    if installer::remove_hermes_plugin() {
        println!("[ok]   Removed.");
    } else {
        println!("[warn] Could not remove {} — delete it manually.", dir.display());
    }

    if with_guard {
        remove_guard();
    } else {
        println!("\nLeft the shared guard in place — other hosts may use it. Pass --with-guard to remove it too.");
    }
    println!("\n[ok]   Hermes plugin remove complete.");
    Ok(())
}

/// `npx` ships with npm, which this CLI already requires for the guard — but say so
/// plainly rather than letting the install fail with a shell error.
fn require_npx() -> Result<(), CliError> {
    if which("npx").is_none() {
        println!(
            "[fail] `npx` not found on PATH. It ships with Node/npm, which the plugin needs anyway\n\
             \x20      (the guard is a Node program). Install Node, then re-run."
        );
        return Err(CliError::Runtime("npx not found".into()));
    }
    Ok(())
}

// ── Cursor: local-clone install ───────────────────────────────────────────────
// Cursor has no plugin-install CLI, so we clone the published repo into
// ~/.cursor/plugins/local/vaibot-cursor (Cursor loads local plugins from there).

fn install_cursor() -> Result<(), CliError> {
    require_git()?;
    let dir = installer::cursor_local_dir();
    println!("[step] Installing the Cursor plugin → {}", dir.display());
    if installer::install_cursor_plugin() {
        println!("[ok]   Installed.");
        println!(
            "\nFinish in Cursor:\n  \
             1. Restart Cursor (or run \"Developer: Reload Window\").\n  \
             2. If it isn't active, enable 'vaibot-cursor' in Customize.\n\n\
             This is a local install — `vaibot plugin update cursor` pulls new versions.\n\
             Prefer auto-updates? Import the repo as a marketplace in Cursor's Dashboard instead."
        );
    } else {
        println!("[fail] Could not install the Cursor plugin. Ensure `git` is installed and github.com is reachable, then re-run.");
        return Err(CliError::Runtime("cursor plugin install failed".into()));
    }
    Ok(())
}

fn update_cursor(skip_guard: bool) -> Result<(), CliError> {
    require_git()?;
    if !skip_guard {
        update_guard();
    }
    println!("[step] Updating the Cursor plugin (git pull)...");
    if installer::update_cursor_plugin() {
        println!("[ok]   Updated. Restart Cursor to load the new version.");
    } else {
        println!("[warn] Could not update — run `vaibot plugin add cursor` to reinstall.");
    }
    println!("\n[ok]   Cursor plugin update complete.");
    Ok(())
}

fn remove_cursor(with_guard: bool) -> Result<(), CliError> {
    let dir = installer::cursor_local_dir();
    println!("[step] Removing the Cursor plugin ({})...", dir.display());
    if installer::remove_cursor_plugin() {
        println!("[ok]   Removed. Restart Cursor to unload it.");
    } else {
        println!("[warn] Could not remove {} — delete it manually.", dir.display());
    }
    if with_guard {
        remove_guard();
    } else {
        println!("\nLeft the shared guard in place — other hosts may use it. Pass --with-guard to remove it too.");
    }
    println!("\n[ok]   Cursor plugin remove complete.");
    Ok(())
}

fn require_git() -> Result<(), CliError> {
    if which("git").is_none() {
        println!("[fail] `git` not found on PATH — it's required to install the Cursor plugin. Install git, then re-run.");
        return Err(CliError::Runtime("git not found".into()));
    }
    Ok(())
}

fn remove(host: String, with_guard: bool) -> Result<(), CliError> {
    let h = parse_host(&host)?;
    if matches!(h, Host::Cursor) {
        return remove_cursor(with_guard);
    }
    if matches!(h, Host::Hermes) {
        return remove_hermes(with_guard);
    }
    require_cli(h)?;

    let cmd = h.remove_cmd();
    println!("[step] Removing {} plugin...", h.label());
    if installer::run_step(cmd) {
        println!("[ok]   Removed");
    } else {
        println!("[warn] Remove failed — try manually: {cmd}");
    }
    verify_after(h, false)?;

    if with_guard {
        remove_guard();
    } else {
        println!("\nLeft the shared guard in place — other hosts may use it. Pass --with-guard to remove it too.");
    }

    println!("\n[ok]   {} plugin remove complete.", h.label());
    Ok(())
}

fn update(host: String, skip_guard: bool) -> Result<(), CliError> {
    let h = parse_host(&host)?;
    update_host_plugin(h, skip_guard)
}

/// Update one host's plugin. Split out of `update` so `vaibot update` can drive every
/// host without re-parsing a string, and can update the shared guard ONCE rather than
/// once per host.
pub fn update_host_plugin(h: Host, skip_guard: bool) -> Result<(), CliError> {
    if matches!(h, Host::Cursor) {
        return update_cursor(skip_guard);
    }
    require_cli(h)?;
    // Hermes' update step IS the npx installer, so say why it cannot run rather than
    // letting the generic loop report an opaque "Updating plugin failed".
    if matches!(h, Host::Hermes) {
        require_npx()?;
    }

    if !skip_guard {
        update_guard();
    }
    for &(label, cmd) in h.update_steps() {
        run_narrated(label, cmd);
    }
    verify_after(h, true)?;

    println!("\n[ok]   {} plugin update complete.", h.label());
    Ok(())
}

// ── shared helpers ───────────────────────────────────────────────────────────

fn parse_host(host: &str) -> Result<Host, CliError> {
    Host::parse(host).ok_or_else(|| {
        println!("[fail] Unknown host \"{host}\". Use one of: claudecode | codex | openclaw | cursor | hermes.");
        CliError::Runtime(format!("unknown host: {host}"))
    })
}

fn require_cli(h: Host) -> Result<(), CliError> {
    if !h.cli_present() {
        println!(
            "[fail] {} CLI (`{}`) not found on PATH. Install {} first, then re-run.",
            h.label(),
            h.cli(),
            h.label()
        );
        return Err(CliError::Runtime(format!("{} not found", h.cli())));
    }
    Ok(())
}

fn run_narrated(label: &str, cmd: &str) {
    println!("[step] {label}...");
    if installer::run_step(cmd) {
        println!("[ok]   {label}");
    } else {
        println!("[warn] {label} failed — try manually: {cmd}");
    }
}

/// Post-op verification of plugin presence. `expect=true` after add/update,
/// `false` after remove. Errors when the host CAN verify and the result
/// contradicts expectation; warns when the host can't verify via CLI (codex).
fn verify_after(h: Host, expect: bool) -> Result<(), CliError> {
    match h.verify_installed() {
        Some(present) if present == expect => {
            println!(
                "[ok]   Verified: {} plugin is {}.",
                h.label(),
                if expect { "installed" } else { "removed" }
            );
            Ok(())
        }
        Some(_) => {
            let what = if expect {
                "not detected after install"
            } else {
                "still present after remove"
            };
            println!("[fail] Verification failed — {} plugin {what}.", h.label());
            Err(CliError::Runtime("plugin verification failed".into()))
        }
        None => {
            println!("[warn] Can't auto-verify {} via its CLI.", h.label());
            Ok(())
        }
    }
}

/// Ensure the shared (host-agnostic) guard is installed.
fn ensure_guard() -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    match resolved.api_key {
        // The guard self-derives key + bases from the creds store (v3); we only
        // gate on a resolvable key to fail fast with a clear message.
        Some(_) => setup::install_guard(),
        None => {
            println!("[fail] No API key found. Run `vaibot init` or `vaibot login` first.");
            Err(CliError::Runtime("no api key".into()))
        }
    }
}

/// What `vaibot update` calls: the guard once, then every plugin that is actually
/// installed.
///
/// "Installed" is decided per host rather than assumed:
///
/// * Host CLI absent — the host is not on this machine; skip silently, because reporting
///   a failure for software the person never installed is noise, not information.
/// * Plugin verifiably absent — say so and name the command that installs it. Updating
///   something that is not there would otherwise look like it had worked.
/// * Unverifiable (codex, cursor) — attempt it, and say that the result could not be
///   confirmed. Better to try and be honest than to skip a host because its CLI has no
///   way to ask.
///
/// Returns (attempted, failed) so the caller can set an exit code without this function
/// deciding what an overall failure means.
pub fn update_everything_installed(skip_guard: bool) -> (usize, usize) {
    let mut guard_failed = false;
    if !skip_guard {
        // Unlike `plugin update <host>`, a whole-stack update reports a guard failure:
        // the person asked for the guard specifically, so silently warning would make
        // the command claim more than it did.
        if let Err(e) = crate::commands::guard::update() {
            guard_failed = true;
            println!("[warn] Guard update failed: {e}");
        }
        println!();
    }

    let mut attempted = 0usize;
    let mut failed = 0usize;
    let mut absent: Vec<&'static str> = Vec::new();
    let mut not_installed: Vec<&'static str> = Vec::new();

    for h in Host::ALL {
        if !h.cli_present() {
            absent.push(h.label());
            continue;
        }
        if h.verify_installed() == Some(false) {
            not_installed.push(h.key());
            continue;
        }
        println!("── {} ──", h.label());
        attempted += 1;
        // skip_guard: true — the guard was handled once above, and updating it per host
        // would restart the daemon four times over.
        if let Err(e) = update_host_plugin(h, true) {
            failed += 1;
            println!("[warn] {} did not update: {e}", h.label());
        }
        println!();
    }

    if !absent.is_empty() {
        println!("Not on this machine, so nothing to update: {}", absent.join(", "));
    }
    for key in &not_installed {
        println!("Host present but the plugin is not installed — `vaibot plugin add {key}`");
    }
    if guard_failed {
        failed += 1;
    }
    (attempted, failed)
}

/// The guard half of `plugin add/update <host>`.
///
/// One implementation of "update the guard" lives in commands::guard; this is the
/// call site that treats a failure as a WARNING rather than an error, because the
/// person asked to update a host's plugin and a stale-but-working shared guard should
/// not fail that. `vaibot guard update` and `vaibot update` take the error.
fn update_guard() {
    if let Err(e) = crate::commands::guard::update() {
        println!("[warn] The plugin update continues despite this: {e}");
    }
}

fn remove_guard() {
    println!("[step] Removing the shared guard...");
    if installer::disable_systemd_service() {
        println!("[ok]   vaibot-guard.service disabled");
    } else {
        println!("[warn] Could not disable the systemd unit (may not be installed).");
    }
    if installer::uninstall_guard() {
        println!("[ok]   Guard uninstalled (npm rm -g @vaibot/guard)");
    } else {
        println!("[warn] Could not uninstall the guard — try: npm uninstall -g @vaibot/guard");
    }
}

/// What the plugin is doing on one host.
///
/// `Unknown` is deliberately distinct from "not installed": codex and cursor expose no
/// scriptable check, so claiming either answer for them would be inventing one.
enum PluginState {
    Installed,
    NotInstalled,
    Unknown,
}

impl PluginState {
    fn json(&self) -> &'static str {
        match self {
            PluginState::Installed => "installed",
            PluginState::NotInstalled => "not-installed",
            PluginState::Unknown => "unknown",
        }
    }
}

/// Build the `--json` document.
///
/// Pure on purpose: the shape is the part with consumers, so it is unit-testable
/// without interrogating a single host.
///
/// ## Two host maps, deliberately
///
/// `allHosts` is the real one — every host the CLI supports, each an object keyed by
/// the word `plugin add` accepts.
///
/// `hosts` is a **compatibility shim, frozen at the shape 0.6.2 published**. It cannot
/// simply be widened, because `hosts.codex` and `hosts.claudeCode` ship as bare
/// booleans there and an object in their place is a type error for anything reading
/// them — so the three keys, their types and their nesting stay exactly as they were,
/// `guardSkill` included, sitting inside `openclaw` where it never belonged. New
/// consumers should read `allHosts`; `hosts` is additive-only and goes away in the next
/// major.
///
/// Both are derived from the same `rows`, not from a second set of `which()` calls.
/// Every one of these surfaces fell behind precisely because it kept its own private
/// copy of "which hosts exist".
fn list_report(
    rows: &[(Host, bool, PluginState)],
    guard_skill: bool,
    guard_service: &str,
) -> serde_json::Value {
    let all_hosts: serde_json::Map<String, serde_json::Value> = rows
        .iter()
        .map(|(h, present, state)| {
            (
                h.key().to_string(),
                serde_json::json!({ "present": present, "plugin": state.json() }),
            )
        })
        .collect();

    // (present, plugin confirmed installed) for one host, or (false, false) if absent
    // from rows — which cannot happen while rows is built from Host::ALL, but returning
    // a value beats an unwrap on a JSON path.
    let find = |want: Host| {
        rows.iter()
            .find(|(h, _, _)| *h == want)
            .map(|(_, present, state)| (*present, matches!(state, PluginState::Installed)))
            .unwrap_or((false, false))
    };
    let (openclaw_present, openclaw_installed) = find(Host::Openclaw);
    let (claude_present, _) = find(Host::Claudecode);
    let (codex_present, _) = find(Host::Codex);

    serde_json::json!({
        "guardSkill": guard_skill,
        "guardService": guard_service,
        "allHosts": all_hosts,
        // Frozen — see above. 0.6.2 computed circuitBreaker as
        // `openclaw_present && <openclaw plugins list contains "circuit-breaker">`,
        // which is what (present, Installed) means for that host.
        "hosts": {
            "openclaw": {
                "present": openclaw_present,
                "guardSkill": guard_skill,
                "circuitBreaker": openclaw_present && openclaw_installed,
            },
            "claudeCode": claude_present,
            "codex": codex_present,
        },
    })
}

fn list(json: bool) -> Result<(), CliError> {
    let guard_skill = installer::guard_skill_exists();
    let guard_service = if is_active_systemd_unit("vaibot-guard") {
        "active"
    } else {
        "unknown"
    };

    // Driven off Host::ALL, not a hand-written list. The previous version named
    // openclaw, claude and codex, so cursor and hermes could not be reported at all —
    // and `plugin list` is exactly where someone looks to confirm an install.
    let rows: Vec<(Host, bool, PluginState)> = Host::ALL
        .into_iter()
        .map(|h| {
            let present = h.cli_present();
            // Only interrogate a host that is actually here: verify_installed shells
            // out, and asking an absent CLI just fails slowly.
            let state = if !present {
                PluginState::Unknown
            } else {
                match h.verify_installed() {
                    Some(true) => PluginState::Installed,
                    Some(false) => PluginState::NotInstalled,
                    None => PluginState::Unknown,
                }
            };
            (h, present, state)
        })
        .collect();

    if json {
        let report = list_report(&rows, guard_skill, guard_service);
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        return Ok(());
    }

    println!("Hosts:");
    for (h, present, state) in &rows {
        // The key, not the label, because it is the word `plugin add` takes.
        let key = h.key();
        if !present {
            println!("  {key:<12} not found");
            continue;
        }
        let detail = match state {
            PluginState::Installed => "plugin installed".to_string(),
            PluginState::NotInstalled => {
                format!("plugin not installed — `vaibot plugin add {key}`")
            }
            PluginState::Unknown => "plugin unknown (host has no scriptable check)".to_string(),
        };
        println!("  {key:<12} present      {detail}");
    }

    println!();
    println!("  {:<15} {}", "guard skill:", yes_no(guard_skill));
    println!("  {:<15} {}", "guard service:", guard_service);
    Ok(())
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "installed"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every host, as `list()` would build them for a machine with everything present
    /// and openclaw's plugin confirmed installed.
    fn rows_all_present() -> Vec<(Host, bool, PluginState)> {
        Host::ALL
            .into_iter()
            .map(|h| {
                let state = match h {
                    Host::Openclaw | Host::Claudecode | Host::Hermes => PluginState::Installed,
                    // No scriptable check on these two.
                    Host::Codex | Host::Cursor => PluginState::Unknown,
                };
                (h, true, state)
            })
            .collect()
    }

    /// The shim's whole purpose: a consumer written against 0.6.2 must keep working
    /// unchanged. This pins the three legacy keys, their TYPES and their nesting.
    ///
    /// The types are the point — `claudeCode` and `codex` ship as bare booleans, which
    /// is why `hosts` could not simply be widened to carry five hosts.
    #[test]
    fn hosts_stays_frozen_at_the_0_6_2_shape() {
        let report = list_report(&rows_all_present(), true, "active");
        let hosts = report.get("hosts").expect("hosts present");

        // Exactly the three keys 0.6.2 published — no more, no fewer.
        let keys: std::collections::BTreeSet<&str> =
            hosts.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["openclaw", "claudeCode", "codex"].into_iter().collect();
        assert_eq!(keys, expected, "the frozen `hosts` map gained or lost a key");

        // Bare booleans, not objects.
        assert_eq!(hosts["claudeCode"], serde_json::json!(true));
        assert_eq!(hosts["codex"], serde_json::json!(true));

        // openclaw keeps its object, guardSkill nested inside it.
        assert_eq!(
            hosts["openclaw"],
            serde_json::json!({ "present": true, "guardSkill": true, "circuitBreaker": true })
        );

        // guardService stayed top-level in 0.6.2 and must remain there.
        assert_eq!(report["guardService"], serde_json::json!("active"));
    }

    /// 0.6.2 computed `circuitBreaker` as openclaw-present AND the plugin confirmed, so
    /// an openclaw that is present but has no plugin reports false, not true.
    #[test]
    fn legacy_circuit_breaker_is_false_when_the_plugin_is_absent() {
        let rows: Vec<(Host, bool, PluginState)> = vec![
            (Host::Openclaw, true, PluginState::NotInstalled),
            (Host::Claudecode, false, PluginState::Unknown),
            (Host::Codex, false, PluginState::Unknown),
        ];
        let report = list_report(&rows, true, "unknown");
        assert_eq!(report["hosts"]["openclaw"]["circuitBreaker"], serde_json::json!(false));
        assert_eq!(report["hosts"]["openclaw"]["present"], serde_json::json!(true));
        // An absent host reports false rather than being omitted.
        assert_eq!(report["hosts"]["claudeCode"], serde_json::json!(false));
    }

    /// The new map is the one that carries every host — including the two the old shape
    /// could not express at all.
    #[test]
    fn all_hosts_covers_every_supported_host() {
        let report = list_report(&rows_all_present(), true, "active");
        let all = report["allHosts"].as_object().expect("allHosts is an object");

        assert_eq!(all.len(), Host::ALL.len(), "allHosts must cover Host::ALL");
        for h in Host::ALL {
            let entry = all
                .get(h.key())
                .unwrap_or_else(|| panic!("{} missing from allHosts", h.key()));
            assert!(entry.get("present").is_some(), "{} has no `present`", h.key());
            assert!(entry.get("plugin").is_some(), "{} has no `plugin`", h.key());
        }

        // The two hosts the frozen shape cannot represent.
        assert_eq!(all["hermes"]["plugin"], serde_json::json!("installed"));
        assert_eq!(all["cursor"]["plugin"], serde_json::json!("unknown"));
    }

    /// "Unknown" must never be reported as "not installed" — codex and cursor expose no
    /// check, and inventing a negative answer would send someone chasing a non-problem.
    #[test]
    fn unknown_is_not_reported_as_not_installed() {
        let report = list_report(&rows_all_present(), true, "active");
        for key in ["codex", "cursor"] {
            assert_eq!(
                report["allHosts"][key]["plugin"],
                serde_json::json!("unknown"),
                "{key} must report unknown, not a made-up answer"
            );
        }
    }
}
