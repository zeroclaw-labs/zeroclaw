//! SPKI export and fingerprint computation for Ed25519.
//!
//! The fingerprint is the cross-system contract with WardToken. Both ends must
//! compute byte-identical strings for the same public key, or `verify` returns
//! a flat 403 (and we can't tell format-mismatch from no-grant by design).
//!
//! Format per WardToken `wardtoken-core/src/agents.rs`:
//!   `"sha256:" + base64url-no-pad(SHA256(spki_der))`
//!
//! The SPKI prefix is fixed and universal per RFC 8410 §3.3:
//!   `30 2a 30 05 06 03 2b 65 70 03 21 00` ‖ 32-byte public key = 44 bytes total.

use ring::digest::{digest, SHA256};

/// Fixed Ed25519 SPKI prefix per RFC 8410 §3.3 (12 bytes).
///
/// This prefix has been stable since 2018 and is universal — any Ed25519
/// public key wrapped in SubjectPublicKeyInfo uses exactly these 12 bytes
/// before the 32-byte key. Hand-rolled as a const: the bytes are public,
/// audited, and unchanged.
pub const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Wrap a 32-byte Ed25519 public key in the standard SPKI DER envelope.
///
/// Result is always 44 bytes (12-byte prefix + 32-byte key). The reverse
/// operation — extracting the raw 32-byte key from an SPKI DER — is just
/// `spki_der[12..]`, since the prefix is fixed-length.
#[inline]
pub fn spki_from_pubkey(pubkey: &[u8; 32]) -> [u8; 44] {
    let mut out = [0u8; 44];
    out[..12].copy_from_slice(&ED25519_SPKI_PREFIX);
    out[12..].copy_from_slice(pubkey);
    out
}

/// Extract the raw 32-byte Ed25519 public key from an SPKI DER blob.
///
/// Returns `None` if the prefix doesn't match. Use this to confirm an
/// SPKI blob came from our format (defense against arbitrary DER input).
#[inline]
pub fn pubkey_from_spki(spki: &[u8]) -> Option<[u8; 32]> {
    if spki.len() != 44 || spki[..12] != ED25519_SPKI_PREFIX {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&spki[12..]);
    Some(out)
}

/// Compute the WardToken `fingerprint_der` string for an SPKI DER blob.
///
/// `"sha256:" + base64url-no-pad(SHA256(spki_der))`
pub fn fingerprint_spki(spki_der: &[u8]) -> String {
    let hash = digest(&SHA256, spki_der);
    let b64 = base64_url_no_pad(hash.as_ref());
    format!("sha256:{b64}")
}

/// Compute the WardToken `fingerprint_der` string directly from a 32-byte
/// Ed25519 public key. Convenience wrapper around [`spki_from_pubkey`] +
/// [`fingerprint_spki`].
pub fn fingerprint_pubkey(pubkey: &[u8; 32]) -> String {
    let spki = spki_from_pubkey(pubkey);
    fingerprint_spki(&spki)
}

/// Strip a SPKI PEM envelope back to SPKI DER bytes.
///
/// Mirror of the private `pem_to_der` used in `local` for PKCS#8, but
/// pinned to the `BEGIN PUBLIC KEY` label so a PKCS#8 PEM can't sneak
/// through and cause a quiet shape mismatch (44 vs 83 bytes).
pub fn spki_pem_to_der(pem: &[u8]) -> Result<Vec<u8>, String> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let s = std::str::from_utf8(pem).map_err(|e| format!("spki pem utf-8: {e}"))?;
    if !s.contains("BEGIN PUBLIC KEY") {
        return Err("spki pem is missing 'BEGIN PUBLIC KEY' label".into());
    }
    let mut b64 = String::new();
    for line in s.lines() {
        if line.starts_with("-----") {
            continue;
        }
        b64.push_str(line.trim());
    }
    STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("spki pem base64 decode: {e}"))
}

/// Encode bytes as base64url with no padding.
///
/// This is the same encoding as `base64::engine::general_purpose::URL_SAFE_NO_PAD`
/// but inlined to keep the cryptographic primitive path dep-free and to make
/// the algorithm visible at the call site. The encoding is RFC 4648 §5
/// with the URL-safe alphabet and padding stripped.
fn base64_url_no_pad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut chunks = data.chunks_exact(3);
    for chunk in &mut chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        }
        _ => {}
    }
    out
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    /// A canonical 32-byte test pubkey — all zeros except the last byte
    /// to avoid any accidental "all zero key" special-casing downstream.
    const TEST_PUBKEY: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];

    /// The exact 44-byte SPKI output for the test pubkey. This is the
    /// known-vector test that pins the prefix against the RFC.
    const EXPECTED_SPKI: [u8; 44] = [
        // RFC 8410 §3.3 prefix
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00, // ‖
        // raw 32-byte pubkey
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];

    /// The exact fingerprint string for the test pubkey. Computed once by
    /// the reference Python below; pinned here so any change to SPKI
    /// prefix, hashing, or base64 alphabet fails this test before it
    /// fails a live integration.
    ///
    /// Reference:
    ///   import base64, hashlib
    ///   spki = bytes.fromhex("302a300506032b6570032100") + bytes(range(32))
    ///   h = hashlib.sha256(spki).digest()
    ///   "sha256:" + base64.urlsafe_b64encode(h).rstrip(b"=").decode()
    ///
    /// Result: `sha256:lAhFeu_Qcc7BJ8H5hTmTCGGtG6lMlA25dclywJ_Gi2g`
    const EXPECTED_FINGERPRINT: &str = "sha256:lAhFeu_Qcc7BJ8H5hTmTCGGtG6lMlA25dclywJ_Gi2g";

    #[test]
    fn spki_from_pubkey_matches_rfc8410_test_vector() {
        // Format test: known 32-byte input → exact 44-byte output, byte-for-byte.
        let got = spki_from_pubkey(&TEST_PUBKEY);
        assert_eq!(got, EXPECTED_SPKI);
        assert_eq!(got.len(), 44);
        assert_eq!(&got[..12], &ED25519_SPKI_PREFIX[..]);
    }

    #[test]
    fn pubkey_from_spki_roundtrips() {
        // Round-trip: pubkey → spki → pubkey. Bytes match.
        let spki = spki_from_pubkey(&TEST_PUBKEY);
        let recovered = pubkey_from_spki(&spki).expect("spki must be valid");
        assert_eq!(recovered, TEST_PUBKEY);
    }

    #[test]
    fn pubkey_from_spki_rejects_bad_input() {
        // Wrong length → None.
        assert!(pubkey_from_spki(&[0u8; 43]).is_none());
        assert!(pubkey_from_spki(&[0u8; 45]).is_none());
        assert!(pubkey_from_spki(&[]).is_none());

        // Right length, wrong prefix → None.
        let mut bad = EXPECTED_SPKI;
        bad[0] = 0x31; // first byte off by one
        assert!(pubkey_from_spki(&bad).is_none());
    }

    #[test]
    fn fingerprint_matches_known_vector() {
        // Cross-system contract: if this string changes, the integration
        // with WardToken breaks. Pinned to the Python reference output
        // above; do not adjust without aligning with WardToken's
        // `fingerprint_der` (wardtoken-core/src/agents.rs).
        let got = fingerprint_pubkey(&TEST_PUBKEY);
        assert_eq!(got, EXPECTED_FINGERPRINT);
    }

    #[test]
    fn fingerprint_format_uses_sha256_prefix_and_urlsafe_nopad() {
        // Independent check of the format pieces, separate from the
        // exact-string test above. If someone changes either piece, the
        // exact-string test breaks first, but this test names the bug.
        let got = fingerprint_pubkey(&TEST_PUBKEY);
        assert!(got.starts_with("sha256:"), "missing sha256: prefix: {got}");

        // After the prefix, the body should be base64url-no-pad of the
        // 32-byte SHA-256 digest. Base64url-no-pad of 32 bytes = 43 chars
        // (no padding). Use the standard `base64` crate to confirm.
        let body = &got["sha256:".len()..];
        assert_eq!(body.len(), 43, "base64url-no-pad of 32 bytes is 43 chars");
        assert!(!body.contains('+'), "must be url-safe, not std base64: {body}");
        assert!(!body.contains('='), "must be unpadded: {body}");

        // Round-trip through the reference encoder — bytes must match.
        let spki = spki_from_pubkey(&TEST_PUBKEY);
        let hash = digest(&SHA256, &spki);
        let expected_body = URL_SAFE_NO_PAD.encode(hash.as_ref());
        assert_eq!(body, expected_body, "base64 encoding must match reference");
    }

    #[test]
    fn fingerprint_is_deterministic() {
        // Same input → same output, always.
        let a = fingerprint_pubkey(&TEST_PUBKEY);
        let b = fingerprint_pubkey(&TEST_PUBKEY);
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_differs_per_pubkey() {
        // Different pubkeys → different fingerprints. One-bit diff is
        // sufficient — SHA-256 is a one-way function, not strictly
        // required, but this is the smoke check.
        let mut other = TEST_PUBKEY;
        other[0] ^= 0x01;
        let a = fingerprint_pubkey(&TEST_PUBKEY);
        let b = fingerprint_pubkey(&other);
        assert_ne!(a, b);
    }
}
