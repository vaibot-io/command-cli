//! Foreground child-process supervision for shelling out to the SEPARATE guard
//! / gateway binaries.
//!
//! INSTALLER-NOT-RUNTIME GUARDRAIL: the CLI never embeds a daemon. `guard serve`
//! and `gateway serve` LOCATE a standalone binary and exec it, forwarding
//! SIGINT/SIGTERM so Ctrl-C cleanly stops the child, and passing through its
//! exit code. This module is the ONLY place the CLI starts another process as a
//! long-running foreground child.

use std::process::Stdio;

use tokio::process::Command;
use tokio::signal::unix::{signal, SignalKind};

use crate::error::CliError;

/// Options for a foreground child run.
#[derive(Debug, Default)]
pub struct SpawnOptions {
    pub args: Vec<String>,
}

/// Run `bin` to completion with stdio inherited, forwarding SIGINT/SIGTERM to
/// the child, and resolving with its exit code (never erroring on a non-zero
/// exit — the caller maps it to the process exit code). A signalled child maps
/// to exit code 1, mirroring the TS wrapper.
pub async fn run_foreground(bin: &str, opts: SpawnOptions) -> Result<i32, CliError> {
    let mut child = Command::new(bin)
        .args(&opts.args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| CliError::Runtime(format!("failed to spawn {bin}: {e}")))?;

    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|e| CliError::Runtime(format!("sigint handler: {e}")))?;
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|e| CliError::Runtime(format!("sigterm handler: {e}")))?;

    let pid = child.id();

    loop {
        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(|e| CliError::Runtime(format!("wait {bin}: {e}")))?;
                return Ok(status.code().unwrap_or(1));
            }
            _ = sigint.recv() => relay_signal(pid, nix::sys::signal::Signal::SIGINT),
            _ = sigterm.recv() => relay_signal(pid, nix::sys::signal::Signal::SIGTERM),
        }
    }
}

/// Relay a signal to the child by PID (best-effort).
fn relay_signal(pid: Option<u32>, sig: nix::sys::signal::Signal) {
    if let Some(pid) = pid {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passes_through_exit_code() {
        // `false` exits 1; `true` exits 0 — portable POSIX utilities.
        let code = run_foreground("true", SpawnOptions::default()).await.unwrap();
        assert_eq!(code, 0);
        let code = run_foreground("false", SpawnOptions::default()).await.unwrap();
        assert_eq!(code, 1);
    }

    #[tokio::test]
    async fn missing_binary_is_runtime_error() {
        let err = run_foreground("vaibot-no-such-binary-xyz", SpawnOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::Runtime(_)));
    }
}
