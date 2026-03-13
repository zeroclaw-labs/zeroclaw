//! Common OAuth2 utilities shared across providers.
//!
//! This module contains shared functionality for OAuth2 authentication:
//! - PKCE (Proof Key for Code Exchange) state generation
//! - URL encoding/decoding
//! - Query parameter parsing

use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// PKCE state container for OAuth2 authorization code flow.
#[derive(Debug, Clone)]
pub struct PkceState {
    pub code_verifier: String,
    pub code_challenge: String,
    pub state: String,
}

/// Generate a new PKCE state with cryptographically random values.
///
/// Creates a code verifier, derives the S256 code challenge, and generates
/// a random state parameter for CSRF protection.
pub fn generate_pkce_state() -> PkceState {
    let code_verifier = random_base64url(64);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);

    PkceState {
        code_verifier,
        code_challenge,
        state: random_base64url(24),
    }
}

/// Generate a cryptographically random base64url-encoded string.
pub fn random_base64url(byte_len: usize) -> String {
    use chacha20poly1305::aead::{rand_core::RngCore, OsRng};

    let mut bytes = vec![0_u8; byte_len];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// URL-encode a string using percent encoding (RFC 3986).
pub fn url_encode(input: &str) -> String {
    input
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect::<String>()
}

/// URL-decode a percent-encoded string.
pub fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = bytes[i + 1] as char;
                let lo = bytes[i + 2] as char;
                if let (Some(h), Some(l)) = (hi.to_digit(16), lo.to_digit(16)) {
                    if let Ok(value) = u8::try_from(h * 16 + l) {
                        out.push(value);
                        i += 3;
                        continue;
                    }
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).to_string()
}

/// Parse URL query parameters into a BTreeMap.
///
/// Handles URL-encoded keys and values.
pub fn parse_query_params(input: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.insert(url_decode(key), url_decode(value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_generation_is_valid() {
        let pkce = generate_pkce_state();
        // Code verifier should be at least 43 chars (base64url of 32 bytes)
        assert!(pkce.code_verifier.len() >= 43);
        assert!(!pkce.code_challenge.is_empty());
        assert!(!pkce.state.is_empty());
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let pkce = generate_pkce_state();
        let expected = {
            let digest = Sha256::digest(pkce.code_verifier.as_bytes());
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
        };
        assert_eq!(pkce.code_challenge, expected);
    }

    #[test]
    fn url_encode_basic() {
        assert_eq!(url_encode("hello"), "hello");
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a=b&c=d"), "a%3Db%26c%3Dd");
    }

    #[test]
    fn url_decode_basic() {
        assert_eq!(url_decode("hello"), "hello");
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("hello+world"), "hello world");
        assert_eq!(url_decode("a%3Db%26c%3Dd"), "a=b&c=d");
    }

    #[test]
    fn url_encode_decode_roundtrip() {
        let original = "hello world! @#$%^&*()";
        let encoded = url_encode(original);
        let decoded = url_decode(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn parse_query_params_basic() {
        let params = parse_query_params("code=abc123&state=xyz");
        assert_eq!(params.get("code"), Some(&"abc123".to_string()));
        assert_eq!(params.get("state"), Some(&"xyz".to_string()));
    }

    #[test]
    fn parse_query_params_encoded() {
        let params = parse_query_params("name=hello%20world&value=a%3Db");
        assert_eq!(params.get("name"), Some(&"hello world".to_string()));
        assert_eq!(params.get("value"), Some(&"a=b".to_string()));
    }

    #[test]
    fn parse_query_params_empty() {
        let params = parse_query_params("");
        assert!(params.is_empty());
    }

    #[test]
    fn random_base64url_length() {
        let s = random_base64url(32);
        // base64url encodes 3 bytes to 4 chars, so 32 bytes = ~43 chars
        assert!(s.len() >= 42);
    }
}
