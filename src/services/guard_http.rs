//! Local guard HTTP reads — REAL.
//!
//! - `run_guard_status`: systemctl --user is-active + GET /health on the
//!   singleton port.
//! - `run_policy_list`: GET /v1/policy from the guard (what it actually loaded,
//!   verified, and enforces) + pretty-print.
//!
//! `resolve_guard_base_url`: explicit override → discovery lock file →
//! default port. Mirrors how the plugins discover the daemon.

use std::time::Duration;

use directories::BaseDirs;
use serde::Deserialize;

use super::guard_bin::GUARD_SINGLETON_PORT;
use super::{is_active_systemd_unit, systemd_available};
use crate::error::CliError;

#[derive(Debug, Clone, Deserialize)]
pub struct GuardBundle {
    pub version: Option<String>,
    pub issuer: Option<String>,
    #[serde(rename = "issuedAt")]
    pub issued_at: Option<String>,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
    pub hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuardPolicy {
    pub source: String,
    pub signature: String,
    pub bundle: Option<GuardBundle>,
    #[serde(default)]
    pub denylist: Vec<String>,
    #[serde(default, rename = "classifierTablesPresent")]
    pub classifier_tables_present: bool,
}

/// Resolve the guard's base URL with the documented precedence.
pub fn resolve_guard_base_url() -> String {
    if let Ok(o) = std::env::var("VAIBOT_GUARD_BASE_URL") {
        if !o.is_empty() {
            return o.trim_end_matches('/').to_string();
        }
    }
    if let Some(base) = BaseDirs::new() {
        let lock = base.home_dir().join(".vaibot").join("guard").join("guard.json");
        if let Ok(raw) = std::fs::read_to_string(&lock) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let (Some(host), Some(port)) = (
                    v.get("host").and_then(|h| h.as_str()),
                    v.get("port").and_then(|p| p.as_u64()),
                ) {
                    return format!("http://{host}:{port}");
                }
            }
        }
    }
    format!("http://127.0.0.1:{GUARD_SINGLETON_PORT}")
}

/// Read the guard-published effective enforce/observe mode from the rendezvous
/// file (`~/.vaibot/guard/guard.json` → `effective_mode`). The guard is the single
/// source of truth and stamps the server-resolved mode here; this is the
/// offline-readable, request-free surface. Returns `None` when the file is absent
/// or predates the field (an older guard) — callers fall back to their own view.
pub fn read_guard_mode() -> Option<String> {
    let base = BaseDirs::new()?;
    let lock = base.home_dir().join(".vaibot").join("guard").join("guard.json");
    let raw = std::fs::read_to_string(&lock).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    match v.get("effective_mode").and_then(|m| m.as_str()) {
        Some("observe") => Some("observe".to_string()),
        Some("enforce") => Some("enforce".to_string()),
        _ => None,
    }
}

/// Read the guard-published resolved env from the rendezvous file
/// (`~/.vaibot/guard/guard.json` → `env`). v3 guards publish this so the CLI's
/// production-coherence gate has a signal even though the de-pinned env file no
/// longer carries a `VAIBOT_POLICY_URL` to infer it from. `None` for a pre-v3 guard
/// that doesn't publish the field (callers fall back to the env-file parse).
pub fn read_guard_env() -> Option<String> {
    let base = BaseDirs::new()?;
    let lock = base.home_dir().join(".vaibot").join("guard").join("guard.json");
    let raw = std::fs::read_to_string(&lock).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    match v.get("env").and_then(|m| m.as_str()) {
        Some("production") => Some("production".to_string()),
        Some("staging") => Some("staging".to_string()),
        _ => None,
    }
}

/// Read the guard rendezvous token (`guard.json` → `token`), needed to auth POSTs.
fn read_guard_token() -> Option<String> {
    let base = BaseDirs::new()?;
    let lock = base.home_dir().join(".vaibot").join("guard").join("guard.json");
    let raw = std::fs::read_to_string(&lock).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("token").and_then(|t| t.as_str()).filter(|s| !s.is_empty()).map(String::from)
}

/// Force the guard to re-poll the control-plane account mode NOW
/// (`POST /v1/mode/refresh`). Returns the `effective_mode` the guard enforces AFTER
/// the re-poll — so the CLI display reflects the live, just-applied enforcement state.
/// `Err("HTTP <code>")` ⇒ the guard answered but lacks the route (older build);
/// anything else ⇒ unreachable.
pub async fn refresh_guard_mode() -> Result<String, CliError> {
    let base = resolve_guard_base_url();
    let client = http_client()?;
    let mut req = client.post(format!("{base}/v1/mode/refresh"));
    if let Some(tok) = read_guard_token() {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|_| CliError::Runtime("guard unreachable".into()))?;
    if !resp.status().is_success() {
        return Err(CliError::Runtime(format!("HTTP {}", resp.status().as_u16())));
    }
    let v: serde_json::Value =
        resp.json().await.map_err(|_| CliError::Runtime("invalid guard response".into()))?;
    v.get("effective_mode")
        .and_then(|m| m.as_str())
        .map(String::from)
        .ok_or_else(|| CliError::Runtime("guard response missing effective_mode".into()))
}

fn http_client() -> Result<reqwest::Client, CliError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| CliError::Runtime(format!("http client: {e}")))
}

/// `vaibot guard status` — systemd check + /health ping.
pub async fn run_guard_status() -> Result<(), CliError> {
    println!("\nVAIBot Guard Status\n");

    if systemd_available() {
        if is_active_systemd_unit("vaibot-guard") {
            println!("  [ok]   systemd service: active");
        } else {
            println!("  [fail] systemd service: not active");
        }
    } else {
        println!("  [info] systemd not available — cannot check service status");
    }

    let base = resolve_guard_base_url();
    println!("  [info] Pinging {base}/health ...");
    let client = http_client()?;
    match client.get(format!("{base}/health")).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("  [ok]   Guard is healthy (HTTP {})", resp.status().as_u16());
            Ok(())
        }
        Ok(resp) => {
            println!("  [fail] Guard is not reachable (HTTP {})", resp.status().as_u16());
            Err(CliError::Runtime("guard not healthy".into()))
        }
        Err(_) => {
            println!("  [fail] Guard is not reachable (timeout)");
            Err(CliError::Runtime("guard unreachable".into()))
        }
    }
}

/// `vaibot guard policy` is a STUB; but `policy show` uses this real read.
pub async fn fetch_guard_policy(base_url: &str) -> Result<GuardPolicy, CliError> {
    let client = http_client()?;
    let resp = client
        .get(format!("{base_url}/v1/policy"))
        .send()
        .await
        .map_err(|_| CliError::Runtime("guard unreachable".into()))?;
    if !resp.status().is_success() {
        return Err(CliError::Runtime(format!("HTTP {}", resp.status().as_u16())));
    }
    resp.json::<GuardPolicy>()
        .await
        .map_err(|_| CliError::Runtime("invalid guard response".into()))
}

/// Render the policy as printable lines (pure).
pub fn format_policy(p: &GuardPolicy) -> Vec<String> {
    let mut lines = Vec::new();
    if p.source == "bundle" {
        if let Some(b) = &p.bundle {
            lines.push("  source:    signed bundle".into());
            lines.push(format!("  version:   {}", b.version.as_deref().unwrap_or("(none)")));
            lines.push(format!("  issuer:    {}", b.issuer.as_deref().unwrap_or("(none)")));
            lines.push(format!("  issued:    {}", b.issued_at.as_deref().unwrap_or("(none)")));
            lines.push(format!("  expires:   {}", b.expires_at.as_deref().unwrap_or("(none)")));
            lines.push(format!(
                "  signature: {}",
                if p.signature == "ok" { "verified" } else { &p.signature }
            ));
            lines.push(format!("  hash:      {}", b.hash));
        }
    } else {
        lines.push("  source:    built-in defaults (no signed bundle in force, fail-closed)".into());
        lines.push(format!("  signature: {}", p.signature));
    }
    lines.push(format!(
        "  denylist:  {}",
        if p.denylist.is_empty() {
            "(empty)".to_string()
        } else {
            p.denylist.join(", ")
        }
    ));
    lines.push(format!(
        "  classifier-table overrides: {}",
        if p.classifier_tables_present {
            "present"
        } else {
            "none (built-in)"
        }
    ));
    lines
}

/// `vaibot policy show` — fetch + format the active guard policy.
pub async fn run_policy_list() -> Result<(), CliError> {
    let base = resolve_guard_base_url();
    println!("\nVAIBot Active Policy\n");
    match fetch_guard_policy(&base).await {
        Ok(p) => {
            for line in format_policy(&p) {
                println!("{line}");
            }
            println!();
            Ok(())
        }
        Err(e) => {
            // `HTTP <code>` ⇒ the guard answered but lacks /v1/policy (older build);
            // anything else ⇒ genuinely unreachable.
            let msg = e.to_string();
            if msg.starts_with("HTTP ") {
                println!("  [fail] Guard is running but lacks /v1/policy ({msg}) — it's an outdated build.");
                println!("  Refresh it with `vaibot guard restart` (or reinstall @vaibot/guard), then retry.");
                Err(CliError::Runtime("guard outdated".into()))
            } else {
                println!("  [fail] Guard not reachable at {base} ({msg}).");
                println!("  Start the guard (run a VAIBot plugin or `vaibot guard status`), or set VAIBOT_GUARD_BASE_URL.");
                Err(CliError::Runtime("guard not reachable".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_policy_builtin() {
        let p = GuardPolicy {
            source: "builtin".into(),
            signature: "no-bundle".into(),
            bundle: None,
            denylist: vec![],
            classifier_tables_present: false,
        };
        let lines = format_policy(&p);
        assert!(lines.iter().any(|l| l.contains("built-in defaults")));
        assert!(lines.iter().any(|l| l.contains("(empty)")));
    }

    #[test]
    fn format_policy_bundle() {
        let p = GuardPolicy {
            source: "bundle".into(),
            signature: "ok".into(),
            bundle: Some(GuardBundle {
                version: Some("v3".into()),
                issuer: Some("vaibot".into()),
                issued_at: Some("t0".into()),
                expires_at: Some("t1".into()),
                hash: "abc".into(),
            }),
            denylist: vec!["rm -rf".into()],
            classifier_tables_present: true,
        };
        let lines = format_policy(&p);
        assert!(lines.iter().any(|l| l.contains("verified")));
        assert!(lines.iter().any(|l| l.contains("rm -rf")));
        assert!(lines.iter().any(|l| l.contains("present")));
    }
}
