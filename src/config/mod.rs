//! Config-dir resolution + derived on-disk paths, all rooted at the VAIBot
//! config directory.
//!
//! Precedence (mirrors the TS `config-dir.ts` chain, layered on `directories`):
//!
//!   $VAIBOT_CONFIG_DIR → $VAIBOT_CREDS_DIR → $XDG_CONFIG_HOME/vaibot → ~/.vaibot
//!
//! Credential split (intentional, see login plan §5):
//!   credentials.json — the env-namespaced api_key store (creds.rs owns it).
//!   oauth.json       — interactive USER session tokens (token_store.rs owns it),
//!                      a SEPARATE sidecar so the slim creds writer never drops
//!                      an oauth block.
//!   policy.yaml      — local policy working copy (policy set/pull, deferred).
//!   logs/            — CLI-side log dir.
//!
//! The guard daemon keeps its OWN creds under ~/.config/vaibot-guard/ so it
//! survives `vaibot logout`.

pub mod atomic;
pub mod creds;
pub mod token_store;

use std::path::PathBuf;

use directories::{BaseDirs, ProjectDirs};

/// Environment accessor abstraction so tests can inject a map without touching
/// the process environment. The real impl reads `std::env`.
pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

/// Reads from the actual process environment.
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// In-memory env source for tests.
#[derive(Default)]
pub struct MapEnv(pub std::collections::HashMap<String, String>);

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

/// Resolve the VAIBot config directory using the documented precedence.
pub fn config_dir(env: &dyn EnvSource) -> PathBuf {
    if let Some(d) = env.get("VAIBOT_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = env.get("VAIBOT_CREDS_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = env.get("XDG_CONFIG_HOME") {
        return PathBuf::from(d).join("vaibot");
    }
    // ProjectDirs gives the correct XDG-aware base; we deliberately pin the dir
    // name to `.vaibot` under the home dir to stay aligned with the credential
    // store (creds.rs reads $VAIBOT_CREDS_DIR | ~/.vaibot). When XDG_CONFIG_HOME
    // is unset, ProjectDirs::config_dir would land on ~/.config/vaibot, which
    // would DIVERGE from creds.rs's ~/.vaibot — so we use the home dir directly.
    if let Some(base) = BaseDirs::new() {
        return base.home_dir().join(".vaibot");
    }
    // Last resort if even the home dir can't be resolved.
    let _ = ProjectDirs::from("io", "vaibot", "vaibot");
    PathBuf::from(".vaibot")
}

/// `~/.vaibot/credentials.json` — the env-namespaced api_key store.
pub fn credentials_path(env: &dyn EnvSource) -> PathBuf {
    config_dir(env).join("credentials.json")
}

/// `~/.vaibot/oauth.json` — the interactive user OAuth session sidecar.
pub fn oauth_path(env: &dyn EnvSource) -> PathBuf {
    config_dir(env).join("oauth.json")
}

/// `~/.vaibot/policy.yaml` — local policy working copy (deferred commands).
pub fn policy_path(env: &dyn EnvSource) -> PathBuf {
    config_dir(env).join("policy.yaml")
}

/// `~/.vaibot/logs` — CLI-side log dir.
pub fn logs_dir(env: &dyn EnvSource) -> PathBuf {
    config_dir(env).join("logs")
}

/// Guard-owned env file. NOT under our config dir on purpose — the guard
/// daemon must survive `vaibot logout` clearing oauth.json.
pub fn guard_env_path() -> PathBuf {
    BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("vaibot-guard")
        .join("vaibot-guard.env")
}

/// Central per-host guard audit-log dir (`~/.vaibot/guard/log`). Cohesive with the
/// rendezvous lock at `~/.vaibot/guard/guard.json`, so ALL guard state lives under
/// `~/.vaibot/guard/`. Pinned via `VAIBOT_GUARD_LOG_DIR` so every guard — the systemd
/// daemon AND a plugin-spawned fallback — writes ONE coherent, anchorable Merkle
/// chain here, instead of dropping a `.vaibot-guard/` into whatever project the agent
/// is working in (which would both pollute repos and fragment the audit chain).
pub fn guard_log_dir() -> PathBuf {
    BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".vaibot")
        .join("guard")
        .join("log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> MapEnv {
        MapEnv(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn config_dir_honors_precedence() {
        assert_eq!(
            config_dir(&map(&[("VAIBOT_CONFIG_DIR", "/a"), ("VAIBOT_CREDS_DIR", "/b")])),
            PathBuf::from("/a")
        );
        assert_eq!(
            config_dir(&map(&[("VAIBOT_CREDS_DIR", "/b")])),
            PathBuf::from("/b")
        );
        assert_eq!(
            config_dir(&map(&[("XDG_CONFIG_HOME", "/x")])),
            PathBuf::from("/x/vaibot")
        );
    }

    #[test]
    fn derived_paths_root_at_config_dir() {
        let env = map(&[("VAIBOT_CONFIG_DIR", "/cfg")]);
        assert_eq!(credentials_path(&env), PathBuf::from("/cfg/credentials.json"));
        assert_eq!(oauth_path(&env), PathBuf::from("/cfg/oauth.json"));
        assert_eq!(policy_path(&env), PathBuf::from("/cfg/policy.yaml"));
        assert_eq!(logs_dir(&env), PathBuf::from("/cfg/logs"));
    }

    #[test]
    fn empty_env_falls_back_to_home_dotvaibot() {
        let env = MapEnv(HashMap::new());
        let d = config_dir(&env);
        assert!(d.ends_with(".vaibot"), "{d:?}");
    }
}
