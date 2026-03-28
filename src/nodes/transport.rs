//! Corporate-friendly secure node transport using standard HTTPS + HMAC-SHA256 authentication.
//!
//! All inter-node traffic uses plain HTTPS on port 443 — no exotic protocols,
//! no custom binary framing, no UDP tunneling.  This makes the transport
//! compatible with corporate proxies, firewalls, and IT audit expectations.

use anyhow::{Result, bail};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Signs a request payload with HMAC-SHA256.
///
/// Uses `timestamp` + `nonce` alongside the payload to prevent replay attacks.
pub fn sign_request(
    shared_secret: &str,
    payload: &[u8],
    timestamp: i64,
    nonce: &str,
) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(shared_secret.as_bytes())
        .map_err(|e| anyhow::anyhow!("HMAC key error: {e}"))?;
    mac.update(&timestamp.to_le_bytes());
    mac.update(nonce.as_bytes());
    mac.update(payload);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Verify a signed request, rejecting stale timestamps for replay protection.
pub fn verify_request(
    shared_secret: &str,
    payload: &[u8],
    timestamp: i64,
    nonce: &str,
    signature: &str,
    max_age_secs: i64,
) -> Result<bool> {
    let now = Utc::now().timestamp();
    if (now - timestamp).abs() > max_age_secs {
        bail!("Request timestamp too old or too far in future");
    }

    let expected = sign_request(shared_secret, payload, timestamp, nonce)?;
    Ok(constant_time_eq(expected.as_bytes(), signature.as_bytes()))
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

// ── Node transport client ───────────────────────────────────────

/// Sends authenticated HTTPS requests to peer nodes.
///
/// Every outgoing request carries three custom headers:
/// - `X-ZeroClaw-Timestamp` — unix epoch seconds
/// - `X-ZeroClaw-Nonce` — random UUID v4
/// - `X-ZeroClaw-Signature` — HMAC-SHA256 hex digest
///
/// Incoming requests are verified with the same scheme via [`Self::verify_incoming`].
pub struct NodeTransport {
    http: reqwest::Client,
    shared_secret: String,
    max_request_age_secs: i64,
}

impl NodeTransport {
    pub fn new(shared_secret: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("HTTP client build"),
            shared_secret,
            max_request_age_secs: 300, // 5 min replay window
        }
    }

    /// Send an authenticated request to a peer node.
    pub async fn send(
        &self,
        node_address: &str,
        endpoint: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let body = serde_json::to_vec(&payload)?;
        let timestamp = Utc::now().timestamp();
        let nonce = uuid::Uuid::new_v4().to_string();
        let signature = sign_request(&self.shared_secret, &body, timestamp, &nonce)?;

        let url = format!("https://{node_address}/api/node-control/{endpoint}");
        let resp = self
            .http
            .post(&url)
            .header("X-ZeroClaw-Timestamp", timestamp.to_string())
            .header("X-ZeroClaw-Nonce", &nonce)
            .header("X-ZeroClaw-Signature", &signature)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            bail!(
                "Node request failed: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }

        Ok(resp.json().await?)
    }

    /// Verify an incoming request from a peer node.
    pub fn verify_incoming(
        &self,
        payload: &[u8],
        timestamp_header: &str,
        nonce_header: &str,
        signature_header: &str,
    ) -> Result<bool> {
        let timestamp: i64 = timestamp_header
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid timestamp header"))?;
        verify_request(
            &self.shared_secret,
            payload,
            timestamp,
            nonce_header,
            signature_header,
            self.max_request_age_secs,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "test-shared-secret-key";

    #[test]
    fn sign_request_deterministic() {
        let sig1 = sign_request(TEST_SECRET, b"hello", 1_700_000_000, "nonce-1").unwrap();
        let sig2 = sign_request(TEST_SECRET, b"hello", 1_700_000_000, "nonce-1").unwrap();
        assert_eq!(sig1, sig2, "Same inputs must produce the same signature");
    }

    #[test]
    fn verify_request_accepts_valid_signature() {
        let now = Utc::now().timestamp();
        let sig = sign_request(TEST_SECRET, b"payload", now, "nonce-a").unwrap();
        let ok = verify_request(TEST_SECRET, b"payload", now, "nonce-a", &sig, 300).unwrap();
        assert!(ok, "Valid signature must pass verification");
    }

    #[test]
    fn verify_request_rejects_tampered_payload() {
        let now = Utc::now().timestamp();
        let sig = sign_request(TEST_SECRET, b"original", now, "nonce-b").unwrap();
        let ok = verify_request(TEST_SECRET, b"tampered", now, "nonce-b", &sig, 300).unwrap();
        assert!(!ok, "Tampered payload must fail verification");
    }

    #[test]
    fn verify_request_rejects_expired_timestamp() {
        let old = Utc::now().timestamp() - 600;
        let sig = sign_request(TEST_SECRET, b"data", old, "nonce-c").unwrap();
        let result = verify_request(TEST_SECRET, b"data", old, "nonce-c", &sig, 300);
        assert!(result.is_err(), "Expired timestamp must be rejected");
    }

    #[test]
    fn verify_request_rejects_wrong_secret() {
        let now = Utc::now().timestamp();
        let sig = sign_request(TEST_SECRET, b"data", now, "nonce-d").unwrap();
        let ok = verify_request("wrong-secret", b"data", now, "nonce-d", &sig, 300).unwrap();
        assert!(!ok, "Wrong secret must fail verification");
    }

    #[test]
    fn constant_time_eq_correctness() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn node_transport_construction() {
        let transport = NodeTransport::new("secret-key".into());
        assert_eq!(transport.max_request_age_secs, 300);
    }

    #[test]
    fn node_transport_verify_incoming_valid() {
        let transport = NodeTransport::new(TEST_SECRET.into());
        let now = Utc::now().timestamp();
        let payload = b"test-body";
        let nonce = "incoming-nonce";
        let sig = sign_request(TEST_SECRET, payload, now, nonce).unwrap();

        let ok = transport
            .verify_incoming(payload, &now.to_string(), nonce, &sig)
            .unwrap();
        assert!(ok, "Valid incoming request must pass verification");
    }

    #[test]
    fn node_transport_verify_incoming_bad_timestamp_header() {
        let transport = NodeTransport::new(TEST_SECRET.into());
        let result = transport.verify_incoming(b"body", "not-a-number", "nonce", "sig");
        assert!(result.is_err(), "Non-numeric timestamp header must error");
    }

    #[test]
    fn sign_request_different_nonce_different_signature() {
        let sig1 = sign_request(TEST_SECRET, b"data", 1_700_000_000, "nonce-1").unwrap();
        let sig2 = sign_request(TEST_SECRET, b"data", 1_700_000_000, "nonce-2").unwrap();
        assert_ne!(
            sig1, sig2,
            "Different nonces must produce different signatures"
        );
    }
}
