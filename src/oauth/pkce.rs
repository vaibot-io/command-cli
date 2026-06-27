//! PKCE verifier/challenge (S256) + random CSRF state.
//!
//! Thin wrapper over `oauth2`'s primitives so the loopback/device modules share
//! one construction point. The challenge is always S256.

use oauth2::{CsrfToken, PkceCodeChallenge, PkceCodeVerifier};

/// A freshly minted PKCE pair + CSRF state.
pub struct PkcePair {
    pub challenge: PkceCodeChallenge,
    pub verifier: PkceCodeVerifier,
    pub state: CsrfToken,
}

/// Generate a new S256 PKCE pair and random state.
pub fn generate() -> PkcePair {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    PkcePair {
        challenge,
        verifier,
        state: CsrfToken::new_random(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oauth2::PkceCodeChallengeMethod;

    #[test]
    fn generates_s256_challenge_and_nonempty_state() {
        let p = generate();
        assert_eq!(p.challenge.method(), &PkceCodeChallengeMethod::new("S256".to_string()));
        assert!(!p.state.secret().is_empty());
        assert!(!p.verifier.secret().is_empty());
    }
}
