//! Proof that an HTTP answer comes from this daemon's own gateway listener.
//!
//! Each gateway generation draws a fresh secret key. The core publishes the
//! key, with the address its listener bound, only while that listener accepts
//! connections. The gateway answers a `/health` challenge with an HMAC of the
//! caller's nonce under the key. A client that gets a nonce and the expected
//! proof over the verified RPC socket can then tell the core's own listener
//! from any other process on the same address: no other process holds the
//! key, whatever process ID it reports.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Domain separation, so a proof can never stand in for another HMAC under
/// the same key.
const PROOF_DOMAIN: &[u8] = b"zeroclaw-gateway-possession-v1\n";

/// The longest challenge nonce the gateway answers. A nonce from
/// [`new_nonce`] is 64 characters.
pub const MAX_NONCE_LEN: usize = 128;

/// One gateway generation's possession key. It is never serialized, and its
/// `Debug` form does not print it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GatewayPossession([u8; 32]);

impl std::fmt::Debug for GatewayPossession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GatewayPossession(..)")
    }
}

impl GatewayPossession {
    /// A fresh key from a cryptographically secure generator.
    #[must_use]
    pub fn generate() -> Self {
        Self(rand::random())
    }

    /// The key as hex, for a gateway in another process that registers its
    /// listener with the core over its local RPC connection.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// A key a gateway in another process registered, from its hex form.
    #[must_use]
    pub fn from_hex(key: &str) -> Option<Self> {
        let bytes: [u8; 32] = hex::decode(key).ok()?.try_into().ok()?;
        Some(Self(bytes))
    }

    /// A prover for a key this process holds itself, such as a separate
    /// gateway's own: it answers until `released` is set.
    #[must_use]
    pub fn prover(self, released: std::sync::Arc<std::sync::atomic::AtomicBool>) -> GatewayProver {
        GatewayProver::new(self, released)
    }

    /// The proof of `nonce`: lowercase hex of HMAC-SHA256 over a fixed domain
    /// string followed by the nonce.
    #[must_use]
    pub fn proof(&self, nonce: &str) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC accepts a key of any length");
        mac.update(PROOF_DOMAIN);
        mac.update(nonce.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

/// What a gateway listener answers `/health` challenges with: its
/// generation's key, while the listener still accepts connections. Once it
/// stops, it answers none, so a connection opened to it earlier cannot relay
/// a proof for an address it no longer holds.
#[derive(Debug, Clone)]
pub struct GatewayProver {
    possession: GatewayPossession,
    released: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl GatewayProver {
    pub(crate) fn new(
        possession: GatewayPossession,
        released: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            possession,
            released,
        }
    }

    /// The proof of `nonce`, or `None` once the listener stopped accepting
    /// or when the nonce is longer than [`MAX_NONCE_LEN`].
    #[must_use]
    pub fn prove(&self, nonce: &str) -> Option<String> {
        if nonce.len() > MAX_NONCE_LEN || self.released.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        Some(self.possession.proof(nonce))
    }
}

/// A fresh challenge nonce: 32 random bytes, hex.
#[must_use]
pub fn new_nonce() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proof_is_fixed_for_a_key_and_nonce_and_differs_otherwise() {
        let key = GatewayPossession([7; 32]);
        assert_eq!(key.proof("n1"), key.proof("n1"));
        assert_ne!(key.proof("n1"), key.proof("n2"));
        assert_ne!(key.proof("n1"), GatewayPossession([8; 32]).proof("n1"));
        assert_eq!(key.proof("n1").len(), 64);
    }

    #[test]
    fn generated_keys_and_nonces_are_fresh() {
        assert_ne!(GatewayPossession::generate(), GatewayPossession::generate());
        let nonce = new_nonce();
        assert_eq!(nonce.len(), 64);
        assert!(nonce.len() <= MAX_NONCE_LEN);
        assert_ne!(nonce, new_nonce());
    }

    #[test]
    fn a_registered_key_round_trips_through_hex_and_nothing_else_does() {
        let key = GatewayPossession::generate();
        assert_eq!(GatewayPossession::from_hex(&key.to_hex()), Some(key));
        assert_eq!(GatewayPossession::from_hex("abcd"), None);
        assert_eq!(GatewayPossession::from_hex(&"zz".repeat(32)), None);
    }

    #[test]
    fn a_prover_stops_answering_once_released() {
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let key = GatewayPossession::generate();
        let prover = key.prover(std::sync::Arc::clone(&released));
        assert_eq!(prover.prove("n"), Some(key.proof("n")));
        assert_eq!(prover.prove(&"n".repeat(MAX_NONCE_LEN + 1)), None);
        released.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(prover.prove("n"), None);
    }

    #[test]
    fn the_key_never_appears_in_debug_output() {
        assert_eq!(
            format!("{:?}", GatewayPossession([0xab; 32])),
            "GatewayPossession(..)"
        );
    }
}
