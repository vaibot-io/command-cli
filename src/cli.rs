//! The clap command tree — the single composition root for the whole CLI.
//!
//! `Cli` holds global args + the top-level `Command` enum. Each component group
//! carries a nested subcommand enum that lives IN that group's module (the
//! gateway's `Command`/`PolicyCmd` two-level derive shape). The `receipts`
//! variant is a TWIN of `provenance` pointing at the same `ProvenanceCmd` enum
//! and the same dispatch (a faithful port of the "registered as a sibling
//! top-level key" alias — clap can't add a nested-subcommand alias).
//!
//! `--version` reflects the crate version (`env!("CARGO_PKG_VERSION")`), so
//! `vaibot --version` always prints what `cargo install` resolved. It was
//! previously pinned to "0.3.0" to mirror the legacy TS CLI's version; that
//! froze `--version` while the crate advanced (0.4.x+), so it was unpinned.

use clap::{Parser, Subcommand};

use crate::commands::{account::AccountCmd, gateway::GatewayCmd, guard::GuardCmd, mcp::McpCmd, mode::ModeCmd, plugin::PluginCmd, policy::PolicyCmd, provenance::ProvenanceCmd};

#[derive(Parser, Debug)]
#[command(
    name = "vaibot",
    version = env!("CARGO_PKG_VERSION"),
    about = "Install, govern, and observe the VAIBot stack",
    long_about = "VAIBot front-door CLI: one installer/supervisor for guard, gateway, \
plugins, policy, and MCP across the machine-intelligence lifecycle. The CLI is an \
orchestrator, NOT a runtime — `guard serve` and `gateway serve` shell out to the \
separate daemons; they are never embedded."
)]
pub struct Cli {
    /// Override the OAuth issuer / API URL (staging / self-host).
    #[arg(long = "api-url", global = true)]
    pub api_url: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    // ── auth ──
    /// Log in to VAIBot (browser loopback PKCE, or --device).
    Login {
        /// Use the device-code flow (authorize on another device).
        #[arg(long)]
        device: bool,
        /// Print the auth URL instead of opening a browser.
        #[arg(long = "no-browser")]
        no_browser: bool,
    },
    /// Clear the local VAIBot session.
    Logout {
        /// Revoke keys minted for all components (not yet wired).
        #[arg(long = "all-hosts")]
        all_hosts: bool,
    },
    /// Show the current VAIBot identity.
    Whoami {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Account identity — link this machine to your real account.
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },

    // ── lifecycle ──
    /// Onboard: log in, then install + configure the VAIBot stack.
    Init {
        /// Skip prompts, use defaults.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Environment: staging | production.
        #[arg(long)]
        env: Option<String>,
        /// Provide an API key (skips login/bootstrap).
        #[arg(long = "api-key")]
        api_key: Option<String>,
        /// Skip the interactive OAuth login.
        #[arg(long = "skip-login")]
        skip_login: bool,
        /// Also set up the gateway (not yet wired).
        #[arg(long = "with-gateway")]
        with_gateway: bool,
        /// Also register the VAIBot MCP server with every detected agent.
        #[arg(long = "with-mcp")]
        with_mcp: bool,
        /// Set your governance floor at activation: permissive | balanced | strict.
        #[arg(long = "preset")]
        preset: Option<String>,
    },
    /// Auth context, API health, and quota.
    Status {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Verify the full stack (guard + plugin + API).
    Doctor {
        /// Attempt auto-remediation (not yet wired).
        #[arg(long)]
        fix: bool,
    },
    /// Update the VAIBot CLI and components (not yet wired).
    Update,

    // ── component groups ──
    /// Manage the local guard service.
    Guard {
        #[command(subcommand)]
        cmd: GuardCmd,
    },
    /// Manage the local-first LLM gateway proxy.
    Gateway {
        #[command(subcommand)]
        cmd: GatewayCmd,
    },
    /// Install / manage VAIBot host integrations.
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,
    },
    /// View and change the governed policy.
    Policy {
        #[command(subcommand)]
        cmd: PolicyCmd,
    },
    /// View / set the governance mode (observe | enforce).
    Mode {
        #[command(subcommand)]
        cmd: ModeCmd,
    },
    /// Connect this host to the VAIBot MCP server.
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    /// Browse, follow, and verify governance receipts.
    Provenance {
        #[command(subcommand)]
        cmd: ProvenanceCmd,
    },
    /// Alias of `provenance` (historical noun) — same subcommands.
    Receipts {
        #[command(subcommand)]
        cmd: ProvenanceCmd,
    },
}
