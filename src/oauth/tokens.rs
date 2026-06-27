//! Token request/response types shared by the loopback + device flows.
//!
//! `TokenSet` is the normalized shape `session_from_tokens` consumes. The id
//! token's `sub`/`email` claims are decoded best-effort (no signature
//! verification — the CLI trusts the issuer it just spoke TLS to; the access
//! token is the security-bearing artifact).

use base64::Engine;

/// Normalized token-endpoint response.
#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
    pub subject: Option<String>,
    pub email: Option<String>,
}

/// Best-effort decode of the `sub` + `email` claims from an id_token's payload
/// (the middle base64url segment). Returns `(None, None)` on any parse failure.
pub fn claims_from_id_token(id_token: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(jwt) = id_token else {
        return (None, None);
    };
    let mut parts = jwt.split('.');
    let (_, payload, _) = (parts.next(), parts.next(), parts.next());
    let Some(payload) = payload else {
        return (None, None);
    };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return (None, None);
    };
    let sub = value.get("sub").and_then(|s| s.as_str()).map(String::from);
    let email = value.get("email").and_then(|s| s.as_str()).map(String::from);
    (sub, email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_sub_and_email_from_id_token() {
        // {"sub":"user-1","email":"a@b.c"}
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"user-1","email":"a@b.c"}"#);
        let jwt = format!("header.{payload}.sig");
        let (sub, email) = claims_from_id_token(Some(&jwt));
        assert_eq!(sub.as_deref(), Some("user-1"));
        assert_eq!(email.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn malformed_token_yields_none() {
        assert_eq!(claims_from_id_token(Some("garbage")), (None, None));
        assert_eq!(claims_from_id_token(None), (None, None));
    }
}
