//! Policy → canonical JSON (sorted keys) + a LOCAL sha256 fingerprint.
//!
//! STUB-PATH: this is the scaffold the deferred `policy set`/`diff` will use to
//! produce a deterministic wire form for the server to sign. The sha256 here is
//! a LOCAL/offline fingerprint (per the hash convention: sha256 for local,
//! keccak256 0x only at the anchored V1 leaf — which is NEVER computed here).

use sha2::{Digest, Sha256};

use super::Policy;
use crate::error::CliError;

/// Serialize a policy to canonical JSON with sorted object keys.
pub fn to_canonical_json(policy: &Policy) -> Result<String, CliError> {
    // serde_json with sorted keys: round-trip through a BTreeMap-backed value.
    let value = serde_json::to_value(policy)
        .map_err(|e| CliError::Runtime(format!("policy serialize: {e}")))?;
    let sorted = sort_value(value);
    serde_json::to_string(&sorted).map_err(|e| CliError::Runtime(format!("policy canonical: {e}")))
}

/// LOCAL sha256 fingerprint of the canonical form (no `0x` — not anchored).
pub fn local_fingerprint(policy: &Policy) -> Result<String, CliError> {
    let canon = to_canonical_json(policy)?;
    let mut hasher = Sha256::new();
    hasher.update(canon.as_bytes());
    Ok(hex(&hasher.finalize()))
}

fn sort_value(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<String> = m.keys().cloned().collect();
            keys.sort();
            for k in keys {
                sorted.insert(k.clone(), sort_value(m[&k].clone()));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(a) => {
            serde_json::Value::Array(a.into_iter().map(sort_value).collect())
        }
        other => other,
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_no_0x_prefix() {
        let p = Policy {
            denylist: vec!["rm -rf".into()],
            ..Default::default()
        };
        let fp = local_fingerprint(&p).unwrap();
        assert_eq!(fp.len(), 64);
        assert!(!fp.starts_with("0x"));
        // Deterministic.
        assert_eq!(fp, local_fingerprint(&p).unwrap());
    }
}
