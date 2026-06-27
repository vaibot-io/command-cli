//! Per-host plugin-manager dispatch for `vaibot plugin add/remove/update`.
//!
//! Each host wires the circuit-breaker plugin through its OWN native CLI:
//!   claudecode → `claude plugin ...`           (marketplace add + install; verifiable)
//!   openclaw   → `openclaw plugins ...`         (install npm spec; verifiable)
//!   codex      → `codex plugin marketplace ...` (register; ENABLE is an interactive picker)
//! The shared guard is installed separately (host-agnostic, see setup::install_guard).

use super::{run_capture, which};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Claudecode,
    Codex,
    Openclaw,
}

impl Host {
    /// Every host, for detection / iteration.
    pub const ALL: [Host; 3] = [Host::Claudecode, Host::Codex, Host::Openclaw];

    pub fn parse(s: &str) -> Option<Host> {
        match s.to_ascii_lowercase().as_str() {
            "claudecode" | "claude" | "claude-code" => Some(Host::Claudecode),
            "codex" => Some(Host::Codex),
            "openclaw" => Some(Host::Openclaw),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Host::Claudecode => "Claude Code",
            Host::Codex => "Codex",
            Host::Openclaw => "OpenClaw",
        }
    }

    /// The host key as accepted by `vaibot plugin add <key>`.
    pub fn key(self) -> &'static str {
        match self {
            Host::Claudecode => "claudecode",
            Host::Codex => "codex",
            Host::Openclaw => "openclaw",
        }
    }

    /// The host's native CLI binary name.
    pub fn cli(self) -> &'static str {
        match self {
            Host::Claudecode => "claude",
            Host::Codex => "codex",
            Host::Openclaw => "openclaw",
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
        }
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
        }
    }

    /// Remove command.
    pub fn remove_cmd(self) -> &'static str {
        match self {
            Host::Claudecode => "claude plugin uninstall vaibot-governance@vaibot-claudecode",
            Host::Codex => "codex plugin marketplace remove vaibot-codex",
            Host::Openclaw => "openclaw plugins uninstall circuit-breaker-openclaw-plugin",
        }
    }

    /// Post-op verification — is the plugin detectably installed? `None` means the
    /// host exposes no scriptable check (codex has no `marketplace list`, and its
    /// enable is an interactive picker), so the caller can't confirm via CLI.
    pub fn verify_installed(self) -> Option<bool> {
        let (cmd, needle) = match self {
            Host::Claudecode => ("claude plugin list", "vaibot-governance"),
            Host::Openclaw => ("openclaw plugins list", "circuit-breaker"),
            Host::Codex => return None,
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
            _ => None,
        }
    }
}
