//! Credential store + environment resolver — a faithful port of the vendored
//! `creds.mjs`.
//!
//! Single source of truth for: which environment a surface operates in, where
//! credentials live on disk, and how to read/write them without one env
//! clobbering the other.
//!
//! Store schema (v2), ~/.vaibot/credentials.json:
//!   {
//!     "version": 2,
//!     "active_env": "production",
//!     "environments": {
//!       "production": { "api_key": "vb_live_…", "wallet_address": "0x…" },
//!       "staging":    { "api_key": "vb_stg_…", "wallet_address": "0x…" }
//!     }
//!   }
//!
//! Only api_key + wallet_address are persisted — everything else is derivable
//! (api_url from env) or fetchable (/v2/accounts/me).

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{atomic, EnvSource};
use crate::error::CliError;

pub const STORE_VERSION: u32 = 2;

/// The closed environment set: production | staging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaibotEnv {
    Production,
    Staging,
}

pub const DEFAULT_ENV: VaibotEnv = VaibotEnv::Production;

impl fmt::Display for VaibotEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            VaibotEnv::Production => "production",
            VaibotEnv::Staging => "staging",
        })
    }
}

impl VaibotEnv {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "production" => Some(VaibotEnv::Production),
            "staging" => Some(VaibotEnv::Staging),
            _ => None,
        }
    }

    fn api_base(self) -> &'static str {
        match self {
            VaibotEnv::Production => "https://api.vaibot.io",
            VaibotEnv::Staging => "https://staging-api.vaibot.io",
        }
    }

    fn key_prefix(self) -> &'static str {
        match self {
            VaibotEnv::Production => "vb_live_",
            VaibotEnv::Staging => "vb_stg_",
        }
    }
}

/// Canonical API base for an env, with an optional override (trailing slashes
/// stripped).
pub fn api_base_for_env(env: VaibotEnv, override_url: Option<&str>) -> String {
    if let Some(o) = override_url {
        return o.trim_end_matches('/').to_string();
    }
    env.api_base().to_string()
}

/// Which env does an api_key's prefix indicate? `None` if unrecognized.
pub fn env_for_key(api_key: &str) -> Option<VaibotEnv> {
    if api_key.starts_with(VaibotEnv::Production.key_prefix()) {
        Some(VaibotEnv::Production)
    } else if api_key.starts_with(VaibotEnv::Staging.key_prefix()) {
        Some(VaibotEnv::Staging)
    } else {
        None
    }
}

/// Lenient prefix guard: a key with a recognized prefix must match `env`; an
/// unrecognized prefix is allowed (avoids false denials for custom/test keys).
pub fn key_prefix_matches_env(api_key: &str, env: VaibotEnv) -> bool {
    match env_for_key(api_key) {
        None => true,
        Some(e) => e == env,
    }
}

/// Map an API base URL to an env. Unknown hosts → `None`.
pub fn env_for_api_url(url: &str) -> Option<VaibotEnv> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    if host == "api.vaibot.io" {
        Some(VaibotEnv::Production)
    } else if host == "staging-api.vaibot.io" || host.contains("staging") {
        Some(VaibotEnv::Staging)
    } else {
        None
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredRecord {
    pub api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wallet_address: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Store {
    pub active_env: VaibotEnv,
    pub production: Option<CredRecord>,
    pub staging: Option<CredRecord>,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            active_env: DEFAULT_ENV,
            production: None,
            staging: None,
        }
    }
}

impl Store {
    fn get(&self, env: VaibotEnv) -> Option<&CredRecord> {
        match env {
            VaibotEnv::Production => self.production.as_ref(),
            VaibotEnv::Staging => self.staging.as_ref(),
        }
    }

    fn set(&mut self, env: VaibotEnv, rec: CredRecord) {
        match env {
            VaibotEnv::Production => self.production = Some(rec),
            VaibotEnv::Staging => self.staging = Some(rec),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let mut environments = serde_json::Map::new();
        if let Some(r) = &self.production {
            environments.insert("production".into(), slim_value(r));
        }
        if let Some(r) = &self.staging {
            environments.insert("staging".into(), slim_value(r));
        }
        serde_json::json!({
            "version": STORE_VERSION,
            "active_env": self.active_env.to_string(),
            "environments": serde_json::Value::Object(environments),
        })
    }
}

fn slim_value(rec: &CredRecord) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("api_key".into(), serde_json::Value::String(rec.api_key.clone()));
    if let Some(w) = &rec.wallet_address {
        if !w.is_empty() {
            m.insert("wallet_address".into(), serde_json::Value::String(w.clone()));
        }
    }
    serde_json::Value::Object(m)
}

fn record_from_value(v: &serde_json::Value) -> Option<CredRecord> {
    let api_key = v.get("api_key")?.as_str()?;
    if api_key.is_empty() {
        return None;
    }
    let wallet_address = v
        .get("wallet_address")
        .and_then(|w| w.as_str())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string());
    Some(CredRecord {
        api_key: api_key.to_string(),
        wallet_address,
    })
}

/// Pure: normalize any parsed JSON into a v2 store. Never panics.
pub fn migrate_store(raw: &serde_json::Value) -> Store {
    if !raw.is_object() {
        return Store::default();
    }
    let version = raw.get("version").and_then(|v| v.as_u64()).unwrap_or(0);

    // Already v2 (or newer) with environments — normalize defensively.
    if version >= STORE_VERSION as u64 {
        if let Some(envs) = raw.get("environments").and_then(|e| e.as_object()) {
            let active_env = raw
                .get("active_env")
                .and_then(|a| a.as_str())
                .and_then(VaibotEnv::parse)
                .unwrap_or(DEFAULT_ENV);
            let mut out = Store {
                active_env,
                ..Store::default()
            };
            if let Some(r) = envs.get("production").and_then(record_from_value) {
                out.set(VaibotEnv::Production, r);
            }
            if let Some(r) = envs.get("staging").and_then(record_from_value) {
                out.set(VaibotEnv::Staging, r);
            }
            return out;
        }
    }

    // v1 flat: { api_key, api_url?, wallet_address? }
    if let Some(api_key) = raw.get("api_key").and_then(|k| k.as_str()) {
        if !api_key.is_empty() {
            let e = raw
                .get("api_url")
                .and_then(|u| u.as_str())
                .and_then(env_for_api_url)
                .or_else(|| env_for_key(api_key))
                .unwrap_or(DEFAULT_ENV);
            let mut out = Store {
                active_env: e,
                ..Store::default()
            };
            out.set(
                e,
                CredRecord {
                    api_key: api_key.to_string(),
                    wallet_address: raw
                        .get("wallet_address")
                        .and_then(|w| w.as_str())
                        .map(|w| w.to_string()),
                },
            );
            return out;
        }
    }

    Store::default()
}

/// Read + migrate in memory. Never writes, never errors out — missing/corrupt
/// files yield an empty store.
pub fn load_store(path: &Path) -> Store {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Store::default(),
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => migrate_store(&v),
        Err(_) => Store::default(),
    }
}

/// Merge-on-write: read the latest store, set ONLY this env's slot (slimmed),
/// bump active_env, then write atomically (temp + rename, 0600). The merge
/// prevents a concurrent save to the other env from being clobbered.
pub fn save_creds_for_env(path: &Path, env: VaibotEnv, rec: CredRecord) -> Result<Store, CliError> {
    if rec.api_key.is_empty() {
        return Err(CliError::Runtime("save_creds_for_env: api_key required".into()));
    }
    let mut store = load_store(path);
    store.set(env, rec);
    store.active_env = env;
    atomic::write_json_atomic_0600(path, &store.to_json())?;
    Ok(store)
}

/// The stored api_key for a SPECIFIC env (not the active one). Use when a command
/// targets an explicit env so one env's key is never sent to another env's API.
pub fn api_key_for_env(store: &Store, env: VaibotEnv) -> Option<String> {
    store.get(env).map(|r| r.api_key.clone())
}

/// Switch active_env to `env` without touching any key — only when a credential for
/// `env` already exists (never make an empty env active). Preserves BOTH env slots,
/// so you can hold staging + production keys and flip between them.
pub fn save_active_env(path: &Path, env: VaibotEnv) -> Result<(), CliError> {
    let mut store = load_store(path);
    if store.get(env).is_some() && store.active_env != env {
        store.active_env = env;
        atomic::write_json_atomic_0600(path, &store.to_json())?;
    }
    Ok(())
}

/// Resolve which env a surface should operate in.
///
/// Precedence: VAIBOT_ENV → VAIBOT_API_URL → VAIBOT_API_KEY prefix →
///             stored active key prefix → stored active_env → default.
pub fn resolve_env(env: &dyn EnvSource, store: &Store) -> VaibotEnv {
    if let Some(v) = env.get("VAIBOT_ENV") {
        if let Some(e) = VaibotEnv::parse(&v) {
            return e;
        }
    }
    if let Some(u) = env.get("VAIBOT_API_URL") {
        if let Some(e) = env_for_api_url(&u) {
            return e;
        }
    }
    if let Some(k) = env.get("VAIBOT_API_KEY") {
        if let Some(e) = env_for_key(&k) {
            return e;
        }
    }
    if let Some(rec) = store.get(store.active_env) {
        if let Some(e) = env_for_key(&rec.api_key) {
            return e;
        }
    }
    store.active_env
}

/// All-in-one resolver result.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub env: VaibotEnv,
    pub api_base_url: String,
    /// `None` when the only candidate fails the prefix guard (see key_mismatch).
    pub api_key: Option<String>,
    /// Stored public address for the env (display only), or `None`.
    pub wallet_address: Option<String>,
    /// `true` when a candidate key's prefix names a different env.
    pub key_mismatch: bool,
}

/// The entry point surfaces call to talk to the right API.
pub fn resolve_credentials(env: &dyn EnvSource, store: &Store) -> Resolved {
    let env_name = resolve_env(env, store);
    let api_base_url = api_base_for_env(env_name, env.get("VAIBOT_API_URL").as_deref());
    let record = store.get(env_name);
    let wallet_address = record.and_then(|r| r.wallet_address.clone());

    let candidate = env
        .get("VAIBOT_API_KEY")
        .filter(|s| !s.is_empty())
        .or_else(|| record.map(|r| r.api_key.clone()));

    let mut api_key = candidate.clone();
    let mut key_mismatch = false;
    if let Some(c) = &candidate {
        if !key_prefix_matches_env(c, env_name) {
            api_key = None;
            key_mismatch = true;
        }
    }

    Resolved {
        env: env_name,
        api_base_url,
        api_key,
        wallet_address,
        key_mismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MapEnv;

    fn map(pairs: &[(&str, &str)]) -> MapEnv {
        MapEnv(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn env_for_key_recognizes_prefixes() {
        assert_eq!(env_for_key("vb_live_abc"), Some(VaibotEnv::Production));
        assert_eq!(env_for_key("vb_stg_abc"), Some(VaibotEnv::Staging));
        assert_eq!(env_for_key("custom_xyz"), None);
    }

    #[test]
    fn resolve_env_precedence() {
        let store = Store::default();
        assert_eq!(resolve_env(&map(&[("VAIBOT_ENV", "staging")]), &store), VaibotEnv::Staging);
        assert_eq!(
            resolve_env(&map(&[("VAIBOT_API_URL", "https://staging-api.vaibot.io")]), &store),
            VaibotEnv::Staging
        );
        assert_eq!(
            resolve_env(&map(&[("VAIBOT_API_KEY", "vb_live_xyz")]), &store),
            VaibotEnv::Production
        );
        assert_eq!(resolve_env(&map(&[]), &store), VaibotEnv::Production);
    }

    #[test]
    fn resolve_credentials_flags_key_mismatch() {
        let mut store = Store::default();
        store.set(
            VaibotEnv::Staging,
            CredRecord {
                api_key: "vb_live_oops".into(),
                wallet_address: None,
            },
        );
        store.active_env = VaibotEnv::Staging;
        // env forced to staging, but the stored key is a prod key.
        let r = resolve_credentials(&map(&[("VAIBOT_ENV", "staging")]), &store);
        assert_eq!(r.env, VaibotEnv::Staging);
        assert!(r.api_key.is_none());
        assert!(r.key_mismatch);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("credentials.json");
        save_creds_for_env(
            &p,
            VaibotEnv::Production,
            CredRecord {
                api_key: "vb_live_abc".into(),
                wallet_address: Some("0xdead".into()),
            },
        )
        .unwrap();
        let store = load_store(&p);
        assert_eq!(store.active_env, VaibotEnv::Production);
        let rec = store.get(VaibotEnv::Production).unwrap();
        assert_eq!(rec.api_key, "vb_live_abc");
        assert_eq!(rec.wallet_address.as_deref(), Some("0xdead"));
    }
}
