//! Host installer primitives — REAL (best-effort shell-outs).
//!
//! Used by `vaibot init` / `guard install` / `plugin add` to install the VAIBot
//! guard (`npm install -g @vaibot/guard`) + write the guard.env + systemd unit.
//! (Host circuit-breaker plugins are handled per-host in `services::host`.) Each
//! returns a bool (success) and never panics — the handlers narrate the result.

use std::path::PathBuf;

use super::{run_capture, which};
use crate::config::atomic::write_atomic_0600;
use crate::config::creds::VaibotEnv;
use crate::config::{guard_env_path, guard_log_dir};
use crate::error::CliError;

/// Run a single best-effort shell step; true on success.
pub fn run_step(cmd: &str) -> bool {
    run_capture(cmd).map(|r| r.ok).unwrap_or(false)
}

/// Is the guard already installed? (the `@vaibot/guard` bins resolve on PATH).
pub fn guard_skill_exists() -> bool {
    which("vaibot-guard").is_some() || which("vaibot-guard-service").is_some()
}

/// Install the guard from npm (`npm install -g @vaibot/guard`, best-effort).
pub fn install_guard_skill() -> bool {
    run_capture("npm install -g @vaibot/guard")
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
/// Pins, in the CLI-managed block, everything the customer's guard needs:
///   - `VAIBOT_API_KEY`       — provenance anchoring + account
///   - `VAIBOT_POLICY_URL`    — `{base}/v2/policy` (the signed-bundle feed; wired now so the guard adopts signed policy the moment VAIBot's public key is provisioned and pinned for the account)
///   - `VAIBOT_GUARD_LOG_DIR` — central per-host audit-log dir, so every guard (daemon or plugin-spawned, via the env-file loader) writes ONE coherent Merkle chain there instead of a `.vaibot-guard/` per project.
///
/// NOTE: the CLI deliberately does NOT pin a `VAIBOT_POLICY_PUBKEY`. The customer
/// never holds a policy signing key — VAIBot signs bundles server-side. Until the
/// well-known VAIBot public key is provisioned + pinned (a server-side step), the
/// guard runs on its built-in floor (`!POLICY_PUBKEY` ⇒ it skips the remote fetch),
/// which is the correct, safe default.
pub fn write_guard_env_file(
    _env: VaibotEnv,
    api_base: &str,
    api_key: &str,
) -> Result<PathBuf, CliError> {
    let path = guard_env_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    // Central audit-log dir, created up front (the guard also mkdirs it; this just
    // guarantees it exists with our ownership before the daemon starts).
    let log_dir = guard_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);

    let policy_url = format!("{}/v2/policy", api_base.trim_end_matches('/'));
    let vars: Vec<(&str, String)> = vec![
        ("VAIBOT_API_KEY", api_key.to_string()),
        ("VAIBOT_POLICY_URL", policy_url),
        ("VAIBOT_GUARD_LOG_DIR", log_dir.to_string_lossy().to_string()),
    ];

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
