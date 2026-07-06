//! `plugin` group.
//!   add   [REAL] — ensure the shared guard, then install the host plugin via its native CLI + verify.
//!   list  [REAL,--json].
//!   remove [REAL] — uninstall the host plugin (+ --with-guard for the shared guard).
//!   update [REAL] — re-pull the guard + host plugin to latest.

use clap::Subcommand;

use crate::config::creds::{load_store, resolve_credentials};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;
use crate::services::host::Host;
use crate::services::installer;
use crate::services::{is_active_systemd_unit, which};

use super::setup;

#[derive(Subcommand, Debug)]
pub enum PluginCmd {
    /// Install a host's circuit-breaker plugin (and ensure the shared guard).
    Add {
        /// Target host: claudecode | codex | openclaw | cursor.
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
        /// Target host: claudecode | codex | openclaw | cursor.
        #[arg(default_value = "openclaw")]
        host: String,
        /// Also uninstall the SHARED guard (npm + systemd). Off by default —
        /// the guard is shared across hosts.
        #[arg(long = "with-guard")]
        with_guard: bool,
    },
    /// Upgrade a VAIBot host integration (guard + circuit-breaker plugin).
    Update {
        /// Target host: claudecode | codex | openclaw | cursor.
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
        } => add(host, skip_guard, skip_plugin),
        PluginCmd::List { json } => list(json),
        PluginCmd::Remove { host, with_guard } => remove(host, with_guard),
        PluginCmd::Update { host, skip_guard } => update(host, skip_guard),
    }
}

fn add(host: String, skip_guard: bool, skip_plugin: bool) -> Result<(), CliError> {
    let h = parse_host(&host)?;

    if !skip_guard {
        ensure_guard()?;
    }
    if skip_plugin {
        println!("\nSkipped the {} plugin (--skip-plugin).", h.label());
        return Ok(());
    }
    install_host_plugin(h)?;

    println!("\n[ok]   {} plugin add complete.", h.label());
    Ok(())
}

/// Install + verify a single host's circuit-breaker plugin via its native CLI.
/// Assumes the caller already ensured the shared guard. Reused by `plugin add`
/// and `init`'s auto-detect.
pub fn install_host_plugin(h: Host) -> Result<(), CliError> {
    if matches!(h, Host::Cursor) {
        return install_cursor();
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
    if matches!(h, Host::Cursor) {
        return update_cursor(skip_guard);
    }
    require_cli(h)?;

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
        println!("[fail] Unknown host \"{host}\". Use one of: claudecode | codex | openclaw | cursor.");
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

fn update_guard() {
    println!("[step] Updating the guard (npm)...");
    if installer::install_guard_skill() {
        println!("[ok]   Guard updated (npm i -g {})", installer::GUARD_NPM_SPEC);
    } else {
        println!("[warn] Could not update the guard — try: npm install -g {}", installer::GUARD_NPM_SPEC);
    }
    println!("[step] Restarting guard service...");
    if installer::restart_systemd_service() {
        println!("[ok]   vaibot-guard.service restarted");
    } else {
        println!("[warn] Could not restart the guard service (may not be systemd-managed).");
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

fn list(json: bool) -> Result<(), CliError> {
    let openclaw = which("openclaw").is_some();
    let guard_skill = installer::guard_skill_exists();
    let circuit_breaker = openclaw && installer::verify_plugin();
    let claude = which("claude").is_some();
    let codex = which("codex").is_some();
    let guard_service = if is_active_systemd_unit("vaibot-guard") {
        "active"
    } else {
        "unknown"
    };

    if json {
        let report = serde_json::json!({
            "hosts": {
                "openclaw": { "present": openclaw, "guardSkill": guard_skill, "circuitBreaker": circuit_breaker },
                "claudeCode": claude,
                "codex": codex,
            },
            "guardService": guard_service,
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        return Ok(());
    }

    println!("Installed hosts:");
    println!("  openclaw:       {}", present(openclaw));
    println!("    guard skill:    {}", yes_no(guard_skill));
    println!("    circuit-breaker:{}", if circuit_breaker { " installed" } else { " no" });
    println!("  claude-code:    {}", present(claude));
    println!("  codex:          {}", present(codex));
    println!("  guard service:  {guard_service}");
    Ok(())
}

fn present(b: bool) -> &'static str {
    if b {
        "present"
    } else {
        "not found"
    }
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "installed"
    } else {
        "no"
    }
}
