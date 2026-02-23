use super::traits::{Tool, ToolResult};
use crate::wallet::erc20;
use crate::wallet::provider::EvmProvider;
use crate::wallet::storage::WalletStore;
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct WalletTokenSendTool {
    store: Arc<WalletStore>,
    provider: Arc<EvmProvider>,
}

impl WalletTokenSendTool {
    pub fn new(store: Arc<WalletStore>, provider: Arc<EvmProvider>) -> Self {
        Self { store, provider }
    }
}

#[async_trait]
impl Tool for WalletTokenSendTool {
    fn name(&self) -> &str {
        "wallet_token_send"
    }

    fn description(&self) -> &str {
        "Sends ERC-20 tokens on-chain from the agent's wallet. \
         Parameters: token_address, to, amount (raw units string). \
         Irreversible — requires Full autonomy."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "token_address": {
                    "type": "string",
                    "description": "ERC-20 contract address (0x-prefixed)"
                },
                "to": {
                    "type": "string",
                    "description": "Recipient EVM address (0x-prefixed)"
                },
                "amount": {
                    "type": "string",
                    "description": "Amount to send in raw token units (e.g. smallest denomination)"
                }
            },
            "required": ["token_address", "to", "amount"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let token_str = args
            .get("token_address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: token_address"))?;

        let to_str = args
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: to"))?;

        let amount_str = args
            .get("amount")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: amount"))?;

        let token_addr: Address = token_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid token address: {e}"))?;

        let to: Address = to_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid recipient address: {e}"))?;

        let amount: U256 = amount_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid amount: {e}"))?;

        if amount.is_zero() {
            anyhow::bail!("Refusing to send zero-amount token transfer");
        }

        let keypair = self
            .store
            .load()
            .map_err(|e| anyhow::anyhow!("Failed to load wallet: {e}"))?;

        let from = keypair.address();
        let data = erc20::encode_transfer(to, amount);
        let tx_hash = self
            .provider
            .send_contract_tx(&keypair, token_addr, data)
            .await?;

        let output = serde_json::to_string_pretty(&json!({
            "success": true,
            "tx_hash": format!("{tx_hash:#x}"),
            "token_address": format!("{token_addr:#x}"),
            "from": from.as_str(),
            "to": format!("{to:#x}"),
            "amount": amount_str,
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
        let tool = WalletTokenSendTool::new(test_store_with_wallet(&tmp), test_provider());
        assert_eq!(tool.name(), "wallet_token_send");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn rejects_missing_params() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletTokenSendTool::new(test_store_with_wallet(&tmp), test_provider());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_zero_amount() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletTokenSendTool::new(test_store_with_wallet(&tmp), test_provider());
        let result = tool
            .execute(json!({
                "token_address": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
                "to": "0x0000000000000000000000000000000000000001",
                "amount": "0"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("zero-amount"));
    }

    #[tokio::test]
    async fn rejects_invalid_address() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletTokenSendTool::new(test_store_with_wallet(&tmp), test_provider());
        let result = tool
            .execute(json!({
                "token_address": "not-an-address",
                "to": "0x0000000000000000000000000000000000000001",
                "amount": "1000"
            }))
            .await;
        assert!(result.is_err());
    }
}
