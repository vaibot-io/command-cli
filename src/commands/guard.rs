//! `guard` group.
//!   serve   [SHELL-OUT] — execs the separate guard binary.
//!   status  [REAL] systemd + /health.   restart/stop [REAL] systemctl.
//!   logs    [REAL] journalctl.           policy [REAL] GET /v1/policy.
//!   verify/provision-offline [STUB].

use clap::Subcommand;

use crate::commands::setup::install_guard;
use crate::config::creds::{api_base_for_env, load_store, resolve_credentials};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::{CliError, ExitCode};
use crate::services::child::{run_foreground, SpawnOptions};
use crate::services::guard_bin::{locate_guard_bin, GuardSource, GUARD_SINGLETON_PORT};
use crate::services::guard_http;
use crate::services::installer;
use crate::services::{is_active_systemd_unit, systemd_available};

#[derive(Subcommand, Debug)]
pub enum GuardCmd {
    /// Install the guard (npm `@vaibot/guard`) + env file + systemd unit.
    Install,
    /// Run the guard service (shells out to the separate binary).
    Serve {
        /// Args forwarded verbatim to the guard binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
    /// Check guard service health.
    Status,
    /// Restart the guard service (systemctl --user restart).
    Restart,
    /// Stop the guard service (systemctl --user stop).
    Stop,
    /// Tail the guard service logs (journalctl).
    Logs {
        /// Number of recent lines to show.
        #[arg(short = 'n', long = "lines", default_value_t = 100)]
        lines: usize,
        /// Follow the log (Ctrl-C to stop).
        #[arg(short = 'f', long = "follow")]
        follow: bool,
    },
    /// Show the guard's active policy (GET /v1/policy).
    Policy,
    /// Verify a signed policy bundle offline (not yet wired).
    Verify,
    /// Provision the guard for air-gapped use (not yet wired).
    #[command(name = "provision-offline")]
    ProvisionOffline,
}

pub async fn dispatch(cmd: GuardCmd) -> Result<(), CliError> {
    match cmd {
        GuardCmd::Install => install(),
        GuardCmd::Serve { passthrough } => serve(passthrough).await,
        GuardCmd::Status => guard_http::run_guard_status().await,
        GuardCmd::Restart => restart(),
        GuardCmd::Stop => stop(),
        GuardCmd::Logs { lines, follow } => logs(lines, follow).await,
        GuardCmd::Policy => guard_http::run_policy_list().await,
        GuardCmd::Verify => Err(CliError::stub("guard verify")),
        GuardCmd::ProvisionOffline => Err(CliError::stub("guard provision-offline")),
    }
}

/// `vaibot guard install` — first-class, host-agnostic guard install
/// (npm `@vaibot/guard` + env file + systemd unit). Same path `init` runs.
fn install() -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let resolved = resolve_credentials(&ProcessEnv, &store);
    let Some(key) = resolved.api_key.clone() else {
        println!("[fail] No API key found. Run `vaibot init` or `vaibot login` first.");
        return Err(CliError::Runtime("no api key".into()));
    };
    let base = api_base_for_env(resolved.env, Some(&resolved.api_base_url));
    install_guard(resolved.env, &base, &key)
}

/// SHELL-OUT: locate the separate guard binary and exec it; never embed it.
async fn serve(passthrough: Vec<String>) -> Result<(), CliError> {
    let Some(loc) = locate_guard_bin() else {
        eprintln!("vaibot guard serve: no guard binary found.\n");
        eprintln!("The guard is a SEPARATE service — the CLI orchestrates it, it does not host it.");
        eprintln!("It runs as a per-host singleton on port {GUARD_SINGLETON_PORT}.\n");
        eprintln!("Install it (recommended):");
        eprintln!("  npm install -g @vaibot/guard   # provides vaibot-guard + vaibot-guard-service");
        eprintln!("  systemctl --user enable --now vaibot-guard   # if the unit is installed\n");
        eprintln!("Or point the CLI at the binary:");
        eprintln!("  export VAIBOT_GUARD_BIN=/path/to/vaibot-guard-service");
        std::process::exit(ExitCode::Error as i32);
    };

    let source = match loc.source {
        GuardSource::Env => "env",
        GuardSource::Path => "PATH",
        GuardSource::Service => "service",
    };
    eprintln!(
        "Starting guard via {source} ({}) on :{GUARD_SINGLETON_PORT}. Ctrl-C to stop.",
        loc.bin
    );
    let mut args = loc.args;
    args.extend(passthrough);
    let code = run_foreground(&loc.bin, SpawnOptions { args }).await?;
    std::process::exit(code);
}

/// `vaibot guard restart` — restart the systemd-managed guard.
fn restart() -> Result<(), CliError> {
    println!("\nRestarting the VAIBot guard...\n");
    if systemd_available() && installer::restart_systemd_service() {
        println!("  [ok]   vaibot-guard.service restarted");
        Ok(())
    } else {
        println!("  [warn] Couldn't restart via systemd (`systemctl --user restart vaibot-guard`).");
        println!("  If the guard is plugin-spawned (no systemd unit), it relaunches on the next tool call.");
        Ok(())
    }
}

/// `vaibot guard stop` — stop a systemd-managed guard, else guide.
fn stop() -> Result<(), CliError> {
    println!("\nStopping the VAIBot guard...\n");
    if systemd_available() && is_active_systemd_unit("vaibot-guard") {
        if installer::run_step("systemctl --user stop vaibot-guard") {
            println!("  [ok]   vaibot-guard.service stopped");
            return Ok(());
        }
        println!("  [warn] `systemctl --user stop vaibot-guard` failed.");
    }
    println!("  [info] The guard isn't managed by systemd here (it's plugin-spawned on demand).");
    println!("  A plugin-spawned guard exits on its own; otherwise stop the vaibot-guard-service process.");
    Ok(())
}

/// `vaibot guard logs` — tail the guard service journald logs.
async fn logs(lines: usize, follow: bool) -> Result<(), CliError> {
    if !systemd_available() {
        println!("\nThe guard's service logs are via journald, but systemd isn't available here.");
        println!("If the guard is plugin-spawned, its tamper-evident audit log lives under");
        println!("  <workspace>/.vaibot-guard/   (the JSONL event chain).");
        return Ok(());
    }
    let mut args = vec![
        "--user".to_string(),
        "-u".to_string(),
        "vaibot-guard".to_string(),
        "-n".to_string(),
        lines.to_string(),
    ];
    if follow {
        args.push("-f".to_string());
    }
    let code = run_foreground("journalctl", SpawnOptions { args }).await?;
    std::process::exit(code);
}
