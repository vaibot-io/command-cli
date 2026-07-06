//! Host installer primitives — REAL (best-effort shell-outs).
//!
//! Used by `vaibot init` / `guard install` / `plugin add` to install the VAIBot
//! guard (`npm install -g @vaibot/guard`) + write the guard.env + systemd unit.
//! (Host circuit-breaker plugins are handled per-host in `services::host`.) Each
//! returns a bool (success) and never panics — the handlers narrate the result.

use std::path::{Path, PathBuf};
use std::process::Command;

use directories::BaseDirs;

use super::{run_capture, which};
use crate::config::atomic::write_atomic_0600;
use crate::config::{guard_env_path, guard_log_dir};
use crate::error::CliError;

/// The npm spec the guard is installed from — the single place to bump on each
/// guard release. `@^2.0.0` takes the latest 2.x, so guard security patches flow
/// to a fresh install / `guard update` without a CLI release — but never a
/// breaking 3.0.0, which would need a deliberate bump of the floor here.
pub const GUARD_NPM_SPEC: &str = "@vaibot/guard@^2.0.0";

/// Run a single best-effort shell step; true on success.
pub fn run_step(cmd: &str) -> bool {
    run_capture(cmd).map(|r| r.ok).unwrap_or(false)
}

/// Is the guard already installed? (the `@vaibot/guard` bins resolve on PATH).
pub fn guard_skill_exists() -> bool {
    which("vaibot-guard").is_some() || which("vaibot-guard-service").is_some()
}

/// Install the guard from npm (best-effort). Pinned via [`GUARD_NPM_SPEC`].
pub fn install_guard_skill() -> bool {
    run_capture(&format!("npm install -g {GUARD_NPM_SPEC}"))
        .map(|r| r.ok)
        .unwrap_or(false)
}

// A CLI-owned block inside the guard EnvironmentFile. We rewrite ONLY between
// these markers so `init` / `doctor --fix` can re-pin idempotently without
// clobbering a user's hand-added lines (token, bind host, custom policy path).
const MANAGED_BEGIN: &str = "# >>> vaibot-managed (vaibot init / doctor --fix rewrites this block; edits here are lost) >>>";
const MANAGED_END: &str = "# <<< vaibot-managed <<<";

/// Format an env value for a systemd EnvironmentFile. Simple values stay bare;
/// anything with whitespace/newlines is double-quoted — systemd preserves embedded
/// newlines inside double quotes (verified on systemd 255), which is how the
/// multi-line `VAIBOT_POLICY_PUBKEY` PEM is transported. PEM bodies contain no
/// double-quotes or backslashes, so no escaping is required.
fn fmt_env_value(v: &str) -> String {
    if v.is_empty() || v.contains(|c: char| c.is_whitespace()) {
        format!("\"{v}\"")
    } else {
        v.to_string()
    }
}

/// Pure: drop any existing vaibot-managed block from `existing` (preserving every
/// other line), then append a fresh managed block from `vars`. Tolerant of a
/// truncated file whose END marker is missing (drops to EOF).
pub fn render_guard_env(existing: &str, vars: &[(&str, String)]) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in existing.lines() {
        if line.trim() == MANAGED_BEGIN {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == MANAGED_END {
                in_block = false;
            }
            continue;
        }
        kept.push(line);
    }
    // Trim trailing blank lines from the preserved preamble for tidy output.
    while matches!(kept.last(), Some(l) if l.trim().is_empty()) {
        kept.pop();
    }

    let mut out = String::new();
    if !kept.is_empty() {
        out.push_str(&kept.join("\n"));
        out.push('\n');
    }
    out.push_str(MANAGED_BEGIN);
    out.push('\n');
    for (k, v) in vars {
        out.push_str(k);
        out.push('=');
        out.push_str(&fmt_env_value(v));
        out.push('\n');
    }
    out.push_str(MANAGED_END);
    out.push('\n');
    out
}

/// Write/refresh the guard EnvironmentFile (0600) under ~/.config/vaibot-guard/.
///
/// v3 (credentials split): the guard now derives `VAIBOT_API_KEY` + the V2
/// governance / V1 provenance bases + the policy feed (`{governance}/v2/policy`)
/// straight from the resolved env's slot in `~/.vaibot/credentials.json`, so the
/// CLI no longer pins `VAIBOT_API_KEY` or `VAIBOT_POLICY_URL` here — pinning them
/// is exactly what let a stale staging URL pair with a prod key. The only value
/// left in the CLI-managed block is:
///   - `VAIBOT_GUARD_LOG_DIR` — central per-host audit-log dir, so every guard
///     (daemon or plugin-spawned, via the env-file loader) writes ONE coherent
///     Merkle chain there instead of a `.vaibot-guard/` per project. This is an
///     install-location concern, not creds-derived, so it stays pinned.
///
/// Re-running init also *removes* any previously-pinned `VAIBOT_API_KEY` /
/// `VAIBOT_POLICY_URL` (render_guard_env rewrites the whole managed block).
///
/// NOTE: the CLI still deliberately does NOT pin a `VAIBOT_POLICY_PUBKEY` — the
/// customer never holds a policy signing key; VAIBot signs bundles server-side and
/// the guard pins the well-known per-env public key itself (see pinned-keys.mjs).
pub fn write_guard_env_file() -> Result<PathBuf, CliError> {
    let path = guard_env_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    // Central audit-log dir, created up front (the guard also mkdirs it; this just
    // guarantees it exists with our ownership before the daemon starts).
    let log_dir = guard_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);

    let vars: Vec<(&str, String)> =
        vec![("VAIBOT_GUARD_LOG_DIR", log_dir.to_string_lossy().to_string())];

    let contents = render_guard_env(&existing, &vars);
    write_atomic_0600(&path, &contents)?;
    Ok(path)
}

/// Enable + start the systemd user service (best-effort).
pub fn install_systemd_service() -> bool {
    run_capture("systemctl --user enable --now vaibot-guard")
        .map(|r| r.ok)
        .unwrap_or(false)
}

/// Platform-aware service install: shell out to the guard's own `vaibot-guard install`,
/// which walks the root-preferred ladder (systemd / launchd / self-spawn), writes the
/// unit, starts it, and health-verifies. Returns true only when that succeeds (exit 0),
/// so a false means "not confirmed healthy — will self-spawn." Keeps the CLI a thin
/// orchestrator: one source of truth for the ladder lives in node, not duplicated here.
pub fn install_guard_service_platform() -> bool {
    run_step("vaibot-guard install")
}

/// Verify the plugin appears loaded (best-effort).
pub fn verify_plugin() -> bool {
    run_capture("openclaw plugins list")
        .map(|r| r.ok && r.stdout.contains("circuit-breaker"))
        .unwrap_or(false)
}

/// Uninstall the guard npm package globally (best-effort).
pub fn uninstall_guard() -> bool {
    run_capture("npm uninstall -g @vaibot/guard")
        .map(|r| r.ok)
        .unwrap_or(false)
}

/// Disable + stop the systemd user service (best-effort).
pub fn disable_systemd_service() -> bool {
    run_capture("systemctl --user disable --now vaibot-guard")
        .map(|r| r.ok)
        .unwrap_or(false)
}

/// Restart the systemd user service to pick up an updated guard (best-effort).
pub fn restart_systemd_service() -> bool {
    run_capture("systemctl --user restart vaibot-guard")
        .map(|r| r.ok)
        .unwrap_or(false)
}

// ── Cursor plugin (local install) ─────────────────────────────────────────────
// Cursor has no plugin-install CLI, so `plugin add cursor` clones the PUBLISHED
// repo into `~/.cursor/plugins/local/<name>/` — Cursor loads local plugins from
// there on restart. The repo root IS the plugin (single-plugin layout), so a
// direct clone lands `.cursor-plugin/plugin.json` + `hooks/` at the plugin root.

/// Published Cursor plugin repo (public → HTTPS clone needs no auth).
const CURSOR_PLUGIN_REPO: &str = "https://github.com/vaibot-io/cursor-circuitbreaker-plugin.git";

/// `~/.cursor/plugins/local/vaibot-cursor` — where Cursor loads the local plugin.
pub fn cursor_local_dir() -> PathBuf {
    BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cursor")
        .join("plugins")
        .join("local")
        .join("vaibot-cursor")
}

/// Install the Cursor plugin: clone fresh, or fast-forward if already cloned.
/// Returns whether it ended up present + current.
pub fn install_cursor_plugin() -> bool {
    let dir = cursor_local_dir();
    if dir.join(".git").is_dir() {
        return git_ff(&dir); // already a clone → update in place (idempotent add)
    }
    if dir.exists() {
        // a non-git dir is in the way → clear it so the clone can land
        if std::fs::remove_dir_all(&dir).is_err() {
            return false;
        }
    }
    if let Some(parent) = dir.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    // Inherit stdio so git's clone progress is visible.
    Command::new("git")
        .args(["clone", CURSOR_PLUGIN_REPO])
        .arg(&dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Fast-forward the local clone; reinstall if the dir isn't a git checkout.
pub fn update_cursor_plugin() -> bool {
    let dir = cursor_local_dir();
    if dir.join(".git").is_dir() {
        git_ff(&dir)
    } else {
        install_cursor_plugin()
    }
}

/// Remove the local plugin dir. Idempotent — a missing dir counts as success.
pub fn remove_cursor_plugin() -> bool {
    let dir = cursor_local_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir).is_ok()
    } else {
        true
    }
}

fn git_ff(dir: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["pull", "--ff-only"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEM: &str = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAabc=\n-----END PUBLIC KEY-----";

    fn managed() -> Vec<(&'static str, String)> {
        vec![
            ("VAIBOT_API_KEY", "vbk_live_123".to_string()),
            ("VAIBOT_POLICY_URL", "https://api.example/v2/policy".to_string()),
            ("VAIBOT_POLICY_PUBKEY", PEM.to_string()),
        ]
    }

    #[test]
    fn simple_values_are_bare_multiline_is_quoted() {
        assert_eq!(fmt_env_value("vbk_live_123"), "vbk_live_123");
        assert_eq!(fmt_env_value("https://api.example/v2/policy"), "https://api.example/v2/policy");
        // a multi-line PEM must be double-quoted so systemd keeps the newlines.
        assert_eq!(fmt_env_value(PEM), format!("\"{PEM}\""));
        assert_eq!(fmt_env_value(""), "\"\"");
    }

    #[test]
    fn empty_file_gets_a_single_managed_block() {
        let out = render_guard_env("", &managed());
        assert_eq!(out.matches(MANAGED_BEGIN).count(), 1);
        assert_eq!(out.matches(MANAGED_END).count(), 1);
        assert!(out.contains("VAIBOT_API_KEY=vbk_live_123\n"));
        assert!(out.contains("VAIBOT_POLICY_URL=https://api.example/v2/policy\n"));
        // pubkey is quoted and keeps its internal newlines.
        assert!(out.contains("VAIBOT_POLICY_PUBKEY=\"-----BEGIN PUBLIC KEY-----\n"));
        assert!(out.contains("-----END PUBLIC KEY-----\"\n"));
    }

    #[test]
    fn re_pin_replaces_the_block_without_duplicating() {
        let first = render_guard_env("", &managed());
        let second = render_guard_env(
            &first,
            &[
                ("VAIBOT_API_KEY", "vbk_live_123".to_string()),
                ("VAIBOT_POLICY_URL", "https://api.example/v2/policy".to_string()),
                ("VAIBOT_POLICY_PUBKEY", "-----BEGIN PUBLIC KEY-----\nROTATED=\n-----END PUBLIC KEY-----".to_string()),
            ],
        );
        // exactly one managed block survives a re-pin; the new key replaced the old.
        assert_eq!(second.matches(MANAGED_BEGIN).count(), 1);
        assert!(second.contains("ROTATED="));
        assert!(!second.contains("MCowBQYDK2VwAyEAabc="));
    }

    #[test]
    fn v3_re_pin_drops_api_key_and_policy_url_from_the_managed_block() {
        // A guard env file pinned by a pre-v3 CLI (API_KEY + POLICY_URL + LOG_DIR).
        let legacy = render_guard_env(
            "",
            &[
                ("VAIBOT_API_KEY", "vbk_live_123".to_string()),
                ("VAIBOT_POLICY_URL", "https://api.example/v2/policy".to_string()),
                ("VAIBOT_GUARD_LOG_DIR", "/home/u/.local/share/vaibot/guard".to_string()),
            ],
        );
        // v3 write_guard_env_file pins ONLY the log dir; the guard derives the rest.
        let out = render_guard_env(
            &legacy,
            &[("VAIBOT_GUARD_LOG_DIR", "/home/u/.local/share/vaibot/guard".to_string())],
        );
        assert!(out.contains("VAIBOT_GUARD_LOG_DIR=/home/u/.local/share/vaibot/guard\n"));
        // the stale creds-derived pins are gone — no more staging-URL-with-prod-key trap.
        assert!(!out.contains("VAIBOT_API_KEY="));
        assert!(!out.contains("VAIBOT_POLICY_URL="));
        assert_eq!(out.matches(MANAGED_BEGIN).count(), 1);
    }

    #[test]
    fn preserves_user_lines_outside_the_block() {
        let existing = "# my notes\nVAIBOT_GUARD_TOKEN=secret\nVAIBOT_POLICY_PATH=references/policy.default.json\n";
        let out = render_guard_env(existing, &managed());
        assert!(out.contains("VAIBOT_GUARD_TOKEN=secret"));
        assert!(out.contains("VAIBOT_POLICY_PATH=references/policy.default.json"));
        assert!(out.contains("# my notes"));
        // and the managed block is appended once after the preserved preamble.
        assert_eq!(out.matches(MANAGED_BEGIN).count(), 1);
        let preamble = out.split(MANAGED_BEGIN).next().unwrap();
        assert!(preamble.contains("VAIBOT_GUARD_TOKEN=secret"));
    }

    #[test]
    fn tolerates_a_truncated_block_missing_its_end_marker() {
        // a half-written file (END marker lost) must not leak stale managed lines.
        let truncated = format!("# notes\n{MANAGED_BEGIN}\nVAIBOT_API_KEY=old\nVAIBOT_POLICY_PUBKEY=\"-----BEGIN");
        let out = render_guard_env(&truncated, &managed());
        assert!(!out.contains("VAIBOT_API_KEY=old"));
        assert_eq!(out.matches(MANAGED_BEGIN).count(), 1);
        assert!(out.contains("# notes"));
        assert!(out.contains("VAIBOT_API_KEY=vbk_live_123"));
    }
}
