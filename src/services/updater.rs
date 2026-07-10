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

/// Parse a semantic version string into (major, minor, patch) tuple.
fn parse_version(version: &str) -> (u32, u32, u32) {
    let parts: Vec<&str> = version.split('.').collect();
    let major = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
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

    // Fetch the latest version, with a timeout to avoid blocking the CLI
    let latest = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
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

/// Perform the actual update by downloading and running the install script.
pub async fn perform_update(non_interactive: bool) -> Result<()> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    eprintln!("⬇️  Downloading VAIBot installer...");
    let script = client
        .get(INSTALL_SCRIPT_URL)
        .header("User-Agent", "vaibot-cli")
        .send()
        .await?
        .text()
        .await?;

    // Write to temp file and execute
    let temp_script = tempfile::NamedTempFile::new()?;
    std::fs::write(temp_script.path(), &script)?;

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
    fn test_version_comparison() {
        assert!(is_update_available("0.3.0", "0.4.0"));
        assert!(is_update_available("0.3.0", "1.0.0"));
        assert!(!is_update_available("0.3.0", "0.3.0"));
        assert!(!is_update_available("0.3.0", "0.2.0"));
    }
}
