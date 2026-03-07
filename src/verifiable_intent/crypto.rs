//! SD-JWT / KB-SD-JWT cryptographic primitives.
//!
//! Provides JWS signing/verification (ES256), SD-JWT disclosure hashing,
//! `sd_hash` computation, and selective disclosure resolution.
//!
//! Uses `ring` for ECDSA P-256 (already a dependency) and `sha2`/`base64`
//! for hashing and encoding (also existing dependencies).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use sha2::{Digest, Sha256};

use crate::verifiable_intent::error::{ViError, ViErrorKind};
use crate::verifiable_intent::types::Jwk;

// ── Base64url helpers ────────────────────────────────────────────────

/// Encode bytes as base64url without padding.
pub fn b64u_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

/// Decode base64url without padding.
pub fn b64u_decode(s: &str) -> Result<Vec<u8>, ViError> {
    URL_SAFE_NO_PAD.decode(s).map_err(|e| {
        ViError::new(
            ViErrorKind::InvalidPayload,
            format!("base64url decode: {e}"),
        )
    })
}

// ── Hashing ──────────────────────────────────────────────────────────

/// Compute `B64U(SHA-256(ASCII(input)))` — used for `sd_hash`, `checkout_hash`,
/// `transaction_id`, disclosure hashes, and `conditional_transaction_id`.
pub fn sd_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    b64u_encode(&digest)
}

/// Compute raw SHA-256 hash of a byte slice.
pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

// ── JWS / ES256 signing ─────────────────────────────────────────────

/// Sign a JWS (compact serialization) over the given header and payload JSON.
/// Returns the full `header.payload.signature` string.
pub fn jws_sign(
    header_json: &[u8],
    payload_json: &[u8],
    key_pair: &EcdsaKeyPair,
) -> Result<String, ViError> {
    let header_b64 = b64u_encode(header_json);
    let payload_b64 = b64u_encode(payload_json);
    let signing_input = format!("{header_b64}.{payload_b64}");

    let rng = SystemRandom::new();
    let sig = key_pair.sign(&rng, signing_input.as_bytes()).map_err(|e| {
        ViError::new(
            ViErrorKind::SignatureInvalid,
            format!("signing failed: {e}"),
        )
    })?;

    let sig_b64 = b64u_encode(sig.as_ref());
    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Verify an ES256 JWS compact-serialization string against a public key.
pub fn jws_verify(compact: &str, public_key_bytes: &[u8]) -> Result<(), ViError> {
    let parts: Vec<&str> = compact.splitn(3, '.').collect();
    if parts.len() != 3 {
        return Err(ViError::new(
            ViErrorKind::InvalidHeader,
            "JWS must have 3 dot-separated parts",
        ));
    }

    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let sig_bytes = b64u_decode(parts[2])?;

    let peer_public_key =
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, public_key_bytes);

    peer_public_key
        .verify(signing_input.as_bytes(), &sig_bytes)
        .map_err(|_| {
            ViError::new(
                ViErrorKind::SignatureInvalid,
                "ES256 signature verification failed",
            )
        })
}

/// Decode the payload segment of a JWS compact string (the middle part).
pub fn jws_decode_payload(compact: &str) -> Result<serde_json::Value, ViError> {
    let parts: Vec<&str> = compact.splitn(3, '.').collect();
    if parts.len() < 2 {
        return Err(ViError::new(
            ViErrorKind::InvalidPayload,
            "JWS must have at least 2 dot-separated parts",
        ));
    }
    let bytes = b64u_decode(parts[1])?;
    serde_json::from_slice(&bytes)
        .map_err(|e| ViError::new(ViErrorKind::InvalidPayload, format!("payload JSON: {e}")))
}

/// Decode the header segment of a JWS compact string (the first part).
pub fn jws_decode_header(compact: &str) -> Result<serde_json::Value, ViError> {
    let part = compact
        .split('.')
        .next()
        .ok_or_else(|| ViError::new(ViErrorKind::InvalidHeader, "empty JWS"))?;
    let bytes = b64u_decode(part)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| ViError::new(ViErrorKind::InvalidHeader, format!("header JSON: {e}")))
}

// ── EC P-256 key utilities ──────────────────────────────────────────

/// Generate a fresh EC P-256 key pair.  Returns (pkcs8_document, Jwk_public).
pub fn generate_ec_p256() -> Result<(Vec<u8>, Jwk), ViError> {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(|e| ViError::new(ViErrorKind::KeyUnsupported, format!("keygen: {e}")))?;

    let key_pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|e| ViError::new(ViErrorKind::KeyUnsupported, format!("parse pkcs8: {e}")))?;

    let pub_bytes = key_pair.public_key().as_ref();
    let jwk = ec_public_bytes_to_jwk(pub_bytes)?;

    Ok((pkcs8.as_ref().to_vec(), jwk))
}

/// Load an `EcdsaKeyPair` from PKCS#8 DER bytes.
pub fn load_key_pair(pkcs8_der: &[u8]) -> Result<EcdsaKeyPair, ViError> {
    let rng = SystemRandom::new();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8_der, &rng)
        .map_err(|e| ViError::new(ViErrorKind::KeyUnsupported, format!("load pkcs8: {e}")))
}

/// Convert the raw uncompressed public key bytes (65 bytes: 0x04 || x || y)
/// into a [`Jwk`].
pub fn ec_public_bytes_to_jwk(pub_bytes: &[u8]) -> Result<Jwk, ViError> {
    if pub_bytes.len() != 65 || pub_bytes[0] != 0x04 {
        return Err(ViError::new(
            ViErrorKind::KeyUnsupported,
            "expected 65-byte uncompressed EC point (0x04 || x || y)",
        ));
    }
    Ok(Jwk {
        kty: "EC".into(),
        crv: "P-256".into(),
        x: b64u_encode(&pub_bytes[1..33]),
        y: b64u_encode(&pub_bytes[33..65]),
        d: None,
    })
}

/// Convert a [`Jwk`] (public) back to raw uncompressed bytes (65 bytes).
pub fn jwk_to_public_bytes(jwk: &Jwk) -> Result<Vec<u8>, ViError> {
    if jwk.kty != "EC" || jwk.crv != "P-256" {
        return Err(ViError::new(
            ViErrorKind::KeyUnsupported,
            format!("unsupported key type: {}:{}", jwk.kty, jwk.crv),
        ));
    }
    let x = b64u_decode(&jwk.x)?;
    let y = b64u_decode(&jwk.y)?;
    if x.len() != 32 || y.len() != 32 {
        return Err(ViError::new(
            ViErrorKind::KeyUnsupported,
            "x/y coordinates must be 32 bytes each",
        ));
    }
    let mut bytes = Vec::with_capacity(65);
    bytes.push(0x04);
    bytes.extend_from_slice(&x);
    bytes.extend_from_slice(&y);
    Ok(bytes)
}

// ── SD-JWT disclosure helpers ────────────────────────────────────────

/// Create a single SD-JWT disclosure: `[salt, claim_name, claim_value]`.
/// Returns `(disclosure_b64, disclosure_hash)`.
pub fn create_disclosure(
    claim_name: &str,
    claim_value: &serde_json::Value,
) -> Result<(String, String), ViError> {
    let rng = SystemRandom::new();
    let mut salt_bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&rng, &mut salt_bytes)
        .map_err(|e| ViError::new(ViErrorKind::IssuanceInputInvalid, format!("rng: {e}")))?;
    let salt = b64u_encode(&salt_bytes);

    let disclosure_json = serde_json::json!([salt, claim_name, claim_value]);
    let disclosure_str = serde_json::to_string(&disclosure_json).map_err(|e| {
        ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            format!("disclosure JSON: {e}"),
        )
    })?;
    let disclosure_b64 = b64u_encode(disclosure_str.as_bytes());
    let hash = sd_hash(&disclosure_b64);
    Ok((disclosure_b64, hash))
}

/// Serialize an SD-JWT: `issuer_jwt~disclosure1~disclosure2~...~kb_jwt`
/// (omit `kb_jwt` for L1 which has no key-binding JWT).
pub fn serialize_sd_jwt(issuer_jwt: &str, disclosures: &[String], kb_jwt: Option<&str>) -> String {
    let mut result = issuer_jwt.to_string();
    for d in disclosures {
        result.push('~');
        result.push_str(d);
    }
    result.push('~');
    if let Some(kb) = kb_jwt {
        result.push_str(kb);
    }
    result
}

/// Parse a serialized SD-JWT into (issuer_jwt, disclosures, optional_kb_jwt).
pub fn parse_sd_jwt(serialized: &str) -> Result<(&str, Vec<&str>, Option<&str>), ViError> {
    let parts: Vec<&str> = serialized.split('~').collect();
    if parts.len() < 2 {
        return Err(ViError::new(
            ViErrorKind::InvalidDisclosure,
            "SD-JWT must have at least issuer JWT and trailing ~",
        ));
    }
    let issuer_jwt = parts[0];
    let last = *parts.last().unwrap();
    let kb_jwt = if last.is_empty() { None } else { Some(last) };

    let disclosures = parts[1..parts.len() - 1].to_vec();

    Ok((issuer_jwt, disclosures, kb_jwt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sd_hash_deterministic() {
        let h1 = sd_hash("hello");
        let h2 = sd_hash("hello");
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn b64u_roundtrip() {
        let data = b"test data";
        let encoded = b64u_encode(data);
        let decoded = b64u_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn generate_key_and_convert_roundtrip() {
        let (_pkcs8, jwk) = generate_ec_p256().unwrap();
        assert_eq!(jwk.kty, "EC");
        assert_eq!(jwk.crv, "P-256");
        assert!(jwk.d.is_none());
        let bytes = jwk_to_public_bytes(&jwk).unwrap();
        assert_eq!(bytes.len(), 65);
        assert_eq!(bytes[0], 0x04);
        let jwk2 = ec_public_bytes_to_jwk(&bytes).unwrap();
        assert_eq!(jwk, jwk2);
    }

    #[test]
    fn jws_sign_and_verify() {
        let (pkcs8, jwk) = generate_ec_p256().unwrap();
        let key_pair = load_key_pair(&pkcs8).unwrap();
        let header = serde_json::json!({"alg": "ES256", "typ": "sd+jwt"});
        let payload = serde_json::json!({"sub": "test"});
        let compact = jws_sign(
            header.to_string().as_bytes(),
            payload.to_string().as_bytes(),
            &key_pair,
        )
        .unwrap();

        let pub_bytes = jwk_to_public_bytes(&jwk).unwrap();
        jws_verify(&compact, &pub_bytes).unwrap();
    }

    #[test]
    fn jws_verify_rejects_tampered() {
        let (pkcs8, jwk) = generate_ec_p256().unwrap();
        let key_pair = load_key_pair(&pkcs8).unwrap();
        let header = serde_json::json!({"alg": "ES256"});
        let payload = serde_json::json!({"sub": "test"});
        let mut compact = jws_sign(
            header.to_string().as_bytes(),
            payload.to_string().as_bytes(),
            &key_pair,
        )
        .unwrap();
        // Tamper with payload
        compact = compact.replacen('.', ".AAAA", 1);
        let pub_bytes = jwk_to_public_bytes(&jwk).unwrap();
        assert!(jws_verify(&compact, &pub_bytes).is_err());
    }

    #[test]
    fn disclosure_creation() {
        let (b64, hash) =
            create_disclosure("email", &serde_json::json!("user@example.com")).unwrap();
        assert!(!b64.is_empty());
        assert!(!hash.is_empty());
        // Verify hash matches
        assert_eq!(sd_hash(&b64), hash);
    }

    #[test]
    fn sd_jwt_serialize_parse_roundtrip() {
        let jwt = "eyJhbGciOiJFUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.sig";
        let disclosures = vec!["disc1".to_string(), "disc2".to_string()];
        let serialized = serialize_sd_jwt(jwt, &disclosures, None);
        let (parsed_jwt, parsed_disc, parsed_kb) = parse_sd_jwt(&serialized).unwrap();
        assert_eq!(parsed_jwt, jwt);
        assert_eq!(parsed_disc, vec!["disc1", "disc2"]);
        assert!(parsed_kb.is_none());
    }

    #[test]
    fn sd_jwt_serialize_with_kb_jwt() {
        let jwt = "header.payload.sig";
        let disclosures = vec!["d1".to_string()];
        let serialized = serialize_sd_jwt(jwt, &disclosures, Some("kb.jwt.here"));
        let (parsed_jwt, parsed_disc, parsed_kb) = parse_sd_jwt(&serialized).unwrap();
        assert_eq!(parsed_jwt, jwt);
        assert_eq!(parsed_disc, vec!["d1"]);
        assert_eq!(parsed_kb, Some("kb.jwt.here"));
    }

    #[test]
    fn jws_decode_payload_works() {
        let header = b64u_encode(b"{\"alg\":\"ES256\"}");
        let payload = b64u_encode(b"{\"sub\":\"test\"}");
        let compact = format!("{header}.{payload}.fake-sig");
        let decoded = jws_decode_payload(&compact).unwrap();
        assert_eq!(decoded["sub"], "test");
    }
}
