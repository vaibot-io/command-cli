//! `mcp` group — connect a host agent to the VAIBot MCP server.
//!
//! The VAIBot MCP server exposes the governance tools over HTTP (JSON-RPC 2.0) at
//! `{api_base}/v2/mcp`, authenticated by the customer's API key as an
//! `Authorization: Bearer` header (the endpoint accepts an api-key bearer as an
//! OAuth alternative — no per-host browser click). `connect` registers it through
//! each host's NATIVE MCP config; the CLI never runs a local MCP server.
//!
//! Per host (all remote-HTTP + bearer):
//!   claudecode → `claude mcp add --transport http vaibot <url> --header ... --scope user`
//!   codex      → `codex mcp add vaibot --url <url> --bearer-token-env-var VAIBOT_API_KEY`
//!   openclaw   → `openclaw mcp set vaibot '{"url":...,"headers":{...}}'`

use std::process::Command;

use clap::Subcommand;

use crate::config::creds::{api_base_for_env, api_key_for_env, load_store, VaibotEnv};
use crate::config::{credentials_path, ProcessEnv};
use crate::error::CliError;
use crate::services::host::Host;
use crate::services::run_capture;

use super::current_env;

/// The MCP server name registered with each host.
const MCP_NAME: &str = "vaibot";

#[derive(Subcommand, Debug)]
pub enum McpCmd {
    /// Register the VAIBot MCP server with a host (default: every detected agent).
    Connect {
        /// Target host: claudecode | codex | openclaw. Omit to connect all detected.
        host: Option<String>,
    },
    /// Show which detected hosts have the VAIBot MCP server registered.
    Status,
    /// Remove the VAIBot MCP server from a host (default: every detected agent).
    Disconnect {
        /// Target host: claudecode | codex | openclaw. Omit to remove from all.
        host: Option<String>,
    },
}

pub fn dispatch(cmd: McpCmd, api_url: Option<String>) -> Result<(), CliError> {
    match cmd {
        McpCmd::Connect { host } => connect(host, api_url),
        McpCmd::Status => status(),
        McpCmd::Disconnect { host } => disconnect(host),
    }
}

/// Resolve the target hosts: an explicit one (validated), or every host whose CLI
/// is on PATH.
fn targets(host: Option<String>) -> Result<Vec<Host>, CliError> {
    match host {
        Some(h) => {
            let host = Host::parse(&h).ok_or_else(|| {
                CliError::Runtime(format!(
                    "Unknown host \"{h}\". Use one of: claudecode | codex | openclaw."
                ))
            })?;
            Ok(vec![host])
        }
        None => Ok(Host::ALL.into_iter().filter(|h| h.cli_present()).collect()),
    }
}

/// `vaibot mcp connect [host]` — register the VAIBot MCP server for the active env.
pub fn connect(host: Option<String>, api_url: Option<String>) -> Result<(), CliError> {
    connect_to_env(current_env(), host, api_url.as_deref(), false)
}

/// Register the MCP server for an EXPLICIT env (used by `init` to reconcile to
/// production regardless of a staging shell override). `only_existing` restricts
/// to hosts that already have a `vaibot` entry — i.e. re-pin drift without adding
/// the integration to hosts that never had it.
pub fn connect_to_env(
    env: VaibotEnv,
    host: Option<String>,
    api_url: Option<&str>,
    only_existing: bool,
) -> Result<(), CliError> {
    let store = load_store(&credentials_path(&ProcessEnv));
    let key = api_key_for_env(&store, env).ok_or(CliError::Auth)?;
    let url = format!("{}/v2/mcp", api_base_for_env(env, api_url).trim_end_matches('/'));

    let mut hosts = targets(host)?;
    if only_existing {
        hosts.retain(|h| mcp_registered(*h));
    }
    if hosts.is_empty() {
        // Silent in reconcile mode (nothing to re-pin is the common case).
        if !only_existing {
            println!("No host agent detected (claude / codex / openclaw). Install one, then re-run.");
        }
        return Ok(());
    }

    let what = if only_existing { "Re-pinning the VAIBot MCP server" } else { "Connecting the VAIBot MCP server" };
    println!("{what}  (env: {env})");
    println!("  endpoint: {url}\n");
    let mut any_ok = false;
    for h in hosts {
        any_ok |= connect_one(h, &url, &key);
    }
    if any_ok && !only_existing {
        println!(
            "\nDone. Your agent can now call VAIBot governance tools directly\n  (status · pending · approve · deny · receipts · policy · …)."
        );
    }
    Ok(())
}

/// Register the server with one host via its native CLI (explicit argv — no shell,
/// so the bearer/JSON are never re-parsed by `sh`). Returns whether it succeeded.
fn connect_one(h: Host, url: &str, key: &str) -> bool {
    let args: Vec<String> = match h {
        Host::Claudecode => vec![
            "mcp".into(), "add".into(),
            "--transport".into(), "http".into(),
            MCP_NAME.into(), url.into(),
            "--header".into(), format!("Authorization: Bearer {key}"),
            "--scope".into(), "user".into(),
        ],
        Host::Codex => vec![
            "mcp".into(), "add".into(), MCP_NAME.into(),
            "--url".into(), url.into(),
            "--bearer-token-env-var".into(), "VAIBOT_API_KEY".into(),
        ],
        Host::Openclaw => {
            let json = serde_json::json!({
                "url": url,
                "headers": { "Authorization": format!("Bearer {key}") },
            })
            .to_string();
            vec!["mcp".into(), "set".into(), MCP_NAME.into(), json]
        }
    };

    println!("  {:<12} registering '{MCP_NAME}'…", h.label());
    // Idempotent: `claude/codex mcp add` error if the name already exists, so drop
    // any prior entry first (silent — a missing entry is not an error here).
    // OpenClaw's `mcp set` already upserts, so it needs no pre-remove.
    if matches!(h, Host::Claudecode | Host::Codex) {
        let _ = run_capture(&format!("{} mcp remove {MCP_NAME}", h.cli()));
    }
    // Inherit stdio so the host CLI's own confirmation / error is visible.
    let ok = Command::new(h.cli())
        .args(&args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        println!("  {:<12} ✓ connected", "");
        if let Host::Codex = h {
            println!(
                "  {:<12} note: Codex reads the token from $VAIBOT_API_KEY at runtime —\n  {:<12}       export it in your shell (e.g. add `export VAIBOT_API_KEY=…`\n  {:<12}       to ~/.bashrc) so the server authenticates.",
                "", "", ""
            );
        }
    } else {
        println!("  {:<12} ✗ failed — see the host CLI output above", "");
    }
    ok
}

/// `vaibot mcp status` — which detected hosts have `vaibot` registered.
fn status() -> Result<(), CliError> {
    println!("VAIBot MCP connection status\n");
    let mut any = false;
    for h in Host::ALL {
        if !h.cli_present() {
            continue;
        }
        any = true;
        let state = if mcp_registered(h) {
            "registered"
        } else {
            "not registered"
        };
        println!("  {:<12} {state}", h.label());
    }
    if !any {
        println!("  No host agent detected (claude / codex / openclaw).");
    }
    Ok(())
}

/// Is `vaibot` registered in the host's MCP config? Targets the exact server name
/// via the host's `get`/`show <name>` (exit 0 ⇒ present) — precise, so sibling
/// entries like `vaibot-prod` never false-positive.
fn mcp_registered(h: Host) -> bool {
    let cmd = match h {
        Host::Claudecode => format!("claude mcp get {MCP_NAME}"),
        Host::Codex => format!("codex mcp get {MCP_NAME}"),
        Host::Openclaw => format!("openclaw mcp show {MCP_NAME}"),
    };
    run_capture(&cmd).map(|r| r.ok).unwrap_or(false)
}

/// `vaibot mcp disconnect [host]` — remove the VAIBot MCP server.
fn disconnect(host: Option<String>) -> Result<(), CliError> {
    let hosts = targets(host)?;
    if hosts.is_empty() {
        println!("No host agent detected (claude / codex / openclaw).");
        return Ok(());
    }
    for h in hosts {
        let args: &[&str] = match h {
            Host::Claudecode => &["mcp", "remove", MCP_NAME],
            Host::Codex => &["mcp", "remove", MCP_NAME],
            Host::Openclaw => &["mcp", "unset", MCP_NAME],
        };
        let ok = Command::new(h.cli())
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        println!(
            "  {:<12} {}",
            h.label(),
            if ok { "disconnected" } else { "not connected / failed" }
        );
    }
    Ok(())
}
