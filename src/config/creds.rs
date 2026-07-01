//! Credential store + environment resolver — a faithful port of the vendored
//! `creds.mjs`.
//!
//! Single source of truth for: which environment a surface operates in, where
//! credentials live on disk, and how to read/write them without one env
//! clobbering the other.
//!
//! Store schema (v3), ~/.vaibot/credentials.json:
//!   {
//!     "version": 3,
//!     "active_env": "production",
//!     "environments": {
//!       "production": {
//!         "api_key": "vb_live_…", "wallet_address": "0x…",
//!         "governance": { "url": null },   // V2; null ⇒ canonical default
//!         "provenance": { "url": null }    // V1; null ⇒ canonical default
//!       },
//!       "staging": { "api_key": "vb_stg_…", "wallet_address": "0x…" }
//!     }
//!   }
//!
//! api_key + wallet_address persist; governance/provenance URLs persist only when an
//! explicit override is stored — otherwise each resolves to its canonical per-env
//! default. V1 provenance and V2 governance bases are tracked SEPARATELY so a staging
//! key can never anchor to a prod provenance endpoint. See docs/credentials-v2-split.md.
//! A v2 file (no governance/provenance slots) reads transparently and upgrades on next
//! write — the slots default to the canonical URLs.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{atomic, EnvSource};
use crate::error::CliError;

pub const STORE_VERSION: u32 = 3;

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

    /// Canonical V2 governance base for this env.
    fn api_base(self) -> &'static str {
        match self {
            VaibotEnv::Production => "https://api.vaibot.io",
            VaibotEnv::Staging => "https://staging-api.vaibot.io",
        }
    }

    /// Canonical V1 provenance base for this env. The V1 proxy routes under `/api`,
    /// so the base INCLUDES `/api`; callers append `/prove`.
    fn provenance_base(self) -> &'static str {
        match self {
            VaibotEnv::Production => "https://provenance.vaibot.io/api",
            VaibotEnv::Staging => "https://vaibot-api-v1.fly.dev/api",
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

/// Per-API endpoint slot. `url` None ⇒ use the env's canonical default. Modeled as
/// a nested object (not a flat field) so approach B can add an optional per-slot
/// `api_key` later without reshaping the file or re-migrating.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApiSlot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredRecord {
    pub api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wallet_address: Option<String>,
    /// V2 governance endpoint override (None ⇒ canonical default for the env).
    #[serde(default)]
    pub governance: ApiSlot,
    /// V1 provenance endpoint override (None ⇒ canonical default for the env).
    #[serde(default)]
    pub provenance: ApiSlot,
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
    // Persist a slot only when it carries an explicit override — a stock record
    // (URLs derived from canonical defaults) stays as { api_key, wallet_address }.
    for (key, slot) in [("governance", &rec.governance), ("provenance", &rec.provenance)] {
        if let Some(u) = &slot.url {
            if !u.is_empty() {
                m.insert(key.into(), serde_json::json!({ "url": u }));
            }
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
    // Optional per-API slots; absent (v2 file) ⇒ ApiSlot::default ⇒ canonical URL.
    let slot = |key: &str| ApiSlot {
        url: v
            .get(key)
            .and_then(|s| s.get("url"))
            .and_then(|u| u.as_str())
            .filter(|u| !u.is_empty())
            .map(|u| u.to_string()),
    };
    Some(CredRecord {
        api_key: api_key.to_string(),
        wallet_address,
        governance: slot("governance"),
        provenance: slot("provenance"),
    })
}

/// Pure: normalize any parsed JSON into a v2 store. Never panics.
pub fn migrate_store(raw: &serde_json::Value) -> Store {
    if !raw.is_object() {
        return Store::default();
    }
    // v2 AND v3 both nest credentials under `environments` — gate on its presence,
    // NOT on the version number (gating on `version >= 3` would drop a v2 file to an
    // empty store). record_from_value reads the optional governance/provenance slots;
    // absent in a v2 file, so it upgrades to v3 transparently on the next write.
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
                    ..Default::default()
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

/// V2 GOVERNANCE base for an env: explicit override → stored slot url → canonical.
/// Policy, mode-poll, decide, and governance receipts all hang off this.
pub fn governance_base_for_env(store: &Store, env: VaibotEnv, override_url: Option<&str>) -> String {
    slot_base(override_url, store.get(env).map(|r| &r.governance.url), env.api_base())
}

/// V1 PROVENANCE base for an env: explicit override → stored slot url → canonical.
/// `/prove` anchoring hangs off this. Tracked separately from governance so a
/// staging key can never anchor to the prod provenance endpoint.
pub fn provenance_base_for_env(store: &Store, env: VaibotEnv, override_url: Option<&str>) -> String {
    slot_base(override_url, store.get(env).map(|r| &r.provenance.url), env.provenance_base())
}

/// Is the deliberate-act flag for a URL override set? (`VAIBOT_ALLOW_URL_OVERRIDE`
/// = `1`/`true`/`yes`.) Required — together with an admin account, enforced by the
/// production-env preflight — to redirect a *production* base off its canonical host.
pub fn url_override_allowed(env: &dyn EnvSource) -> bool {
    env.get("VAIBOT_ALLOW_URL_OVERRIDE")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "TRUE"))
        .unwrap_or(false)
}

/// §5 LOCAL gate on a requested URL override — the env-injectable half.
///
/// Returns the override the base resolver should actually use:
/// - non-production: the override is honored (callers may WARN-log the divergence).
/// - production: honored ONLY when `allow_override` is set; otherwise SUPPRESSED
///   (→ stored slot / canonical), so a prod key is never silently diverted by an
///   env var alone. The ADMIN half (a prod override is admitted only for an admin
///   account) needs a network `/me` check and is enforced by the preflight, which
///   refuses the command for a non-admin — this pure gate just makes the safe
///   default unbypassable. `None`/empty in ⇒ `None` out.
pub fn gate_url_override<'a>(
    env: VaibotEnv,
    requested: Option<&'a str>,
    allow_override: bool,
) -> Option<&'a str> {
    let u = requested?;
    if u.is_empty() {
        return None;
    }
    if env == VaibotEnv::Production && !allow_override {
        return None; // suppressed → stored slot / canonical
    }
    Some(u)
}

/// Shared precedence: non-empty override → non-empty stored slot url → canonical.
fn slot_base(override_url: Option<&str>, stored: Option<&Option<String>>, canonical: &str) -> String {
    if let Some(u) = override_url {
        if !u.is_empty() {
            return u.trim_end_matches('/').to_string();
        }
    }
    if let Some(Some(u)) = stored {
        if !u.is_empty() {
            return u.trim_end_matches('/').to_string();
        }
    }
    canonical.to_string()
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
    /// V2 governance base (policy / mode / decide / receipts).
    pub api_base_url: String,
    /// V1 provenance base (`/prove` anchoring) — resolved separately from governance.
    pub provenance_base_url: String,
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
    // V2 governance: VAIBOT_GOVERNANCE_URL → stored slot → canonical.
    // V1 provenance: VAIBOT_PROVENANCE_URL → stored slot → canonical.
    // NOTE: deprecated VAIBOT_API_URL overrides NEITHER base — it is too overloaded
    // (historically the CLI's V2 base but the guard's /prove base). It survives only
    // for env inference (resolve_env); redirects need the explicit per-API vars.
    // §5 flag-gate: a PRODUCTION override is suppressed here unless the deliberate-act
    // flag is set (the admin half is enforced by the preflight). This makes the safe
    // default unbypassable even on a path that skips the preflight.
    let allow = url_override_allowed(env);
    let gov_override = env.get("VAIBOT_GOVERNANCE_URL");
    let gov_gated = gate_url_override(env_name, gov_override.as_deref(), allow);
    let api_base_url = governance_base_for_env(store, env_name, gov_gated);
    let prov_override = env.get("VAIBOT_PROVENANCE_URL");
    let prov_gated = gate_url_override(env_name, prov_override.as_deref(), allow);
    let provenance_base_url = provenance_base_for_env(store, env_name, prov_gated);
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
        provenance_base_url,
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
                ..Default::default()
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
    fn url_override_allowed_parses_the_flag() {
        assert!(url_override_allowed(&map(&[("VAIBOT_ALLOW_URL_OVERRIDE", "1")])));
        assert!(url_override_allowed(&map(&[("VAIBOT_ALLOW_URL_OVERRIDE", "true")])));
        assert!(url_override_allowed(&map(&[("VAIBOT_ALLOW_URL_OVERRIDE", "yes")])));
        assert!(!url_override_allowed(&map(&[("VAIBOT_ALLOW_URL_OVERRIDE", "0")])));
        assert!(!url_override_allowed(&map(&[])));
    }

    #[test]
    fn gate_url_override_suppresses_prod_without_flag() {
        // production: dropped without the flag, honored with it.
        assert_eq!(gate_url_override(VaibotEnv::Production, Some("https://evil"), false), None);
        assert_eq!(
            gate_url_override(VaibotEnv::Production, Some("https://ok"), true),
            Some("https://ok")
        );
        // staging: always honored (the flag is a production-only gate).
        assert_eq!(
            gate_url_override(VaibotEnv::Staging, Some("https://stg"), false),
            Some("https://stg")
        );
        // empty / absent ⇒ no override.
        assert_eq!(gate_url_override(VaibotEnv::Production, Some(""), true), None);
        assert_eq!(gate_url_override(VaibotEnv::Production, None, true), None);
    }

    #[test]
    fn resolve_credentials_drops_prod_url_override_without_flag() {
        let mut store = Store::default();
        store.set(
            VaibotEnv::Production,
            CredRecord { api_key: "vb_live_abc".into(), ..Default::default() },
        );
        // A prod governance override with NO flag → canonical base, key stays put.
        let r = resolve_credentials(
            &map(&[("VAIBOT_GOVERNANCE_URL", "https://evil.example")]),
            &store,
        );
        assert_eq!(r.env, VaibotEnv::Production);
        assert_eq!(r.api_base_url, "https://api.vaibot.io");

        // Same override WITH the flag → honored (the admin half is the preflight's job).
        let r2 = resolve_credentials(
            &map(&[
                ("VAIBOT_GOVERNANCE_URL", "https://ok.example"),
                ("VAIBOT_ALLOW_URL_OVERRIDE", "1"),
            ]),
            &store,
        );
        assert_eq!(r2.api_base_url, "https://ok.example");

        // Provenance override is gated the same way.
        let r3 = resolve_credentials(
            &map(&[("VAIBOT_PROVENANCE_URL", "https://evil.example/api")]),
            &store,
        );
        assert_eq!(r3.provenance_base_url, "https://provenance.vaibot.io/api");
    }

    #[test]
    fn resolve_credentials_honors_staging_override_without_flag() {
        let mut store = Store::default();
        store.set(
            VaibotEnv::Staging,
            CredRecord { api_key: "vb_stg_abc".into(), ..Default::default() },
        );
        store.active_env = VaibotEnv::Staging;
        let r = resolve_credentials(
            &map(&[
                ("VAIBOT_ENV", "staging"),
                ("VAIBOT_GOVERNANCE_URL", "https://stg-override.example"),
            ]),
            &store,
        );
        assert_eq!(r.env, VaibotEnv::Staging);
        assert_eq!(r.api_base_url, "https://stg-override.example");
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
                ..Default::default()
            },
        )
        .unwrap();
        let store = load_store(&p);
        assert_eq!(store.active_env, VaibotEnv::Production);
        let rec = store.get(VaibotEnv::Production).unwrap();
        assert_eq!(rec.api_key, "vb_live_abc");
        assert_eq!(rec.wallet_address.as_deref(), Some("0xdead"));
    }

    // ── v3: split provenance (V1) / governance (V2) ──────────────────────────

    #[test]
    fn v2_file_loads_and_resolves_canonical_urls() {
        // A pre-split v2 file (no governance/provenance slots) must still load, and
        // each base resolves to the canonical default for its env (gating on
        // `environments` presence, not the version number).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("credentials.json");
        std::fs::write(
            &p,
            r#"{"version":2,"active_env":"staging","environments":{"staging":{"api_key":"vb_stg_x"}}}"#,
        )
        .unwrap();
        let store = load_store(&p);
        assert_eq!(store.active_env, VaibotEnv::Staging);
        assert_eq!(store.get(VaibotEnv::Staging).unwrap().api_key, "vb_stg_x");
        assert_eq!(governance_base_for_env(&store, VaibotEnv::Staging, None), "https://staging-api.vaibot.io");
        assert_eq!(provenance_base_for_env(&store, VaibotEnv::Staging, None), "https://vaibot-api-v1.fly.dev/api");
    }

    #[test]
    fn canonical_bases_per_env() {
        let store = Store::default();
        assert_eq!(governance_base_for_env(&store, VaibotEnv::Production, None), "https://api.vaibot.io");
        assert_eq!(provenance_base_for_env(&store, VaibotEnv::Production, None), "https://provenance.vaibot.io/api");
        assert_eq!(provenance_base_for_env(&store, VaibotEnv::Staging, None), "https://vaibot-api-v1.fly.dev/api");
    }

    #[test]
    fn stored_slot_urls_override_canonical_and_roundtrip_as_v3() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("credentials.json");
        save_creds_for_env(
            &p,
            VaibotEnv::Staging,
            CredRecord {
                api_key: "vb_stg_x".into(),
                governance: ApiSlot { url: Some("https://gov.example".into()) },
                provenance: ApiSlot { url: Some("https://prov.example/api".into()) },
                ..Default::default()
            },
        )
        .unwrap();
        let store = load_store(&p);
        assert_eq!(governance_base_for_env(&store, VaibotEnv::Staging, None), "https://gov.example");
        assert_eq!(provenance_base_for_env(&store, VaibotEnv::Staging, None), "https://prov.example/api");
        let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(raw["version"], 3);
        assert_eq!(raw["environments"]["staging"]["provenance"]["url"], "https://prov.example/api");
    }

    #[test]
    fn explicit_override_beats_stored_and_canonical() {
        let store = Store::default();
        assert_eq!(
            governance_base_for_env(&store, VaibotEnv::Production, Some("https://override.example/")),
            "https://override.example" // trailing slash trimmed
        );
        assert_eq!(
            provenance_base_for_env(&store, VaibotEnv::Production, Some("https://prov.override/api")),
            "https://prov.override/api"
        );
    }

    #[test]
    fn resolve_carries_both_bases() {
        let store = Store::default();
        let r = resolve_credentials(&map(&[("VAIBOT_ENV", "production")]), &store);
        assert_eq!(r.api_base_url, "https://api.vaibot.io");
        assert_eq!(r.provenance_base_url, "https://provenance.vaibot.io/api");

        let r2 = resolve_credentials(
            &map(&[("VAIBOT_ENV", "staging"), ("VAIBOT_PROVENANCE_URL", "https://p.example/api")]),
            &store,
        );
        assert_eq!(r2.api_base_url, "https://staging-api.vaibot.io");
        assert_eq!(r2.provenance_base_url, "https://p.example/api");
    }
}
