//! Self-serve daemon claim proof for `zeroclaw relay claim`.
//!
//! The daemon proves control of its relay-registration identity to the ZeroRelay
//! control plane, which then allowlists that identity's fingerprint so the daemon
//! may register against the relay. This module derives the byte-exact proof the
//! control plane's `/v1/claim` verifier checks.
//!
//! The proof is signed with the SAME Ed25519 registration key the relay bridge
//! registers with ([`super::relay::ensure_signing_key`], and the `keypair.sign`
//! step in [`super::relay`]), loaded through the same `ring` API, so the
//! fingerprint proven here is identical to the one the daemon later registers
//! under. A separate crate or crypto library here could drift from that key.
//!
//! Wire contract — must stay byte-identical to the private `zerorelay-control`
//! verifier and to the relay's own fingerprinting in `apps/zerorelay`:
//! - public key: base64 STANDARD (padded, `+/`) of the raw 32-byte Ed25519 key.
//! - fingerprint: lowercase hex of `SHA-256(raw 32-byte public key)`, 64 chars,
//!   matching `hex::encode(Sha256::digest(&pubkey))` in `apps/zerorelay`.
//! - signed message: [`CLAIM_DOMAIN_TAG`] || `0x0A` || claim_token || `0x0A` ||
//!   fingerprint, all UTF-8.
//! - signature: base64 STANDARD of the raw 64-byte Ed25519 signature.

use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};

/// Domain-separation tag for the self-serve claim signature (18 bytes). Disjoint
/// from the relay registration handshake, which signs a bare 32-byte nonce, so a
/// claim signature can never be replayed as a registration proof. Must equal the
/// control plane's `CLAIM_DOMAIN_TAG`.
pub const CLAIM_DOMAIN_TAG: &str = "zerorelay-claim-v1";

/// The exact bytes the daemon signs to claim a node:
/// `CLAIM_DOMAIN_TAG || "\n" || claim_token || "\n" || fingerprint`, UTF-8.
///
/// The claim token and the 64-hex fingerprint contain no `\n`, so the two
/// separators frame the message unambiguously without length prefixes. This is
/// byte-identical to the control plane's `canonical_claim_message`.
pub fn claim_signing_message(claim_token: &str, fingerprint: &str) -> Vec<u8> {
    let mut msg =
        Vec::with_capacity(CLAIM_DOMAIN_TAG.len() + claim_token.len() + fingerprint.len() + 2);
    msg.extend_from_slice(CLAIM_DOMAIN_TAG.as_bytes());
    msg.push(b'\n');
    msg.extend_from_slice(claim_token.as_bytes());
    msg.push(b'\n');
    msg.extend_from_slice(fingerprint.as_bytes());
    msg
}

/// Lowercase-hex `SHA-256` fingerprint of a raw Ed25519 public key. Identical to
/// `apps/zerorelay`'s `hex::encode(Sha256::digest(&pubkey))`.
pub fn fingerprint_of_pubkey(pubkey: &[u8]) -> String {
    hex::encode(Sha256::digest(pubkey))
}

/// The proof a daemon presents to the control plane's `POST /v1/claim`. Field
/// encodings match the wire contract in this module's header.
#[derive(Debug, Clone)]
pub struct ClaimProof {
    /// base64 STANDARD of the raw 32-byte Ed25519 public key.
    pub public_key_b64: String,
    /// Lowercase hex of `SHA-256(pubkey)`, 64 chars.
    pub fingerprint: String,
    /// base64 STANDARD of the raw 64-byte Ed25519 signature.
    pub signature_b64: String,
}

/// Derive and sign the claim proof from the daemon's PKCS#8 registration key (as
/// returned by [`super::relay::ensure_signing_key`]). Loads the key with the same
/// `ring` API the registration path uses, so the presented fingerprint equals the
/// one the daemon registers under.
pub fn build_claim_proof(signing_key_pkcs8: &[u8], claim_token: &str) -> Result<ClaimProof> {
    let keypair = Ed25519KeyPair::from_pkcs8(signing_key_pkcs8)
        .map_err(|e| anyhow::Error::msg(format!("loading relay signing key: {e}")))?;
    let pubkey = keypair.public_key().as_ref();
    let fingerprint = fingerprint_of_pubkey(pubkey);
    let message = claim_signing_message(claim_token, &fingerprint);
    let signature = keypair.sign(&message);
    Ok(ClaimProof {
        public_key_b64: B64.encode(pubkey),
        fingerprint,
        signature_b64: B64.encode(signature.as_ref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{ED25519, UnparsedPublicKey};

    /// The claim message framing is a fixed contract. This golden vector is the
    /// locus any tag or separator change breaks, independent of the key.
    #[test]
    fn claim_message_is_byte_exact() {
        assert_eq!(CLAIM_DOMAIN_TAG, "zerorelay-claim-v1");
        let msg = claim_signing_message("tok-123", "abc0def");
        assert_eq!(msg, b"zerorelay-claim-v1\ntok-123\nabc0def");
    }

    /// End-to-end byte-exactness against exactly what the control plane checks:
    /// fingerprint == hex(sha256(pubkey)); the signed message is the canonical
    /// framing; and the signature verifies under the pubkey with a strict Ed25519
    /// verifier over that message. `ring`'s verifier enforces the same canonical
    /// encoding the control plane's `verify_strict` requires.
    #[test]
    fn claim_proof_round_trips_and_verifies_strictly() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let token = "clm_A1b2.C3d4-E5f6_G7h8";

        let proof = build_claim_proof(pkcs8.as_ref(), token).unwrap();

        // The advertised public key is the raw 32-byte key, base64 STANDARD.
        let pubkey = B64.decode(proof.public_key_b64.as_bytes()).unwrap();
        assert_eq!(pubkey.len(), 32);

        // (a) fingerprint == hex(sha256(pubkey)), 64 lowercase hex chars.
        assert_eq!(proof.fingerprint, hex::encode(Sha256::digest(&pubkey)));
        assert_eq!(proof.fingerprint.len(), 64);
        assert!(
            proof
                .fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );

        // (b) the signed message equals the exact tag||\n||token||\n||fpr bytes.
        let expected = {
            let mut m = Vec::new();
            m.extend_from_slice(b"zerorelay-claim-v1");
            m.push(0x0A);
            m.extend_from_slice(token.as_bytes());
            m.push(0x0A);
            m.extend_from_slice(proof.fingerprint.as_bytes());
            m
        };
        assert_eq!(claim_signing_message(token, &proof.fingerprint), expected);

        // (c) the signature verifies under the pubkey over that message, using a
        // strict verifier — the guard that the daemon and control plane agree.
        let sig = B64.decode(proof.signature_b64.as_bytes()).unwrap();
        assert_eq!(sig.len(), 64);
        UnparsedPublicKey::new(&ED25519, &pubkey)
            .verify(&expected, &sig)
            .expect("claim signature must verify over the canonical message");
    }

    /// A wrong token yields a signature that does not verify over the real
    /// message, so the proof is bound to the token, not merely carrying it.
    #[test]
    fn signature_is_bound_to_the_token() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let proof = build_claim_proof(pkcs8.as_ref(), "token-A").unwrap();

        let pubkey = B64.decode(proof.public_key_b64.as_bytes()).unwrap();
        let sig = B64.decode(proof.signature_b64.as_bytes()).unwrap();
        let other = claim_signing_message("token-B", &proof.fingerprint);
        assert!(
            UnparsedPublicKey::new(&ED25519, &pubkey)
                .verify(&other, &sig)
                .is_err()
        );
    }
}
