//! `gateway` group.
//!   serve  [SHELL-OUT] — execs the separate Rust gateway binary.
//!   status [REAL] — systemd (if any) + GET /healthz.
//!   config [REAL] — print the resolved vaibot-gateway.toml.
//!   stop   [REAL] — systemctl stop (if managed), else guidance.
//!   logs   [REAL] — shells out to `vaibot-gateway inspect` (egress log).

use std::time::Duration;

use clap::Subcommand;

use crate::error::{CliError, ExitCode};
use crate::services::child::{run_foreground, SpawnOptions};
use crate::services::gateway_bin::{
    gateway_config_path, locate_gateway_bin, resolve_gateway_base_url, GatewaySource,
    GATEWAY_DEFAULT_BASE_URL,
};
use crate::services::installer::run_step;
use crate::services::{is_active_systemd_unit, systemd_available};

#[derive(Subcommand, Debug)]
pub enum GatewayCmd {
    /// Run the gateway proxy (shells out to the Rust binary).
    Serve {
        /// Args forwarded verbatim to the gateway binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
    /// Show gateway status (systemd if any + GET /healthz).
    Status,
    /// Print the resolved gateway config (vaibot-gateway.toml).
    Config,
    /// Stop the gateway.
    Stop,
    /// Show recent gateway egress-log entries.
    Logs {
        /// Number of recent entries to show.
        #[arg(short = 'n', long = "lines", default_value_t = 50)]
        lines: usize,
    },
}

pub async fn dispatch(cmd: GatewayCmd) -> Result<(), CliError> {
    match cmd {
        GatewayCmd::Serve { passthrough } => serve(passthrough).await,
        GatewayCmd::Status => status().await,
        GatewayCmd::Config => config(),
        GatewayCmd::Stop => stop(),
        GatewayCmd::Logs { lines } => logs(lines).await,
    }
}

/// SHELL-OUT: locate the separate gateway binary and exec it; never embed it.
async fn serve(passthrough: Vec<String>) -> Result<(), CliError> {
    let Some(loc) = locate_gateway_bin() else {
        eprintln!("vaibot gateway serve: vaibot-gateway binary not found.\n");
        eprintln!("The gateway is a SEPARATE local-first LLM proxy (Rust) — the CLI");
        eprintln!("orchestrates it, it does not host it.\n");
        eprintln!("Once installed, route an agent through it by pointing its base URL at");
        eprintln!("the proxy, e.g.:");
        eprintln!("  export ANTHROPIC_BASE_URL={GATEWAY_DEFAULT_BASE_URL}\n");
        eprintln!("Point the CLI at the binary:");
        eprintln!("  export VAIBOT_GATEWAY_BIN=/path/to/vaibot-gateway");
        std::process::exit(ExitCode::Error as i32);
    };

    let source = match loc.source {
        GatewaySource::Env => "env",
        GatewaySource::Path => "PATH",
    };
    eprintln!("Starting gateway via {source} ({}). Ctrl-C to stop.", loc.bin);
    let mut args = loc.args;
    args.extend(passthrough);
    let code = run_foreground(&loc.bin, SpawnOptions { args }).await?;
    std::process::exit(code);
}

/// `vaibot gateway status` — systemd check (if a unit exists) + GET /healthz.
async fn status() -> Result<(), CliError> {
    println!("\nVAIBot Gateway Status\n");
    if systemd_available() && is_active_systemd_unit("vaibot-gateway") {
        println!("  [ok]   systemd service: active");
    }
    let base = resolve_gateway_base_url();
    println!("  [info] Pinging {base}/healthz ...");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| CliError::Runtime(format!("http client: {e}")))?;
    match client.get(format!("{base}/healthz")).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("  [ok]   Gateway is healthy (HTTP {})", resp.status().as_u16());
            println!("  [info] Route an agent through it: export ANTHROPIC_BASE_URL={base}");
            Ok(())
        }
        Ok(resp) => {
            println!("  [fail] Gateway returned HTTP {}", resp.status().as_u16());
            Err(CliError::Runtime("gateway not healthy".into()))
        }
        Err(_) => {
            println!("  [fail] Gateway not reachable at {base} (timeout).");
            println!("  Start it with `vaibot gateway serve`, or set VAIBOT_GATEWAY_BASE_URL.");
            Err(CliError::Runtime("gateway unreachable".into()))
        }
    }
}

/// `vaibot gateway config` — print the resolved vaibot-gateway.toml.
fn config() -> Result<(), CliError> {
    let path = gateway_config_path();
    println!("\nVAIBot Gateway Config\n");
    println!("  path: {}", path.display());
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            println!();
            for line in contents.lines() {
                println!("  {line}");
            }
            Ok(())
        }
        Err(_) => {
            println!("  [info] No config at that path yet. Create one with `vaibot-gateway init`.");
            Ok(())
        }
    }
}

/// `vaibot gateway stop` — stop a systemd-managed gateway, else guide.
fn stop() -> Result<(), CliError> {
    println!("\nStopping the VAIBot gateway...\n");
    if systemd_available() && is_active_systemd_unit("vaibot-gateway") {
        if run_step("systemctl --user stop vaibot-gateway") {
            println!("  [ok]   vaibot-gateway.service stopped");
            return Ok(());
        }
        println!("  [warn] `systemctl --user stop vaibot-gateway` failed.");
    }
    println!("  [info] The gateway isn't managed by systemd here.");
    println!("  If it's running in the foreground (`vaibot gateway serve`), stop it with Ctrl-C;");
    println!("  otherwise stop the `vaibot-gateway` process you started.");
    Ok(())
}

/// `vaibot gateway logs` — shell out to the daemon's egress-log viewer.
async fn logs(lines: usize) -> Result<(), CliError> {
    let Some(loc) = locate_gateway_bin() else {
        eprintln!("vaibot gateway logs: vaibot-gateway binary not found.");
        eprintln!("Install it, or set VAIBOT_GATEWAY_BIN, then retry.");
        std::process::exit(ExitCode::Error as i32);
    };
    // The gateway's per-request egress log is exposed via `vaibot-gateway inspect`.
    let mut args = loc.args;
    args.push("inspect".into());
    args.push("--limit".into());
    args.push(lines.to_string());
    let code = run_foreground(&loc.bin, SpawnOptions { args }).await?;
    std::process::exit(code);
}
