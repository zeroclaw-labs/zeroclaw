use super::traits::{Tool, ToolResult};
use crate::wallet::erc20;
use crate::wallet::provider::EvmProvider;
use crate::wallet::storage::WalletStore;
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct WalletTokenBalanceTool {
    store: Arc<WalletStore>,
    provider: Arc<EvmProvider>,
}

impl WalletTokenBalanceTool {
    pub fn new(store: Arc<WalletStore>, provider: Arc<EvmProvider>) -> Self {
        Self { store, provider }
    }
}

#[async_trait]
impl Tool for WalletTokenBalanceTool {
    fn name(&self) -> &str {
        "wallet_token_balance"
    }

    fn description(&self) -> &str {
        "Queries the on-chain ERC-20 token balance for an address. \
         Requires token_address. If no account address is provided, uses the agent's wallet."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "token_address": {
                    "type": "string",
                    "description": "ERC-20 contract address (0x-prefixed)"
                },
                "address": {
                    "type": "string",
                    "description": "Account address to query (0x-prefixed). Defaults to agent wallet."
                }
            },
            "required": ["token_address"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let token_str = args
            .get("token_address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: token_address"))?;

        let token_addr: Address = token_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid token address: {e}"))?;

        let account: Address =
            if let Some(addr_str) = args.get("address").and_then(|v| v.as_str()) {
                addr_str
                    .parse()
                    .map_err(|e| anyhow::anyhow!("Invalid address: {e}"))?
            } else {
                let addr_str = self
                    .store
                    .address()
                    .map_err(|e| anyhow::anyhow!("No wallet available: {e}"))?;
                addr_str
                    .as_str()
                    .parse()
                    .map_err(|e| anyhow::anyhow!("Invalid wallet address: {e}"))?
            };

        let balance_data = self
            .provider
            .call(token_addr, erc20::encode_balance_of(account))
            .await?;
        let balance_raw = erc20::decode_balance_of(&balance_data)?;

        let decimals_data = self
            .provider
            .call(token_addr, erc20::encode_decimals())
            .await?;
        let decimals = erc20::decode_decimals(&decimals_data)?;

        let symbol_data = self
            .provider
            .call(token_addr, erc20::encode_symbol())
            .await?;
        let symbol = erc20::decode_symbol(&symbol_data)?;

        let balance_formatted = format_token_balance(balance_raw, decimals);

        let output = serde_json::to_string_pretty(&json!({
            "token_address": format!("{token_addr:#x}"),
            "account": format!("{account:#x}"),
            "balance_raw": balance_raw.to_string(),
            "balance_formatted": balance_formatted,
            "decimals": decimals,
            "symbol": symbol,
            "chain_id": self.provider.chain_id(),
        }))?;

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

fn format_token_balance(raw: U256, decimals: u8) -> String {
    if decimals == 0 {
        return raw.to_string();
    }
    let divisor = U256::from(10u64).pow(U256::from(decimals));
    let whole = raw / divisor;
    let remainder = raw % divisor;
    let remainder_str = format!("{remainder:0>width$}", width = decimals as usize);
    let trimmed = remainder_str.trim_end_matches('0');
    if trimmed.is_empty() {
        format!("{whole}.0")
    } else {
        format!("{whole}.{trimmed}")
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
        let provider =
            Arc::new(EvmProvider::connect("https://rpc.sepolia.org", 11155111).unwrap());
        let tool = WalletTokenBalanceTool::new(store, provider);
        assert_eq!(tool.name(), "wallet_token_balance");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_token_address_rejection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = test_store_with_wallet(&tmp);
        let provider =
            Arc::new(EvmProvider::connect("https://rpc.sepolia.org", 11155111).unwrap());
        let tool = WalletTokenBalanceTool::new(store, provider);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn format_token_balance_18_decimals() {
        let raw = U256::from(1_500_000_000_000_000_000u64);
        assert_eq!(format_token_balance(raw, 18), "1.5");
    }

    #[test]
    fn format_token_balance_6_decimals() {
        let raw = U256::from(1_000_000u64);
        assert_eq!(format_token_balance(raw, 6), "1.0");
    }

    #[test]
    fn format_token_balance_zero_decimals() {
        let raw = U256::from(42u64);
        assert_eq!(format_token_balance(raw, 0), "42");
    }
}
