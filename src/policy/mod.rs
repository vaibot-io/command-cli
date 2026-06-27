//! Local policy model + YAML loader (scaffold for the deferred `policy set`).
//!
//! Wired policy WRITES go only through `policy tighten` → POST /v2/policy/request
//! (signed server-side). The YAML→canonical-JSON path here exists so the future
//! `set`/`diff`/`pull` commands have a typed home — they are STUBs today.

pub mod canonical;

use serde::{Deserialize, Serialize};

use crate::error::CliError;

/// A minimal local policy document. The wire/canonical form is produced by
/// `canonical::to_canonical_json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub denylist: Vec<String>,
    #[serde(default)]
    pub classifier_tables: serde_json::Map<String, serde_json::Value>,
}

impl Policy {
    /// Load + validate a policy from a YAML string.
    pub fn load_yaml(s: &str) -> Result<Policy, CliError> {
        let p: Policy = serde_yaml::from_str(s)
            .map_err(|e| CliError::Runtime(format!("policy yaml: {e}")))?;
        p.validate()?;
        Ok(p)
    }

    fn validate(&self) -> Result<(), CliError> {
        if self.denylist.iter().any(|d| d.trim().is_empty()) {
            return Err(CliError::Runtime("policy denylist contains an empty pattern".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_denylist_from_yaml() {
        let p = Policy::load_yaml("denylist:\n  - rm -rf\n  - curl evil\n").unwrap();
        assert_eq!(p.denylist, vec!["rm -rf", "curl evil"]);
    }

    #[test]
    fn rejects_empty_pattern() {
        assert!(Policy::load_yaml("denylist:\n  - '   '\n").is_err());
    }
}
