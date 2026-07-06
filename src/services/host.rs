//! Per-host plugin-manager dispatch for `vaibot plugin add/remove/update`.
//!
//! Each host wires the circuit-breaker plugin through its OWN native CLI:
//!   claudecode → `claude plugin ...`           (marketplace add + install; verifiable)
//!   openclaw   → `openclaw plugins ...`         (install npm spec; verifiable)
//!   codex      → `codex plugin marketplace ...` (register; ENABLE is an interactive picker)
//!   cursor     → NO plugin-install CLI. The published plugin is cloned into
//!                `~/.cursor/plugins/local/vaibot-cursor` (Cursor loads local plugins
//!                from there); add/remove/update are handled in commands::plugin
//!                (install_cursor/…), not the command-based steps below. Its MCP stays
//!                file-based (`~/.cursor/mcp.json`), so `is_file_based()` excludes it
//!                from `mcp connect`.
//! The shared guard is installed separately (host-agnostic, see setup::install_guard).

use super::{run_capture, which};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Claudecode,
    Codex,
    Openclaw,
    Cursor,
}

impl Host {
    /// Every host, for detection / iteration.
    pub const ALL: [Host; 4] = [Host::Claudecode, Host::Codex, Host::Openclaw, Host::Cursor];

    pub fn parse(s: &str) -> Option<Host> {
        match s.to_ascii_lowercase().as_str() {
            "claudecode" | "claude" | "claude-code" => Some(Host::Claudecode),
            "codex" => Some(Host::Codex),
            "openclaw" => Some(Host::Openclaw),
            "cursor" => Some(Host::Cursor),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Host::Claudecode => "Claude Code",
            Host::Codex => "Codex",
            Host::Openclaw => "OpenClaw",
            Host::Cursor => "Cursor",
        }
    }

    /// The host key as accepted by `vaibot plugin add <key>`.
    pub fn key(self) -> &'static str {
        match self {
            Host::Claudecode => "claudecode",
            Host::Codex => "codex",
            Host::Openclaw => "openclaw",
            Host::Cursor => "cursor",
        }
    }

    /// The host's native CLI binary name.
    pub fn cli(self) -> &'static str {
        match self {
            Host::Claudecode => "claude",
            Host::Codex => "codex",
            Host::Openclaw => "openclaw",
            Host::Cursor => "cursor",
        }
    }

    pub fn cli_present(self) -> bool {
        which(self.cli()).is_some()
    }

    /// Ordered install steps: (label, command). Each best-effort; run in sequence.
    pub fn install_steps(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Host::Claudecode => &[
                (
                    "Registering marketplace",
                    "claude plugin marketplace add vaibot-io/claudecode-circuitbreaker-plugin",
                ),
                (
                    "Installing plugin",
                    "claude plugin install vaibot-governance@vaibot-claudecode",
                ),
            ],
            Host::Codex => &[(
                "Registering marketplace",
                "codex plugin marketplace add vaibot-io/codex-circuitbreaker-plugin",
            )],
            Host::Openclaw => &[(
                "Installing plugin",
                "openclaw plugins install @vaibot/circuit-breaker-openclaw-plugin",
            )],
            // File-based: no install command — the caller prints setup guidance instead.
            Host::Cursor => &[],
        }
    }

    /// True when the host has no plugin-install CLI. Cursor: its MCP is file-based
    /// (`~/.cursor/mcp.json`), so `mcp connect` skips it; the circuit-breaker plugin
    /// is installed by cloning the published repo into `~/.cursor/plugins/local/`
    /// (see commands::plugin::install_cursor).
    pub fn is_file_based(self) -> bool {
        matches!(self, Host::Cursor)
    }

    /// Ordered update steps: (label, command).
    pub fn update_steps(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Host::Claudecode => &[
                (
                    "Updating marketplace",
                    "claude plugin marketplace update vaibot-claudecode",
                ),
                (
                    "Updating plugin",
                    "claude plugin update vaibot-governance@vaibot-claudecode",
                ),
            ],
            Host::Codex => &[(
                "Refreshing marketplace",
                "codex plugin marketplace add vaibot-io/codex-circuitbreaker-plugin",
            )],
            Host::Openclaw => &[("Updating plugins", "openclaw plugins update")],
            Host::Cursor => &[],
        }
    }

    /// Remove command.
    pub fn remove_cmd(self) -> &'static str {
        match self {
            Host::Claudecode => "claude plugin uninstall vaibot-governance@vaibot-claudecode",
            Host::Codex => "codex plugin marketplace remove vaibot-codex",
            Host::Openclaw => "openclaw plugins uninstall circuit-breaker-openclaw-plugin",
            // Unused — remove() short-circuits file-based hosts before running this.
            Host::Cursor => "true",
        }
    }

    /// Post-op verification — is the plugin detectably installed? `None` means the
    /// host exposes no scriptable check (codex has no `marketplace list`, and its
    /// enable is an interactive picker; cursor is the same today), so the caller
    /// can't confirm via CLI.
    pub fn verify_installed(self) -> Option<bool> {
        let (cmd, needle) = match self {
            Host::Claudecode => ("claude plugin list", "vaibot-governance"),
            Host::Openclaw => ("openclaw plugins list", "circuit-breaker"),
            Host::Codex | Host::Cursor => return None,
        };
        Some(
            run_capture(cmd)
                .map(|r| r.ok && r.stdout.contains(needle))
                .unwrap_or(false),
        )
    }

    /// A manual step the user must run when the host's enable is interactive.
    pub fn manual_enable(self) -> Option<&'static str> {
        match self {
            Host::Codex => Some("codex plugin   # then enable 'vaibot-codex' in the picker"),
            // Cursor's guidance is printed up front via file_setup() (file-based).
            _ => None,
        }
    }
}
