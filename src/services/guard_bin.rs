//! Locate the SEPARATE guard binary / service. The CLI orchestrates; it does
//! NOT host the guard daemon.
//!
//! Resolution order (all provided by `npm install -g @vaibot/guard`):
//!   1. $VAIBOT_GUARD_BIN              — explicit override (must exist).
//!   2. `vaibot-guard-service` on PATH  — the daemon bin.
//!   3. `vaibot-guard` on PATH          — fallback/operator bin.
//!
//! Returns `None` when nothing is found — `guard serve` then prints how to
//! install it and exits without ever starting an in-CLI daemon.

use std::path::PathBuf;

use super::which;

pub const GUARD_SINGLETON_PORT: u16 = 39111;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardSource {
    Env,
    Path,
    Service,
}

#[derive(Debug, Clone)]
pub struct GuardBinLocation {
    /// Command to exec.
    pub bin: String,
    /// Extra args (e.g. the .mjs path when bin is `node`).
    pub args: Vec<String>,
    pub source: GuardSource,
}

/// Locate the guard binary using the documented precedence.
pub fn locate_guard_bin() -> Option<GuardBinLocation> {
    if let Ok(bin) = std::env::var("VAIBOT_GUARD_BIN") {
        if PathBuf::from(&bin).exists() {
            return Some(GuardBinLocation {
                bin,
                args: vec![],
                source: GuardSource::Env,
            });
        }
    }
    if which("vaibot-guard-service").is_some() {
        return Some(GuardBinLocation {
            bin: "vaibot-guard-service".into(),
            args: vec![],
            source: GuardSource::Service,
        });
    }
    if which("vaibot-guard").is_some() {
        return Some(GuardBinLocation {
            bin: "vaibot-guard".into(),
            args: vec![],
            source: GuardSource::Path,
        });
    }
    None
}
