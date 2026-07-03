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

/// Best-effort decode of the `sub` + `email` claims from a JWT payload (the middle
/// base64url segment). Works on either the id_token or the access_token — both are
/// JWTs carrying those claims. Returns `(None, None)` on any parse failure.
pub fn claims_from_jwt(jwt: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(jwt) = jwt else {
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

/// Resolve `(subject, email)` for a session, preferring the id_token but falling back
/// to the access_token when the server issues no id_token. Our OAuth server returns
/// only access + refresh; the access token is a Supabase JWT that also carries the
/// `sub` + `email` claims. Without this fallback the CLI never learns who you are —
/// whoami shows "(unknown)" and identity/claim flows treat you as a new user.
pub fn identity_from_tokens(
    access_token: &str,
    id_token: Option<&str>,
) -> (Option<String>, Option<String>) {
    let (sub, email) = claims_from_jwt(id_token);
    if sub.is_none() && email.is_none() {
        claims_from_jwt(Some(access_token))
    } else {
        (sub, email)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt_with(claims: &str) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.as_bytes());
        format!("header.{payload}.sig")
    }

    #[test]
    fn decodes_sub_and_email_from_jwt() {
        let jwt = jwt_with(r#"{"sub":"user-1","email":"a@b.c"}"#);
        let (sub, email) = claims_from_jwt(Some(&jwt));
        assert_eq!(sub.as_deref(), Some("user-1"));
        assert_eq!(email.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn malformed_token_yields_none() {
        assert_eq!(claims_from_jwt(Some("garbage")), (None, None));
        assert_eq!(claims_from_jwt(None), (None, None));
    }

    #[test]
    fn identity_falls_back_to_access_token_when_no_id_token() {
        // The server issues no id_token; the access token (a Supabase JWT) has sub+email.
        let access = jwt_with(r#"{"sub":"acc-9","email":"me@vaibot.io"}"#);
        let (sub, email) = identity_from_tokens(&access, None);
        assert_eq!(sub.as_deref(), Some("acc-9"));
        assert_eq!(email.as_deref(), Some("me@vaibot.io"));
    }

    #[test]
    fn identity_prefers_id_token_over_access_token() {
        let id = jwt_with(r#"{"sub":"id-sub","email":"id@x.io"}"#);
        let access = jwt_with(r#"{"sub":"acc-sub","email":"acc@x.io"}"#);
        let (sub, email) = identity_from_tokens(&access, Some(&id));
        assert_eq!(sub.as_deref(), Some("id-sub"));
        assert_eq!(email.as_deref(), Some("id@x.io"));
    }
}
