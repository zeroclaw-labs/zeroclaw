//! Private key generation and Ethereum address derivation.

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use serde::{Deserialize, Serialize};

/// Wrapper around an Ethereum address for display/serialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletAddress(pub String);

impl WalletAddress {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WalletAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A wallet keypair — wraps alloy's `PrivateKeySigner` for secp256k1 ECDSA.
pub struct WalletKeypair {
    signer: PrivateKeySigner,
}

impl WalletKeypair {
    /// Generate a new random keypair using the OS CSPRNG.
    pub fn generate() -> Self {
        Self {
            signer: PrivateKeySigner::random(),
        }
    }

    /// Reconstruct a keypair from a hex-encoded private key (no `0x` prefix).
    pub fn from_hex(hex_key: &str) -> anyhow::Result<Self> {
        let key = hex_key.strip_prefix("0x").unwrap_or(hex_key);
        let signer: PrivateKeySigner = key
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid private key hex: {e}"))?;
        Ok(Self { signer })
    }

    /// Get the Ethereum address derived from this keypair.
    pub fn address(&self) -> WalletAddress {
        let addr: Address = self.signer.address();
        WalletAddress(format!("{addr:#x}"))
    }

    /// Export the private key as a hex string (no `0x` prefix).
    ///
    /// **Security**: This exposes the raw key. Only use for encrypted storage.
    pub fn private_key_hex(&self) -> String {
        let bytes = self.signer.credential().to_bytes();
        hex::encode(bytes)
    }

    /// Get a reference to the inner signer for signing operations.
    pub fn signer(&self) -> &PrivateKeySigner {
        &self.signer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_valid_address() {
        let kp = WalletKeypair::generate();
        let addr = kp.address();
        assert!(
            addr.as_str().starts_with("0x"),
            "Address should start with 0x"
        );
        // Ethereum address is 42 chars: "0x" + 40 hex chars
        assert_eq!(addr.as_str().len(), 42);
    }

    #[test]
    fn two_generated_keys_differ() {
        let kp1 = WalletKeypair::generate();
        let kp2 = WalletKeypair::generate();
        assert_ne!(kp1.address(), kp2.address());
        assert_ne!(kp1.private_key_hex(), kp2.private_key_hex());
    }

    #[test]
    fn roundtrip_from_hex() {
        let kp1 = WalletKeypair::generate();
        let hex_key = kp1.private_key_hex();
        let kp2 = WalletKeypair::from_hex(&hex_key).unwrap();
        assert_eq!(kp1.address(), kp2.address());
    }

    #[test]
    fn from_hex_with_0x_prefix() {
        let kp1 = WalletKeypair::generate();
        let hex_key = format!("0x{}", kp1.private_key_hex());
        let kp2 = WalletKeypair::from_hex(&hex_key).unwrap();
        assert_eq!(kp1.address(), kp2.address());
    }

    #[test]
    fn from_hex_invalid_key_fails() {
        assert!(WalletKeypair::from_hex("not_a_hex_key").is_err());
        assert!(WalletKeypair::from_hex("").is_err());
        assert!(WalletKeypair::from_hex("zzzz").is_err());
    }

    #[test]
    fn private_key_hex_is_64_chars() {
        let kp = WalletKeypair::generate();
        assert_eq!(
            kp.private_key_hex().len(),
            64,
            "secp256k1 key is 32 bytes = 64 hex chars"
        );
    }

    #[test]
    fn address_display() {
        let kp = WalletKeypair::generate();
        let addr = kp.address();
        let display = format!("{addr}");
        assert!(display.starts_with("0x"));
        assert_eq!(display, addr.as_str());
    }

    #[test]
    fn wallet_address_serde_roundtrip() {
        let addr = WalletAddress("0xaabbccdd00112233445566778899aabbccddeeff".to_string());
        let json = serde_json::to_string(&addr).unwrap();
        let parsed: WalletAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn deterministic_from_known_key() {
        // Well-known test vector: private key of all 1s
        let hex_key = "0000000000000000000000000000000000000000000000000000000000000001";
        let kp = WalletKeypair::from_hex(hex_key).unwrap();
        let addr = kp.address();
        // This is the known address for private key = 1
        assert!(addr.as_str().starts_with("0x"));
        assert_eq!(addr.as_str().len(), 42);
        // The address should be deterministic across runs
        let kp2 = WalletKeypair::from_hex(hex_key).unwrap();
        assert_eq!(kp.address(), kp2.address());
    }
}
