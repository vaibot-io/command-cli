//! Locate the SEPARATE vaibot-gateway Rust binary. Same installer-not-runtime
//! rule as the guard: the CLI shells out, it never embeds the proxy.
//!
//! Resolution order:
//!   1. $VAIBOT_GATEWAY_BIN  — explicit override (must exist).
//!   2. `vaibot-gateway` on PATH.
//!
//! Returns `None` → `gateway serve` prints the local-first proxy model (set
//! ANTHROPIC_BASE_URL to route through it) and how to install it, then exits
//! without starting anything in-process.

use std::path::PathBuf;

use directories::BaseDirs;

use super::which;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewaySource {
    Env,
    Path,
}

#[derive(Debug, Clone)]
pub struct GatewayBinLocation {
    pub bin: String,
    pub args: Vec<String>,
    pub source: GatewaySource,
}

/// The default base URL agents point at to route through the gateway.
pub const GATEWAY_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8787";

/// Locate the gateway binary using the documented precedence.
pub fn locate_gateway_bin() -> Option<GatewayBinLocation> {
    if let Ok(bin) = std::env::var("VAIBOT_GATEWAY_BIN") {
        if PathBuf::from(&bin).exists() {
            return Some(GatewayBinLocation {
                bin,
                args: vec![],
                source: GatewaySource::Env,
            });
        }
    }
    if which("vaibot-gateway").is_some() {
        return Some(GatewayBinLocation {
            bin: "vaibot-gateway".into(),
            args: vec![],
            source: GatewaySource::Path,
        });
    }
    None
}

/// Resolve the gateway base URL: `$VAIBOT_GATEWAY_BASE_URL` → default (:8787).
pub fn resolve_gateway_base_url() -> String {
    if let Ok(o) = std::env::var("VAIBOT_GATEWAY_BASE_URL") {
        if !o.is_empty() {
            return o.trim_end_matches('/').to_string();
        }
    }
    GATEWAY_DEFAULT_BASE_URL.to_string()
}

/// Resolve the gateway config path, mirroring the daemon's precedence:
/// `./vaibot-gateway.toml`, then `~/.vaibot/gateway/vaibot-gateway.toml`. Returns
/// the first that exists, else the home default (which may not exist yet).
pub fn gateway_config_path() -> PathBuf {
    let cwd = PathBuf::from("vaibot-gateway.toml");
    if cwd.exists() {
        return cwd;
    }
    if let Some(base) = BaseDirs::new() {
        return base
            .home_dir()
            .join(".vaibot")
            .join("gateway")
            .join("vaibot-gateway.toml");
    }
    cwd
}
