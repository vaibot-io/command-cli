//! Service-layer helpers: process supervision (child), binary location
//! (guard_bin/gateway_bin), local guard HTTP reads (guard_http), and the host
//! installer primitives (installer). Plus small shell helpers shared across them.

pub mod child;
pub mod gateway_bin;
pub mod guard_bin;
pub mod guard_http;
pub mod host;
pub mod installer;
pub mod signing_keys;

use std::path::PathBuf;
use std::process::Command;

/// Result of a captured shell command.
#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub ok: bool,
    pub stdout: String,
    #[allow(dead_code)]
    pub stderr: String,
}

/// Locate a binary on PATH (like `which`). Returns its resolved path, or `None`.
pub fn which(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Run a `sh -c` command and capture its trimmed stdout/stderr + success flag.
/// Never panics — a spawn failure yields `ok == false`.
pub fn run_capture(cmd: &str) -> Option<CaptureResult> {
    let output = Command::new("sh").arg("-c").arg(cmd).output().ok()?;
    Some(CaptureResult {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Is systemd (user) available?
pub fn systemd_available() -> bool {
    which("systemctl").is_some()
}

/// Is a given `--user` systemd unit active?
pub fn is_active_systemd_unit(unit: &str) -> bool {
    run_capture(&format!("systemctl --user is-active {unit}"))
        .map(|r| r.stdout == "active")
        .unwrap_or(false)
}
