//! Version checking and auto-update notification for the VAIBot CLI.
//!
//! Queries crates.io for the latest vaibot release, compares with the current
//! version, and optionally displays an update notification or runs the installer.

use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use std::path::PathBuf;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CRATES_IO_API: &str = "https://crates.io/api/v1/crates/vaibot";
const INSTALL_SCRIPT_URL: &str = "https://vaibot.io/install.sh";

#[derive(Debug, Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    crate_info: CrateInfo,
}

#[derive(Debug, Deserialize)]
struct CrateInfo {
    max_stable_version: String,
}

/// Fetch the latest stable version of vaibot from crates.io.
pub async fn fetch_latest_version() -> Result<String> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let response: CratesIoResponse = client
        .get(CRATES_IO_API)
        .header("User-Agent", "vaibot-cli")
        .send()
        .await?
        .json()
        .await?;

    Ok(response.crate_info.max_stable_version)
}

/// Compare two semantic versions. Returns true if `current` < `latest`.
pub fn is_update_available(current: &str, latest: &str) -> bool {
    parse_version(current) < parse_version(latest)
}

/// Parse a semantic version string into a (major, minor, patch) tuple.
///
/// Tolerant of real-world version strings so a malformed input never silently
/// collapses a component to 0 (which could hide or fabricate an update):
///   - an optional leading `v`/`V` is stripped (`v1.2.3` -> (1,2,3));
///   - prerelease/build suffixes are dropped to the numeric core, so the base
///     release is used (`1.2.3-beta.1` / `1.2.3+build` -> (1,2,3)). Note this
///     treats a prerelease as equal to its release; that is intentional here —
///     we compare against crates.io `max_stable_version`, which is never a
///     prerelease, so we never offer a downgrade to one;
///   - extra components beyond patch are ignored (`1.2.3.4` -> (1,2,3));
///   - missing components default to 0 (`1.2` -> (1,2,0)).
fn parse_version(version: &str) -> (u32, u32, u32) {
    // Strip a leading v/V and any prerelease (`-`) or build (`+`) metadata.
    let core = version
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(['-', '+'])
        .next()
        .unwrap_or("");

    let mut nums = core.split('.').map(|s| s.parse::<u32>().unwrap_or(0));
    let major = nums.next().unwrap_or(0);
    let minor = nums.next().unwrap_or(0);
    let patch = nums.next().unwrap_or(0);
    (major, minor, patch)
}

/// Get the update cache file path (~/.vaibot/update-check.json).
fn cache_file() -> Result<PathBuf> {
    let cache_dir = directories::ProjectDirs::from("io", "vaibot", "vaibot")
        .ok_or_else(|| anyhow::anyhow!("Cannot determine cache directory"))?
        .cache_dir()
        .to_path_buf();

    std::fs::create_dir_all(&cache_dir)?;
    Ok(cache_dir.join("update-check.json"))
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct UpdateCache {
    checked_at: String,
    latest_version: String,
}

/// Check if we should skip the version check (checked within last 24h).
fn should_check_cache() -> bool {
    if let Ok(cache_path) = cache_file() {
        if let Ok(content) = std::fs::read_to_string(&cache_path) {
            if let Ok(cache) = serde_json::from_str::<UpdateCache>(&content) {
                if let Ok(checked) = chrono::DateTime::parse_from_rfc3339(&cache.checked_at) {
                    let now = chrono::Utc::now();
                    let duration = now.signed_duration_since(checked.with_timezone(&chrono::Utc));
                    return duration.num_hours() < 24;
                }
            }
        }
    }
    false
}

/// Save the latest version to cache.
fn save_cache(version: &str) -> Result<()> {
    let cache_path = cache_file()?;
    let cache = UpdateCache {
        checked_at: chrono::Utc::now().to_rfc3339(),
        latest_version: version.to_string(),
    };
    let content = serde_json::to_string(&cache)?;
    std::fs::write(&cache_path, content)?;
    Ok(())
}

/// Load the cached version if available.
pub fn load_cached_version() -> Option<String> {
    if let Ok(cache_path) = cache_file() {
        if let Ok(content) = std::fs::read_to_string(&cache_path) {
            if let Ok(cache) = serde_json::from_str::<UpdateCache>(&content) {
                return Some(cache.latest_version);
            }
        }
    }
    None
}

/// Display an update notification if a new version is available.
/// Returns the latest version if an update is available, None otherwise.
pub async fn check_and_notify_update() -> Option<String> {
    // Skip if checked recently (within 24h)
    if should_check_cache() {
        return None;
    }

    // Fetch the latest version under a single 2s budget so the auto-check never
    // noticeably blocks the CLI. This is the only timeout on this path — callers
    // must not wrap it in another.
    let latest = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fetch_latest_version(),
    )
    .await
    {
        Ok(Ok(v)) => v,
        _ => {
            // If the check fails or times out, try to use cached version
            return load_cached_version()
                .filter(|v| is_update_available(CURRENT_VERSION, v))
                .or(None);
        }
    };

    // Cache the result
    let _ = save_cache(&latest);

    // Check if update is available
    if is_update_available(CURRENT_VERSION, &latest) {
        return Some(latest);
    }

    None
}

/// Display the update notification message.
pub fn show_update_notification(latest_version: &str) {
    eprintln!();
    eprintln!("🎉 Update available! VAIBot {} → {}", CURRENT_VERSION, latest_version);
    eprintln!();
    eprintln!("  Update via:");
    eprintln!("    sh -c 'curl -fsSL {} | VAIBOT_NON_INTERACTIVE=1 sh'", INSTALL_SCRIPT_URL);
    eprintln!();
    eprintln!("  Or run: vaibot update");
    eprintln!();
}

/// Upper bound on the installer size; a legitimate install.sh is a couple KB.
const MAX_SCRIPT_BYTES: usize = 1024 * 1024; // 1 MiB

/// Lowercase hex SHA-256 of the given bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Validate that a downloaded payload looks like a shell installer — not an
/// HTML error page, a redirect stub, or truncated/garbage — before it is ever
/// handed to `sh`.
fn validate_install_script(script: &str) -> Result<()> {
    if script.len() > MAX_SCRIPT_BYTES {
        anyhow::bail!(
            "Installer is unexpectedly large ({} bytes > {} cap); refusing to run",
            script.len(),
            MAX_SCRIPT_BYTES
        );
    }
    let head = script.trim_start();
    if head.is_empty() {
        anyhow::bail!("Installer download was empty; refusing to run");
    }
    // A real installer begins with a shebang. This also rejects HTML error
    // pages that some hosts serve with a 200 status.
    if !head.starts_with("#!") {
        anyhow::bail!("Installer does not begin with a shebang (#!); refusing to run");
    }
    let sniff = head[..head.len().min(256)].to_ascii_lowercase();
    if sniff.contains("<!doctype") || sniff.contains("<html") {
        anyhow::bail!("Installer looks like an HTML page, not a shell script; refusing to run");
    }
    Ok(())
}

/// Prompt on the terminal to confirm executing the installer with `digest`.
fn confirm_execute(digest: &str) -> Result<bool> {
    use std::io::Write;
    eprint!("Run this installer (SHA-256 {digest})? [y/N] ");
    std::io::stderr().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Perform the actual update by downloading and running the install script.
///
/// Hardening applied before the script reaches a shell:
///   - the transport is asserted to be HTTPS;
///   - a non-2xx response aborts instead of piping an error page into `sh`;
///   - the payload is validated (non-empty, shebang, size cap, not HTML);
///   - the SHA-256 is always printed for auditability, and if
///     `VAIBOT_INSTALL_SHA256` is set the download must match it or we abort
///     (lets CI and cautious users pin a known-good installer);
///   - in interactive mode the user must confirm the digest before execution.
///
/// This does NOT defend against a compromised install host serving a
/// well-formed but malicious script — that requires signed releases, tracked
/// separately.
pub async fn perform_update(non_interactive: bool) -> Result<()> {
    // Defense in depth: never fetch the installer over a non-TLS transport.
    if !INSTALL_SCRIPT_URL.starts_with("https://") {
        anyhow::bail!("Refusing to fetch installer over non-HTTPS URL: {INSTALL_SCRIPT_URL}");
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    eprintln!("⬇️  Downloading VAIBot installer...");
    let script = client
        .get(INSTALL_SCRIPT_URL)
        .header("User-Agent", "vaibot-cli")
        .send()
        .await?
        .error_for_status()? // 4xx/5xx must not be executed as a script
        .text()
        .await?;

    validate_install_script(&script)?;

    let digest = sha256_hex(script.as_bytes());
    eprintln!("   SHA-256: {digest}");

    // Optional pinning: abort if the download doesn't match a caller-supplied hash.
    if let Ok(expected) = std::env::var("VAIBOT_INSTALL_SHA256") {
        let expected = expected.trim().to_ascii_lowercase();
        if !expected.is_empty() && expected != digest {
            anyhow::bail!(
                "Installer SHA-256 mismatch: expected {expected}, got {digest}; aborting."
            );
        }
        if !expected.is_empty() {
            eprintln!("   ✓ Matches pinned VAIBOT_INSTALL_SHA256");
        }
    }

    // In interactive mode, require explicit confirmation before executing.
    if !non_interactive && !confirm_execute(&digest)? {
        anyhow::bail!("Update cancelled.");
    }

    // Write to a private temp file and execute.
    let temp_script = tempfile::NamedTempFile::new()?;
    std::fs::write(temp_script.path(), script.as_bytes())?;

    let mut cmd = std::process::Command::new("sh");
    cmd.arg(temp_script.path());

    if non_interactive {
        cmd.env("VAIBOT_NON_INTERACTIVE", "1");
    }

    let status = cmd.status()?;

    if status.success() {
        eprintln!("✅ VAIBot updated successfully!");
        Ok(())
    } else {
        anyhow::bail!("Update failed with exit code: {:?}", status.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_parsing() {
        assert_eq!(parse_version("1.2.3"), (1, 2, 3));
        assert_eq!(parse_version("0.3.0"), (0, 3, 0));
        assert_eq!(parse_version("2.0.1"), (2, 0, 1));
    }

    #[test]
    fn test_version_parsing_hardened() {
        // Leading v/V is stripped.
        assert_eq!(parse_version("v1.2.3"), (1, 2, 3));
        assert_eq!(parse_version("V0.7.0"), (0, 7, 0));
        // Prerelease / build metadata collapses to the base release.
        assert_eq!(parse_version("1.2.3-beta.1"), (1, 2, 3));
        assert_eq!(parse_version("1.0.0-rc1"), (1, 0, 0));
        assert_eq!(parse_version("1.2.3+build.5"), (1, 2, 3));
        // Extra components are ignored; missing ones default to 0.
        assert_eq!(parse_version("1.2.3.4"), (1, 2, 3));
        assert_eq!(parse_version("1.2"), (1, 2, 0));
        assert_eq!(parse_version("  0.6.1  "), (0, 6, 1));
        // Junk never panics and yields all-zero.
        assert_eq!(parse_version(""), (0, 0, 0));
        assert_eq!(parse_version("garbage"), (0, 0, 0));
    }

    #[test]
    fn test_version_comparison() {
        assert!(is_update_available("0.3.0", "0.4.0"));
        assert!(is_update_available("0.3.0", "1.0.0"));
        assert!(!is_update_available("0.3.0", "0.3.0"));
        assert!(!is_update_available("0.3.0", "0.2.0"));
    }

    // Hits the network (vaibot.io) and proves a wrong pinned hash aborts BEFORE
    // the installer is executed. Run explicitly: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn perform_update_aborts_on_pin_mismatch() {
        std::env::set_var("VAIBOT_INSTALL_SHA256", "deadbeef");
        let res = perform_update(true).await;
        std::env::remove_var("VAIBOT_INSTALL_SHA256");
        let err = res.expect_err("must abort on pin mismatch").to_string();
        assert!(err.contains("SHA-256 mismatch"), "unexpected error: {err}");
    }

    #[test]
    fn test_sha256_hex() {
        // Known vector: SHA-256 of the empty input.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_validate_install_script() {
        // A real installer with a shebang passes.
        assert!(validate_install_script("#!/usr/bin/env sh\necho hi\n").is_ok());
        // Leading whitespace before the shebang is tolerated.
        assert!(validate_install_script("\n  #!/bin/sh\n").is_ok());
        // Empty / whitespace-only is rejected.
        assert!(validate_install_script("").is_err());
        assert!(validate_install_script("   \n ").is_err());
        // No shebang is rejected.
        assert!(validate_install_script("echo not an installer").is_err());
        // HTML error pages served with 200 are rejected.
        assert!(validate_install_script("<!DOCTYPE html><html>404</html>").is_err());
        // Oversized payloads are rejected.
        let huge = format!("#!/bin/sh\n{}", "a".repeat(MAX_SCRIPT_BYTES));
        assert!(validate_install_script(&huge).is_err());
    }

    #[test]
    fn test_version_comparison_hardened() {
        // Double-digit components must compare numerically, not lexically.
        assert!(is_update_available("0.9.0", "0.10.0"));
        assert!(!is_update_available("0.10.0", "0.9.0"));
        // A prerelease of the same release is not treated as an upgrade.
        assert!(!is_update_available("1.0.0", "1.0.0-rc1"));
        // v-prefixed latest still compares correctly.
        assert!(is_update_available("0.6.1", "v0.7.0"));
    }
}
