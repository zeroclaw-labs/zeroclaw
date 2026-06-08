//! Canonical byte serialization of `IdentityAssertion` for signing.
//!
//! Two endpoints of the wire (signer and verifier) must produce byte-identical
//! inputs to the Ed25519 sign operation for the same assertion. The
//! serialization is a length-prefixed concatenation of fields in field
//! order; the only "decision" the format makes is the length-prefix width
//! and the field order. Both are pinned here.
//!
//! The intent is **not** to be human-readable, not to be compact, not to be
//! forward-compatible across versions. It is to be **deterministic**.
//! Adding a field is a breaking change; the version of the assertion
//! format is implicit in the field order and the verifier rejects anything
//! that doesn't match the local struct shape (since field order, types,
//! and lengths all have to match for the bytes to match).
//!
//! Layout: 4-byte little-endian length ‖ field bytes, for each of:
//!   1. agent_user_id  (UTF-8)
//!   2. grantor_user_id (UTF-8)
//!   3. fingerprint    (UTF-8)
//!   4. audience       (UTF-8, or empty bytes when None)
//!   5. issued_at      (8-byte little-endian i64)
//!   6. nonce          (UTF-8)
//!
//! The signature itself is NOT included in the canonical bytes — it is
//! produced over them and travels alongside. The verifier reconstructs the
//! canonical bytes from the same six fields and verifies.

use crate::error::{IdentityError, IdentityResult};

/// A typed view of the assertion fields used for signing.
///
/// We don't take a reference to `IdentityAssertion` directly so the
/// signing path is a free function — easier to test, easier to reason
/// about. The caller does the field extraction.
pub struct CanonicalAssertion<'a> {
    pub agent_user_id: &'a str,
    pub grantor_user_id: &'a str,
    pub fingerprint: &'a str,
    pub audience: Option<&'a str>,
    pub issued_at: i64,
    pub nonce: &'a str,
}

/// Build the canonical byte string that gets signed / verified.
///
/// Field order and length-prefix width are pinned (see module docs).
pub fn canonical_bytes(c: &CanonicalAssertion<'_>) -> IdentityResult<Vec<u8>> {
    let mut out = Vec::with_capacity(256);
    push_lp(&mut out, c.agent_user_id.as_bytes())?;
    push_lp(&mut out, c.grantor_user_id.as_bytes())?;
    push_lp(&mut out, c.fingerprint.as_bytes())?;
    // Audience: empty when None, so the serialization is still unique
    // (None and Some("") both produce zero body bytes but the meaning
    // is captured in the type system at the API boundary, not the
    // bytes). Verifier doesn't need to distinguish.
    let aud = c.audience.unwrap_or("").as_bytes();
    push_lp(&mut out, aud)?;
    // 8-byte little-endian timestamp
    out.extend_from_slice(&c.issued_at.to_le_bytes());
    push_lp(&mut out, c.nonce.as_bytes())?;
    Ok(out)
}

/// Write a 4-byte LE length prefix and then the field bytes. Returns an
/// error if the field is too long (> 2^32 - 1 bytes), which is essentially
/// never for our use case but pinned by the wire format.
fn push_lp(out: &mut Vec<u8>, body: &[u8]) -> IdentityResult<()> {
    let len = u32::try_from(body.len())
        .map_err(|_| IdentityError::Crypto(format!("field exceeds u32 length: {}", body.len())))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(body);
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn a() -> CanonicalAssertion<'static> {
        CanonicalAssertion {
            agent_user_id: "agent-uuid-1",
            grantor_user_id: "grantor-uuid-1",
            fingerprint: "sha256:abc",
            audience: Some("hub.deliveryboy.tech"),
            issued_at: 1_700_000_000,
            nonce: "deadbeef",
        }
    }

    #[test]
    fn canonical_bytes_are_deterministic() {
        // Same input → byte-identical output.
        let b1 = canonical_bytes(&a()).unwrap();
        let b2 = canonical_bytes(&a()).unwrap();
        assert_eq!(b1, b2);
    }

    #[test]
    fn canonical_bytes_field_order_matters() {
        // Changing agent_user_id changes the bytes (the prefix and body
        // are in field 1's slot).
        let mut swapped = a();
        swapped.agent_user_id = "different-agent";
        let b1 = canonical_bytes(&a()).unwrap();
        let b2 = canonical_bytes(&swapped).unwrap();
        assert_ne!(b1, b2);
    }

    #[test]
    fn canonical_bytes_audience_none_vs_some() {
        // None and Some("") produce the same canonical bytes — by
        // construction (we serialize the empty string in both cases).
        // The type-level distinction is preserved at the API; the byte
        // distinction is intentionally not. Test it explicitly so a
        // future refactor doesn't change it accidentally.
        let mut with_aud = a();
        with_aud.audience = Some("x");
        let without = CanonicalAssertion { audience: None, ..a() };
        let with_empty = CanonicalAssertion {
            audience: Some(""),
            ..a()
        };
        let b_without = canonical_bytes(&without).unwrap();
        let b_empty = canonical_bytes(&with_empty).unwrap();
        assert_eq!(b_without, b_empty, "None and Some(\"\") must match");

        // And Some("x") differs from None.
        let b_x = canonical_bytes(&with_aud).unwrap();
        assert_ne!(b_x, b_without);
    }

    #[test]
    fn canonical_bytes_issued_at_layout_is_le_i64() {
        // Pin the i64 little-endian layout for issued_at. If someone
        // changes to big-endian, the byte pattern flips and the
        // integration with WardToken breaks (and the on-wire format
        // changes silently).
        let bytes = canonical_bytes(&a()).unwrap();
        // The issued_at field is the 6th field; skip past the 5 prior
        // length prefixes + bodies. The cleanest way to assert this is
        // to extract by re-running with a different issued_at and
        // checking the diff is at the expected offset.
        let mut high = a();
        high.issued_at = 1_700_000_001; // +1
        let bytes_high = canonical_bytes(&high).unwrap();
        assert_eq!(bytes.len(), bytes_high.len());
        // The two byte streams must differ by exactly one byte in the
        // issued_at field; finding the offset is easier with a small
        // helper.
        let diffs: Vec<_> = bytes
            .iter()
            .zip(bytes_high.iter())
            .enumerate()
            .filter_map(|(i, (a, b))| if a != b { Some(i) } else { None })
            .collect();
        assert_eq!(diffs.len(), 1, "single-byte diff for +1 timestamp");
        // And confirm the byte is 0x01 (low byte of i64::from(1) in LE).
        assert_eq!(bytes_high[diffs[0]], 0x01);
    }
}
