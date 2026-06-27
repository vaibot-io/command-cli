//! TokenStore — the only module that touches the on-disk OAuth session.
//!
//! Backed by ~/.vaibot/oauth.json (sidecar to credentials.json). Env-namespaced
//! to mirror the api_key store's production|staging split. The broker depends on
//! this INTERFACE, not the file, so a future ScopedCredentialBroker can swap in
//! a mint+cache implementation behind the same shape with zero broker changes.
//!
//! Corrupt files are tolerated (treated as empty), but loose-permission files
//! surface as an error via `read_secret_file`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::creds::VaibotEnv;
use super::{atomic, oauth_path, EnvSource};
use crate::error::CliError;

/// The persisted user OAuth session for one environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthSession {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// epoch ms
    pub expires_at: f64,
    pub token_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Issuer this session was minted against — so refresh hits the SAME one
    /// (e.g. a --api-url staging session must not refresh against prod).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthFile {
    version: u32,
    environments: BTreeMap<String, OAuthSession>,
}

impl Default for OAuthFile {
    fn default() -> Self {
        OAuthFile {
            version: 1,
            environments: BTreeMap::new(),
        }
    }
}

/// The store interface. The real impl is `FileTokenStore`.
pub trait TokenStore: Send + Sync {
    fn load(&self, env: VaibotEnv) -> Result<Option<OAuthSession>, CliError>;
    fn save(&self, session: &OAuthSession, env: VaibotEnv) -> Result<(), CliError>;
    fn clear(&self, env: VaibotEnv) -> Result<(), CliError>;
}

/// File-backed store at oauth.json.
pub struct FileTokenStore {
    path: PathBuf,
}

impl FileTokenStore {
    /// Build a store from the resolved config dir for the given env source.
    pub fn new(env: &dyn EnvSource) -> Self {
        FileTokenStore {
            path: oauth_path(env),
        }
    }

    /// Build a store rooted at an explicit oauth.json path (tests).
    pub fn at(path: PathBuf) -> Self {
        FileTokenStore { path }
    }

    fn read(&self) -> Result<OAuthFile, CliError> {
        // Permission errors surface; absent/corrupt files become an empty file.
        let raw = atomic::read_secret_file(&self.path)?;
        let Some(raw) = raw else {
            return Ok(OAuthFile::default());
        };
        match serde_json::from_str::<OAuthFile>(&raw) {
            Ok(f) if f.version == 1 => Ok(f),
            _ => Ok(OAuthFile::default()),
        }
    }

    fn write(&self, file: &OAuthFile) -> Result<(), CliError> {
        let v = serde_json::to_value(file)
            .map_err(|e| CliError::Runtime(format!("serialize oauth file: {e}")))?;
        atomic::write_json_atomic_0600(&self.path, &v)
    }
}

impl TokenStore for FileTokenStore {
    fn load(&self, env: VaibotEnv) -> Result<Option<OAuthSession>, CliError> {
        let file = self.read()?;
        Ok(file.environments.get(&env.to_string()).cloned())
    }

    fn save(&self, session: &OAuthSession, env: VaibotEnv) -> Result<(), CliError> {
        let mut file = self.read()?;
        file.environments.insert(env.to_string(), session.clone());
        self.write(&file)
    }

    fn clear(&self, env: VaibotEnv) -> Result<(), CliError> {
        let mut file = self.read()?;
        file.environments.remove(&env.to_string());
        self.write(&file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(token: &str) -> OAuthSession {
        OAuthSession {
            access_token: token.into(),
            refresh_token: Some("r".into()),
            expires_at: 1.0,
            token_type: "Bearer".into(),
            scope: Some("openid".into()),
            subject: Some("sub".into()),
            email: Some("a@b.c".into()),
            issuer: Some("https://oauth.vaibot.io".into()),
        }
    }

    #[test]
    fn save_load_clear_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::at(dir.path().join("oauth.json"));
        assert!(store.load(VaibotEnv::Production).unwrap().is_none());
        store.save(&sample("tok"), VaibotEnv::Production).unwrap();
        let got = store.load(VaibotEnv::Production).unwrap().unwrap();
        assert_eq!(got.access_token, "tok");
        // staging is untouched.
        assert!(store.load(VaibotEnv::Staging).unwrap().is_none());
        store.clear(VaibotEnv::Production).unwrap();
        assert!(store.load(VaibotEnv::Production).unwrap().is_none());
    }

    #[test]
    fn corrupt_file_is_treated_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("oauth.json");
        atomic::write_atomic_0600(&p, "{ not json").unwrap();
        let store = FileTokenStore::at(p);
        assert!(store.load(VaibotEnv::Production).unwrap().is_none());
    }
}
