//! EIP-712 typed data signing for x402 payment authorization.

use super::keypair::WalletKeypair;
use alloy_primitives::{Address, B256, U256};
use alloy_signer::Signer;
use alloy_sol_types::{eip712_domain, sol, SolStruct};
use serde::{Deserialize, Serialize};

// Define the x402 payment authorization struct using alloy's sol! macro.
sol! {
    #[derive(Debug, Serialize, Deserialize)]
    struct PaymentAuthorization {
        address recipient;
        uint256 amount;
        uint256 nonce;
        uint256 expiry;
    }
}

/// Domain separator for x402 payment signing.
fn x402_domain(chain_id: u64) -> alloy_sol_types::Eip712Domain {
    eip712_domain! {
        name: "x402-payment",
        version: "1",
        chain_id: chain_id,
    }
}

/// Signed payment authorization for x402.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPayment {
    pub signer_address: String,
    pub recipient: String,
    pub amount: String,
    pub nonce: u64,
    pub expiry: u64,
    pub chain_id: u64,
    pub signature: String,
}

/// EIP-712 signer for x402 payment authorizations.
pub struct Eip712Signer;

impl Eip712Signer {
    /// Sign a payment authorization using EIP-712 typed data.
    pub async fn sign_payment(
        keypair: &WalletKeypair,
        recipient: Address,
        amount: U256,
        nonce: u64,
        expiry: u64,
        chain_id: u64,
    ) -> anyhow::Result<SignedPayment> {
        let auth = PaymentAuthorization {
            recipient,
            amount,
            nonce: U256::from(nonce),
            expiry: U256::from(expiry),
        };

        let domain = x402_domain(chain_id);

        // Compute the EIP-712 signing hash
        let signing_hash = auth.eip712_signing_hash(&domain);

        // Sign the hash
        let signature = keypair
            .signer()
            .sign_hash(&B256::from(signing_hash))
            .await
            .map_err(|e| anyhow::anyhow!("EIP-712 signing failed: {e}"))?;

        let sig_hex = format!("0x{}", hex::encode(signature.as_bytes()));

        Ok(SignedPayment {
            signer_address: keypair.address().to_string(),
            recipient: format!("{recipient:#x}"),
            amount: amount.to_string(),
            nonce,
            expiry,
            chain_id,
            signature: sig_hex,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sign_payment_produces_valid_signature() {
        let kp = WalletKeypair::generate();
        let recipient = "0x0000000000000000000000000000000000000001"
            .parse::<Address>()
            .unwrap();
        let amount = U256::from(100_000u64);

        let signed = Eip712Signer::sign_payment(&kp, recipient, amount, 1, 9999999999, 1)
            .await
            .unwrap();

        assert!(signed.signature.starts_with("0x"));
        // ECDSA signature is 65 bytes = 130 hex chars + "0x" prefix
        assert_eq!(signed.signature.len(), 132);
        assert_eq!(signed.nonce, 1);
        assert_eq!(signed.chain_id, 1);
        assert_eq!(signed.signer_address, kp.address().to_string());
    }

    #[tokio::test]
    async fn different_nonces_produce_different_signatures() {
        let kp = WalletKeypair::generate();
        let recipient = "0x0000000000000000000000000000000000000001"
            .parse::<Address>()
            .unwrap();
        let amount = U256::from(100u64);

        let sig1 = Eip712Signer::sign_payment(&kp, recipient, amount, 1, 9999999999, 1)
            .await
            .unwrap();
        let sig2 = Eip712Signer::sign_payment(&kp, recipient, amount, 2, 9999999999, 1)
            .await
            .unwrap();

        assert_ne!(sig1.signature, sig2.signature);
    }

    #[tokio::test]
    async fn different_chains_produce_different_signatures() {
        let kp = WalletKeypair::generate();
        let recipient = "0x0000000000000000000000000000000000000001"
            .parse::<Address>()
            .unwrap();
        let amount = U256::from(100u64);

        let sig_mainnet = Eip712Signer::sign_payment(&kp, recipient, amount, 1, 9999999999, 1)
            .await
            .unwrap();
        let sig_base = Eip712Signer::sign_payment(&kp, recipient, amount, 1, 9999999999, 8453)
            .await
            .unwrap();

        assert_ne!(sig_mainnet.signature, sig_base.signature);
    }

    #[test]
    fn signed_payment_serde_roundtrip() {
        let sp = SignedPayment {
            signer_address: "0xaabb".to_string(),
            recipient: "0xccdd".to_string(),
            amount: "100000".to_string(),
            nonce: 1,
            expiry: 9999999999,
            chain_id: 1,
            signature: "0xabcdef".to_string(),
        };
        let json = serde_json::to_string(&sp).unwrap();
        let parsed: SignedPayment = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.nonce, 1);
        assert_eq!(parsed.chain_id, 1);
    }
}
