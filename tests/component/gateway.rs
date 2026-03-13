//! Gateway component tests.
//!
//! Tests public gateway infrastructure (rate limiter, idempotency, signature
//! verification) in isolation. The gateway module (`lightwave_sys::gateway`) exposes
//! `verify_whatsapp_signature` and the server function `run_gateway`, but the
//! internal rate limiter and idempotency store constructors are crate-private.
//! Tests here verify behavior through the public API surface.

use lightwave_sys::gateway::verify_whatsapp_signature;

// ═════════════════════════════════════════════════════════════════════════════
// WhatsApp webhook signature verification (public API)
// ═════════════════════════════════════════════════════════════════════════════

/// Valid HMAC-SHA256 signature is accepted.
#[test]
fn gateway_whatsapp_valid_signature_accepted() {
    let secret = "test_app_secret";
    let body = b"test body content";

    // Compute expected signature
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let result = mac.finalize();
    let signature = hex::encode(result.into_bytes());
    let header = format!("sha256={signature}");

    assert!(
        verify_whatsapp_signature(secret, body, &header),
        "Valid signature should be accepted"
    );
}

/// Wrong signature is rejected.
#[test]
fn gateway_whatsapp_wrong_signature_rejected() {
    let secret = "test_app_secret";
    let body = b"test body content";
    let header = "sha256=0000000000000000000000000000000000000000000000000000000000000000";

    assert!(
        !verify_whatsapp_signature(secret, body, header),
        "Wrong signature should be rejected"
    );
}

/// Missing sha256= prefix is rejected.
#[test]
fn gateway_whatsapp_missing_prefix_rejected() {
    let secret = "test_app_secret";
    let body = b"test body content";
    let header = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

    assert!(
        !verify_whatsapp_signature(secret, body, header),
        "Missing sha256= prefix should be rejected"
    );
}

/// Empty signature is rejected.
#[test]
fn gateway_whatsapp_empty_signature_rejected() {
    let secret = "test_app_secret";
    let body = b"test body content";

    assert!(
        !verify_whatsapp_signature(secret, body, ""),
        "Empty signature should be rejected"
    );
}

/// Tampered body is rejected (signature computed for different body).
#[test]
fn gateway_whatsapp_tampered_body_rejected() {
    let secret = "test_app_secret";
    let original_body = b"original body";
    let tampered_body = b"tampered body";

    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(original_body);
    let result = mac.finalize();
    let signature = hex::encode(result.into_bytes());
    let header = format!("sha256={signature}");

    assert!(
        !verify_whatsapp_signature(secret, tampered_body, &header),
        "Tampered body should be rejected"
    );
}

/// Different secrets produce different signatures.
#[test]
fn gateway_whatsapp_different_secrets_differ() {
    let body = b"same body";

    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let mut mac1 = HmacSha256::new_from_slice(b"secret_one").unwrap();
    mac1.update(body);
    let sig1 = hex::encode(mac1.finalize().into_bytes());

    let mut mac2 = HmacSha256::new_from_slice(b"secret_two").unwrap();
    mac2.update(body);
    let sig2 = hex::encode(mac2.finalize().into_bytes());

    assert_ne!(
        sig1, sig2,
        "Different secrets should produce different signatures"
    );

    let header1 = format!("sha256={sig1}");
    assert!(verify_whatsapp_signature("secret_one", body, &header1));
    assert!(!verify_whatsapp_signature("secret_two", body, &header1));
}

// ═════════════════════════════════════════════════════════════════════════════
// Gateway constants and configuration validation
// ═════════════════════════════════════════════════════════════════════════════

/// Gateway body limit constant is reasonable.
#[test]
fn gateway_body_limit_is_reasonable() {
    assert_eq!(
        lightwave_sys::gateway::MAX_BODY_SIZE,
        65_536,
        "Max body size should be 64KB"
    );
}

/// Gateway timeout constant is reasonable.
#[test]
fn gateway_timeout_is_reasonable() {
    assert_eq!(
        lightwave_sys::gateway::REQUEST_TIMEOUT_SECS,
        30,
        "Request timeout should be 30 seconds"
    );
}

/// Gateway rate limit window is 60 seconds.
#[test]
fn gateway_rate_limit_window_is_60s() {
    assert_eq!(
        lightwave_sys::gateway::RATE_LIMIT_WINDOW_SECS,
        60,
        "Rate limit window should be 60 seconds"
    );
}
