//! Typed exit codes + the canonical "not yet wired" stub error.
//!
//! Every command-tree leaf the plan marks STUB returns `CliError::Stub`. The
//! root error handler (`main.rs`) prints the canonical line and sets the
//! documented exit code. The `commands-stub` / `lib-errors` tests assert this
//! shape, so the wording + codes are load-bearing — change them in lockstep
//! with those tests. The `Display` string is verbatim the TS contract:
//!
//!   vaibot <noun> is not yet wired — see <noun> (tracked; this is an orchestrator stub)
//!
//! plus an optional `\n  <hint>` (two-space indent, em-dash `—`).

/// Process exit codes the CLI uses. Mirrors the TS `ExitCode` map.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Success.
    Ok = 0,
    /// Generic runtime failure.
    Error = 1,
    /// A command that is scaffolded but not yet implemented (orchestrator stub).
    Stub = 2,
    /// No usable credential / not logged in.
    Auth = 3,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The single canonical stub. `noun` is the command path the user can
    /// re-run once it's wired (e.g. "policy set", "gateway serve").
    #[error("vaibot {noun} is not yet wired — see {noun} (tracked; this is an orchestrator stub){}",
            .hint.as_ref().map(|h| format!("\n  {h}")).unwrap_or_default())]
    Stub {
        noun: String,
        hint: Option<String>,
    },

    /// No usable credential / not logged in.
    #[error("Not logged in. Run `vaibot login` first.")]
    Auth,

    /// Refused to read a secret file because of loose permissions.
    #[error("{0}")]
    Permission(String),

    /// Any other runtime failure carrying a human message.
    #[error("{0}")]
    Runtime(String),
}

impl CliError {
    /// Construct a stub error with no hint.
    pub fn stub(noun: impl Into<String>) -> Self {
        CliError::Stub {
            noun: noun.into(),
            hint: None,
        }
    }

    /// Construct a stub error carrying a one-line hint.
    pub fn stub_hint(noun: impl Into<String>, hint: impl Into<String>) -> Self {
        CliError::Stub {
            noun: noun.into(),
            hint: Some(hint.into()),
        }
    }

    /// Map the error to its documented exit code.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            CliError::Stub { .. } => ExitCode::Stub,
            CliError::Auth => ExitCode::Auth,
            CliError::Permission(_) | CliError::Runtime(_) => ExitCode::Error,
        }
    }
}

/// Convenience: anyhow errors collapse to a generic runtime CliError.
impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        CliError::Runtime(format!("{e:#}"))
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::Runtime(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_message_is_canonical_exit2_line() {
        let e = CliError::stub("mode enforce");
        assert_eq!(e.exit_code(), ExitCode::Stub);
        assert_eq!(ExitCode::Stub as i32, 2);
        let msg = e.to_string();
        assert!(msg.contains("not yet wired — see mode enforce"), "{msg}");
        assert!(msg.contains("(tracked; this is an orchestrator stub)"), "{msg}");
    }

    #[test]
    fn stub_hint_appends_two_space_indented_line() {
        let e = CliError::stub_hint("policy set", "Needs the YAML → canonical-JSON Ed25519 sign path.");
        let msg = e.to_string();
        assert!(msg.contains("\n  Needs the YAML"), "{msg}");
    }

    #[test]
    fn auth_error_is_exit3() {
        assert_eq!(CliError::Auth.exit_code(), ExitCode::Auth);
        assert_eq!(ExitCode::Auth as i32, 3);
        assert_eq!(CliError::Auth.to_string(), "Not logged in. Run `vaibot login` first.");
    }
}
