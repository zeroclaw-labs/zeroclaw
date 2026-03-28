//! Integration tests for WhatsApp webhook signature verification.
//!
//! These tests validate that:
//! 1. Webhooks with valid signatures are accepted
//! 2. Webhooks with invalid signatures are rejected
//! 3. Webhooks with missing signatures are rejected
//! 4. Webhooks are rejected even if JSON is valid but signature is bad

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Compute valid HMAC-SHA256 signature for a webhook payload
fn compute_signature(app_secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()).unwrap();
    mac.update(body);
    let result = mac.finalize();
    format!("sha256={}", hex::encode(result.into_bytes()))
}

#[test]
fn whatsapp_signature_rejects_missing_sha256_prefix() {
    let secret = "test_app_secret";
    let body = b"test payload";
    let bad_sig = "abc123"; // Missing sha256= prefix

    assert!(!zeroclaw::gateway::verify_whatsapp_signature(
        secret, body, bad_sig
    ));
}

#[test]
fn whatsapp_signature_rejects_invalid_hex() {
    let secret = "test_app_secret";
    let body = b"test payload";
    let bad_sig = "sha256=not-valid-hex!!";

    assert!(!zeroclaw::gateway::verify_whatsapp_signature(
        secret, body, bad_sig
    ));
}

#[test]
fn whatsapp_signature_rejects_wrong_signature() {
    let secret = "test_app_secret";
    let body = b"test payload";
    let bad_sig = "sha256=00112233445566778899aabbccddeeff";

    assert!(!zeroclaw::gateway::verify_whatsapp_signature(
        secret, body, bad_sig
    ));
}

#[test]
fn whatsapp_signature_accepts_valid_signature() {
    let secret = "test_app_secret";
    let body = b"test payload";
    let valid_sig = compute_signature(secret, body);

    assert!(zeroclaw::gateway::verify_whatsapp_signature(
        secret, body, &valid_sig
    ));
}

#[test]
fn whatsapp_signature_rejects_tampered_body() {
    let secret = "test_app_secret";
    let original_body = b"original message";
    let tampered_body = b"tampered message";

    // Compute signature for original body
    let sig = compute_signature(secret, original_body);

    // Tampered body should be rejected even with valid-looking signature
    assert!(!zeroclaw::gateway::verify_whatsapp_signature(
        secret,
        tampered_body,
        &sig
    ));
}

#[test]
fn whatsapp_signature_rejects_wrong_secret() {
    let correct_secret = "correct_secret";
    let wrong_secret = "wrong_secret";
    let body = b"test payload";

    // Compute signature with correct secret
    let sig = compute_signature(correct_secret, body);

    // Wrong secret should reject the signature
    assert!(!zeroclaw::gateway::verify_whatsapp_signature(
        wrong_secret,
        body,
        &sig
    ));
}

#[test]
fn whatsapp_signature_rejects_empty_signature() {
    let secret = "test_app_secret";
    let body = b"test payload";

    assert!(!zeroclaw::gateway::verify_whatsapp_signature(
        secret, body, ""
    ));
}

#[test]
fn whatsapp_signature_different_secrets_produce_different_sigs() {
    let secret1 = "secret_one";
    let secret2 = "secret_two";
    let body = b"same payload";

    let sig1 = compute_signature(secret1, body);
    let sig2 = compute_signature(secret2, body);

    // Different secrets should produce different signatures
    assert_ne!(sig1, sig2);

    // Each signature should only verify with its own secret
    assert!(zeroclaw::gateway::verify_whatsapp_signature(
        secret1, body, &sig1
    ));
    assert!(!zeroclaw::gateway::verify_whatsapp_signature(
        secret2, body, &sig1
    ));
    assert!(zeroclaw::gateway::verify_whatsapp_signature(
        secret2, body, &sig2
    ));
    assert!(!zeroclaw::gateway::verify_whatsapp_signature(
        secret1, body, &sig2
    ));
}
