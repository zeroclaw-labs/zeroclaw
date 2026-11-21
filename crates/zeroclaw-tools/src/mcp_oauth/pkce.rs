//! RFC 7636 PKCE (S256) and flow state material.
//!
//! The verifier and state are fresh CSPRNG output per flow; nothing here is
//! shared with the provider OAuth helpers in `zeroclaw-providers` (their PKCE
//! utilities are `pub(super)`-adjacent to their own flows, and the dependency
//! direction forbids importing them from `zeroclaw-tools`).

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

/// Base64url without padding, the alphabet RFC 7636 and RFC 6749 require for
/// PKCE parameters and a sane flow state.
fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// One flow's PKCE pair: the secret verifier and the challenge the
/// authorization server sees.
#[derive(Clone)]
pub struct Pkce {
    /// High-entropy per-flow secret, sent only to the token endpoint.
    pub verifier: String,
    /// `BASE64URL(SHA256(verifier))`, sent on the authorization URL.
    pub challenge: String,
}

/// RFC 7636 §5 keeps the verifier confidential until the token exchange;
/// derived `Debug` would print it into logs and test output, so `verifier`
/// is redacted here (the challenge is public by design and may show).
impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"[redacted]")
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    /// Generate a fresh verifier/challenge pair. 32 CSPRNG bytes encode to 43
    /// base64url characters, the minimum length RFC 7636 §4.1 allows, and
    /// SHA-256 preimage security is unchanged.
    pub fn generate() -> Self {
        let bytes: [u8; 32] = rand::random();
        Self::from_verifier(&b64url(&bytes))
    }

    /// Derive the pair from an existing verifier (used by tests against the
    /// RFC 7636 Appendix B vector and by any resume path that must reuse a
    /// stored verifier).
    pub fn from_verifier(verifier: &str) -> Self {
        Self {
            verifier: verifier.to_string(),
            challenge: challenge_for(verifier),
        }
    }
}

/// `S256` challenge derivation: `BASE64URL(SHA256(ASCII(verifier)))`.
pub fn challenge_for(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

/// Fresh one-shot CSRF/state token for one flow (RFC 6749 §10.12). 32 CSPRNG
/// bytes, so a callback cannot guess or brute-force a live flow.
pub fn new_state() -> String {
    let bytes: [u8; 32] = rand::random();
    b64url(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_matches_rfc_7636_appendix_b_vector() {
        // The RFC's worked example pins the exact derivation.
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_and_challenge_are_rfc_shaped() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier.len(), 43, "32 bytes b64url = 43 chars");
        assert!(
            pkce.verifier
                .chars()
                .all(|c| { c.is_ascii_alphanumeric() || c == '-' || c == '_' })
        );
        assert_eq!(pkce.challenge.len(), 43);
        assert_ne!(pkce.verifier, pkce.challenge);
        assert_eq!(
            Pkce::from_verifier(&pkce.verifier).challenge,
            pkce.challenge
        );
    }

    #[test]
    fn generated_values_are_unique_across_flows() {
        // Not a statistical test; a single collision between two CSPRNG draws
        // would mean a broken RNG, which is exactly what we want to catch.
        assert_ne!(Pkce::generate().verifier, Pkce::generate().verifier);
        assert_ne!(new_state(), new_state());
    }
}
