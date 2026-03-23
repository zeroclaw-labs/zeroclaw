//! CSRF (Cross-Site Request Forgery) protection for the Axum gateway.
//!
//! Uses the **Signed-Double-Submit** pattern:
//! - The server generates a token as `HMAC-SHA256(secret, nonce)` where `nonce`
//!   is a random value embedded in the token itself.
//! - The token is served via `GET /api/csrf-token` and must be included as the
//!   `X-CSRF-Token` header on every state-mutating request (POST/PUT/DELETE/PATCH).
//! - Because the token is in a header — not a cookie — browsers cannot forge it
//!   across origins via a simple HTML form submission.
//!
//! ## Exemptions (no CSRF check applied)
//! - `GET`, `HEAD`, `OPTIONS` (safe / idempotent methods)
//! - `/webhook`, `/whatsapp`, `/linq`, `/wati`, `/nextcloud-talk`
//!   (third-party webhooks; these use their own HMAC signatures)
//! - `/ws/*` (WebSocket upgrades — browser does not send custom headers during
//!   the upgrade handshake)
//! - `/pair` (has its own pairing-code challenge)
//! - `/health`, `/metrics` (read-only monitoring)
//!
//! ## Usage
//!
//! ```rust,ignore
//! use zeroclaw::gateway::csrf::CsrfProtection;
//!
//! let csrf = CsrfProtection::new(); // random secret per process
//! let token = csrf.issue_token();
//! assert!(csrf.validate_token(&token));
//! ```

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// CSRF protection helper.
///
/// Holds a 32-byte secret generated at gateway startup. Each token embeds a
/// random nonce so tokens are single-use (the caller is responsible for
/// enforcing this by not reusing `X-CSRF-Token` values across requests if
/// strict replay prevention is required; for the dashboard the default is
/// stateless validation which is sufficient against CSRF attacks).
#[derive(Clone)]
pub struct CsrfProtection {
    secret: [u8; 32],
}

impl CsrfProtection {
    /// Create a new instance with a freshly generated random secret.
    pub fn new() -> Self {
        Self {
            secret: rand::random(),
        }
    }

    /// Issue a new CSRF token. The token is `base64url(nonce || hmac)` where
    /// `nonce` is 16 random bytes and `hmac` is `HMAC-SHA256(secret, nonce)`.
    pub fn issue_token(&self) -> String {
        let nonce: [u8; 16] = rand::random();
        let mac = self.compute_mac(&nonce);

        let mut raw = Vec::with_capacity(16 + 32);
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&mac);
        URL_SAFE_NO_PAD.encode(&raw)
    }

    /// Validate a token previously issued by [`issue_token`].
    ///
    /// Returns `false` on malformed tokens or MAC mismatches.
    pub fn validate_token(&self, token: &str) -> bool {
        let Ok(raw) = URL_SAFE_NO_PAD.decode(token.trim()) else {
            return false;
        };

        if raw.len() != 48 {
            // 16-byte nonce + 32-byte HMAC
            return false;
        }

        let (nonce, provided_mac) = raw.split_at(16);
        let expected_mac = self.compute_mac(nonce);

        // Constant-time comparison to prevent timing attacks.
        constant_time_eq(provided_mac, &expected_mac)
    }

    /// Returns `true` when the request path is exempt from CSRF validation.
    ///
    /// Exempt paths are third-party webhook ingress points or routes that have
    /// their own authentication scheme.
    pub fn is_exempt_path(path: &str) -> bool {
        // Normalise the path (strip query string / fragment).
        let path = path.split('?').next().unwrap_or(path);
        let path = path.split('#').next().unwrap_or(path);

        matches!(
            path,
            "/webhook"
                | "/whatsapp"
                | "/linq"
                | "/wati"
                | "/nextcloud-talk"
                | "/pair"
                | "/health"
                | "/metrics"
        ) || path.starts_with("/ws/")
    }

    fn compute_mac(&self, nonce: &[u8]) -> [u8; 32] {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(nonce);
        mac.finalize().into_bytes().into()
    }
}

impl Default for CsrfProtection {
    fn default() -> Self {
        Self::new()
    }
}

/// Constant-time byte-slice comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_validate_roundtrip() {
        let csrf = CsrfProtection::new();
        let token = csrf.issue_token();
        assert!(
            csrf.validate_token(&token),
            "freshly issued token must validate"
        );
    }

    #[test]
    fn different_instances_share_nothing() {
        // Tokens from one instance should not validate against another.
        let a = CsrfProtection::new();
        let b = CsrfProtection::new();
        let token = a.issue_token();
        assert!(
            !b.validate_token(&token),
            "token from instance A must not validate against instance B"
        );
    }

    #[test]
    fn tampered_token_rejected() {
        let csrf = CsrfProtection::new();
        let mut token = csrf.issue_token();
        // Flip the last character.
        let last = token.pop().unwrap();
        let replacement = if last == 'A' { 'B' } else { 'A' };
        token.push(replacement);
        assert!(
            !csrf.validate_token(&token),
            "tampered token must be rejected"
        );
    }

    #[test]
    fn empty_token_rejected() {
        let csrf = CsrfProtection::new();
        assert!(!csrf.validate_token(""));
        assert!(!csrf.validate_token("   "));
    }

    #[test]
    fn each_token_is_unique() {
        let csrf = CsrfProtection::new();
        let t1 = csrf.issue_token();
        let t2 = csrf.issue_token();
        assert_ne!(t1, t2, "each issued token must be unique");
    }

    #[test]
    fn exempt_paths_are_correctly_identified() {
        assert!(CsrfProtection::is_exempt_path("/webhook"));
        assert!(CsrfProtection::is_exempt_path("/whatsapp"));
        assert!(CsrfProtection::is_exempt_path("/linq"));
        assert!(CsrfProtection::is_exempt_path("/wati"));
        assert!(CsrfProtection::is_exempt_path("/nextcloud-talk"));
        assert!(CsrfProtection::is_exempt_path("/pair"));
        assert!(CsrfProtection::is_exempt_path("/health"));
        assert!(CsrfProtection::is_exempt_path("/metrics"));
        assert!(CsrfProtection::is_exempt_path("/ws/chat"));
        assert!(CsrfProtection::is_exempt_path("/ws/nodes"));
        // Non-exempt
        assert!(!CsrfProtection::is_exempt_path("/api/config"));
        assert!(!CsrfProtection::is_exempt_path("/api/memory"));
        assert!(!CsrfProtection::is_exempt_path("/admin/shutdown"));
    }

    #[test]
    fn query_string_stripped_from_exempt_check() {
        assert!(CsrfProtection::is_exempt_path(
            "/webhook?hub.verify_token=foo"
        ));
        assert!(!CsrfProtection::is_exempt_path("/api/config?debug=1"));
    }
}
