//! Wallet send tool — send ETH on-chain via JSON-RPC (requires Full autonomy).

use super::traits::{Tool, ToolResult};
use crate::wallet::provider::EvmProvider;
use crate::wallet::storage::WalletStore;
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct WalletSendTool {
    store: Arc<WalletStore>,
    provider: Arc<EvmProvider>,
}

impl WalletSendTool {
    pub fn new(store: Arc<WalletStore>, provider: Arc<EvmProvider>) -> Self {
        Self { store, provider }
    }
}

#[async_trait]
impl Tool for WalletSendTool {
    fn name(&self) -> &str {
        "wallet_send"
    }

    fn description(&self) -> &str {
        "Sends ETH on-chain from the agent's wallet. \
         Parameters: to (0x address), amount_wei (string). \
         Irreversible — requires Full autonomy."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Recipient EVM address (0x-prefixed)"
                },
                "amount_wei": {
                    "type": "string",
                    "description": "Amount to send in wei"
                }
            },
            "required": ["to", "amount_wei"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let to_str = args
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: to"))?;

        let amount_str = args
            .get("amount_wei")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: amount_wei"))?;

        let to: Address = to_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid recipient address: {e}"))?;

        let value: U256 = amount_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid amount: {e}"))?;

        if value.is_zero() {
            anyhow::bail!("Refusing to send zero-value transaction");
        }

        let keypair = self
            .store
            .load()
            .map_err(|e| anyhow::anyhow!("Failed to load wallet: {e}"))?;

        let from = keypair.address();
        let tx_hash = self.provider.send_eth(&keypair, to, value).await?;

        let output = serde_json::to_string_pretty(&json!({
            "success": true,
            "tx_hash": format!("{tx_hash:#x}"),
            "from": from.as_str(),
            "to": format!("{to:#x}"),
            "amount_wei": amount_str,
            "chain_id": self.provider.chain_id(),
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

    fn test_provider() -> Arc<EvmProvider> {
        Arc::new(EvmProvider::connect("https://rpc.sepolia.org", 11155111).unwrap())
    }

    #[test]
    fn tool_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletSendTool::new(test_store_with_wallet(&tmp), test_provider());
        assert_eq!(tool.name(), "wallet_send");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rejects_missing_params() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletSendTool::new(test_store_with_wallet(&tmp), test_provider());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_zero_value() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletSendTool::new(test_store_with_wallet(&tmp), test_provider());
        let result = tool
            .execute(json!({
                "to": "0x0000000000000000000000000000000000000001",
                "amount_wei": "0"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("zero-value"));
    }

    #[tokio::test]
    async fn rejects_invalid_address() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletSendTool::new(test_store_with_wallet(&tmp), test_provider());
        let result = tool
            .execute(json!({
                "to": "not-an-address",
                "amount_wei": "1000"
            }))
            .await;
        assert!(result.is_err());
    }
}
