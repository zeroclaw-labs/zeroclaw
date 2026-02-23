//! Wallet sign tool — EIP-712 typed data signing (requires Full autonomy).

use super::traits::{Tool, ToolResult};
use crate::wallet::signing::Eip712Signer;
use crate::wallet::storage::WalletStore;
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Signs an EIP-712 payment authorization. Requires Full autonomy level.
pub struct WalletSignTool {
    store: Arc<WalletStore>,
}

impl WalletSignTool {
    pub fn new(store: Arc<WalletStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for WalletSignTool {
    fn name(&self) -> &str {
        "wallet_sign"
    }

    fn description(&self) -> &str {
        "Signs an EIP-712 payment authorization with the agent's wallet. \
         Parameters: recipient (0x address), amount (wei string), chain_id (default 8453), \
         nonce (default 1), expiry (default max). Requires Full autonomy."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "recipient": {
                    "type": "string",
                    "description": "EVM address of the payment recipient (0x-prefixed)"
                },
                "amount": {
                    "type": "string",
                    "description": "Payment amount in wei"
                },
                "chain_id": {
                    "type": "integer",
                    "description": "EVM chain ID (default: 8453 for Base)"
                },
                "nonce": {
                    "type": "integer",
                    "description": "Payment nonce (default: 1)"
                },
                "expiry": {
                    "type": "integer",
                    "description": "Payment expiry timestamp (default: u64::MAX)"
                }
            },
            "required": ["recipient", "amount"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let recipient_str = args
            .get("recipient")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: recipient"))?;

        let amount_str = args
            .get("amount")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: amount"))?;

        let chain_id = args
            .get("chain_id")
            .and_then(|v| v.as_u64())
            .unwrap_or(8453);

        let nonce = args.get("nonce").and_then(|v| v.as_u64()).unwrap_or(1);

        let expiry = args
            .get("expiry")
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::MAX);

        // Load wallet
        let keypair = self
            .store
            .load()
            .map_err(|e| anyhow::anyhow!("Failed to load wallet: {e}"))?;

        // Parse inputs
        let recipient: Address = recipient_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid recipient address: {e}"))?;
        let amount: U256 = amount_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid amount: {e}"))?;

        // Sign
        let signed =
            Eip712Signer::sign_payment(&keypair, recipient, amount, nonce, expiry, chain_id)
                .await?;

        let output = serde_json::to_string_pretty(&json!({
            "signer": signed.signer_address,
            "recipient": signed.recipient,
            "amount": signed.amount,
            "chain_id": signed.chain_id,
            "nonce": signed.nonce,
            "expiry": signed.expiry,
            "signature": signed.signature,
        }))?;

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::WalletKeypair;

    fn test_store_with_wallet(tmp: &tempfile::TempDir) -> Arc<WalletStore> {
        let store = Arc::new(WalletStore::new(&tmp.path().join("wallet"), tmp.path()));
        let kp = WalletKeypair::generate();
        store.save(&kp).unwrap();
        store
    }

    #[test]
    fn tool_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = test_store_with_wallet(&tmp);
        let tool = WalletSignTool::new(store);
        assert_eq!(tool.name(), "wallet_sign");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["required"].as_array().unwrap().len() == 2);
    }

    #[tokio::test]
    async fn sign_returns_valid_signature() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = test_store_with_wallet(&tmp);
        let tool = WalletSignTool::new(store);

        let result = tool
            .execute(json!({
                "recipient": "0x0000000000000000000000000000000000000001",
                "amount": "1000000"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("signature"));
        assert!(result.output.contains("signer"));
    }

    #[tokio::test]
    async fn sign_fails_without_wallet() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(WalletStore::new(&tmp.path().join("wallet"), tmp.path()));
        let tool = WalletSignTool::new(store);

        let result = tool
            .execute(json!({
                "recipient": "0x0000000000000000000000000000000000000001",
                "amount": "1000000"
            }))
            .await;

        assert!(result.is_err() || !result.unwrap().success);
    }

    #[tokio::test]
    async fn sign_fails_with_invalid_address() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = test_store_with_wallet(&tmp);
        let tool = WalletSignTool::new(store);

        let result = tool
            .execute(json!({
                "recipient": "not-an-address",
                "amount": "1000000"
            }))
            .await;

        assert!(result.is_err());
    }
}
