use anyhow::{Context, Result};
use base64::Engine;
use parking_lot::RwLock;
use ring::signature;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

const JWKS_DEFAULT_TTL: Duration = Duration::from_secs(3600);
const JWKS_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

const ACCOUNT_ID_KEYS: &[&str] = &[
    "account_id",
    "accountId",
    "acct",
    "sub",
    "https://api.openai.com/account_id",
];

#[derive(Debug, Clone)]
pub struct JwkPublicKey {
    pub kid: Option<String>,
    pub alg: JwkAlgorithm,
    pub key_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwkAlgorithm {
    Rs256,
    Es256,
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<JwksKey>,
}

#[derive(Debug, Deserialize)]
struct JwksKey {
    #[serde(default)]
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default)]
    crv: Option<String>,
}

pub struct JwksCache {
    keys: Arc<RwLock<Option<CachedKeys>>>,
    ttl: Duration,
}

struct CachedKeys {
    keys: Vec<JwkPublicKey>,
    fetched_at: Instant,
}

impl JwksCache {
    pub fn new() -> Self {
        Self {
            keys: Arc::new(RwLock::new(None)),
            ttl: JWKS_DEFAULT_TTL,
        }
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            keys: Arc::new(RwLock::new(None)),
            ttl,
        }
    }

    pub async fn get_or_fetch(
        &self,
        client: &reqwest::Client,
        issuer_url: &str,
    ) -> Result<Vec<JwkPublicKey>> {
        {
            let guard = self.keys.read();
            if let Some(cached) = guard.as_ref() {
                if cached.fetched_at.elapsed() < self.ttl {
                    return Ok(cached.keys.clone());
                }
            }
        }

        let keys = fetch_jwks(client, issuer_url).await?;
        let mut guard = self.keys.write();
        *guard = Some(CachedKeys {
            keys: keys.clone(),
            fetched_at: Instant::now(),
        });
        Ok(keys)
    }

    pub fn invalidate(&self) {
        let mut guard = self.keys.write();
        *guard = None;
    }
}

pub async fn fetch_jwks(client: &reqwest::Client, issuer_url: &str) -> Result<Vec<JwkPublicKey>> {
    let base = issuer_url.trim_end_matches('/');
    let url = format!("{base}/.well-known/jwks.json");

    let response = tokio::time::timeout(JWKS_FETCH_TIMEOUT, client.get(&url).send())
        .await
        .context("JWKS fetch timed out")?
        .context("JWKS fetch request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!("JWKS endpoint returned {status}");
    }

    let jwks: JwksResponse = response
        .json()
        .await
        .context("Failed to parse JWKS response")?;

    let mut keys = Vec::new();
    for key in &jwks.keys {
        if let Some(parsed) = parse_jwk(key) {
            keys.push(parsed);
        }
    }

    if keys.is_empty() {
        anyhow::bail!("JWKS response contained no usable keys");
    }

    Ok(keys)
}

fn parse_jwk(key: &JwksKey) -> Option<JwkPublicKey> {
    match key.kty.as_str() {
        "RSA" => parse_rsa_jwk(key),
        "EC" => parse_ec_jwk(key),
        _ => None,
    }
}

fn parse_rsa_jwk(key: &JwksKey) -> Option<JwkPublicKey> {
    let n_b64 = key.n.as_deref()?;
    let e_b64 = key.e.as_deref()?;

    let n_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(n_b64)
        .ok()?;
    let e_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(e_b64)
        .ok()?;

    let der = encode_rsa_public_key_der(&n_bytes, &e_bytes);

    Some(JwkPublicKey {
        kid: key.kid.clone(),
        alg: JwkAlgorithm::Rs256,
        key_bytes: der,
    })
}

fn parse_ec_jwk(key: &JwksKey) -> Option<JwkPublicKey> {
    let crv = key.crv.as_deref()?;
    if crv != "P-256" {
        return None;
    }

    let x_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(key.x.as_deref()?)
        .ok()?;
    let y_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(key.y.as_deref()?)
        .ok()?;

    if x_bytes.len() != 32 || y_bytes.len() != 32 {
        return None;
    }

    let mut uncompressed = Vec::with_capacity(65);
    uncompressed.push(0x04);
    uncompressed.extend_from_slice(&x_bytes);
    uncompressed.extend_from_slice(&y_bytes);

    Some(JwkPublicKey {
        kid: key.kid.clone(),
        alg: JwkAlgorithm::Es256,
        key_bytes: uncompressed,
    })
}

fn encode_rsa_public_key_der(n: &[u8], e: &[u8]) -> Vec<u8> {
    let n_der = encode_der_unsigned_integer(n);
    let e_der = encode_der_unsigned_integer(e);

    let seq_content_len = n_der.len() + e_der.len();
    let mut seq = Vec::new();
    seq.push(0x30);
    encode_der_length(seq_content_len, &mut seq);
    seq.extend_from_slice(&n_der);
    seq.extend_from_slice(&e_der);

    seq
}

fn encode_der_unsigned_integer(bytes: &[u8]) -> Vec<u8> {
    let stripped = strip_leading_zeros(bytes);
    let needs_pad = !stripped.is_empty() && (stripped[0] & 0x80) != 0;
    let content_len = stripped.len() + usize::from(needs_pad);

    let mut out = Vec::new();
    out.push(0x02);
    encode_der_length(content_len, &mut out);
    if needs_pad {
        out.push(0x00);
    }
    out.extend_from_slice(stripped);
    out
}

fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < bytes.len() - 1 && bytes[start] == 0 {
        start += 1;
    }
    &bytes[start..]
}

fn encode_der_length(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        #[allow(clippy::cast_possible_truncation)]
        out.push(len as u8);
    } else if len < 0x100 {
        out.push(0x81);
        #[allow(clippy::cast_possible_truncation)]
        out.push(len as u8);
    } else {
        out.push(0x82);
        #[allow(clippy::cast_possible_truncation)]
        {
            out.push((len >> 8) as u8);
            out.push(len as u8);
        }
    }
}

pub fn verify_and_extract_account_id(token: &str, keys: &[JwkPublicKey]) -> Result<String> {
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() != 3 {
        anyhow::bail!("Malformed JWT: expected 3 dot-separated parts");
    }

    let header_b64 = parts[0];
    let payload_b64 = parts[1];
    let sig_b64 = parts[2];

    let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(header_b64)
        .context("Failed to decode JWT header")?;
    let header: serde_json::Value =
        serde_json::from_slice(&header_bytes).context("Failed to parse JWT header")?;

    let token_kid = header.get("kid").and_then(|v| v.as_str());
    let token_alg = header
        .get("alg")
        .and_then(|v| v.as_str())
        .unwrap_or("RS256");

    let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sig_b64)
        .context("Failed to decode JWT signature")?;

    let message = format!("{header_b64}.{payload_b64}");
    let message_bytes = message.as_bytes();

    let mut verified = false;
    for key in keys {
        if let Some(kid) = token_kid {
            if let Some(key_kid) = &key.kid {
                if kid != key_kid {
                    continue;
                }
            }
        }

        let result = match (token_alg, key.alg) {
            ("RS256", JwkAlgorithm::Rs256) => {
                let public_key = signature::RsaPublicKeyComponents::<Vec<u8>>::from(key);
                public_key.verify(
                    &signature::RSA_PKCS1_2048_8192_SHA256,
                    message_bytes,
                    &sig_bytes,
                )
            }
            ("ES256", JwkAlgorithm::Es256) => {
                let public_key = signature::UnparsedPublicKey::new(
                    &signature::ECDSA_P256_SHA256_FIXED,
                    &key.key_bytes,
                );
                public_key.verify(message_bytes, &sig_bytes)
            }
            _ => continue,
        };

        if result.is_ok() {
            verified = true;
            break;
        }
    }

    if !verified {
        anyhow::bail!("JWT signature verification failed: no matching key");
    }

    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .context("Failed to decode JWT payload")?;
    let claims: serde_json::Value =
        serde_json::from_slice(&payload_bytes).context("Failed to parse JWT claims")?;

    for key in ACCOUNT_ID_KEYS {
        if let Some(value) = claims.get(*key).and_then(|v| v.as_str()) {
            if !value.trim().is_empty() {
                return Ok(value.to_string());
            }
        }
    }

    anyhow::bail!("JWT verified but no account ID found in claims")
}

impl From<&JwkPublicKey> for signature::RsaPublicKeyComponents<Vec<u8>> {
    fn from(key: &JwkPublicKey) -> Self {
        let der = &key.key_bytes;
        let (n, e) = decode_rsa_public_key_der(der);
        signature::RsaPublicKeyComponents { n, e }
    }
}

fn decode_rsa_public_key_der(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut pos = 0;

    if der[pos] != 0x30 {
        return (Vec::new(), Vec::new());
    }
    pos += 1;
    let (_, len_size) = read_der_length(&der[pos..]);
    pos += len_size;

    if der[pos] != 0x02 {
        return (Vec::new(), Vec::new());
    }
    pos += 1;
    let (n_len, n_len_size) = read_der_length(&der[pos..]);
    pos += n_len_size;
    let n = der[pos..pos + n_len].to_vec();
    pos += n_len;

    if der[pos] != 0x02 {
        return (Vec::new(), Vec::new());
    }
    pos += 1;
    let (e_len, e_len_size) = read_der_length(&der[pos..]);
    pos += e_len_size;
    let e = der[pos..pos + e_len].to_vec();

    (n, e)
}

fn read_der_length(bytes: &[u8]) -> (usize, usize) {
    if bytes[0] < 0x80 {
        (bytes[0] as usize, 1)
    } else if bytes[0] == 0x81 {
        (bytes[1] as usize, 2)
    } else {
        let len = ((bytes[1] as usize) << 8) | (bytes[2] as usize);
        (len, 3)
    }
}

pub async fn extract_account_id_verified(
    client: &reqwest::Client,
    token: &str,
    issuer: &str,
) -> Result<String> {
    let keys = fetch_jwks(client, issuer).await?;
    verify_and_extract_account_id(token, &keys)
}

pub fn extract_account_id_with_fallback(
    token: &str,
    keys: Option<&[JwkPublicKey]>,
) -> Option<String> {
    if let Some(keys) = keys {
        match verify_and_extract_account_id(token, keys) {
            Ok(account_id) => return Some(account_id),
            Err(err) => {
                warn!(
                    "JWT signature verification failed, falling back to unverified decode: {err}"
                );
            }
        }
    }

    super::openai_oauth::extract_account_id_from_jwt(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    fn make_ec_test_key() -> (EcdsaKeyPair, JwkPublicKey) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();

        let pub_bytes = key_pair.public_key().as_ref().to_vec();

        let jwk = JwkPublicKey {
            kid: Some("test-key-1".to_string()),
            alg: JwkAlgorithm::Es256,
            key_bytes: pub_bytes,
        };

        (key_pair, jwk)
    }

    fn sign_jwt(key_pair: &EcdsaKeyPair, header: &str, payload: &str) -> String {
        let rng = SystemRandom::new();
        let header_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header);
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        let message = format!("{header_b64}.{payload_b64}");
        let sig = key_pair.sign(&rng, message.as_bytes()).unwrap();
        let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.as_ref());
        format!("{message}.{sig_b64}")
    }

    #[test]
    fn verify_valid_jwt_extracts_account_id() {
        let (key_pair, jwk) = make_ec_test_key();
        let header = r#"{"alg":"ES256","typ":"JWT","kid":"test-key-1"}"#;
        let payload = r#"{"account_id":"acct_test_456","sub":"user_123"}"#;
        let token = sign_jwt(&key_pair, header, payload);

        let result = verify_and_extract_account_id(&token, &[jwk]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "acct_test_456");
    }

    #[test]
    fn verify_tampered_jwt_is_rejected() {
        let (key_pair, jwk) = make_ec_test_key();
        let header = r#"{"alg":"ES256","typ":"JWT","kid":"test-key-1"}"#;
        let payload = r#"{"account_id":"acct_test_456"}"#;
        let token = sign_jwt(&key_pair, header, payload);

        let tampered_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"account_id":"acct_EVIL"}"#);
        let parts: Vec<&str> = token.splitn(3, '.').collect();
        let tampered = format!("{}.{}.{}", parts[0], tampered_payload, parts[2]);

        let result = verify_and_extract_account_id(&tampered, &[jwk]);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("signature verification failed"));
    }

    #[test]
    fn verify_wrong_kid_is_rejected() {
        let (key_pair, mut jwk) = make_ec_test_key();
        jwk.kid = Some("other-key".to_string());
        let header = r#"{"alg":"ES256","typ":"JWT","kid":"test-key-1"}"#;
        let payload = r#"{"account_id":"acct_test_789"}"#;
        let token = sign_jwt(&key_pair, header, payload);

        let result = verify_and_extract_account_id(&token, &[jwk]);
        assert!(result.is_err());
    }

    #[test]
    fn fallback_works_when_no_keys_provided() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("{}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"account_id":"acct_fallback"}"#);
        let token = format!("{header}.{payload}.fake_sig");

        let result = extract_account_id_with_fallback(&token, None);
        assert_eq!(result.as_deref(), Some("acct_fallback"));
    }

    #[test]
    fn fallback_works_when_verification_fails() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("{}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"account_id":"acct_fallback_2"}"#);
        let token = format!("{header}.{payload}.fake_sig");

        let (_, jwk) = make_ec_test_key();
        let result = extract_account_id_with_fallback(&token, Some(&[jwk]));
        assert_eq!(result.as_deref(), Some("acct_fallback_2"));
    }

    #[test]
    fn malformed_jwt_rejected() {
        let result = verify_and_extract_account_id("not.a.valid.jwt", &[]);
        assert!(result.is_err());

        let result2 = verify_and_extract_account_id("nope", &[]);
        assert!(result2.is_err());
    }

    #[test]
    fn jwks_cache_starts_empty() {
        let cache = JwksCache::new();
        let guard = cache.keys.read();
        assert!(guard.is_none());
    }

    #[test]
    fn parse_rsa_jwk_roundtrip() {
        let n = vec![0x00, 0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67];
        let e = vec![0x01, 0x00, 0x01];

        let der = encode_rsa_public_key_der(&n, &e);
        let (decoded_n, decoded_e) = decode_rsa_public_key_der(&der);

        let n_stripped = strip_leading_zeros(&n);
        assert_eq!(strip_leading_zeros(&decoded_n), n_stripped);
        assert_eq!(decoded_e, e);
    }

    #[test]
    fn parse_ec_jwk_valid() {
        let x_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0x01u8; 32]);
        let y_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0x02u8; 32]);

        let key = JwksKey {
            kty: "EC".to_string(),
            kid: Some("ec-1".to_string()),
            alg: Some("ES256".to_string()),
            n: None,
            e: None,
            x: Some(x_b64),
            y: Some(y_b64),
            crv: Some("P-256".to_string()),
        };

        let parsed = parse_ec_jwk(&key);
        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.alg, JwkAlgorithm::Es256);
        assert_eq!(parsed.key_bytes.len(), 65);
        assert_eq!(parsed.key_bytes[0], 0x04);
    }

    #[test]
    fn parse_ec_jwk_wrong_curve_rejected() {
        let key = JwksKey {
            kty: "EC".to_string(),
            kid: None,
            alg: Some("ES384".to_string()),
            n: None,
            e: None,
            x: Some("AAAA".to_string()),
            y: Some("BBBB".to_string()),
            crv: Some("P-384".to_string()),
        };

        assert!(parse_ec_jwk(&key).is_none());
    }
}
