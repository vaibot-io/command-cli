//! Per-host plugin-manager dispatch for `vaibot plugin add/remove/update`.
//!
//! Each host wires the circuit-breaker plugin through its OWN native CLI:
//!   claudecode → `claude plugin ...`           (marketplace add + install; verifiable)
//!   openclaw   → `openclaw plugins ...`         (install npm spec; verifiable)
//!   codex      → `codex plugin marketplace ...` (register; ENABLE is an interactive picker)
//!   hermes     → `hermes plugins enable` exists, but the FILES have to be placed
//!                first: the plugin is Python, published to PyPI, and distributed on
//!                npm as an installer that fetches the wheel and verifies it against a
//!                pinned digest. That installer is delegated to rather than
//!                reimplemented here — a second implementation of a digest check is
//!                exactly what the plugins refuse to do with the guard's classifier.
//!                Handled by commands::plugin::install_hermes, not the steps below,
//!                because a digest mismatch must fail hard rather than warn.
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
    Hermes,
}

impl Host {
    /// Every host, for detection / iteration.
    pub const ALL: [Host; 5] = [
        Host::Claudecode,
        Host::Codex,
        Host::Openclaw,
        Host::Cursor,
        Host::Hermes,
    ];

    pub fn parse(s: &str) -> Option<Host> {
        match s.to_ascii_lowercase().as_str() {
            "claudecode" | "claude" | "claude-code" => Some(Host::Claudecode),
            "codex" => Some(Host::Codex),
            "openclaw" => Some(Host::Openclaw),
            "cursor" => Some(Host::Cursor),
            "hermes" => Some(Host::Hermes),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Host::Claudecode => "Claude Code",
            Host::Codex => "Codex",
            Host::Openclaw => "OpenClaw",
            Host::Cursor => "Cursor",
            Host::Hermes => "Hermes",
        }
    }

    /// The host key as accepted by `vaibot plugin add <key>`.
    pub fn key(self) -> &'static str {
        match self {
            Host::Claudecode => "claudecode",
            Host::Codex => "codex",
            Host::Openclaw => "openclaw",
            Host::Cursor => "cursor",
            Host::Hermes => "hermes",
        }
    }

    /// The host's native CLI binary name.
    pub fn cli(self) -> &'static str {
        match self {
            Host::Claudecode => "claude",
            Host::Codex => "codex",
            Host::Openclaw => "openclaw",
            Host::Cursor => "cursor",
            Host::Hermes => "hermes",
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
            // Handled by install_hermes(): the files are placed by the published npm
            // installer, whose digest check must be able to fail the whole operation
            // rather than warn, which the best-effort loop above cannot express.
            Host::Hermes => &[],
        }
    }

    /// True when the host has no plugin-install CLI. Cursor: its MCP is file-based
    /// (`~/.cursor/mcp.json`), so `mcp connect` skips it; the circuit-breaker plugin
    /// is installed by cloning the published repo into `~/.cursor/plugins/local/`
    /// (see commands::plugin::install_cursor).
    pub fn is_file_based(self) -> bool {
        matches!(self, Host::Cursor)
    }

    /// Can `vaibot mcp connect` register the VAIBot MCP server through this host?
    ///
    /// Separate from `is_file_based` on purpose, because the two say different things.
    /// Cursor's MCP is a file we could write but there is no CLI for it. Hermes is
    /// excluded for a different reason: **nobody has established how Hermes registers
    /// an MCP server**, and the circuit-breaker plugin does not do it. Guessing at a
    /// command and running it against someone's agent config is worse than not
    /// offering the feature, so it is off until that is known.
    pub fn supports_mcp_connect(self) -> bool {
        !matches!(self, Host::Cursor | Host::Hermes)
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
            // Re-runs the installer, which re-fetches and re-verifies the wheel.
            // `--force` because the plugin directory already exists.
            Host::Hermes => &[(
                "Updating plugin",
                "npx --yes @vaibot/hermes-circuitbreaker-plugin install --force",
            )],
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
            // Disabling is all the CLI can do; the directory is removed separately by
            // remove_hermes(), because `plugins disable` leaves the files in place.
            Host::Hermes => "hermes plugins disable vaibot",
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
            Host::Hermes => ("hermes plugins list", "vaibot"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_parses_and_is_in_all() {
        assert!(matches!(Host::parse("hermes"), Some(Host::Hermes)));
        assert!(matches!(Host::parse("Hermes"), Some(Host::Hermes)));
        assert_eq!(Host::ALL.len(), 5);
        assert!(Host::ALL.iter().any(|h| matches!(h, Host::Hermes)));
    }

    #[test]
    fn every_host_has_a_key_that_parses_back() {
        // A key that does not round-trip is a host you cannot actually name on the
        // command line, which is the sort of thing that ships unnoticed.
        for h in Host::ALL {
            let parsed = Host::parse(h.key());
            assert!(parsed.is_some(), "{} key {:?} does not parse", h.label(), h.key());
            assert!(parsed.unwrap() == h, "{} key round-trips to a different host", h.label());
        }
    }

    #[test]
    fn every_host_has_a_label_and_a_cli() {
        for h in Host::ALL {
            assert!(!h.label().is_empty());
            assert!(!h.cli().is_empty());
        }
    }

    #[test]
    fn hermes_is_excluded_from_mcp_connect() {
        // Nobody has established how Hermes registers an MCP server, so it must not be
        // swept into `mcp connect` merely because the hermes CLI is on PATH. Cursor is
        // excluded for the different reason that its MCP is a file.
        assert!(!Host::Hermes.supports_mcp_connect());
        assert!(!Host::Cursor.supports_mcp_connect());
        for h in [Host::Claudecode, Host::Codex, Host::Openclaw] {
            assert!(h.supports_mcp_connect(), "{} should support mcp connect", h.label());
        }
    }

    #[test]
    fn hermes_is_not_file_based() {
        // It has a real plugin CLI (`hermes plugins enable`), unlike Cursor. The two
        // predicates say different things and conflating them is what let a host with
        // no MCP surface through the single-host path.
        assert!(!Host::Hermes.is_file_based());
        assert!(Host::Cursor.is_file_based());
    }

    #[test]
    fn hermes_has_no_generic_install_steps() {
        // Install is handled by install_hermes(), because a digest mismatch has to be
        // able to fail the operation and the generic loop only warns.
        assert!(Host::Hermes.install_steps().is_empty());
    }

    #[test]
    fn hermes_update_re_fetches_and_verifies() {
        let steps = Host::Hermes.update_steps();
        assert_eq!(steps.len(), 1);
        assert!(steps[0].1.contains("@vaibot/hermes-circuitbreaker-plugin"));
        // --force, because the plugin directory already exists on an update.
        assert!(steps[0].1.contains("--force"));
        // --yes, so npx does not stop to ask in a non-interactive shell.
        assert!(steps[0].1.contains("--yes"));
    }

    #[test]
    fn hermes_can_be_verified_after_install() {
        // Unlike codex and cursor, Hermes exposes a scriptable check, so add/remove can
        // actually be confirmed rather than only warned about. `Some(false)` on a machine
        // without the hermes CLI is the correct answer and still proves the host is
        // verifiable in principle — what must not happen is `None`, which is how
        // verify_after() decides it cannot check at all.
        assert!(
            Host::Hermes.verify_installed().is_some(),
            "Hermes should be verifiable; None means verify_after only warns"
        );
        // And the hosts that genuinely cannot be checked still say so.
        assert!(Host::Codex.verify_installed().is_none());
        assert!(Host::Cursor.verify_installed().is_none());
    }

    #[test]
    fn hermes_remove_disables_rather_than_pretending_to_delete() {
        // `plugins disable` leaves the files, so remove_hermes() also deletes the
        // directory. This pins that the command really is the disable, so nobody later
        // reads it as a full uninstall.
        assert!(Host::Hermes.remove_cmd().contains("disable"));
    }
}
